//! The plot model.
//!
//! Holds the identifier, data-area background, data limits, margins, and the
//! optional colormap used to draw the colorbar. The item list, log/inverted
//! axis flags, and dirty tracking are added in later steps
//! (`doc/design.md` §1·§4·§11).

use egui::{Color32, Rect};

use crate::core::colormap::Colormap;
use crate::core::marker::Marker;
use crate::core::roi::Roi;
use crate::core::shape::Shape;
use crate::core::transform::{Axis, Margins, Scale, Transform, keep_aspect_limits};
use crate::core::triangles::Triangles;

/// Per-axis pan/zoom range constraints mirroring silx
/// `Axis.setRangeConstraints` / `Axis.setLimitsConstraints`.
///
/// All fields are optional; `None` means unconstrained. Applied by the
/// interaction helpers after every pan/zoom so the display range always
/// satisfies all set constraints.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AxisConstraints {
    /// Minimum allowed span (display range). Prevents zooming in past this.
    pub min_range: Option<f64>,
    /// Maximum allowed span (display range). Prevents zooming out past this.
    pub max_range: Option<f64>,
    /// Minimum allowed lower bound. Prevents panning the view below this value.
    pub min_pos: Option<f64>,
    /// Maximum allowed upper bound. Prevents panning the view above this value.
    pub max_pos: Option<f64>,
}

impl AxisConstraints {
    /// Return `(lo, hi)` clamped so all set constraints are satisfied. The
    /// span is corrected first (centered on the current midpoint), then the
    /// position window is clamped (shifting both ends equally).
    pub fn apply(self, lo: f64, hi: f64) -> (f64, f64) {
        let mut span = hi - lo;
        if span <= 0.0 {
            return (lo, hi);
        }

        // 1. Clamp the span.
        if let Some(min) = self.min_range
            && span < min
        {
            span = min;
        }
        if let Some(max) = self.max_range
            && span > max
        {
            span = max;
        }

        // 2. Re-center the clamped span on the original midpoint.
        let mid = (lo + hi) * 0.5;
        let mut new_lo = mid - span * 0.5;
        let mut new_hi = mid + span * 0.5;

        // 3. Clamp the position window (shift both ends to stay inside bounds).
        if let Some(min_pos) = self.min_pos
            && new_lo < min_pos
        {
            let shift = min_pos - new_lo;
            new_lo += shift;
            new_hi += shift;
        }
        if let Some(max_pos) = self.max_pos
            && new_hi > max_pos
        {
            let shift = new_hi - max_pos;
            new_lo -= shift;
            new_hi -= shift;
        }

        // 4. Final sanity — keep lo < hi even if constraints are contradictory.
        if new_lo >= new_hi {
            return (lo, hi);
        }

        (new_lo, new_hi)
    }

    /// `true` when all fields are `None` (no constraints set).
    pub fn is_unconstrained(self) -> bool {
        self.min_range.is_none()
            && self.max_range.is_none()
            && self.min_pos.is_none()
            && self.max_pos.is_none()
    }
}

/// Identifier for a single `Plot` instance.
///
/// `egui_wgpu`'s `callback_resources` is a global type map, so multi-plot keeps
/// per-plot GPU state separated by `PlotId` (`doc/design.md` §3.1·§12). The
/// current steps handle a single plot, so no separation map exists yet.
pub type PlotId = u64;

/// Grid lines drawn in the plot data area.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GraphGrid {
    /// No grid lines.
    None,
    /// Major tick grid lines only.
    #[default]
    Major,
    /// Major and minor tick grid lines.
    MajorAndMinor,
}

impl GraphGrid {
    /// Whether major grid lines are drawn.
    pub fn major(self) -> bool {
        matches!(self, Self::Major | Self::MajorAndMinor)
    }

    /// Whether minor grid lines are drawn.
    pub fn minor(self) -> bool {
        matches!(self, Self::MajorAndMinor)
    }
}

/// The full data range of a plot, mirroring silx `_PlotDataRange`
/// (`PlotWidget.getDataRange`). Each member is the `(min, max)` data bounds for
/// that axis, or `None` when no data is associated with the axis.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DataRange {
    /// X-axis data bounds, or `None` when no item drives the X axis.
    pub x: Option<(f64, f64)>,
    /// Left Y-axis data bounds.
    pub y: Option<(f64, f64)>,
    /// Right (y2) Y-axis data bounds.
    pub y2: Option<(f64, f64)>,
}

