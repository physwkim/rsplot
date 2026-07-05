//! Statistics table widget ported from silx `gui/plot/StatsWidget.py`.
//!
//! [`StatsWidget`] renders one row per tracked item and one column per
//! statistic (min / coords min / max / coords max / COM / mean / std),
//! mirroring silx `DEFAULT_STATS` (StatsWidget.py:1266-1276) exactly. The
//! sum/delta aggregates of [`crate::core::stats::Stats`] stay available on the
//! computed [`Stats`] rows but are not default table columns (silx's default
//! table has none).
//!
//! It carries an auto/manual update toggle (silx `setUpdateMode`,
//! StatsWidget.py:1258-1263) and a "use visible data range" toggle (silx
//! `setStatsOnVisibleData`, StatsWidget.py:1254) that selects between
//! [`StatScope::All`] and [`StatScope::OnLimits`].
//!
//! Numeric formatting uses [`format_stat`], a pure port of silx
//! `StatFormatter.format` (statshandler.py:77-84: `"{0:.3f}"`, `"--"` for
//! `None`). A pure significant-digits helper [`format_significant`] is also
//! provided for callers that prefer significant-figure rounding.

use crate::core::stats::{ComCoord, StatScope, Stats};

/// One tracked input row for the table.
///
/// Curve rows carry `(xs, ys)`; histogram rows carry the `N` raw counts at
/// their bin-anchor x positions; scatter rows carry `(xs, ys, values)`; image
/// rows carry scalar pixel data plus the origin/scale geometry needed to map
/// COM and argmin/argmax indices back to data coordinates (silx maps through
/// the axes, stats.py:819-838).
pub enum StatsInput<'a> {
    /// A curve `(xs, ys)`.
    Curve {
        /// X data.
        xs: &'a [f64],
        /// Y data.
        ys: &'a [f64],
    },
    /// A histogram: `N` raw counts at their `N` bin-anchor x positions (silx
    /// `_HistogramContext`, stats.py:376-414 — `values = yData`, `axes =
    /// (xData,)` with `xData = item._revertComputeEdges(edges, alignment)`),
    /// **not** the rendered 2N step polyline.
    Histogram {
        /// Bin-anchor x positions (`_revertComputeEdges` output, length `N`).
        xs: &'a [f64],
        /// Raw bin counts (length `N`).
        counts: &'a [f64],
    },
    /// A scatter: statistics over the per-point `values` array with `(x, y)`
    /// position axes (silx `_ScatterContext`, stats.py:425-498).
    Scatter {
        /// X positions.
        xs: &'a [f64],
        /// Y positions.
        ys: &'a [f64],
        /// Per-point values (the statistic data).
        values: &'a [f64],
    },
    /// A 2D scalar image in row-major order (`data[row * width + col]`).
    Image {
        /// Row-major scalar pixel data.
        data: &'a [f64],
        /// Image width in pixels.
        width: usize,
        /// Image height in pixels.
        height: usize,
        /// Data-space top-left corner `(x, y)`.
        origin: (f64, f64),
        /// Data-space pixel size `(dx, dy)`.
        scale: (f64, f64),
    },
}

impl StatsInput<'_> {
    /// Compute the stats for this input under the given scope.
    fn compute(&self, scope: StatScope) -> Stats {
        match self {
            StatsInput::Curve { xs, ys } => Stats::for_curve(xs, ys, scope),
            // silx _HistogramContext reduces exactly like the curve context:
            // values = the N counts, position axis = the N bin anchors, with
            // the same x-only on-limits mask (stats.py:387-414).
            StatsInput::Histogram { xs, counts } => Stats::for_curve(xs, counts, scope),
            StatsInput::Scatter { xs, ys, values } => Stats::for_scatter(xs, ys, values, scope),
            StatsInput::Image {
                data,
                width,
                height,
                origin,
                scale,
            } => Stats::for_image(data, *width, *height, *origin, *scale, scope),
        }
    }
}

