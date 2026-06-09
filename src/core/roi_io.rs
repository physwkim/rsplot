//! Serialize a list of [`ManagedRoi`] to and from a flat text format, mirroring
//! silx `CurvesROIWidget.save`/`load` (which dump a dict of ROIs through
//! `silx.io.dictdump`, `CurvesROIWidget.py:889-918`, each ROI a keyed record
//! with `type`/`name`/geometry — `ROI.toDict`/`_fromDict`, :1140-1169).
//!
//! siplot has no serde dependency, so this is a hand-written, line-oriented
//! encoder/decoder over a fixed schema — the same manual-serialization approach
//! as the `.npy` mask path ([`crate::widget::mask_tools`] `read_npy`). The pure
//! [`encode_rois`]/[`decode_rois`] functions are headlessly testable;
//! [`save_rois`]/[`load_rois`] are the thin filesystem wrappers (silx
//! `save(filename)`/`load(filename)`).
//!
//! Format (version 1):
//!
//! ```text
//! siplot-roi 1
//! roi <type>
//! name <display name — the rest of the line>
//! color <RRGGBBAA hex | none>
//! line_width <number>
//! line_style <solid | dashed | dotted>
//! gap_color <RRGGBBAA hex | none>
//! fill <true | false>
//! geom <space-separated numbers; arity depends on the type>
//! ```
//!
//! A record begins at each `roi <type>` line. The transient `selected`
//! highlight is not persisted (silx's `toDict` likewise omits it); decoded ROIs
//! are unselected. Unknown keys are ignored for forward compatibility (silx
//! stashes them in `_extraInfo`).

use egui::Color32;

use crate::core::roi::{ManagedRoi, Roi, RoiLineStyle};

/// Magic + version header of a siplot ROI file.
const ROI_IO_HEADER: &str = "siplot-roi 1";

/// An error decoding a siplot ROI text blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoiIoError {
    /// Missing or wrong `siplot-roi <version>` header line.
    BadHeader,
    /// A record was missing a required field (e.g. the `geom` line, or a
    /// field line appeared before any `roi <type>` line).
    MissingField(&'static str),
    /// `roi <type>` named a ROI variant this version does not know.
    UnknownType(String),
    /// A `color` / `line_width` / `line_style` / `fill` / `geom` value did not
    /// parse; the field name is given.
    BadValue(&'static str),
    /// The `geom` number count did not match the ROI type's arity.
    BadGeometry,
}

impl std::fmt::Display for RoiIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoiIoError::BadHeader => write!(f, "missing or wrong `{ROI_IO_HEADER}` header"),
            RoiIoError::MissingField(k) => write!(f, "missing required field `{k}`"),
            RoiIoError::UnknownType(t) => write!(f, "unknown ROI type `{t}`"),
            RoiIoError::BadValue(k) => write!(f, "invalid value for field `{k}`"),
            RoiIoError::BadGeometry => {
                write!(f, "wrong number of geometry values for the ROI type")
            }
        }
    }
}

impl std::error::Error for RoiIoError {}

