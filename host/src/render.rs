//! Minimal SDL2 window for the M1 throwaway mirror. Frames are painted as
//! soon as they arrive with no vsync pacing and no jitter buffer, per
//! CLAUDE.md's latency constraints. M2's renderer is the one that gets
//! actually tuned; this one just has to prove decode -> display works.

use anyhow::{Context, Result};
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::PixelFormatEnum;
use sdl2::render::{Canvas, Texture, TextureCreator};
use sdl2::video::{Window, WindowContext};
use sdl2::{EventPump, Sdl};

/// Everything needed to keep the SDL video subsystem alive. `Sdl` and
/// `EventPump` have no useful methods once you have them, but dropping
/// either early tears down the subsystem out from under `canvas`.
pub struct Display {
    _sdl: Sdl,
    pub event_pump: EventPump,
    pub canvas: Canvas<Window>,
}

pub fn open(title: &str, width: u32, height: u32) -> Result<Display> {
    let sdl = sdl2::init().map_err(|e| anyhow::anyhow!("sdl2::init: {e}"))?;
    let video = sdl
        .video()
        .map_err(|e| anyhow::anyhow!("sdl video subsystem: {e}"))?;
    let window = video
        .window(title, width, height)
        .position_centered()
        .resizable()
        .build()
        .context("creating SDL window")?;
    let mut canvas = window
        .into_canvas()
        .accelerated()
        .build()
        .context("creating SDL canvas")?;
    // Letterbox on resize instead of stretching the picture.
    canvas
        .set_logical_size(width, height)
        .context("setting canvas logical size")?;
    let event_pump = sdl
        .event_pump()
        .map_err(|e| anyhow::anyhow!("sdl event pump: {e}"))?;
    Ok(Display {
        _sdl: sdl,
        event_pump,
        canvas,
    })
}

pub fn texture_creator(win: &Display) -> TextureCreator<WindowContext> {
    win.canvas.texture_creator()
}

pub fn new_texture<'a>(
    creator: &'a TextureCreator<WindowContext>,
    width: u32,
    height: u32,
) -> Result<Texture<'a>> {
    creator
        .create_texture_streaming(PixelFormatEnum::IYUV, width, height)
        .context("creating streaming YUV texture")
}

/// True if the window should close: the close button, or Escape/Q.
pub fn should_quit(event_pump: &mut EventPump) -> bool {
    for event in event_pump.poll_iter() {
        match event {
            Event::Quit { .. } => return true,
            Event::KeyDown {
                keycode: Some(Keycode::Escape | Keycode::Q),
                ..
            } => return true,
            _ => {}
        }
    }
    false
}

/// One planar YUV 4:2:0 plane triple, borrowed from wherever the caller
/// keeps the decoded frame.
pub struct YuvPlanes<'a> {
    pub y: &'a [u8],
    pub y_stride: usize,
    pub u: &'a [u8],
    pub u_stride: usize,
    pub v: &'a [u8],
    pub v_stride: usize,
}

/// Upload a planar YUV 4:2:0 frame into `texture` and present it. Plane
/// order and pitches are the caller's responsibility to get right; this
/// function is a thin wrapper over `SDL_UpdateYUVTexture` + present.
pub fn present(
    canvas: &mut Canvas<Window>,
    texture: &mut Texture,
    planes: YuvPlanes,
) -> Result<()> {
    texture
        .update_yuv(
            None,
            planes.y,
            planes.y_stride,
            planes.u,
            planes.u_stride,
            planes.v,
            planes.v_stride,
        )
        .map_err(|e| anyhow::anyhow!("texture update_yuv: {e}"))?;
    canvas.clear();
    canvas
        .copy(texture, None, None)
        .map_err(|e| anyhow::anyhow!("canvas copy: {e}"))?;
    canvas.present();
    Ok(())
}
