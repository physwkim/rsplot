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
//!
//! Beyond PNG, the raster snapshot can also be exported as PPM (P6), SVG (a
//! base64 PNG `<image>`), and uncompressed baseline TIFF (with DPI resolution
//! tags), mirroring silx `PlotImageFile.saveImageToFile` /
//! `BackendBase.saveGraph(fileName, fileFormat, dpi)`. The `encode_*` helpers
//! are pure functions over the RGBA pixels so they are testable without a GPU
//! or the filesystem.
//!
//! DEFERRED (not implemented here): true *vector* export of plot primitives
//! (the SVG embeds the rendered raster rather than re-emitting geometry, which
//! would require the backend to record draw ops); JPEG/EPS/PDF/PS (the
//! matplotlib-only formats in `PlotWidget.saveGraph`); and matplotlib-parity
//! DPI scaling of the actual render (DPI is recorded in the TIFF resolution
//! tags but does not rescale the rendered pixel grid).

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

/// Drop the alpha channel of a tightly packed `width * height` RGBA8 buffer,
/// returning tightly packed `width * height` RGB8 (3 bytes per pixel).
///
/// silx's raster image export (`PlotImageFile.saveImageToFile`) operates on an
/// `(h, w, 3)` RGB array; the PPM body carries RGB, so the readback's RGBA is
/// reduced to RGB by discarding alpha.
pub fn rgba_to_rgb(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let n = (width as usize) * (height as usize);
    let mut out = Vec::with_capacity(n * 3);
    for px in rgba.chunks_exact(4).take(n) {
        out.push(px[0]);
        out.push(px[1]);
        out.push(px[2]);
    }
    out
}

/// Encode tightly packed `width * height` RGBA8 pixels as a binary (P6) PPM
/// byte stream.
///
/// Faithful to silx `PlotImageFile.saveImageToFile` (`fileFormat == "ppm"`):
/// the header is `P6\n<width> <height>\n255\n` followed by raw RGB bytes (the
/// alpha channel is dropped). The header is ASCII and self-describing.
pub fn encode_ppm(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let rgb = rgba_to_rgb(rgba, width, height);
    let header = format!("P6\n{width} {height}\n255\n");
    let mut out = Vec::with_capacity(header.len() + rgb.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&rgb);
    out
}

