//! Thin wrapper around the `adb` command-line tool.
//!
//! Design notes:
//! - We invoke `adb` as a child process rather than linking a USB/IP library
//!   directly. The reverse-tunnel mechanics that this project depends on are
//!   not exposed by any stable host-side API; the `adb` binary is the surface
//!   that scrcpy and the Android team use, and it ships as part of the
//!   platform-tools package. Reusing it keeps us free of libusb bookkeeping.
//! - All public functions return `anyhow::Result` so callers can attach
//!   context without ceremony. Internally we keep the raw `String` from
//!   stderr so error messages include what `adb` actually said.
//! - `TunnelGuard::drop` runs `adb reverse --remove` on unwind (Ctrl-C,
//!   normal exit, panic). For a SIGKILL or taskkill /f, the OS closes our
//!   adb connection and `adbd` clears reverse tunnels on client disconnect,
//!   so the tunnel does not leak. The Drop body itself does not panic, so
//!   the unwind path is reliable.

use anyhow::{anyhow, bail, Context, Result};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tracing::{debug, trace, warn};

/// Where on the device we push server artifacts. `shell` can write here but
/// world cannot, so another app cannot swap the jar out before launch.
pub const DEVICE_TMP: &str = "/data/local/tmp";

/// Locate the `adb` binary. Tries PATH first, then a few well-known install
/// locations, then `ANDROID_HOME`/`ANDROID_SDK_ROOT`.
pub fn locate_adb() -> Result<PathBuf> {
    // PATH first via `where` on Windows, `which` on Unix. We avoid the
    // `which` crate to keep the dependency surface minimal.
    if let Some(p) = find_on_path("adb") {
        return Ok(p);
    }

    let well_known = [
        // Windows: standalone Android SDK install.
        r"C:\Program Files\Android\platform-tools\adb.exe",
        // Windows: Android Studio default user install (matches this machine).
        r"C:\Users\SENTHILKUMARAN\AppData\Local\Android\Sdk\platform-tools\adb.exe",
        // Linux / WSL.
        "/opt/android-sdk/platform-tools/adb",
        "/usr/bin/adb",
        // macOS Homebrew.
        "/opt/homebrew/bin/adb",
        "/usr/local/bin/adb",
    ];
    for candidate in well_known {
        if Path::new(candidate).is_file() {
            return Ok(PathBuf::from(candidate));
        }
    }

    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(root) = std::env::var_os(var) {
            let p = PathBuf::from(root).join("platform-tools").join(adb_exe());
            if p.is_file() {
                return Ok(p);
            }
        }
    }

    Err(anyhow!(
        "could not find `adb`. Install Android platform-tools, add it to PATH, \
         or set ANDROID_HOME."
    ))
}

#[cfg(windows)]
fn adb_exe() -> &'static str {
    "adb.exe"
}
#[cfg(not(windows))]
fn adb_exe() -> &'static str {
    "adb"
}

/// One connected device. `serial` is what `adb` uses to identify it; `state`
/// is `device`, `unauthorized`, `offline`, etc.
#[derive(Debug, Clone)]
pub struct Device {
    pub serial: String,
    pub state: String,
}

/// Run `adb <args>` and return stdout as a `String`. stderr is folded into the
/// error if the command exits non-zero.
fn run<I, S>(adb: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new(adb);
    cmd.args(args);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    debug!(?cmd, "running adb");
    let out = cmd.output().context("failed to spawn adb")?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    trace!(?stdout, ?stderr, "adb output");
    if !out.status.success() {
        let trimmed_stderr = stderr.trim();
        if trimmed_stderr.is_empty() {
            bail!("adb exited with status {}", out.status);
        }
        bail!("adb failed: {}", trimmed_stderr);
    }
    Ok(stdout)
}

/// Search PATH for `name`. Returns the first match with the platform-correct
/// executable suffix.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        });
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// List connected devices. Output is parsed from `adb devices -l`.
pub fn list_devices(adb: &Path) -> Result<Vec<Device>> {
    let raw = run(adb, ["devices", "-l"])?;
    let mut devs = Vec::new();
    for line in raw.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Format: "<serial>  <state>  <key:value ...>" or just
        //         "<serial>  <state>". Split on whitespace, the second token
        //         is state, everything after is key:value pairs we ignore.
        let mut it = line.split_whitespace();
        let Some(serial) = it.next() else { continue };
        let Some(state) = it.next() else { continue };
        if state != "device" {
            warn!(%serial, %state, "skipping non-ready device");
            continue;
        }
        devs.push(Device {
            serial: serial.to_string(),
            state: state.to_string(),
        });
    }
    Ok(devs)
}

/// Pick the device to use. Errors clearly if the count doesn't match what
/// the user asked for.
pub fn select_device(adb: &Path, serial: Option<&str>) -> Result<Device> {
    let devs = list_devices(adb)?;
    match (devs.len(), serial) {
        (0, _) => bail!("no connected devices. Plug in a phone and enable USB debugging."),
        (1, _) => Ok(devs.into_iter().next().unwrap()),
        (_, Some(s)) => devs
            .into_iter()
            .find(|d| d.serial == s)
            .ok_or_else(|| anyhow!("--serial {s} not found among connected devices")),
        (n, None) => bail!(
            "{n} devices connected; pass --serial <id> to choose one. \
             Run `cargo run -- devices` to list them."
        ),
    }
}

