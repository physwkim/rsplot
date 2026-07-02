//! The plot model.
//!
//! Holds the identifier, data-area background, data limits, margins, and the
//! optional colormap used to draw the colorbar. The item list, log/inverted
//! axis flags, and dirty tracking are added in later steps
//! (`doc/design.md` §1·§4·§11).

use egui::{Color32, Rect};

use crate::core::backend::ItemHandle;
use crate::core::colormap::Colormap;
use crate::core::dtime_ticks::TimeZone;
use crate::core::marker::Marker;
use crate::core::roi::{DEFAULT_ROI_COLOR, ManagedRoi};
use crate::core::shape::{Line, Shape};
use crate::core::transform::{Axis, AxisSide, Margins, Scale, Transform, keep_aspect_limits};
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
    /// position window is clamped.
    ///
    /// Mirrors silx `ViewConstraints` (`_utils/panzoom.py`) with
    /// `allow_scaling=True` — the only mode siplot uses, since [`apply`]
    /// runs after every pan/zoom. Two silx mechanisms are reproduced:
    ///
    /// - The `update` sanity check (panzoom.py:297-305): when `max_range`,
    ///   `min_pos` and `max_pos` are all set, the maximum span can never
    ///   exceed the `[min_pos, max_pos]` window width, so `max_range` is
    ///   capped to it.
    /// - The `normalize` position clamp (panzoom.py:337-363): when **both**
    ///   ends fall outside `[min_pos, max_pos]` the view is wider than the
    ///   window, so it snaps to exactly `[min_pos, max_pos]` (the span adapts
    ///   to fit — silx's "adaptive expansion"). When only one end is out, the
    ///   whole window shifts to pull that end back in bounds.
    ///
    /// [`apply`]: AxisConstraints::apply
    pub fn apply(self, lo: f64, hi: f64) -> (f64, f64) {
        let mut span = hi - lo;
        if span <= 0.0 {
            return (lo, hi);
        }

        // 1. Cap the max span to the position window when both bounds and a
        //    max range are set (silx ViewConstraints.update, panzoom.py:297-305).
        let mut effective_max_range = self.max_range;
        if let (Some(max_range), Some(min_pos), Some(max_pos)) =
            (self.max_range, self.min_pos, self.max_pos)
        {
            effective_max_range = Some(max_range.min(max_pos - min_pos));
        }

        // 2. Clamp the span.
        if let Some(min) = self.min_range
            && span < min
        {
            span = min;
        }
        if let Some(max) = effective_max_range
            && span > max
        {
            span = max;
        }

        // 3. Re-center the clamped span on the original midpoint.
        let mid = (lo + hi) * 0.5;
        let mut new_lo = mid - span * 0.5;
        let mut new_hi = mid + span * 0.5;

        // 4. Clamp the position window (silx ViewConstraints.normalize,
        //    panzoom.py:337-363). Both ends out -> snap to the window so the
        //    span shrinks to fit; one end out -> shift the window to pull it in.
        let below = self.min_pos.filter(|&min_pos| new_lo < min_pos);
        let above = self.max_pos.filter(|&max_pos| new_hi > max_pos);
        match (below, above) {
            (Some(min_pos), Some(max_pos)) => {
                new_lo = min_pos;
                new_hi = max_pos;
            }
            (Some(min_pos), None) => {
                let shift = min_pos - new_lo;
                new_lo += shift;
                new_hi += shift;
            }
            (None, Some(max_pos)) => {
                let shift = max_pos - new_hi;
                new_lo += shift;
                new_hi += shift;
            }
            (None, None) => {}
        }

        // 5. Final sanity — keep lo < hi even if constraints are contradictory.
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

/// Whether the X axis lays out regular numeric ticks or date-time ticks,
/// mirroring silx `items.axis.TickMode` (`items/axis.py:43-47`). In silx only
/// `XAxis` overrides `getTickMode` / `setTickMode` (`items/axis.py:391-403`),
/// backed by `setXAxisTimeSeries`; `YAxis` inherits the base
/// `Axis.setTickMode`, which raises `NotImplementedError` — there is no
/// `setYAxisTimeSeries`. So the time-series tick mode is an X-axis-only concept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TickMode {
    /// Ticks are regular numbers (silx `TickMode.DEFAULT = 0`). Zero behavior
    /// change from the pre-existing numeric tick layout.
    #[default]
    Numeric,
    /// Ticks are date-times: the axis data values are epoch seconds (UTC) and
    /// labels are formatted via [`crate::core::dtime_ticks`] (silx
    /// `TickMode.TIME_SERIES = 1`).
    TimeSeries,
}

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

/// Resolve the label to display on an axis, mirroring silx
/// `items/axis.py:187-218` (`Axis.getLabel` / `_setCurrentLabel`): the active
/// item's per-axis label is shown when one is set, otherwise it falls back to
/// the axis' own default label, otherwise an empty string.
///
/// `default_label` is the axis' own label (silx `_defaultLabel`, set via
/// `setGraphXLabel`); `active_label` is the active curve/image's label for this
/// axis (silx `getXLabel`/`getYLabel`). silx `_setActiveItem` calls
/// `_setCurrentLabel(activeLabel)`, which displays `activeLabel` when non-empty
/// and otherwise falls back to `_defaultLabel` — so the active curve's label
/// *overrides* the graph default when present. A `Some("")` is treated the same
/// as `None` (silx `_setCurrentLabel` treats an empty string as "no label").
pub fn resolved_axis_label(default_label: Option<&str>, active_label: Option<&str>) -> String {
    fn non_empty(s: Option<&str>) -> Option<&str> {
        s.filter(|l| !l.is_empty())
    }
    non_empty(active_label)
        .or(non_empty(default_label))
        .unwrap_or("")
        .to_string()
}

/// The plot's redraw-dirty state, mirroring silx `PlotWidget._dirty`
/// (`_getDirtyPlot` returns `False | "overlay" | True`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DirtyState {
    /// Nothing changed since the last replot (silx `False`).
    #[default]
    Clean,
    /// Only the overlay changed; just the overlay needs redrawing
    /// (silx `"overlay"`).
    Overlay,
    /// The full plot needs redrawing (silx `True`).
    Full,
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

/// One of the additional stacked Y axes beyond the built-in left axis
/// (`Plot::limits`) and right axis (`Plot::y2`), backing [`crate::YAxis::Extra`]
/// (`Plot::extra`). Each carries its own data range, scale, on-screen side,
/// autoscale flag, and label — the per-axis state the left/right axes spread
/// across dedicated `Plot` fields, generalized to N axes for silx-style
/// multi-axis plots. Curves bound to an axis with no `range` fall back to the
/// left transform (so they still draw), mirroring the right axis' `y2 == None`
/// fallback.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtraAxis {
    /// Data range `(min, max)`, or `None` until set explicitly or autoscaled.
    pub range: Option<(f64, f64)>,
    /// Linear or log10 scale, independent of the left/right axes.
    pub scale: Scale,
    /// Which gutter the ticks and label are drawn in; same-side extra axes stack
    /// outward in creation order.
    pub side: AxisSide,
    /// Whether this axis refits to its curves' data on reset-zoom (silx
    /// per-axis `setAutoScale`). Defaults to `true`.
    pub autoscale: bool,
    /// Axis label drawn rotated in the gutter outside the ticks, or `None`.
    pub label: Option<String>,
}

