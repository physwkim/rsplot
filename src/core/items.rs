//! Shared item vocabulary used by both the GPU data layer and the egui overlay
//! layer: line stroke styles, curve symbols, filled-curve baselines, and error
//! bars.
//!
//! These types live in `core` (not `render`) so the `core::Plot` model — which
//! stores overlay items (markers, shapes) carrying a [`LineStyle`] — and the
//! backend API can name curve styling without `core` depending on `render`
//! (`doc/design.md` §9 `core/items.rs`).

/// Line stroke style (silx `linestyle`). Dash lengths for the predefined styles
/// scale with the line width (`max(width, 1)`) so they stay proportionate at any
/// thickness; a [`LineStyle::Custom`] pattern is taken verbatim. The dash unit is
/// physical pixels on the GPU curve path and logical points on the egui painter
/// overlay path.
#[derive(Clone, Debug, PartialEq)]
pub enum LineStyle {
    /// No line drawn (markers only, if any). silx `' '` / `''`.
    None,
    /// Continuous line. silx `'-'`.
    Solid,
    /// Dashed line. silx `'--'`.
    Dashed,
    /// Dash-dot line. silx `'-.'`.
    DashDot,
    /// Dotted line. silx `':'`.
    Dotted,
    /// Custom dash pattern: alternating on/off lengths (`on, off, on, off`), with
    /// `offset` the starting phase. silx `(offset, (dash pattern))`.
    Custom { offset: f32, pattern: Vec<f32> },
}

impl LineStyle {
    /// Whether this style draws a line at all (false only for [`LineStyle::None`]).
    pub(crate) fn draws_line(&self) -> bool {
        !matches!(self, LineStyle::None)
    }

    /// Dash and gap lengths plus the phase offset for egui's
    /// [`egui::Shape::dashed_line_with_offset`], or `None` for a solid (un-dashed)
    /// line. This is the painter-overlay counterpart of the GPU curve's
    /// `dash_spec`: the same proportions, expressed as the dash/gap arrays egui's
    /// dashed-line builder consumes (lengths in logical points). Predefined
    /// patterns scale with `max(width, 1)` so they look right at any thickness.
    pub(crate) fn painter_dashes(&self, width: f32) -> Option<(Vec<f32>, Vec<f32>, f32)> {
        let u = width.max(1.0);
        match self {
            LineStyle::None | LineStyle::Solid => None,
            // on, off
            LineStyle::Dashed => Some((vec![5.0 * u], vec![4.0 * u], 0.0)),
            // dot, gap
            LineStyle::Dotted => Some((vec![1.5 * u], vec![2.5 * u], 0.0)),
            // dash, gap, dot, gap
            LineStyle::DashDot => Some((vec![6.0 * u, 1.5 * u], vec![3.0 * u, 3.0 * u], 0.0)),
            LineStyle::Custom { offset, pattern } => {
                // pattern = [on, off, on, off, ...]: dashes are the even indices,
                // gaps the odd ones. egui cycles each array independently.
                let dashes: Vec<f32> = pattern.iter().step_by(2).copied().collect();
                let gaps: Vec<f32> = pattern.iter().skip(1).step_by(2).copied().collect();
                // A pattern with no gap (or a zero-length period) is just a solid
                // line: leave it un-dashed so the modulo stays well-defined.
                let period: f32 = dashes.iter().chain(&gaps).sum();
                if dashes.is_empty() || gaps.is_empty() || period <= 0.0 {
                    None
                } else {
                    Some((dashes, gaps, *offset))
                }
            }
        }
    }
}

