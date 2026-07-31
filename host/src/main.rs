//! USB Display Bridge host.
//!
//! Session 0: scaffold + adb handshake. Two subcommands:
//!   * `devices`     - list devices `adb` sees
//!   * `test-tunnel` - push a file, open a reverse tunnel, sleep, tear it down
//!
//! No video code yet. The next milestone (M1) wires `screenrecord` through
//! ffmpeg-next and SDL.

mod adb;
mod capture;
mod decode;
mod render;
mod screencap;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tracing::{info, trace, warn};
use tracing_subscriber::EnvFilter;

/// Extend mode's device-app package/activity (see device-app/) and wire
/// protocol version -- see docs/protocol.md.
const EXTEND_PACKAGE: &str = "dev.usbdisplaybridge.extend";
const EXTEND_ACTIVITY: &str = ".MainActivity";
const EXTEND_PROTOCOL_VERSION: u8 = 1;

#[derive(Debug, Parser)]
#[command(
    name = "usb-display-bridge",
    version,
    about = "Wired USB display bridge between an Android phone and a desktop PC."
)]
struct Cli {
    /// ADB device serial. Required when more than one device is connected.
    #[arg(long, global = true)]
    serial: Option<String>,

    /// TCP port on the host used for the control/data tunnels.
    #[arg(long, global = true, default_value_t = 27183)]
    port: u16,

    /// Enable verbose logging. Repeat for trace-level.
    #[arg(long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// List connected adb devices.
    Devices,
    /// Open a reverse tunnel, verify it, then close it. Acceptance test for M0.
    TestTunnel,
    /// M1 throwaway mirror: `screenrecord` -> ffmpeg decode -> SDL window.
    /// Expect 1-3s of lag; that's screenrecord's own buffering. M2 replaces
    /// this source entirely, so it isn't worth chasing here.
    Mirror {
        #[arg(long, default_value_t = 1280)]
        width: u32,
        #[arg(long, default_value_t = 720)]
        height: u32,
    },
    /// M6 extend mode: capture a Windows display and stream it. See
    /// docs/extend-mode.md before running this without --preview.
    Extend {
        /// DXGI adapter/output index to capture (ddagrab's `output_idx`).
        /// Does not necessarily match Display Settings' "1"/"2" labels --
        /// use --preview to find the right one empirically.
        #[arg(long, default_value_t = 0)]
        output_idx: u32,
        #[arg(long, default_value_t = 60)]
        framerate: u32,
        /// Decode and render locally instead of installing/streaming to the
        /// phone. No adb, no phone, no virtual display driver required --
        /// this is Stage A's verification path for the capture+encode
        /// pipeline on its own.
        #[arg(long)]
        preview: bool,
    },
}

fn main() -> ExitCode {
    // Ctrl-C: rely on the default SIGINT handler, which lets the runtime
    // unwind. TunnelGuard::drop runs during that unwind and removes the
    // reverse tunnel.

    if let Err(e) = real_main() {
        eprintln!("error: {e:#}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    let adb = adb::locate_adb().context("locating adb")?;
    info!(adb = %adb.display(), "using adb");

    match cli.cmd {
        Cmd::Devices => cmd_devices(&adb),
        Cmd::TestTunnel => cmd_test_tunnel(&adb, &cli),
        Cmd::Mirror { width, height } => cmd_mirror(&adb, &cli, width, height),
        Cmd::Extend {
            output_idx,
            framerate,
            preview,
        } => cmd_extend(&adb, &cli, output_idx, framerate, preview),
    }
}

fn init_tracing(verbose: u8) {
    // -v -> info, -vv -> debug, -vvv -> trace. Default is warn so we are not
    // noisy on success but useful when something goes wrong.
    let level = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("usb_display_bridge={level}")));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time() // keep output scannable for acceptance
        .init();
}

fn cmd_devices(adb: &std::path::Path) -> Result<()> {
    let devs = adb::list_devices(adb)?;
    if devs.is_empty() {
        println!("(no devices)");
        return Ok(());
    }
    for d in devs {
        println!("{}\t{}", d.serial, d.state);
    }
    Ok(())
}

