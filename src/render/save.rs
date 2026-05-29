//! Save the current plot view to a PNG (silx `saveGraph`).
//!
//! [`save_graph`] renders the data layer (background clear, image, curves) for
//! the plot's current limits into an offscreen texture at a chosen pixel size,
//! copies it back to the CPU, and writes a PNG. This captures the wgpu-rendered
//! data layer; the egui-drawn chrome (axes, ticks, colorbar) is not included
//! (`doc/design.md` §13 E1).
//!
//! The readback's row stride is padded to `COPY_BYTES_PER_ROW_ALIGNMENT`, and
//! the bytes are converted to tightly packed RGBA8 (swapping channels when the
//! surface format is BGRA). The pure byte-layout and PNG-encoding helpers are
//! unit-tested; the GPU render + readback runs only with a real device.

use std::fmt;
use std::path::Path;

use egui::{Pos2, Rect};
use egui_wgpu::{RenderState, wgpu};

use crate::core::plot::Plot;
use crate::core::transform::Scale;
use crate::render::backend_wgpu::WgpuResources;

/// Why a [`save_graph`] call failed.
#[derive(Debug)]
pub enum SaveError {
    /// Writing the PNG file to disk failed.
    Io(std::io::Error),
    /// Encoding the pixels as PNG failed.
    Encode(png::EncodingError),
    /// The GPU readback (buffer map / device poll) failed, or the size was zero.
    Readback(String),
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveError::Io(e) => write!(f, "save_graph: writing PNG: {e}"),
            SaveError::Encode(e) => write!(f, "save_graph: encoding PNG: {e}"),
            SaveError::Readback(e) => write!(f, "save_graph: GPU readback: {e}"),
        }
    }
}

impl std::error::Error for SaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SaveError::Io(e) => Some(e),
            SaveError::Encode(e) => Some(e),
            SaveError::Readback(_) => None,
        }
    }
}

impl From<std::io::Error> for SaveError {
    fn from(e: std::io::Error) -> Self {
        SaveError::Io(e)
    }
}

impl From<png::EncodingError> for SaveError {
    fn from(e: png::EncodingError) -> Self {
        SaveError::Encode(e)
    }
}

/// Row stride, in bytes, for an `width`-pixel RGBA8 row padded up to wgpu's
/// `COPY_BYTES_PER_ROW_ALIGNMENT` (the alignment `copy_texture_to_buffer`
/// requires).
pub(crate) fn padded_bytes_per_row(width: u32) -> u32 {
    let unpadded = 4 * width;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    unpadded.div_ceil(align) * align
}

/// Convert a mapped readback buffer (rows padded to `bytes_per_row`) into a
/// tightly packed `width * height * 4` RGBA8 image, swapping R/B when the
/// surface format is BGRA so the output is always RGBA.
pub(crate) fn rows_to_rgba8(
    mapped: &[u8],
    width: u32,
    height: u32,
    bytes_per_row: u32,
    format: wgpu::TextureFormat,
) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let bpr = bytes_per_row as usize;
    let row_bytes = w * 4;
    let swap = matches!(
        format,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
    );

    let mut out = vec![0u8; w * h * 4];
    for row in 0..h {
        let src = &mapped[row * bpr..row * bpr + row_bytes];
        let dst = &mut out[row * row_bytes..(row + 1) * row_bytes];
        if swap {
            for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                d[3] = s[3];
            }
        } else {
            dst.copy_from_slice(src);
        }
    }
    out
}

/// Encode tightly packed `width * height` RGBA8 pixels as a PNG byte stream.
pub fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(out)
}

/// Per-axis log flags `[x, y]` (1.0 = log10) for the shaders, matching a
/// transform's scales.
fn axis_log_flags(t: &crate::core::transform::Transform) -> [f32; 2] {
    [
        f32::from(t.x.scale == Scale::Log10),
        f32::from(t.y.scale == Scale::Log10),
    ]
}

