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
//! base64 PNG `<image>`), uncompressed baseline TIFF (with DPI resolution
//! tags), EPS (an embedded `colorimage` raster), and PDF (a single-page
//! `/DeviceRGB` image XObject), mirroring silx `PlotImageFile.saveImageToFile`
//! / `BackendBase.saveGraph(fileName, fileFormat, dpi)`.
//! [`save_graph_with_format`] dispatches by [`SaveFormat`], and each `encode_*`
//! helper is a pure function over the RGBA pixels so it is testable without a
//! GPU or the filesystem.
//!
//! DEFERRED (not implemented here): true *vector* export of plot primitives
//! (the SVG/EPS/PDF embed the rendered raster rather than re-emitting geometry,
//! which would require the backend to record draw ops); and matplotlib-parity
//! DPI scaling of the actual render (DPI is recorded in the TIFF resolution
//! tags / JFIF density but does not rescale the rendered pixel grid). JPEG is
//! covered by the hand-written baseline encoder in [`crate::render::jpeg`].

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

/// Encode a 2D `uint8` array `(height, width)` (row-major / C-order) as a NumPy
/// `.npy` v1.0 byte stream.
///
/// Mirrors `numpy.save` of a `uint8` array (silx `MaskToolsWidget.save(..,
/// "npy")`): a `\x93NUMPY` magic, a `\x01\x00` version, a little-endian `u16`
/// header length, then the ASCII header dict
/// `{'descr': '|u1', 'fortran_order': False, 'shape': (h, w), }` padded with
/// spaces so the whole preamble length is a multiple of 64 and terminated by a
/// newline, then the raw C-order bytes. Pure (no GPU / filesystem) so it is
/// directly unit-testable; `data` is expected to be `height * width` bytes long.
pub fn encode_mask_npy(height: u32, width: u32, data: &[u8]) -> Vec<u8> {
    const MAGIC: &[u8] = b"\x93NUMPY";
    let header =
        format!("{{'descr': '|u1', 'fortran_order': False, 'shape': ({height}, {width}), }}");
    // Preamble = magic(6) + version(2) + header-len(2) + header + '\n',
    // padded with spaces so the whole preamble length is a multiple of 64.
    let unpadded = MAGIC.len() + 2 + 2 + header.len() + 1;
    let pad = (64 - (unpadded % 64)) % 64;
    let header_len = header.len() + pad + 1; // padding + trailing newline
    debug_assert!(header_len <= u16::MAX as usize);

    let mut out = Vec::with_capacity(unpadded + pad + data.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[1u8, 0u8]); // version 1.0
    out.extend_from_slice(&(header_len as u16).to_le_bytes());
    out.extend_from_slice(header.as_bytes());
    out.extend(std::iter::repeat_n(b' ', pad));
    out.push(b'\n');
    out.extend_from_slice(data);
    out
}