/// Per-side data-margin ratios added around the visible data on reset-zoom
/// (silx `PlotWidget.setDataMargins` / `_utils.addMarginsToLimits`).
///
/// Each field is a ratio of the data range applied to one limit. silx names
/// these `(xMinMargin, xMaxMargin, yMinMargin, yMaxMargin)`; the field names
/// here keep that axis/side mapping explicit:
///
/// - `x_min` — the X lower (left) side,
/// - `x_max` — the X upper (right) side,
/// - `y_min` — the Y lower (bottom) side,
/// - `y_max` — the Y upper (top) side.
///
/// The Y margins also apply to the y2 axis, matching silx (the y2 branch in
/// `addMarginsToLimits` reuses `yMinMargin`/`yMaxMargin`). Zero by default.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DataMargins {
    /// X lower (left) margin ratio.
    pub x_min: f64,
    /// X upper (right) margin ratio.
    pub x_max: f64,
    /// Y lower (bottom) margin ratio.
    pub y_min: f64,
    /// Y upper (top) margin ratio.
    pub y_max: f64,
}

impl DataMargins {
    /// Expand `(lo, hi)` on a single axis by the low/high ratios, in log space
    /// when `is_log` (silx `addMarginsToLimits`). For a log axis with a
    /// non-positive bound the margin is skipped (silx "Do not apply margins if
    /// limits < 0").
    fn expand_axis(lo: f64, hi: f64, low_ratio: f64, high_ratio: f64, is_log: bool) -> (f64, f64) {
        if !is_log {
            let range = hi - lo;
            (lo - low_ratio * range, hi + high_ratio * range)
        } else if lo > 0.0 && hi > 0.0 {
            let lo_log = lo.log10();
            let hi_log = hi.log10();
            let range_log = hi_log - lo_log;
            (
                10f64.powf(lo_log - low_ratio * range_log),
                10f64.powf(hi_log + high_ratio * range_log),
            )
        } else {
            (lo, hi)
        }
    }
}

/// One plot.
pub struct Plot {
    /// Instance identifier.
    pub id: PlotId,
    /// Data-area background color (maps to silx `setBackgroundColors`' data background).
    pub data_background: Color32,
    /// Data-space limits `(x_min, x_max, y_min, y_max)`.
    pub limits: (f64, f64, f64, f64),
    /// Margins reserving extra space inside the chrome gutters. Zero by default.
    pub margins: Margins,
    /// Colormap drawn as the colorbar (mirrors the displayed image's colormap).
    /// `None` hides the colorbar (`doc/design.md` §5·§8).
    pub colormap: Option<Colormap>,
    /// Limits to restore on a double-click "reset". The widget captures the
    /// first observed `limits` here so the home view survives pan/zoom
    /// (`doc/design.md` §8·§11.6). `None` until the first frame.
    pub home_limits: Option<(f64, f64, f64, f64)>,
    /// X-axis scale (linear or log10) (`doc/design.md` §13 A3).
    pub x_scale: Scale,
    /// Y-axis scale (linear or log10).
    pub y_scale: Scale,
    /// Reverse the X-axis on-screen direction (`doc/design.md` §13 A2).
    pub x_inverted: bool,
    /// Reverse the Y-axis on-screen direction.
    pub y_inverted: bool,
    /// Keep data square on screen by expanding the tighter axis' display range
    /// (silx `setKeepDataAspectRatio`). Only honored when both axes are linear
    /// (`doc/design.md` §13 A4).
    pub keep_aspect: bool,
    /// Secondary right Y axis limits `(y2_min, y2_max)`, or `None` for no y2
    /// axis. Curves bound to [`crate::YAxis::Right`] are plotted against it and
    /// its ticks are drawn in the right gutter (linear, `doc/design.md` §13 A5).
    pub y2: Option<(f64, f64)>,
    /// Draw a crosshair + coordinate readout following the pointer when it is
    /// over the data area (silx `setGraphCursor`, `doc/design.md` §13 C1).
    pub crosshair: bool,
    /// Regions of interest drawn over the data area with draggable edge
    /// handles. Dragging an edge updates that ROI's bounds in place and the
    /// widget reports the changed index (`doc/design.md` §13 C3).
    pub rois: Vec<Roi>,
    /// Point / line markers drawn over the data area (silx `addMarker`). Each is
    /// a static overlay; the widget draws the list every frame.
    pub markers: Vec<Marker>,
    /// Polygon / rectangle / polyline / line shapes drawn over the data area
    /// (silx `addShape`). Static overlays drawn every frame.
    pub shapes: Vec<Shape>,
    /// Per-vertex-colored filled triangle meshes drawn in the data layer (silx
    /// `addTriangles`). Drawn every frame under the chrome.
    pub triangles: Vec<Triangles>,
    /// Graph title, drawn centered above the data area (silx `setGraphTitle`,
    /// `BackendBase.setGraphTitle`). `None` reserves no top space for it.
    pub title: Option<String>,
    /// X-axis label, drawn centered below the X tick labels (silx
    /// `setGraphXLabel`). `None` reserves no extra bottom space.
    pub x_label: Option<String>,
    /// Left Y-axis label, drawn rotated at the far left (silx `setGraphYLabel`).
    /// `None` reserves no extra left space.
    pub y_label: Option<String>,
    /// Right (y2) Y-axis label, drawn rotated at the far right; only shown when
    /// a [`Self::y2`] axis exists. `None` reserves no extra right space.
    pub y2_label: Option<String>,
    /// Foreground color override for axes/frame/ticks/labels (silx
    /// `setForegroundColor`). `None` follows the egui theme's text color.
    pub foreground: Option<Color32>,
    /// Grid-line color override (silx `setGridColor`). `None` uses a faint tint
    /// of the foreground color.
    pub grid_color: Option<Color32>,
    /// Grid lines drawn in the data area (`setGraphGrid`).
    pub grid: GraphGrid,
    /// Pan/zoom constraints for the X axis (silx `getXAxis().setRangeConstraints`).
    pub x_constraints: AxisConstraints,
    /// Pan/zoom constraints for the left Y axis (silx `getYAxis().setRangeConstraints`).
    pub y_constraints: AxisConstraints,
    /// Maximum number of major ticks on the X axis.  `None` uses the default
    /// (8).  The chrome calls `nice_ticks` with this value, so the actual count
    /// may be slightly lower to keep round step sizes.
    pub x_max_ticks: Option<usize>,
    /// Maximum number of major ticks on the left Y axis.  `None` uses the
    /// default (6).
    pub y_max_ticks: Option<usize>,
    /// Limits-history stack mirroring silx `LimitsHistory`. Each entry is a
    /// full view snapshot `(x_min, x_max, y_min, y_max, y2)`. The widget pushes
    /// before a zoom/box-zoom/pan; [`Self::zoom_back`] restores the most recent
    /// entry. Like silx, the stack is unbounded (silx `LimitsHistory` is a plain
    /// list with no depth cap).
    limits_history: Vec<LimitsHistoryEntry>,
    /// Whether the X axis refits to data on reset-zoom (silx
    /// `Axis.setAutoScale` / `PlotWidget.setXAxisAutoScale`). Defaults to `true`.
    x_autoscale: bool,
    /// Whether the left Y axis refits to data on reset-zoom
    /// (`setYAxisAutoScale`). Defaults to `true`.
    y_autoscale: bool,
    /// Whether the right (y2) Y axis refits to data on reset-zoom. Defaults to
    /// `true`. silx ties y2 autoscale to the left Y axis flag; here it is
    /// tracked separately so a caller can pin only the y2 range.
    y2_autoscale: bool,
    /// Cached per-axis data bounds, mirroring silx `PlotWidget._dataRange`
    /// (returned by `getDataRange`). The high-level widget owns item data and
    /// pushes the accumulated bounds here via [`Self::set_data_range`]; the model
    /// layer holds no items, so this is `None` until populated.
    data_range: Option<DataRange>,
    /// Per-side data margins applied around the visible data on reset-zoom
    /// (silx `setDataMargins`). Zero by default.
    data_margins: DataMargins,
}