fn cmd_test_tunnel(adb: &std::path::Path, cli: &Cli) -> Result<()> {
    let dev = adb::select_device(adb, cli.serial.as_deref())?;
    info!(serial = %dev.serial, port = cli.port, "selected device");

    // Step 1: push a small file to prove push works. CARGO_MANIFEST_DIR
    // (not cwd) so this works regardless of where `cargo run` is invoked
    // from.
    let probe: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let pushed = adb::push_file(adb, &dev.serial, &probe)
        .with_context(|| format!("pushing {}", probe.display()))?;
    info!(remote = %pushed.display(), "pushed probe file");

    // Step 2: open a tunnel and confirm it is listed.
    let tunnel_name = "usdb_test";
    let guard = adb::TunnelGuard::open(adb, &dev.serial, tunnel_name, cli.port)?;
    let listed = adb::list_reverse(adb, &dev.serial)?;
    if !listed.contains(tunnel_name) {
        anyhow::bail!(
            "tunnel opened but `adb reverse --list` does not contain {tunnel_name}:\n{listed}"
        );
    }
    println!("reverse tunnel open: {tunnel_name} -> tcp:{}", cli.port);
    println!("press Ctrl-C to abort, or waiting 2s to test clean teardown...");
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Step 3: tear it down. Drop runs the same code as `close()` would,
    // and the explicit `drop(...)` makes the acceptance test unambiguous.
    drop(guard);

    let after = adb::list_reverse(adb, &dev.serial)?;
    if after.lines().any(|l| l.contains(tunnel_name)) {
        anyhow::bail!("tunnel still listed after close:\n{after}");
    }
    println!("tunnel closed cleanly.");
    Ok(())
}

/// One decoded picture, copied out of ffmpeg's buffers into plain `Vec`s so
/// it can cross the thread boundary without dragging ffmpeg types (and
/// their non-`Send` raw pointers) along with it.
struct FrameBuf {
    width: u32,
    height: u32,
    y: Vec<u8>,
    y_stride: usize,
    u: Vec<u8>,
    u_stride: usize,
    v: Vec<u8>,
    v_stride: usize,
}

impl FrameBuf {
    /// Fails loudly if the decoder ever hands back something other than
    /// 4:2:0 -- that would silently corrupt the picture otherwise, since
    /// the renderer assumes YUV420P plane layout.
    fn from_yuv420p(frame: &ffmpeg_next::frame::Video) -> Result<Self> {
        use ffmpeg_next::format::Pixel;
        if frame.format() != Pixel::YUV420P {
            anyhow::bail!(
                "decoded frame is {:?}, expected YUV420P -- mirror only handles 4:2:0",
                frame.format()
            );
        }
        Ok(Self {
            width: frame.width(),
            height: frame.height(),
            y: frame.data(0).to_vec(),
            y_stride: frame.stride(0),
            u: frame.data(1).to_vec(),
            u_stride: frame.stride(1),
            v: frame.data(2).to_vec(),
            v_stride: frame.stride(2),
        })
    }
}

/// Single-slot mailbox: at most one undisplayed frame ever waits here. A
/// fresh frame overwrites (drops) whatever was in it, per CLAUDE.md hard
/// constraint 2 -- a growing queue is a bug, never backpressure.
type FrameSlot = Arc<(Mutex<Option<FrameBuf>>, Condvar)>;

fn cmd_mirror(adb: &Path, cli: &Cli, width: u32, height: u32) -> Result<()> {
    let dev = adb::select_device(adb, cli.serial.as_deref())?;
    info!(serial = %dev.serial, width, height, "starting mirror (M1 naive, screenrecord source)");

    let (capture, mut stdout) = capture::Capture::start(adb, &dev.serial, width, height)
        .context("starting screenrecord capture")?;

    let slot: FrameSlot = Arc::new((Mutex::new(None), Condvar::new()));
    let decode_slot = Arc::clone(&slot);
    let decode_thread = std::thread::Builder::new()
        .name("decode".into())
        .spawn(move || decode_loop(&mut stdout, &decode_slot))
        .context("spawning decode thread")?;

    let mut display = render::open("USB Display Bridge -- mirror (naive, M1)", width, height)?;
    let texture_creator = render::texture_creator(&display);
    let mut texture: Option<sdl2::render::Texture> = None;
    let mut texture_dims = (0u32, 0u32);

    loop {
        if render::should_quit(&mut display.event_pump) {
            break;
        }

        let frame = {
            let (mutex, cvar) = &*slot;
            let guard = mutex.lock().unwrap();
            let mut guard = cvar
                .wait_timeout(guard, Duration::from_millis(16))
                .unwrap()
                .0;
            guard.take()
        };

        let Some(frame) = frame else { continue };

        if texture.is_none() || texture_dims != (frame.width, frame.height) {
            texture = Some(render::new_texture(
                &texture_creator,
                frame.width,
                frame.height,
            )?);
            texture_dims = (frame.width, frame.height);
        }

        render::present(
            &mut display.canvas,
            texture.as_mut().unwrap(),
            render::YuvPlanes {
                y: &frame.y,
                y_stride: frame.y_stride,
                u: &frame.u,
                u_stride: frame.u_stride,
                v: &frame.v,
                v_stride: frame.v_stride,
            },
        )?;
    }

    // Kill screenrecord/adb before joining: the decode thread is blocked on
    // a read from the pipe, and killing the child is what makes that read
    // return EOF so the thread can exit.
    drop(capture);
    if let Err(e) = decode_thread.join() {
        warn!(?e, "decode thread panicked");
    }

    Ok(())
}

