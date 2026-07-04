//! Cursor position readout bar ported from silx
//! `gui/plot/tools/PositionInfo.py`.
//!
//! [`PositionInfo`] is a horizontal label bar showing the current cursor data
//! coordinates through a list of named converter functions
//! `(name, fn(x, y) -> String)`. It mirrors silx `PositionInfo`
//! (PositionInfo.py:64-318):
//!
//! - The default converters display `X` and `Y` (PositionInfo.py:117).
//! - A polar converter pair (`Radius`, `Angle`) is provided as the silx
//!   documentation example (PositionInfo.py:92-94).
//! - When no cursor position is available the value fields show `"------"`
//!   (PositionInfo.py:131).
//! - Numeric converter results are formatted with `%.7g` (silx
//!   `valueToString`, PositionInfo.py:315) via [`format_value`].
//!
//! The pure snapping kernels (PositionInfo.py:236-292) live here:
//! [`snap_to_nearest`] / [`SNAP_THRESHOLD_DIST`] rank projected candidates by
//! pixel distance, while the silx *engage* gates — each item's `pick()` — are
//! [`pick_polyline_indices`] (the GLPlotCurve2D `±`[`PICK_OFFSET`] box pick,
//! GLPlotCurve.py:1396-1494) and [`pick_filled_histogram`] (the filled-bar
//! area pick, items/histogram.py:245-291). Candidate *selection*
//! (`SNAPPING_CURVE`/`SNAPPING_SCATTER`/`SNAPPING_ACTIVE_ONLY`/…) is
//! [`snapping_candidates`]. All are wired against a live plot by
//! [`PlotWidget::snap_cursor`](crate::widget::high_level::PlotWidget::snap_cursor),
//! which builds the [`SnapItem`] list from the plot's items, pick-gates each
//! candidate item, projects the picked vertices through the cached display
//! transform, and returns the [`Snap`]; the host then feeds `snap.data` to
//! [`PositionInfo::ui_snapped`], which reddens the labels in the no-snap state
//! (silx :200).

/// A converter mapping cursor data coordinates `(x, y)` to a display string.
///
/// This is the boxed `Fn(f64, f64) -> String` half of the silx
/// `(name, function)` converter pair (PositionInfo.py:127); the convenience
/// constructors wrap numeric silx converters with [`format_value`] (`%.7g`).
pub type Converter = Box<dyn Fn(f64, f64) -> String>;

/// A horizontal cursor-coordinate readout bar.
///
/// Holds an ordered list of `(label, converter)` pairs. Each converter maps
/// the cursor data coordinates `(x, y)` to a display string; numeric silx
/// converters are wrapped with [`format_value`] (`%.7g`) by the convenience
/// constructors.
pub struct PositionInfo {
    converters: Vec<(String, Converter)>,
}

impl Default for PositionInfo {
    /// The silx default: `X` and `Y` columns (PositionInfo.py:117).
    fn default() -> Self {
        Self::with_xy()
    }
}

impl PositionInfo {
    /// Create a readout bar from an explicit list of converters.
    pub fn new(converters: Vec<(String, Converter)>) -> Self {
        Self { converters }
    }

    /// The silx default converters: `X -> x`, `Y -> y` (PositionInfo.py:117),
    /// each formatted with `%.7g`.
    pub fn with_xy() -> Self {
        Self::new(vec![
            ("X".to_owned(), Box::new(|x: f64, _y: f64| format_value(x))),
            ("Y".to_owned(), Box::new(|_x: f64, y: f64| format_value(y))),
        ])
    }

    /// The silx documentation polar example (PositionInfo.py:92-94):
    /// `Radius -> sqrt(x*x + y*y)`, `Angle -> degrees(atan2(y, x))`, each
    /// formatted with `%.7g`.
    pub fn polar() -> Self {
        Self::new(vec![
            (
                "Radius".to_owned(),
                Box::new(|x: f64, y: f64| format_value(x.hypot(y))),
            ),
            (
                "Angle".to_owned(),
                Box::new(|x: f64, y: f64| format_value(y.atan2(x).to_degrees())),
            ),
        ])
    }

    /// Append a converter `(label, fn)` to the bar.
    pub fn push(&mut self, label: impl Into<String>, converter: Converter) {
        self.converters.push((label.into(), converter));
    }

    /// Convenience: append a numeric converter, formatting its result with
    /// `%.7g` (silx `valueToString`, PositionInfo.py:315).
    pub fn push_numeric(
        &mut self,
        label: impl Into<String>,
        converter: impl Fn(f64, f64) -> f64 + 'static,
    ) {
        self.converters.push((
            label.into(),
            Box::new(move |x, y| format_value(converter(x, y))),
        ));
    }

    /// The number of converter columns.
    pub fn len(&self) -> usize {
        self.converters.len()
    }