/// Serialize `rois` to the siplot ROI text format (silx `CurvesROIWidget.save`
/// dict-dump). Geometry, name, per-ROI color, outline width/style, gap color,
/// and fill are persisted; the transient `selected` highlight is not.
#[must_use]
pub fn encode_rois(rois: &[ManagedRoi]) -> String {
    let mut out = String::new();
    out.push_str(ROI_IO_HEADER);
    out.push('\n');
    for r in rois {
        let (type_name, geom) = roi_type_and_geom(&r.roi);
        push_kv(&mut out, "roi", type_name);
        // The name is the rest of the line; strip newlines so one record stays
        // one block.
        push_kv(&mut out, "name", &r.name.replace(['\n', '\r'], " "));
        push_kv(&mut out, "color", &color_to_hex(r.color));
        push_kv(&mut out, "line_width", &r.line_width.to_string());
        push_kv(&mut out, "line_style", line_style_str(r.line_style));
        push_kv(&mut out, "gap_color", &color_to_hex(r.gap_color));
        push_kv(&mut out, "fill", if r.fill { "true" } else { "false" });
        let geom_str = geom
            .iter()
            .map(f64::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        push_kv(&mut out, "geom", &geom_str);
    }
    out
}

/// Parse a siplot ROI text blob back into [`ManagedRoi`]s (silx
/// `CurvesROIWidget.load`). Missing optional appearance fields fall back to
/// [`ManagedRoi::new`]'s silx defaults; a record needs at least its
/// `roi <type>` line and a `geom` line.
pub fn decode_rois(text: &str) -> Result<Vec<ManagedRoi>, RoiIoError> {
    let mut lines = text.lines();
    match lines.next() {
        Some(h) if h == ROI_IO_HEADER => {}
        _ => return Err(RoiIoError::BadHeader),
    }

    let mut rois = Vec::new();
    let mut cur: Option<RecordBuilder> = None;
    for line in lines {
        if line.is_empty() {
            continue; // tolerate blank separator lines
        }
        let (key, val) = split_kv(line);
        if key == "roi" {
            if let Some(b) = cur.take() {
                rois.push(b.build()?);
            }
            cur = Some(RecordBuilder::new(val.to_string()));
        } else {
            let b = cur.as_mut().ok_or(RoiIoError::MissingField("roi"))?;
            b.set(key, val)?;
        }
    }
    if let Some(b) = cur.take() {
        rois.push(b.build()?);
    }
    Ok(rois)
}

/// Write `rois` to `path` in the siplot ROI text format (silx
/// `CurvesROIWidget.save(filename)`).
pub fn save_rois(path: impl AsRef<std::path::Path>, rois: &[ManagedRoi]) -> std::io::Result<()> {
    std::fs::write(path, encode_rois(rois))
}

/// Read ROIs from `path` written by [`save_rois`] (silx
/// `CurvesROIWidget.load(filename)`). A parse error maps to
/// [`std::io::ErrorKind::InvalidData`].
pub fn load_rois(path: impl AsRef<std::path::Path>) -> std::io::Result<Vec<ManagedRoi>> {
    let text = std::fs::read_to_string(path)?;
    decode_rois(&text)
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

fn color_to_hex(c: Option<Color32>) -> String {
    // Store the raw (premultiplied) bytes so the decode round-trips exactly.
    match c {
        Some(c) => format!("{:02X}{:02X}{:02X}{:02X}", c.r(), c.g(), c.b(), c.a()),
        None => "none".to_string(),
    }
}

fn parse_color(val: &str) -> Result<Option<Color32>, RoiIoError> {
    if val == "none" {
        return Ok(None);
    }
    if val.len() != 8 || !val.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(RoiIoError::BadValue("color"));
    }
    let byte = |i: usize| u8::from_str_radix(&val[i..i + 2], 16).expect("validated hex");
    Ok(Some(Color32::from_rgba_premultiplied(
        byte(0),
        byte(2),
        byte(4),
        byte(6),
    )))
}

fn line_style_str(s: RoiLineStyle) -> &'static str {
    match s {
        RoiLineStyle::Solid => "solid",
        RoiLineStyle::Dashed => "dashed",
        RoiLineStyle::Dotted => "dotted",
    }
}

fn parse_line_style(val: &str) -> Result<RoiLineStyle, RoiIoError> {
    match val {
        "solid" => Ok(RoiLineStyle::Solid),
        "dashed" => Ok(RoiLineStyle::Dashed),
        "dotted" => Ok(RoiLineStyle::Dotted),
        _ => Err(RoiIoError::BadValue("line_style")),
    }
}

fn parse_bool(val: &str) -> Result<bool, RoiIoError> {
    match val {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(RoiIoError::BadValue("fill")),
    }
}

/// The type tag and flat geometry numbers for a [`Roi`] (inverse of
/// [`roi_from_type_and_geom`]).
fn roi_type_and_geom(roi: &Roi) -> (&'static str, Vec<f64>) {
    match roi {
        Roi::Rect { x, y } => ("rect", vec![x.0, x.1, y.0, y.1]),
        Roi::HRange { y } => ("hrange", vec![y.0, y.1]),
        Roi::VRange { x } => ("vrange", vec![x.0, x.1]),
        Roi::HLine { y } => ("hline", vec![*y]),
        Roi::VLine { x } => ("vline", vec![*x]),
        Roi::Point { x, y } => ("point", vec![*x, *y]),
        Roi::Line { start, end } => ("line", vec![start.0, start.1, end.0, end.1]),
        Roi::Polygon { vertices } => (
            "polygon",
            vertices.iter().flat_map(|v| [v.0, v.1]).collect(),
        ),
        Roi::Cross { center } => ("cross", vec![center.0, center.1]),
        Roi::Circle { center, radius } => ("circle", vec![center.0, center.1, *radius]),
        Roi::Ellipse {
            center,
            radii,
            orientation,
        } => (
            "ellipse",
            vec![center.0, center.1, radii.0, radii.1, *orientation],
        ),
        Roi::Arc {
            center,
            inner_radius,
            outer_radius,
            start_angle,
            end_angle,
        } => (
            "arc",
            vec![
                center.0,
                center.1,
                *inner_radius,
                *outer_radius,
                *start_angle,
                *end_angle,
            ],
        ),
        Roi::Band { begin, end, width } => ("band", vec![begin.0, begin.1, end.0, end.1, *width]),
    }
}

