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
    Signal(i32),
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
            Status::Timeout => write!(f, "timeout"),
            Status::OutputLimit => write!(f, "output limit exceeded"),
            Status::MemoryLimit => write!(f, "memory limit exceeded"),
        }
    }
}

pub struct Spec<'a> {
    pub bin: &'a Path,
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

fn kill(child: &Mutex<Child>) {
    if let Ok(mut c) = child.lock() {
        let _ = c.kill();
    }
}

/// Read until EOF, or until more than `cap` bytes arrived (then kill the child).
fn read_capped(mut r: impl Read, cap: usize, child: &Mutex<Child>, over: &AtomicBool) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if buf.len() + n > cap {
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

pub fn run(spec: &Spec) -> Result<Output, String> {
    let io_err = |e: std::io::Error| format!("{}: {e}", spec.bin.display());
    let mut cmd = Command::new(spec.bin);
    cmd.args(spec.args)
        .current_dir(spec.cwd)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
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
        // lock the watchdog needs to kill it.
        loop {
            let exited = child
                .lock()
                .map_err(|_| "child mutex poisoned".to_string())?
                .try_wait()
                .map_err(|e| format!("wait failed: {e}"))?
                .is_some();
            if exited {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        use std::io::{Seek, SeekFrom};
        let mut out = Vec::new();
        f.seek(SeekFrom::Start(0)).map_err(io_err)?;
        f.take(spec.max_output as u64 + 1)
            .read_to_end(&mut out)
            .map_err(io_err)?;
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
    let status = if mem_over.load(Ordering::SeqCst) {
        Status::MemoryLimit
    } else if timed_out.load(Ordering::SeqCst) {
        Status::Timeout
    } else if over.load(Ordering::SeqCst) {
        Status::OutputLimit
    } else if let Some(code) = status.code() {
        Status::Exit(code)
    } else {
        Status::Signal(status.signal().unwrap_or(-1))
    };
    Ok(Output {
        status,
        stdout: out,
        stderr: err,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, stdin: Option<&[u8]>, timeout_ms: u64, cap: usize) -> Output {
        let args = vec!["-c".to_string(), script.to_string()];
        run(&Spec {
            bin: Path::new("/bin/sh"),
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

    #[test]
    fn signal_is_reported() {
        let o = sh("kill -SEGV $$", None, 5000, 1 << 16);
        assert_eq!(o.status, Status::Signal(11));
    }

    #[test]
    fn environment_is_exactly_the_given_one() {
        let o = run(&Spec {
            bin: Path::new("/usr/bin/env"),
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
