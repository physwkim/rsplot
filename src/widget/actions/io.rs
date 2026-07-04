//! Plot I/O actions, mirroring silx `silx.gui.plot.actions.io`.
//!
//! The figure-save (PNG) and data-save (CSV) behaviors here mirror silx
//! `SaveAction` (`actions/io.py`). The load-bearing logic — mapping a chosen
//! file extension to a [`SaveTarget`] and serializing a curve's `(x, y)` to CSV
//! — is pure and unit-tested; the native `rfd` file dialog and the GPU figure
//! readback are thin untestable shims around it.

use std::borrow::Cow;
use std::path::Path;

use crate::core::items::ErrorBars;
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
    /// target. `csv` saves curve data; the extensions recognized by
    /// [`SaveFormat::from_extension`] (`png`, `ppm`, `svg`, `tif`/`tiff`,
    /// `eps`, `pdf`) save the figure. Returns `None` for unknown extensions.
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

/// One CSV data column: a slice of per-point values, or a scalar broadcast to
/// every row (silx `_get1dData` turns a scalar error into
/// `numpy.zeros_like(y_data) + err`).
enum CsvColumn<'a> {
    Values(&'a [f64]),
    Broadcast(f64),
}

/// Append the CSV column(s) and label(s) for one error-bar set, exactly as
/// silx `SaveAction._get1dData` (`actions/io.py:264-289`): a scalar error
/// broadcasts to a full `<label>_errors` column, a 1-D error is a
/// `<label>_errors` column as-is, and a `(2, N)` asymmetric error splits into
/// `<label>_errors_below` (row 0) then `<label>_errors_above` (row 1).
fn push_error_columns<'a>(
    error: &'a ErrorBars,
    label: &str,
    labels: &mut Vec<String>,
    columns: &mut Vec<CsvColumn<'a>>,
) {
    match error {
        ErrorBars::Symmetric(e) => {
            labels.push(format!("{label}_errors"));
            columns.push(CsvColumn::Broadcast(*e));
        }
        ErrorBars::PerPoint(es) => {
            labels.push(format!("{label}_errors"));
            columns.push(CsvColumn::Values(es));
        }
        ErrorBars::Asymmetric { lower, upper } => {
            labels.push(format!("{label}_errors_below"));
            columns.push(CsvColumn::Values(lower));
            labels.push(format!("{label}_errors_above"));
            columns.push(CsvColumn::Values(upper));
        }
    }
}