/// Rebuild a [`Roi`] from its type tag and flat geometry numbers (inverse of
/// [`roi_type_and_geom`]).
fn roi_from_type_and_geom(type_name: &str, g: &[f64]) -> Result<Roi, RoiIoError> {
    let need = |n: usize| {
        if g.len() == n {
            Ok(())
        } else {
            Err(RoiIoError::BadGeometry)
        }
    };
    Ok(match type_name {
        "rect" => {
            need(4)?;
            Roi::Rect {
                x: (g[0], g[1]),
                y: (g[2], g[3]),
            }
        }
        "hrange" => {
            need(2)?;
            Roi::HRange { y: (g[0], g[1]) }
        }
        "vrange" => {
            need(2)?;
            Roi::VRange { x: (g[0], g[1]) }
        }
        "hline" => {
            need(1)?;
            Roi::HLine { y: g[0] }
        }
        "vline" => {
            need(1)?;
            Roi::VLine { x: g[0] }
        }
        "point" => {
            need(2)?;
            Roi::Point { x: g[0], y: g[1] }
        }
        "line" => {
            need(4)?;
            Roi::Line {
                start: (g[0], g[1]),
                end: (g[2], g[3]),
            }
        }
        "polygon" => {
            if g.is_empty() || !g.len().is_multiple_of(2) {
                return Err(RoiIoError::BadGeometry);
            }
            Roi::Polygon {
                vertices: g.chunks_exact(2).map(|c| (c[0], c[1])).collect(),
            }
        }
        "cross" => {
            need(2)?;
            Roi::Cross {
                center: (g[0], g[1]),
            }
        }
        "circle" => {
            need(3)?;
            Roi::Circle {
                center: (g[0], g[1]),
                radius: g[2],
            }
        }
        "ellipse" => {
            need(5)?;
            Roi::Ellipse {
                center: (g[0], g[1]),
                radii: (g[2], g[3]),
                orientation: g[4],
            }
        }
        "arc" => {
            need(6)?;
            Roi::Arc {
                center: (g[0], g[1]),
                inner_radius: g[2],
                outer_radius: g[3],
                start_angle: g[4],
                end_angle: g[5],
            }
        }
        "band" => {
            need(5)?;
            Roi::Band {
                begin: (g[0], g[1]),
                end: (g[2], g[3]),
                width: g[4],
            }
        }
        other => return Err(RoiIoError::UnknownType(other.to_string())),
    })
}

/// Accumulates one record's fields, then [`build`](RecordBuilder::build)s a
/// [`ManagedRoi`] seeded from [`ManagedRoi::new`]'s defaults.
struct RecordBuilder {
    type_name: String,
    name: Option<String>,
    color: Option<Option<Color32>>,
    line_width: Option<f32>,
    line_style: Option<RoiLineStyle>,
    gap_color: Option<Option<Color32>>,
    fill: Option<bool>,
    geom: Option<Vec<f64>>,
}

impl RecordBuilder {
    fn new(type_name: String) -> Self {
        Self {
            type_name,
            name: None,
            color: None,
            line_width: None,
            line_style: None,
            gap_color: None,
            fill: None,
            geom: None,
        }
    }

    fn set(&mut self, key: &str, val: &str) -> Result<(), RoiIoError> {
        match key {
            "name" => self.name = Some(val.to_string()),
            "color" => self.color = Some(parse_color(val)?),
            "line_width" => {
                self.line_width = Some(
                    val.parse()
                        .map_err(|_| RoiIoError::BadValue("line_width"))?,
                )
            }
            "line_style" => self.line_style = Some(parse_line_style(val)?),
            "gap_color" => self.gap_color = Some(parse_color(val)?),
            "fill" => self.fill = Some(parse_bool(val)?),
            "geom" => {
                let mut v = Vec::new();
                for tok in val.split_whitespace() {
                    v.push(
                        tok.parse::<f64>()
                            .map_err(|_| RoiIoError::BadValue("geom"))?,
                    );
                }
                self.geom = Some(v);
            }
            // Unknown keys are ignored for forward compatibility (silx _extraInfo).
            _ => {}
        }
        Ok(())
    }

