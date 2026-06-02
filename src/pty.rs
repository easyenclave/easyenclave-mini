//! PTY allocation for tty-backed workloads and attach sessions. Replaces the
//! `script -qfc` shell-out (and thus busybox's `script` applet) with
//! `openpty` + a `pre_exec` that makes the slave the controlling terminal.
//!
//! This is tokio-safe: `Command` does the fork/exec, so we never manually
//! `fork()` in the multithreaded runtime. The `pre_exec` closure only calls
//! async-signal-safe libc functions (`setsid`, `ioctl`).

use std::io;
use tokio::process::{Child, Command};

/// Spawn `program args...` attached to a fresh PTY. Returns the child and the
/// master side: reading it yields the child's combined stdout+stderr; writing
/// it feeds the child's stdin. `envs` are extra environment overrides layered
/// on the inherited environment.
pub fn spawn_on_pty(
    program: &str,
    args: &[&str],
    envs: &[(String, String)],
) -> io::Result<(Child, std::fs::File)> {
    let pty = nix::pty::openpty(None, None).map_err(|e| io::Error::from_raw_os_error(e as i32))?;
    // Three independent slave handles for the child's stdin/stdout/stderr.
    let slave_in = pty.slave.try_clone()?;
    let slave_out = pty.slave.try_clone()?;
    let slave_err = pty.slave.try_clone()?;

    let mut cmd = Command::new(program);
    cmd.args(args)
        .env("TERM", "xterm-256color")
        .stdin(slave_in)
        .stdout(slave_out)
        .stderr(slave_err);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    // Safety: only async-signal-safe calls. After Command wires the slave to
    // fds 0/1/2, start a new session and adopt the controlling terminal.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    // Parent keeps only the master; dropping the original slave (the three
    // clones moved into the child) lets the master see EOF when the child exits.
    drop(pty.slave);
    Ok((child, std::fs::File::from(pty.master)))
}