fn cmd_extend(adb: &Path, cli: &Cli, output_idx: u32, framerate: u32, preview: bool) -> Result<()> {
    if preview {
        return cmd_extend_preview(output_idx, framerate);
    }
    cmd_extend_stream(adb, cli, output_idx, framerate)
}

/// Stage B: the real path. Installs and launches device-app, waits for it
/// to connect back over the reverse tunnel, then streams
/// `[u32 BE length][Annex-B bytes]` frames -- see docs/protocol.md.
///
/// No SDL window here (unlike --preview), so there is no in-process "quit"
/// event to watch for. This runs until the phone disconnects (the app is
/// closed, the cable is unplugged) or a write fails, at which point it
/// cleans up and returns -- Ctrl-C from the terminal is not guaranteed to
/// run that cleanup, matching the rest of this codebase's current
/// signal-handling gap rather than introducing a new one.
fn cmd_extend_stream(adb: &Path, cli: &Cli, output_idx: u32, framerate: u32) -> Result<()> {
    let apk = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("device-app/app/build/outputs/apk/debug/app-debug.apk");
    if !apk.is_file() {
        anyhow::bail!(
            "device-app APK not found at {} -- run `cd device-app && ./gradlew assembleDebug` first",
            apk.display()
        );
    }

    let dev = adb::select_device(adb, cli.serial.as_deref())?;
    info!(serial = %dev.serial, output_idx, framerate, port = cli.port, "starting extend mode");

    // Device-app connects to 127.0.0.1:<cli.port> on-device; the reverse
    // tunnel forwards that to our listener on the same port number here.
    // Reusing one port number for both ends is a deliberate v1
    // simplification -- device-app's connect-out port is currently a
    // hardcoded constant matching cli.port's default (see
    // device-app/.../MainActivity.kt), not passed via intent extra, so
    // overriding --port also requires updating and rebuilding device-app.
    // Documented as a known rough edge in docs/protocol.md.
    let listener = TcpListener::bind(("127.0.0.1", cli.port))
        .with_context(|| format!("binding 127.0.0.1:{}", cli.port))?;

    let tunnel =
        adb::TunnelGuard::open_remote(adb, &dev.serial, &format!("tcp:{}", cli.port), cli.port)?;

    adb::install_apk(adb, &dev.serial, &apk).context("installing device-app")?;

    let result = (|| -> Result<()> {
        adb::start_activity(adb, &dev.serial, EXTEND_PACKAGE, EXTEND_ACTIVITY)
            .context("launching device-app")?;

        info!(port = cli.port, "waiting for device-app to connect back");
        let (mut stream, peer) = listener
            .accept()
            .context("accepting device-app connection")?;
        stream.set_nodelay(true).context("setting TCP_NODELAY")?;
        info!(%peer, "device-app connected");

        stream
            .write_all(&[EXTEND_PROTOCOL_VERSION])
            .context("sending version byte")?;

        let mut encoder = screencap::ScreenEncoder::new(output_idx, framerate)
            .context("starting display capture")?;

        loop {
            let Some(packet) = encoder.next_packet()? else {
                continue;
            };
            let len = u32::try_from(packet.data.len()).context("packet too large to frame")?;
            trace!(len, keyframe = packet.keyframe, "sending frame");
            stream
                .write_all(&len.to_be_bytes())
                .context("writing frame length")?;
            stream
                .write_all(&packet.data)
                .context("writing frame payload")?;
        }
    })();

    // Always tear down the installed app before the tunnel guard drops,
    // regardless of how the loop above exited -- nothing persists on the
    // phone after a session, per CLAUDE.md's non-goal on installed apps.
    adb::force_stop(adb, &dev.serial, EXTEND_PACKAGE);
    adb::uninstall(adb, &dev.serial, EXTEND_PACKAGE);
    drop(tunnel);

    result
}

