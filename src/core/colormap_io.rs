//! Serialize a [`Colormap`] to and from a flat text format, mirroring silx
//! `Colormap.saveState`/`restoreState` (`silx.gui.colors`, :985-1078) — the
//! round-trip Qt persists into a `QByteArray` via `QDataStream`.
//!
//! siplot has no serde dependency, so this is a hand-written, line-oriented
//! encoder/decoder over a fixed schema — the same manual-serialization approach
//! as [`crate::core::roi_io`] and the `.npy` mask path. The pure
//! [`encode_colormap`]/[`decode_colormap`] functions are headlessly testable;
//! [`save_colormap`]/[`load_colormap`] are the thin filesystem wrappers.
//!
//! Unlike silx — which serializes the colormap *name* and rebuilds the LUT from
//! the (registered) name on restore — siplot's [`Colormap`] stores a resolved
//! 256-entry LUT and keeps no name (`Colormap::set_name` rewrites the LUT in
//! place). The state therefore serializes the LUT itself, so the round-trip is
//! lossless (`decode_colormap(&encode_colormap(c)) == c`) regardless of whether
//! the LUT came from a catalog name, [`Colormap::from_colors`], or a registered
//! name.
//!
//! Format (version 1):
//!
//! ```text
//! siplot-colormap 1
//! vmin <number>
//! vmax <number>
//! normalization <linear | log | sqrt | gamma | arcsinh>
//! gamma <number>
//! nan_color <RRGGBBAA hex>
//! percentiles <low> <high>
//! editable <true | false>
//! lut <2048 hex chars — 256 RGBA entries, 8 hex digits each>
//! ```
//!
//! `lut` is the only required line (a colormap is its LUT); every other field
//! falls back to the [`Colormap::new`] default when absent, mirroring silx
//! `restoreState`'s version-gated defaults (missing normalization → linear,
//! missing autoscale info → defaults). Unknown keys are ignored for forward
//! compatibility.

use crate::core::colormap::{Colormap, Normalization};

/// Magic + version header of a siplot colormap-state file.
const COLORMAP_IO_HEADER: &str = "siplot-colormap 1";

/// An error decoding a siplot colormap-state text blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColormapIoError {
    /// Missing or wrong `siplot-colormap <version>` header line.
    BadHeader,
    /// The required `lut` line was absent.
    MissingField(&'static str),
    /// A field value did not parse; the field name is given.
    BadValue(&'static str),
}

impl std::fmt::Display for ColormapIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ColormapIoError::BadHeader => {
                write!(f, "missing or wrong `{COLORMAP_IO_HEADER}` header")
            }
            ColormapIoError::MissingField(k) => write!(f, "missing required field `{k}`"),
            ColormapIoError::BadValue(k) => write!(f, "invalid value for field `{k}`"),
        }
    }
}

impl std::error::Error for ColormapIoError {}

/// Serialize `colormap` to the siplot colormap-state text format (silx
/// `Colormap.saveState`). All of siplot's colormap state — LUT, value range,
/// normalization, gamma, NaN color, autoscale percentiles, and the editable
/// flag — is persisted, so the round-trip is lossless.
#[must_use]
pub fn encode_colormap(colormap: &Colormap) -> String {
    let mut out = String::new();
    out.push_str(COLORMAP_IO_HEADER);
    out.push('\n');
    push_kv(&mut out, "vmin", &colormap.vmin.to_string());
    push_kv(&mut out, "vmax", &colormap.vmax.to_string());
    push_kv(
        &mut out,
        "normalization",
        normalization_str(colormap.normalization),
    );
    push_kv(&mut out, "gamma", &colormap.gamma.to_string());
    push_kv(&mut out, "nan_color", &rgba_to_hex(colormap.nan_color));
    let (low, high) = colormap.autoscale_percentiles;
    push_kv(&mut out, "percentiles", &format!("{low} {high}"));
    push_kv(
        &mut out,
        "editable",
        if colormap.editable { "true" } else { "false" },
    );
    push_kv(&mut out, "lut", &lut_to_hex(&colormap.lut));
    out
}