/// Marker symbol drawn at each curve vertex (silx `addCurve` `symbol`). The
/// catalog mirrors silx's full GL-backend symbol set (`silx.gui.plot.items.core`
/// `SymbolMixIn._SUPPORTED_SYMBOLS`); [`Symbol::Triangle`] is an egui extra silx
/// has no code for. The `Heart` glyph (silx `'♥'`) is not implemented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Symbol {
    /// Circle marker. silx `'o'`.
    Circle,
    /// Square marker. silx `'s'`.
    Square,
    /// Diagonal "x" marker. silx `'x'`.
    Cross,
    /// Upright "+" marker. silx `'+'`.
    Plus,
    /// Upward-pointing triangle marker (egui extra; not a silx symbol).
    Triangle,
    /// Diamond (rotated square) marker. silx `'d'`.
    Diamond,
    /// Small filled circle. silx `'.'`.
    Point,
    /// Single-pixel square. silx `','`.
    Pixel,
    /// Vertical line stroke. silx `'|'`.
    VerticalLine,
    /// Horizontal line stroke. silx `'_'`.
    HorizontalLine,
    /// Leftward (left half) tick stroke. silx `'tickleft'`.
    TickLeft,
    /// Rightward (right half) tick stroke. silx `'tickright'`.
    TickRight,
    /// Upward (top half) tick stroke. silx `'tickup'`.
    TickUp,
    /// Downward (bottom half) tick stroke. silx `'tickdown'`.
    TickDown,
    /// Left-pointing open caret. silx `'caretleft'`.
    CaretLeft,
    /// Right-pointing open caret. silx `'caretright'`.
    CaretRight,
    /// Up-pointing open caret. silx `'caretup'`.
    CaretUp,
    /// Down-pointing open caret. silx `'caretdown'`.
    CaretDown,
}

impl Symbol {
    /// Shader symbol code (must match the `switch` in `markers.wgsl`).
    pub(crate) fn code(self) -> u32 {
        match self {
            Symbol::Circle => 0,
            Symbol::Square => 1,
            Symbol::Cross => 2,
            Symbol::Plus => 3,
            Symbol::Triangle => 4,
            Symbol::Diamond => 5,
            Symbol::Point => 6,
            Symbol::Pixel => 7,
            Symbol::VerticalLine => 8,
            Symbol::HorizontalLine => 9,
            Symbol::TickLeft => 10,
            Symbol::TickRight => 11,
            Symbol::TickUp => 12,
            Symbol::TickDown => 13,
            Symbol::CaretLeft => 14,
            Symbol::CaretRight => 15,
            Symbol::CaretUp => 16,
            Symbol::CaretDown => 17,
        }
    }

    /// The physical-pixel size (full extent) this symbol is actually drawn at,
    /// given the curve's requested `marker_size`. Mirrors the size overrides in
    /// silx `GLPlotCurve.SymbolPoints.render`:
    ///
    /// - [`Symbol::Pixel`] is always a single pixel.
    /// - [`Symbol::Point`] shrinks to `ceil(0.5 * size) + 1`, the small dot
    ///   matplotlib draws for `'.'`.
    /// - The 1-pixel strokes ([`Symbol::Plus`], the lines, and the ticks) round to
    ///   the nearest odd pixel so the stroke straddles a pixel center.
    /// - Every other symbol keeps `marker_size` unchanged.
    pub(crate) fn render_size_px(self, marker_size: f32) -> f32 {
        match self {
            Symbol::Pixel => 1.0,
            Symbol::Point => (0.5 * marker_size).ceil() + 1.0,
            Symbol::Plus
            | Symbol::VerticalLine
            | Symbol::HorizontalLine
            | Symbol::TickLeft
            | Symbol::TickRight
            | Symbol::TickUp
            | Symbol::TickDown => (marker_size / 2.0).floor() * 2.0 + 1.0,
            _ => marker_size,
        }
    }