/// How the table refreshes its computed values.
///
/// Mirrors silx `UpdateMode` (StatsWidget.py:1258-1263): in [`Auto`] mode the
/// stats recompute every frame from the current inputs; in [`Manual`] mode
/// they only recompute when the user presses the update button (or
/// [`StatsWidget::request_update`] is called).
///
/// [`Auto`]: UpdateMode::Auto
/// [`Manual`]: UpdateMode::Manual
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UpdateMode {
    /// Recompute every frame (silx auto update).
    #[default]
    Auto,
    /// Recompute only on explicit request (silx manual update).
    Manual,
}

/// A scrollable statistics table over a set of named items.
///
/// The widget holds only display configuration (update mode, on-limits
/// toggle, last-computed rows); the data is borrowed at render time so the
/// caller owns item storage. This keeps the widget free of GPU/plot state and
/// makes the formatting and computation paths unit-testable.
#[derive(Clone, Debug, Default)]
pub struct StatsWidget {
    update_mode: UpdateMode,
    /// When true, restrict stats to the visible data range
    /// ([`StatScope::OnLimits`]); silx `setStatsOnVisibleData`.
    on_visible_data: bool,
    /// Pending recompute flag for manual mode.
    needs_update: bool,
    /// Last computed rows: `(label, stats)`.
    rows: Vec<(String, Stats)>,
}

impl StatsWidget {
    /// Create an empty stats table in auto-update mode over the full data
    /// range.
    pub fn new() -> Self {
        Self {
            update_mode: UpdateMode::Auto,
            on_visible_data: false,
            needs_update: true,
            rows: Vec::new(),
        }
    }

    /// Current update mode.
    pub fn update_mode(&self) -> UpdateMode {
        self.update_mode
    }

    /// Set the update mode (silx `setUpdateMode`).
    pub fn set_update_mode(&mut self, mode: UpdateMode) {
        self.update_mode = mode;
    }

    /// Whether statistics are restricted to the visible data range.
    pub fn on_visible_data(&self) -> bool {
        self.on_visible_data
    }

    /// Set whether to restrict statistics to the visible data range
    /// (silx `setStatsOnVisibleData`).
    pub fn set_on_visible_data(&mut self, value: bool) {
        self.on_visible_data = value;
        self.needs_update = true;
    }

    /// Request a recompute on the next [`Self::recompute`] / [`Self::ui`] call,
    /// regardless of update mode (silx manual update button).
    pub fn request_update(&mut self) {
        self.needs_update = true;
    }

    /// The last computed rows, as `(label, stats)`.
    pub fn rows(&self) -> &[(String, Stats)] {
        &self.rows
    }

