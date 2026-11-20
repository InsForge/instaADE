//! Runs a local command with a deadline, so a stuck CLI cannot hang the app.

use std::ffi::OsStr;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Exit code (`None` when killed by a signal), stdout and stderr.
pub type Captured = (Option<i32>, Vec<u8>, Vec<u8>);

#[derive(Debug)]
pub enum RunError {
    /// The command could not start (not found, not executable).
    Spawn(std::io::Error),
    /// The command ran past the deadline and was killed.
    TimedOut,
}

/// A command for `program` that opens no console window on Windows.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Reads a pipe on a thread into a shared buffer; the flag flips at EOF.
fn spawn_reader<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
) -> (Arc<Mutex<Vec<u8>>>, Arc<AtomicBool>) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(AtomicBool::new(false));
    let (b, d) = (buf.clone(), done.clone());
    std::thread::spawn(move || {
        if let Some(mut p) = pipe {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = p.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                b.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        }
        d.store(true, Ordering::SeqCst);
    });
    (buf, done)
}

/// Spawns `cmd` and collects stdout and stderr, bounded by `timeout` for the whole
/// operation. Readers run on threads, so a background process that inherits a pipe
/// cannot block us: on timeout the child is killed. After exit we wait at most one
/// second for the pipes to close, then keep what arrived.
pub fn run_bounded(mut cmd: Command, timeout: Duration) -> Result<Captured, RunError> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(RunError::Spawn)?;
    let (out, out_done) = spawn_reader(child.stdout.take());
    let (err, err_done) = spawn_reader(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::TimedOut);
            }
        }
    };
    let grace = Instant::now()
        + Duration::from_secs(1).min(deadline.saturating_duration_since(Instant::now()));
    while Instant::now() < grace
        && !(out_done.load(Ordering::SeqCst) && err_done.load(Ordering::SeqCst))
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let o = out.lock().unwrap().clone();
    let e = err.lock().unwrap().clone();
    Ok((status.code(), o, e))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_stuck_command_is_killed_at_the_deadline() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "exec sleep 10"]);
        let start = Instant::now();
        assert!(matches!(
            run_bounded(cmd, Duration::from_millis(200)),
            Err(RunError::TimedOut)
        ));
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn output_and_code_come_back() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "echo out; echo err >&2; exit 3"]);
        let (code, out, err) = run_bounded(cmd, Duration::from_secs(5)).unwrap();
        assert_eq!(code, Some(3));
        assert_eq!(out, b"out\n");
        assert_eq!(err, b"err\n");
        assert!(matches!(
            run_bounded(Command::new("/nonexistent/x"), Duration::from_secs(1)),
            Err(RunError::Spawn(_))
        ));
    }
}
