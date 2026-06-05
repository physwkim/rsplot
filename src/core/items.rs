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

    /// Human-readable name for this symbol, matching the values of silx
    /// `SymbolMixIn._SUPPORTED_SYMBOLS` (silx `getSymbolName`).
    /// [`Symbol::Triangle`] (an egui extra silx lacks) is named `"Triangle"`.
    /// The returned name round-trips through [`Symbol::from_code`].
    pub fn name(self) -> &'static str {
        match self {
            Symbol::Circle => "Circle",
            Symbol::Diamond => "Diamond",
            Symbol::Square => "Square",
            Symbol::Plus => "Plus",
            Symbol::Cross => "Cross",
            Symbol::Point => "Point",
            Symbol::Pixel => "Pixel",
            Symbol::VerticalLine => "Vertical line",
            Symbol::HorizontalLine => "Horizontal line",
            Symbol::TickLeft => "Tick left",
            Symbol::TickRight => "Tick right",
            Symbol::TickUp => "Tick up",
            Symbol::TickDown => "Tick down",
            Symbol::CaretLeft => "Caret left",
            Symbol::CaretRight => "Caret right",
            Symbol::CaretUp => "Caret up",
            Symbol::CaretDown => "Caret down",
            Symbol::Triangle => "Triangle",
        }
    }

    /// Every supported symbol, ordered to match silx `_SUPPORTED_SYMBOLS`
    /// (silx `getSupportedSymbols`) with [`Symbol::Triangle`] — an egui extra
    /// silx lacks — appended last. silx's empty "None" symbol and the `'♥'`
    /// Heart glyph are not representable here and so are absent (see
    /// [`Symbol::from_code`]). Used to build the silx `SymbolToolButton` menu.
    pub const ALL: [Symbol; 18] = [
        Symbol::Circle,
        Symbol::Diamond,
        Symbol::Square,
        Symbol::Plus,
        Symbol::Cross,
        Symbol::Point,
        Symbol::Pixel,
        Symbol::VerticalLine,
        Symbol::HorizontalLine,
        Symbol::TickLeft,
        Symbol::TickRight,
        Symbol::TickUp,
        Symbol::TickDown,
        Symbol::CaretLeft,
        Symbol::CaretRight,
        Symbol::CaretUp,
        Symbol::CaretDown,
        Symbol::Triangle,
    ];
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

/// A per-pixel validity mask for a scalar image, applied at the CPU data-prep
/// stage (silx `ImageDataBase` `getMaskData` / `setMaskData` / `getValueData`,
/// `image.py:209-284`).
///
/// silx stores a 2D mask alongside the scalar data; [`getValueData`] returns the
/// data with masked pixels set to NaN (`data[mask != 0] = numpy.nan`). The egui
/// port has no place to hang state on the render-layer `ImageData`, so masking is
/// modeled as this standalone, GPU-free value type: build it with a mask, then
/// call [`ScalarMask::apply`] to get the masked scalar field to hand to
/// `ImageData::new`. Masked pixels become `f32::NAN`, which the existing scalar
/// pipeline renders via its `nan_color` — no shader edit needed.
///
/// [`getValueData`]: ScalarMask::apply
/// [`getMaskData`]: ScalarMask::get_mask_data
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScalarMask {
    /// Image width in pixels (number of columns); rows are `width` wide.
    width: usize,
    /// Image height in pixels (number of rows).
    height: usize,
    /// Row-major per-pixel mask, length `width * height`. A pixel is masked
    /// (invalid) where the entry is non-zero, matching silx's `mask != 0`.
    mask: Vec<u8>,
}

