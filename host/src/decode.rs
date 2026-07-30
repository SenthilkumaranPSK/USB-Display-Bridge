//! Raw H.264 Annex-B decode, no container.
//!
//! `screenrecord --output-format=h264` writes a bare elementary stream over
//! the pipe: NAL units with Annex-B start codes, no framing, no timestamps.
//! ffmpeg-next's `format` module expects a real AVIOContext-backed source
//! (a path or URL); we only have bytes arriving as we read them off the
//! child's stdout. So instead we drive libavcodec's own bitstream parser
//! (`av_parser_*`, reached through the `ffi` re-export) directly: feed it
//! however many bytes we just read, it hands back one complete access unit
//! at a time, and we send each to the decoder. This is the documented usage
//! pattern for `av_parser_parse2` in the FFmpeg headers.

use anyhow::{anyhow, Context, Result};
use ffmpeg_next as ffmpeg;
use ffmpeg_next::ffi;
use std::ptr;

/// FFmpeg's sentinel for "no timestamp." Not exposed as a constant by the
/// bindgen output, so reproduced here: `(int64_t)UINT64_C(0x8000000000000000)`.
const AV_NOPTS_VALUE: i64 = i64::MIN;

pub struct H264Decoder {
    parser: *mut ffi::AVCodecParserContext,
    decoder: ffmpeg::decoder::Video,
}

// `parser` is a raw pointer with no shared aliasing: only this struct's
// owner ever touches it, and ownership moves with the struct.
unsafe impl Send for H264Decoder {}

impl H264Decoder {
    pub fn new() -> Result<Self> {
        let decoder = ffmpeg::decoder::new()
            .open_as(ffmpeg::codec::Id::H264)
            .context("opening H.264 decoder")?
            .video()
            .context("H.264 codec did not open as a video decoder")?;

        let parser = unsafe { ffi::av_parser_init(ffi::AVCodecID::AV_CODEC_ID_H264 as i32) };
        if parser.is_null() {
            return Err(anyhow!("av_parser_init(H264) failed"));
        }

        Ok(Self { parser, decoder })
    }

    /// Feed newly read bytes in. The parser splits them into complete
    /// access units (handling NALs that straddle two reads); each complete
    /// unit is sent to the decoder, and any resulting frames are appended
    /// to `out`.
    pub fn push(&mut self, mut data: &[u8], out: &mut Vec<ffmpeg::frame::Video>) -> Result<()> {
        while !data.is_empty() {
            let mut out_buf: *mut u8 = ptr::null_mut();
            let mut out_size: i32 = 0;
            let consumed = unsafe {
                ffi::av_parser_parse2(
                    self.parser,
                    self.decoder.as_mut_ptr(),
                    &mut out_buf,
                    &mut out_size,
                    data.as_ptr(),
                    data.len() as i32,
                    AV_NOPTS_VALUE,
                    AV_NOPTS_VALUE,
                    0,
                )
            };
            if consumed < 0 {
                return Err(anyhow!("av_parser_parse2 failed: {consumed}"));
            }
            // consumed == 0 with no output would spin forever; the parser
            // never actually does this for well-formed Annex-B input, but
            // guard it explicitly rather than trust that.
            if consumed == 0 && out_size == 0 {
                return Err(anyhow!(
                    "H.264 parser made no progress on {} bytes",
                    data.len()
                ));
            }
            data = &data[consumed as usize..];

            if out_size > 0 && !out_buf.is_null() {
                let packet_bytes =
                    unsafe { std::slice::from_raw_parts(out_buf, out_size as usize) };
                let packet = ffmpeg::Packet::copy(packet_bytes);
                self.decoder.send_packet(&packet)?;
                self.drain_frames(out)?;
            }
        }
        Ok(())
    }

    fn drain_frames(&mut self, out: &mut Vec<ffmpeg::frame::Video>) -> Result<()> {
        loop {
            let mut frame = ffmpeg::frame::Video::empty();
            match self.decoder.receive_frame(&mut frame) {
                Ok(()) => out.push(frame),
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::sys::EAGAIN => break,
                Err(ffmpeg::Error::Eof) => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

impl Drop for H264Decoder {
    fn drop(&mut self) {
        unsafe { ffi::av_parser_close(self.parser) };
    }
}