/// One snapshot in [`Plot::limits_history`]: the left-axes limits plus the
/// optional right (y2) axis range, mirroring silx `LimitsHistory`'s
/// `(xmin, xmax, ymin, ymax, y2min, y2max)` tuple.
type LimitsHistoryEntry = ((f64, f64, f64, f64), Option<(f64, f64)>);

impl Plot {
    /// Create a plot with the given id, a default dark background, unit limits,
    /// no margins, and no colorbar.
    pub fn new(id: PlotId) -> Self {
        Self {
            id,
            data_background: Color32::from_rgb(16, 16, 24),
            limits: (0.0, 1.0, 0.0, 1.0),
            margins: Margins::ZERO,
            colormap: None,
            home_limits: None,
            x_scale: Scale::Linear,
            y_scale: Scale::Linear,
            x_inverted: false,
            y_inverted: false,
            keep_aspect: false,
            y2: None,
            crosshair: false,
            rois: Vec::new(),
            markers: Vec::new(),
            shapes: Vec::new(),
            triangles: Vec::new(),
            title: None,
            x_label: None,
            y_label: None,
            y2_label: None,
            foreground: None,
            grid_color: None,
            grid: GraphGrid::Major,
            x_constraints: AxisConstraints::default(),
            y_constraints: AxisConstraints::default(),
            x_max_ticks: None,
            y_max_ticks: None,
            limits_history: Vec::new(),
            x_autoscale: true,
            y_autoscale: true,
            y2_autoscale: true,
            data_range: None,
            data_margins: DataMargins::default(),
        }
    }

