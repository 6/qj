//! Running one tool invocation with a timeout, output caps and a memory cap.

use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Status {
    Exit(i32),
    /// Killed by a signal, without a core dump.
    Signal(i32),
    /// Killed by a signal, and the kernel dumped its core (`WCOREDUMP`): a
    /// shell says `Segmentation fault (core dumped)`. Whether it does is up to
    /// the kernel's settings (`ulimit -c`, `core_pattern`) as they apply to
    /// the process, so it is part of what a crash looks like.
    CoreDumped(i32),
    Timeout,
    /// stdout or stderr exceeded the capture limit; the process was killed.
    OutputLimit,
    /// Resident memory exceeded the limit; the process was killed.
    MemoryLimit,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Exit(c) => write!(f, "{c}"),
            Status::Signal(s) => write!(f, "signal {s}"),
            Status::CoreDumped(s) => write!(f, "signal {s} (core dumped)"),
            Status::Timeout => write!(f, "timeout"),
            Status::OutputLimit => write!(f, "output limit exceeded"),
            Status::MemoryLimit => write!(f, "memory limit exceeded"),
        }
    }
}

pub struct Spec<'a> {
    pub bin: &'a Path,
    /// `argv[0]`, when it isn't `bin` itself.
    pub arg0: Option<&'a str>,
    pub args: &'a [String],
    pub cwd: &'a Path,
    /// The complete environment (the child's environment is cleared first).
    pub env: &'a [(String, String)],
    /// `None` connects stdin to /dev/null.
    pub stdin: Option<&'a [u8]>,
    pub timeout: Duration,
    pub max_output: usize,
    /// Kill the process when its resident set exceeds this many bytes. The
    /// machine is shared, and some divergences (e.g. `.[1e9] = 5`) make a
    /// tool allocate without bound.
    pub max_rss: u64,
    pub merge: Merge,
    /// Standard descriptors to close in the child before it execs, so the
    /// tool starts with them closed (`qj -n 1 >&-`). Rust's runtime used to
    /// reopen those on /dev/null; C tools see them closed.
    pub close_fds: &'a [i32],
}

/// Whether stderr goes where stdout goes, so that the order in which a tool
/// writes the two streams shows (jq's stdout is a stdio buffer, flushed in
/// `st_blksize` blocks; stderr isn't buffered). The combined bytes are the
/// captured stdout, and stderr is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Merge {
    /// Separate pipes.
    #[default]
    No,
    /// `>out 2>&1`: one regular file for both.
    File,
    /// `2>&1 |`: one pipe for both.
    Pipe,
}

impl Merge {
    /// The name in CLI case files (`merge = "file"`); `None` for [`Merge::No`].
    pub fn name(self) -> Option<&'static str> {
        match self {
            Merge::No => None,
            Merge::File => Some("file"),
            Merge::Pipe => Some("pipe"),
        }
    }

    pub fn parse(s: &str) -> Option<Merge> {
        [Merge::File, Merge::Pipe]
            .into_iter()
            .find(|m| m.name() == Some(s))
    }
}

pub struct Output {
    pub status: Status,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Stdin up to this size is written before reading output: it always fits in
/// the pipe buffer, so the write cannot block. Larger stdin gets a thread.
const INLINE_STDIN: usize = 8 * 1024;

/// How often the watchdog samples the child's resident memory. Most
/// invocations finish before the first sample.
const RSS_POLL: Duration = Duration::from_millis(25);

/// Resident set size of a live (or not yet reaped) process.
#[cfg(target_os = "macos")]
fn rss_bytes(pid: u32) -> Option<u64> {
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: `info` is a valid, writable buffer of `size` bytes.
    let n = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTASKINFO,
            0,
            (&mut info as *mut libc::proc_taskinfo).cast(),
            size,
        )
    };
    (n == size).then_some(info.pti_resident_size)
}

#[cfg(target_os = "linux")]
fn rss_bytes(pid: u32) -> Option<u64> {
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    // SAFETY: sysconf has no preconditions.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    Some(pages * u64::try_from(page).ok()?)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn rss_bytes(_pid: u32) -> Option<u64> {
    None
}