/// Render the plot's current view to a `size = (width, height)` pixel PNG at
/// `path`. Captures the data layer (clear + image + curves); chrome is not
/// included. Requires [`crate::install`] to have run on `render_state`.
pub fn save_graph(
    render_state: &RenderState,
    plot: &Plot,
    size: (u32, u32),
    path: impl AsRef<Path>,
) -> Result<(), SaveError> {
    let (w, h) = size;
    if w == 0 || h == 0 {
        return Err(SaveError::Readback("zero-size target".into()));
    }

    // Build the transform for a target-sized area. The ortho mapping is area
    // independent, but the viewport pixel size drives the line-width expansion.
    let area = Rect::from_min_size(Pos2::ZERO, egui::vec2(w as f32, h as f32));
    let transform = plot.transform(area);
    let transform_right = plot.transform_y2(area);
    let ortho_left = transform.ortho_matrix();
    let axis_log_left = axis_log_flags(&transform);
    let (ortho_right, axis_log_right) = match &transform_right {
        Some(t) => (t.ortho_matrix(), axis_log_flags(t)),
        None => (ortho_left, axis_log_left),
    };
    let bg = egui::Rgba::from(plot.data_background).to_array();

    let rgba = {
        let renderer = render_state.renderer.read();
        let res: &WgpuResources = renderer
            .callback_resources
            .get()
            .expect("WgpuResources not installed — call egui_silx::install() first");
        res.render_to_rgba(
            &render_state.device,
            &render_state.queue,
            render_state.target_format,
            size,
            bg,
            ortho_left,
            axis_log_left,
            ortho_right,
            axis_log_right,
        )?
    };

    let png = encode_png(&rgba, w, h)?;
    std::fs::write(path, png)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_per_row_rounds_up_to_256() {
        assert_eq!(padded_bytes_per_row(1), 256); // 4 → 256
        assert_eq!(padded_bytes_per_row(64), 256); // 256 → 256 (exact)
        assert_eq!(padded_bytes_per_row(65), 512); // 260 → 512
        assert_eq!(padded_bytes_per_row(100), 512); // 400 → 512
    }

    #[test]
    fn rows_to_rgba8_unpads_and_passes_rgba_through() {
        // 1×2 image, row stride padded to 256. Rgba format → no channel swap.
        let bpr = padded_bytes_per_row(1);
        let mut mapped = vec![0u8; (bpr as usize) * 2];
        mapped[0..4].copy_from_slice(&[10, 20, 30, 40]); // row 0
        mapped[bpr as usize..bpr as usize + 4].copy_from_slice(&[50, 60, 70, 80]); // row 1
        let out = rows_to_rgba8(&mapped, 1, 2, bpr, wgpu::TextureFormat::Rgba8UnormSrgb);
        assert_eq!(out, vec![10, 20, 30, 40, 50, 60, 70, 80]);
    }

    #[test]
    fn rows_to_rgba8_swaps_bgra_to_rgba() {
        let bpr = padded_bytes_per_row(1);
        let mut mapped = vec![0u8; bpr as usize];
        mapped[0..4].copy_from_slice(&[30, 20, 10, 40]); // stored BGRA
        let out = rows_to_rgba8(&mapped, 1, 1, bpr, wgpu::TextureFormat::Bgra8UnormSrgb);
        assert_eq!(out, vec![10, 20, 30, 40]); // → RGBA
    }

    #[test]
    fn encode_png_round_trips() {
        // 2×2 RGBA encoded then decoded yields the same pixels.
        let rgba: Vec<u8> = (0..16).map(|i| i as u8 * 16).collect();
        let png = encode_png(&rgba, 2, 2).expect("encode");

        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = decoder.read_info().expect("read info");
        let mut buf = vec![0u8; reader.output_buffer_size().expect("buffer size")];
        let info = reader.next_frame(&mut buf).expect("frame");
        assert_eq!(info.width, 2);
        assert_eq!(info.height, 2);
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(&buf[..rgba.len()], rgba.as_slice());
    }
}