/// Parse a siplot colormap-state text blob back into a [`Colormap`] (silx
/// `Colormap.restoreState`). The `lut` line is required; every other field
/// falls back to the [`Colormap::new`] default when absent.
pub fn decode_colormap(text: &str) -> Result<Colormap, ColormapIoError> {
    let mut lines = text.lines();
    match lines.next() {
        Some(h) if h == COLORMAP_IO_HEADER => {}
        _ => return Err(ColormapIoError::BadHeader),
    }

    // A neutral base supplies the defaults for any absent field (silx
    // restoreState defaults: linear norm, default gamma / NaN color / autoscale).
    let mut cm = Colormap::new(crate::core::colormap::ColormapName::Gray, 0.0, 1.0);
    let mut have_lut = false;

    for line in lines {
        if line.is_empty() {
            continue; // tolerate blank separator lines
        }
        let (key, val) = split_kv(line);
        match key {
            "vmin" => cm.vmin = val.parse().map_err(|_| ColormapIoError::BadValue("vmin"))?,
            "vmax" => cm.vmax = val.parse().map_err(|_| ColormapIoError::BadValue("vmax"))?,
            "normalization" => {
                cm.normalization =
                    parse_normalization(val).ok_or(ColormapIoError::BadValue("normalization"))?
            }
            "gamma" => {
                cm.gamma = val
                    .parse()
                    .map_err(|_| ColormapIoError::BadValue("gamma"))?
            }
            "nan_color" => {
                cm.nan_color = parse_rgba(val).ok_or(ColormapIoError::BadValue("nan_color"))?
            }
            "percentiles" => {
                cm.autoscale_percentiles =
                    parse_percentiles(val).ok_or(ColormapIoError::BadValue("percentiles"))?
            }
            "editable" => {
                cm.editable = parse_bool(val).ok_or(ColormapIoError::BadValue("editable"))?
            }
            "lut" => {
                cm.lut = parse_lut(val).ok_or(ColormapIoError::BadValue("lut"))?;
                have_lut = true;
            }
            _ => {} // ignore unknown keys for forward compatibility
        }
    }

    if !have_lut {
        return Err(ColormapIoError::MissingField("lut"));
    }
    Ok(cm)
}

/// Write `colormap` to `path` in the siplot colormap-state text format (silx
/// `Colormap.saveState` persisted to a file).
pub fn save_colormap(
    path: impl AsRef<std::path::Path>,
    colormap: &Colormap,
) -> std::io::Result<()> {
    std::fs::write(path, encode_colormap(colormap))
}