/// Kill the child and everything it started. The child leads its own process
/// group ([`run`] sets `process_group(0)`), so `killpg` reaches any helper it
/// forked — e.g. `sh -c 'yes'` that outlived a plain `kill` and kept writing
/// to a deleted merged tempfile, filling the disk. `Child::kill` as well, in
/// case the group wasn't set.
fn kill(child: &Mutex<Child>) {
    if let Ok(mut c) = child.lock() {
        // SAFETY: killpg sends a signal to a process group; a negative or
        // recycled pgid at worst signals nothing (the child is reaped only
        // after the watchdog is joined, so its pid can't be reused yet).
        unsafe {
            libc::killpg(c.id() as libc::pid_t, libc::SIGKILL);
        }
        let _ = c.kill();
    }
}

/// Read until EOF, or until more than `cap` bytes arrived: then kill the
/// child, keeping exactly the first `cap` bytes, so that what two tools wrote
/// up to the cap compares the same however the pipe split it into reads.
fn read_capped(mut r: impl Read, cap: usize, child: &Mutex<Child>, over: &AtomicBool) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let room = cap - buf.len();
                if n > room {
                    buf.extend_from_slice(&chunk[..room]);
                    over.store(true, Ordering::SeqCst);
                    kill(child);
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    buf
}

/// On macOS, sets this process's `ulimit -c` to 0 (once), which every tool it
/// starts inherits. macOS dumps a core only when that limit asks for one, into
/// `/cores`, and a core there is the whole address space, a GB or more: a
/// run of crash cases must never fill the disk. Linux keeps the limit it was
/// given, because a pipe handler such as systemd-coredump ignores it there,
/// and whether the kernel dumps is compared ([`Status::CoreDumped`]).
fn no_core_files_on_macos() {
    #[cfg(target_os = "macos")]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            // SAFETY: getrlimit writes and setrlimit reads a valid rlimit.
            unsafe {
                let mut lim: libc::rlimit = std::mem::zeroed();
                if libc::getrlimit(libc::RLIMIT_CORE, &mut lim) == 0 {
                    lim.rlim_cur = 0;
                    libc::setrlimit(libc::RLIMIT_CORE, &lim);
                }
            }
        });
    }
}

