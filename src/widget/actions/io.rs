//! Plot I/O actions, mirroring silx `silx.gui.plot.actions.io`.
//!
//! The figure-save (PNG) and data-save (CSV) behaviors here mirror silx
//! `SaveAction` (`actions/io.py`). The load-bearing logic — mapping a chosen
//! file extension to a [`SaveTarget`] and serializing a curve's `(x, y)` to CSV
//! — is pure and unit-tested; the native `rfd` file dialog and the GPU figure
//! readback are thin untestable shims around it.

use std::borrow::Cow;
use std::path::Path;

use crate::render::save::SaveFormat;

/// Digits after the decimal point in the CSV float format: 18, matching silx
/// `SaveAction`'s `","`-CSV filter `fmt="%.18e"` (itself `numpy.savetxt`'s
/// default). See [`format_csv_float`].
const CSV_FLOAT_PRECISION: usize = 18;

/// What a chosen save path resolves to, mirroring silx `SaveAction` splitting
/// its name-filters into figure snapshots and curve-data exports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveTarget {
    /// Save the figure as a raster image in the given [`SaveFormat`] (silx
    /// `SNAPSHOT_FILTER_*`). The GPU figure readback is the untestable shim.
    Figure(SaveFormat),
    /// Save the active curve's `(x, y)` data as CSV (silx `","`-separated
    /// `DEFAULT_CURVE_FILTERS` CSV).
    CurveCsv,
}

impl SaveTarget {
    /// Resolve a file extension (case-insensitive, no leading dot) to a save
    /// target. `csv` saves curve data; the raster extensions recognized by
    /// [`SaveFormat::from_extension`] (`png`, `ppm`, `svg`, `tif`/`tiff`) save
    /// the figure. Returns `None` for unknown extensions.
    pub fn from_extension(ext: &str) -> Option<Self> {
        if ext.eq_ignore_ascii_case("csv") {
            return Some(SaveTarget::CurveCsv);
        }
        SaveFormat::from_extension(ext).map(SaveTarget::Figure)
    }

    /// Resolve a path's extension to a save target via [`Self::from_extension`].
    pub fn from_path(path: &Path) -> Option<Self> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(Self::from_extension)
    }
}

/// Format a single `f64` byte-for-byte as C/Python `%.18e` (what
/// `numpy.savetxt`, and therefore silx `SaveAction`, writes):
/// [`CSV_FLOAT_PRECISION`] digits after the decimal point and a signed,
/// at-least-two-digit exponent (e.g. `1.500000000000000000e+00`).
///
/// Rust's `{:.18e}` produces the right mantissa but a sign-less, zero-pad-less
/// exponent (`...e0`, `...e-3`), so the exponent is reformatted to match.
fn format_csv_float(v: f64) -> String {
    let s = format!("{v:.*e}", CSV_FLOAT_PRECISION);
    match s.split_once('e') {
        Some((mantissa, exp)) => {
            let exp: i32 = exp.parse().unwrap_or(0);
            let sign = if exp < 0 { '-' } else { '+' };
            format!("{mantissa}e{sign}{:02}", exp.unsigned_abs())
        }
        // `{:e}` always yields an exponent; this is just a defensive fallback.
        None => s,
    }
}

/// Serialize a curve's `(x, y)` to silx-style `,`-separated CSV: a header line
/// `x,y` followed by one `xval,yval` row per point, `%.18e`-formatted, `\n`
/// line endings (silx `SaveAction._saveCurve` → `save1D` with the default
/// `","`-CSV filter: `header=True`, `delimiter=","`, `fmt="%.18e"`).
///
/// `x` and `y` must have equal length; on a length mismatch the shorter is
/// followed (each row needs both columns), matching a zipped write. Pure, so the
/// exact byte output is unit-testable without touching the filesystem.
pub fn curve_to_csv(x: &[f64], y: &[f64]) -> String {
    let mut out = String::from("x,y\n");
    for (xv, yv) in x.iter().zip(y.iter()) {
        out.push_str(&format_csv_float(*xv));
        out.push(',');
        out.push_str(&format_csv_float(*yv));
        out.push('\n');
    }
    out
}

/// Decode an 8-bit RGBA PNG into a tightly packed, row-major `width * height`
/// RGBA8 buffer, returning `(width, height, rgba)`. Used by the clipboard-copy
/// shim to turn the figure PNG (the only in-memory figure encoding available
/// here) back into the RGBA the clipboard expects; the figure encoder
/// ([`encode_png`](crate::render::save::encode_png)) always writes 8-bit RGBA,
/// so no channel expansion is needed. Returns an error for a non-RGBA8 PNG. Pure
/// (no GPU/clipboard), so the decode is testable via an `encode_png` round-trip.
pub fn decode_png_to_rgba(png_bytes: &[u8]) -> std::io::Result<(u32, u32, Vec<u8>)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let buf_size = reader.output_buffer_size().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "PNG output size overflow")
    })?;
    let mut buf = vec![0u8; buf_size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "expected 8-bit RGBA PNG",
        ));
    }
    buf.truncate(info.buffer_size());
    Ok((info.width, info.height, buf))
}

