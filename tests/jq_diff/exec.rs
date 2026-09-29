//! Running one tool invocation with a timeout and output caps.

use std::io::{Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Status {
    Exit(i32),
    Signal(i32),
    Timeout,
    /// stdout or stderr exceeded the capture limit; the process was killed.
    OutputLimit,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Exit(c) => write!(f, "{c}"),
            Status::Signal(s) => write!(f, "signal {s}"),
            Status::Timeout => write!(f, "timeout"),
            Status::OutputLimit => write!(f, "output limit exceeded"),
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
}

pub struct Output {
    pub status: Status,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Stdin up to this size is written before reading output: it always fits in
/// the pipe buffer, so the write cannot block. Larger stdin gets a thread.
const INLINE_STDIN: usize = 8 * 1024;

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
    let mut cmd = Command::new(spec.bin);
    cmd.args(spec.args)
        .current_dir(spec.cwd)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", spec.bin.display()))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdin_pipe = child.stdin.take();
    let child = Arc::new(Mutex::new(child));
    let timed_out = Arc::new(AtomicBool::new(false));
    let over = Arc::new(AtomicBool::new(false));

    // The watchdog kills the child at the deadline. It is joined before the
    // child is reaped, so it can never signal a recycled pid.
    let (cancel, cancelled) = mpsc::channel::<()>();
    let watchdog = {
        let child = Arc::clone(&child);
        let timed_out = Arc::clone(&timed_out);
        let timeout = spec.timeout;
        thread::spawn(move || {
            if let Err(RecvTimeoutError::Timeout) = cancelled.recv_timeout(timeout) {
                timed_out.store(true, Ordering::SeqCst);
                kill(&child);
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

    let err_reader = {
        let child = Arc::clone(&child);
        let over = Arc::clone(&over);
        let cap = spec.max_output;
        thread::spawn(move || read_capped(stderr, cap, &child, &over))
    };
    let out = read_capped(stdout, spec.max_output, &child, &over);
    let err = err_reader.join().expect("stderr reader panicked");
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
    let status = if timed_out.load(Ordering::SeqCst) {
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
        })
        .unwrap()
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
        })
        .unwrap();
        // Command orders variables by name; both tools see the same order.
        assert_eq!(String::from_utf8(o.stdout).unwrap(), "A=1\nB=2\n");
    }
}