/// Serialize a curve to silx-style `,`-separated CSV (silx
/// `SaveAction._saveCurve` → `_get1dData` → `save1D` with the default
/// `","`-CSV filter: `header=True`, `delimiter=","`, `fmt="%.18e"`).
///
/// The header is the real labels — `xlabel + "," + ",".join(ylabels)`
/// (`silx/io/utils.py:279`), where the y-label list starts with `ylabel` and
/// grows one entry per error column: the x-error column(s) first, then the
/// y-error column(s), scalar errors broadcast and `(2, N)` asymmetric errors
/// split into `_errors_below`/`_errors_above` (silx `_get1dData`,
/// `actions/io.py:254-289`). silx writes whatever the labels resolve to —
/// empty labels stay empty in the header.
///
/// All per-point columns must have equal length; on a mismatch the shortest is
/// followed (every row needs all its columns), matching a zipped write. Pure,
/// so the exact byte output is unit-testable without touching the filesystem.
pub fn curve_to_csv(
    x: &[f64],
    y: &[f64],
    xlabel: &str,
    ylabel: &str,
    x_error: Option<&ErrorBars>,
    y_error: Option<&ErrorBars>,
) -> String {
    // Column order is silx `_get1dData`: y, then x-errors, then y-errors.
    let mut labels: Vec<String> = vec![ylabel.to_string()];
    let mut columns: Vec<CsvColumn<'_>> = vec![CsvColumn::Values(y)];
    if let Some(error) = x_error {
        push_error_columns(error, xlabel, &mut labels, &mut columns);
    }
    if let Some(error) = y_error {
        push_error_columns(error, ylabel, &mut labels, &mut columns);
    }

    let mut out = String::new();
    out.push_str(xlabel);
    for label in &labels {
        out.push(',');
        out.push_str(label);
    }
    out.push('\n');

    let rows = columns
        .iter()
        .filter_map(|c| match c {
            CsvColumn::Values(v) => Some(v.len()),
            CsvColumn::Broadcast(_) => None,
        })
        .fold(x.len(), usize::min);
    for i in 0..rows {
        out.push_str(&format_csv_float(x[i]));
        for column in &columns {
            let v = match column {
                CsvColumn::Values(vals) => vals[i],
                CsvColumn::Broadcast(e) => *e,
            };
            out.push(',');
            out.push_str(&format_csv_float(v));
        }
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
        // The raster-embedding vector formats now resolve through SaveFormat.
        assert_eq!(
            SaveTarget::from_extension("eps"),
            Some(SaveTarget::Figure(SaveFormat::Eps))
        );
        assert_eq!(
            SaveTarget::from_extension("pdf"),
            Some(SaveTarget::Figure(SaveFormat::Pdf))
        );
        assert_eq!(
            SaveTarget::from_extension("jpeg"),
            Some(SaveTarget::Figure(SaveFormat::Jpeg))
        );
        // Still-unsupported / unknown extensions are rejected.
        assert_eq!(SaveTarget::from_extension("ps"), None);
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
        let csv = curve_to_csv(&x, &y, "x", "y", None, None);
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
    fn curve_to_csv_header_uses_the_real_labels() {
        // silx save1D: header = xlabel + "," + ",".join(ylabels)
        // (io/utils.py:279) — the resolved axis labels, not literal "x,y".
        let csv = curve_to_csv(&[1.0], &[2.0], "Energy [keV]", "Counts", None, None);
        assert!(
            csv.starts_with("Energy [keV],Counts\n"),
            "header must carry the real labels, got {csv:?}"
        );
        // Empty labels stay empty (a bare silx PlotWidget has "" axis labels).
        let csv = curve_to_csv(&[1.0], &[2.0], "", "", None, None);
        assert!(csv.starts_with(",\n"));
    }

    #[test]
    fn curve_to_csv_error_columns_follow_silx_get1ddata() {
        // silx _get1dData (actions/io.py:254-289): columns y, then x-errors,
        // then y-errors; a scalar broadcasts to a full column; a (2,N) error
        // splits into _errors_below (row 0) then _errors_above (row 1).
        let x = [1.0, 2.0];
        let y = [10.0, 20.0];
        let x_err = ErrorBars::Symmetric(0.5);
        let y_err = ErrorBars::Asymmetric {
            lower: vec![3.0, 4.0],
            upper: vec![5.0, 6.0],
        };
        let csv = curve_to_csv(&x, &y, "x", "y", Some(&x_err), Some(&y_err));
        let mut lines = csv.lines();
        assert_eq!(
            lines.next(),
            Some("x,y,x_errors,y_errors_below,y_errors_above")
        );
        assert_eq!(
            lines.next(),
            Some(
                "1.000000000000000000e+00,1.000000000000000000e+01,\
                 5.000000000000000000e-01,3.000000000000000000e+00,\
                 5.000000000000000000e+00"
            )
        );
        assert_eq!(
            lines.next(),
            Some(
                "2.000000000000000000e+00,2.000000000000000000e+01,\
                 5.000000000000000000e-01,4.000000000000000000e+00,\
                 6.000000000000000000e+00"
            )
        );
        assert_eq!(lines.next(), None);

        // A 1-D per-point error is a single `<label>_errors` column.
        // (`'%.18e' % 0.1` == 1.000000000000000056e-01 — the f64 tail shows.)
        let y_err = ErrorBars::PerPoint(vec![0.1, 0.2]);
        let csv = curve_to_csv(&x, &y, "x", "y", None, Some(&y_err));
        assert!(csv.starts_with("x,y,y_errors\n"));
        assert!(csv.contains(",1.000000000000000056e-01\n"));
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
        assert_eq!(curve_to_csv(&[], &[], "x", "y", None, None), "x,y\n");
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