/// Decode a 2D `uint8` NumPy `.npy` byte stream into `(height, width, data)` in
/// C (row-major) order.
///
/// Accepts only `descr` of `|u1` / `<u1` / `>u1` / `u1` (uint8) with
/// `fortran_order: False` and a 2D shape — what [`encode_mask_npy`] /
/// `numpy.save` of a mask produces. Any other dtype, Fortran order,
/// dimensionality, truncated body, or malformed header is an
/// [`std::io::ErrorKind::InvalidData`] error. Pure over a byte stream so the
/// round-trip is directly unit-testable.
pub fn decode_mask_npy(bytes: &[u8]) -> std::io::Result<(u32, u32, Vec<u8>)> {
    use std::io::Read;
    let mut r = bytes;
    let invalid = |msg: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, msg.to_string());

    let mut magic = [0u8; 6];
    r.read_exact(&mut magic)?;
    if &magic != b"\x93NUMPY" {
        return Err(invalid("not a .npy file (bad magic)"));
    }

    let mut version = [0u8; 2];
    r.read_exact(&mut version)?;
    // Header length is u16 (v1.0) or u32 (v2.0+); support both.
    let header_len = if version[0] >= 2 {
        let mut len = [0u8; 4];
        r.read_exact(&mut len)?;
        u32::from_le_bytes(len) as usize
    } else {
        let mut len = [0u8; 2];
        r.read_exact(&mut len)?;
        u16::from_le_bytes(len) as usize
    };

    let mut header_bytes = vec![0u8; header_len];
    r.read_exact(&mut header_bytes)?;
    let header =
        std::str::from_utf8(&header_bytes).map_err(|_| invalid("npy header is not UTF-8"))?;

    let descr =
        npy_header_field(header, "descr").ok_or_else(|| invalid("npy header missing 'descr'"))?;
    // uint8: '|u1' is canonical; tolerate explicit endianness markers.
    if !matches!(descr.as_str(), "|u1" | "<u1" | ">u1" | "u1") {
        return Err(invalid("npy mask must be uint8 ('|u1')"));
    }

    let fortran = npy_header_field(header, "fortran_order")
        .ok_or_else(|| invalid("npy header missing 'fortran_order'"))?;
    if fortran != "False" {
        return Err(invalid("npy mask must be C-order (fortran_order: False)"));
    }

    let (height, width) = npy_shape_2d(header)?;

    let count = (height as usize) * (width as usize);
    let mut data = vec![0u8; count];
    r.read_exact(&mut data)?;
    Ok((height, width, data))
}

/// Extract the value of a `key` from a NumPy `.npy` header dict literal,
/// stripping surrounding quotes (so `'|u1'` becomes `|u1` and the bare literal
/// `False` stays `False`). Returns `None` if the key is absent.
fn npy_header_field(header: &str, key: &str) -> Option<String> {
    // Match `'key':` then take up to the next ',' or '}'.
    let needle = format!("'{key}':");
    let start = header.find(&needle)? + needle.len();
    let rest = &header[start..];
    let end = rest.find([',', '}'])?;
    let value = rest[..end].trim();
    Some(value.trim_matches(['\'', '"']).to_string())
}

/// Parse the `shape` tuple of a 2D NumPy `.npy` header into `(height, width)`.
///
/// Rejects shapes that are not exactly 2D, matching silx's mask load which only
/// handles 2D image masks.
fn npy_shape_2d(header: &str) -> std::io::Result<(u32, u32)> {
    let invalid = |msg: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, msg.to_string());
    let start = header
        .find("'shape':")
        .ok_or_else(|| invalid("npy header missing 'shape'"))?
        + "'shape':".len();
    let rest = &header[start..];
    let open = rest.find('(').ok_or_else(|| invalid("malformed shape"))?;
    let close = rest.find(')').ok_or_else(|| invalid("malformed shape"))?;
    let dims: Vec<u32> = rest[open + 1..close]
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<u32>())
        .collect::<Result<_, _>>()
        .map_err(|_| invalid("non-integer shape dimension"))?;
    if dims.len() != 2 {
        return Err(invalid("npy mask must be 2D"));
    }
    Ok((dims[0], dims[1]))
}

