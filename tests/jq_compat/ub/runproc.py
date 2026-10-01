"""Run a jq/qj probe so that nothing outlives it.

Every child starts in its own session (so it leads its own process group),
and on a timeout the whole group is killed with SIGKILL, not just the child
or the wait. The groups still running are killed too when the runner itself
exits, returns from an exception, or gets SIGTERM, SIGINT or SIGHUP; a
runner should be stopped with SIGTERM, never SIGKILL, so that happens.

    status, stdout, stderr = run(argv, stdin=b"...", env={...}, timeout=3)

`status` is "exit N", "signal N" or "timeout".
"""
import atexit
import os
import signal
import subprocess
import sys
import threading

_live = set()
_lock = threading.Lock()


def _kill_group(pid):
    try:
        os.killpg(pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass


def kill_all():
    with _lock:
        pids = list(_live)
    for pid in pids:
        _kill_group(pid)


def live_pids():
    with _lock:
        return list(_live)


def _on_signal(signum, frame):
    kill_all()
    sys.exit(128 + signum)


atexit.register(kill_all)
for _s in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
    try:
        signal.signal(_s, _on_signal)
    except ValueError:  # not the main thread
        pass


def run(argv, *, stdin=None, env=None, timeout=3, preexec=None, cwd=None, executable=None):
    if isinstance(stdin, str):
        stdin = stdin.encode()
    p = subprocess.Popen(argv, executable=executable, cwd=cwd, env=env,
                         stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                         start_new_session=True, preexec_fn=preexec)
    with _lock:
        _live.add(p.pid)
    try:
        try:
            out, err = p.communicate(stdin, timeout=timeout)
        except subprocess.TimeoutExpired:
            _kill_group(p.pid)
            out, err = p.communicate()
            return "timeout", out, err
        rc = p.returncode
        if rc >= 0:
            return f"exit {rc}", out, err
        return f"signal {-rc}", out, err
    finally:
        with _lock:
            _live.discard(p.pid)