    /// Whether the bar has no converters.
    pub fn is_empty(&self) -> bool {
        self.converters.is_empty()
    }

    /// Compute the display string for each converter at `(x, y)`.
    ///
    /// Returns one string per column. This is the pure core of [`Self::ui`];
    /// `None` cursor yields the silx empty placeholder `"------"`
    /// (PositionInfo.py:131) for every column.
    pub fn values(&self, cursor: Option<[f64; 2]>) -> Vec<String> {
        match cursor {
            None => vec![EMPTY_PLACEHOLDER.to_owned(); self.converters.len()],
            Some([x, y]) => self
                .converters
                .iter()
                .map(|(_label, func)| func(x, y))
                .collect(),
        }
    }

    /// Render the readout bar: a horizontal row of `<label>: <value>` pairs.
    ///
    /// `cursor` is the current cursor position in data coordinates, or `None`
    /// when the cursor is outside the plot area (silx shows `"------"`).
    pub fn ui(&self, ui: &mut egui::Ui, cursor: Option<[f64; 2]>) {
        ui.horizontal(|ui| {
            let values = self.values(cursor);
            for ((label, _func), value) in self.converters.iter().zip(values) {
                ui.strong(format!("{label}:"));
                ui.label(value);
                ui.add_space(8.0);
            }
        });
    }

    /// Render the readout bar, reddening the value labels when `snapped` is
    /// `false` (silx PositionInfo's "not snapped" red style, PositionInfo.py:200;
    /// the normal style is restored when the cursor snaps to a point, :288).
    ///
    /// Use with [`PlotWidget::snap_cursor`](crate::widget::high_level::PlotWidget::snap_cursor)
    /// while a [`SnappingMode`] is engaged: pass the snapped data coordinate as
    /// `cursor` (or the raw cursor when nothing snapped) and `snapped =
    /// snap.is_some()`. When snapping is *disabled*, call the plain [`Self::ui`]
    /// instead — silx only reddens once snapping is engaged but unmatched.
    pub fn ui_snapped(&self, ui: &mut egui::Ui, cursor: Option<[f64; 2]>, snapped: bool) {
        ui.horizontal(|ui| {
            let values = self.values(cursor);
            for ((label, _func), value) in self.converters.iter().zip(values) {
                ui.strong(format!("{label}:"));
                if snapped {
                    ui.label(value);
                } else {
                    ui.colored_label(egui::Color32::RED, value);
                }
                ui.add_space(8.0);
            }
        });
    }
}

/// The silx empty-field placeholder shown when no cursor is available
/// (PositionInfo.py:131).
const EMPTY_PLACEHOLDER: &str = "------";

/// Format a numeric value like silx `valueToString` (`"%.7g"`,
/// PositionInfo.py:315): up to 7 significant digits, trailing zeros trimmed,
/// switching to exponential notation for very large/small magnitudes.
///
/// Non-finite values format as `"nan"` / `"inf"` like C `%g` would; we keep
/// Rust's spelling for those edge cases since silx delegates to Python's
/// `%g` which also prints `nan` / `inf`.
pub fn format_value(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value < 0.0 {
            "-inf".to_owned()
        } else {
            "inf".to_owned()
        };
    }
    crate::widget::stats_widget::format_significant(value, 7)
}

/// Snap radius in logical pixels (silx `PositionInfo.SNAP_THRESHOLD_DIST`,
/// PositionInfo.py:107).
///
/// silx scales this by the device-pixel ratio before squaring
/// (PositionInfo.py:237); a caller working in physical pixels should pass
/// `SNAP_THRESHOLD_DIST * device_pixel_ratio` as the threshold to
/// [`snap_to_nearest`].
pub const SNAP_THRESHOLD_DIST: f64 = 5.0;

/// Pick-box half-extent in logical pixels — silx `BackendOpenGL._PICK_OFFSET`
/// (BackendOpenGL.py:1267).
///
/// silx enlarges it per item to `max(offset, markerSize / 2, lineWidth / 2)`
/// before building the pick box (`__pickCurves`, BackendOpenGL.py:1290-1304);
/// unlike [`SNAP_THRESHOLD_DIST`] it is *not* scaled by the device-pixel
/// ratio.
pub const PICK_OFFSET: f64 = 3.0;

/// Outcodes of silx's Cohen–Sutherland pick clipping (GLPlotCurve.py:1427-1447).
const PICK_TOP: u8 = 1 << 3;
const PICK_BOTTOM: u8 = 1 << 2;
const PICK_RIGHT: u8 = 1 << 1;
const PICK_LEFT: u8 = 1 << 0;

