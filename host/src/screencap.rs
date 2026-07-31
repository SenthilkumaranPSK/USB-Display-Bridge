//! PC-side display capture + H.264 encode for extend mode (M6).
//!
//! Captures a Windows display via FFmpeg's `ddagrab` avfilter (Desktop
//! Duplication API) and encodes it with libx264, tuned per CLAUDE.md's hard
//! constraints: no B-frames, no lookahead, baseline profile, H.264 default.
//! `next_packet` is a single synchronous pull-encode-return call -- no
//! queue, so a slow consumer backpressures the capture rate itself rather
//! than anything buffering (hard constraint 2).

use anyhow::{anyhow, Context as _, Result};
use ffmpeg_next as ffmpeg;
use ffmpeg_next::{codec, encoder, filter, format, Dictionary};
use tracing::{info, warn};

pub struct EncodedPacket {
    pub data: Vec<u8>,
    pub keyframe: bool,
}

pub struct ScreenEncoder {
    output_idx: u32,
    framerate: u32,
    graph: filter::Graph,
    enc: Option<encoder::video::Encoder>,
    dims: (u32, u32),
}

impl ScreenEncoder {
    pub fn new(output_idx: u32, framerate: u32) -> Result<Self> {
        let graph = build_graph(output_idx, framerate)?;
        Ok(Self {
            output_idx,
            framerate,
            graph,
            enc: None,
            dims: (0, 0),
        })
    }

    /// Pull one captured frame -- this is what actually drives `ddagrab`'s
    /// capture, paced internally against `framerate` -- and return its
    /// encoded packet if the encoder produced one this call. `Ok(None)` is
    /// normal at stream start, before the encoder has enough input to emit.
    pub fn next_packet(&mut self) -> Result<Option<EncodedPacket>> {
        let mut frame = ffmpeg::frame::Video::empty();
        if let Err(e) = pull_frame(&mut self.graph, &mut frame) {
            // Desktop Duplication invalidates itself for a bunch of benign
            // reasons -- a UAC prompt, the lock screen, a GPU mode switch,
            // a resolution change (observed live: DXGI_ERROR_ACCESS_LOST,
            // "AcquireNextFrame failed: 887a0026", after ~20s of clean
            // capture). The documented recovery is to recreate the
            // duplication interface, which for us means rebuilding the
            // whole filter graph. One retry covers the transient case; if
            // that also fails, this is a real problem worth surfacing
            // rather than looping on it forever.
            warn!(error = %e, "capture frame failed, rebuilding capture graph and retrying once");
            self.graph = build_graph(self.output_idx, self.framerate)
                .context("rebuilding capture graph after frame-pull failure")?;
            pull_frame(&mut self.graph, &mut frame)
                .context("pulling captured frame after graph rebuild")?;
        }

        let dims = (frame.width(), frame.height());
        if self.enc.is_none() || self.dims != dims {
            if self.enc.is_some() {
                warn!(?dims, prev = ?self.dims, "display size changed, rebuilding encoder");
            }
            self.enc = Some(open_encoder(dims.0, dims.1, self.framerate)?);
            self.dims = dims;
        }
        let enc = self.enc.as_mut().expect("just set above");

        enc.send_frame(&frame).context("sending frame to encoder")?;

        let mut packet = ffmpeg::Packet::empty();
        match enc.receive_packet(&mut packet) {
            Ok(()) => Ok(Some(EncodedPacket {
                data: packet.data().unwrap_or(&[]).to_vec(),
                keyframe: packet.is_key(),
            })),
            Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::sys::EAGAIN => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

fn pull_frame(graph: &mut filter::Graph, frame: &mut ffmpeg::frame::Video) -> Result<()> {
    graph
        .get("sink")
        .ok_or_else(|| anyhow!("filter graph missing sink"))?
        .sink()
        .frame(frame)
        .context("pulling captured frame")
}

fn build_graph(output_idx: u32, framerate: u32) -> Result<filter::Graph> {
    let ddagrab = filter::find("ddagrab").ok_or_else(|| {
        anyhow!("this ffmpeg build has no ddagrab filter -- Windows Desktop Duplication capture is unavailable")
    })?;
    let hwdownload = filter::find("hwdownload")
        .ok_or_else(|| anyhow!("this ffmpeg build has no hwdownload filter"))?;
    let fmt_filter =
        filter::find("format").ok_or_else(|| anyhow!("this ffmpeg build has no format filter"))?;
    let buffersink = filter::find("buffersink")
        .ok_or_else(|| anyhow!("this ffmpeg build has no buffersink filter"))?;

    let mut graph = filter::Graph::new();
    let mut src = graph
        .add(
            &ddagrab,
            "src",
            &format!("output_idx={output_idx}:framerate={framerate}"),
        )
        .context("adding ddagrab source")?;
    let mut hwdl = graph
        .add(&hwdownload, "hwdl", "")
        .context("adding hwdownload filter")?;
    // `hwdownload` only copies GPU memory to CPU memory -- it cannot
    // itself change pixel format. It has to be told the exact format the
    // D3D11 surface actually is (BGRA for Desktop Duplication); asking it
    // for yuv420p directly fails ("Invalid output format yuv420p for
    // hwframe download"), and leaving it unconstrained doesn't make it
    // pick something sensible either (observed picking AV_PIX_FMT_MONOWHITE,
    // enum value 0 -- an uninitialized-looking default, not a real choice).
    let mut native = graph
        .add(&fmt_filter, "native", "pix_fmts=bgra")
        .context("adding native-format filter")?;
    // The actual yuv420p conversion happens here, between two ordinary
    // software filters -- this is a real swscale conversion, unlike the
    // hw-to-sw boundary above which cannot convert at all.
    let mut fmt = graph
        .add(&fmt_filter, "fmt", "pix_fmts=yuv420p")
        .context("adding format filter")?;
    let mut sink = graph
        .add(&buffersink, "sink", "")
        .context("adding buffersink")?;

    src.link(0, &mut hwdl, 0);
    hwdl.link(0, &mut native, 0);
    native.link(0, &mut fmt, 0);
    fmt.link(0, &mut sink, 0);
    graph.validate().context("configuring filter graph")?;

    info!(output_idx, framerate, "display capture graph ready");
    Ok(graph)
}

fn open_encoder(width: u32, height: u32, framerate: u32) -> Result<encoder::video::Encoder> {
    // Named to avoid shadowing the `codec` module path used just below.
    let h264 =
        encoder::find(codec::Id::H264).ok_or_else(|| anyhow!("no H.264 encoder available"))?;
    let mut ctx = codec::Context::new_with_codec(h264)
        .encoder()
        .video()
        .context("opening encoder context as video")?;

    ctx.set_width(width);
    ctx.set_height(height);
    ctx.set_format(format::Pixel::YUV420P);
    ctx.set_time_base((1, framerate as i32));
    // Hard constraint 4: no B-frames, no lookahead, baseline/main profile.
    ctx.set_max_b_frames(0);
    ctx.set_gop(framerate * 2);

    let mut opts = Dictionary::new();
    opts.set("preset", "ultrafast");
    opts.set("tune", "zerolatency");
    opts.set("profile", "baseline");
    // repeat-headers: SPS/PPS ride inline before every IDR, so the wire
    // format (docs/protocol.md) needs no separate config message.
    // rc-lookahead=0 makes constraint 4's "no lookahead" explicit rather
    // than left implicit in the zerolatency tune.
    opts.set("x264-params", "repeat-headers=1:rc-lookahead=0");

    ctx.open_as_with(h264, opts)
        .context("opening H.264 encoder")
}