impl ScalarMask {
    /// An all-valid (empty) mask for a `width * height` scalar image: every pixel
    /// is unmasked. Mirrors silx's `_mask is None` initial state via an explicit
    /// zero mask, so [`ScalarMask::apply`] is a no-op until [`set_mask_data`] is
    /// called.
    ///
    /// [`set_mask_data`]: ScalarMask::set_mask_data
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            mask: vec![0; width.saturating_mul(height)],
        }
    }

    /// Image width (columns).
    pub fn width(&self) -> usize {
        self.width
    }

    /// Image height (rows).
    pub fn height(&self) -> usize {
        self.height
    }

    /// Set the validity mask (silx `setMaskData`). A pixel is masked where its
    /// entry is non-zero. If `mask` does not match the image shape it is clipped
    /// or zero-extended to `width * height` by copying the overlapping top-left
    /// region, mirroring silx's lazy clip/extend in `getMaskData`
    /// (`image.py:215-226`): the new mask is `width` columns wide, and only the
    /// rows/columns present in the supplied mask are copied.
    ///
    /// `src_width` is the column count of the supplied `mask` (its row stride);
    /// its row count is inferred as `mask.len() / src_width` (the trailing partial
    /// row, if any, is ignored). A `src_width` of zero is treated as a single row.
    pub fn set_mask_data(&mut self, mask: &[u8], src_width: usize) {
        if src_width == self.width && mask.len() == self.width * self.height {
            // Exact shape match: take it verbatim (silx `mask.shape == shape`).
            self.mask.clear();
            self.mask.extend_from_slice(mask);
            return;
        }

        // Clip/extend: build a zero mask of the image shape and copy the
        // overlapping top-left rectangle (silx
        // `newMask[:m_h, :m_w] = mask[:h, :w]`).
        let src_width = src_width.max(1);
        let src_height = mask.len() / src_width;
        let copy_w = src_width.min(self.width);
        let copy_h = src_height.min(self.height);

        let mut new_mask = vec![0u8; self.width * self.height];
        for row in 0..copy_h {
            let dst = row * self.width;
            let src = row * src_width;
            new_mask[dst..dst + copy_w].copy_from_slice(&mask[src..src + copy_w]);
        }
        self.mask = new_mask;
    }

    /// The current row-major mask (silx `getMaskData`), length `width * height`.
    /// A pixel is masked where its entry is non-zero.
    pub fn get_mask_data(&self) -> &[u8] {
        &self.mask
    }

    /// Whether pixel `(col, row)` is masked (non-zero in the mask). Out-of-bounds
    /// indices are reported as unmasked.
    pub fn is_masked(&self, col: usize, row: usize) -> bool {
        if col >= self.width || row >= self.height {
            return false;
        }
        self.mask[row * self.width + col] != 0
    }

    /// Apply the mask to a scalar field (silx `getValueData`): every masked pixel
    /// becomes `f32::NAN`, every unmasked value is passed through unchanged. The
    /// result is the row-major field to hand to `ImageData::new`, where the scalar
    /// pipeline's `nan_color` renders the masked pixels.
    ///
    /// `data` must have one value per pixel (`width * height`); a length mismatch
    /// panics, matching the construction-time contract of `ImageData::new`.
    pub fn apply(&self, data: &[f32]) -> Vec<f32> {
        assert_eq!(
            data.len(),
            self.width * self.height,
            "data length must equal width * height"
        );
        data.iter()
            .zip(&self.mask)
            .map(|(&v, &m)| if m != 0 { f32::NAN } else { v })
            .collect()
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
    fn all_catalog_covers_every_variant_with_unique_round_tripping_names() {
        // `ALL` lists every variant exactly once (17 silx symbols + Triangle).
        assert_eq!(Symbol::ALL.len(), 18);
        let mut names: Vec<&str> = Symbol::ALL.iter().map(|s| s.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 18, "symbol names must be unique");
        // Every catalog name parses back to the same symbol (silx getSymbolName /
        // setSymbol round-trip), so the tool-button labels are valid codes.
        for symbol in Symbol::ALL {
            assert_eq!(
                Symbol::from_code(symbol.name()),
                Some(symbol),
                "{} name must round-trip",
                symbol.name()
            );
        }
        // silx order: the first five are o, d, s, +, x.
        assert_eq!(
            &Symbol::ALL[..5],
            &[
                Symbol::Circle,
                Symbol::Diamond,
                Symbol::Square,
                Symbol::Plus,
                Symbol::Cross
            ]
        );
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
    fn scalar_mask_new_is_all_valid() {
        // An untouched mask leaves every value unchanged (silx `_mask is None`).
        let m = ScalarMask::new(2, 2);
        let data = vec![1.0, 2.0, 3.0, 4.0];
        assert_eq!(m.apply(&data), data);
        assert!(m.get_mask_data().iter().all(|&v| v == 0));
    }

    #[test]
    fn scalar_mask_sets_masked_pixels_to_nan_and_keeps_others() {
        // 2x2 image; mask the (col 1, row 0) and (col 0, row 1) pixels.
        let mut m = ScalarMask::new(2, 2);
        m.set_mask_data(&[0, 1, 1, 0], 2);
        let out = m.apply(&[10.0, 20.0, 30.0, 40.0]);
        assert_eq!(out[0], 10.0); // unmasked
        assert!(out[1].is_nan()); // masked
        assert!(out[2].is_nan()); // masked
        assert_eq!(out[3], 40.0); // unmasked
        assert!(m.is_masked(1, 0));
        assert!(m.is_masked(0, 1));
        assert!(!m.is_masked(0, 0));
    }

    #[test]
    fn scalar_mask_any_nonzero_value_masks() {
        // silx masks where `mask != 0`, not only where mask == 1.
        let mut m = ScalarMask::new(3, 1);
        m.set_mask_data(&[0, 7, 255], 3);
        let out = m.apply(&[1.0, 2.0, 3.0]);
        assert_eq!(out[0], 1.0);
        assert!(out[1].is_nan());
        assert!(out[2].is_nan());
    }

    #[test]
    fn scalar_mask_clips_oversized_mask_to_image_shape() {
        // Supplied mask is 3x3 but the image is 2x2: copy the top-left 2x2 block
        // (silx clip), so the row-2 / col-2 entries are dropped.
        let mut m = ScalarMask::new(2, 2);
        #[rustfmt::skip]
        let big = [
            1, 0, 9,
            0, 1, 9,
            9, 9, 9,
        ];
        m.set_mask_data(&big, 3);
        // Top-left 2x2 = [[1,0],[0,1]] row-major.
        assert_eq!(m.get_mask_data(), &[1, 0, 0, 1]);
        let out = m.apply(&[5.0, 6.0, 7.0, 8.0]);
        assert!(out[0].is_nan());
        assert_eq!(out[1], 6.0);
        assert_eq!(out[2], 7.0);
        assert!(out[3].is_nan());
    }

    #[test]
    fn scalar_mask_zero_extends_undersized_mask() {
        // Supplied mask is 1x1 but the image is 2x2: copy the single pixel into
        // the top-left, zero-fill the rest (silx extend).
        let mut m = ScalarMask::new(2, 2);
        m.set_mask_data(&[1], 1);
        assert_eq!(m.get_mask_data(), &[1, 0, 0, 0]);
    }

    #[test]
    fn scalar_mask_extends_when_only_height_differs() {
        // Same width (2) but only one source row for a 2x2 image: the second row
        // stays unmasked. This exercises the `src_width == self.width` clip path
        // where lengths still differ.
        let mut m = ScalarMask::new(2, 2);
        m.set_mask_data(&[1, 1], 2);
        assert_eq!(m.get_mask_data(), &[1, 1, 0, 0]);
    }

    #[test]
    #[should_panic(expected = "data length must equal width * height")]
    fn scalar_mask_apply_rejects_length_mismatch() {
        let m = ScalarMask::new(2, 2);
        m.apply(&[1.0, 2.0, 3.0]);
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