    fn build(self) -> Result<ManagedRoi, RoiIoError> {
        let geom = self.geom.ok_or(RoiIoError::MissingField("geom"))?;
        // Seed from `new` so absent appearance fields keep silx defaults.
        let mut m = ManagedRoi::new(roi_from_type_and_geom(&self.type_name, &geom)?);
        if let Some(n) = self.name {
            m.name = n;
        }
        if let Some(c) = self.color {
            m.color = c;
        }
        if let Some(w) = self.line_width {
            m.line_width = w;
        }
        if let Some(s) = self.line_style {
            m.line_style = s;
        }
        if let Some(g) = self.gap_color {
            m.gap_color = g;
        }
        if let Some(f) = self.fill {
            m.fill = f;
        }
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn one_per_variant() -> Vec<ManagedRoi> {
        vec![
            ManagedRoi::new(Roi::Rect {
                x: (0.0, 10.0),
                y: (-2.5, 3.25),
            }),
            ManagedRoi::new(Roi::HRange { y: (1.0, 2.0) }),
            ManagedRoi::new(Roi::VRange { x: (3.0, 4.0) }),
            ManagedRoi::new(Roi::HLine { y: 5.0 }),
            ManagedRoi::new(Roi::VLine { x: 6.0 }),
            ManagedRoi::new(Roi::Point { x: 7.0, y: 8.0 }),
            ManagedRoi::new(Roi::Line {
                start: (0.0, 0.0),
                end: (1.0, 1.0),
            }),
            ManagedRoi::new(Roi::Polygon {
                vertices: vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)],
            }),
            ManagedRoi::new(Roi::Cross { center: (2.0, 3.0) }),
            ManagedRoi::new(Roi::Circle {
                center: (1.0, 1.0),
                radius: 2.0,
            }),
            ManagedRoi::new(Roi::Ellipse {
                center: (1.0, 1.0),
                radii: (2.0, 3.0),
                orientation: PI / 4.0,
            }),
            ManagedRoi::new(Roi::Arc {
                center: (0.0, 0.0),
                inner_radius: 1.0,
                outer_radius: 2.0,
                start_angle: 0.0,
                end_angle: PI,
            }),
            ManagedRoi::new(Roi::Band {
                begin: (0.0, 0.0),
                end: (4.0, 0.0),
                width: 1.5,
            }),
        ]
    }

    #[test]
    fn round_trip_every_variant_with_defaults() {
        let rois = one_per_variant();
        let decoded = decode_rois(&encode_rois(&rois)).expect("decodes");
        assert_eq!(decoded, rois);
    }

    #[test]
    fn round_trip_full_appearance() {
        // A ROI with every appearance field set away from the defaults, plus a
        // name containing spaces (the "rest of line" parse must keep them).
        let mut m = ManagedRoi::new(Roi::Rect {
            x: (0.0, 1.0),
            y: (0.0, 1.0),
        });
        m.name = "box one (left)".to_string();
        m.color = Some(Color32::from_rgba_premultiplied(0x12, 0x34, 0x56, 0x78));
        m.line_width = 2.5;
        m.line_style = RoiLineStyle::Dashed;
        m.gap_color = Some(Color32::from_rgba_premultiplied(0xAB, 0xCD, 0xEF, 0xFF));
        m.fill = true;
        let rois = vec![m];
        assert_eq!(decode_rois(&encode_rois(&rois)).expect("decodes"), rois);
    }

    #[test]
    fn empty_list_round_trips_to_header_only() {
        assert_eq!(encode_rois(&[]), "siplot-roi 1\n");
        assert_eq!(decode_rois("siplot-roi 1\n").expect("decodes"), vec![]);
    }

    #[test]
    fn selected_flag_is_not_persisted() {
        let mut m = ManagedRoi::new(Roi::Point { x: 1.0, y: 2.0 });
        m.selected = true;
        let decoded = decode_rois(&encode_rois(&[m])).expect("decodes");
        assert_eq!(decoded.len(), 1);
        assert!(!decoded[0].selected, "decoded ROI is unselected");
    }

    #[test]
    fn bad_header_rejected() {
        assert_eq!(decode_rois("nope\n"), Err(RoiIoError::BadHeader));
        assert_eq!(decode_rois(""), Err(RoiIoError::BadHeader));
    }

    #[test]
    fn unknown_type_rejected() {
        let text = "siplot-roi 1\nroi blob\ngeom 0 0\n";
        assert_eq!(
            decode_rois(text),
            Err(RoiIoError::UnknownType("blob".to_string()))
        );
    }

    #[test]
    fn wrong_geometry_arity_rejected() {
        // A rect needs 4 numbers.
        let text = "siplot-roi 1\nroi rect\ngeom 0 1 2\n";
        assert_eq!(decode_rois(text), Err(RoiIoError::BadGeometry));
    }

    #[test]
    fn bad_color_rejected() {
        let text = "siplot-roi 1\nroi point\ncolor xyz\ngeom 0 0\n";
        assert_eq!(decode_rois(text), Err(RoiIoError::BadValue("color")));
    }

    #[test]
    fn unknown_keys_are_tolerated() {
        // A forward-compat field siplot does not know is ignored, not an error.
        let text = "siplot-roi 1\nroi point\nfuture_field whatever\ngeom 3 4\n";
        let decoded = decode_rois(text).expect("tolerates unknown key");
        assert_eq!(
            decoded,
            vec![ManagedRoi::new(Roi::Point { x: 3.0, y: 4.0 })]
        );
    }
}