/// Standard base64 alphabet (RFC 4648), used by [`encode_svg`].
const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode bytes as standard (RFC 4648) base64 with `=` padding.
///
/// Implemented inline so the SVG export needs no external base64 crate
/// (mirrors silx using the stdlib `base64.b64encode`).
fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(BASE64_ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(BASE64_ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(BASE64_ALPHABET[((n >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(BASE64_ALPHABET[(n & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Encode tightly packed `width * height` RGB8 pixels (3 bytes/pixel) as PNG.
fn encode_rgb_png(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgb)?;
    }
    Ok(out)
}

/// Encode tightly packed `width * height` RGBA8 pixels as an SVG document that
/// embeds the raster as a base64 PNG `<image>`.
///
/// Faithful to silx `PlotImageFile.saveImageToFile` (`fileFormat == "svg"`):
/// the same XML declaration, SVG 1.1 DOCTYPE, root `<svg>` carrying `width`/
/// `height` in px, and a single `<image xlink:href="data:image/png;base64,…">`
/// placed at `x=0 y=0` with the same width/height and `id="image"`. silx tracks
/// no vector primitives in its raster path, so the rendered bitmap is embedded
/// rather than re-emitted as vector geometry (see Defer note in the module).
///
/// The embedded PNG is RGB (alpha dropped) to match silx's `(h, w, 3)` array.
pub fn encode_svg(rgba: &[u8], width: u32, height: u32) -> Result<String, png::EncodingError> {
    let rgb = rgba_to_rgb(rgba, width, height);
    let png = encode_rgb_png(&rgb, width, height)?;
    let b64 = base64_encode(&png);
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n");
    s.push_str("<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\"\n");
    s.push_str("  \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\">\n");
    s.push_str("<svg xmlns:xlink=\"http://www.w3.org/1999/xlink\"\n");
    s.push_str("     xmlns=\"http://www.w3.org/2000/svg\"\n");
    s.push_str("     version=\"1.1\"\n");
    s.push_str(&format!("     width=\"{width}\"\n"));
    s.push_str(&format!("     height=\"{height}\">\n"));
    s.push_str("    <image xlink:href=\"data:image/png;base64,");
    s.push_str(&b64);
    s.push_str("\"\n");
    s.push_str("           x=\"0\"\n");
    s.push_str("           y=\"0\"\n");
    s.push_str(&format!("           width=\"{width}\"\n"));
    s.push_str(&format!("           height=\"{height}\"\n"));
    s.push_str("           id=\"image\" />\n");
    s.push_str("</svg>");
    Ok(s)
}

/// Encode tightly packed `width * height` RGBA8 pixels as an uncompressed
/// baseline TIFF (RGB, alpha dropped) with the requested resolution.
///
/// Hand-written little-endian (`II`/42) baseline TIFF — no external crate. The
/// IFD carries the baseline required tags plus resolution:
///
/// - 256 ImageWidth (LONG), 257 ImageLength (LONG)
/// - 258 BitsPerSample (3 × SHORT = 8,8,8, stored out-of-line)
/// - 259 Compression = 1 (none)
/// - 262 PhotometricInterpretation = 2 (RGB)
/// - 273 StripOffsets (LONG, single strip)
/// - 277 SamplesPerPixel = 3
/// - 278 RowsPerStrip = `height`
/// - 279 StripByteCounts (LONG = width·height·3)
/// - 282 XResolution (RATIONAL = `dpi`/1), 283 YResolution (RATIONAL)
/// - 296 ResolutionUnit = 2 (inch)
///
/// Tags are emitted in ascending ID order as the baseline spec requires. This
/// extends silx's TIFF path (which delegates to fabio's `TiffIO.writeImage`)
/// with explicit DPI resolution tags, as `saveGraph(..., dpi)` requests.
pub fn encode_tiff(rgba: &[u8], width: u32, height: u32, dpi: u32) -> Vec<u8> {
    let rgb = rgba_to_rgb(rgba, width, height);
    let dpi = dpi.max(1);

    // 12 IFD entries: 256, 257, 258, 259, 262, 273, 277, 278, 279, 282, 283, 296.
    const N_ENTRIES: u16 = 12;
    // Header (8) + entry count (2) + entries (12·N) + next-IFD offset (4).
    let ifd_start: u32 = 8;
    let ifd_len: u32 = 2 + 12 * (N_ENTRIES as u32) + 4;
    // Out-of-line data that follows the IFD, in the order it is written:
    //   BitsPerSample (3 × SHORT = 6 bytes),
    //   XResolution (RATIONAL = 8 bytes), YResolution (RATIONAL = 8 bytes).
    let after_ifd: u32 = ifd_start + ifd_len;
    let bits_offset: u32 = after_ifd;
    let xres_offset: u32 = bits_offset + 6;
    let yres_offset: u32 = xres_offset + 8;
    let strip_offset: u32 = yres_offset + 8;
    let strip_byte_count: u32 = width * height * 3;

    let mut out: Vec<u8> = Vec::with_capacity(strip_offset as usize + rgb.len());

    // --- Image File Header (little-endian) ---
    out.extend_from_slice(b"II"); // byte order: little-endian
    out.extend_from_slice(&42u16.to_le_bytes()); // magic
    out.extend_from_slice(&ifd_start.to_le_bytes()); // offset of first IFD

    // --- Image File Directory ---
    out.extend_from_slice(&N_ENTRIES.to_le_bytes());

    // Helper closures append one 12-byte IFD entry.
    // type codes: 3 = SHORT, 4 = LONG, 5 = RATIONAL.
    let mut entry = |tag: u16, typ: u16, count: u32, value_or_offset: u32, is_short: bool| {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&typ.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        if is_short {
            // A single SHORT value is left-justified in the 4-byte field.
            out.extend_from_slice(&(value_or_offset as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
        } else {
            out.extend_from_slice(&value_or_offset.to_le_bytes());
        }
    };

    entry(256, 4, 1, width, false); // ImageWidth (LONG)
    entry(257, 4, 1, height, false); // ImageLength (LONG)
    entry(258, 3, 3, bits_offset, false); // BitsPerSample (3 SHORTs, out-of-line)
    entry(259, 3, 1, 1, true); // Compression = none
    entry(262, 3, 1, 2, true); // Photometric = RGB
    entry(273, 4, 1, strip_offset, false); // StripOffsets (LONG)
    entry(277, 3, 1, 3, true); // SamplesPerPixel = 3
    entry(278, 4, 1, height, false); // RowsPerStrip = height (one strip)
    entry(279, 4, 1, strip_byte_count, false); // StripByteCounts (LONG)
    entry(282, 5, 1, xres_offset, false); // XResolution (RATIONAL, out-of-line)
    entry(283, 5, 1, yres_offset, false); // YResolution (RATIONAL, out-of-line)
    entry(296, 3, 1, 2, true); // ResolutionUnit = inch

    out.extend_from_slice(&0u32.to_le_bytes()); // next IFD offset = 0 (last)

    // --- Out-of-line values ---
    // BitsPerSample: 8,8,8.
    out.extend_from_slice(&8u16.to_le_bytes());
    out.extend_from_slice(&8u16.to_le_bytes());
    out.extend_from_slice(&8u16.to_le_bytes());
    // XResolution = dpi/1.
    out.extend_from_slice(&dpi.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    // YResolution = dpi/1.
    out.extend_from_slice(&dpi.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());

    // --- Image data (single strip) ---
    debug_assert_eq!(out.len() as u32, strip_offset);
    out.extend_from_slice(&rgb);
    out
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
            plot.id,
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

    /// One parsed IFD entry: (type code, count, raw 4-byte value/offset field).
    type IfdEntry = (u16, u32, [u8; 4]);
    /// Map of TIFF tag ID → parsed IFD entry.
    type IfdTags = std::collections::HashMap<u16, IfdEntry>;
    /// Parsed baseline TIFF: (width, height, IFD tags, strip pixel bytes).
    type ParsedTiff = (u32, u32, IfdTags, Vec<u8>);

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

    #[test]
    fn rgba_to_rgb_drops_alpha() {
        // 2×1 RGBA → RGB; alpha bytes (4th of each quad) are removed.
        let rgba = [10, 20, 30, 99, 40, 50, 60, 88];
        let rgb = rgba_to_rgb(&rgba, 2, 1);
        assert_eq!(rgb, vec![10, 20, 30, 40, 50, 60]);
    }

    #[test]
    fn encode_ppm_header_and_pixels_round_trip() {
        // 2×1 image with distinct pixels.
        let rgba = [1, 2, 3, 255, 4, 5, 6, 255];
        let ppm = encode_ppm(&rgba, 2, 1);

        // Header is exactly "P6\n2 1\n255\n" then raw RGB.
        let header = b"P6\n2 1\n255\n";
        assert_eq!(&ppm[..header.len()], header);
        // Raw RGB body, alpha dropped.
        assert_eq!(&ppm[header.len()..], &[1, 2, 3, 4, 5, 6]);
        // Total length = header + width*height*3 (2×1 pixels × 3 channels).
        assert_eq!(ppm.len(), header.len() + 6);
    }

    #[test]
    fn base64_encode_matches_known_vector() {
        // RFC 4648 test vectors.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn encode_svg_is_well_formed_with_size_and_png_payload() {
        let rgba = [
            11, 22, 33, 255, 44, 55, 66, 255, 77, 88, 99, 255, 1, 2, 3, 255,
        ];
        let svg = encode_svg(&rgba, 2, 2).expect("svg");

        // XML declaration and SVG 1.1 DOCTYPE.
        assert!(svg.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>"));
        assert!(svg.contains("<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\""));
        // Root dimensions in px appear on the <svg> element.
        assert!(svg.contains("width=\"2\""));
        assert!(svg.contains("height=\"2\""));
        // The <image> element with a base64 PNG data URI and id.
        assert!(svg.contains("<image xlink:href=\"data:image/png;base64,"));
        assert!(svg.contains("x=\"0\""));
        assert!(svg.contains("y=\"0\""));
        assert!(svg.contains("id=\"image\" />"));
        assert!(svg.trim_end().ends_with("</svg>"));

        // The embedded payload decodes to a valid RGB PNG of the right size,
        // matching the input pixels (alpha dropped).
        let marker = "base64,";
        let start = svg.find(marker).expect("data uri") + marker.len();
        let end = svg[start..].find('"').expect("end quote") + start;
        let b64 = &svg[start..end];
        let png_bytes = base64_decode_for_test(b64);
        let decoder = png::Decoder::new(std::io::Cursor::new(&png_bytes));
        let mut reader = decoder.read_info().expect("read info");
        let mut buf = vec![0u8; reader.output_buffer_size().expect("buffer size")];
        let info = reader.next_frame(&mut buf).expect("frame");
        assert_eq!(info.width, 2);
        assert_eq!(info.height, 2);
        assert_eq!(info.color_type, png::ColorType::Rgb);
        let expected_rgb = rgba_to_rgb(&rgba, 2, 2);
        assert_eq!(&buf[..expected_rgb.len()], expected_rgb.as_slice());
    }

    /// Minimal base64 decoder for the SVG payload round-trip test.
    fn base64_decode_for_test(s: &str) -> Vec<u8> {
        fn val(c: u8) -> Option<u8> {
            match c {
                b'A'..=b'Z' => Some(c - b'A'),
                b'a'..=b'z' => Some(c - b'a' + 26),
                b'0'..=b'9' => Some(c - b'0' + 52),
                b'+' => Some(62),
                b'/' => Some(63),
                _ => None,
            }
        }
        let mut out = Vec::new();
        let mut acc = 0u32;
        let mut bits = 0u32;
        for &c in s.as_bytes() {
            if c == b'=' {
                break;
            }
            let Some(v) = val(c) else { continue };
            acc = (acc << 6) | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        out
    }

    // --- TIFF ---

    /// Minimal little-endian baseline-TIFF reader for tests: returns
    /// `(width, height, ifd_entries, pixel_bytes)` where `ifd_entries` maps
    /// tag → (type, count, raw 4-byte value/offset field) and `pixel_bytes` is
    /// the StripOffsets/StripByteCounts strip.
    fn parse_tiff(bytes: &[u8]) -> ParsedTiff {
        assert_eq!(&bytes[0..2], b"II", "byte order must be little-endian");
        assert_eq!(u16::from_le_bytes([bytes[2], bytes[3]]), 42, "magic 42");
        let ifd_off = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        let n = u16::from_le_bytes([bytes[ifd_off], bytes[ifd_off + 1]]) as usize;
        let mut tags = std::collections::HashMap::new();
        for i in 0..n {
            let base = ifd_off + 2 + i * 12;
            let tag = u16::from_le_bytes([bytes[base], bytes[base + 1]]);
            let typ = u16::from_le_bytes([bytes[base + 2], bytes[base + 3]]);
            let count = u32::from_le_bytes([
                bytes[base + 4],
                bytes[base + 5],
                bytes[base + 6],
                bytes[base + 7],
            ]);
            let val = [
                bytes[base + 8],
                bytes[base + 9],
                bytes[base + 10],
                bytes[base + 11],
            ];
            tags.insert(tag, (typ, count, val));
        }
        // The next-IFD pointer must be 0 (single image).
        let next_off = ifd_off + 2 + n * 12;
        assert_eq!(
            u32::from_le_bytes([
                bytes[next_off],
                bytes[next_off + 1],
                bytes[next_off + 2],
                bytes[next_off + 3]
            ]),
            0,
            "single-image TIFF: next IFD offset is 0"
        );

        let width = le_u32(&tags[&256].2);
        let height = le_u32(&tags[&257].2);
        let strip_off = le_u32(&tags[&273].2) as usize;
        let strip_len = le_u32(&tags[&279].2) as usize;
        let pixels = bytes[strip_off..strip_off + strip_len].to_vec();
        (width, height, tags, pixels)
    }

    fn le_u32(v: &[u8; 4]) -> u32 {
        u32::from_le_bytes(*v)
    }

    /// Read a SHORT value left-justified in a 4-byte IFD field.
    fn le_short(v: &[u8; 4]) -> u16 {
        u16::from_le_bytes([v[0], v[1]])
    }

    /// Read a RATIONAL (num/den) given the byte stream and its out-of-line
    /// offset (stored in the IFD value field).
    fn read_rational(bytes: &[u8], off: u32) -> (u32, u32) {
        let o = off as usize;
        let num = u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
        let den = u32::from_le_bytes([bytes[o + 4], bytes[o + 5], bytes[o + 6], bytes[o + 7]]);
        (num, den)
    }

    #[test]
    fn encode_tiff_header_tags_and_pixels_round_trip() {
        // 2×2 RGBA image, distinct pixels.
        let rgba = [
            10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255,
        ];
        let tiff = encode_tiff(&rgba, 2, 2, 96);
        let (w, h, tags, pixels) = parse_tiff(&tiff);

        assert_eq!((w, h), (2, 2));
        // Baseline required tags.
        assert_eq!(le_short(&tags[&259].2), 1, "Compression = none");
        assert_eq!(le_short(&tags[&262].2), 2, "Photometric = RGB");
        assert_eq!(le_short(&tags[&277].2), 3, "SamplesPerPixel = 3");
        assert_eq!(le_u32(&tags[&278].2), 2, "RowsPerStrip = height");
        assert_eq!(le_u32(&tags[&279].2), 2 * 2 * 3, "StripByteCounts = w*h*3");

        // BitsPerSample is 3 SHORTs stored out-of-line; verify 8,8,8.
        let (typ, count, bits_val) = tags[&258];
        assert_eq!(typ, 3);
        assert_eq!(count, 3);
        let bits_off = le_u32(&bits_val) as usize;
        assert_eq!(
            &tiff[bits_off..bits_off + 6],
            &[8, 0, 8, 0, 8, 0],
            "BitsPerSample = 8,8,8"
        );

        // Pixel bytes round-trip as RGB (alpha dropped).
        let expected_rgb = rgba_to_rgb(&rgba, 2, 2);
        assert_eq!(pixels, expected_rgb);
    }

    #[test]
    fn encode_tiff_resolution_tags_reflect_dpi() {
        let rgba = [1, 2, 3, 255];
        let tiff = encode_tiff(&rgba, 1, 1, 300);
        let (_, _, tags, _) = parse_tiff(&tiff);

        // ResolutionUnit = 2 (inch).
        assert_eq!(le_short(&tags[&296].2), 2, "ResolutionUnit = inch");
        // XResolution / YResolution are RATIONAL = 300/1.
        let xres = read_rational(&tiff, le_u32(&tags[&282].2));
        let yres = read_rational(&tiff, le_u32(&tags[&283].2));
        assert_eq!(xres, (300, 1), "XResolution = 300 dpi");
        assert_eq!(yres, (300, 1), "YResolution = 300 dpi");
    }

    #[test]
    fn encode_tiff_clamps_zero_dpi_to_one() {
        // dpi = 0 would write a 0/1 rational; clamp to 1 so the tag is valid.
        let rgba = [1, 2, 3, 255];
        let tiff = encode_tiff(&rgba, 1, 1, 0);
        let (_, _, tags, _) = parse_tiff(&tiff);
        assert_eq!(read_rational(&tiff, le_u32(&tags[&282].2)), (1, 1));
    }
}