/// Push a local file to /data/local/tmp on the device. Returns the remote
/// path it landed at (same basename, under DEVICE_TMP).
pub fn push_file(adb: &Path, serial: &str, local: &Path) -> Result<PathBuf> {
    let name = local
        .file_name()
        .ok_or_else(|| anyhow!("local path has no filename: {}", local.display()))?
        .to_string_lossy();
    // Remote paths are always POSIX (this runs on the device's shell), so we
    // join with `/` explicitly rather than `PathBuf::join`, which would use
    // `\` on a Windows host and produce a single bad filename on the device.
    let remote_str = format!("{DEVICE_TMP}/{name}");
    let local_str = local.to_string_lossy().into_owned();
    run(adb, ["-s", serial, "push", &local_str, &remote_str]).context("adb push failed")?;
    Ok(PathBuf::from(remote_str))
}

/// RAII guard for `adb reverse <remote_spec> tcp:<port>`. Removing on drop
/// means Ctrl-C, normal exit, and panics all tear the tunnel down. If the
/// process is killed outright (SIGKILL), `adb` itself clears reverse
/// tunnels on client disconnect, so the tunnel does not leak.
pub struct TunnelGuard<'a> {
    adb: &'a Path,
    serial: &'a str,
    remote_spec: String,
}

impl<'a> TunnelGuard<'a> {
    /// General form. The mirror path's Java server uses Android's
    /// `LocalSocket` (abstract Unix socket), reached via
    /// `localabstract:<name>` -- see `open()` below. A normal installed app
    /// like extend mode's Kotlin receiver just does `Socket("127.0.0.1",
    /// port)`, reached via `tcp:<port>` instead, so the remote spec has to
    /// be caller-supplied rather than hardcoded to one form.
    pub fn open_remote(
        adb: &'a Path,
        serial: &'a str,
        remote_spec: &str,
        host_port: u16,
    ) -> Result<Self> {
        run(
            adb,
            [
                "-s",
                serial,
                "reverse",
                remote_spec,
                &format!("tcp:{host_port}"),
            ],
        )
        .context("adb reverse failed")?;
        debug!(remote_spec, host_port, "reverse tunnel open");
        Ok(Self {
            adb,
            serial,
            remote_spec: remote_spec.to_string(),
        })
    }

    pub fn open(adb: &'a Path, serial: &'a str, name: &str, port: u16) -> Result<Self> {
        Self::open_remote(adb, serial, &format!("localabstract:{name}"), port)
    }
}

impl Drop for TunnelGuard<'_> {
    fn drop(&mut self) {
        if let Err(e) = run(
            self.adb,
            ["-s", self.serial, "reverse", "--remove", &self.remote_spec],
        ) {
            warn!(?e, remote_spec = %self.remote_spec, "failed to remove reverse tunnel");
        } else {
            debug!(remote_spec = %self.remote_spec, "reverse tunnel closed");
        }
    }
}

/// `adb install -r <apk>`. `-r` so a stale install from a previous crashed
/// session doesn't block a fresh one.
pub fn install_apk(adb: &Path, serial: &str, apk: &Path) -> Result<()> {
    let apk_str = apk.to_string_lossy().into_owned();
    run(adb, ["-s", serial, "install", "-r", &apk_str]).context("adb install failed")?;
    Ok(())
}

/// `adb uninstall <package>`. Errors are logged, not propagated -- session
/// teardown should not fail the whole run because uninstall raced with the
/// device being unplugged or the app was never installed.
pub fn uninstall(adb: &Path, serial: &str, package: &str) {
    if let Err(e) = run(adb, ["-s", serial, "uninstall", package]) {
        warn!(?e, package, "failed to uninstall device-app");
    }
}

/// `adb shell am start -n <package>/<activity>`.
pub fn start_activity(adb: &Path, serial: &str, package: &str, activity: &str) -> Result<()> {
    let component = format!("{package}/{activity}");
    run(
        adb,
        ["-s", serial, "shell", "am", "start", "-n", &component],
    )
    .context("adb shell am start failed")?;
    Ok(())
}

/// `adb shell am force-stop <package>`. Used on teardown before uninstall:
/// `am start` on a still-running Activity does not restart it cleanly, and
/// uninstalling while it's still running can leave a dangling process.
/// Same "log, don't propagate" reasoning as `uninstall`.
pub fn force_stop(adb: &Path, serial: &str, package: &str) {
    if let Err(e) = run(adb, ["-s", serial, "shell", "am", "force-stop", package]) {
        warn!(?e, package, "failed to force-stop device-app");
    }
}

/// Confirm no reverse tunnels remain on the device. Used by the test-tunnel
/// subcommand to prove clean teardown.
pub fn list_reverse(adb: &Path, serial: &str) -> Result<String> {
    run(adb, ["-s", serial, "reverse", "--list"])
}