pub fn run(spec: &Spec) -> Result<Output, String> {
    let io_err = |e: std::io::Error| format!("{}: {e}", spec.bin.display());
    let mut cmd = Command::new(spec.bin);
    if let Some(arg0) = spec.arg0 {
        cmd.arg0(arg0);
    }
    cmd.args(spec.args)
        .current_dir(spec.cwd)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        // The child leads its own process group, so [`kill`] can `killpg`
        // everything it starts (a `sh -c 'yes'` grandchild, a worker jq).
        .process_group(0)
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    // Merge::File: one open file (both fds share its offset, so writes land
    // in order); Merge::Pipe: one pipe.
    let mut merged_file = None;
    let mut merged_pipe = None;
    match spec.merge {
        Merge::No => {
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        }
        Merge::File => {
            let f = tempfile::tempfile().map_err(io_err)?;
            cmd.stdout(f.try_clone().map_err(io_err)?)
                .stderr(f.try_clone().map_err(io_err)?);
            merged_file = Some(f);
        }
        Merge::Pipe => {
            let (r, w) = std::io::pipe().map_err(io_err)?;
            cmd.stdout(w.try_clone().map_err(io_err)?).stderr(w);
            merged_pipe = Some(r);
        }
    }
    no_core_files_on_macos();
    if !spec.close_fds.is_empty() {
        let fds: Vec<i32> = spec.close_fds.to_vec();
        // SAFETY: runs in the forked child between the dup2s and exec;
        // close(2) is async-signal-safe and touches no memory we share.
        unsafe {
            cmd.pre_exec(move || {
                for &fd in &fds {
                    libc::close(fd);
                }
                Ok(())
            })
        };
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", spec.bin.display()))?;
    // The command holds our copies of the merged file or pipe: the pipe only
    // reaches EOF once they are closed.
    drop(cmd);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdin_pipe = child.stdin.take();
    let pid = child.id();
    let child = Arc::new(Mutex::new(child));
    let timed_out = Arc::new(AtomicBool::new(false));
    let over = Arc::new(AtomicBool::new(false));
    let mem_over = Arc::new(AtomicBool::new(false));

    // The watchdog kills the child at the deadline or when it uses too much
    // memory (or, writing to a merged file, too much output). It is joined
    // before the child is reaped, so it can never look at or signal a
    // recycled pid.
    let (cancel, cancelled) = mpsc::channel::<()>();
    let watchdog = {
        let child = Arc::clone(&child);
        let timed_out = Arc::clone(&timed_out);
        let mem_over = Arc::clone(&mem_over);
        let over = Arc::clone(&over);
        let deadline = Instant::now() + spec.timeout;
        let max_rss = spec.max_rss;
        let max_output = spec.max_output as u64;
        let file = match &merged_file {
            Some(f) => Some(f.try_clone().map_err(io_err)?),
            None => None,
        };
        thread::spawn(move || {
            loop {
                let now = Instant::now();
                if now >= deadline {
                    timed_out.store(true, Ordering::SeqCst);
                    kill(&child);
                    return;
                }
                match cancelled.recv_timeout(RSS_POLL.min(deadline - now)) {
                    Err(RecvTimeoutError::Timeout) => {
                        if rss_bytes(pid).is_some_and(|rss| rss > max_rss) {
                            mem_over.store(true, Ordering::SeqCst);
                            kill(&child);
                            return;
                        }
                        if let Some(f) = &file
                            && f.metadata().is_ok_and(|m| m.len() > max_output)
                        {
                            over.store(true, Ordering::SeqCst);
                            kill(&child);
                            return;
                        }
                    }
                    _ => return,
                }
            }
        })
    };

    let writer = match (stdin_pipe, spec.stdin) {
        (Some(mut pipe), Some(data)) if data.len() <= INLINE_STDIN => {
            // EPIPE (the child exited without reading) is expected and fine.
            let _ = pipe.write_all(data);
            None
        }
        (Some(mut pipe), Some(data)) => {
            let data = data.to_vec();
            Some(thread::spawn(move || {
                let _ = pipe.write_all(&data);
            }))
        }
        _ => None,
    };

    let (out, err) = if let Some(mut f) = merged_file {
        // Nothing to read until the child is gone: wait without holding the
        // lock the watchdog needs to kill it, and without reaping it, so that
        // its pid can't be reused before the watchdog is joined.
        while !exited_unreaped(pid).map_err(|e| format!("wait failed: {e}"))? {
            thread::sleep(Duration::from_millis(2));
        }
        use std::io::{Seek, SeekFrom};
        let mut out = Vec::new();
        f.seek(SeekFrom::Start(0)).map_err(io_err)?;
        f.take(spec.max_output as u64 + 1)
            .read_to_end(&mut out)
            .map_err(io_err)?;
        // Past the cap is past the cap, whether the watchdog saw it before
        // the child exited or not: keep exactly the first `max_output` bytes,
        // as `read_capped` does.
        if out.len() > spec.max_output {
            out.truncate(spec.max_output);
            over.store(true, Ordering::SeqCst);
        }
        (out, Vec::new())
    } else if let Some(pipe) = merged_pipe {
        (
            read_capped(pipe, spec.max_output, &child, &over),
            Vec::new(),
        )
    } else {
        let stderr = stderr.expect("piped stderr");
        let err_reader = {
            let child = Arc::clone(&child);
            let over = Arc::clone(&over);
            let cap = spec.max_output;
            thread::spawn(move || read_capped(stderr, cap, &child, &over))
        };
        let out = read_capped(
            stdout.expect("piped stdout"),
            spec.max_output,
            &child,
            &over,
        );
        (out, err_reader.join().expect("stderr reader panicked"))
    };
    if let Some(w) = writer {
        let _ = w.join();
    }
    let _ = cancel.send(());
    watchdog.join().expect("watchdog panicked");

    let status = child
        .lock()
        .map_err(|_| "child mutex poisoned".to_string())?
        .wait()
        .map_err(|e| format!("wait failed: {e}"))?;
    no_disk_trouble(spec, &out, &err)?;
    let status = if mem_over.load(Ordering::SeqCst) {
        Status::MemoryLimit
    } else if timed_out.load(Ordering::SeqCst) {
        Status::Timeout
    } else if over.load(Ordering::SeqCst) {
        Status::OutputLimit
    } else if let Some(code) = status.code() {
        Status::Exit(code)
    } else if status.core_dumped() {
        Status::CoreDumped(status.signal().unwrap_or(-1))
    } else {
        Status::Signal(status.signal().unwrap_or(-1))
    };
    Ok(Output {
        status,
        stdout: out,
        stderr: err,
    })
}

/// Whether the child `pid` has exited, without reaping it (`WNOWAIT`).
fn exited_unreaped(pid: u32) -> std::io::Result<bool> {
    // SAFETY: waitid writes a siginfo_t into a valid out-pointer.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let r = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: waitid filled `info` in (si_pid is 0 while the child runs).
    Ok(unsafe { info.si_pid() } != 0)
}

/// Free bytes on the filesystem holding `dir`, if it can be told.
fn free_bytes(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs writes a statvfs into a valid out-pointer.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(path.as_ptr(), &mut st) } == 0)
        .then(|| u64::from(st.f_bavail) * u64::from(st.f_frsize))
}