    /// Append the current view (left limits plus the y2 range) to the limits
    /// history, mirroring silx `LimitsHistory.push`. The widget calls this
    /// before applying a zoom/box-zoom/pan so [`Self::zoom_back`] can restore it.
    pub fn push_limits(&mut self) {
        self.limits_history.push((self.limits, self.y2));
    }

    /// Restore the most recently pushed view, mirroring silx
    /// `LimitsHistory.pop`. Returns `true` if a stored view was restored, or
    /// `false` if the history was empty (silx falls back to `resetZoom`; here
    /// the caller decides, and `false` signals that nothing was restored).
    pub fn zoom_back(&mut self) -> bool {
        if let Some((limits, y2)) = self.limits_history.pop() {
            self.limits = limits;
            self.y2 = y2;
            true
        } else {
            false
        }
    }

    /// Clear the stored limits history, mirroring silx `LimitsHistory.clear`
    /// (called on reset / zoom-mode change).
    pub fn clear_limits_history(&mut self) {
        self.limits_history.clear();
    }

    /// Number of stored history entries, mirroring `len(LimitsHistory)`.
    pub fn limits_history_len(&self) -> usize {
        self.limits_history.len()
    }

    /// Whether the X axis refits to data on reset-zoom (silx
    /// `isXAxisAutoScale`).
    pub fn x_autoscale(&self) -> bool {
        self.x_autoscale
    }

    /// Set whether the X axis refits to data on reset-zoom
    /// (silx `setXAxisAutoScale`).
    pub fn set_x_autoscale(&mut self, on: bool) {
        self.x_autoscale = on;
    }

    /// Whether the left Y axis refits to data on reset-zoom (silx
    /// `isYAxisAutoScale`).
    pub fn y_autoscale(&self) -> bool {
        self.y_autoscale
    }

    /// Set whether the left Y axis refits to data on reset-zoom
    /// (silx `setYAxisAutoScale`).
    pub fn set_y_autoscale(&mut self, on: bool) {
        self.y_autoscale = on;
    }

    /// Whether the right (y2) Y axis refits to data on reset-zoom.
    pub fn y2_autoscale(&self) -> bool {
        self.y2_autoscale
    }

    /// Set whether the right (y2) Y axis refits to data on reset-zoom.
    pub fn set_y2_autoscale(&mut self, on: bool) {
        self.y2_autoscale = on;
    }

    /// The cached per-axis data range, mirroring silx `getDataRange`. Returns a
    /// [`DataRange`] with each member `None` until the high-level widget pushes
    /// bounds via [`Self::set_data_range`]. silx lazily recomputes from items
    /// here; this model layer holds no items, so an unset range reads as all
    /// `None`.
    pub fn data_range(&self) -> DataRange {
        self.data_range.unwrap_or_default()
    }

    /// Store the accumulated per-axis data bounds (silx populates `_dataRange`
    /// from its items in `_updateDataRange`). The high-level widget owns the
    /// item data and calls this; [`Self::reset_zoom`] then refits from it.
    pub fn set_data_range(&mut self, range: DataRange) {
        self.data_range = Some(range);
    }

    /// Refit the view from the cached [`Self::data_range`], honoring the per-axis
    /// autoscale flags (silx `PlotWidget.resetZoom` with `getDataRange()`).
    /// Equivalent to `reset_zoom_to_data_range(self.data_range())`.
    pub fn reset_zoom(&mut self) {
        self.reset_zoom_to_data_range(self.data_range());
    }

    /// The per-side data margins applied around the data on reset-zoom (silx
    /// `getDataMargins`).
    pub fn data_margins(&self) -> DataMargins {
        self.data_margins
    }

    /// Set the per-side data margins (silx `setDataMargins`). The ratios expand
    /// each refit axis around its data range on the next reset-zoom; for log
    /// axes they expand in log space.
    pub fn set_data_margins(&mut self, margins: DataMargins) {
        self.data_margins = margins;
    }