/// Stage A's verification path: capture and encode a display, decode our
/// own output, and render it locally -- no adb, no phone, no virtual
/// display driver required. Validates the ddagrab filter graph and encoder
/// setup in isolation before any of that plumbing gets involved.
fn cmd_extend_preview(output_idx: u32, framerate: u32) -> Result<()> {
    info!(
        output_idx,
        framerate, "extend preview: capturing display locally, no adb/phone involved"
    );

    let mut encoder =
        screencap::ScreenEncoder::new(output_idx, framerate).context("starting display capture")?;
    let mut decoder = decode::H264Decoder::new().context("opening H.264 decoder")?;
    let mut frames = Vec::new();

    // The SDL window can't be sized until we know the capture's actual
    // resolution, which depends on whatever display `output_idx` selects --
    // so block for the first decoded frame before opening it.
    let first = loop {
        let Some(packet) = encoder.next_packet()? else {
            continue;
        };
        frames.clear();
        decoder.push(&packet.data, &mut frames)?;
        if let Some(f) = frames.drain(..).next() {
            break f;
        }
    };

    let (width, height) = (first.width(), first.height());
    let mut display = render::open("USB Display Bridge -- extend preview", width, height)?;
    let texture_creator = render::texture_creator(&display);
    let mut texture = render::new_texture(&texture_creator, width, height)?;
    let mut texture_dims = (width, height);

    let fb = FrameBuf::from_yuv420p(&first)?;
    render::present(
        &mut display.canvas,
        &mut texture,
        render::YuvPlanes {
            y: &fb.y,
            y_stride: fb.y_stride,
            u: &fb.u,
            u_stride: fb.u_stride,
            v: &fb.v,
            v_stride: fb.v_stride,
        },
    )?;

    loop {
        if render::should_quit(&mut display.event_pump) {
            break;
        }

        let Some(packet) = encoder.next_packet()? else {
            continue;
        };
        frames.clear();
        decoder.push(&packet.data, &mut frames)?;

        for f in frames.drain(..) {
            let fb = FrameBuf::from_yuv420p(&f)?;
            if texture_dims != (fb.width, fb.height) {
                texture = render::new_texture(&texture_creator, fb.width, fb.height)?;
                texture_dims = (fb.width, fb.height);
            }
            render::present(
                &mut display.canvas,
                &mut texture,
                render::YuvPlanes {
                    y: &fb.y,
                    y_stride: fb.y_stride,
                    u: &fb.u,
                    u_stride: fb.u_stride,
                    v: &fb.v,
                    v_stride: fb.v_stride,
                },
            )?;
        }
    }

    Ok(())
}

fn decode_loop(stdout: &mut std::process::ChildStdout, slot: &FrameSlot) {
    let mut decoder = match decode::H264Decoder::new() {
        Ok(d) => d,
        Err(e) => {
            warn!(?e, "failed to open H.264 decoder");
            return;
        }
    };

    let mut buf = [0u8; 64 * 1024];
    let mut frames = Vec::new();
    loop {
        let n = match stdout.read(&mut buf) {
            Ok(0) => {
                info!("screenrecord stream ended");
                return;
            }
            Ok(n) => n,
            Err(e) => {
                warn!(?e, "reading screenrecord stream");
                return;
            }
        };

        frames.clear();
        if let Err(e) = decoder.push(&buf[..n], &mut frames) {
            warn!(?e, "decode error");
            return;
        }

        for f in frames.drain(..) {
            match FrameBuf::from_yuv420p(&f) {
                Ok(fb) => {
                    let (mutex, cvar) = &**slot;
                    *mutex.lock().unwrap() = Some(fb);
                    cvar.notify_one();
                }
                Err(e) => {
                    warn!(?e, "dropping frame");
                }
            }
        }
    }
}
