//! M1 throwaway video source: the device's built-in `screenrecord`, piped
//! straight over `adb exec-out`. No reverse tunnel needed here since
//! `exec-out` streams the child's stdout back to us directly.
//!
//! This is deliberately temporary. `screenrecord` buffers internally (the
//! 1-3s of lag CLAUDE.md warns about), and M2 replaces this whole module
//! with our own MediaCodec server. Do not try to tune it.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread;
use tracing::{debug, warn};

/// Handle on a running `screenrecord` capture. Dropping it kills and reaps
/// the child so no zombie `adb` process is left behind. The stdout pipe is
/// handed out separately by `start` so it can be moved into a reader
/// thread while this handle stays with whoever controls shutdown.
pub struct Capture {
    child: Child,
}

impl Capture {
    /// Start `adb -s <serial> exec-out screenrecord --output-format=h264
    /// --size <width>x<height> -`. stderr is drained on a background thread
    /// and logged, since `screenrecord` writes status/errors there and an
    /// unread pipe can fill up and stall the child. Returns the handle and
    /// the child's stdout pipe.
    pub fn start(adb: &Path, serial: &str, width: u32, height: u32) -> Result<(Self, ChildStdout)> {
        let size = format!("{width}x{height}");
        let mut child = Command::new(adb)
            .args([
                "-s",
                serial,
                "exec-out",
                "screenrecord",
                "--output-format=h264",
                "--size",
                &size,
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawning adb exec-out screenrecord")?;

        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        thread::Builder::new()
            .name("screenrecord-stderr".into())
            .spawn(move || drain_stderr(stderr))
            .context("spawning stderr drain thread")?;

        debug!(width, height, "screenrecord capture started");
        Ok((Self { child }, stdout))
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Err(e) = self.child.kill() {
            // Already exited is not a problem; anything else is worth a warning.
            if e.kind() != std::io::ErrorKind::InvalidInput {
                warn!(?e, "failed to kill screenrecord child");
            }
        }
        if let Err(e) = self.child.wait() {
            warn!(?e, "failed to reap screenrecord child");
        } else {
            debug!("screenrecord child reaped");
        }
    }
}

fn drain_stderr(stderr: std::process::ChildStderr) {
    use std::io::{BufRead, BufReader};
    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
        warn!(target: "screenrecord", "{line}");
    }
}