    /// Refit the view to `data` honoring the per-axis autoscale flags, mirroring
    /// silx `PlotWidget.resetZoom`. An axis whose autoscale flag is off keeps its
    /// current display range; an axis whose flag is on is refit to its data
    /// bounds (when present).
    ///
    /// silx also forces autoscale on a log axis whose current lower limit is
    /// `<= 0` (so toggling to log re-fits to positive data); that rule is applied
    /// here per axis via the [`Scale::Log10`] check (matches
    /// `PlotWidget.resetZoom`:3377-3382). Axes with no data bounds and autoscale
    /// off are left untouched.
    ///
    /// This is the pure model operation; the high-level widget owns the actual
    /// `data` accumulation (its `DataBounds`) and calls this with the current
    /// range.
    pub fn reset_zoom_to_data_range(&mut self, data: DataRange) {
        let (mut x_min, mut x_max, mut y_min, mut y_max) = self.limits;
        let mut y2 = self.y2;

        // Force autoscale on a log axis whose lower limit is <= 0 (silx
        // resetZoom:3377-3382).
        let x_auto = self.x_autoscale || (self.x_scale == Scale::Log10 && x_min <= 0.0);
        let y_log_force = self.y_scale == Scale::Log10
            && (y_min <= 0.0 || self.y2.map(|(lo, _)| lo <= 0.0).unwrap_or(false));
        let y_auto = self.y_autoscale || y_log_force;
        let y2_auto = self.y2_autoscale || y_log_force;

        // Track which axes are refit; only those receive data margins, matching
        // silx (pinned axes are restored after _forceResetZoom without margins).
        let mut x_refit = false;
        let mut y_refit = false;
        let mut y2_refit = false;

        if x_auto && let Some((dmin, dmax)) = data.x {
            x_min = dmin;
            x_max = dmax;
            x_refit = true;
        }
        if y_auto && let Some((dmin, dmax)) = data.y {
            y_min = dmin;
            y_max = dmax;
            y_refit = true;
        }
        if y2_auto && let Some((dmin, dmax)) = data.y2 {
            y2 = Some((dmin, dmax));
            y2_refit = true;
        }

        // Expand refit axes by the data margins (silx applies margins=True in
        // setLimits during _forceResetZoom; addMarginsToLimits respects log
        // axes and the shared y/y2 margin ratios).
        let m = self.data_margins;
        let x_is_log = self.x_scale == Scale::Log10;
        let y_is_log = self.y_scale == Scale::Log10;
        if x_refit {
            (x_min, x_max) = DataMargins::expand_axis(x_min, x_max, m.x_min, m.x_max, x_is_log);
        }
        if y_refit {
            (y_min, y_max) = DataMargins::expand_axis(y_min, y_max, m.y_min, m.y_max, y_is_log);
        }
        if y2_refit && let Some((lo, hi)) = y2 {
            // y2 axis uses the Y margin ratios and the Y log flag (silx reuses
            // yMinMargin/yMaxMargin and isYLog for the y2 branch).
            y2 = Some(DataMargins::expand_axis(lo, hi, m.y_min, m.y_max, y_is_log));
        }

        self.limits = (x_min, x_max, y_min, y_max);
        self.y2 = y2;
    }

    /// Build the data↔screen transform for the given data-area rect, honoring
    /// the per-axis scale, inversion, and (linear-only) aspect-ratio lock.
    ///
    /// Aspect correction is derived here from the stable requested `limits`, so
    /// it is the same view used for rendering, chrome, and pointer mapping —
    /// and resizing never compounds the expansion (`doc/design.md` §13 A4).
    pub fn transform(&self, area: Rect) -> Transform {
        let linear = self.x_scale == Scale::Linear && self.y_scale == Scale::Linear;
        let (x_min, x_max, y_min, y_max) = if self.keep_aspect && linear {
            keep_aspect_limits(self.limits, area)
        } else {
            self.limits
        };
        let x = Axis {
            min: x_min,
            max: x_max,
            scale: self.x_scale,
            inverted: self.x_inverted,
        };
        let y = Axis {
            min: y_min,
            max: y_max,
            scale: self.y_scale,
            inverted: self.y_inverted,
        };
        Transform::with_axes(x, y, area)
    }