    /// Recompute the cached rows from the supplied inputs if needed.
    ///
    /// In [`UpdateMode::Auto`] this always recomputes; in
    /// [`UpdateMode::Manual`] it recomputes only when an update was requested.
    /// `viewport` is the visible data rectangle `((x0, x1), (y0, y1))`, used
    /// only when on-visible-data is enabled.
    pub fn recompute(
        &mut self,
        inputs: &[(&str, StatsInput<'_>)],
        viewport: Option<((f64, f64), (f64, f64))>,
    ) {
        let should = self.update_mode == UpdateMode::Auto || self.needs_update;
        if !should {
            return;
        }
        let scope = match (self.on_visible_data, viewport) {
            (true, Some((x_range, y_range))) => StatScope::OnLimits { x_range, y_range },
            _ => StatScope::All,
        };
        self.rows = inputs
            .iter()
            .map(|(label, input)| ((*label).to_owned(), input.compute(scope)))
            .collect();
        self.needs_update = false;
    }

    /// Render the toolbar (update-mode + visible-data toggles) and the table.
    ///
    /// `inputs` is the current set of `(label, data)` items; `viewport` is the
    /// visible data rectangle used when the on-visible-data toggle is on.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        inputs: &[(&str, StatsInput<'_>)],
        viewport: Option<((f64, f64), (f64, f64))>,
    ) {
        ui.horizontal(|ui| {
            let mut auto = self.update_mode == UpdateMode::Auto;
            if ui.checkbox(&mut auto, "Auto update").changed() {
                self.update_mode = if auto {
                    UpdateMode::Auto
                } else {
                    UpdateMode::Manual
                };
            }
            let mut on_visible = self.on_visible_data;
            if ui.checkbox(&mut on_visible, "Visible data only").changed() {
                self.set_on_visible_data(on_visible);
            }
            if self.update_mode == UpdateMode::Manual && ui.button("Update").clicked() {
                self.request_update();
            }
        });

        self.recompute(inputs, viewport);

        egui::ScrollArea::both()
            .auto_shrink([false, true])
            .show(ui, |ui| {
                egui::Grid::new("stats_widget_grid")
                    .striped(true)
                    .num_columns(STAT_COLUMNS.len() + 1)
                    .show(ui, |ui| {
                        ui.strong("item");
                        for col in STAT_COLUMNS {
                            ui.strong(col);
                        }
                        ui.end_row();

                        for (label, stats) in &self.rows {
                            ui.label(label);
                            for cell in row_cells(stats) {
                                ui.label(cell);
                            }
                            ui.end_row();
                        }
                    });
            });
    }
}

/// Column headers, matching silx `DEFAULT_STATS` order exactly
/// (StatsWidget.py:1266-1276): min, coords min, max, coords max, COM, mean,
/// std.
const STAT_COLUMNS: [&str; 7] = [
    "min",
    "coords min",
    "max",
    "coords max",
    "COM",
    "mean",
    "std",
];

/// Format one table row's cells in [`STAT_COLUMNS`] order.
fn row_cells(stats: &Stats) -> [String; 7] {
    [
        format_stat(stats.min),
        format_coord(stats.coord_min),
        format_stat(stats.max),
        format_coord(stats.coord_max),
        format_coord(stats.com),
        format_stat(stats.mean),
        format_stat(stats.std),
    ]
}

/// Format a coordinate (COM / argmin / argmax) as silx `valueToString` does
/// for a tuple (PositionInfo.py:310-312, comma-joined), using the `.7g`-style
/// float formatting silx applies to coordinate components.
///
/// `(None, None)` -> `"--"`. Curve coords (`y == None`) render the x only.
/// Non-finite components print as Python would inside silx's `str(tuple)`
/// fallback (`"nan"`, `"inf"`, `"-inf"`, statshandler.py:84) — a NaN COM or
/// a NaN-positioned extremum is visible data, not an undefined stat (R2-11).
fn format_coord(coord: ComCoord) -> String {
    match (coord.x, coord.y) {
        (None, _) => "--".to_owned(),
        (Some(x), None) => coord_component(x),
        (Some(x), Some(y)) => format!("{}, {}", coord_component(x), coord_component(y)),
    }
}

/// One coordinate component: `%.7g` for finite values, Python float
/// spellings for non-finite ones.
fn coord_component(v: f64) -> String {
    if v.is_finite() {
        format_g7(v)
    } else if v.is_nan() {
        "nan".to_owned()
    } else if v > 0.0 {
        "inf".to_owned()
    } else {
        "-inf".to_owned()
    }
}

/// Port of silx `StatFormatter.format` with the default `"{0:.3f}"` formatter
/// (statshandler.py:71-84): `None` (silx `None` / `numpy.ma.masked`) ->
/// `"--"`; a NaN/±inf VALUE formats as Python's `"{0:.3f}"` does (`"nan"`,
/// `"inf"`, `"-inf"`) — silx shows `nan` for the mean of NaN-bearing data,
/// `--` only for undefined stats (R2-11).
pub fn format_stat(value: Option<f64>) -> String {
    match value {
        None => "--".to_owned(),
        // Rust `{:.3}` would print "NaN"; Python prints "nan". "inf"/"-inf"
        // already agree between the two.
        Some(v) if v.is_nan() => "nan".to_owned(),
        Some(v) => format!("{v:.3}"),
    }
}

/// Format a float like silx `valueToString` does for reals (`"%.7g"`,
/// PositionInfo.py:315): up to 7 significant digits, trailing-zero trimmed.
fn format_g7(v: f64) -> String {
    if !v.is_finite() {
        return "--".to_owned();
    }
    format_significant(v, 7)
}

/// Pure significant-digits formatter: round `value` to `digits` significant
/// figures and render without trailing zeros, switching to exponential
/// notation for very large/small magnitudes (mirrors C `%g`).
///
/// `digits` is clamped to `1..=17`. `0.0` formats as `"0"`; non-finite values
/// format as `"--"`.
pub fn format_significant(value: f64, digits: usize) -> String {
    if !value.is_finite() {
        return "--".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    let digits = digits.clamp(1, 17);
    // C `%g` decides fixed-vs-exponential from the exponent the value has
    // AFTER rounding to `digits` significant figures, not from the raw value's
    // exponent (`log10().floor()`). Round once via `%e` — which normalizes the
    // mantissa to `[1, 10)` and rounds — and read the exponent back, so a value
    // that carries up across a decade (`9999999.9` → `1e+07`, `9.9999999e-05` →
    // `0.0001`) is classified on its rounded form. (silx `"%.7g"`,
    // PositionInfo.py:315.)
    let sci = format!("{:.*e}", digits - 1, value);
    let exp: i32 = sci
        .split_once('e')
        .and_then(|(_, e)| e.parse().ok())
        .unwrap_or(0);
    // %g switches to exponential when exp < -4 or exp >= precision.
    if exp < -4 || exp >= digits as i32 {
        trim_exponential(&sci)
    } else {
        // Number of fractional digits to reach `digits` significant figures.
        let frac = (digits as i32 - 1 - exp).max(0) as usize;
        let s = format!("{value:.frac$}");
        trim_fraction(&s)
    }
}

/// Format like Python's `f"{value:.{digits}g}"`, including the non-finite
/// spellings CPython prints (`"inf"`, `"-inf"`, `"nan"`).
///
/// Distinct from [`format_significant`], whose `"--"` non-finite rendering
/// matches silx's `PositionInfo`/`valueToString` convention, not raw `%g`. Use
/// this where silx feeds a value straight through `f"{v:.5g}"` — e.g. the
/// `HistogramWidget` stat line (`_StatWidget.setValue`, histogram.py:95), whose
/// mean/std/sum can be ±inf/nan (silx `nanmean`/`nanstd`/`nansum`).
pub fn format_g_python(value: f64, digits: usize) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-inf" } else { "inf" }.to_owned();
    }
    format_significant(value, digits)
}