/// Encode a 2D `uint8` mask `(height, width)` (row-major / C-order) as an ESRF
/// Data Format (EDF) byte stream.
///
/// Mirrors `fabio.edfimage` writing of a `uint8` image (silx
/// `MaskToolsWidget.save(.., "edf")`): an ASCII header block opening with `{`,
/// one `KEY = VALUE ;` line per field (`HeaderID`/`Image`/`ByteOrder`/
/// `DataType`/`Dim_1`/`Dim_2`/`Size`), space-padded so the header block length
/// (through the closing `}\n`) is a multiple of 512 bytes, then the raw C-order
/// pixel bytes. `Dim_1` is the fast axis (width / columns) and `Dim_2` the slow
/// axis (height / rows), so the byte layout matches the row-major mask directly
/// with no transpose. Pure (no GPU / filesystem); `data` is expected to be
/// `height * width` bytes long.
pub fn encode_mask_edf(height: u32, width: u32, data: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 512;
    let size = (height as usize) * (width as usize);
    let mut header = String::from("{\n");
    header.push_str("HeaderID = EH:000001:000000:000000 ;\n");
    header.push_str("Image = 1 ;\n");
    header.push_str("ByteOrder = LowByteFirst ;\n");
    header.push_str("DataType = UnsignedByte ;\n");
    header.push_str(&format!("Dim_1 = {width} ;\n"));
    header.push_str(&format!("Dim_2 = {height} ;\n"));
    header.push_str(&format!("Size = {size} ;\n"));
    // Pad with spaces so the block (including the closing "}\n") is 512-aligned.
    let unpadded = header.len() + 2; // + "}\n"
    let pad = (BLOCK - (unpadded % BLOCK)) % BLOCK;
    header.extend(std::iter::repeat_n(' ', pad));
    header.push_str("}\n");

    let mut out = Vec::with_capacity(header.len() + data.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(data);
    out
}

/// Decode a 2D `uint8` EDF byte stream into `(height, width, data)` in C
/// (row-major) order.
///
/// Accepts the `UnsignedByte` 2D layout that [`encode_mask_edf`] / fabio
/// produce: the ASCII `{ … }` header supplies `Dim_1` (width), `Dim_2` (height)
/// and `DataType`, and the pixel data is the `Dim_1 * Dim_2` bytes that begin
/// immediately after the header's closing `}` (skipping the single `\n` the
/// encoder writes there). Any other `DataType`, a missing/non-integer
/// dimension, an unterminated header, or a body shorter than `Dim_1 * Dim_2` is
/// an [`std::io::ErrorKind::InvalidData`] error. Pure over a byte stream so the
/// round-trip is directly unit-testable.
pub fn decode_mask_edf(bytes: &[u8]) -> std::io::Result<(u32, u32, Vec<u8>)> {
    let invalid = |msg: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, msg.to_string());
    let open = bytes
        .iter()
        .position(|&b| b == b'{')
        .ok_or_else(|| invalid("not an EDF file (no '{')"))?;
    let close = bytes[open..]
        .iter()
        .position(|&b| b == b'}')
        .ok_or_else(|| invalid("EDF header not terminated ('}' missing)"))?
        + open;
    let header = std::str::from_utf8(&bytes[open + 1..close])
        .map_err(|_| invalid("EDF header is not UTF-8"))?;

    let data_type = edf_header_field(header, "DataType")
        .ok_or_else(|| invalid("EDF header missing 'DataType'"))?;
    if data_type != "UnsignedByte" {
        return Err(invalid("EDF mask must be UnsignedByte"));
    }
    let width: u32 = edf_header_field(header, "Dim_1")
        .ok_or_else(|| invalid("EDF header missing 'Dim_1'"))?
        .parse()
        .map_err(|_| invalid("EDF 'Dim_1' is not an integer"))?;
    let height: u32 = edf_header_field(header, "Dim_2")
        .ok_or_else(|| invalid("EDF header missing 'Dim_2'"))?
        .parse()
        .map_err(|_| invalid("EDF 'Dim_2' is not an integer"))?;

    // The body begins right after the closing brace; our encoder (and fabio)
    // writes a single '\n' there before the block-aligned data.
    let mut data_start = close + 1;
    if bytes.get(data_start) == Some(&b'\n') {
        data_start += 1;
    }
    let count = (height as usize) * (width as usize);
    if bytes.len().saturating_sub(data_start) < count {
        return Err(invalid("EDF body shorter than Dim_1 * Dim_2"));
    }
    let data = bytes[data_start..data_start + count].to_vec();
    Ok((height, width, data))
}