/// Read a colormap from `path` written by [`save_colormap`] (silx
/// `Colormap.restoreState`). A parse error maps to
/// [`std::io::ErrorKind::InvalidData`].
pub fn load_colormap(path: impl AsRef<std::path::Path>) -> std::io::Result<Colormap> {
    let text = std::fs::read_to_string(path)?;
    decode_colormap(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

// --- internals --------------------------------------------------------------

fn push_kv(out: &mut String, key: &str, val: &str) {
    out.push_str(key);
    out.push(' ');
    out.push_str(val);
    out.push('\n');
}

fn split_kv(line: &str) -> (&str, &str) {
    match line.find(' ') {
        Some(i) => (&line[..i], &line[i + 1..]),
        None => (line, ""),
    }
}

fn normalization_str(n: Normalization) -> &'static str {
    match n {
        Normalization::Linear => "linear",
        Normalization::Log => "log",
        Normalization::Sqrt => "sqrt",
        Normalization::Gamma => "gamma",
        Normalization::Arcsinh => "arcsinh",
    }
}

fn parse_normalization(val: &str) -> Option<Normalization> {
    Some(match val {
        "linear" => Normalization::Linear,
        "log" => Normalization::Log,
        "sqrt" => Normalization::Sqrt,
        "gamma" => Normalization::Gamma,
        "arcsinh" => Normalization::Arcsinh,
        _ => return None,
    })
}

fn rgba_to_hex(c: [u8; 4]) -> String {
    format!("{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3])
}

fn parse_rgba(val: &str) -> Option<[u8; 4]> {
    if val.len() != 8 {
        return None;
    }
    Some([
        u8::from_str_radix(&val[0..2], 16).ok()?,
        u8::from_str_radix(&val[2..4], 16).ok()?,
        u8::from_str_radix(&val[4..6], 16).ok()?,
        u8::from_str_radix(&val[6..8], 16).ok()?,
    ])
}

fn parse_percentiles(val: &str) -> Option<(f64, f64)> {
    let mut it = val.split_whitespace();
    let low = it.next()?.parse().ok()?;
    let high = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((low, high))
}

fn parse_bool(val: &str) -> Option<bool> {
    match val {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn lut_to_hex(lut: &[[u8; 4]; 256]) -> String {
    let mut s = String::with_capacity(256 * 8);
    for entry in lut {
        for byte in entry {
            s.push_str(&format!("{byte:02x}"));
        }
    }
    s
}

fn parse_lut(val: &str) -> Option<[[u8; 4]; 256]> {
    // 256 entries × 4 bytes × 2 hex digits.
    if val.len() != 256 * 4 * 2 || !val.is_char_boundary(0) {
        return None;
    }
    let bytes = val.as_bytes();
    if !bytes.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let mut lut = [[0u8; 4]; 256];
    for (i, entry) in lut.iter_mut().enumerate() {
        for (j, byte) in entry.iter_mut().enumerate() {
            let off = (i * 4 + j) * 2;
            *byte = u8::from_str_radix(&val[off..off + 2], 16).ok()?;
        }
    }
    Some(lut)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::colormap::ColormapName;

    fn round_trip(cm: &Colormap) -> Colormap {
        decode_colormap(&encode_colormap(cm)).expect("decode our own encode")
    }

    #[test]
    fn round_trips_a_catalog_colormap() {
        let cm = Colormap::new(ColormapName::Viridis, -3.0, 7.5);
        assert_eq!(round_trip(&cm), cm);
    }

    #[test]
    fn round_trips_every_non_default_field() {
        let mut cm = Colormap::new(ColormapName::Jet, 0.25, 100.0)
            .with_normalization(Normalization::Arcsinh)
            .with_gamma(3.5)
            .with_nan_color([10, 20, 30, 40]);
        assert!(cm.set_autoscale_percentiles(2.5, 97.5));
        cm.set_editable(false);
        assert_eq!(round_trip(&cm), cm);
    }

    #[test]
    fn round_trips_a_custom_lut_colormap() {
        // A from_colors LUT has no catalog name; the LUT itself must survive.
        let cm = Colormap::from_colors(&[[1, 2, 3, 255], [250, 240, 230, 128]], 0.0, 1.0).unwrap();
        let back = round_trip(&cm);
        assert_eq!(back.lut, cm.lut);
        assert_eq!(back, cm);
    }

    #[test]
    fn bad_header_is_rejected() {
        assert_eq!(decode_colormap("nope\n"), Err(ColormapIoError::BadHeader));
        assert_eq!(decode_colormap(""), Err(ColormapIoError::BadHeader));
    }

    #[test]
    fn missing_lut_is_an_error() {
        let text = "siplot-colormap 1\nvmin 0\nvmax 1\n";
        assert_eq!(
            decode_colormap(text),
            Err(ColormapIoError::MissingField("lut"))
        );
    }

    #[test]
    fn absent_optional_fields_fall_back_to_defaults() {
        // Only the header + lut: everything else defaults (silx restoreState).
        let base = Colormap::new(ColormapName::Gray, 0.0, 1.0);
        let text = format!("siplot-colormap 1\nlut {}\n", lut_to_hex(&base.lut));
        let cm = decode_colormap(&text).unwrap();
        assert_eq!(cm, base);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let cm = Colormap::new(ColormapName::Magma, 1.0, 2.0);
        let mut text = encode_colormap(&cm);
        text.push_str("future_field 123\n");
        assert_eq!(decode_colormap(&text).unwrap(), cm);
    }

    #[test]
    fn bad_value_names_its_field() {
        let cm = Colormap::new(ColormapName::Gray, 0.0, 1.0);
        let good = encode_colormap(&cm);
        let broken = good.replace("normalization linear", "normalization wat");
        assert_eq!(
            decode_colormap(&broken),
            Err(ColormapIoError::BadValue("normalization"))
        );
        let short_lut = "siplot-colormap 1\nlut abcd\n";
        assert_eq!(
            decode_colormap(short_lut),
            Err(ColormapIoError::BadValue("lut"))
        );
    }
}