/// Pick the vertices of a polyline against a data-space box, porting
/// `GLPlotCurve2D.pick` (GLPlotCurve.py:1396-1494).
///
/// The box is `[x_min, x_max] × [y_min, y_max]` in *data* coordinates — silx
/// converts the cursor's `±offset` pixel box corners through `pixelToData`
/// and tests the raw data arrays (`__pickCurves`, BackendOpenGL.py:1306-1330),
/// so log-axis quirks (segment crossings interpolated linearly in data space,
/// not along the rendered pixel-space segment) are ported as-is.
///
/// Returns the sorted indices of the picked vertices:
///
/// - every finite vertex inside the box (a NaN coordinate compares false on
///   every bound so its outcode is 0, but silx's `notNaN` mask drops it,
///   :1434-1439);
/// - with `has_line`, the *lower* endpoint of every segment that crosses the
///   box with neither endpoint inside (:1441-1481) — silx tests the crossing
///   Cohen–Sutherland-style against the bound flagged in the *second*
///   endpoint's outcode only, and a segment touching a NaN vertex is never
///   tested (the NaN's outcode 0 fails the both-outside precondition).
///
/// `has_line` is silx's `lineDashPattern is not None`: solid lines map to the
/// empty tuple `()` (BackendOpenGL `_DASH_PATTERNS`, :885-893), so every
/// line-styled curve takes the segment path while a marker-only curve
/// (linestyle `' '`/`''` → `None`) tests bare vertices (:1483-1493). A curve
/// with neither a line nor markers is unpickable in silx (:1409-1416) — the
/// caller skips it before reaching here.
pub fn pick_polyline_indices(
    xs: &[f64],
    ys: &[f64],
    has_line: bool,
    x_min: f64,
    x_max: f64,
    y_min: f64,
    y_max: f64,
) -> Vec<usize> {
    let n = xs.len().min(ys.len());
    // Outcode per vertex (GLPlotCurve.py:1427-1432); NaN compares false on
    // every bound → code 0, excluded from the inside set by the finite test.
    let codes: Vec<u8> = (0..n)
        .map(|i| {
            u8::from(ys[i] > y_max) << 3
                | u8::from(ys[i] < y_min) << 2
                | u8::from(xs[i] > x_max) << 1
                | u8::from(xs[i] < x_min)
        })
        .collect();

    let mut indices: Vec<usize> = (0..n)
        .filter(|&i| codes[i] == 0 && xs[i].is_finite() && ys[i].is_finite())
        .collect();

    if has_line {
        // Segments that might cross the box with no endpoint inside
        // (GLPlotCurve.py:1441-1481).
        for i in 0..n.saturating_sub(1) {
            if codes[i] == 0 || codes[i + 1] == 0 || codes[i] & codes[i + 1] != 0 {
                continue;
            }
            let (x0, y0, x1, y1) = (xs[i], ys[i], xs[i + 1], ys[i + 1]);
            let code1 = codes[i + 1];

            // Crossing with the horizontal bound flagged in code1 (y0 == y1
            // cannot happen: both endpoints in one vertical band would share
            // an outcode bit, silx :1455-1457).
            let x = if code1 & PICK_TOP != 0 {
                Some(x0 + (x1 - x0) * (y_max - y0) / (y1 - y0))
            } else if code1 & PICK_BOTTOM != 0 {
                Some(x0 + (x1 - x0) * (y_min - y0) / (y1 - y0))
            } else {
                None
            };
            if let Some(x) = x
                && (x_min..=x_max).contains(&x)
            {
                indices.push(i);
                continue;
            }

            // Otherwise the vertical bound flagged in code1 (silx :1468-1480).
            let y = if code1 & PICK_RIGHT != 0 {
                Some(y0 + (y1 - y0) * (x_max - x0) / (x1 - x0))
            } else if code1 & PICK_LEFT != 0 {
                Some(y0 + (y1 - y0) * (x_min - x0) / (x1 - x0))
            } else {
                None
            };
            if let Some(y) = y
                && (y_min..=y_max).contains(&y)
            {
                indices.push(i);
            }
        }
        indices.sort_unstable();
    }
    indices
}

