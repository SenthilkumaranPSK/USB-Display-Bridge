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
        .ok_or_else(|| anyhow!("local path has no filename: {}", local.display()))?;
    let remote = PathBuf::from(DEVICE_TMP).join(name);
    let remote_str = remote.to_string_lossy().into_owned();
    let local_str = local.to_string_lossy().into_owned();
    run(adb, ["-s", serial, "push", &local_str, &remote_str]).context("adb push failed")?;
    Ok(remote)
}

/// RAII guard for `adb reverse localabstract:<name> tcp:<port>`. Removing on
/// drop means Ctrl-C, normal exit, and panics all tear the tunnel down.
/// If the process is killed outright (SIGKILL), `adb` itself clears reverse
/// tunnels on client disconnect, so the tunnel does not leak.
pub struct TunnelGuard<'a> {
    adb: &'a Path,
    serial: &'a str,
    name: String,
}

impl<'a> TunnelGuard<'a> {
    pub fn open(adb: &'a Path, serial: &'a str, name: &str, port: u16) -> Result<Self> {
        let port_s = port.to_string();
        run(
            adb,
            [
                "-s",
                serial,
                "reverse",
                &format!("localabstract:{name}"),
                &format!("tcp:{port_s}"),
            ],
        )
        .context("adb reverse failed")?;
        debug!(name, port, "reverse tunnel open");
        Ok(Self {
            adb,
            serial,
            name: name.to_string(),
        })
    }
}

impl Drop for TunnelGuard<'_> {
    fn drop(&mut self) {
        let target = format!("localabstract:{}", self.name);
        if let Err(e) = run(
            self.adb,
            ["-s", self.serial, "reverse", "--remove", &target],
        ) {
            warn!(?e, name = self.name, "failed to remove reverse tunnel");
        } else {
            debug!(name = self.name, "reverse tunnel closed");
        }
    }
}

/// Confirm no reverse tunnels remain on the device. Used by the test-tunnel
/// subcommand to prove clean teardown.
pub fn list_reverse(adb: &Path, serial: &str) -> Result<String> {
    run(adb, ["-s", serial, "reverse", "--list"])
}
