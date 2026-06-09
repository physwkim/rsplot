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
//! The pure snapping kernel (PositionInfo.py:236-292) is provided by
//! [`snap_to_nearest`] / [`SNAP_THRESHOLD_DIST`]: given the cursor and
//! candidate data points already projected to pixel space, it returns the
//! nearest point within the snap radius. Selecting *which* items become
//! candidates (silx `SNAPPING_CURVE`/`SNAPPING_SCATTER`/`SNAPPING_ACTIVE_ONLY`
//! and the data→pixel projection) needs live plot/GPU state and stays the
//! caller's responsibility; the live `PlotWidget` event wiring
//! (`sigPlotSignal`) is likewise out of scope.

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

/// A successful snap: the nearest candidate within the snap radius.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snap {
    /// Index of the snapped point in the candidate slice passed to
    /// [`snap_to_nearest`].
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
}