/// Area-pick a filled histogram at a data-space cursor, porting
/// `Histogram.__pickFilledHistogram` (items/histogram.py:245-291): any cursor
/// between the baseline and a bar's value picks that bar's bin.
///
/// Returns the picked bin index, or `None` when the cursor is outside the
/// histogram's bounds or the bar's `[baseline, value]` band. Ported quirks:
///
/// - The bounds gate is *strict* (`xmin < x < xmax`, :258-260) against the
///   silx item bounds, whose y range always includes 0
///   (`min(0, nanmin(values))` / `max(0, nanmax(values))`, :236-243) even
///   when every count is positive.
/// - The bin is `searchsorted(edges, x, side="left") - 1` clipped to a valid
///   bin (:263-267), so a cursor exactly on an interior edge belongs to the
///   bin *left* of that edge.
/// - A downward bar (`value < baseline`) picks between `value` and
///   `baseline` (:275-281).
pub fn pick_filled_histogram(
    edges: &[f64],
    counts: &[f64],
    baseline: f64,
    x: f64,
    y: f64,
) -> Option<usize> {
    if counts.is_empty() || edges.len() != counts.len() + 1 {
        return None;
    }
    let nan_fold = |acc: (f64, f64), &v: &f64| {
        if v.is_nan() {
            acc
        } else {
            (acc.0.min(v), acc.1.max(v))
        }
    };
    let (x_min, x_max) = edges
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), nan_fold);
    let (v_min, v_max) = counts
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), nan_fold);
    let (y_min, y_max) = (v_min.min(0.0), v_max.max(0.0));
    if !(x_min < x && x < x_max && y_min < y && y < y_max) {
        return None; // Outside bounding box (histogram.py:258-260)
    }

    // numpy.searchsorted(edges, x, side="left") - 1, clipped (:263-267).
    let index = edges
        .partition_point(|&e| e < x)
        .saturating_sub(1)
        .min(counts.len() - 1);

    let value = counts[index];
    ((baseline <= value && baseline <= y && y <= value)
        || (value < baseline && value <= y && y <= baseline))
        .then_some(index)
}

/// A successful snap: the nearest candidate within the snap radius.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snap {
    /// Index of the snapped point: for [`snap_to_nearest`], into the candidate
    /// slice it was given; for
    /// [`PlotWidget::snap_cursor`](crate::widget::high_level::PlotWidget::snap_cursor),
    /// the vertex index within the snapped item's data arrays (a histogram's
    /// picked *bin* index, silx `result.getIndices()[0]`,
    /// PositionInfo.py:246-250).
    pub index: usize,
    /// The snapped data coordinate (the candidate's data position), to feed to
    /// [`PositionInfo::values`] in place of the raw cursor.
    pub data: [f64; 2],
}

/// Snap a cursor to the nearest candidate point, porting the inner loop of
/// silx `PositionInfo._updateStatusBar` (PositionInfo.py:236-292).
///
/// `cursor_px` and every `candidates[i].0` are positions in the same pixel
/// space; `candidates[i].1` is that point's data coordinate. Returns the
/// candidate with the smallest squared pixel distance to the cursor, provided
/// that distance is `<= threshold_px²` (silx `closestSqDistInPixels <=
/// sqDistInPixels`, :286). Candidates whose squared distance is not finite are
/// skipped (silx `numpy.isfinite` guard, :281).
///
/// silx walks items one at a time, shrinking the live threshold to the best
/// distance found so far (:292); a single pass that keeps the global nearest
/// within the original threshold is equivalent, since a point only wins when it
/// is both within `threshold_px²` and no farther than the current best. Ties
/// resolve to the later candidate, matching silx's `<=` update order.
///
/// Returns `None` when nothing is within the radius — silx then leaves the
/// readout at the raw cursor and styles the labels red (:200); a `Some` result
/// is the "snapped" state that clears the style (:288).
pub fn snap_to_nearest(
    cursor_px: [f64; 2],
    candidates: &[([f64; 2], [f64; 2])],
    threshold_px: f64,
) -> Option<Snap> {
    let mut best: Option<Snap> = None;
    let mut best_sq = threshold_px * threshold_px;
    for (index, &(px, data)) in candidates.iter().enumerate() {
        let dx = px[0] - cursor_px[0];
        let dy = px[1] - cursor_px[1];
        let sq = dx * dx + dy * dy;
        if !sq.is_finite() {
            continue;
        }
        if sq <= best_sq {
            best = Some(Snap { index, data });
            best_sq = sq;
        }
    }
    best
}

/// The snapping mode bitfield — silx `PositionInfo` `SNAPPING_*` flags
/// (PositionInfo.py:322-337), combinable with `|`.
///
/// A data-kind flag ([`Self::CURVE`] and/or [`Self::SCATTER`]) selects which
/// item kinds are snap candidates; the modifiers restrict that set:
/// [`Self::ACTIVE_ONLY`] to the active item, [`Self::SYMBOLS_ONLY`] to items
/// showing a symbol. [`Self::CROSSHAIR`] gates snapping on the crosshair being
/// active (a live-cursor condition handled by the caller, not by
/// [`snapping_candidates`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SnappingMode(u8);