/// Below this, a run's output may have been cut short by a full disk.
const MIN_FREE_BYTES: u64 = 256 << 20;

/// A full disk makes a tool's writes fail (`No space left on device`), which
/// looks like a divergence and, for jq, would be cached as its answer. That is
/// the machine's trouble, not a case's, so it fails the whole run: a CI flake
/// was exactly this, jq's merged output losing 16 KB and exiting 2 while a
/// stray `yes` from an earlier test filled the runner's disk.
fn no_disk_trouble(spec: &Spec, out: &[u8], err: &[u8]) -> Result<(), String> {
    const ENOSPC: &[u8] = b"No space left on device";
    let reported = [out, err]
        .iter()
        .any(|b| memchr::memmem::find(b, ENOSPC).is_some());
    let free = free_bytes(spec.cwd);
    if reported || free.is_some_and(|f| f < MIN_FREE_BYTES) {
        return Err(format!(
            "{}: the disk holding {} is full or nearly full ({} MB free{}); \
             nothing written during this run can be trusted",
            spec.bin.display(),
            spec.cwd.display(),
            free.map_or("?".to_string(), |f| (f >> 20).to_string()),
            if reported {
                ", and the tool reported No space left on device"
            } else {
                ""
            }
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, stdin: Option<&[u8]>, timeout_ms: u64, cap: usize) -> Output {
        let args = vec!["-c".to_string(), script.to_string()];
        run(&Spec {
            bin: Path::new("/bin/sh"),
            arg0: None,
            args: &args,
            cwd: Path::new("/"),
            env: &[("PATH".into(), "/usr/bin:/bin".into())],
            stdin,
            timeout: Duration::from_millis(timeout_ms),
            max_output: cap,
            max_rss: 1 << 30,
            merge: Merge::No,
            close_fds: &[],
        })
        .unwrap()
    }

    fn sh_merged(script: &str, merge: Merge, timeout_ms: u64, cap: usize) -> Output {
        let args = vec!["-c".to_string(), script.to_string()];
        run(&Spec {
            bin: Path::new("/bin/sh"),
            arg0: None,
            args: &args,
            cwd: Path::new("/"),
            env: &[("PATH".into(), "/usr/bin:/bin".into())],
            stdin: None,
            timeout: Duration::from_millis(timeout_ms),
            max_output: cap,
            max_rss: 1 << 30,
            merge,
            close_fds: &[],
        })
        .unwrap()
    }

    /// The exit is seen without reaping the child, so its status is still
    /// there for `wait`, and its pid isn't free for reuse before then.
    #[test]
    fn an_exit_is_seen_without_reaping() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 3"])
            .spawn()
            .expect("spawn sh");
        while !exited_unreaped(child.id()).expect("waitid") {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(exited_unreaped(child.id()).expect("waitid, again"));
        assert_eq!(child.wait().expect("wait").code(), Some(3));
    }

    /// A tool that ran out of disk fails the run instead of scoring a case.
    #[test]
    fn a_full_disk_fails_the_run() {
        let args = vec![
            "-c".to_string(),
            "echo 'jq: error: writing output failed: No space left on device' >&2; exit 2"
                .to_string(),
        ];
        let r = run(&Spec {
            bin: Path::new("/bin/sh"),
            arg0: None,
            args: &args,
            cwd: Path::new("/"),
            env: &[("PATH".into(), "/usr/bin:/bin".into())],
            stdin: None,
            timeout: Duration::from_secs(5),
            max_output: 1 << 16,
            max_rss: 1 << 30,
            merge: Merge::File,
            close_fds: &[],
        });
        let e = r.err().expect("an error");
        assert!(e.contains("full"), "{e}");
    }

    #[test]
    fn merged_streams_keep_write_order() {
        for merge in [Merge::File, Merge::Pipe] {
            let o = sh_merged("echo a; echo b >&2; echo c; exit 4", merge, 5000, 1 << 20);
            assert_eq!(o.stdout, b"a\nb\nc\n", "{merge:?}");
            assert_eq!(o.stderr, b"", "{merge:?}");
            assert_eq!(o.status, Status::Exit(4), "{merge:?}");
        }
    }

    #[test]
    fn merged_streams_are_limited() {
        for merge in [Merge::File, Merge::Pipe] {
            assert_eq!(
                sh_merged("yes", merge, 10_000, 1 << 16).status,
                Status::OutputLimit,
                "{merge:?}"
            );
            assert_eq!(
                sh_merged("sleep 5", merge, 200, 1 << 16).status,
                Status::Timeout,
                "{merge:?}"
            );
        }
    }

    #[test]
    fn captures_stdout_stderr_and_exit_code() {
        let o = sh("cat; echo err >&2; exit 3", Some(b"in\n"), 5000, 1 << 20);
        assert_eq!(o.stdout, b"in\n");
        assert_eq!(o.stderr, b"err\n");
        assert_eq!(o.status, Status::Exit(3));
    }

    #[test]
    fn large_stdin_does_not_deadlock() {
        let data = vec![b'x'; 1 << 20];
        let o = sh("cat", Some(&data), 10_000, 4 << 20);
        assert_eq!(o.stdout.len(), data.len());
        assert_eq!(o.status, Status::Exit(0));
    }

    #[test]
    fn timeout_kills() {
        let o = sh("sleep 5", None, 200, 1 << 20);
        assert_eq!(o.status, Status::Timeout);
    }

    #[test]
    fn output_limit_kills() {
        let o = sh("yes", None, 10_000, 1 << 16);
        assert_eq!(o.status, Status::OutputLimit);
    }

    #[test]
    fn memory_limit_kills() {
        // perl is on every macOS and Linux CI image; grow to ~400 MB.
        let args = vec![
            "-e".to_string(),
            "$x = 'a' x 400_000_000; sleep 5".to_string(),
        ];
        let o = run(&Spec {
            bin: Path::new("/usr/bin/perl"),
            arg0: None,
            args: &args,
            cwd: Path::new("/"),
            env: &[],
            stdin: None,
            timeout: Duration::from_secs(10),
            max_output: 1 << 16,
            max_rss: 100 << 20,
            merge: Merge::No,
            close_fds: &[],
        })
        .unwrap();
        assert_eq!(o.status, Status::MemoryLimit);
    }

    /// Whether the kernel dumps the shell's core is up to its settings (on
    /// GitHub's Linux runners systemd-coredump takes every one, whatever
    /// `ulimit -c` says), so either status is right; the signal is 11.
    #[test]
    fn signal_is_reported() {
        let o = sh("kill -SEGV $$", None, 5000, 1 << 16);
        assert!(
            matches!(o.status, Status::Signal(11) | Status::CoreDumped(11)),
            "{:?}",
            o.status
        );
    }

    /// With a core-size limit of 0 and a file `core_pattern`, a kernel dumps
    /// nothing; macOS always has a file pattern (and the harness sets the
    /// limit to 0 there anyway), so its status must say so.
    #[cfg(target_os = "macos")]
    #[test]
    fn no_core_is_dumped_on_macos() {
        let o = sh("kill -SEGV $$", None, 5000, 1 << 16);
        assert_eq!(o.status, Status::Signal(11));
    }

    #[test]
    fn environment_is_exactly_the_given_one() {
        let o = run(&Spec {
            bin: Path::new("/usr/bin/env"),
            arg0: None,
            args: &[],
            cwd: Path::new("/"),
            env: &[("B".into(), "2".into()), ("A".into(), "1".into())],
            stdin: None,
            timeout: Duration::from_secs(5),
            max_output: 1 << 16,
            max_rss: 1 << 30,
            merge: Merge::No,
            close_fds: &[],
        })
        .unwrap();
        // Command orders variables by name; both tools see the same order.
        assert_eq!(String::from_utf8(o.stdout).unwrap(), "A=1\nB=2\n");
    }
}