/// Shape a tightly packed, row-major `width * height` RGBA8 buffer into an
/// owned [`arboard::ImageData`] for the clipboard (silx `CopyAction` puts a
/// figure bitmap on the clipboard via `QApplication.clipboard().setImage`).
///
/// arboard expects `width * height * 4` bytes, top-to-bottom rows, RGBA channel
/// order — the same layout the GPU figure readback produces — so the bytes are
/// taken verbatim. Returns `None` when `rgba.len()` does not equal
/// `width * height * 4` (the only shaping invariant), so a malformed buffer is
/// rejected before the clipboard shim. Pure and unit-testable without touching
/// the clipboard.
pub fn rgba_to_clipboard_image(
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Option<arboard::ImageData<'static>> {
    let expected = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if rgba.len() != expected {
        return None;
    }
    Some(arboard::ImageData {
        width: width as usize,
        height: height as usize,
        bytes: Cow::Owned(rgba.to_vec()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_target_from_extension_maps_csv_and_raster() {
        assert_eq!(
            SaveTarget::from_extension("csv"),
            Some(SaveTarget::CurveCsv)
        );
        assert_eq!(
            SaveTarget::from_extension("CSV"),
            Some(SaveTarget::CurveCsv)
        );
        assert_eq!(
            SaveTarget::from_extension("png"),
            Some(SaveTarget::Figure(SaveFormat::Png))
        );
        assert_eq!(
            SaveTarget::from_extension("PNG"),
            Some(SaveTarget::Figure(SaveFormat::Png))
        );
        assert_eq!(
            SaveTarget::from_extension("svg"),
            Some(SaveTarget::Figure(SaveFormat::Svg))
        );
        // Unknown / matplotlib-only extensions are rejected.
        assert_eq!(SaveTarget::from_extension("pdf"), None);
        assert_eq!(SaveTarget::from_extension("xyz"), None);
    }

    #[test]
    fn save_target_from_path_uses_extension() {
        assert_eq!(
            SaveTarget::from_path(Path::new("/tmp/curve.csv")),
            Some(SaveTarget::CurveCsv)
        );
        assert_eq!(
            SaveTarget::from_path(Path::new("/tmp/plot.png")),
            Some(SaveTarget::Figure(SaveFormat::Png))
        );
        assert_eq!(SaveTarget::from_path(Path::new("/tmp/noext")), None);
    }

    #[test]
    fn curve_to_csv_produces_exact_silx_style_output() {
        let x = [0.0, 1.5];
        let y = [-2.0, 3.25];
        let csv = curve_to_csv(&x, &y);
        // These rows are byte-for-byte what silx writes: numpy.savetxt with
        // fmt="%.18e", which is C/Python `'%.18e' % v` — signed, two-digit
        // exponent (`e+00`), 18 fractional digits. (Cross-checked against
        // `python3 -c "print('%.18e' % 1.5)"` → 1.500000000000000000e+00.)
        let expected = "x,y\n\
             0.000000000000000000e+00,-2.000000000000000000e+00\n\
             1.500000000000000000e+00,3.250000000000000000e+00\n";
        assert_eq!(csv, expected);
    }

    #[test]
    fn format_csv_float_matches_c_printf_exponent() {
        // Byte-for-byte equal to `python3 -c "print('%.18e' % v)"` (verified),
        // including the f64 representation tail of 0.001 (...021e-03) — proving
        // the format is faithful to numpy.savetxt's `%.18e`, not Rust's `{:e}`.
        assert_eq!(format_csv_float(0.0), "0.000000000000000000e+00");
        assert_eq!(format_csv_float(1000.0), "1.000000000000000000e+03");
        assert_eq!(format_csv_float(0.001), "1.000000000000000021e-03");
        assert_eq!(format_csv_float(-3.25), "-3.250000000000000000e+00");
    }

    #[test]
    fn curve_to_csv_empty_is_header_only() {
        assert_eq!(curve_to_csv(&[], &[]), "x,y\n");
    }

    #[test]
    fn rgba_to_clipboard_image_shapes_a_valid_buffer() {
        // 2x1 image: two RGBA pixels (8 bytes).
        let rgba: Vec<u8> = vec![10, 20, 30, 255, 40, 50, 60, 128];
        let image = rgba_to_clipboard_image(&rgba, 2, 1).expect("valid buffer");
        assert_eq!(image.width, 2);
        assert_eq!(image.height, 1);
        assert_eq!(image.bytes.len(), 8);
        // Bytes are taken verbatim in row order.
        assert_eq!(image.bytes.as_ref(), rgba.as_slice());
    }

    #[test]
    fn rgba_to_clipboard_image_rejects_wrong_length() {
        // 7 bytes for a 2x1 (needs 8) is rejected.
        assert!(rgba_to_clipboard_image(&[0; 7], 2, 1).is_none());
        // 9 bytes is also rejected.
        assert!(rgba_to_clipboard_image(&[0; 9], 2, 1).is_none());
    }

    #[test]
    fn decode_png_to_rgba_round_trips_encode_png() {
        use crate::render::save::encode_png;

        // 2x2 RGBA image.
        let rgba: Vec<u8> = vec![
            1, 2, 3, 255, 4, 5, 6, 255, // row 0
            7, 8, 9, 255, 10, 11, 12, 255, // row 1
        ];
        let png = encode_png(&rgba, 2, 2).expect("encode");
        let (w, h, decoded) = decode_png_to_rgba(&png).expect("decode");
        assert_eq!((w, h), (2, 2));
        assert_eq!(decoded, rgba);
    }
}