    /// Build the transform for the secondary right (y2) axis, sharing the left
    /// transform's X axis exactly (including any aspect expansion) so curves on
    /// both axes stay aligned in X. `None` when the plot has no y2 axis. The y2
    /// axis is linear, non-inverted (`doc/design.md` §13 A5).
    pub fn transform_y2(&self, area: Rect) -> Option<Transform> {
        let (y2_min, y2_max) = self.y2?;
        let left = self.transform(area);
        let y2 = Axis::linear(y2_min, y2_max);
        Some(Transform::with_axes(left.x, y2, area))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn area() -> Rect {
        Rect::from_min_max(pos2(0.0, 0.0), pos2(200.0, 100.0))
    }

    #[test]
    fn axis_constraints_unconstrained_is_passthrough() {
        let c = AxisConstraints::default();
        assert_eq!(c.apply(0.0, 10.0), (0.0, 10.0));
        assert!(c.is_unconstrained());
    }

    #[test]
    fn axis_constraints_min_range_widens_span() {
        let c = AxisConstraints {
            min_range: Some(5.0),
            ..Default::default()
        };
        // Current span is 2.0, below min; should be widened to 5.0 centered on 1.0.
        let (lo, hi) = c.apply(0.0, 2.0);
        assert!((hi - lo - 5.0).abs() < 1e-10, "span={}", hi - lo);
        assert!(((lo + hi) / 2.0 - 1.0).abs() < 1e-10); // centered on original mid
    }

    #[test]
    fn axis_constraints_max_range_narrows_span() {
        let c = AxisConstraints {
            max_range: Some(5.0),
            ..Default::default()
        };
        // Current span is 10.0, above max; should be narrowed to 5.0 centered on 5.0.
        let (lo, hi) = c.apply(0.0, 10.0);
        assert!((hi - lo - 5.0).abs() < 1e-10, "span={}", hi - lo);
        assert!(((lo + hi) / 2.0 - 5.0).abs() < 1e-10);
    }

    #[test]
    fn axis_constraints_min_pos_shifts_window_right() {
        let c = AxisConstraints {
            min_pos: Some(2.0),
            ..Default::default()
        };
        // View [0, 4] would place lo below min_pos=2; shift right so lo=2.
        let (lo, hi) = c.apply(0.0, 4.0);
        assert!((lo - 2.0).abs() < 1e-10, "lo={lo}");
        assert!((hi - 6.0).abs() < 1e-10, "hi={hi}");
    }

    #[test]
    fn axis_constraints_max_pos_shifts_window_left() {
        let c = AxisConstraints {
            max_pos: Some(8.0),
            ..Default::default()
        };
        // View [6, 12] places hi above max_pos=8; shift left so hi=8.
        let (lo, hi) = c.apply(6.0, 12.0);
        assert!((hi - 8.0).abs() < 1e-10, "hi={hi}");
        assert!((lo - 2.0).abs() < 1e-10, "lo={lo}");
    }

    #[test]
    fn axis_constraints_degenerate_span_is_passthrough() {
        let c = AxisConstraints {
            min_range: Some(1.0),
            ..Default::default()
        };
        // Already-invalid spans return unchanged (guard against further corruption).
        assert_eq!(c.apply(5.0, 3.0), (5.0, 3.0));
    }

    #[test]
    fn transform_y2_is_none_without_y2_axis() {
        let plot = Plot::new(0);
        assert!(plot.transform_y2(area()).is_none());
    }

    #[test]
    fn limits_history_starts_empty() {
        let plot = Plot::new(0);
        assert_eq!(plot.limits_history_len(), 0);
    }

    #[test]
    fn limits_history_push_then_zoom_back_restores_previous() {
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 1.0, 0.0, 1.0);
        plot.y2 = Some((0.0, 2.0));
        // Push the initial view, then change the view (as a zoom would).
        plot.push_limits();
        assert_eq!(plot.limits_history_len(), 1);
        plot.limits = (0.25, 0.75, 0.25, 0.75);
        plot.y2 = Some((0.5, 1.5));
        // zoom_back restores the pushed view (limits AND y2) and pops the entry.
        assert!(plot.zoom_back());
        assert_eq!(plot.limits, (0.0, 1.0, 0.0, 1.0));
        assert_eq!(plot.y2, Some((0.0, 2.0)));
        assert_eq!(plot.limits_history_len(), 0);
    }

    #[test]
    fn zoom_back_on_empty_history_returns_false_and_keeps_view() {
        // Boundary: empty stack -> zoom_back is a no-op returning false (silx
        // pop() returns False on empty history).
        let mut plot = Plot::new(0);
        plot.limits = (1.0, 2.0, 3.0, 4.0);
        assert!(!plot.zoom_back());
        assert_eq!(plot.limits, (1.0, 2.0, 3.0, 4.0));
    }

    #[test]
    fn limits_history_is_lifo_and_unbounded() {
        // silx LimitsHistory is a plain list (no depth cap); pushes stack LIFO.
        let mut plot = Plot::new(0);
        for i in 0..1000 {
            plot.limits = (i as f64, i as f64 + 1.0, 0.0, 1.0);
            plot.push_limits();
        }
        assert_eq!(plot.limits_history_len(), 1000);
        // Pop order is last-in-first-out.
        assert!(plot.zoom_back());
        assert_eq!(plot.limits, (999.0, 1000.0, 0.0, 1.0));
        assert!(plot.zoom_back());
        assert_eq!(plot.limits, (998.0, 999.0, 0.0, 1.0));
        assert_eq!(plot.limits_history_len(), 998);
    }

    #[test]
    fn clear_limits_history_empties_the_stack() {
        let mut plot = Plot::new(0);
        plot.push_limits();
        plot.push_limits();
        assert_eq!(plot.limits_history_len(), 2);
        plot.clear_limits_history();
        assert_eq!(plot.limits_history_len(), 0);
        assert!(!plot.zoom_back());
    }

    #[test]
    fn transform_y2_shares_left_x_and_maps_its_own_y() {
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 100.0);
        plot.y2 = Some((-1.0, 1.0));
        let left = plot.transform(area());
        let right = plot.transform_y2(area()).expect("y2 transform");