/// Extract the value of `key` from an EDF ASCII header (the inside of the
/// `{ … }` block): fields are `KEY = VALUE ;`-separated. Returns `None` if the
/// key is absent.
fn edf_header_field(header: &str, key: &str) -> Option<String> {
    header.split(';').find_map(|entry| {
        let (k, v) = entry.split_once('=')?;
        (k.trim() == key).then(|| v.trim().to_string())
    })
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

/// Lowercase hex digits for the PostScript/PDF ASCII-hex image bodies.
const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

/// Append `data` as an ASCII-hex stream to `s`, wrapping lines at 40 bytes
/// (80 hex chars) so neither PostScript nor PDF readers see an over-long line.
fn push_ascii_hex(s: &mut String, data: &[u8]) {
    for (i, &b) in data.iter().enumerate() {
        s.push(HEX_DIGITS[(b >> 4) as usize] as char);
        s.push(HEX_DIGITS[(b & 0x0F) as usize] as char);
        if (i + 1) % 40 == 0 {
            s.push('\n');
        }
    }
}

/// Encode tightly packed `width * height` RGBA8 pixels as an Encapsulated
/// PostScript (EPS) byte stream embedding the raster (alpha dropped).
///
/// Like [`encode_svg`], this is the faithful raster-embedding substitute for
/// silx's matplotlib-only vector EPS (siplot has no matplotlib dependency): a
/// minimal EPSF-3.0 document whose body is a single `colorimage` operator. The
/// `[W 0 0 -H 0 H]` image matrix flips Y so the first scanline (top of the
/// image) maps to the top of the page, and the RGB samples are fed as
/// whitespace-tolerant ASCII hex. Pure over the RGBA pixels (no GPU /
/// filesystem) so it is directly testable.
pub fn encode_eps(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let rgb = rgba_to_rgb(rgba, width, height);
    let mut s = String::with_capacity(200 + rgb.len() * 2);
    s.push_str("%!PS-Adobe-3.0 EPSF-3.0\n");
    s.push_str(&format!("%%BoundingBox: 0 0 {width} {height}\n"));
    s.push_str("%%EndComments\n");
    s.push_str("gsave\n");
    // Map the unit image square onto the bounding box, then draw the raster.
    s.push_str(&format!("{width} {height} scale\n"));
    s.push_str(&format!("/picstr {} string def\n", width as usize * 3));
    s.push_str(&format!(
        "{width} {height} 8 [ {width} 0 0 -{height} 0 {height} ]\n"
    ));
    s.push_str("{ currentfile picstr readhexstring pop } false 3 colorimage\n");
    push_ascii_hex(&mut s, &rgb);
    s.push('\n');
    s.push_str("grestore\nshowpage\n");
    s.push_str("%%EOF\n");
    s.into_bytes()
}

/// Encode tightly packed `width * height` RGBA8 pixels as a single-page PDF
/// embedding the raster (alpha dropped).
///
/// Like [`encode_eps`], this is the faithful raster-embedding substitute for
/// silx's matplotlib-only vector PDF (siplot has no matplotlib dependency): a
/// minimal PDF-1.4 file with a catalog, one page sized to the image, and a
/// `/DeviceRGB` `/ASCIIHexDecode` image XObject drawn by a content stream whose
/// `cm` matrix scales the unit image to the page. PDF image space puts the
/// first sample row at the top, so feeding top-to-bottom RGB renders upright
/// (no Y-flip needed). The cross-reference table records each object's byte
/// offset. Pure over the RGBA pixels (no GPU / filesystem) so it is directly
/// testable.
pub fn encode_pdf(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let rgb = rgba_to_rgb(rgba, width, height);
    // Image stream: ASCII-hex samples terminated by the '>' EOD marker.
    let mut hex = String::with_capacity(rgb.len() * 2 + 1);
    push_ascii_hex(&mut hex, &rgb);
    hex.push('>');
    let img_len = hex.len();

    // Content stream: scale the unit image to the page and paint it.
    let content = format!("q {width} 0 0 {height} 0 0 cm /Im0 Do Q");
    let content_len = content.len();

    let mut out: Vec<u8> = Vec::with_capacity(img_len + 600);
    // Byte offset of each object (index = object number; slot 0 is the free head).
    let mut offsets = [0usize; 6];

    out.extend_from_slice(b"%PDF-1.4\n");

    offsets[1] = out.len();
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    offsets[2] = out.len();
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    offsets[3] = out.len();
    out.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] \
             /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>\nendobj\n"
        )
        .as_bytes(),
    );

    offsets[4] = out.len();
    out.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /XObject /Subtype /Image /Width {width} /Height {height} \
             /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /ASCIIHexDecode \
             /Length {img_len} >>\nstream\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(hex.as_bytes());
    out.extend_from_slice(b"\nendstream\nendobj\n");

    offsets[5] = out.len();
    out.extend_from_slice(format!("5 0 obj\n<< /Length {content_len} >>\nstream\n").as_bytes());
    out.extend_from_slice(content.as_bytes());
    out.extend_from_slice(b"\nendstream\nendobj\n");

    // Cross-reference table: 20 bytes per entry (10-digit offset, 5-digit gen).
    let xref_offset = out.len();
    out.extend_from_slice(b"xref\n0 6\n");
    out.extend_from_slice(b"0000000000 65535 f \n");
    for &off in &offsets[1..6] {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
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

/// An output image format for [`save_graph_with_format`].
///
/// Faithful to silx `PlotWidget.saveGraph` / `PlotImageFile.saveImageToFile`,
/// which support PNG, PPM, SVG, and TIFF over a raster snapshot, plus EPS and
/// PDF as raster-embedding substitutes for silx's matplotlib-only vector EPS/
/// PDF and JPEG via the hand-written baseline encoder
/// ([`crate::render::jpeg`]). True vector export is not implemented (see the
/// module-level Defer note).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SaveFormat {
    /// PNG (RGBA), via [`encode_png`].
    Png,
    /// Binary (P6) PPM (RGB), via [`encode_ppm`].
    Ppm,
    /// SVG wrapping a base64 PNG `<image>` (RGB), via [`encode_svg`].
    Svg,
    /// Uncompressed baseline TIFF (RGB) with resolution tags, via
    /// [`encode_tiff`].
    Tiff,
    /// Baseline JFIF JPEG (RGB, alpha dropped), via
    /// [`crate::render::jpeg::encode_jpeg`].
    Jpeg,
    /// Encapsulated PostScript embedding the raster (RGB), via [`encode_eps`].
    Eps,
    /// Single-page PDF embedding the raster (RGB), via [`encode_pdf`].
    Pdf,
}