    /// The silx symbol code for this symbol, or `None` for [`Symbol::Triangle`]
    /// (an egui extra silx has no code for). The inverse of the codes accepted by
    /// [`Symbol::from_code`]; matches the keys of silx
    /// `SymbolMixIn._SUPPORTED_SYMBOLS`.
    pub fn code_str(self) -> Option<&'static str> {
        Some(match self {
            Symbol::Circle => "o",
            Symbol::Diamond => "d",
            Symbol::Square => "s",
            Symbol::Plus => "+",
            Symbol::Cross => "x",
            Symbol::Point => ".",
            Symbol::Pixel => ",",
            Symbol::VerticalLine => "|",
            Symbol::HorizontalLine => "_",
            Symbol::TickLeft => "tickleft",
            Symbol::TickRight => "tickright",
            Symbol::TickUp => "tickup",
            Symbol::TickDown => "tickdown",
            Symbol::CaretLeft => "caretleft",
            Symbol::CaretRight => "caretright",
            Symbol::CaretUp => "caretup",
            Symbol::CaretDown => "caretdown",
            Symbol::Triangle => return None,
        })
    }

    /// Parse a silx symbol code or human-readable name into a [`Symbol`], or
    /// `None` if unrecognized. Mirrors silx `SymbolMixIn.setSymbol`: a code from
    /// `_SUPPORTED_SYMBOLS` matches first, otherwise the human-readable name is
    /// matched case-insensitively. silx's empty-string ("None") symbol and the
    /// `'♥'` Heart glyph are not representable here, so they return `None`.
    /// [`Symbol::Triangle`] has no silx code and is reachable only by its name
    /// `"triangle"`.
    pub fn from_code(s: &str) -> Option<Symbol> {
        let symbol = match s {
            "o" => Symbol::Circle,
            "d" => Symbol::Diamond,
            "s" => Symbol::Square,
            "+" => Symbol::Plus,
            "x" => Symbol::Cross,
            "." => Symbol::Point,
            "," => Symbol::Pixel,
            "|" => Symbol::VerticalLine,
            "_" => Symbol::HorizontalLine,
            "tickleft" => Symbol::TickLeft,
            "tickright" => Symbol::TickRight,
            "tickup" => Symbol::TickUp,
            "tickdown" => Symbol::TickDown,
            "caretleft" => Symbol::CaretLeft,
            "caretright" => Symbol::CaretRight,
            "caretup" => Symbol::CaretUp,
            "caretdown" => Symbol::CaretDown,
            // Not a silx code: case-insensitive match on the human-readable name.
            _ => {
                return match s.to_ascii_lowercase().as_str() {
                    "circle" => Some(Symbol::Circle),
                    "diamond" => Some(Symbol::Diamond),
                    "square" => Some(Symbol::Square),
                    "plus" => Some(Symbol::Plus),
                    "cross" => Some(Symbol::Cross),
                    "point" => Some(Symbol::Point),
                    "pixel" => Some(Symbol::Pixel),
                    "vertical line" => Some(Symbol::VerticalLine),
                    "horizontal line" => Some(Symbol::HorizontalLine),
                    "tick left" => Some(Symbol::TickLeft),
                    "tick right" => Some(Symbol::TickRight),
                    "tick up" => Some(Symbol::TickUp),
                    "tick down" => Some(Symbol::TickDown),
                    "caret left" => Some(Symbol::CaretLeft),
                    "caret right" => Some(Symbol::CaretRight),
                    "caret up" => Some(Symbol::CaretUp),
                    "caret down" => Some(Symbol::CaretDown),
                    "triangle" => Some(Symbol::Triangle),
                    _ => None,
                };
            }
        };
        Some(symbol)
    }
}

/// Where a filled curve's area extends to (silx `baseline`). The fill is the
/// band between the curve and this baseline.
#[derive(Clone, Debug, PartialEq)]
pub enum Baseline {
    /// Fill down to a constant y value (silx scalar baseline; `0.0` by default).
    Scalar(f64),
    /// Fill to a per-vertex y value (silx array baseline), one entry per vertex.
    PerPoint(Vec<f64>),
}

impl Baseline {
    /// The baseline y values for an `n`-vertex curve, broadcasting a scalar.
    pub(crate) fn values(&self, n: usize) -> Vec<f32> {
        match self {
            Baseline::Scalar(v) => vec![*v as f32; n],
            Baseline::PerPoint(vs) => vs.iter().map(|&v| v as f32).collect(),
        }
    }
}

/// Per-point uncertainty drawn as error bars (silx `xerror` / `yerror`).
#[derive(Clone, Debug, PartialEq)]
pub enum ErrorBars {
    /// The same `+/-` error for every point (silx scalar error).
    Symmetric(f64),
    /// A per-point symmetric `+/-` error (silx 1D error array).
    PerPoint(Vec<f64>),
    /// Per-point asymmetric error: `lower` extends below/left, `upper`
    /// above/right (silx `(2, N)` error array).
    Asymmetric { lower: Vec<f64>, upper: Vec<f64> },
}

impl ErrorBars {
    /// The `(lower, upper)` error magnitudes at point `i`.
    pub(crate) fn bounds(&self, i: usize) -> (f32, f32) {
        match self {
            ErrorBars::Symmetric(e) => (*e as f32, *e as f32),
            ErrorBars::PerPoint(es) => (es[i] as f32, es[i] as f32),
            ErrorBars::Asymmetric { lower, upper } => (lower[i] as f32, upper[i] as f32),
        }
    }