        // X axis is shared exactly, so curves on both axes align in X.
        assert_eq!(left.x, right.x);
        // The right axis maps its own y2 range: y2_min at the bottom edge, y2_max
        // at the top edge of the same area.
        let bottom = right.data_to_pixel(0.0, -1.0).y;
        let top = right.data_to_pixel(0.0, 1.0).y;
        assert!((bottom - area().bottom()).abs() <= 1e-3, "{bottom}");
        assert!((top - area().top()).abs() <= 1e-3, "{top}");
    }

    #[test]
    fn autoscale_defaults_on_for_all_axes() {
        let plot = Plot::new(0);
        assert!(plot.x_autoscale());
        assert!(plot.y_autoscale());
        assert!(plot.y2_autoscale());
    }

    #[test]
    fn reset_zoom_refits_only_autoscale_on_axes() {
        // X autoscale off: X range preserved; Y autoscale on: Y refit to data.
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 1.0, 0.0, 1.0);
        plot.set_x_autoscale(false);
        plot.set_y_autoscale(true);
        plot.reset_zoom_to_data_range(DataRange {
            x: Some((10.0, 20.0)),
            y: Some((-5.0, 5.0)),
            y2: None,
        });
        // X preserved (autoscale off), Y refit (autoscale on).
        assert_eq!(plot.limits, (0.0, 1.0, -5.0, 5.0));
    }

    #[test]
    fn reset_zoom_refits_x_when_only_x_autoscale_on() {
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 1.0, 0.0, 1.0);
        plot.set_x_autoscale(true);
        plot.set_y_autoscale(false);
        plot.reset_zoom_to_data_range(DataRange {
            x: Some((10.0, 20.0)),
            y: Some((-5.0, 5.0)),
            y2: None,
        });
        // X refit, Y preserved.
        assert_eq!(plot.limits, (10.0, 20.0, 0.0, 1.0));
    }

    #[test]
    fn reset_zoom_with_all_autoscale_off_is_noop() {
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 1.0, 0.0, 1.0);
        plot.y2 = Some((0.0, 2.0));
        plot.set_x_autoscale(false);
        plot.set_y_autoscale(false);
        plot.set_y2_autoscale(false);
        plot.reset_zoom_to_data_range(DataRange {
            x: Some((10.0, 20.0)),
            y: Some((-5.0, 5.0)),
            y2: Some((-1.0, 1.0)),
        });
        // Nothing changes: every axis pinned.
        assert_eq!(plot.limits, (0.0, 1.0, 0.0, 1.0));
        assert_eq!(plot.y2, Some((0.0, 2.0)));
    }

    #[test]
    fn reset_zoom_autoscale_on_axis_with_no_data_is_preserved() {
        // Boundary: autoscale on but no data bounds -> range left untouched.
        let mut plot = Plot::new(0);
        plot.limits = (3.0, 7.0, 2.0, 8.0);
        plot.reset_zoom_to_data_range(DataRange {
            x: None,
            y: Some((-1.0, 1.0)),
            y2: None,
        });
        // X has no data -> preserved; Y refit.
        assert_eq!(plot.limits, (3.0, 7.0, -1.0, 1.0));
    }

    #[test]
    fn reset_zoom_log_axis_forces_autoscale_when_lower_limit_nonpositive() {
        // X is log with a <= 0 lower limit and autoscale OFF; silx forces it on.
        let mut plot = Plot::new(0);
        plot.x_scale = Scale::Log10;
        plot.limits = (-1.0, 100.0, 0.0, 1.0);
        plot.set_x_autoscale(false);
        plot.set_y_autoscale(false);
        plot.reset_zoom_to_data_range(DataRange {
            x: Some((1.0, 1000.0)),
            y: Some((-5.0, 5.0)),
            y2: None,
        });
        // X refit despite autoscale off (forced by log + nonpositive lower).
        assert_eq!(plot.limits.0, 1.0);
        assert_eq!(plot.limits.1, 1000.0);
        // Y stays pinned.
        assert_eq!((plot.limits.2, plot.limits.3), (0.0, 1.0));
    }

    #[test]
    fn data_margins_default_zero_and_noop() {
        let mut plot = Plot::new(0);
        assert_eq!(plot.data_margins(), DataMargins::default());
        plot.set_data_range(DataRange {
            x: Some((0.0, 10.0)),
            y: Some((0.0, 10.0)),
            y2: None,
        });
        plot.reset_zoom();
        // No margins -> exact data bounds.
        assert_eq!(plot.limits, (0.0, 10.0, 0.0, 10.0));
    }

    #[test]
    fn data_margins_linear_left_expands_xmin_by_ratio_of_range() {
        // 0.1 left margin on a [0, 10] range expands xmin by 10% of 10 = 1.
        let mut plot = Plot::new(0);
        plot.set_data_margins(DataMargins {
            x_min: 0.1,
            ..Default::default()
        });
        plot.set_data_range(DataRange {
            x: Some((0.0, 10.0)),
            y: Some((0.0, 10.0)),
            y2: None,
        });
        plot.reset_zoom();
        assert!(
            (plot.limits.0 - (-1.0)).abs() < 1e-9,
            "xmin={}",
            plot.limits.0
        );
        // xmax untouched (no right margin), y untouched (no y margins).
        assert_eq!(plot.limits.1, 10.0);
        assert_eq!((plot.limits.2, plot.limits.3), (0.0, 10.0));
    }

    #[test]
    fn data_margins_log_expands_in_log_space() {
        // Log X over [1, 100] (2 decades). A 0.1 left margin expands xmin by 10%
        // of the 2-decade range in log space: 10^(0 - 0.1*2) = 10^-0.2.
        let mut plot = Plot::new(0);
        plot.x_scale = Scale::Log10;
        plot.set_data_margins(DataMargins {
            x_min: 0.1,
            ..Default::default()
        });
        plot.set_data_range(DataRange {
            x: Some((1.0, 100.0)),
            y: Some((1.0, 100.0)),
            y2: None,
        });
        plot.reset_zoom();
        let expected = 10f64.powf(-0.2);
        assert!(
            (plot.limits.0 - expected).abs() < 1e-9,
            "xmin={} expected={expected}",
            plot.limits.0
        );
        assert_eq!(plot.limits.1, 100.0);
    }

    #[test]
    fn data_margins_log_skips_nonpositive_bound() {
        // Boundary: log axis with a non-positive lower bound -> margin skipped
        // (silx "Do not apply margins if limits < 0"), but the bound itself is
        // still the refit value.
        let (lo, hi) = DataMargins::expand_axis(0.0, 100.0, 0.1, 0.1, true);
        assert_eq!((lo, hi), (0.0, 100.0));
    }

    #[test]
    fn data_margins_only_applied_to_refit_axes() {
        // X autoscale off -> X keeps its range and gets NO margin even though a
        // left margin is set; Y refit and margined.
        let mut plot = Plot::new(0);
        plot.limits = (5.0, 6.0, 0.0, 0.0);
        plot.set_x_autoscale(false);
        plot.set_data_margins(DataMargins {
            x_min: 0.5,
            y_min: 0.1,
            ..Default::default()
        });
        plot.set_data_range(DataRange {
            x: Some((0.0, 10.0)),
            y: Some((0.0, 10.0)),
            y2: None,
        });
        plot.reset_zoom();
        // X pinned, no margin applied.
        assert_eq!((plot.limits.0, plot.limits.1), (5.0, 6.0));
        // Y refit with 0.1 bottom margin: ymin = 0 - 0.1*10 = -1.
        assert!(
            (plot.limits.2 - (-1.0)).abs() < 1e-9,
            "ymin={}",
            plot.limits.2
        );
    }

    #[test]
    fn data_range_is_empty_until_set() {
        let plot = Plot::new(0);
        let r = plot.data_range();
        assert_eq!(r, DataRange::default());
        assert!(r.x.is_none() && r.y.is_none() && r.y2.is_none());
    }

    #[test]
    fn reset_zoom_uses_cached_data_range() {
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 1.0, 0.0, 1.0);
        plot.set_data_range(DataRange {
            x: Some((2.0, 4.0)),
            y: Some((6.0, 8.0)),
            y2: None,
        });
        plot.reset_zoom();
        assert_eq!(plot.limits, (2.0, 4.0, 6.0, 8.0));
    }

    #[test]
    fn reset_zoom_refits_y2_independently() {
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 1.0, 0.0, 1.0);
        plot.y2 = Some((0.0, 1.0));
        plot.set_x_autoscale(false);
        plot.set_y_autoscale(false);
        plot.set_y2_autoscale(true);
        plot.reset_zoom_to_data_range(DataRange {
            x: Some((10.0, 20.0)),
            y: Some((-5.0, 5.0)),
            y2: Some((100.0, 200.0)),
        });
        // Only y2 refit.
        assert_eq!(plot.limits, (0.0, 1.0, 0.0, 1.0));
        assert_eq!(plot.y2, Some((100.0, 200.0)));
    }

    #[test]
    fn transform_y2_shares_aspect_expanded_x() {
        // With the aspect lock on, the left transform's X is expanded; the y2
        // transform must inherit that same expanded X (not the raw limits).
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        plot.keep_aspect = true;
        plot.y2 = Some((0.0, 5.0));
        let left = plot.transform(area());
        let right = plot.transform_y2(area()).expect("y2 transform");
        assert_eq!(left.x, right.x);
        // Sanity: the lock actually widened X beyond the raw [0, 10].
        assert!(left.x.min < 0.0 && left.x.max > 10.0, "{:?}", left.x);
    }
}