impl SnappingMode {
    /// No snapping (silx `SNAPPING_DISABLED`).
    pub const DISABLED: Self = Self(0);
    /// Snap only while the crosshair cursor is active (silx `SNAPPING_CROSSHAIR`).
    pub const CROSSHAIR: Self = Self(1 << 0);
    /// Restrict candidates to the active curve/scatter (silx `SNAPPING_ACTIVE_ONLY`).
    pub const ACTIVE_ONLY: Self = Self(1 << 1);
    /// Restrict candidates to items showing a symbol (silx `SNAPPING_SYMBOLS_ONLY`).
    pub const SYMBOLS_ONLY: Self = Self(1 << 2);
    /// Snap to curves (and histograms) (silx `SNAPPING_CURVE`).
    pub const CURVE: Self = Self(1 << 3);
    /// Snap to scatters (silx `SNAPPING_SCATTER`).
    pub const SCATTER: Self = Self(1 << 4);

    /// Whether every flag in `flag` is set in `self`.
    #[must_use]
    pub fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }

    /// The raw bits (silx integer mode value).
    #[must_use]
    pub fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for SnappingMode {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// The kind of a plot item as seen by snapping selection — the silx `isinstance`
/// branches in `PositionInfo._updateStatusBar` (PositionInfo.py:217-246).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapItemKind {
    /// A curve (silx `items.Curve`).
    Curve,
    /// A histogram (silx `items.Histogram`) — snapped under [`SnappingMode::CURVE`].
    Histogram,
    /// A scatter (silx `items.Scatter`) — snapped under [`SnappingMode::SCATTER`].
    Scatter,
    /// Any other item, never a snap candidate.
    Other,
}

/// One plot item described for snapping candidate selection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapItem {
    /// The item's kind (silx `isinstance` test).
    pub kind: SnapItemKind,
    /// Whether the item is visible (silx `item.isVisible()`).
    pub visible: bool,
    /// Whether the item shows a symbol (silx `isinstance(item, SymbolMixIn) and
    /// item.getSymbol()` truthy).
    pub has_symbol: bool,
    /// Whether the item is the active curve/scatter (silx `getActiveCurve()` /
    /// `getActiveScatter()`).
    pub active: bool,
}