    /// Panic if a per-point/asymmetric array does not match the vertex count.
    pub(crate) fn check_len(&self, n: usize) {
        match self {
            ErrorBars::Symmetric(_) => {}
            ErrorBars::PerPoint(es) => {
                assert_eq!(
                    es.len(),
                    n,
                    "per-point error must have one entry per vertex"
                );
            }
            ErrorBars::Asymmetric { lower, upper } => {
                assert_eq!(
                    lower.len(),
                    n,
                    "asymmetric error `lower` must have one entry per vertex"
                );
                assert_eq!(
                    upper.len(),
                    n,
                    "asymmetric error `upper` must have one entry per vertex"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_line_false_only_for_none() {
        assert!(!LineStyle::None.draws_line());
        assert!(LineStyle::Solid.draws_line());
        assert!(LineStyle::Dashed.draws_line());
        assert!(LineStyle::DashDot.draws_line());
        assert!(LineStyle::Dotted.draws_line());
    }

    #[test]
    fn painter_dashes_solid_and_none_are_undashed() {
        assert_eq!(LineStyle::Solid.painter_dashes(1.0), None);
        assert_eq!(LineStyle::None.painter_dashes(1.0), None);
    }

    #[test]
    fn painter_dashes_predefined_scale_with_width() {
        // Dashed at width 1: on 5, off 4.
        assert_eq!(
            LineStyle::Dashed.painter_dashes(1.0),
            Some((vec![5.0], vec![4.0], 0.0))
        );
        // Width 2 doubles the unit.
        assert_eq!(
            LineStyle::Dashed.painter_dashes(2.0),
            Some((vec![10.0], vec![8.0], 0.0))
        );
        // Dash-dot: dashes [6, 1.5], gaps [3, 3].
        assert_eq!(
            LineStyle::DashDot.painter_dashes(1.0),
            Some((vec![6.0, 1.5], vec![3.0, 3.0], 0.0))
        );
    }

    /// Every silx symbol code and its corresponding [`Symbol`]; the canonical
    /// set used to check the code mapping in both directions.
    const SILX_CODES: &[(&str, Symbol)] = &[
        ("o", Symbol::Circle),
        ("d", Symbol::Diamond),
        ("s", Symbol::Square),
        ("+", Symbol::Plus),
        ("x", Symbol::Cross),
        (".", Symbol::Point),
        (",", Symbol::Pixel),
        ("|", Symbol::VerticalLine),
        ("_", Symbol::HorizontalLine),
        ("tickleft", Symbol::TickLeft),
        ("tickright", Symbol::TickRight),
        ("tickup", Symbol::TickUp),
        ("tickdown", Symbol::TickDown),
        ("caretleft", Symbol::CaretLeft),
        ("caretright", Symbol::CaretRight),
        ("caretup", Symbol::CaretUp),
        ("caretdown", Symbol::CaretDown),
    ];

    #[test]
    fn from_code_maps_every_silx_code() {
        for &(code, symbol) in SILX_CODES {
            assert_eq!(Symbol::from_code(code), Some(symbol), "code {code:?}");
        }
    }

    #[test]
    fn code_str_round_trips_every_coded_symbol() {
        for &(code, symbol) in SILX_CODES {
            assert_eq!(symbol.code_str(), Some(code), "reverse of {symbol:?}");
            assert_eq!(
                Symbol::from_code(symbol.code_str().unwrap()),
                Some(symbol),
                "round-trip of {symbol:?}"
            );
        }
    }

    #[test]
    fn from_code_matches_human_names_case_insensitively() {
        // Each silx human-readable name (case-insensitive), one per symbol.
        assert_eq!(Symbol::from_code("Circle"), Some(Symbol::Circle));
        assert_eq!(Symbol::from_code("DIAMOND"), Some(Symbol::Diamond));
        assert_eq!(Symbol::from_code("square"), Some(Symbol::Square));
        assert_eq!(Symbol::from_code("Plus"), Some(Symbol::Plus));
        assert_eq!(Symbol::from_code("Cross"), Some(Symbol::Cross));
        assert_eq!(Symbol::from_code("Point"), Some(Symbol::Point));
        assert_eq!(Symbol::from_code("Pixel"), Some(Symbol::Pixel));
        assert_eq!(
            Symbol::from_code("Vertical line"),
            Some(Symbol::VerticalLine)
        );
        assert_eq!(
            Symbol::from_code("Horizontal line"),
            Some(Symbol::HorizontalLine)
        );
        assert_eq!(Symbol::from_code("Tick left"), Some(Symbol::TickLeft));
        assert_eq!(Symbol::from_code("Tick right"), Some(Symbol::TickRight));
        assert_eq!(Symbol::from_code("Tick up"), Some(Symbol::TickUp));
        assert_eq!(Symbol::from_code("Tick down"), Some(Symbol::TickDown));
        assert_eq!(Symbol::from_code("Caret left"), Some(Symbol::CaretLeft));
        assert_eq!(Symbol::from_code("Caret right"), Some(Symbol::CaretRight));
        assert_eq!(Symbol::from_code("Caret up"), Some(Symbol::CaretUp));
        assert_eq!(Symbol::from_code("Caret down"), Some(Symbol::CaretDown));
    }

    #[test]
    fn triangle_has_a_name_but_no_silx_code() {
        // egui extra: reachable by name, but silx has no code for it.
        assert_eq!(Symbol::from_code("triangle"), Some(Symbol::Triangle));
        assert_eq!(Symbol::from_code("Triangle"), Some(Symbol::Triangle));
        assert_eq!(Symbol::Triangle.code_str(), None);
    }

    #[test]
    fn from_code_rejects_unsupported_codes() {
        // silx None symbol (empty string), the Heart glyph, and any garbage.
        assert_eq!(Symbol::from_code(""), None);
        assert_eq!(Symbol::from_code("\u{2665}"), None);
        assert_eq!(Symbol::from_code("heart"), None);
        assert_eq!(Symbol::from_code("nope"), None);
    }

    #[test]
    fn render_size_px_overrides_per_symbol() {
        // Pixel is always a single pixel regardless of the requested size.
        assert_eq!(Symbol::Pixel.render_size_px(7.0), 1.0);
        assert_eq!(Symbol::Pixel.render_size_px(20.0), 1.0);

        // Point shrinks to ceil(0.5 * size) + 1: 7 -> ceil(3.5)+1 = 5;
        // 8 -> ceil(4)+1 = 5 (the .5 boundary rounds up).
        assert_eq!(Symbol::Point.render_size_px(7.0), 5.0);
        assert_eq!(Symbol::Point.render_size_px(8.0), 5.0);

        // The 1px strokes round to the nearest odd pixel: an odd size is kept,
        // an even size becomes the next odd one up.
        for s in [
            Symbol::Plus,
            Symbol::VerticalLine,
            Symbol::HorizontalLine,
            Symbol::TickLeft,
            Symbol::TickRight,
            Symbol::TickUp,
            Symbol::TickDown,
        ] {
            assert_eq!(s.render_size_px(7.0), 7.0, "odd stays odd");
            assert_eq!(s.render_size_px(8.0), 9.0, "even rounds to next odd");
        }

        // Every other symbol keeps the requested size unchanged.
        for s in [
            Symbol::Circle,
            Symbol::Square,
            Symbol::Cross,
            Symbol::Triangle,
            Symbol::Diamond,
            Symbol::CaretLeft,
            Symbol::CaretRight,
            Symbol::CaretUp,
            Symbol::CaretDown,
        ] {
            assert_eq!(s.render_size_px(7.0), 7.0);
            assert_eq!(s.render_size_px(8.0), 8.0);
        }
    }

    #[test]
    fn painter_dashes_custom_splits_on_off_and_keeps_offset() {
        let style = LineStyle::Custom {
            offset: 2.0,
            pattern: vec![3.0, 1.0, 2.0, 4.0],
        };
        assert_eq!(
            style.painter_dashes(1.0),
            Some((vec![3.0, 2.0], vec![1.0, 4.0], 2.0))
        );
        // A dash with no gap is solid (no usable period).
        let no_gap = LineStyle::Custom {
            offset: 0.0,
            pattern: vec![3.0],
        };
        assert_eq!(no_gap.painter_dashes(1.0), None);
        // An empty pattern is solid.
        let empty = LineStyle::Custom {
            offset: 0.0,
            pattern: vec![],
        };
        assert_eq!(empty.painter_dashes(1.0), None);
    }
}