/// Trim trailing zeros (and a dangling decimal point) from a fixed-notation
/// number, matching C `%g`.
fn trim_fraction(s: &str) -> String {
    if !s.contains('.') {
        return s.to_owned();
    }
    let trimmed = s.trim_end_matches('0');
    let trimmed = trimmed.trim_end_matches('.');
    trimmed.to_owned()
}

/// Trim trailing zeros in the mantissa of a Rust `{:e}` string and normalize
/// to a `%g`-style exponent.
fn trim_exponential(s: &str) -> String {
    let Some((mantissa, exp)) = s.split_once('e') else {
        return s.to_owned();
    };
    let mantissa = if mantissa.contains('.') {
        let t = mantissa.trim_end_matches('0');
        t.trim_end_matches('.').to_owned()
    } else {
        mantissa.to_owned()
    };
    // Rust emits `e5` / `e-5`; %g emits `e+05` / `e-05`.
    let (sign, digits) = match exp.strip_prefix('-') {
        Some(rest) => ('-', rest),
        None => ('+', exp.strip_prefix('+').unwrap_or(exp)),
    };
    format!("{mantissa}e{sign}{digits:0>2}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_stat_none_is_dashes() {
        assert_eq!(format_stat(None), "--");
    }

    #[test]
    fn format_stat_non_finite_prints_python_spellings() {
        // silx: "{0:.3f}".format applies to the VALUE — nan/inf are data,
        // "--" is only for None/masked (statshandler.py:77-84, R2-11).
        assert_eq!(format_stat(Some(f64::NAN)), "nan");
        assert_eq!(format_stat(Some(f64::INFINITY)), "inf");
        assert_eq!(format_stat(Some(f64::NEG_INFINITY)), "-inf");
    }

    #[test]
    fn format_coord_non_finite_components_visible() {
        let c = ComCoord {
            x: Some(f64::NAN),
            y: Some(f64::NEG_INFINITY),
        };
        assert_eq!(format_coord(c), "nan, -inf");
    }

    #[test]
    fn format_stat_three_decimals() {
        // silx default formatter "{0:.3f}".
        assert_eq!(format_stat(Some(1.0)), "1.000");
        assert_eq!(format_stat(Some(4.56789)), "4.568");
        assert_eq!(format_stat(Some(-2.5)), "-2.500");
    }

    #[test]
    fn format_significant_zero() {
        assert_eq!(format_significant(0.0, 7), "0");
    }

    #[test]
    fn format_significant_non_finite() {
        assert_eq!(format_significant(f64::NAN, 7), "--");
    }

    #[test]
    fn format_significant_trims_trailing_zeros() {
        // 7 sig figs of 1.5 -> "1.5", not "1.500000".
        assert_eq!(format_significant(1.5, 7), "1.5");
        assert_eq!(format_significant(100.0, 7), "100");
    }

    #[test]
    fn format_significant_rounds_to_digits() {
        // 3 sig figs.
        assert_eq!(format_significant(4.56789, 3), "4.57");
        assert_eq!(format_significant(12345.0, 3), "1.23e+04");
    }

    #[test]
    fn format_significant_small_uses_exponential() {
        // exp < -4 -> exponential.
        assert_eq!(format_significant(0.00001234, 4), "1.234e-05");
    }

    #[test]
    fn format_g_python_matches_python_5g() {
        // Finite: same 5-sig-fig %g as format_significant.
        assert_eq!(format_g_python(1.5, 5), "1.5");
        assert_eq!(format_g_python(0.0000123456, 5), "1.2346e-05");
        assert_eq!(format_g_python(123456.0, 5), "1.2346e+05");
        // Non-finite: CPython's f"{v:.5g}" spellings, NOT format_significant's
        // "--" (which is silx's PositionInfo convention).
        assert_eq!(format_g_python(f64::INFINITY, 5), "inf");
        assert_eq!(format_g_python(f64::NEG_INFINITY, 5), "-inf");
        assert_eq!(format_g_python(f64::NAN, 5), "nan");
        // Contrast: format_significant renders non-finite as "--".
        assert_eq!(format_significant(f64::INFINITY, 5), "--");
    }

    #[test]
    fn format_significant_negative() {
        assert_eq!(format_significant(-42.5, 4), "-42.5");
    }

    #[test]
    fn format_significant_digits_clamped() {
        // digits=0 clamps to 1.
        assert_eq!(format_significant(9.0, 0), "9");
    }

    #[test]
    fn format_significant_decides_notation_after_rounding() {
        // R2-25: C `%g` picks fixed-vs-exponential from the exponent AFTER
        // rounding to `digits` sig figs, not the raw value's exponent.
        // (Cross-checked: python3 -c "print('%.7g' % v)".)
        //
        // 9999999.9: raw exp 6 (< 7 -> would wrongly pick fixed -> "10000000"),
        // but rounds up to 1e7 -> exp 7 -> exponential -> "1e+07".
        assert_eq!(format_significant(9999999.9, 7), "1e+07");
        // 9.9999999e-05: raw exp -5 (< -4 -> would wrongly pick exponential ->
        // "1e-04"), but rounds to 1.000000e-04 -> exp -4 -> fixed -> "0.0001".
        assert_eq!(format_significant(9.9999999e-05, 7), "0.0001");
        // A value that does NOT cross a decade is unaffected.
        assert_eq!(format_significant(1234567.0, 7), "1234567");
        assert_eq!(format_significant(0.0001234567, 7), "0.0001234567");
    }

    #[test]
    fn format_coord_curve_x_only() {
        let c = ComCoord {
            x: Some(2.5),
            y: None,
        };
        assert_eq!(format_coord(c), "2.5");
    }

    #[test]
    fn format_coord_image_xy() {
        let c = ComCoord {
            x: Some(1.0),
            y: Some(3.0),
        };
        assert_eq!(format_coord(c), "1, 3");
    }

    #[test]
    fn format_coord_none_is_dashes() {
        assert_eq!(format_coord(ComCoord::NONE), "--");
    }

    #[test]
    fn stat_columns_match_silx_default_stats_exactly() {
        // silx DEFAULT_STATS (StatsWidget.py:1266-1276): StatMin, StatCoordMin,
        // StatMax, StatCoordMax, StatCOM, ("mean", numpy.mean),
        // ("std", numpy.std) — no sum, no delta.
        assert_eq!(
            STAT_COLUMNS,
            [
                "min",
                "coords min",
                "max",
                "coords max",
                "COM",
                "mean",
                "std"
            ]
        );
    }

    #[test]
    fn row_cells_render_std_column() {
        // numpy.std of [2, 4, 4, 4, 5, 5, 7, 9] is exactly 2 (population std).
        let ys = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        let xs: Vec<f64> = (0..ys.len()).map(|i| i as f64).collect();
        let stats = Stats::for_curve(&xs, &ys, StatScope::All);
        let cells = row_cells(&stats);
        assert_eq!(cells.len(), STAT_COLUMNS.len());
        // "std" is the last DEFAULT_STATS column; default "{0:.3f}" format.
        assert_eq!(cells[6], "2.000");
    }

    #[test]
    fn recompute_auto_always_runs() {
        let mut w = StatsWidget::new();
        w.set_update_mode(UpdateMode::Auto);
        let xs = [0.0, 1.0, 2.0];
        let ys = [1.0, 2.0, 3.0];
        let inputs: Vec<(&str, StatsInput<'_>)> =
            vec![("c", StatsInput::Curve { xs: &xs, ys: &ys })];
        w.recompute(&inputs, None);
        assert_eq!(w.rows().len(), 1);
        assert_eq!(w.rows()[0].0, "c");
        assert_eq!(w.rows()[0].1.included_count, 3);
    }

    #[test]
    fn recompute_manual_waits_for_request() {
        let mut w = StatsWidget::new();
        w.set_update_mode(UpdateMode::Manual);
        let xs = [0.0, 1.0];
        let ys = [1.0, 2.0];
        let inputs: Vec<(&str, StatsInput<'_>)> =
            vec![("c", StatsInput::Curve { xs: &xs, ys: &ys })];
        // First call: needs_update is true from new(); recomputes once.
        w.recompute(&inputs, None);
        assert_eq!(w.rows().len(), 1);
        // Mutate inputs and recompute without request: rows stay stale.
        let xs2 = [0.0, 1.0, 2.0, 3.0];
        let ys2 = [1.0, 1.0, 1.0, 1.0];
        let inputs2: Vec<(&str, StatsInput<'_>)> =
            vec![("c", StatsInput::Curve { xs: &xs2, ys: &ys2 })];
        w.recompute(&inputs2, None);
        assert_eq!(w.rows()[0].1.included_count, 2, "should not have refreshed");
        // After explicit request, it refreshes.
        w.request_update();
        w.recompute(&inputs2, None);
        assert_eq!(w.rows()[0].1.included_count, 4);
    }

    #[test]
    fn recompute_on_visible_data_clips() {
        let mut w = StatsWidget::new();
        w.set_on_visible_data(true);
        let xs = [0.0, 1.0, 2.0, 3.0, 4.0];
        let ys = [10.0, 20.0, 30.0, 40.0, 50.0];
        let inputs: Vec<(&str, StatsInput<'_>)> =
            vec![("c", StatsInput::Curve { xs: &xs, ys: &ys })];
        // Viewport x in [1,3] keeps 3 points.
        w.recompute(&inputs, Some(((1.0, 3.0), (-1e9, 1e9))));
        assert_eq!(w.rows()[0].1.included_count, 3);
        // Without viewport given, on-visible falls back to All.
        w.request_update();
        w.recompute(&inputs, None);
        assert_eq!(w.rows()[0].1.included_count, 5);
    }

    #[test]
    fn recompute_image_input() {
        let mut w = StatsWidget::new();
        let data = [1.0, 2.0, 3.0, 9.0];
        let inputs: Vec<(&str, StatsInput<'_>)> = vec![(
            "img",
            StatsInput::Image {
                data: &data,
                width: 2,
                height: 2,
                origin: (0.0, 0.0),
                scale: (1.0, 1.0),
            },
        )];
        w.recompute(&inputs, None);
        let s = &w.rows()[0].1;
        assert_eq!(s.max, Some(9.0));
        // argmax at col=1,row=1 -> coords (1,1).
        assert_eq!(s.coord_max.x, Some(1.0));
        assert_eq!(s.coord_max.y, Some(1.0));
    }
}