/// Select the snap-candidate items for `mode`, porting silx
/// `PositionInfo._updateStatusBar`'s item selection (PositionInfo.py:196-244).
///
/// Returns the indices into `items` that should be projected to pixels and fed
/// to [`snap_to_nearest`]. Empty when neither [`SnappingMode::CURVE`] nor
/// [`SnappingMode::SCATTER`] is set (silx engages snapping only then, :197).
///
/// Faithful asymmetry: in the all-items path silx's `CURVE` kind list is
/// `(Curve, Histogram)` (:217-219) and the candidates are filtered by
/// `isVisible()` (:225); the [`SnappingMode::ACTIVE_ONLY`] path instead takes
/// `getActiveCurve()` (a `Curve`, never a histogram) and `getActiveScatter()`
/// (:202-213) regardless of the visible flag. [`SnappingMode::SYMBOLS_ONLY`]
/// then drops items without a symbol on either path (:240-244).
///
/// The [`SnappingMode::CROSSHAIR`] gate (:198) is a live-cursor precondition the
/// caller applies; it does not affect which kinds are candidates.
#[must_use]
pub fn snapping_candidates(mode: SnappingMode, items: &[SnapItem]) -> Vec<usize> {
    let want_curve = mode.contains(SnappingMode::CURVE);
    let want_scatter = mode.contains(SnappingMode::SCATTER);
    if !want_curve && !want_scatter {
        return Vec::new();
    }
    let active_only = mode.contains(SnappingMode::ACTIVE_ONLY);
    let symbols_only = mode.contains(SnappingMode::SYMBOLS_ONLY);

    items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            let kind_match = match item.kind {
                // The active-only path uses getActiveCurve (a Curve), so a
                // histogram is a CURVE candidate only in the all-items path.
                SnapItemKind::Curve => want_curve,
                SnapItemKind::Histogram => want_curve && !active_only,
                SnapItemKind::Scatter => want_scatter,
                SnapItemKind::Other => false,
            };
            if !kind_match {
                return false;
            }
            if active_only {
                if !item.active {
                    return false;
                }
            } else if !item.visible {
                // All-items path filters by visibility (silx :225); the
                // active-only path does not.
                return false;
            }
            if symbols_only && !item.has_symbol {
                return false;
            }
            true
        })
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_xy() {
        let p = PositionInfo::default();
        assert_eq!(p.len(), 2);
        let v = p.values(Some([3.0, 4.0]));
        assert_eq!(v, vec!["3".to_owned(), "4".to_owned()]);
    }

    #[test]
    fn no_cursor_shows_placeholder() {
        let p = PositionInfo::default();
        let v = p.values(None);
        assert_eq!(v, vec!["------".to_owned(), "------".to_owned()]);
    }

    #[test]
    fn polar_radius_is_hypot() {
        let p = PositionInfo::polar();
        // (3,4) -> radius 5, angle atan2(4,3) deg ~= 53.13010...
        let v = p.values(Some([3.0, 4.0]));
        assert_eq!(v[0], "5");
        // %.7g of 53.13010235... -> "53.1301" (trailing zero trimmed by %g).
        assert_eq!(v[1], "53.1301");
    }

    #[test]
    fn polar_angle_on_axes() {
        let p = PositionInfo::polar();
        // (1, 0) -> radius 1, angle 0.
        let v = p.values(Some([1.0, 0.0]));
        assert_eq!(v[0], "1");
        assert_eq!(v[1], "0");
        // (0, 1) -> angle 90.
        let v = p.values(Some([0.0, 1.0]));
        assert_eq!(v[1], "90");
    }

    #[test]
    fn custom_numeric_converter() {
        let mut p = PositionInfo::new(vec![]);
        p.push_numeric("Sum", |x, y| x + y);
        assert!(!p.is_empty());
        let v = p.values(Some([2.5, 1.5]));
        assert_eq!(v, vec!["4".to_owned()]);
    }

    #[test]
    fn custom_string_converter() {
        let p = PositionInfo::new(vec![(
            "Quad".to_owned(),
            Box::new(|x: f64, y: f64| {
                if x >= 0.0 && y >= 0.0 {
                    "I".to_owned()
                } else {
                    "other".to_owned()
                }
            }),
        )]);
        assert_eq!(p.values(Some([1.0, 1.0])), vec!["I".to_owned()]);
        assert_eq!(p.values(Some([-1.0, 1.0])), vec!["other".to_owned()]);
    }

    #[test]
    fn format_value_seven_sig_figs() {
        // %.7g of 1/3.
        assert_eq!(format_value(1.0 / 3.0), "0.3333333");
        assert_eq!(format_value(1234567.0), "1234567");
        // 8 sig figs collapses to exponential under %g (exp >= 7).
        assert_eq!(format_value(12345678.0), "1.234568e+07");
    }

    #[test]
    fn format_value_non_finite() {
        assert_eq!(format_value(f64::NAN), "nan");
        assert_eq!(format_value(f64::INFINITY), "inf");
        assert_eq!(format_value(f64::NEG_INFINITY), "-inf");
    }

    #[test]
    fn snap_picks_nearest_within_radius() {
        // Two candidates; cursor closest to the second. Both within 5px.
        let cursor = [10.0, 10.0];
        let candidates = [
            ([13.0, 10.0], [1.0, 2.0]), // 3px away
            ([10.0, 12.0], [3.0, 4.0]), // 2px away (closest)
        ];
        let snap = snap_to_nearest(cursor, &candidates, SNAP_THRESHOLD_DIST).unwrap();
        assert_eq!(snap.index, 1);
        assert_eq!(snap.data, [3.0, 4.0]);
    }

    #[test]
    fn snap_returns_none_when_all_outside_radius() {
        // Nearest is 6px away, threshold 5px -> no snap (silx red label state).
        let cursor = [0.0, 0.0];
        let candidates = [([6.0, 0.0], [1.0, 1.0])];
        assert_eq!(
            snap_to_nearest(cursor, &candidates, SNAP_THRESHOLD_DIST),
            None
        );
    }

    #[test]
    fn snap_includes_point_exactly_on_radius() {
        // silx uses `<=` (closestSqDistInPixels <= sqDistInPixels): a point at
        // exactly the threshold distance snaps.
        let cursor = [0.0, 0.0];
        let candidates = [([5.0, 0.0], [7.0, 8.0])];
        let snap = snap_to_nearest(cursor, &candidates, SNAP_THRESHOLD_DIST).unwrap();
        assert_eq!(snap.data, [7.0, 8.0]);
    }

    #[test]
    fn snap_skips_non_finite_candidates() {
        // A NaN pixel position is skipped (silx isfinite guard); the finite
        // in-range point still wins.
        let cursor = [0.0, 0.0];
        let candidates = [([f64::NAN, 0.0], [9.0, 9.0]), ([2.0, 0.0], [1.0, 1.0])];
        let snap = snap_to_nearest(cursor, &candidates, SNAP_THRESHOLD_DIST).unwrap();
        assert_eq!(snap.index, 1);
        assert_eq!(snap.data, [1.0, 1.0]);
    }

    #[test]
    fn snap_empty_candidates_is_none() {
        assert_eq!(snap_to_nearest([0.0, 0.0], &[], SNAP_THRESHOLD_DIST), None);
    }

    #[test]
    fn snap_tie_resolves_to_later_candidate() {
        // Equal distances: silx's `<=` update keeps the later item's point.
        let cursor = [0.0, 0.0];
        let candidates = [([3.0, 0.0], [1.0, 1.0]), ([0.0, 3.0], [2.0, 2.0])];
        let snap = snap_to_nearest(cursor, &candidates, SNAP_THRESHOLD_DIST).unwrap();
        assert_eq!(snap.index, 1);
        assert_eq!(snap.data, [2.0, 2.0]);
    }

    fn item(kind: SnapItemKind, visible: bool, has_symbol: bool, active: bool) -> SnapItem {
        SnapItem {
            kind,
            visible,
            has_symbol,
            active,
        }
    }

    #[test]
    fn snapping_disabled_without_a_kind_flag() {
        // No CURVE / SCATTER bit -> silx never engages snapping (:197), even
        // with the modifier bits set.
        let items = [item(SnapItemKind::Curve, true, true, true)];
        assert!(snapping_candidates(SnappingMode::DISABLED, &items).is_empty());
        assert!(
            snapping_candidates(
                SnappingMode::ACTIVE_ONLY | SnappingMode::SYMBOLS_ONLY,
                &items
            )
            .is_empty()
        );
    }

    #[test]
    fn snapping_curve_all_items_takes_visible_curves_and_histograms() {
        // CURVE all-items path: visible Curve + Histogram, skipping Scatter,
        // invisible items, and Other (silx kinds = (Curve, Histogram), :217-225).
        let items = [
            item(SnapItemKind::Curve, true, false, false),
            item(SnapItemKind::Histogram, true, false, false),
            item(SnapItemKind::Curve, false, false, false), // invisible
            item(SnapItemKind::Scatter, true, false, false), // wrong kind
            item(SnapItemKind::Other, true, false, false),
        ];
        assert_eq!(snapping_candidates(SnappingMode::CURVE, &items), vec![0, 1]);
    }

    #[test]
    fn snapping_scatter_all_items_takes_visible_scatters_only() {
        let items = [
            item(SnapItemKind::Scatter, true, false, false),
            item(SnapItemKind::Curve, true, false, false),
            item(SnapItemKind::Scatter, false, false, false), // invisible
        ];
        assert_eq!(snapping_candidates(SnappingMode::SCATTER, &items), vec![0]);
        // CURVE|SCATTER takes both kinds.
        assert_eq!(
            snapping_candidates(SnappingMode::CURVE | SnappingMode::SCATTER, &items),
            vec![0, 1]
        );
    }

    #[test]
    fn snapping_active_only_ignores_visibility_and_excludes_histograms() {
        // Active-only uses getActiveCurve/getActiveScatter: the active item is
        // taken even when invisible, but a histogram is NOT an "active curve".
        let items = [
            item(SnapItemKind::Curve, false, false, true), // active, invisible -> kept
            item(SnapItemKind::Curve, true, false, false), // not active -> skipped
            item(SnapItemKind::Histogram, true, false, true), // active histogram -> excluded
            item(SnapItemKind::Scatter, false, false, true), // active scatter -> kept
        ];
        assert_eq!(
            snapping_candidates(
                SnappingMode::CURVE | SnappingMode::SCATTER | SnappingMode::ACTIVE_ONLY,
                &items
            ),
            vec![0, 3]
        );
    }

    #[test]
    fn snapping_symbols_only_drops_items_without_a_symbol() {
        // SYMBOLS_ONLY filters on both paths (silx :240-244).
        let items = [
            item(SnapItemKind::Curve, true, true, false), // has symbol -> kept
            item(SnapItemKind::Curve, true, false, false), // no symbol -> dropped
        ];
        assert_eq!(
            snapping_candidates(SnappingMode::CURVE | SnappingMode::SYMBOLS_ONLY, &items),
            vec![0]
        );
    }

    #[test]
    fn snapping_mode_bits_and_contains() {
        let mode = SnappingMode::CURVE | SnappingMode::ACTIVE_ONLY;
        assert!(mode.contains(SnappingMode::CURVE));
        assert!(mode.contains(SnappingMode::ACTIVE_ONLY));
        assert!(!mode.contains(SnappingMode::SCATTER));
        // silx flag values: CURVE=1<<3, ACTIVE_ONLY=1<<1.
        assert_eq!(mode.bits(), (1 << 3) | (1 << 1));
    }

    // --- pick_polyline_indices (GLPlotCurve2D.pick port) ---

    #[test]
    fn pick_takes_vertices_inside_the_box() {
        // Marker path (:1483-1493): bare vertex-in-box test, no segments.
        let xs = [0.0, 5.0, 10.0];
        let ys = [0.0, 5.0, 10.0];
        assert_eq!(
            pick_polyline_indices(&xs, &ys, false, 4.0, 6.0, 4.0, 6.0),
            vec![1]
        );
        // Off-box cursor picks nothing even though a segment crosses the box.
        assert_eq!(
            pick_polyline_indices(&xs, &ys, false, 2.0, 3.0, 2.0, 3.0),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn pick_line_adds_the_lower_endpoint_of_a_crossing_segment() {
        // Segment (0,0)-(10,10) crosses the box [2,3]×[2,3] with neither
        // endpoint inside: the LOWER index is picked (GLPlotCurve.py:1441-1481).
        let xs = [0.0, 10.0];
        let ys = [0.0, 10.0];
        assert_eq!(
            pick_polyline_indices(&xs, &ys, true, 2.0, 3.0, 2.0, 3.0),
            vec![0]
        );
        // Same geometry without a line: nothing picked.
        assert_eq!(
            pick_polyline_indices(&xs, &ys, false, 2.0, 3.0, 2.0, 3.0),
            Vec::<usize>::new()
        );
        // A near-horizontal segment crossing left→right engages the
        // vertical-bound branch (code1 & RIGHT, GLPlotCurve.py:1468-1480).
        assert_eq!(
            pick_polyline_indices(&[0.0, 10.0], &[2.5, 2.6], true, 2.0, 3.0, 2.0, 3.0),
            vec![0]
        );
    }

    #[test]
    fn pick_line_skips_a_corner_missing_segment() {
        // Both endpoints outside with disjoint single-bound outcodes (LEFT-only
        // vs TOP-only) but the segment passes OUTSIDE the box: silx's
        // bound-crossing test rejects it (x at y_max falls left of the box,
        // and code1 sets no vertical flag).
        let xs = [-10.0, 1.2];
        let ys = [1.5, 20.0];
        assert_eq!(
            pick_polyline_indices(&xs, &ys, true, 1.0, 3.0, 1.0, 3.0),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn pick_excludes_nan_vertices_and_their_segments() {
        // A NaN coordinate outcodes to 0 ("inside") but the finite mask drops
        // it (silx notNaN, :1434-1439), and a segment touching a NaN vertex is
        // never crossing-tested (outcode 0 fails the both-outside gate).
        let xs = [0.0, f64::NAN, 10.0];
        let ys = [0.0, f64::NAN, 10.0];
        assert_eq!(
            pick_polyline_indices(&xs, &ys, true, 2.0, 3.0, 2.0, 3.0),
            Vec::<usize>::new()
        );
        // The finite vertex inside the box still picks.
        assert_eq!(
            pick_polyline_indices(&xs, &ys, true, -1.0, 1.0, -1.0, 1.0),
            vec![0]
        );
    }

    // --- pick_filled_histogram (Histogram.__pickFilledHistogram port) ---

    #[test]
    fn filled_histogram_picks_anywhere_inside_the_bar() {
        // Cursor in the MIDDLE of the tall bar [4,6)×[0,9] — far from the
        // apex — still picks bin 2 (histogram.py:262-291).
        let edges = [0.0, 2.0, 4.0, 6.0, 8.0];
        let counts = [1.0, 3.0, 9.0, 2.0];
        assert_eq!(
            pick_filled_histogram(&edges, &counts, 0.0, 5.0, 4.5),
            Some(2)
        );
        // Above the bar's value: outside the [baseline, value] band.
        assert_eq!(pick_filled_histogram(&edges, &counts, 0.0, 7.0, 4.5), None);
    }

    #[test]
    fn filled_histogram_bounds_are_strict_and_include_zero() {
        let edges = [0.0, 2.0, 4.0];
        let counts = [3.0, 5.0];
        // Exactly on the y bounds max (5.0) or the x edges: strict `<` gates
        // (histogram.py:258-260).
        assert_eq!(pick_filled_histogram(&edges, &counts, 0.0, 1.0, 5.0), None);
        assert_eq!(pick_filled_histogram(&edges, &counts, 0.0, 0.0, 1.0), None);
        assert_eq!(pick_filled_histogram(&edges, &counts, 0.0, 4.0, 1.0), None);
    }

    #[test]
    fn filled_histogram_interior_edge_belongs_to_the_left_bin() {
        // searchsorted(side="left") - 1: x exactly on an interior edge maps to
        // the bin LEFT of it (histogram.py:263-267).
        let edges = [0.0, 2.0, 4.0];
        let counts = [3.0, 5.0];
        assert_eq!(
            pick_filled_histogram(&edges, &counts, 0.0, 2.0, 1.0),
            Some(0)
        );
    }

    #[test]
    fn filled_histogram_downward_bar_picks_below_the_baseline() {
        // value < baseline: the band is [value, baseline] (histogram.py:275-281),
        // and the silx bounds include 0 via min(0, nanmin(values)).
        let edges = [0.0, 2.0, 4.0];
        let counts = [-4.0, 3.0];
        assert_eq!(
            pick_filled_histogram(&edges, &counts, 0.0, 1.0, -2.0),
            Some(0)
        );
        assert_eq!(pick_filled_histogram(&edges, &counts, 0.0, 1.0, 1.0), None);
    }
}