impl SaveFormat {
    /// Map a file extension (case-insensitive, no leading dot) to a format.
    ///
    /// Recognizes silx's raster extensions `png`, `ppm`, `svg`, `tif`/`tiff`,
    /// `jpg`/`jpeg` plus `eps`/`pdf` (raster-embedding). Returns `None` for
    /// still-unsupported extensions (`ps`).
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "png" => Some(SaveFormat::Png),
            "ppm" => Some(SaveFormat::Ppm),
            "svg" => Some(SaveFormat::Svg),
            "tif" | "tiff" => Some(SaveFormat::Tiff),
            "jpg" | "jpeg" => Some(SaveFormat::Jpeg),
            "eps" => Some(SaveFormat::Eps),
            "pdf" => Some(SaveFormat::Pdf),
            _ => None,
        }
    }

    /// Infer the format from a path's extension via [`Self::from_extension`].
    pub fn from_path(path: &Path) -> Option<Self> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(Self::from_extension)
    }
}

/// Render the plot's current view to a `size = (width, height)` pixel image and
/// return the readback as tightly packed RGBA8. Captures the data layer (clear,
/// image, curves); chrome is not included. Requires [`crate::install`] to have
/// run on `render_state`.
fn render_plot_rgba(
    render_state: &RenderState,
    plot: &Plot,
    size: (u32, u32),
) -> Result<Vec<u8>, SaveError> {
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

    let renderer = render_state.renderer.read();
    let res: &WgpuResources = renderer
        .callback_resources
        .get()
        .expect("WgpuResources not installed — call siplot::install() first");
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
    )
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
    let rgba = render_plot_rgba(render_state, plot, size)?;
    let png = encode_png(&rgba, w, h)?;
    std::fs::write(path, png)?;
    Ok(())
}