impl ExtraAxis {
    /// A linear, autoscaling extra axis on `side` with no range or label yet.
    pub fn new(side: AxisSide) -> Self {
        Self {
            range: None,
            scale: Scale::Linear,
            side,
            autoscale: true,
            label: None,
        }
    }
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
    /// Whether the built-in colorbar is drawn when a [`colormap`](Self::colormap)
    /// is present. Defaults to `true`. Composite views that render their own
    /// dedicated colorbar (e.g. `ImageView`, whose internal image plot still
    /// carries the active image's colormap) set this `false` so the colorbar is
    /// not drawn twice.
    pub show_colorbar: bool,
    /// When `true` (and a [`colormap`](Self::colormap) is shown), the colorbar is
    /// the interactive pyqtgraph-style histogram colorbar (drag the handles to set
    /// the colormap's `vmin`/`vmax`) instead of a static strip. Defaults to
    /// `false`. The drag is surfaced via `PlotResponse::colorbar_dragged_levels`
    /// for the caller to apply (the colormap and any GPU image re-upload are the
    /// caller's, mirroring the marker/ROI single-owner pattern).
    pub colorbar_interactive: bool,
    /// `(counts, edges)` value-distribution histogram drawn beside the gradient
    /// when the colorbar is interactive (see
    /// [`crate::core::histogram::compute_histogram`]); `None` draws just the
    /// gradient + handles.
    pub colorbar_histogram: Option<(Vec<u64>, Vec<f64>)>,
    /// The image value range `(min, max)` the interactive colorbar's axis spans
    /// (handles move within it). `None` falls back to the colormap's
    /// `vmin`/`vmax`.
    pub colorbar_value_range: Option<(f64, f64)>,
    /// Reserve the title gutter even when [`title`](Self::title) is `None`. Lets a
    /// side panel (e.g. an `ImageView` profile) keep its data area aligned with a
    /// reference plot that *does* carry a title, so the two data areas coincide
    /// by construction. Defaults to `false`.
    pub reserve_title_gutter: bool,
    /// Reserve the X-axis-label gutter even when [`x_label`](Self::x_label) is
    /// `None` (see [`reserve_title_gutter`](Self::reserve_title_gutter)).
    pub reserve_x_label_gutter: bool,
    /// Reserve the (left) Y-axis-label gutter even when [`y_label`](Self::y_label)
    /// is `None` (see [`reserve_title_gutter`](Self::reserve_title_gutter)).
    pub reserve_y_label_gutter: bool,
    /// Limits to restore via the Reset Zoom context-menu item. The widget
    /// captures the first observed `limits` here so the home view survives
    /// pan/zoom (`doc/design.md` §8·§11.6). `None` until the first frame.
    pub home_limits: Option<(f64, f64, f64, f64)>,
    /// Armed by a view reset (Reset Zoom / Zoom Back / reset-to-data) so residual
    /// pointer-scroll *momentum* cannot immediately re-zoom and undo the reset.
    /// The pointer sits over the data area during a right-click "Reset Zoom", and
    /// macOS trackpad/Magic-Mouse momentum keeps `smooth_scroll_delta` non-zero
    /// for ~1 s after the gesture — so without this the wheel-zoom handler re-zooms
    /// on the frames right after the menu closes. Cleared once the scroll input
    /// settles back to zero, so a fresh scroll gesture zooms normally again.
    pub(crate) reset_scroll_guard: bool,
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
    /// Additional stacked Y axes beyond the built-in left/right pair (silx-style
    /// multi-axis). `extra[i]` backs [`crate::YAxis::Extra(i)`](crate::YAxis::Extra);
    /// curves bound there plot against its range/scale and its ticks/label are
    /// drawn stacked outward in its [`AxisSide`] gutter. Empty by default.
    pub extra: Vec<ExtraAxis>,
    /// Draw a crosshair + coordinate readout following the pointer when it is
    /// over the data area (silx `setGraphCursor`, `doc/design.md` §13 C1).
    pub crosshair: bool,
    /// Regions of interest drawn over the data area with draggable edge
    /// handles, each carrying its own appearance (color, name, selection,
    /// line width/style, fill — silx `RegionOfInterest`). Dragging an edge
    /// updates that ROI's geometry in place and the widget reports the changed
    /// index (`doc/design.md` §13 C3).
    pub rois: Vec<ManagedRoi>,
    /// Default ROI outline color applied to ROIs without an explicit color
    /// override (silx `RegionOfInterestManager.getColor`/`setColor`, default
    /// red). The render resolves each ROI's color as
    /// `managed.color.unwrap_or(roi_color)`.
    pub roi_color: Color32,
    /// Index of the current/highlighted ROI, or `None` (silx
    /// `RegionOfInterestManager.getCurrentRoi`). Private so
    /// [`Self::set_current_roi`] is the sole writer of each ROI's `selected`
    /// flag, keeping "exactly the current ROI is highlighted" true by
    /// construction.
    current_roi: Option<usize>,
    /// Point / line markers drawn over the data area (silx `addMarker`). Each is
    /// a static overlay; the widget draws the list every frame.
    pub markers: Vec<Marker>,
    /// Backend item handles parallel to [`Self::markers`]: `marker_handles[i]` is
    /// the [`ItemHandle`] of `markers[i]`. Both vectors are rebuilt together by
    /// the backend's `sync_plot_items` (same length and order), so a marker drag
    /// can map the dragged mirror index back to the owning backend item for
    /// persistence. Empty until the first sync.
    ///
    /// INVARIANT: `marker_handles.len() == markers.len()` and
    /// `marker_handles[i]` identifies `markers[i]`.
    pub marker_handles: Vec<ItemHandle>,
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
    /// Active curve's X label, overriding [`Self::x_label`] while that curve is
    /// active (silx `Axis._currentLabel`, set by `_setActiveItem` from the active
    /// curve's `getXLabel`). The high-level widget repopulates this each frame
    /// from the active curve; `None` falls back to the default. See
    /// [`Self::displayed_x_label`].
    pub active_x_label: Option<String>,
    /// Active curve's left-Y label, overriding [`Self::y_label`] (silx
    /// `Axis._currentLabel`). Set only when the active curve is bound to the left
    /// Y axis. See [`Self::active_x_label`].
    pub active_y_label: Option<String>,
    /// Active curve's right (y2) label, overriding [`Self::y2_label`] (silx
    /// `Axis._currentLabel`). Set only when the active curve is bound to the right
    /// Y axis. See [`Self::active_x_label`].
    pub active_y2_label: Option<String>,
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
    /// Whether the axes (frame, ticks, labels) are displayed (silx
    /// `setAxesDisplayed` / `isAxesDisplayed`). Defaults to `true`. Wired: when
    /// `false` the widget passes `axes_hidden` to `chrome::layout`, which
    /// collapses the axis gutters to zero so the data area fills the rect
    /// (silx `setAxesDisplayed(False)` -> `setAxesMargins(0,0,0,0)`).
    axes_displayed: bool,
    /// Redraw-dirty state (silx `_dirty`). Defaults to [`DirtyState::Clean`].
    dirty: DirtyState,
    /// Whether the plot is redrawn automatically on change (silx `_autoreplot`).
    /// Defaults to `true`, matching silx after `_init`.
    autoreplot: bool,
    /// X-axis tick mode (silx `getXAxis().getTickMode`). Defaults to
    /// [`TickMode::Numeric`] (zero behavior change). When [`TickMode::TimeSeries`]
    /// the chrome formats the X tick labels as date-times. silx supports the
    /// time-series mode on the X axis only (see [`TickMode`]), so there is no
    /// Y-axis counterpart.
    x_tick_mode: TickMode,
    /// X-axis time zone used to lay out date-time ticks when
    /// [`TickMode::TimeSeries`] is active (silx `getXAxis().setTimeZone`).
    /// Defaults to [`TimeZone::Utc`], matching the previous UTC-only behavior.
    x_time_zone: TimeZone,
    /// Epoch offset added to X tick *values* before they are formatted as
    /// date-times under [`TickMode::TimeSeries`] (siplot extension, no silx
    /// counterpart). The stored X data values are interpreted as
    /// `epoch - x_time_offset`, so a caller can upload small relative
    /// coordinates — an `f32`-safe GPU vertex range — while the time ticks still
    /// read absolute wall-clock. The curve vertex path packs positions as `f32`
    /// (`render/gpu_curve.rs::pack`), so an absolute epoch (~1.7e9) would lose
    /// ~128 s of precision and a few-second strip-chart window could not render.
    /// Defaults to `0.0` (the X values *are* epoch seconds, so TimeSeries labels
    /// are unchanged from the offset-free behavior).
    x_time_offset: f64,
    /// Infinite line items drawn over the data area (silx `Line`,
    /// `items/shape.py:289`). Each is clipped to the current viewport and drawn
    /// every frame.
    lines: Vec<Line>,
    /// Whether the arrow keys pan the data area when the plot is focused (silx
    /// `setPanWithArrowKeys` / `isPanWithArrowKeys`). Defaults to `true`,
    /// matching silx `PlotWidget._panWithArrowKeys = True`. When `false` the
    /// widget ignores arrow-key presses (silx gates the same handler on
    /// `if self._panWithArrowKeys` in `PlotWidget._handleArrowKey`).
    pan_with_arrow_keys: bool,
    /// Whether a box zoom changes the X axis (silx `ZoomEnabledAxesMenu` /
    /// `Zoom.enabledAxes.xaxis`). Defaults to `true`. When `false` a box zoom
    /// keeps the current X range, matching silx `Zoom._getAxesExtent`, which
    /// replaces a disabled axis's selection extent with the full plot bounds so
    /// that axis is left unchanged.
    zoom_x_enabled: bool,
    /// Whether a box zoom changes the (left) Y axis (silx
    /// `Zoom.enabledAxes.yaxis`). Defaults to `true`. silx also tracks the right
    /// (y2) axis here, but siplot's box zoom operates on the left axes only, so
    /// there is no y2 counterpart.
    zoom_y_enabled: bool,
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
            show_colorbar: true,
            colorbar_interactive: false,
            colorbar_histogram: None,
            colorbar_value_range: None,
            reserve_title_gutter: false,
            reserve_x_label_gutter: false,
            reserve_y_label_gutter: false,
            home_limits: None,
            reset_scroll_guard: false,
            x_scale: Scale::Linear,
            y_scale: Scale::Linear,
            x_inverted: false,
            y_inverted: false,
            keep_aspect: false,
            y2: None,
            extra: Vec::new(),
            crosshair: false,
            rois: Vec::new(),
            roi_color: DEFAULT_ROI_COLOR,
            current_roi: None,
            markers: Vec::new(),
            marker_handles: Vec::new(),
            shapes: Vec::new(),
            triangles: Vec::new(),
            title: None,
            x_label: None,
            y_label: None,
            y2_label: None,
            active_x_label: None,
            active_y_label: None,
            active_y2_label: None,
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
            axes_displayed: true,
            dirty: DirtyState::Clean,
            autoreplot: true,
            x_tick_mode: TickMode::Numeric,
            x_time_zone: TimeZone::Utc,
            x_time_offset: 0.0,
            lines: Vec::new(),
            pan_with_arrow_keys: true,
            zoom_x_enabled: true,
            zoom_y_enabled: true,
        }
    }

    /// The index of the current/highlighted ROI, or `None` (silx
    /// `RegionOfInterestManager.getCurrentRoi`).
    pub fn current_roi(&self) -> Option<usize> {
        self.current_roi
    }

    /// Set the current ROI by index, or `None` to clear it (silx
    /// `RegionOfInterestManager.setCurrentRoi`): the previous current ROI loses
    /// its highlight and the new one gains it. An out-of-range index clears the
    /// selection. This is the sole writer of every ROI's `selected` flag, so the
    /// invariant "exactly the current ROI is highlighted" holds by construction.
    pub fn set_current_roi(&mut self, index: Option<usize>) {
        self.current_roi = match index {
            Some(i) if i < self.rois.len() => Some(i),
            _ => None,
        };
        self.sync_roi_selection();
    }

    /// Mirror [`Self::current_roi`] onto each ROI's `selected` flag so exactly
    /// the current ROI is highlighted (silx highlights only the current ROI).
    fn sync_roi_selection(&mut self) {
        let current = self.current_roi;
        for (i, r) in self.rois.iter_mut().enumerate() {
            r.selected = Some(i) == current;
        }
    }

    /// Remove the ROI at `index`, adjusting [`Self::current_roi`] so it keeps
    /// pointing at the same ROI (or clears when the current one is removed),
    /// then re-syncing the `selected` flags (silx
    /// `RegionOfInterestManager.removeRoi`). An out-of-range index is ignored.
    /// This is the sole removal path, so the current-ROI invariant holds across
    /// every removal (no caller pokes `rois`/`current_roi` directly).
    pub fn remove_roi(&mut self, index: usize) {
        if index >= self.rois.len() {
            return;
        }
        self.rois.remove(index);
        self.current_roi = match self.current_roi {
            Some(c) if c == index => None,
            Some(c) if c > index => Some(c - 1),
            other => other,
        };
        self.sync_roi_selection();
    }

    /// Remove every ROI and clear the current selection (silx
    /// `RegionOfInterestManager.clear`). Resetting `current_roi` to `None` keeps
    /// the invariant: no current index dangles past the emptied collection.
    pub fn clear_rois(&mut self) {
        self.rois.clear();
        self.current_roi = None;
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
            self.reset_scroll_guard = true;
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

    /// Whether the axes (frame/ticks/labels) are displayed (silx
    /// `isAxesDisplayed`).
    pub fn axes_displayed(&self) -> bool {
        self.axes_displayed
    }

    /// Show or hide the axes (silx `setAxesDisplayed`). When hidden, the widget
    /// passes `axes_hidden` to `chrome::layout`, which drops the axis gutters to
    /// zero (silx `setAxesMargins(0,0,0,0)`). Marks the plot dirty
    /// (full redraw) when the value changes, mirroring silx
    /// `setAxesDisplayed`'s `_setDirtyPlot()`.
    pub fn set_axes_displayed(&mut self, displayed: bool) {
        if displayed != self.axes_displayed {
            self.axes_displayed = displayed;
            self.set_dirty(false);
        }
    }

    /// Whether the arrow keys pan the data area when the plot is focused (silx
    /// `isPanWithArrowKeys`).
    pub fn pan_with_arrow_keys(&self) -> bool {
        self.pan_with_arrow_keys
    }

    /// Enable or disable arrow-key panning (silx `setPanWithArrowKeys`). Unlike
    /// most setters this does not mark the plot dirty: it only changes how a
    /// future key press is handled, never the current frame (silx
    /// `setPanWithArrowKeys` sets the flag without `_setDirtyPlot`).
    pub fn set_pan_with_arrow_keys(&mut self, pan: bool) {
        self.pan_with_arrow_keys = pan;
    }

    /// Whether a box zoom changes the X axis (silx `Zoom.enabledAxes.xaxis`).
    pub fn zoom_x_enabled(&self) -> bool {
        self.zoom_x_enabled
    }

    /// Whether a box zoom changes the (left) Y axis (silx
    /// `Zoom.enabledAxes.yaxis`).
    pub fn zoom_y_enabled(&self) -> bool {
        self.zoom_y_enabled
    }

    /// Choose which axes a box zoom affects (silx
    /// `PlotInteraction.setZoomEnabledAxes`). A disabled axis keeps its current
    /// range when a box zoom is applied. Like silx's setter this is a plain
    /// flag set: it does not mark the plot dirty (it only changes how a future
    /// box zoom is applied, not the current frame).
    pub fn set_zoom_enabled_axes(&mut self, x_enabled: bool, y_enabled: bool) {
        self.zoom_x_enabled = x_enabled;
        self.zoom_y_enabled = y_enabled;
    }

    /// The current redraw-dirty state (silx `_getDirtyPlot`).
    pub fn dirty(&self) -> DirtyState {
        self.dirty
    }

    /// Mark the plot as needing redraw (silx `_setDirtyPlot`). `overlay_only`
    /// requests an overlay-only redraw. The transition matches silx exactly:
    /// from [`DirtyState::Clean`] an overlay-only mark becomes
    /// [`DirtyState::Overlay`] and a full mark becomes [`DirtyState::Full`];
    /// from any already-dirty state the mark escalates to [`DirtyState::Full`]
    /// (an overlay-only mark cannot downgrade an already-pending full redraw).
    pub fn set_dirty(&mut self, overlay_only: bool) {
        self.dirty = if self.dirty == DirtyState::Clean && overlay_only {
            DirtyState::Overlay
        } else {
            DirtyState::Full
        };
    }

    /// Clear the dirty state to [`DirtyState::Clean`] (silx resets `_dirty` to
    /// `False` in `_paintContext` after drawing). Call after a redraw has been
    /// performed.
    pub fn replot(&mut self) {
        self.dirty = DirtyState::Clean;
    }

    /// Whether automatic replot is enabled (silx `getAutoReplot`).
    pub fn autoreplot(&self) -> bool {
        self.autoreplot
    }

    /// Enable or disable automatic replot (silx `setAutoReplot`). State only;
    /// the render loop that would honor this is at the widget layer (deferred).
    pub fn set_autoreplot(&mut self, autoreplot: bool) {
        self.autoreplot = autoreplot;
    }

    /// The X-axis tick mode (silx `getXAxis().getTickMode`).
    pub fn x_tick_mode(&self) -> TickMode {
        self.x_tick_mode
    }

    /// Set the X-axis tick mode (silx `getXAxis().setTickMode`). With
    /// [`TickMode::TimeSeries`] the chrome formats the X tick labels as
    /// date-times (the data values are epoch seconds, UTC).
    pub fn set_x_tick_mode(&mut self, mode: TickMode) {
        self.x_tick_mode = mode;
    }

    /// The X-axis time zone for date-time ticks (silx
    /// `getXAxis().getTimeZone`).
    pub fn x_time_zone(&self) -> TimeZone {
        self.x_time_zone
    }

    /// Set the X-axis time zone for date-time ticks (silx
    /// `getXAxis().setTimeZone`). Only affects layout while the X tick mode is
    /// [`TickMode::TimeSeries`]; the data values stay epoch seconds (UTC) and
    /// the ticks are laid out in this zone's wall-clock calendar.
    pub fn set_x_time_zone(&mut self, tz: TimeZone) {
        self.x_time_zone = tz;
    }

    /// The X-axis epoch offset applied to date-time tick labels (siplot
    /// extension; see the `x_time_offset` field docs).
    pub fn x_time_offset(&self) -> f64 {
        self.x_time_offset
    }

    /// Set the epoch offset added to X tick values before [`TickMode::TimeSeries`]
    /// formatting, so a caller feeding relative (`f32`-safe) X coordinates still
    /// gets absolute wall-clock tick labels. The stored X values are interpreted
    /// as `epoch - offset`; `0.0` (the default) means the X values already are
    /// epoch seconds. No effect outside the TimeSeries tick mode.
    pub fn set_x_time_offset(&mut self, offset: f64) {
        self.x_time_offset = offset;
    }

    /// Append an infinite line item (silx `addItem` of a `Line`). The widget
    /// clips each line to the current viewport and draws it every frame.
    pub fn add_line(&mut self, line: Line) {
        self.lines.push(line);
    }

    /// The infinite line items (silx `Line` items).
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// Mutable access to the infinite line items.
    pub fn lines_mut(&mut self) -> &mut Vec<Line> {
        &mut self.lines
    }

    /// The X-axis label to display, given the active curve's X label (silx
    /// `Axis.getLabel`). The active curve's `active_label` overrides the default
    /// [`Self::x_label`] when set, otherwise the default shows, otherwise empty.
    /// See [`resolved_axis_label`].
    pub fn x_axis_label(&self, active_label: Option<&str>) -> String {
        resolved_axis_label(self.x_label.as_deref(), active_label)
    }

    /// The left-Y-axis label to display, given the active curve's Y label (silx
    /// `Axis.getLabel`). See [`Self::x_axis_label`].
    pub fn y_axis_label(&self, active_label: Option<&str>) -> String {
        resolved_axis_label(self.y_label.as_deref(), active_label)
    }

    /// The right (y2) axis label to display, given the active curve's y2 label
    /// (silx `Axis.getLabel`). See [`Self::x_axis_label`].
    pub fn y2_axis_label(&self, active_label: Option<&str>) -> String {
        resolved_axis_label(self.y2_label.as_deref(), active_label)
    }

    /// The X-axis label actually drawn this frame: the active curve's X label
    /// ([`Self::active_x_label`], set by the widget from the active curve)
    /// overriding the graph default [`Self::x_label`], or `None` when neither is
    /// set (silx `Axis._currentLabel`). `None` means nothing is drawn.
    pub fn displayed_x_label(&self) -> Option<String> {
        let label = self.x_axis_label(self.active_x_label.as_deref());
        (!label.is_empty()).then_some(label)
    }

    /// The left-Y-axis label actually drawn this frame (silx `Axis._currentLabel`).
    /// See [`Self::displayed_x_label`].
    pub fn displayed_y_label(&self) -> Option<String> {
        let label = self.y_axis_label(self.active_y_label.as_deref());
        (!label.is_empty()).then_some(label)
    }

    /// The right (y2) axis label actually drawn this frame (silx
    /// `Axis._currentLabel`). See [`Self::displayed_x_label`].
    pub fn displayed_y2_label(&self) -> Option<String> {
        let label = self.y2_axis_label(self.active_y2_label.as_deref());
        (!label.is_empty()).then_some(label)
    }

    /// Whether the title gutter must be reserved this frame: a title is present,
    /// or [`reserve_title_gutter`](Self::reserve_title_gutter) forces it (e.g. an
    /// `ImageView` profile aligning to a titled reference plot). Single source of
    /// truth for the chrome request bool, so the three gutters stay parallel.
    pub fn needs_title_gutter(&self) -> bool {
        self.title.is_some() || self.reserve_title_gutter
    }

    /// Whether the X-axis-label gutter must be reserved this frame. See
    /// [`needs_title_gutter`](Self::needs_title_gutter).
    pub fn needs_x_label_gutter(&self) -> bool {
        self.x_label.is_some() || self.reserve_x_label_gutter
    }

    /// Whether the (left) Y-axis-label gutter must be reserved this frame. See
    /// [`needs_title_gutter`](Self::needs_title_gutter).
    pub fn needs_y_label_gutter(&self) -> bool {
        self.y_label.is_some() || self.reserve_y_label_gutter
    }

    /// The explicit grid-line color override (silx `getGridColor`). `None` means
    /// the grid follows the foreground color; see [`Self::effective_grid_color`].
    pub fn grid_color(&self) -> Option<Color32> {
        self.grid_color
    }

    /// Set (or clear, with `None`) the grid-line color override (silx
    /// `setGridColor`). Marks the plot dirty on change, mirroring silx's
    /// `_foregroundColorsUpdated` -> `_setDirtyPlot()`. Wired: the widget passes
    /// this through `chrome::Theme::with_overrides(foreground, grid_color)`
    /// (`plot_widget.rs`), so the grid renders with this color independently of
    /// the foreground (axis/frame) color; see [`Self::effective_grid_color`].
    pub fn set_grid_color(&mut self, color: Option<Color32>) {
        if self.grid_color != color {
            self.grid_color = color;
            self.set_dirty(false);
        }
    }

    /// Resolve the color the grid lines should use given the resolved
    /// `foreground` color, mirroring silx `_foregroundColorsUpdated`: the
    /// explicit [`Self::grid_color`] when set, otherwise `foreground`.
    pub fn effective_grid_color(&self, foreground: Color32) -> Color32 {
        self.grid_color.unwrap_or(foreground)
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
        self.reset_scroll_guard = true;
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

    /// Append an extra Y axis on `side` (range/label unset, linear, autoscaling)
    /// and return its index, usable as [`crate::YAxis::Extra(index)`](crate::YAxis::Extra).
    pub fn add_extra_axis(&mut self, side: AxisSide) -> usize {
        self.extra.push(ExtraAxis::new(side));
        self.extra.len() - 1
    }

    /// The extra axes in creation order (silx-style multi-axis).
    pub fn extra_axes(&self) -> &[ExtraAxis] {
        &self.extra
    }

    /// Shared read access to extra axis `index`, or `None` when unknown.
    pub fn extra_axis(&self, index: usize) -> Option<&ExtraAxis> {
        self.extra.get(index)
    }

    /// Mutable access to extra axis `index`, or `None` when unknown.
    pub fn extra_axis_mut(&mut self, index: usize) -> Option<&mut ExtraAxis> {
        self.extra.get_mut(index)
    }

    /// Build the transform for extra axis `index`, sharing the left transform's
    /// X axis exactly (including any aspect expansion) so curves on every axis
    /// stay aligned in X, and using the extra axis' own range and scale as Y.
    /// `None` when the index is unknown or the axis has no range yet (the caller
    /// then falls back to the left transform, matching [`Self::transform_y2`]).
    pub fn transform_extra(&self, index: usize, area: Rect) -> Option<Transform> {
        let ax = self.extra.get(index)?;
        let (min, max) = ax.range?;
        let left = self.transform(area);
        let y = Axis {
            min,
            max,
            scale: ax.scale,
            inverted: false,
        };
        Some(Transform::with_axes(left.x, y, area))
    }

    /// Refit each autoscale-on extra axis to its data bounds, the multi-axis
    /// sibling of the left/right refit in [`Self::reset_zoom_to_data_range`].
    /// `data[i]` is extra axis `i`'s data bounds; a missing or `None` entry
    /// leaves that axis' range unchanged. Refit axes are expanded by the Y data
    /// margins (reusing the y2 margin/log rules), and a log axis whose lower
    /// bound is non-positive is force-refit so toggling it to log re-fits to
    /// positive data (matching the y2 branch of `reset_zoom_to_data_range`).
    pub fn reset_extra_axes_to(&mut self, data: &[Option<(f64, f64)>]) {
        let m = self.data_margins;
        for (i, ax) in self.extra.iter_mut().enumerate() {
            let is_log = ax.scale == Scale::Log10;
            let log_force = is_log && ax.range.map(|(lo, _)| lo <= 0.0).unwrap_or(true);
            if !(ax.autoscale || log_force) {
                continue;
            }
            if let Some(Some((lo, hi))) = data.get(i).copied() {
                ax.range = Some(DataMargins::expand_axis(lo, hi, m.y_min, m.y_max, is_log));
            }
        }
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
    fn axis_constraints_view_wider_than_window_snaps_to_window() {
        let c = AxisConstraints {
            min_pos: Some(0.0),
            max_pos: Some(10.0),
            ..Default::default()
        };
        // View [-5, 15] (span 20) is wider than the [0, 10] window with both
        // ends out; silx normalize snaps it to exactly the window (span 10).
        let (lo, hi) = c.apply(-5.0, 15.0);
        assert!((lo - 0.0).abs() < 1e-10, "lo={lo}");
        assert!((hi - 10.0).abs() < 1e-10, "hi={hi}");
    }

    #[test]
    fn axis_constraints_max_range_capped_to_window_keeps_offcenter_in_bounds() {
        let c = AxisConstraints {
            min_pos: Some(0.0),
            max_pos: Some(10.0),
            max_range: Some(100.0),
            ..Default::default()
        };
        // max_range=100 is capped to the 10-wide window (silx update sanity),
        // so the span-20 off-center view [2, 22] shrinks to the window and
        // shifts fully in bounds instead of overshooting the far edge.
        let (lo, hi) = c.apply(2.0, 22.0);
        assert!((lo - 0.0).abs() < 1e-10, "lo={lo}");
        assert!((hi - 10.0).abs() < 1e-10, "hi={hi}");
    }

    #[test]
    fn axis_constraints_one_end_out_shifts_within_both_bounds() {
        let c = AxisConstraints {
            min_pos: Some(0.0),
            max_pos: Some(10.0),
            ..Default::default()
        };
        // View [-2, 3] (span 5) fits the window; only the low end is out, so
        // the window shifts right to [0, 5] without touching the span.
        let (lo, hi) = c.apply(-2.0, 3.0);
        assert!((lo - 0.0).abs() < 1e-10, "lo={lo}");
        assert!((hi - 5.0).abs() < 1e-10, "hi={hi}");
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
    fn view_resets_arm_scroll_guard() {
        // Invariant: every model-level view reset arms `reset_scroll_guard` so
        // residual pointer-scroll momentum cannot immediately re-zoom the
        // restored view. Boundaries: a fresh plot is unarmed; reset-to-data arms;
        // a successful (history non-empty) zoom_back arms; an empty-history
        // zoom_back restores nothing and so leaves the flag untouched.
        let mut plot = Plot::new(0);
        assert!(!plot.reset_scroll_guard, "fresh plot is unarmed");

        plot.reset_zoom_to_data_range(DataRange {
            x: Some((0.0, 1.0)),
            y: Some((0.0, 1.0)),
            y2: None,
        });
        assert!(plot.reset_scroll_guard, "reset-to-data arms the guard");

        // A successful zoom_back re-arms after an explicit disarm.
        plot.reset_scroll_guard = false;
        plot.limits = (0.0, 1.0, 0.0, 1.0);
        plot.push_limits();
        plot.limits = (2.0, 3.0, 2.0, 3.0);
        assert!(plot.zoom_back(), "restored a pushed view");
        assert!(plot.reset_scroll_guard, "zoom_back arms the guard");

        // Empty-history zoom_back restores nothing: it must not arm.
        plot.reset_scroll_guard = false;
        assert!(!plot.zoom_back(), "empty history restores nothing");
        assert!(
            !plot.reset_scroll_guard,
            "a no-op zoom_back leaves the guard as-is"
        );
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
    fn grid_color_defaults_none_and_follows_foreground() {
        let plot = Plot::new(0);
        assert_eq!(plot.grid_color(), None);
        let fg = Color32::from_rgb(200, 200, 200);
        // No explicit grid color -> effective is the foreground.
        assert_eq!(plot.effective_grid_color(fg), fg);
    }

    #[test]
    fn grid_color_explicit_overrides_foreground() {
        let mut plot = Plot::new(0);
        let grid = Color32::from_rgb(64, 64, 64);
        let fg = Color32::from_rgb(200, 200, 200);
        plot.set_grid_color(Some(grid));
        assert_eq!(plot.grid_color(), Some(grid));
        // Explicit grid color wins over foreground.
        assert_eq!(plot.effective_grid_color(fg), grid);
    }

    #[test]
    fn set_grid_color_change_marks_full_dirty() {
        let mut plot = Plot::new(0);
        // Setting to the same value (None) is a no-op for dirty.
        plot.set_grid_color(None);
        assert_eq!(plot.dirty(), DirtyState::Clean);
        // A real change marks dirty (silx _foregroundColorsUpdated).
        plot.set_grid_color(Some(Color32::RED));
        assert_eq!(plot.dirty(), DirtyState::Full);
    }

    #[test]
    fn axis_label_active_curve_wins_over_default() {
        // silx _setActiveItem: the active curve's label overrides the graph
        // default (_setCurrentLabel displays the active label when non-empty).
        assert_eq!(
            resolved_axis_label(Some("Energy"), Some("curve X")),
            "curve X"
        );
    }

    #[test]
    fn axis_label_falls_back_to_default_when_no_active() {
        // No active curve label -> the axis' own default label.
        assert_eq!(resolved_axis_label(Some("Energy"), None), "Energy");
        // Active label only -> active label.
        assert_eq!(resolved_axis_label(None, Some("curve X")), "curve X");
    }

    #[test]
    fn axis_label_empty_when_neither_set() {
        assert_eq!(resolved_axis_label(None, None), "");
    }

    #[test]
    fn axis_label_empty_active_falls_back_to_default() {
        // silx _setCurrentLabel treats "" as no label -> falls back to default.
        assert_eq!(resolved_axis_label(Some("Energy"), Some("")), "Energy");
        // Active label wins over a default even when the default is set.
        assert_eq!(resolved_axis_label(Some("Energy"), Some("Time")), "Time");
        // Both empty / unset -> empty.
        assert_eq!(resolved_axis_label(Some(""), Some("")), "");
        assert_eq!(resolved_axis_label(None, Some("")), "");
    }

    #[test]
    fn plot_axis_label_active_overrides_default() {
        let mut plot = Plot::new(0);
        plot.x_label = Some("X axis".to_string());
        // Active curve label overrides the explicit default (silx semantics).
        assert_eq!(plot.x_axis_label(Some("curve")), "curve");
        // Default shows when there is no active label.
        assert_eq!(plot.x_axis_label(None), "X axis");
        // No default on y -> active curve label.
        assert_eq!(plot.y_axis_label(Some("intensity")), "intensity");
        // No default, no active -> empty.
        assert_eq!(plot.y2_axis_label(None), "");
    }

    #[test]
    fn displayed_labels_resolve_active_override_against_default() {
        let mut plot = Plot::new(0);
        // Defaults set, no active override -> defaults are displayed.
        plot.x_label = Some("Energy".to_string());
        plot.y_label = Some("Counts".to_string());
        assert_eq!(plot.displayed_x_label().as_deref(), Some("Energy"));
        assert_eq!(plot.displayed_y_label().as_deref(), Some("Counts"));
        // y2 has neither default nor override -> nothing drawn.
        assert_eq!(plot.displayed_y2_label(), None);

        // Active overrides win over the defaults (silx _setActiveItem).
        plot.active_x_label = Some("Time".to_string());
        plot.active_y_label = Some("Intensity".to_string());
        assert_eq!(plot.displayed_x_label().as_deref(), Some("Time"));
        assert_eq!(plot.displayed_y_label().as_deref(), Some("Intensity"));

        // An empty override falls back to the default; an active y2 override with
        // no y2 default still drives the y2 label.
        plot.active_x_label = Some(String::new());
        plot.active_y2_label = Some("Right".to_string());
        assert_eq!(plot.displayed_x_label().as_deref(), Some("Energy"));
        assert_eq!(plot.displayed_y2_label().as_deref(), Some("Right"));
    }

    #[test]
    fn dirty_defaults_clean_and_autoreplot_on_and_axes_displayed() {
        let plot = Plot::new(0);
        assert_eq!(plot.dirty(), DirtyState::Clean);
        assert!(plot.autoreplot());
        assert!(plot.axes_displayed());
    }

    #[test]
    fn dirty_clean_overlay_only_becomes_overlay() {
        let mut plot = Plot::new(0);
        plot.set_dirty(true);
        assert_eq!(plot.dirty(), DirtyState::Overlay);
    }

    #[test]
    fn dirty_clean_full_becomes_full() {
        let mut plot = Plot::new(0);
        plot.set_dirty(false);
        assert_eq!(plot.dirty(), DirtyState::Full);
    }

    #[test]
    fn dirty_overlay_then_overlay_only_escalates_to_full() {
        // silx: once dirty, even an overlay-only mark sets _dirty = True.
        let mut plot = Plot::new(0);
        plot.set_dirty(true);
        assert_eq!(plot.dirty(), DirtyState::Overlay);
        plot.set_dirty(true);
        assert_eq!(plot.dirty(), DirtyState::Full);
    }

    #[test]
    fn dirty_full_then_overlay_only_stays_full() {
        let mut plot = Plot::new(0);
        plot.set_dirty(false);
        plot.set_dirty(true);
        assert_eq!(plot.dirty(), DirtyState::Full);
    }

    #[test]
    fn replot_clears_dirty_to_clean() {
        let mut plot = Plot::new(0);
        plot.set_dirty(false);
        assert_eq!(plot.dirty(), DirtyState::Full);
        plot.replot();
        assert_eq!(plot.dirty(), DirtyState::Clean);
    }

    #[test]
    fn set_axes_displayed_change_marks_full_dirty() {
        let mut plot = Plot::new(0);
        // No change -> no dirty.
        plot.set_axes_displayed(true);
        assert_eq!(plot.dirty(), DirtyState::Clean);
        // Change -> full dirty.
        plot.set_axes_displayed(false);
        assert!(!plot.axes_displayed());
        assert_eq!(plot.dirty(), DirtyState::Full);
    }

    #[test]
    fn pan_with_arrow_keys_defaults_true_and_set_does_not_dirty() {
        let mut plot = Plot::new(0);
        // silx PlotWidget._panWithArrowKeys = True.
        assert!(plot.pan_with_arrow_keys(), "default enabled");

        // setPanWithArrowKeys is a plain flag set: it never marks the plot dirty
        // (it only changes how a future key press is handled, not the frame).
        plot.set_pan_with_arrow_keys(false);
        assert!(!plot.pan_with_arrow_keys());
        assert_eq!(plot.dirty(), DirtyState::Clean, "toggling must not dirty");

        plot.set_pan_with_arrow_keys(true);
        assert!(plot.pan_with_arrow_keys());
        assert_eq!(plot.dirty(), DirtyState::Clean);
    }

    #[test]
    fn zoom_enabled_axes_default_true_and_set_does_not_dirty() {
        let mut plot = Plot::new(0);
        // silx ZoomEnabledAxesMenu: all axes checked by default.
        assert!(plot.zoom_x_enabled());
        assert!(plot.zoom_y_enabled());

        // setZoomEnabledAxes is a plain flag set (it only changes how a future
        // box zoom applies), so it must not mark the plot dirty.
        plot.set_zoom_enabled_axes(true, false);
        assert!(plot.zoom_x_enabled());
        assert!(!plot.zoom_y_enabled());
        assert_eq!(plot.dirty(), DirtyState::Clean);

        plot.set_zoom_enabled_axes(false, true);
        assert!(!plot.zoom_x_enabled());
        assert!(plot.zoom_y_enabled());
        assert_eq!(plot.dirty(), DirtyState::Clean);
    }

    #[test]
    fn lines_start_empty_and_append() {
        let mut plot = Plot::new(0);
        assert!(plot.lines().is_empty());
        plot.add_line(Line::new(f64::INFINITY, 3.0));
        plot.add_line(Line::new(0.0, 1.0));
        assert_eq!(plot.lines().len(), 2);
        // lines_mut allows in-place edits.
        plot.lines_mut()[1].intercept = 2.0;
        assert_eq!(plot.lines()[1].intercept, 2.0);
        assert!(!plot.lines()[0].slope.is_finite());
    }

    #[test]
    fn tick_mode_defaults_numeric_and_sets_x_only() {
        let mut plot = Plot::new(0);
        assert_eq!(plot.x_tick_mode(), TickMode::Numeric);
        plot.set_x_tick_mode(TickMode::TimeSeries);
        assert_eq!(plot.x_tick_mode(), TickMode::TimeSeries);
        plot.set_x_tick_mode(TickMode::Numeric);
        assert_eq!(plot.x_tick_mode(), TickMode::Numeric);
    }

    #[test]
    fn time_zone_defaults_utc_and_round_trips() {
        let mut plot = Plot::new(0);
        assert_eq!(plot.x_time_zone(), TimeZone::Utc);
        let jst = TimeZone::FixedOffset {
            seconds_east: 32400,
        };
        plot.set_x_time_zone(jst);
        assert_eq!(plot.x_time_zone(), jst);
        plot.set_x_time_zone(TimeZone::Utc);
        assert_eq!(plot.x_time_zone(), TimeZone::Utc);
    }

    #[test]
    fn set_autoreplot_toggles() {
        let mut plot = Plot::new(0);
        plot.set_autoreplot(false);
        assert!(!plot.autoreplot());
        plot.set_autoreplot(true);
        assert!(plot.autoreplot());
    }

    #[test]
    fn show_colorbar_defaults_true() {
        // Backward compat: every plot draws its colorbar when it has a colormap
        // unless a caller (e.g. ImageView's internal image plot) opts out.
        let mut plot = Plot::new(0);
        assert!(plot.show_colorbar);
        plot.show_colorbar = false;
        assert!(!plot.show_colorbar);
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

    // --- extra (stacked) Y axes ---

    #[test]
    fn add_extra_axis_returns_sequential_indices_and_keeps_side() {
        let mut plot = Plot::new(0);
        assert_eq!(plot.add_extra_axis(AxisSide::Right), 0);
        assert_eq!(plot.add_extra_axis(AxisSide::Left), 1);
        assert_eq!(plot.extra_axes().len(), 2);
        assert_eq!(plot.extra_axis(0).unwrap().side, AxisSide::Right);
        assert_eq!(plot.extra_axis(1).unwrap().side, AxisSide::Left);
        // A fresh extra axis has no range, is linear, and autoscales.
        let ax = plot.extra_axis(0).unwrap();
        assert_eq!(ax.range, None);
        assert_eq!(ax.scale, Scale::Linear);
        assert!(ax.autoscale);
        assert!(plot.extra_axis(2).is_none());
    }

    #[test]
    fn transform_extra_shares_left_x_and_maps_its_own_y() {
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 1.0);
        let i = plot.add_extra_axis(AxisSide::Right);
        // No range yet -> no transform (caller falls back to the left axis).
        assert!(plot.transform_extra(i, area()).is_none());
        plot.extra_axis_mut(i).unwrap().range = Some((-1.0, 1.0));
        let left = plot.transform(area());
        let extra = plot.transform_extra(i, area()).expect("extra transform");
        // Shares the left X axis exactly; maps its own Y range with the low value
        // at the bottom edge and the high value at the top edge.
        assert_eq!(left.x, extra.x);
        let bottom = extra.data_to_pixel(0.0, -1.0);
        let top = extra.data_to_pixel(0.0, 1.0);
        assert!((bottom.y - area().bottom()).abs() < 1e-3, "{bottom:?}");
        assert!((top.y - area().top()).abs() < 1e-3, "{top:?}");
        // An unknown index yields no transform.
        assert!(plot.transform_extra(99, area()).is_none());
    }

    #[test]
    fn transform_extra_honors_its_own_log_scale() {
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 1.0);
        let i = plot.add_extra_axis(AxisSide::Right);
        {
            let ax = plot.extra_axis_mut(i).unwrap();
            ax.range = Some((1.0, 1000.0));
            ax.scale = Scale::Log10;
        }
        let t = plot.transform_extra(i, area()).expect("extra transform");
        // Three decades: 10 lands one-third up from the bottom edge.
        let mid = t.data_to_pixel(0.0, 10.0).y;
        let expected = area().bottom() + (area().top() - area().bottom()) / 3.0;
        assert!((mid - expected).abs() < 1e-2, "{mid} vs {expected}");
    }

    #[test]
    fn reset_extra_axes_to_refits_only_autoscale_on_axes() {
        let mut plot = Plot::new(0);
        let a = plot.add_extra_axis(AxisSide::Right); // autoscale on (default)
        let b = plot.add_extra_axis(AxisSide::Left);
        plot.extra_axis_mut(a).unwrap().range = Some((0.0, 1.0));
        plot.extra_axis_mut(b).unwrap().range = Some((0.0, 1.0));
        plot.extra_axis_mut(b).unwrap().autoscale = false;
        plot.reset_extra_axes_to(&[Some((100.0, 200.0)), Some((-9.0, 9.0))]);
        // a refit to its data; b pinned (autoscale off).
        assert_eq!(plot.extra_axis(a).unwrap().range, Some((100.0, 200.0)));
        assert_eq!(plot.extra_axis(b).unwrap().range, Some((0.0, 1.0)));
    }

    #[test]
    fn reset_extra_axes_to_leaves_axis_with_no_data_unchanged() {
        let mut plot = Plot::new(0);
        let a = plot.add_extra_axis(AxisSide::Right);
        plot.extra_axis_mut(a).unwrap().range = Some((0.0, 1.0));
        // No data for this axis (empty slice / None entry) -> unchanged.
        plot.reset_extra_axes_to(&[None]);
        assert_eq!(plot.extra_axis(a).unwrap().range, Some((0.0, 1.0)));
        plot.reset_extra_axes_to(&[]);
        assert_eq!(plot.extra_axis(a).unwrap().range, Some((0.0, 1.0)));
    }

    #[test]
    fn reset_extra_axes_to_force_refits_nonpositive_log_axis() {
        let mut plot = Plot::new(0);
        let a = plot.add_extra_axis(AxisSide::Right);
        {
            let ax = plot.extra_axis_mut(a).unwrap();
            ax.scale = Scale::Log10;
            ax.autoscale = false; // even pinned, a log axis with lo<=0 refits
            ax.range = Some((-5.0, 5.0));
        }
        plot.reset_extra_axes_to(&[Some((1.0, 1000.0))]);
        assert_eq!(plot.extra_axis(a).unwrap().range, Some((1.0, 1000.0)));
    }

    // --- current-ROI selection invariant (Plot is the single owner) ---

    fn point_roi(i: usize) -> ManagedRoi {
        ManagedRoi::new(crate::core::roi::Roi::Point {
            x: i as f64,
            y: 0.0,
        })
    }

    #[test]
    fn roi_color_defaults_to_silx_red() {
        assert_eq!(Plot::new(0).roi_color, Color32::RED);
    }

    #[test]
    fn set_current_roi_highlights_exactly_one() {
        let mut plot = Plot::new(0);
        plot.rois = (0..3).map(point_roi).collect();

        plot.set_current_roi(Some(1));
        assert_eq!(plot.current_roi(), Some(1));
        assert!(!plot.rois[0].selected);
        assert!(plot.rois[1].selected);
        assert!(!plot.rois[2].selected);

        // Switching the current ROI moves the single highlight.
        plot.set_current_roi(Some(2));
        assert!(!plot.rois[1].selected);
        assert!(plot.rois[2].selected);

        // Clearing removes every highlight.
        plot.set_current_roi(None);
        assert_eq!(plot.current_roi(), None);
        assert!(plot.rois.iter().all(|r| !r.selected));
    }

    #[test]
    fn set_current_roi_out_of_range_clears_selection() {
        let mut plot = Plot::new(0);
        plot.rois = vec![point_roi(0)];
        plot.set_current_roi(Some(1));
        assert_eq!(plot.current_roi(), None);
        assert!(!plot.rois[0].selected);
    }

    #[test]
    fn remove_roi_adjusts_current_index() {
        let mut plot = Plot::new(0);
        plot.rois = (0..3).map(point_roi).collect();

        // Current after the removed index shifts down by one.
        plot.set_current_roi(Some(2));
        plot.remove_roi(0);
        assert_eq!(plot.current_roi(), Some(1));
        assert!(plot.rois[1].selected);

        // Removing the current ROI clears the selection.
        plot.set_current_roi(Some(1));
        plot.remove_roi(1);
        assert_eq!(plot.current_roi(), None);
        assert!(plot.rois.iter().all(|r| !r.selected));

        // Current before the removed index is unaffected.
        plot.rois = (0..3).map(point_roi).collect();
        plot.set_current_roi(Some(0));
        plot.remove_roi(2);
        assert_eq!(plot.current_roi(), Some(0));
        assert!(plot.rois[0].selected);
    }

    #[test]
    fn clear_rois_resets_current() {
        let mut plot = Plot::new(0);
        plot.rois = (0..3).map(point_roi).collect();
        plot.set_current_roi(Some(1));
        plot.clear_rois();
        assert_eq!(plot.current_roi(), None);
        assert!(plot.rois.is_empty());
    }

    #[test]
    fn needs_gutter_defaults_false_without_label_or_reserve() {
        let plot = Plot::new(0);
        assert!(!plot.needs_title_gutter());
        assert!(!plot.needs_x_label_gutter());
        assert!(!plot.needs_y_label_gutter());
    }

    #[test]
    fn needs_gutter_true_when_label_present() {
        let mut plot = Plot::new(0);
        plot.title = Some("T".into());
        plot.x_label = Some("X".into());
        plot.y_label = Some("Y".into());
        assert!(plot.needs_title_gutter());
        assert!(plot.needs_x_label_gutter());
        assert!(plot.needs_y_label_gutter());
    }

    #[test]
    fn needs_gutter_true_when_reserve_flag_set_without_label() {
        // The ImageView-profile path: labels cleared (no text drawn), gutter
        // still reserved so the data area aligns with the reference plot.
        let mut plot = Plot::new(0);
        assert!(plot.title.is_none() && plot.x_label.is_none() && plot.y_label.is_none());
        plot.reserve_title_gutter = true;
        plot.reserve_x_label_gutter = true;
        plot.reserve_y_label_gutter = true;
        assert!(plot.needs_title_gutter());
        assert!(plot.needs_x_label_gutter());
        assert!(plot.needs_y_label_gutter());
        // Reservation must not synthesise label text.
        assert!(plot.displayed_x_label().is_none());
        assert!(plot.displayed_y_label().is_none());
    }
}