/// Render the plot's current view and save it to `path` in the given
/// [`SaveFormat`], at the requested `dpi` (used where the format carries
/// resolution — currently TIFF). Captures the data layer only; chrome is not
/// included.
///
/// Faithful to silx `BackendBase.saveGraph(fileName, fileFormat, dpi)`: the
/// caller chooses the format explicitly and threads DPI through. For raster
/// formats (PNG/PPM/SVG) `dpi` is recorded only where the container supports it
/// (SVG width/height stay in px); TIFF writes `XResolution`/`YResolution`.
pub fn save_graph_with_format(
    render_state: &RenderState,
    plot: &Plot,
    size: (u32, u32),
    path: impl AsRef<Path>,
    format: SaveFormat,
    dpi: u32,
) -> Result<(), SaveError> {
    let (w, h) = size;
    let rgba = render_plot_rgba(render_state, plot, size)?;
    match format {
        SaveFormat::Png => {
            let bytes = encode_png(&rgba, w, h)?;
            std::fs::write(path, bytes)?;
        }
        SaveFormat::Ppm => {
            let bytes = encode_ppm(&rgba, w, h);
            std::fs::write(path, bytes)?;
        }
        SaveFormat::Svg => {
            let svg = encode_svg(&rgba, w, h)?;
            std::fs::write(path, svg)?;
        }
        SaveFormat::Tiff => {
            let bytes = encode_tiff(&rgba, w, h, dpi);
            std::fs::write(path, bytes)?;
        }
        SaveFormat::Jpeg => {
            let bytes = crate::render::jpeg::encode_jpeg(&rgba, w, h, dpi);
            std::fs::write(path, bytes)?;
        }
        SaveFormat::Eps => {
            let bytes = encode_eps(&rgba, w, h);
            std::fs::write(path, bytes)?;
        }
        SaveFormat::Pdf => {
            let bytes = encode_pdf(&rgba, w, h);
            std::fs::write(path, bytes)?;
        }
    }
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
    fn save_format_from_extension_maps_silx_raster_formats() {
        assert_eq!(SaveFormat::from_extension("png"), Some(SaveFormat::Png));
        assert_eq!(SaveFormat::from_extension("PNG"), Some(SaveFormat::Png));
        assert_eq!(SaveFormat::from_extension("ppm"), Some(SaveFormat::Ppm));
        assert_eq!(SaveFormat::from_extension("svg"), Some(SaveFormat::Svg));
        assert_eq!(SaveFormat::from_extension("tif"), Some(SaveFormat::Tiff));
        assert_eq!(SaveFormat::from_extension("TIFF"), Some(SaveFormat::Tiff));
        assert_eq!(SaveFormat::from_extension("eps"), Some(SaveFormat::Eps));
        assert_eq!(SaveFormat::from_extension("EPS"), Some(SaveFormat::Eps));
        assert_eq!(SaveFormat::from_extension("pdf"), Some(SaveFormat::Pdf));
        assert_eq!(SaveFormat::from_extension("PDF"), Some(SaveFormat::Pdf));
        assert_eq!(SaveFormat::from_extension("jpg"), Some(SaveFormat::Jpeg));
        assert_eq!(SaveFormat::from_extension("JPEG"), Some(SaveFormat::Jpeg));
    }

    #[test]
    fn save_format_rejects_still_unsupported_and_unknown_extensions() {
        // PostScript (`ps`) is not wired → still unsupported. (EPS, PDF, and
        // JPEG are now covered by the hand-written encoders.)
        assert_eq!(SaveFormat::from_extension("ps"), None);
        assert_eq!(SaveFormat::from_extension("bmp"), None);
        assert_eq!(SaveFormat::from_extension(""), None);
    }

    #[test]
    fn save_format_from_path_uses_extension() {
        use std::path::Path;
        assert_eq!(
            SaveFormat::from_path(Path::new("/tmp/out.tiff")),
            Some(SaveFormat::Tiff)
        );
        assert_eq!(SaveFormat::from_path(Path::new("/tmp/noext")), None);
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
    fn encode_eps_is_well_formed_and_hex_body_round_trips() {
        // 2×2 image with distinct pixels; the alpha channel is dropped.
        let rgba = [
            10, 20, 30, 255, // (0,0)
            40, 50, 60, 255, // (0,1)
            70, 80, 90, 255, // (1,0)
            100, 110, 120, 255, // (1,1)
        ];
        let eps = encode_eps(&rgba, 2, 2);
        let text = std::str::from_utf8(&eps).expect("EPS is ASCII");

        assert!(text.starts_with("%!PS-Adobe-3.0 EPSF-3.0\n"));
        assert!(text.contains("%%BoundingBox: 0 0 2 2\n"));
        // The Y-flip matrix and the colorimage operator drive the raster.
        assert!(text.contains("2 2 8 [ 2 0 0 -2 0 2 ]"));
        assert!(text.contains("false 3 colorimage"));
        assert!(text.trim_end().ends_with("%%EOF"));

        // The ASCII-hex body decodes back to the RGB (alpha-dropped) pixels.
        // Bound it to before the `grestore` trailer (whose letters are hex too).
        let body = text
            .split("colorimage\n")
            .nth(1)
            .expect("body after colorimage")
            .split("\ngrestore")
            .next()
            .expect("hex before grestore");
        let hex: String = body.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        let decoded: Vec<u8> = hex
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        assert_eq!(decoded, rgba_to_rgb(&rgba, 2, 2));
    }

    #[test]
    fn encode_pdf_is_well_formed_and_xref_offsets_point_at_objects() {
        let rgba = [
            10, 20, 30, 255, // (0,0)
            40, 50, 60, 255, // (0,1)
            70, 80, 90, 255, // (1,0)
            100, 110, 120, 255, // (1,1)
        ];
        let pdf = encode_pdf(&rgba, 2, 2);
        let text = std::str::from_utf8(&pdf).expect("PDF body is ASCII here");

        assert!(text.starts_with("%PDF-1.4\n"));
        assert!(text.contains("/Type /Catalog"));
        assert!(text.contains("/MediaBox [0 0 2 2]"));
        assert!(text.contains("/Subtype /Image /Width 2 /Height 2"));
        assert!(text.contains("/ColorSpace /DeviceRGB"));
        assert!(text.contains("/Filter /ASCIIHexDecode"));
        assert!(text.trim_end().ends_with("%%EOF"));

        // startxref points at the `xref` keyword.
        let sx = text.rfind("startxref\n").expect("startxref");
        let after = &text[sx + "startxref\n".len()..];
        let xref_off: usize = after
            .lines()
            .next()
            .unwrap()
            .trim()
            .parse()
            .expect("xref offset int");
        assert!(
            text[xref_off..].starts_with("xref\n"),
            "startxref must point at xref"
        );

        // The image hex stream decodes back to the RGB (alpha-dropped) pixels.
        let stream_at = text.find("/ASCIIHexDecode").expect("image dict");
        let body = &text[stream_at..];
        let body = &body[body.find("stream\n").expect("stream kw") + "stream\n".len()..];
        let hex = &body[..body.find('>').expect("hex EOD marker")];
        let hex: String = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        let decoded: Vec<u8> = hex
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        assert_eq!(decoded, rgba_to_rgb(&rgba, 2, 2));
    }

    // --- NumPy .npy mask codec ---

    #[test]
    fn mask_npy_round_trips_bytes_and_shape() {
        // A small 2x3 uint8 mask round-trips through encode -> decode with
        // identical shape and data.
        let data: Vec<u8> = vec![0, 1, 2, 250, 254, 255];
        let bytes = encode_mask_npy(2, 3, &data);
        let (h, w, out) = decode_mask_npy(&bytes).expect("decode");
        assert_eq!((h, w), (2, 3));
        assert_eq!(out, data);
    }

    #[test]
    fn mask_npy_header_is_valid_v1_format() {
        let data = vec![7u8; 4];
        let bytes = encode_mask_npy(2, 2, &data);
        // Magic \x93NUMPY, version 1.0.
        assert_eq!(&bytes[0..6], b"\x93NUMPY");
        assert_eq!(&bytes[6..8], &[1, 0]);
        // header_len (u16 LE) and the preamble length is a multiple of 64.
        let header_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        let preamble = 10 + header_len;
        assert_eq!(preamble % 64, 0, "preamble {preamble} not 64-aligned");
        // The header dict carries descr/fortran_order/shape and ends in newline.
        let header = std::str::from_utf8(&bytes[10..preamble]).expect("ascii header");
        assert!(header.contains("'descr': '|u1'"));
        assert!(header.contains("'fortran_order': False"));
        assert!(header.contains("'shape': (2, 2)"));
        assert!(header.ends_with('\n'));
        // The raw C-order body follows the preamble exactly.
        assert_eq!(&bytes[preamble..], data.as_slice());
    }

    #[test]
    fn mask_npy_rejects_bad_magic_and_non_uint8() {
        // Bad magic.
        let err = decode_mask_npy(b"not-a-npy-file-at-all").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);

        // Valid framing but a float64 dtype is rejected.
        let mut bytes = encode_mask_npy(1, 1, &[0]);
        let header_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        let header = std::str::from_utf8(&bytes[10..10 + header_len])
            .unwrap()
            .replace("|u1", "<f8");
        bytes.splice(10..10 + header_len, header.bytes());
        let err = decode_mask_npy(&bytes).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn mask_npy_rejects_non_2d_shape() {
        // Reshape the header to a 3D shape; decode must reject it.
        let mut bytes = encode_mask_npy(1, 1, &[0]);
        let header_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        let header = std::str::from_utf8(&bytes[10..10 + header_len])
            .unwrap()
            .replace("(1, 1)", "(1, 1, 1)");
        // Keep total length stable by trimming/padding spaces before newline.
        let mut header = header;
        while header.len() < header_len {
            header.insert(header.len() - 1, ' ');
        }
        let header = &header[..header_len];
        bytes.splice(10..10 + header_len, header.bytes());
        let err = decode_mask_npy(&bytes).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn mask_edf_round_trips_bytes_and_shape() {
        // A small 2x3 uint8 mask round-trips through encode -> decode with
        // identical shape and data. Note byte values 10 (b'\n') and 32 (b' ')
        // appear in the body to prove the trailing-bytes split is not confused
        // by header whitespace.
        let data: Vec<u8> = vec![0, 10, 32, 250, 254, 255];
        let bytes = encode_mask_edf(2, 3, &data);
        let (h, w, out) = decode_mask_edf(&bytes).expect("decode");
        assert_eq!((h, w), (2, 3));
        assert_eq!(out, data);
    }

    #[test]
    fn mask_edf_header_is_512_aligned_and_self_describing() {
        let data = vec![7u8; 6];
        let bytes = encode_mask_edf(2, 3, &data);
        // The header block (everything before the raw body) is 512-aligned.
        let body_start = bytes.len() - data.len();
        assert_eq!(
            body_start % 512,
            0,
            "header block {body_start} not 512-aligned"
        );
        let header = std::str::from_utf8(&bytes[..body_start]).expect("ascii header");
        assert!(header.starts_with('{'));
        assert!(header.contains("DataType = UnsignedByte ;"));
        assert!(header.contains("Dim_1 = 3 ;")); // width = fast axis
        assert!(header.contains("Dim_2 = 2 ;")); // height = slow axis
        assert!(header.contains("Size = 6 ;"));
        assert!(header.trim_end().ends_with('}'));
        // The raw C-order body follows the aligned header exactly.
        assert_eq!(&bytes[body_start..], data.as_slice());
    }

    #[test]
    fn mask_edf_rejects_non_byte_type_and_truncated_body() {
        // A float dtype is rejected.
        let bytes = encode_mask_edf(1, 1, &[0]);
        let header_end = bytes.len() - 1;
        let header = std::str::from_utf8(&bytes[..header_end])
            .unwrap()
            .replace("UnsignedByte", "FloatValue ");
        let mut tampered = header.into_bytes();
        tampered.push(0);
        let err = decode_mask_edf(&tampered).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);

        // A body shorter than Dim_1 * Dim_2 is rejected.
        let mut short = encode_mask_edf(4, 4, &[1u8; 16]);
        short.truncate(short.len() - 8);
        let err = decode_mask_edf(&short).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
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
