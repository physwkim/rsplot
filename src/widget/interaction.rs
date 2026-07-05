//! Interaction math: pure functions mapping pointer input to new data limits.
//!
//! The widget reads egui input, converts it through the *current* on-screen
//! [`Transform`], and applies one of these to
//! produce the next limits. Because everything downstream (the wgpu ortho matrix
//! and the egui chrome) derives from those limits, the image, curve, and axes
//! move together with no extra bookkeeping (`doc/design.md` §4·§8·§11.6).
//!
//! Pointer-mode mapping lives in the widget; this module is just the geometry
//! for pan/zoom/pick math, kept pure so it is unit-testable.

use egui::{Pos2, Rect, Vec2};

use crate::core::marker::{Marker, MarkerConstraint, MarkerKind};
use crate::core::roi::{HandleKind, ManagedRoi, Roi, RoiEdge, RoiHandle, RoiInteractionMode};
use crate::core::transform::{Scale, Transform};
// The float32-safe window and the silx `checkAxisLimits` repair live in the
// core transform module (they are shared by the core reset-zoom refit and the
// widget gesture commits); re-exported here for the existing gesture-side API.
pub use crate::core::transform::{
    FLOAT32_MINPOS, FLOAT32_SAFE_MAX, FLOAT32_SAFE_MIN, clamp_axis_limits,
};

/// Data limits `(x_min, x_max, y_min, y_max)`.
pub type Limits = (f64, f64, f64, f64);

/// Translate a single axis range by a screen-space drag of `delta_px` pixels
/// across an axis of `extent_px` pixels, mirroring silx `Pan.drag`
/// (`PlotInteraction.py`). For a [`Scale::Log10`] axis the shift is applied in
/// log10 space; for [`Scale::Linear`] it is a plain offset.
///
/// `delta_px` is the pixel delta that should be *subtracted* from the range (the
/// data point under the pointer follows the cursor). Returns the new
/// `(min, max)`; on a log axis with a non-positive `min` or an out-of-range
/// result the original range is kept (silx reverts in those cases).
fn pan_axis(min: f64, max: f64, delta_px: f64, extent_px: f64, scale: Scale) -> (f64, f64) {
    match scale {
        Scale::Log10 if min > 0.0 && max > 0.0 => {
            let log_min = min.log10();
            let log_max = max.log10();
            // Per-pixel log10 delta across the axis (the data-to-pixel mapping is
            // linear in log space), matching silx `dx = log10(xData) - log10(lastX)`.
            let d_log = delta_px * (log_max - log_min) / extent_px;
            let new_min = 10f64.powf(log_min - d_log);
            let new_max = 10f64.powf(log_max - d_log);
            // silx keeps the axis only while both bounds stay in positive float32.
            if new_min < FLOAT32_MINPOS || new_max > FLOAT32_SAFE_MAX {
                (min, max)
            } else {
                (new_min, new_max)
            }
        }
        _ => {
            let offset = delta_px * (max - min) / extent_px;
            let new_min = min - offset;
            let new_max = max - offset;
            if new_min < FLOAT32_SAFE_MIN || new_max > FLOAT32_SAFE_MAX {
                (min, max)
            } else {
                (new_min, new_max)
            }
        }
    }
}

/// Translate `limits` by a screen-space drag delta (pixels) so the data point
/// under the pointer stays under the pointer (the content follows the cursor),
/// mirroring silx `Pan.drag` (`PlotInteraction.py`).
///
/// Screen `+x` is right and `+y` is down; the Y axis is flipped (data `y_max` at
/// the top), so a downward drag increases the data Y limits. `x_scale` /
/// `y_scale` select linear vs. log10 translation per axis.
pub fn pan(limits: Limits, area: Rect, delta_px: Vec2, x_scale: Scale, y_scale: Scale) -> Limits {
    let (x_min, x_max, y_min, y_max) = limits;
    let w = area.width().max(1.0) as f64;
    let h = area.height().max(1.0) as f64;
    // X: a rightward drag (+delta_px.x) shifts the view left.
    let (new_x_min, new_x_max) = pan_axis(x_min, x_max, delta_px.x as f64, w, x_scale);
    // Y is flipped: a downward drag (+delta_px.y) shifts the view up, so the
    // subtracted pixel delta is negated relative to the X convention.
    let (new_y_min, new_y_max) = pan_axis(y_min, y_max, -(delta_px.y as f64), h, y_scale);
    (new_x_min, new_x_max, new_y_min, new_y_max)
}

/// Scale a 1D range about an invariant `center` by `scale`, mirroring silx
/// `scale1DRange` (`_utils/panzoom.py`). `scale < 1` zooms out (widens the
/// span); `scale > 1` zooms in. On a log axis the operation is performed in
/// log10 space and the result is clipped to the positive float32 range; on a
/// linear axis it is clipped to the float32 range. A degenerate (`min == max`)
/// range is returned unchanged.
///
/// Note silx's `scale` is the multiplicative *zoom factor* (`range / scale`),
/// the reciprocal of the per-axis shrink ratio used by [`zoom_about`].
fn scale1d_range(min: f64, max: f64, center: f64, scale: f64, is_log: bool) -> (f64, f64) {
    let (mut min, mut center, mut max) = (min, center, max);
    if is_log {
        // Min and center can be <= 0 when autoscale is off and the axis switched
        // to log; silx substitutes FLOAT32_MINPOS in that case.
        min = if min > 0.0 {
            min.log10()
        } else {
            FLOAT32_MINPOS
        };
        center = if center > 0.0 {
            center.log10()
        } else {
            FLOAT32_MINPOS
        };
        max = if max > 0.0 {
            max.log10()
        } else {
            FLOAT32_MINPOS
        };
    }

    if min == max {
        return (min, max);
    }

    let offset = (center - min) / (max - min);
    let range = (max - min) / scale;
    let mut new_min = center - offset * range;
    let mut new_max = center + (1.0 - offset) * range;

    if is_log {
        new_min = 10f64.powf(new_min).clamp(FLOAT32_MINPOS, FLOAT32_SAFE_MAX);
        new_max = 10f64.powf(new_max).clamp(FLOAT32_MINPOS, FLOAT32_SAFE_MAX);
    } else {
        new_min = new_min.clamp(FLOAT32_SAFE_MIN, FLOAT32_SAFE_MAX);
        new_max = new_max.clamp(FLOAT32_SAFE_MIN, FLOAT32_SAFE_MAX);
    }
    (new_min, new_max)
}

/// Scale `limits` about a fixed data point `(cx, cy)`, mirroring silx
/// `applyZoomToPlot` (`_utils/panzoom.py`). `factor < 1` zooms in (shrinks the
/// span); `factor > 1` zooms out. The point `(cx, cy)` keeps its screen
/// position. `x_scale` / `y_scale` select log10 vs. linear scaling per axis.
///
/// silx `scale1DRange` divides the span by its `scale`, so to shrink the span by
/// `factor` here the silx scale is `1 / factor`.
pub fn zoom_about(
    limits: Limits,
    factor: f64,
    cx: f64,
    cy: f64,
    x_scale: Scale,
    y_scale: Scale,
) -> Limits {
    let (x_min, x_max, y_min, y_max) = limits;
    // silx `scale` is the reciprocal of our span-shrink `factor`.
    let silx_scale = 1.0 / factor;
    let (new_x_min, new_x_max) =
        scale1d_range(x_min, x_max, cx, silx_scale, x_scale == Scale::Log10);
    let (new_y_min, new_y_max) =
        scale1d_range(y_min, y_max, cy, silx_scale, y_scale == Scale::Log10);
    (new_x_min, new_x_max, new_y_min, new_y_max)
}

/// A full gesture view snapshot: the left/X `limits` plus the optional right
/// (y2) axis range. This is the unit pan/zoom gestures operate on, mirroring
/// the axes silx moves together (`Pan.drag` and `applyZoomToPlot` update x, y,
/// and y2 in the same call, `PlotInteraction.py:260-335`,
/// `_utils/panzoom.py:132-176`).
pub type ViewLimits = (Limits, Option<(f64, f64)>);

/// Translate the whole view — left axes *and* the right (y2) axis — by a
/// screen-space drag delta, mirroring silx `Pan.drag`
/// (`PlotInteraction.py:260-335`), which computes its own y2 delta via the
/// right-axis pixel mapping (`pixelToData(x, y, axis="right")`) and shifts
/// `y2Min/y2Max` in the same gesture.
///
/// The y2 shift applies the same pixel delta across the same axis extent; for
/// the linear right axis this is exactly silx's `dy2 = y2Data - lastY2`. The
/// left-Y `y_scale` drives the log math for y2 as well, matching silx (the
/// right axis shares the left axis' scale there) and the existing arrow-pan /
/// toolbar-zoom precedent (`plot_widget::arrow_pan`,
/// `actions::control::apply_zoom`).
pub fn pan_view(
    view: ViewLimits,
    area: Rect,
    delta_px: Vec2,
    x_scale: Scale,
    y_scale: Scale,
) -> ViewLimits {
    let (limits, y2) = view;
    let next = pan(limits, area, delta_px, x_scale, y_scale);
    let h = area.height().max(1.0) as f64;
    // Same flipped-Y convention as `pan`: a downward drag shifts the view up.
    let next_y2 = y2.map(|(lo, hi)| pan_axis(lo, hi, -(delta_px.y as f64), h, y_scale));
    (next, next_y2)
}

/// Scale the whole view — left axes *and* the right (y2) axis — about a fixed
/// data point, mirroring silx `applyZoomToPlot` (`_utils/panzoom.py:132-176`):
/// x and left-y scale about `(cx, cy)`, and y2 scales about `cy2`, the zoom
/// centre pixel mapped through the *right-axis* transform (silx
/// `plot.pixelToData(cx, cy, axis="right")`). The left-Y `y_scale` drives the
/// y2 log flag exactly as silx passes `plot.getYAxis()._isLogarithmic()` for
/// the right axis too. A `None` `cy2` (no right-axis transform this frame)
/// leaves y2 unchanged.
///
/// `enabled = (x, y)` mirrors silx's `EnabledAxes` gating inside
/// `applyZoomToPlot` (`if enabled.xaxis: ...`): a disabled axis keeps its
/// current range. The plot model tracks no separate y2 zoom flag (recorded
/// scope decision on `Plot::zoom_y_enabled`), so silx's `y2axis` flag folds
/// into the `y` flag here. `centre` is the left-axes data centre `(cx, cy)`.
pub fn zoom_view_about(
    view: ViewLimits,
    factor: f64,
    centre: (f64, f64),
    cy2: Option<f64>,
    x_scale: Scale,
    y_scale: Scale,
    enabled: (bool, bool),
) -> ViewLimits {
    let ((x_min, x_max, y_min, y_max), y2) = view;
    let (cx, cy) = centre;
    let (x_enabled, y_enabled) = enabled;
    // silx `scale` is the reciprocal of our span-shrink `factor` (see
    // [`zoom_about`]).
    let silx_scale = 1.0 / factor;
    let (nx0, nx1) = if x_enabled {
        scale1d_range(x_min, x_max, cx, silx_scale, x_scale == Scale::Log10)
    } else {
        (x_min, x_max)
    };
    let (ny0, ny1) = if y_enabled {
        scale1d_range(y_min, y_max, cy, silx_scale, y_scale == Scale::Log10)
    } else {
        (y_min, y_max)
    };
    let next_y2 = match (y2, cy2) {
        (Some((lo, hi)), Some(c)) if y_enabled => Some(scale1d_range(
            lo,
            hi,
            c,
            silx_scale,
            y_scale == Scale::Log10,
        )),
        (y2, _) => y2,
    };
    ((nx0, nx1, ny0, ny1), next_y2)
}

/// Pan a single axis range by `pan_factor` (a signed proportion of the range),
/// mirroring silx `applyPan` (`_utils/panzoom.py`). This is the arrow-key /
/// programmatic pan path (distinct from the mouse-drag [`pan`]). For a log axis
/// with a positive `min` the offset is applied in log10 space; otherwise it is a
/// linear offset. Out-of-range results are discarded (the original range is
/// kept), matching silx.
pub fn apply_pan(min: f64, max: f64, pan_factor: f64, is_log10: bool) -> (f64, f64) {
    if is_log10 && min > 0.0 {
        // Negative range with log scale can happen via other backends; skip it.
        let log_min = min.log10();
        let log_max = max.log10();
        let log_offset = pan_factor * (log_max - log_min);
        let new_min = 10f64.powf(log_min + log_offset);
        let new_max = 10f64.powf(log_max + log_offset);
        if new_min > 0.0 && new_max.is_finite() {
            (new_min, new_max)
        } else {
            (min, max)
        }
    } else {
        let offset = pan_factor * (max - min);
        let new_min = min + offset;
        let new_max = max + offset;
        if new_min > f64::NEG_INFINITY && new_max < f64::INFINITY {
            (new_min, new_max)
        } else {
            (min, max)
        }
    }
}

/// A pan direction for [`apply_pan`]-based arrow-key panning, mirroring silx
/// `PlotWidget.pan` directions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanDirection {
    Up,
    Down,
    Left,
    Right,
}

/// Limits covering the data-space box defined by two corners, in any order.
pub fn box_zoom(ax: f64, ay: f64, bx: f64, by: f64) -> Limits {
    (ax.min(bx), ax.max(bx), ay.min(by), ay.max(by))
}

/// Constrain a box-zoom result to the axes enabled for zoom (silx
/// `ZoomEnabledAxesMenu` / `Zoom._getAxesExtent`). A disabled axis keeps its
/// `current` range while an enabled axis takes the `zoomed` range.
///
/// silx achieves this in pixel space — it replaces a disabled axis's selection
/// extent with the full plot bounds before `pixelToData`, which yields the
/// current displayed range for that axis. This is the equivalent substitution
/// in data space: pass the currently displayed `current` limits and the box
/// `zoomed` limits. With both axes enabled the result is `zoomed` unchanged.
/// Pure and deterministic, so the axis gating is unit-testable without a GPU
/// backend.
pub fn constrain_zoom_axes(
    zoomed: Limits,
    current: Limits,
    x_enabled: bool,
    y_enabled: bool,
) -> Limits {
    let (zx0, zx1, zy0, zy1) = zoomed;
    let (cx0, cx1, cy0, cy1) = current;
    let (x0, x1) = if x_enabled { (zx0, zx1) } else { (cx0, cx1) };
    let (y0, y1) = if y_enabled { (zy0, zy1) } else { (cy0, cy1) };
    (x0, x1, y0, y1)
}

/// One discrete wheel notch's worth of `smooth_scroll_delta.y`, in egui
/// points: egui 0.34 maps a `MouseWheelUnit::Line` event of ±1.0 through
/// `InputOptions::line_scroll_speed` (native default 40.0 points; the web
/// backend uses a smaller per-event value but fires proportionally more
/// events), and its scroll smoothing dribbles the delta out sum-conservingly
/// over a few frames — so one notch totals exactly 40.0 points regardless of
/// how many frames it spans (`egui/src/input_state/wheel_state.rs`).
const WHEEL_NOTCH_POINTS: f64 = 40.0;

/// Convert an egui wheel delta (`smooth_scroll_delta.y`, points) to a zoom
/// factor for [`zoom_about`]. Scrolling up (`> 0`) zooms in (`factor < 1`).
///
/// Calibrated to silx `_onWheel` (`PlotInteraction.py:1912-1913`), where one
/// wheel step is exactly ×1.1: the exponent is `ln(1.1) / 40.0` per point, so
/// a full notch (40.0 points, [`WHEEL_NOTCH_POINTS`]) gives a span factor of
/// exactly `1/1.1` and N notches compose to `1.1^N` — the exponential form
/// keeps the calibration exact even when egui's smoothing splits a notch
/// across frames, and lets smooth trackpad deltas zoom continuously at the
/// same per-distance rate (a deliberate egui-ism; silx quantizes to whole
/// steps).
pub fn wheel_zoom_factor(scroll_y: f32) -> f64 {
    (-(scroll_y as f64) * (1.1f64.ln() / WHEEL_NOTCH_POINTS)).exp()
}

/// Whether `limits` are non-degenerate (both spans strictly positive). The
/// widget keeps the previous limits when a candidate fails this.
pub fn is_valid(limits: Limits) -> bool {
    let (x_min, x_max, y_min, y_max) = limits;
    x_max > x_min && y_max > y_min
}

/// Clamp both axes of `limits` into the float32-safe window via
/// [`clamp_axis_limits`], mirroring silx applying `checkAxisLimits` per axis
/// after pan/zoom (`PlotInteraction.py:241-250`, panzoom.py). `x_log` / `y_log`
/// select the log lower bound per axis. Applied after every pan and zoom so an
/// extreme gesture cannot push a bound past the float32-safe range.
pub fn clamp_limits(limits: Limits, x_log: bool, y_log: bool) -> Limits {
    let (x_min, x_max, y_min, y_max) = limits;
    let (nx0, nx1) = clamp_axis_limits(x_min, x_max, x_log);
    let (ny0, ny1) = clamp_axis_limits(y_min, y_max, y_log);
    (nx0, nx1, ny0, ny1)
}

// Draw-mode state machine ####################################################

/// Which shape an interactive draw session produces, mirroring silx's draw-mode
/// state machines (`PlotInteraction.py`): [`SelectRectangle`], [`SelectEllipse`],
/// [`SelectLine`], [`SelectHLine`], [`SelectVLine`], [`SelectPolygon`], and the
/// freehand pencil ([`DrawFreeHand`] / [`SelectFreeLine`]).
///
/// [`SelectRectangle`]: # "PlotInteraction.py:767"
/// [`SelectEllipse`]: # "PlotInteraction.py:681"
/// [`SelectLine`]: # "PlotInteraction.py:809"
/// [`SelectHLine`]: # "PlotInteraction.py:885"
/// [`SelectVLine`]: # "PlotInteraction.py:920"
/// [`SelectPolygon`]: # "PlotInteraction.py:485"
/// [`DrawFreeHand`]: # "PlotInteraction.py:955"
/// [`SelectFreeLine`]: # "PlotInteraction.py:1051"
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawMode {
    /// Two-point axis-aligned rectangle drag (silx `SelectRectangle`).
    Rectangle,
    /// Two-point drag producing an ellipse (silx `SelectEllipse`); the press is
    /// the center and the drag end a point on the ellipse.
    Ellipse,
    /// Two-point line segment drag (silx `SelectLine`).
    Line,
    /// One-point horizontal line at a captured Y (silx `SelectHLine`).
    HLine,
    /// One-point vertical line at a captured X (silx `SelectVLine`).
    VLine,
    /// Point-by-point polygon, closed by clicking near the first vertex (silx
    /// `SelectPolygon`).
    Polygon,
    /// Continuous freehand polyline accumulated while dragging (silx
    /// `DrawFreeHand` / `SelectFreeLine`).
    FreeHand,
    /// Single-click point capture (silx `_plotShape = "point"`, used by
    /// `PointROI`/`CrossROI` whose `setFirstShapePoints` takes `points[0]`,
    /// `items/roi.py:89`/`:176`). One press captures the data position and
    /// finishes the draw immediately — no drag/release is needed.
    Point,
}

/// A pointer sample fed to [`DrawState`]: the data-space position plus the
/// pixel-space position. The pixel position is needed for the polygon's
/// snap-to-first-point pixel threshold (silx `SelectPolygon`); the data position
/// is what the produced shape stores.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawInput {
    /// Data-space `(x, y)` under the cursor.
    pub data: (f64, f64),
    /// Pixel-space `(x, y)` of the cursor.
    pub pixel: (f32, f32),
}

impl DrawInput {
    /// Build a sample from a cursor pixel and the display [`Transform`],
    /// projecting the pixel to data space (the widget's per-event conversion).
    pub fn from_pixel(transform: &Transform, pixel: Pos2) -> Self {
        Self {
            data: transform.pixel_to_data(pixel),
            pixel: (pixel.x, pixel.y),
        }
    }
}

/// Parameters of a finished draw, mirroring the `points` / `parameters` payload
/// of silx's `prepareDrawingSignal` (`PlotEvents.py:34-55`) per shape type. All
/// coordinates are data-space.
#[derive(Clone, Debug, PartialEq)]
pub enum DrawParams {
    /// Axis-aligned rectangle, as silx `prepareDrawingSignal("rectangle", ...)`
    /// derives it: the lower-left `(x, y)` corner plus `width`/`height`
    /// (`PlotEvents.py:49-53`).
    Rectangle {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    /// Ellipse from silx `SelectEllipse`: a `center` plus the semi-axes
    /// `(width, height)` from center to the bounding box
    /// (`PlotInteraction.py:688-746`).
    Ellipse {
        center: (f64, f64),
        /// Semi-axis lengths `(a, b)` from center to bounding box.
        semi_axes: (f64, f64),
    },
    /// Line segment between two endpoints (silx `SelectLine`).
    Line { start: (f64, f64), end: (f64, f64) },
    /// Horizontal line at data `y` (silx `SelectHLine` captures the row; the
    /// widget extends it across the plot bounds for display).
    HLine { y: f64 },
    /// Vertical line at data `x` (silx `SelectVLine`).
    VLine { x: f64 },
    /// Closed polygon vertices (silx `SelectPolygon`). silx duplicates the first
    /// vertex as the last on close; this stores the open vertex ring without the
    /// duplicate so each vertex appears once.
    Polygon { vertices: Vec<(f64, f64)> },
    /// Freehand polyline vertices (silx `DrawFreeHand` / `SelectFreeLine`).
    FreeHand { vertices: Vec<(f64, f64)> },
    /// A single captured point (silx `_plotShape = "point"`): the data-space
    /// position of the click.
    Point { x: f64, y: f64 },
}

/// An event emitted by [`DrawState`], mirroring silx's `drawingProgress` /
/// `drawingFinished` signals (`prepareDrawingSignal`, `PlotEvents.py:34-55`).
#[derive(Clone, Debug, PartialEq)]
pub enum DrawEvent {
    /// The in-progress preview shape (silx `"drawingProgress"`). `points` are the
    /// data-space vertices of the current rubber-band, suitable for overlay
    /// drawing. For an ellipse these are the sampled circle-preview vertices.
    InProgress {
        mode: DrawMode,
        points: Vec<(f64, f64)>,
    },
    /// The draw completed (silx `"drawingFinished"`), carrying the resolved
    /// [`DrawParams`].
    Finished { mode: DrawMode, params: DrawParams },
}

/// Default polygon close / first-point snap threshold in pixels, mirroring silx
/// `SelectPolygon.DRAG_THRESHOLD_DIST` (`PlotInteraction.py:488`).
pub const DRAW_CLOSE_THRESHOLD_PX: f32 = 4.0;

/// Number of preview vertices silx samples for the ellipse/circle rubber band
/// (`PlotInteraction.py:729`).
const ELLIPSE_PREVIEW_POINTS: usize = 27;

/// Internal phase of a two-/one-point or polygon/freehand draw, kept private so
/// the only public surface is [`DrawState`]'s event API.
#[derive(Clone, Debug, PartialEq)]
enum Phase {
    /// No active draw.
    Idle,
    /// A two-point draw in progress: `start` captured, dragging to the end.
    TwoPoint { start: DrawInput },
    /// A one-point draw in progress (hline/vline): tracking the current point.
    OnePoint,
    /// A polygon in progress: `first` is the anchor (for the close test) and
    /// `points` is the committed vertex ring whose last entry tracks the cursor.
    /// Each vertex keeps its pixel position so the close / near-previous tests
    /// run in pixel space exactly as silx does.
    Polygon {
        first: DrawInput,
        points: Vec<DrawInput>,
    },
    /// A freehand draw in progress: accumulated data-space vertices.
    FreeHand { points: Vec<(f64, f64)> },
}

/// A pure draw-mode state machine over data-space coordinates, mirroring silx's
/// `Select*` / `DrawFreeHand` interactions (`PlotInteraction.py:485-1110`).
///
/// The widget feeds it pointer press / move / release events (already projected
/// to [`DrawInput`]); it returns an optional [`DrawEvent`] (`InProgress` preview
/// or `Finished` result) without touching any GPU state, so it is fully
/// unit-testable. The current preview vertices are also available via
/// [`DrawState::preview`] for overlay drawing between events.
#[derive(Clone, Debug)]
pub struct DrawState {
    mode: DrawMode,
    phase: Phase,
    close_threshold_px: f32,
}

impl DrawState {
    /// A fresh idle state for `mode`, using the default close threshold.
    pub fn new(mode: DrawMode) -> Self {
        Self {
            mode,
            phase: Phase::Idle,
            close_threshold_px: DRAW_CLOSE_THRESHOLD_PX,
        }
    }

    /// Override the polygon close / first-point snap threshold (pixels).
    pub fn with_close_threshold(mut self, px: f32) -> Self {
        self.close_threshold_px = px;
        self
    }

    /// The polygon close / first-point snap threshold in pixels. Used to size
    /// the on-plot first-point close target (silx `updateFirstPoint`).
    pub fn close_threshold_px(&self) -> f32 {
        self.close_threshold_px
    }

    /// The active draw mode.
    pub fn mode(&self) -> DrawMode {
        self.mode
    }

    /// Whether a draw is currently in progress (a press has started a shape that
    /// has not finished).
    pub fn is_active(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    /// The current preview vertices (data space) for overlay drawing, or `None`
    /// when idle. Mirrors the rubber-band silx keeps via `setSelectionArea`.
    pub fn preview(&self) -> Option<Vec<(f64, f64)>> {
        match &self.phase {
            Phase::Idle => None,
            Phase::TwoPoint { .. } | Phase::OnePoint => None,
            Phase::Polygon { points, .. } => Some(points.iter().map(|p| p.data).collect()),
            Phase::FreeHand { points } => Some(points.clone()),
        }
    }

    /// Handle a pointer *press* (left-button down). For two-/one-point and
    /// freehand modes this begins the draw; for polygon mode it begins the
    /// polygon on the first press and is a no-op on later presses (vertices are
    /// added on release, mirroring silx `SelectPolygon`).
    pub fn on_press(&mut self, input: DrawInput) -> Option<DrawEvent> {
        match self.mode {
            DrawMode::Rectangle | DrawMode::Ellipse | DrawMode::Line => {
                self.phase = Phase::TwoPoint { start: input };
                None
            }
            DrawMode::HLine | DrawMode::VLine => {
                self.phase = Phase::OnePoint;
                Some(self.one_point_progress(input))
            }
            DrawMode::Polygon => {
                if matches!(self.phase, Phase::Idle) {
                    // First press anchors the polygon (silx enterState seeds
                    // points with [firstPos, firstPos]).
                    self.phase = Phase::Polygon {
                        first: input,
                        points: vec![input, input],
                    };
                    Some(self.polygon_progress())
                } else {
                    None
                }
            }
            DrawMode::FreeHand => {
                // silx SelectFreeLine seeds the first vertex on press (beginDrag).
                self.phase = Phase::FreeHand {
                    points: vec![input.data],
                };
                Some(self.freehand_progress())
            }
            DrawMode::Point => {
                // silx `_plotShape = "point"`: a single click finishes at once.
                // The phase stays Idle (no in-progress preview), so move/release
                // are no-ops and the next press starts a fresh point.
                Some(DrawEvent::Finished {
                    mode: DrawMode::Point,
                    params: DrawParams::Point {
                        x: input.data.0,
                        y: input.data.1,
                    },
                })
            }
        }
    }

    /// Handle a pointer *move*. Emits an `InProgress` preview while a draw is
    /// active, or `None` when idle.
    pub fn on_move(&mut self, input: DrawInput) -> Option<DrawEvent> {
        match self.mode {
            DrawMode::Rectangle | DrawMode::Ellipse | DrawMode::Line => match &self.phase {
                Phase::TwoPoint { start } => Some(self.two_point_progress(*start, input)),
                _ => None,
            },
            DrawMode::HLine | DrawMode::VLine => match self.phase {
                Phase::OnePoint => Some(self.one_point_progress(input)),
                _ => None,
            },
            DrawMode::Polygon => {
                if let Phase::Polygon { first, points } = &mut self.phase {
                    // Snap the tracked last vertex to the first point when the
                    // cursor is within the close threshold (silx onMove,
                    // PlotInteraction.py:593-604).
                    let snapped = if Self::within_threshold(
                        first.pixel,
                        input.pixel,
                        self.close_threshold_px,
                    ) {
                        *first
                    } else {
                        input
                    };
                    if let Some(last) = points.last_mut() {
                        *last = snapped;
                    }
                    Some(self.polygon_progress())
                } else {
                    None
                }
            }
            DrawMode::FreeHand => {
                if let Phase::FreeHand { points } = &mut self.phase {
                    // Accumulate, skipping a repeated identical point (silx
                    // SelectFreeLine._processEvent isNewPoint check).
                    if points.last() != Some(&input.data) {
                        points.push(input.data);
                    }
                    Some(self.freehand_progress())
                } else {
                    None
                }
            }
            // Point finishes on press; a move is a no-op.
            DrawMode::Point => None,
        }
    }

    /// Handle a pointer *release* (left-button up). Two-/one-point and freehand
    /// modes finish here; polygon mode appends a vertex (or closes if released
    /// near the first point with more than two vertices), mirroring silx
    /// `SelectPolygon.onRelease`.
    pub fn on_release(&mut self, input: DrawInput) -> Option<DrawEvent> {
        match self.mode {
            DrawMode::Rectangle | DrawMode::Ellipse | DrawMode::Line => {
                match std::mem::replace(&mut self.phase, Phase::Idle) {
                    Phase::TwoPoint { start } => Some(self.two_point_finished(start, input)),
                    other => {
                        self.phase = other;
                        None
                    }
                }
            }
            DrawMode::HLine | DrawMode::VLine => {
                if matches!(self.phase, Phase::OnePoint) {
                    self.phase = Phase::Idle;
                    Some(self.one_point_finished(input))
                } else {
                    None
                }
            }
            DrawMode::Polygon => self.polygon_on_release(input),
            DrawMode::FreeHand => {
                if let Phase::FreeHand { points } = &mut self.phase {
                    if points.last() != Some(&input.data) {
                        points.push(input.data);
                    }
                    let vertices = std::mem::take(points);
                    self.phase = Phase::Idle;
                    Some(DrawEvent::Finished {
                        mode: DrawMode::FreeHand,
                        params: DrawParams::FreeHand { vertices },
                    })
                } else {
                    None
                }
            }
            // Point finished on press; a release is a no-op.
            DrawMode::Point => None,
        }
    }

    /// Cancel any in-progress draw, returning to idle. Mirrors silx `cancel` /
    /// `cancelSelect` (drops the rubber band without a finished event).
    pub fn cancel(&mut self) {
        self.phase = Phase::Idle;
    }

    /// Programmatically finish the in-progress polygon at its committed
    /// vertices, regardless of the cursor position — silx
    /// `SelectPolygon._validate`, triggered by silx
    /// `ClosePolygonInteractionAction`. Returns the `Finished` event when a real
    /// polygon is in progress (the seeded pair plus at least one appended
    /// vertex, the same `len > 2` gate as the snap-close path), leaving the state
    /// idle; returns `None` and leaves the state untouched otherwise (not in
    /// polygon mode, idle, or too few vertices to close).
    pub fn validate(&mut self) -> Option<DrawEvent> {
        if self.mode != DrawMode::Polygon {
            return None;
        }
        match &self.phase {
            Phase::Polygon { points, .. } if points.len() > 2 => Some(self.close_polygon()),
            _ => None,
        }
    }

    // --- internal helpers -------------------------------------------------

    fn within_threshold(a: (f32, f32), b: (f32, f32), threshold: f32) -> bool {
        // silx tests dx <= threshold AND dy <= threshold (axis-wise box, not a
        // radial distance), PlotInteraction.py:560-565.
        (a.0 - b.0).abs() <= threshold && (a.1 - b.1).abs() <= threshold
    }

    fn two_point_progress(&self, start: DrawInput, cur: DrawInput) -> DrawEvent {
        DrawEvent::InProgress {
            mode: self.mode,
            points: self.two_point_preview(start.data, cur.data),
        }
    }

    fn two_point_finished(&self, start: DrawInput, end: DrawInput) -> DrawEvent {
        let params = match self.mode {
            DrawMode::Rectangle => {
                let (sx, sy) = start.data;
                let (ex, ey) = end.data;
                let x = sx.min(ex);
                let y = sy.min(ey);
                DrawParams::Rectangle {
                    x,
                    y,
                    width: sx.max(ex) - x,
                    height: sy.max(ey) - y,
                }
            }
            DrawMode::Ellipse => {
                let semi_axes = ellipse_semi_axes(start.data, end.data);
                DrawParams::Ellipse {
                    center: start.data,
                    semi_axes,
                }
            }
            DrawMode::Line => DrawParams::Line {
                start: start.data,
                end: end.data,
            },
            _ => unreachable!("two_point_finished only for rectangle/ellipse/line"),
        };
        DrawEvent::Finished {
            mode: self.mode,
            params,
        }
    }

    fn two_point_preview(&self, start: (f64, f64), cur: (f64, f64)) -> Vec<(f64, f64)> {
        match self.mode {
            DrawMode::Rectangle => {
                // silx four corners: start, (start.x, cur.y), cur, (cur.x, start.y).
                vec![start, (start.0, cur.1), cur, (cur.0, start.1)]
            }
            DrawMode::Line => vec![start, cur],
            DrawMode::Ellipse => {
                let (a, b) = ellipse_semi_axes(start, cur);
                ellipse_preview(start, a, b)
            }
            _ => unreachable!("two_point_preview only for rectangle/ellipse/line"),
        }
    }

    fn one_point_progress(&self, input: DrawInput) -> DrawEvent {
        DrawEvent::InProgress {
            mode: self.mode,
            // The pure machine has no plot bounds; the preview point is just the
            // captured coordinate. The widget extends it across the data area.
            points: vec![input.data],
        }
    }

    fn one_point_finished(&self, input: DrawInput) -> DrawEvent {
        let params = match self.mode {
            DrawMode::HLine => DrawParams::HLine { y: input.data.1 },
            DrawMode::VLine => DrawParams::VLine { x: input.data.0 },
            _ => unreachable!("one_point_finished only for hline/vline"),
        };
        DrawEvent::Finished {
            mode: self.mode,
            params,
        }
    }

    fn polygon_progress(&self) -> DrawEvent {
        let points = match &self.phase {
            Phase::Polygon { points, .. } => points.iter().map(|p| p.data).collect(),
            _ => Vec::new(),
        };
        DrawEvent::InProgress {
            mode: DrawMode::Polygon,
            points,
        }
    }

    fn polygon_on_release(&mut self, input: DrawInput) -> Option<DrawEvent> {
        let Phase::Polygon { first, points } = &mut self.phase else {
            return None;
        };
        // Close when there is a real polygon (silx requires len > 2, i.e. the
        // seeded pair plus at least one appended vertex) and the release is near
        // the first point (PlotInteraction.py:565).
        let close = points.len() > 2
            && Self::within_threshold(first.pixel, input.pixel, self.close_threshold_px);
        if close {
            return Some(self.close_polygon());
        }

        // Compare the release pixel to the *previous* committed vertex's pixel
        // (points[-2]); append only if it is far enough, else replace the tracked
        // last vertex (silx PlotInteraction.py:581-588).
        let prev = points.get(points.len().wrapping_sub(2)).map(|p| p.pixel);
        let near_prev = prev
            .map(|pp| Self::within_threshold(pp, input.pixel, self.close_threshold_px))
            .unwrap_or(false);
        if let Some(last) = points.last_mut() {
            *last = input;
        }
        if !near_prev {
            points.push(input);
        }
        Some(self.polygon_progress())
    }

    fn close_polygon(&mut self) -> DrawEvent {
        let vertices = match &mut self.phase {
            Phase::Polygon { points, .. } => {
                let mut v: Vec<(f64, f64)> = points.iter().map(|p| p.data).collect();
                // The tracked last vertex is the cursor; drop it so only the
                // committed ring remains (silx sets points[-1] = points[0] then
                // emits; we drop the cursor tail and keep the open ring without a
                // duplicated first vertex).
                v.pop();
                v
            }
            _ => Vec::new(),
        };
        self.phase = Phase::Idle;
        DrawEvent::Finished {
            mode: DrawMode::Polygon,
            params: DrawParams::Polygon { vertices },
        }
    }

    fn freehand_progress(&self) -> DrawEvent {
        let points = match &self.phase {
            Phase::FreeHand { points } => points.clone(),
            _ => Vec::new(),
        };
        DrawEvent::InProgress {
            mode: DrawMode::FreeHand,
            points,
        }
    }
}

/// How a selection / draw-mode area is filled, mirroring silx
/// `setSelectionArea(fill=...)` (`PlotInteraction.py:98-141`): `'hatch'`,
/// `'solid'`, or `'none'`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FillMode {
    /// Diagonal hatch fill (silx `fill="hatch"`), the default for closed
    /// selection areas (rectangle/ellipse/polygon).
    #[default]
    Hatch,
    /// Solid fill (silx `fill="solid"`).
    Solid,
    /// No fill, outline only (silx `fill="none"`), used for the freehand
    /// polyline and the polygon first-point marker.
    None,
}

/// Style of an in-progress selection / draw-mode overlay, mirroring the
/// parameters silx passes to `setSelectionArea` (`PlotInteraction.py:98-141`):
/// a [`FillMode`] and an RGBA color. silx draws the outline dashed
/// (`linestyle="--"`); the widget honors that when painting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectionStyle {
    /// How the area is filled.
    pub fill: FillMode,
    /// The outline / fill color.
    pub color: egui::Color32,
}

impl Default for SelectionStyle {
    fn default() -> Self {
        // silx default selection color is a translucent black; the widget can
        // override per draw session.
        Self {
            fill: FillMode::Hatch,
            color: egui::Color32::from_rgba_unmultiplied(0, 0, 0, 128),
        }
    }
}

impl SelectionStyle {
    /// A style with the given fill and color.
    pub fn new(fill: FillMode, color: egui::Color32) -> Self {
        Self { fill, color }
    }
}

/// Diagonal (45°) hatch line endpoints covering `rect`, spaced `spacing` pixels
/// apart, mirroring the visual of silx's `fill="hatch"`
/// (`PlotInteraction.py:98-141`). Each returned pair `(a, b)` is a line segment
/// (in `rect`'s coordinate space) clipped to the rectangle. Pure so the line
/// layout is unit-testable without a painter. A non-positive `spacing` or
/// degenerate `rect` yields no lines.
pub fn hatch_lines(rect: Rect, spacing: f32) -> Vec<(Pos2, Pos2)> {
    if spacing <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    // Lines of slope +1 (going down-right): x - y = c. c ranges so the line
    // crosses the rect. For a line x = y + c, it intersects the rect when
    // c ∈ [left - bottom, right - top].
    let (left, right, top, bottom) = (rect.left(), rect.right(), rect.top(), rect.bottom());
    // Start c at the first multiple of spacing at or below the min, so the
    // pattern is stable regardless of rect offset.
    let c_min = left - bottom;
    let c_max = right - top;
    let mut c = (c_min / spacing).floor() * spacing;
    while c <= c_max {
        // Clip the infinite line x = y + c to the rect, collecting entry/exit.
        let mut pts: Vec<Pos2> = Vec::new();
        // Intersection with the four edges; keep those within the rect.
        // Top edge y = top: x = top + c.
        let xt = top + c;
        if xt >= left && xt <= right {
            pts.push(egui::pos2(xt, top));
        }
        // Bottom edge y = bottom: x = bottom + c.
        let xb = bottom + c;
        if xb >= left && xb <= right {
            pts.push(egui::pos2(xb, bottom));
        }
        // Left edge x = left: y = left - c.
        let yl = left - c;
        if yl >= top && yl <= bottom {
            pts.push(egui::pos2(left, yl));
        }
        // Right edge x = right: y = right - c.
        let yr = right - c;
        if yr >= top && yr <= bottom {
            pts.push(egui::pos2(right, yr));
        }
        if pts.len() >= 2 {
            lines.push((pts[0], pts[1]));
        }
        c += spacing;
    }
    lines
}

/// Semi-axes `(a, b)` of the ellipse centered at `center` passing through
/// `point`, mirroring silx `SelectEllipse._getEllipseSize`
/// (`PlotInteraction.py:688-721`). `a`/`b` are the lengths from the center to
/// the bounding box along X/Y. A degenerate point (zero X or Y offset) returns
/// the raw offsets, matching silx's early return.
pub fn ellipse_semi_axes(center: (f64, f64), point: (f64, f64)) -> (f64, f64) {
    let mut x = (center.0 - point.0).abs();
    let mut y = (center.1 - point.1).abs();
    if x == 0.0 || y == 0.0 {
        return (x, y);
    }
    // The eccentricity of the ellipse defined by a=x, b=y is the one we search.
    let swap = x < y;
    if swap {
        std::mem::swap(&mut x, &mut y);
    }
    let e = (x * x - y * y).sqrt() / x;
    // a^2 = x^2 + y^2 / (1 - e^2); b = a * sqrt(1 - e^2).
    let a = (x * x + y * y / (1.0 - e * e)).sqrt();
    let b = a * (1.0 - e * e).sqrt();
    if swap { (b, a) } else { (a, b) }
}

/// Sampled vertices of the ellipse preview centered at `center` with semi-axes
/// `(a, b)`, mirroring silx's [`ELLIPSE_PREVIEW_POINTS`]-point circle sampling
/// (`PlotInteraction.py:729-734`).
fn ellipse_preview(center: (f64, f64), a: f64, b: f64) -> Vec<(f64, f64)> {
    let n = ELLIPSE_PREVIEW_POINTS;
    (0..n)
        .map(|i| {
            let angle = i as f64 * std::f64::consts::TAU / n as f64;
            (center.0 + angle.cos() * a, center.1 + angle.sin() * b)
        })
        .collect()
}

/// Mouse-cursor shape for a draggable plot handle, mirroring silx's
/// `CURSOR_SIZE_HOR` / `CURSOR_SIZE_VER` / `CURSOR_SIZE_ALL` / `CURSOR_DEFAULT`
/// (`backends/BackendBase.py:44-48`, used by `_setCursorForMarker`,
/// `PlotInteraction.py:1165-1184`). A handle that moves only horizontally shows
/// `SizeHor`, only vertically `SizeVer`, freely in both `SizeAll`; nothing
/// grabbable shows `Default`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    /// Horizontal resize (silx `CURSOR_SIZE_HOR`, Qt `SizeHorCursor`).
    SizeHor,
    /// Vertical resize (silx `CURSOR_SIZE_VER`, Qt `SizeVerCursor`).
    SizeVer,
    /// Diagonal resize along the ↘↖ axis: top-left ↔ bottom-right corner (Qt
    /// `SizeFDiagCursor`). silx maps all corner handles to `CURSOR_SIZE_ALL`;
    /// rsplot uses egui's native diagonal cursor for Rect corners, matching
    /// egui's own window-corner resize affordance.
    SizeNwse,
    /// Diagonal resize along the ↗↙ axis: top-right ↔ bottom-left corner (Qt
    /// `SizeBDiagCursor`); the rsplot Rect-corner counterpart of `SizeNwse`.
    SizeNesw,
    /// Move in both axes (silx `CURSOR_SIZE_ALL`, Qt `SizeAllCursor`).
    SizeAll,
    /// The default arrow cursor (silx `CURSOR_DEFAULT`, Qt `ArrowCursor`).
    #[default]
    Default,
}

impl CursorShape {
    /// Map to the egui [`egui::CursorIcon`] the widget sets. `SizeHor` →
    /// `ResizeHorizontal`, `SizeVer` → `ResizeVertical`, `SizeAll` → `Move`,
    /// `Default` → `Default`, matching silx's Qt cursor mapping
    /// (`backends/BackendPygfx.py:2354-2358`).
    pub fn to_egui(self) -> egui::CursorIcon {
        match self {
            CursorShape::SizeHor => egui::CursorIcon::ResizeHorizontal,
            CursorShape::SizeVer => egui::CursorIcon::ResizeVertical,
            CursorShape::SizeNwse => egui::CursorIcon::ResizeNwSe,
            CursorShape::SizeNesw => egui::CursorIcon::ResizeNeSw,
            CursorShape::SizeAll => egui::CursorIcon::Move,
            CursorShape::Default => egui::CursorIcon::Default,
        }
    }
}

/// Cursor shape for a draggable ROI edge handle, mirroring the direction logic
/// of silx `_setCursorForMarker` (`PlotInteraction.py:1165-1184`): a handle that
/// constrains motion to one axis shows that axis's resize cursor; a free
/// (vertex) handle shows the move cursor.
///
/// - [`RoiEdge::Left`] / [`RoiEdge::Right`] move only in X → [`CursorShape::SizeHor`].
/// - [`RoiEdge::Top`] / [`RoiEdge::Bottom`] move only in Y → [`CursorShape::SizeVer`].
/// - [`RoiEdge::TopLeft`] / [`RoiEdge::BottomRight`] resize diagonally along the
///   ↘↖ axis → [`CursorShape::SizeNwse`].
/// - [`RoiEdge::TopRight`] / [`RoiEdge::BottomLeft`] resize diagonally along the
///   ↗↙ axis → [`CursorShape::SizeNesw`].
/// - [`RoiEdge::Vertex`] moves in both axes → [`CursorShape::SizeAll`].
///
/// The corner edges are labeled in *data* space (matching
/// [`Roi::edge_at`](crate::core::roi::Roi::edge_at)), but the diagonal cursor
/// must reflect the *screen* diagonal of the corner. An axis inversion mirrors
/// that data edge's screen position; a horizontal mirror (X inverted) and a
/// vertical mirror (Y inverted) each swap ↘↖ ↔ ↗↙, so the screen diagonal
/// flips iff exactly one axis is inverted (both cancel). `t` supplies the
/// per-axis inversion. The side cursors (`SizeHor`/`SizeVer`) are symmetric and
/// axis-aligned, so inversion never changes them; only the corners flip.
pub fn cursor_for_edge(edge: RoiEdge, t: &Transform) -> CursorShape {
    let flip = t.x.inverted ^ t.y.inverted;
    match edge {
        RoiEdge::Left | RoiEdge::Right => CursorShape::SizeHor,
        RoiEdge::Top | RoiEdge::Bottom => CursorShape::SizeVer,
        RoiEdge::TopLeft | RoiEdge::BottomRight => {
            if flip {
                CursorShape::SizeNesw
            } else {
                CursorShape::SizeNwse
            }
        }
        RoiEdge::TopRight | RoiEdge::BottomLeft => {
            if flip {
                CursorShape::SizeNwse
            } else {
                CursorShape::SizeNesw
            }
        }
        RoiEdge::Vertex(_) => CursorShape::SizeAll,
    }
}

/// Cursor shape for an optional grabbed edge: the edge's shape when `Some`, the
/// default arrow when `None` (nothing grabbable under the cursor). This is the
/// shape the widget passes to egui each hover frame. `t` resolves the corner
/// diagonal against the axis orientation (see [`cursor_for_edge`]).
pub fn cursor_for_grab(edge: Option<RoiEdge>, t: &Transform) -> CursorShape {
    edge.map(|e| cursor_for_edge(e, t)).unwrap_or_default()
}

/// Cursor shape for a draggable marker, reflecting its drag degrees of freedom,
/// mirroring silx's per-marker size cursor (`PlotInteraction.py`
/// `_handleMarkerCursor`, `CURSOR_SIZE_*`):
///
/// - [`MarkerKind::VLine`] moves only in X → [`CursorShape::SizeHor`].
/// - [`MarkerKind::HLine`] moves only in Y → [`CursorShape::SizeVer`].
/// - [`MarkerKind::Point`] with [`MarkerConstraint::None`] moves freely →
///   [`CursorShape::SizeAll`]; with [`MarkerConstraint::Horizontal`] (pins X,
///   leaves Y free) it moves only in Y → [`CursorShape::SizeVer`]; with
///   [`MarkerConstraint::Vertical`] (pins Y, leaves X free) only in X →
///   [`CursorShape::SizeHor`].
///
/// Pure, so the mapping is unit-testable without a `Ui`.
pub fn marker_cursor(marker: &Marker) -> CursorShape {
    match marker.kind {
        MarkerKind::VLine { .. } => CursorShape::SizeHor,
        MarkerKind::HLine { .. } => CursorShape::SizeVer,
        MarkerKind::Point { .. } => match marker.constraint {
            MarkerConstraint::None => CursorShape::SizeAll,
            // Horizontal pins X, leaving Y free: vertical motion only.
            MarkerConstraint::Horizontal => CursorShape::SizeVer,
            // Vertical pins Y, leaving X free: horizontal motion only.
            MarkerConstraint::Vertical => CursorShape::SizeHor,
        },
    }
}

/// Index of the topmost *draggable* marker hit by `cursor` (screen pixels) under
/// `transform`, or `None` when no draggable marker is hit. Iterates in reverse
/// (the last-drawn marker has the highest z, so it wins the pick), skipping any
/// marker whose [`Marker::is_draggable`] is `false` even if the cursor is over
/// it. Pure ([`Marker::pick`] is the per-kind hit-test), so it is unit-testable.
pub fn marker_at(markers: &[Marker], transform: &Transform, cursor: Pos2) -> Option<usize> {
    markers
        .iter()
        .enumerate()
        .rev()
        .find(|(_, m)| m.is_draggable && m.pick(transform, cursor))
        .map(|(i, _)| i)
}

// On-plot ROI creation ########################################################

/// Which of the 11 ROI shapes an on-plot creation interaction produces, mirroring
/// the `RegionOfInterest` subclasses silx arms via
/// `RegionOfInterestManager.start(roiClass)` (`tools/roi.py`). Each kind maps to
/// the draw shape silx's `_plotShape` selects ([`roi_draw_mode`]) and the
/// `setFirstShapePoints` geometry it computes ([`roi_from_draw`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoiDrawKind {
    /// Axis-aligned rectangle (silx `RectangleROI`).
    Rect,
    /// Horizontal band over a Y range, full X (our `Roi::HRange`).
    HRange,
    /// Vertical band over an X range, full Y (our `Roi::VRange`; silx
    /// `HorizontalRangeROI` spans X with two vertical markers).
    VRange,
    /// Single full-span horizontal line at a Y (silx `HorizontalLineROI`,
    /// `_plotShape = "hline"`; our `Roi::HLine`).
    HLine,
    /// Single full-span vertical line at an X (silx `VerticalLineROI`,
    /// `_plotShape = "vline"`; our `Roi::VLine`).
    VLine,
    /// Single point (silx `PointROI`).
    Point,
    /// Line segment (silx `LineROI`).
    Line,
    /// Polygon (silx `PolygonROI`).
    Polygon,
    /// Full-span cross-hairs at a point (silx `CrossROI`).
    Cross,
    /// Circle (silx `CircleROI`).
    Circle,
    /// Axis-aligned ellipse (silx `EllipseROI`).
    Ellipse,
    /// Annular sector (silx `ArcROI`).
    Arc,
    /// Rotatable band (silx `BandROI`).
    Band,
}

impl RoiDrawKind {
    /// The silx `RegionOfInterest.SHORT_NAME` for the ROI class this kind draws
    /// (`items/roi.py`, `items/_arc_roi.py:250`, `items/_band_roi.py:166`), used
    /// in the ROI-creation status message ([`crate::PlotInteractionMode::roi_creation_message`]).
    /// Matches the per-`Roi` table in the manager list so a kind and the ROI it
    /// builds report the same name.
    pub fn short_name(self) -> &'static str {
        match self {
            RoiDrawKind::Rect => "rectangle",
            RoiDrawKind::HRange => "hrange",
            RoiDrawKind::VRange => "vrange",
            RoiDrawKind::HLine => "hline",
            RoiDrawKind::VLine => "vline",
            RoiDrawKind::Point => "point",
            RoiDrawKind::Line => "line",
            RoiDrawKind::Polygon => "polygon",
            RoiDrawKind::Cross => "cross",
            RoiDrawKind::Circle => "circle",
            RoiDrawKind::Ellipse => "ellipse",
            RoiDrawKind::Arc => "arc",
            RoiDrawKind::Band => "band",
        }
    }
}

/// The [`DrawMode`] silx arms for creating `kind`, matching each ROI class's
/// `_plotShape` (`items/roi.py`, `items/_arc_roi.py:253`, `items/_band_roi.py:169`).
///
/// silx uses the 2-point `"line"` drag for `Line`/`Circle`/`Arc`/`Band` and for
/// `HorizontalRangeROI`, computing the final geometry from the two points in
/// `setFirstShapePoints`. `Point`/`Cross` use `"point"` (a single click). `Rect`
/// uses `"rectangle"`. For `Ellipse` this port arms the silx `SelectEllipse`
/// 2-point interaction (press = center, drag = perimeter point) so an
/// axis-aligned ellipse is produced directly; silx's `EllipseROI._plotShape` is
/// `"line"` with an oriented circle default (`items/roi.py:888`/`:953`), which
/// our axis-aligned `Roi::Ellipse` does not model.
pub fn roi_draw_mode(kind: RoiDrawKind) -> DrawMode {
    match kind {
        RoiDrawKind::Rect => DrawMode::Rectangle,
        RoiDrawKind::Ellipse => DrawMode::Ellipse,
        RoiDrawKind::Polygon => DrawMode::Polygon,
        RoiDrawKind::Point | RoiDrawKind::Cross => DrawMode::Point,
        RoiDrawKind::HLine => DrawMode::HLine,
        RoiDrawKind::VLine => DrawMode::VLine,
        RoiDrawKind::Line
        | RoiDrawKind::Circle
        | RoiDrawKind::HRange
        | RoiDrawKind::VRange
        | RoiDrawKind::Arc
        | RoiDrawKind::Band => DrawMode::Line,
    }
}

/// A keyboard action over the active ROI session, mirroring silx
/// `InteractiveRegionOfInterestManager.eventFilter` (`tools/roi.py:1072-1098`).
///
/// silx binds **no** per-shape keys — the roadmap's "R for Rect" / key→shape map
/// was speculative; the real bindings are validate-on-Enter and undo-last-ROI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoiKeyAction {
    /// Return/Enter: finish/validate the in-progress drawing (silx `quit()` in
    /// `ENTER` / `AUTO_ENTER` validation mode; rsplot uses it to close a
    /// polygon at its committed vertices, the `validate` path).
    Validate,
    /// Delete / Backspace / Ctrl+Z: remove the most recently added ROI (silx
    /// `removeRoi(rois[-1])`).
    UndoLast,
}

/// Map a key press to its ROI action, or `None` if the key is unbound (silx
/// `InteractiveRegionOfInterestManager.eventFilter`). `command` is whether the
/// platform command modifier (Ctrl, or ⌘ on macOS) is held, gating `Z` → undo.
pub fn roi_key_action(key: egui::Key, command: bool) -> Option<RoiKeyAction> {
    match key {
        egui::Key::Enter => Some(RoiKeyAction::Validate),
        egui::Key::Delete | egui::Key::Backspace => Some(RoiKeyAction::UndoLast),
        egui::Key::Z if command => Some(RoiKeyAction::UndoLast),
        _ => None,
    }
}

/// Build the [`Roi`] from a finished draw's [`DrawParams`], the
/// `setFirstShapePoints` equivalent per ROI class (`items/roi.py`,
/// `items/_arc_roi.py`, `items/_band_roi.py`). Returns `None` for a
/// `(kind, params)` pair that [`roi_draw_mode`] can never produce together
/// (e.g. an `HLine`/`VLine`/`FreeHand` params for any ROI kind), so an
/// unexpected pairing is dropped rather than mis-built.
///
/// Geometry per kind (each silx default cited inline):
/// - `Rect` <- `Rectangle{x,y,w,h}` (silx `_setBound`, `items/roi.py:558`).
/// - `Line` <- `Line` endpoints (silx `setEndPoints`, `items/roi.py:254`).
/// - `Polygon` <- `Polygon` vertices (silx `setPoints`, `items/roi.py:1236`).
/// - `Point`/`Cross` <- `Point` (silx `setPosition(points[0])`,
///   `items/roi.py:89`/`:176`).
/// - `Circle` <- `Line`: `center = start`, `radius = |end - start|`
///   (silx `CircleROI._setRay`, `items/roi.py:782`).
/// - `Ellipse` <- `Ellipse{center, semi_axes}`: `radii = semi_axes` (the silx
///   `SelectEllipse` interaction; see [`roi_draw_mode`]).
/// - `HRange` <- `Line`: `y = (min, max)` of the two endpoint Ys (the band over
///   a Y range; silx `HorizontalRangeROI.setFirstShapePoints` is the X analogue,
///   `items/roi.py:1420`).
/// - `VRange` <- `Line`: `x = (min, max)` of the two endpoint Xs (silx
///   `HorizontalRangeROI.setFirstShapePoints`, `items/roi.py:1420`).
/// - `Arc` <- `Line`: the faithful silx default arc from the 2 diameter points
///   (silx `ArcROI.setFirstShapePoints` + `_createGeometryFromControlPoints`,
///   `items/_arc_roi.py:363`/`:622`); see [`arc_from_two_points`].
/// - `Band` <- `Line`: `begin = start`, `end = end`,
///   `width = 0.1 * |end - begin|` (silx `BandGeometry.create` default width,
///   `items/_band_roi.py:64-66`).
pub fn roi_from_draw(kind: RoiDrawKind, params: &DrawParams) -> Option<Roi> {
    match (kind, params) {
        (
            RoiDrawKind::Rect,
            DrawParams::Rectangle {
                x,
                y,
                width,
                height,
            },
        ) => Some(Roi::Rect {
            x: (*x, x + width),
            y: (*y, y + height),
        }),
        (RoiDrawKind::Line, DrawParams::Line { start, end }) => Some(Roi::Line {
            start: *start,
            end: *end,
        }),
        (RoiDrawKind::Polygon, DrawParams::Polygon { vertices }) => Some(Roi::Polygon {
            vertices: vertices.clone(),
        }),
        (RoiDrawKind::Point, DrawParams::Point { x, y }) => Some(Roi::Point { x: *x, y: *y }),
        (RoiDrawKind::Cross, DrawParams::Point { x, y }) => Some(Roi::Cross { center: (*x, *y) }),
        (RoiDrawKind::Ellipse, DrawParams::Ellipse { center, semi_axes }) => Some(Roi::Ellipse {
            center: *center,
            radii: *semi_axes,
            // silx's `SelectEllipse` interaction produces an axis-aligned ellipse;
            // rotation comes later from dragging an axis handle off-axis.
            orientation: 0.0,
        }),
        // Circle: center = first point, radius = distance to the second
        // (silx CircleROI._setRay, items/roi.py:782).
        (RoiDrawKind::Circle, DrawParams::Line { start, end }) => {
            let r = (end.0 - start.0).hypot(end.1 - start.1);
            Some(Roi::Circle {
                center: *start,
                radius: r,
            })
        }
        // HRange: a Y band, bounded by the two endpoints' Ys (ordered).
        (RoiDrawKind::HRange, DrawParams::Line { start, end }) => Some(Roi::HRange {
            y: (start.1.min(end.1), start.1.max(end.1)),
        }),
        // VRange: an X band, bounded by the two endpoints' Xs (ordered).
        (RoiDrawKind::VRange, DrawParams::Line { start, end }) => Some(Roi::VRange {
            x: (start.0.min(end.0), start.0.max(end.0)),
        }),
        // HLine/VLine: a single full-span line at the captured position (silx
        // `Horizontal/VerticalLineROI.setFirstShapePoints`, `items/roi.py:402`/`:473`).
        (RoiDrawKind::HLine, DrawParams::HLine { y }) => Some(Roi::HLine { y: *y }),
        (RoiDrawKind::VLine, DrawParams::VLine { x }) => Some(Roi::VLine { x: *x }),
        // Arc: the faithful silx default arc from the 2 diameter points.
        (RoiDrawKind::Arc, DrawParams::Line { start, end }) => {
            Some(arc_from_two_points(*start, *end))
        }
        // Band: begin/end from the 2 points, default width = 0.1 * length.
        (RoiDrawKind::Band, DrawParams::Line { start, end }) => {
            let width = 0.1 * (end.0 - start.0).hypot(end.1 - start.1);
            Some(Roi::Band {
                begin: *start,
                end: *end,
                width: width.max(0.0),
            })
        }
        // Any other (kind, params) pair cannot be produced by roi_draw_mode.
        _ => None,
    }
}

/// The faithful silx `ArcROI` default geometry from two diameter points,
/// porting `ArcROI.setFirstShapePoints` + `_createGeometryFromControlPoints`
/// (`items/_arc_roi.py:363-385`, `:622-664`) and the public-geometry mapping in
/// `getGeometry` / `getInnerRadius` / `getOuterRadius` (`:781-874`).
///
/// silx builds a curvature control point `mid` off the `point0 → point1` line,
/// fits the circle through `(point0, mid, point1)`, and derives a `weight`
/// (band thickness) of `0.2 * |point1 - point0|`. The form our [`Roi::Arc`]
/// stores is silx's `center`, `radius`, `weight`, `start_angle`, `end_angle`
/// (the reported inner/outer are derived, `radius ∓ weight/2`).
///
/// `defaultCurvature = π/5`, `weightCoef = 0.20`
/// (`items/_arc_roi.py:377-381`). The fitted geometry is delegated to
/// [`arc_from_three_points`] (silx's `setFirstShapePoints` likewise routes through
/// `_createGeometryFromControlPoints`), which owns the closed-circle /
/// collinear / general branches. Two coincident points feed that the
/// closed-circle branch, yielding a degenerate zero-radius closed arc (silx's
/// `allclose(start, end)` special case); an on-plot drag never produces
/// coincident diameter points, so that path is only a safe fallback.
pub fn arc_from_two_points(point0: (f64, f64), point1: (f64, f64)) -> Roi {
    // center of the diameter; normal rotated -90 deg (silx: (normal_y, -normal_x)).
    let mid_center = ((point0.0 + point1.0) * 0.5, (point0.1 + point1.1) * 0.5);
    let normal_raw = (point1.0 - mid_center.0, point1.1 - mid_center.1);
    let normal = (normal_raw.1, -normal_raw.0);
    let default_curvature = std::f64::consts::PI / 5.0;
    let weight_coef = 0.20;
    let mid = (
        mid_center.0 - normal.0 * default_curvature,
        mid_center.1 - normal.1 * default_curvature,
    );
    let weight = (point0.0 - point1.0).hypot(point0.1 - point1.1) * weight_coef;
    arc_from_three_points(point0, mid, point1, weight)
}

/// Build a [`Roi::Arc`] from three points on the arc plus a radial `weight` (the
/// band thickness), the faithful port of silx
/// `ArcROI._createGeometryFromControlPoints` (`items/_arc_roi.py:622-664`) — the
/// geometry behind silx's default *ThreePointMode* sub-mode. `start` and `end`
/// are the arc endpoints; `mid` is a point the arc passes through between them
/// (the curvature control). `weight` is the full radial thickness, stored
/// directly alongside the central `radius` (the reported inner/outer are derived,
/// `radius ∓ weight/2`).
///
/// Branches mirror silx exactly:
/// - `start ≈ end` (silx `numpy.allclose`) → a closed circle through
///   `start`/`mid` (silx `_ArcGeometry.createCircle`): center at the
///   `start`–`mid` midpoint, swept the full `2π` from `start`'s angle.
/// - `start`, `mid`, `end` collinear (`|cross| < 1e-5`) → silx builds a
///   center-less "rect" intermediate, which rsplot's [`Roi::Arc`] (always
///   centered) cannot represent; this falls back to a degenerate zero-radius arc
///   at the `start`–`end` midpoint. The transient rect editing shape is not
///   modeled (deviation noted on the roadmap row).
/// - otherwise → the circumscribed circle through the three points
///   (`circle_through`, silx `_circleEquation`) with the sweep direction
///   disambiguated from the mid angle (silx `:652-660`).
pub fn arc_from_three_points(
    start: (f64, f64),
    mid: (f64, f64),
    end: (f64, f64),
    weight: f64,
) -> Roi {
    let two_pi = std::f64::consts::TAU;
    let angle = |p: (f64, f64), c: (f64, f64)| (p.1 - c.1).atan2(p.0 - c.0);

    // Closed circle: silx `numpy.allclose(start, end)` (atol 1e-8, rtol 1e-5).
    let close = |a: f64, b: f64| (a - b).abs() <= 1e-8 + 1e-5 * b.abs();
    if close(start.0, end.0) && close(start.1, end.1) {
        let center = ((start.0 + mid.0) * 0.5, (start.1 + mid.1) * 0.5);
        let radius = (start.0 - center.0).hypot(start.1 - center.1);
        let start_angle = angle(start, center);
        return Roi::Arc {
            center,
            radius,
            weight,
            start_angle,
            end_angle: start_angle + two_pi,
        };
    }

    // Collinear: silx makes a center-less rect; not representable by Roi::Arc.
    let cross = (mid.0 - start.0) * (end.1 - start.1) - (mid.1 - start.1) * (end.0 - start.0);
    if cross.abs() < 1e-5 {
        return Roi::Arc {
            center: ((start.0 + end.0) * 0.5, (start.1 + end.1) * 0.5),
            radius: 0.0,
            weight: 0.0,
            start_angle: 0.0,
            end_angle: 0.0,
        };
    }

    // Circle through (start, mid, end) — silx _circleEquation.
    let (center, radius) = circle_through(start, mid, end);
    let start_angle = angle(start, center);
    let mid_angle = angle(mid, center);
    let mut end_angle = angle(end, center);

    // Disambiguate sweep direction (silx items/_arc_roi.py:652-660).
    let relative_mid = (end_angle - mid_angle + two_pi).rem_euclid(two_pi);
    let relative_end = (end_angle - start_angle + two_pi).rem_euclid(two_pi);
    if relative_mid < relative_end {
        if end_angle < start_angle {
            end_angle += two_pi;
        }
    } else if end_angle > start_angle {
        end_angle -= two_pi;
    }

    Roi::Arc {
        center,
        radius,
        weight,
        start_angle,
        end_angle,
    }
}

/// The three ThreePointMode control points of an arc, in `(start, mid, end)`
/// order, each a data-space `(x, y)` (silx `ArcROI` `_handleStart` /
/// `_handleMid` / `_handleEnd`).
pub type ArcControlPoints = ((f64, f64), (f64, f64), (f64, f64));

/// One of the three control points of an arc in silx ThreePointMode editing
/// (silx `ArcROI` `_handleStart` / `_handleMid` / `_handleEnd`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArcControlPoint {
    /// The start-angle endpoint handle (silx `_handleStart`).
    Start,
    /// The mid/curvature handle on the angular bisector (silx `_handleMid`).
    Mid,
    /// The end-angle endpoint handle (silx `_handleEnd`).
    End,
}

/// The three ThreePointMode control points `(start, mid, end)` of a polar
/// [`Roi::Arc`], on its central radius (silx derives these in `_updateHandles` /
/// `_updateMidHandle`, `items/_arc_roi.py:390-410`): the start/end handles sit at
/// the sweep endpoints and the mid handle on the angular bisector. Returns
/// `None` for a non-arc.
pub fn arc_control_points(arc: &Roi) -> Option<ArcControlPoints> {
    let Roi::Arc {
        center,
        radius,
        start_angle,
        end_angle,
        ..
    } = arc
    else {
        return None;
    };
    let (cx, cy) = *center;
    let at = |a: f64| (cx + radius * a.cos(), cy + radius * a.sin());
    let mid_angle = (*start_angle + *end_angle) * 0.5;
    Some((at(*start_angle), at(mid_angle), at(*end_angle)))
}

/// Reshape an arc in silx ThreePointMode by dragging one of its three control
/// points to `to`, recomputing the circumcircle through the new triple (silx
/// `ArcROI.handleDragUpdated` ThreePointMode branch → `_updateGeometry` via
/// `_createGeometryFromControlPoints`). Unlike PolarMode editing
/// ([`Roi::move_edge`](crate::core::roi::Roi::move_edge), which fixes the
/// center), moving a control point re-fits both the center/radius and the sweep
/// angles. The band thickness (`weight`) is preserved directly, so it survives
/// even when a prior drag drove the reported inner radius to 0 (R2-15). Returns
/// the input unchanged for a non-arc.
pub fn arc_three_point_drag(arc: &Roi, point: ArcControlPoint, to: (f64, f64)) -> Roi {
    let (Some((start, mid, end)), Roi::Arc { weight, .. }) = (arc_control_points(arc), arc) else {
        return arc.clone();
    };
    let weight = *weight;
    let (start, mid, end) = match point {
        ArcControlPoint::Start => (to, mid, end),
        ArcControlPoint::Mid => (start, to, end),
        ArcControlPoint::End => (start, mid, to),
    };
    arc_from_three_points(start, mid, end, weight)
}

/// Map a handle index (`0/1/2`) to its ThreePointMode control point in the
/// `(start, mid, end)` order [`arc_control_points`] / [`arc_three_point_handles`]
/// expose. Any other index clamps to `End`, matching the three-handle layout.
fn arc_control_point_for(index: usize) -> ArcControlPoint {
    match index {
        0 => ArcControlPoint::Start,
        1 => ArcControlPoint::Mid,
        _ => ArcControlPoint::End,
    }
}

/// True when this managed ROI edits in silx **ThreePointMode**: an [`Roi::Arc`]
/// whose interaction mode is [`RoiInteractionMode::ArcThreePoint`]. In that mode
/// the ROI's draggable handles are the three control points (not the four polar
/// handles) and a handle drag reshapes via [`arc_three_point_drag`] (re-fitting
/// the circumcircle) instead of the polar [`Roi::move_edge`].
#[must_use]
pub fn arc_is_three_point(managed: &ManagedRoi) -> bool {
    matches!(managed.roi, Roi::Arc { .. })
        && managed.interaction_mode() == Some(RoiInteractionMode::ArcThreePoint)
}

/// The three ThreePointMode handles of an arc as [`RoiHandle`]s (data-space
/// vertex handles at the `start`/`mid`/`end` control points, in that order), for
/// drawing and hit-testing. `None` for a non-arc.
#[must_use]
pub fn arc_three_point_handles(arc: &Roi) -> Option<[RoiHandle; 3]> {
    let (start, mid, end) = arc_control_points(arc)?;
    let h = |p: (f64, f64)| RoiHandle {
        pos: [p.0, p.1],
        kind: HandleKind::Vertex,
    };
    Some([h(start), h(mid), h(end)])
}

/// Index of the ThreePointMode control-point handle within `grab_px` of
/// `cursor` (screen pixels), as a [`RoiEdge::Vertex`] (`0=start, 1=mid, 2=end`),
/// or `None`. The closest handle within range wins, mirroring
/// [`Roi::edge_at`]'s nearest-handle rule. Pure (geometry only).
#[must_use]
pub fn arc_three_point_grab(
    arc: &Roi,
    transform: &Transform,
    cursor: Pos2,
    grab_px: f32,
) -> Option<RoiEdge> {
    let handles = arc_three_point_handles(arc)?;
    let mut best: Option<(usize, f32)> = None;
    for (i, handle) in handles.iter().enumerate() {
        let p = transform.data_to_pixel(handle.pos[0], handle.pos[1]);
        let dist = cursor.distance(p);
        if dist <= grab_px && best.is_none_or(|(_, d)| dist < d) {
            best = Some((i, dist));
        }
    }
    best.map(|(i, _)| RoiEdge::Vertex(i))
}

/// Apply a handle drag to a managed ROI, mode-gated (silx
/// `HandleBasedROI.handleDragUpdated` dispatched by `InteractionModeMixIn`): a
/// ThreePointMode Arc ([`arc_is_three_point`]) reshapes via
/// [`arc_three_point_drag`] — the grabbed `Vertex(i)` is control point
/// start/mid/end — re-fitting the circumcircle; every other ROI and mode (Arc
/// PolarMode, Band, Rect, …) applies the polar/default [`Roi::move_edge`]. This
/// is the single owner of "how a grabbed handle edits the shape", so the live
/// drag path never has to branch on the mode itself.
pub fn roi_apply_edge_drag(managed: &mut ManagedRoi, edge: RoiEdge, to: (f64, f64)) {
    if arc_is_three_point(managed) {
        if let RoiEdge::Vertex(i) = edge {
            managed.roi = arc_three_point_drag(&managed.roi, arc_control_point_for(i), to);
        }
    } else {
        managed.roi.move_edge(edge, to);
    }
}

/// Center and radius of the circle through three points, porting silx
/// `ArcROI._circleEquation` (`items/_arc_roi.py:986-996`). silx uses complex
/// arithmetic:
/// ```text
/// x, y, z = complex(pt1), complex(pt2), complex(pt3)
/// w = (z - x) / (y - x)
/// c = (x - y) * (w - |w|^2) / (2j * w.imag) - x
/// center = (-c.real, -c.imag);  radius = |c + x|
/// ```
/// Ported below with explicit complex multiply/divide. Collinear points yield
/// `w.imag == 0`; the caller (`arc_from_two_points`) never passes collinear
/// triples because `mid` is offset off the `pt1 → pt3` line.
fn circle_through(pt1: (f64, f64), pt2: (f64, f64), pt3: (f64, f64)) -> ((f64, f64), f64) {
    // Complex helpers on (re, im) tuples.
    let sub = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0, a.1 - b.1);
    let mul = |a: (f64, f64), b: (f64, f64)| (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0);
    let div = |a: (f64, f64), b: (f64, f64)| {
        let den = b.0 * b.0 + b.1 * b.1;
        ((a.0 * b.0 + a.1 * b.1) / den, (a.1 * b.0 - a.0 * b.1) / den)
    };
    let (x, y, z) = (pt1, pt2, pt3);
    let w = div(sub(z, x), sub(y, x));
    let w_abs2 = w.0 * w.0 + w.1 * w.1;
    // numerator: (x - y) * (w - |w|^2)
    let num = mul(sub(x, y), (w.0 - w_abs2, w.1));
    // denominator: 2j * w.imag  ==  (0, 2 * w.imag)
    let den = (0.0, 2.0 * w.1);
    let c = sub(div(num, den), x);
    let center = (-c.0, -c.1);
    // radius = |c + x|
    let cx = (c.0 + x.0, c.1 + x.1);
    let radius = (cx.0 * cx.0 + cx.1 * cx.1).sqrt();
    (center, radius)
}

/// How an on-plot drag grabbed an existing ROI for editing, mirroring silx's
/// `HandleBasedROI` interaction: either a specific edge/vertex handle
/// ([`RoiGrab::Edge`], silx `handleDragUpdated`) or the whole-shape body
/// ([`RoiGrab::Translate`], silx `addTranslateHandle` / drag-on-body).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RoiGrab {
    /// A specific draggable edge/vertex handle was grabbed.
    Edge(RoiEdge),
    /// The ROI body (no handle under the cursor, but inside the shape) was
    /// grabbed for a whole-ROI translate.
    Translate,
}

/// Classify what an on-plot primary-drag grabs at `cursor` (screen pixels) over
/// `rois`, mirroring silx's per-ROI hit priority: a handle wins over the body.
/// Iterates topmost-first (last drawn = highest z); for each ROI returns
/// [`RoiGrab::Edge`] when an edge handle is within `grab_px` of the cursor, else
/// [`RoiGrab::Translate`] when the cursor's data position is inside the shape.
/// Returns the `(index, grab)` of the first ROI that matches, or `None`. Pure,
/// so the priority is unit-testable without a `Ui`.
pub fn roi_grab_at(
    rois: &[ManagedRoi],
    transform: &Transform,
    cursor: Pos2,
    grab_px: f32,
) -> Option<(usize, RoiGrab)> {
    let data = transform.pixel_to_data(cursor);
    for (i, managed) in rois.iter().enumerate().rev() {
        // silx setVisible(False) hides the ROI's plot items and their handles
        // (_roi_base.py:479-489, handle visibility at :824) — a hidden ROI is
        // neither grabbable nor translatable.
        if !managed.visible {
            continue;
        }
        let roi = &managed.roi;
        // ThreePointMode Arc: only the three control-point handles are grabbable
        // (not the polar handles). PolarMode and every other ROI use `edge_at`.
        let edge = if arc_is_three_point(managed) {
            arc_three_point_grab(roi, transform, cursor, grab_px)
        } else {
            roi.edge_at(transform, cursor, grab_px)
        };
        if let Some(edge) = edge {
            return Some((i, RoiGrab::Edge(edge)));
        }
        if roi.contains(data) {
            return Some((i, RoiGrab::Translate));
        }
    }
    None
}

/// Which mouse button a [`PlotPointerEvent`] carries, mirroring silx's
/// `"left" | "middle" | "right"` button strings (`PlotEvents.py:58-71`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

impl MouseButton {
    /// Map an egui [`egui::PointerButton`] to the silx button identity. egui's
    /// extra buttons collapse to the nearest silx button (silx has only three).
    pub fn from_egui(button: egui::PointerButton) -> Self {
        match button {
            egui::PointerButton::Primary => MouseButton::Left,
            egui::PointerButton::Middle => MouseButton::Middle,
            _ => MouseButton::Right,
        }
    }
}

/// A structured pointer event over the plot data area, mirroring silx's
/// `prepareMouseSignal` (`PlotEvents.py:58-71`) and `prepareLimitsChangedSignal`
/// (`PlotEvents.py:176-184`). Each pointer variant carries the button (where a
/// button applies), the data-space position, and the pixel-space position so
/// application code has both without re-projecting.
///
/// This is the structured low-level pointer event produced by [`PlotView`]
/// interaction; it is distinct from the high-level item-lifecycle
/// `PlotEvent` queue owned by `PlotWidget`.
///
/// [`PlotView`]: crate::PlotView
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlotPointerEvent {
    /// A single click (silx `"mouseClicked"`).
    Clicked {
        button: MouseButton,
        /// Data-space `(x, y)` under the cursor.
        data: (f64, f64),
        /// Pixel-space `(x, y)` of the cursor.
        pixel: (f32, f32),
    },
    /// A double click (silx `"mouseDoubleClicked"`). silx only emits this for
    /// the left button, at the position of the first click.
    DoubleClicked {
        button: MouseButton,
        data: (f64, f64),
        pixel: (f32, f32),
    },
    /// The cursor moved over the data area (silx `"mouseMoved"` hover).
    Moved {
        /// `None` for a bare move (silx leaves the button unset when no button
        /// is held); `Some` when a button is held during the move.
        button: Option<MouseButton>,
        data: (f64, f64),
        pixel: (f32, f32),
    },
    /// The display limits changed (silx `"limitsChanged"`), carrying the new
    /// left-X, left-Y, and (optional) right-Y2 ranges as `(min, max)` tuples.
    LimitsChanged {
        x: (f64, f64),
        y: (f64, f64),
        y2: Option<(f64, f64)>,
    },
}

impl PlotPointerEvent {
    /// Build a [`PlotPointerEvent::Clicked`] from a cursor pixel position and
    /// the display [`Transform`], projecting the pixel to data space (silx
    /// `prepareMouseSignal("mouseClicked", ...)`).
    pub fn clicked(button: MouseButton, transform: &Transform, pixel: Pos2) -> Self {
        PlotPointerEvent::Clicked {
            button,
            data: transform.pixel_to_data(pixel),
            pixel: (pixel.x, pixel.y),
        }
    }

    /// Build a [`PlotPointerEvent::DoubleClicked`] from a cursor pixel position
    /// (silx `prepareMouseSignal("mouseDoubleClicked", ...)`).
    pub fn double_clicked(button: MouseButton, transform: &Transform, pixel: Pos2) -> Self {
        PlotPointerEvent::DoubleClicked {
            button,
            data: transform.pixel_to_data(pixel),
            pixel: (pixel.x, pixel.y),
        }
    }

    /// Build a [`PlotPointerEvent::Moved`] hover event from a cursor pixel
    /// position (silx `prepareMouseSignal("mouseMoved", ...)`). `button` is the
    /// held button, if any.
    pub fn moved(button: Option<MouseButton>, transform: &Transform, pixel: Pos2) -> Self {
        PlotPointerEvent::Moved {
            button,
            data: transform.pixel_to_data(pixel),
            pixel: (pixel.x, pixel.y),
        }
    }

    /// Build a [`PlotPointerEvent::LimitsChanged`] (silx
    /// `prepareLimitsChangedSignal`).
    pub fn limits_changed(x: (f64, f64), y: (f64, f64), y2: Option<(f64, f64)>) -> Self {
        PlotPointerEvent::LimitsChanged { x, y, y2 }
    }
}

/// A picked polyline vertex: its index and data coordinates, plus the pixel
/// distance from the cursor (`doc/design.md` §13 C2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointPick {
    pub index: usize,
    pub x: f64,
    pub y: f64,
    pub dist_px: f32,
}

/// Nearest polyline vertex to `cursor` (screen pixels) within `threshold_px`.
/// `points` are data coordinates, projected through `transform` to pixels for
/// the distance test. `None` if no vertex is within the threshold.
pub fn nearest_point(
    points: &[(f64, f64)],
    transform: &Transform,
    cursor: Pos2,
    threshold_px: f32,
) -> Option<PointPick> {
    let mut best: Option<PointPick> = None;
    for (index, &(x, y)) in points.iter().enumerate() {
        let dist_px = transform.data_to_pixel(x, y).distance(cursor);
        if dist_px <= threshold_px && best.is_none_or(|b| dist_px < b.dist_px) {
            best = Some(PointPick {
                index,
                x,
                y,
                dist_px,
            });
        }
    }
    best
}

/// Image pixel `(col, row)` under `cursor` (screen pixels), or `None` if the
/// cursor maps outside the image. `origin` is the data coordinate of pixel
/// `(0, 0)`'s lower-left corner and `scale` is data units per pixel (matching
/// [`crate::ImageData`]); row 0 is at the bottom.
pub fn image_index(
    transform: &Transform,
    origin: (f64, f64),
    scale: (f64, f64),
    dims: (u32, u32),
    cursor: Pos2,
) -> Option<(u32, u32)> {
    if scale.0 <= 0.0 || scale.1 <= 0.0 {
        return None;
    }
    let (x, y) = transform.pixel_to_data(cursor);
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let col = ((x - origin.0) / scale.0).floor();
    let row = ((y - origin.1) / scale.1).floor();
    if col < 0.0 || row < 0.0 {
        return None;
    }
    // Saturating f64->u32 cast handles huge values; the bounds check rejects them.
    let (col, row) = (col as u32, row as u32);
    (col < dims.0 && row < dims.1).then_some((col, row))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    fn area_100() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0))
    }

    fn close(a: Limits, b: Limits) -> bool {
        let t = 1e-9;
        (a.0 - b.0).abs() <= t
            && (a.1 - b.1).abs() <= t
            && (a.2 - b.2).abs() <= t
            && (a.3 - b.3).abs() <= t
    }

    #[test]
    fn roi_draw_kind_short_name_matches_silx() {
        // Each kind reports its silx `RegionOfInterest.SHORT_NAME`, identical to
        // the per-`Roi` manager-list table so a kind and the ROI it builds agree.
        assert_eq!(RoiDrawKind::Rect.short_name(), "rectangle");
        assert_eq!(RoiDrawKind::HRange.short_name(), "hrange");
        assert_eq!(RoiDrawKind::VRange.short_name(), "vrange");
        assert_eq!(RoiDrawKind::HLine.short_name(), "hline");
        assert_eq!(RoiDrawKind::VLine.short_name(), "vline");
        assert_eq!(RoiDrawKind::Point.short_name(), "point");
        assert_eq!(RoiDrawKind::Line.short_name(), "line");
        assert_eq!(RoiDrawKind::Polygon.short_name(), "polygon");
        assert_eq!(RoiDrawKind::Cross.short_name(), "cross");
        assert_eq!(RoiDrawKind::Circle.short_name(), "circle");
        assert_eq!(RoiDrawKind::Ellipse.short_name(), "ellipse");
        assert_eq!(RoiDrawKind::Arc.short_name(), "arc");
        assert_eq!(RoiDrawKind::Band.short_name(), "band");
    }

    #[test]
    fn pan_right_shifts_view_left() {
        // Drag 10px right (10% of width, span 10) -> x limits shift -1.
        let out = pan(
            (0.0, 10.0, 0.0, 10.0),
            area_100(),
            vec2(10.0, 0.0),
            Scale::Linear,
            Scale::Linear,
        );
        assert!(close(out, (-1.0, 9.0, 0.0, 10.0)), "{out:?}");
    }

    #[test]
    fn pan_down_increases_y_limits() {
        // Y is flipped: dragging down raises the data Y window.
        let out = pan(
            (0.0, 10.0, 0.0, 10.0),
            area_100(),
            vec2(0.0, 10.0),
            Scale::Linear,
            Scale::Linear,
        );
        assert!(close(out, (0.0, 10.0, 1.0, 11.0)), "{out:?}");
    }

    #[test]
    fn pan_log_round_trips_in_log_space() {
        // Boundary: a +d drag then a -d drag on a log axis returns to the start.
        let limits = (1.0, 100.0, 1.0, 100.0);
        let area = area_100();
        let forward = pan(limits, area, vec2(20.0, 13.0), Scale::Log10, Scale::Log10);
        let back = pan(
            forward,
            area,
            vec2(-20.0, -13.0),
            Scale::Log10,
            Scale::Log10,
        );
        assert!(close(back, limits), "{back:?}");
        // The intermediate state must have moved (otherwise the round-trip is trivial).
        assert!(!close(forward, limits), "{forward:?}");
    }

    #[test]
    fn pan_log_translates_in_log_space() {
        // A drag of half the width on a log decade [1, 100] shifts both bounds by
        // half a log decade in log10 space (the span is 2 decades over 100px, so
        // 50px == 1 decade).
        let out = pan(
            (1.0, 100.0, 1.0, 100.0),
            area_100(),
            vec2(50.0, 0.0),
            Scale::Log10,
            Scale::Linear,
        );
        // X limits shift left by one decade: 1 -> 0.1, 100 -> 10.
        assert!((out.0 - 0.1).abs() <= 1e-9, "{out:?}");
        assert!((out.1 - 10.0).abs() <= 1e-9, "{out:?}");
        // Y (linear) unchanged.
        assert!(
            (out.2 - 1.0).abs() <= 1e-9 && (out.3 - 100.0).abs() <= 1e-9,
            "{out:?}"
        );
    }

    #[test]
    fn zoom_about_center_halves_span_keeping_center() {
        let out = zoom_about(
            (0.0, 10.0, 0.0, 10.0),
            0.5,
            5.0,
            5.0,
            Scale::Linear,
            Scale::Linear,
        );
        assert!(close(out, (2.5, 7.5, 2.5, 7.5)), "{out:?}");
    }

    #[test]
    fn zoom_about_keeps_anchor_fixed() {
        // The anchor's fractional position within the limits is unchanged.
        let limits = (0.0, 10.0, 0.0, 10.0);
        let (cx, cy) = (8.0, 2.0);
        let out = zoom_about(limits, 0.3, cx, cy, Scale::Linear, Scale::Linear);
        let frac_before = (cx - limits.0) / (limits.1 - limits.0);
        let frac_after = (cx - out.0) / (out.1 - out.0);
        assert!((frac_before - frac_after).abs() <= 1e-9);
        let _ = cy;
    }

    #[test]
    fn zoom_about_log_keeps_anchor_data_coord_fixed() {
        // Boundary: on a log axis the cursor's data coordinate must stay fixed
        // across a zoom (its fractional position in log space is invariant).
        let limits = (1.0, 1000.0, 1.0, 1000.0);
        let (cx, cy) = (10.0, 100.0);
        let out = zoom_about(limits, 0.5, cx, cy, Scale::Log10, Scale::Log10);
        let frac_log =
            |v: f64, lo: f64, hi: f64| (v.log10() - lo.log10()) / (hi.log10() - lo.log10());
        let fx_before = frac_log(cx, limits.0, limits.1);
        let fx_after = frac_log(cx, out.0, out.1);
        assert!(
            (fx_before - fx_after).abs() <= 1e-9,
            "x {fx_before} {fx_after}"
        );
        let fy_before = frac_log(cy, limits.2, limits.3);
        let fy_after = frac_log(cy, out.2, out.3);
        assert!(
            (fy_before - fy_after).abs() <= 1e-9,
            "y {fy_before} {fy_after}"
        );
    }

    #[test]
    fn pan_view_shifts_y2_in_the_same_gesture() {
        // silx `Pan.drag` (PlotInteraction.py:260-335) shifts the right (y2)
        // axis in the same gesture, by the same pixel delta through its own
        // range. Drag 10px down on a 100px-high area: left y [0,10] shifts +1,
        // y2 [0,100] shifts +10 (both by 10% of their span).
        let (out, y2) = pan_view(
            ((0.0, 10.0, 0.0, 10.0), Some((0.0, 100.0))),
            area_100(),
            vec2(0.0, 10.0),
            Scale::Linear,
            Scale::Linear,
        );
        assert!(close(out, (0.0, 10.0, 1.0, 11.0)), "{out:?}");
        let (lo, hi) = y2.expect("y2 present");
        assert!(
            (lo - 10.0).abs() <= 1e-9 && (hi - 110.0).abs() <= 1e-9,
            "{lo} {hi}"
        );
    }

    #[test]
    fn pan_view_without_y2_axis_stays_none() {
        // No right axis: the view pans exactly like `pan` and y2 stays absent.
        let (out, y2) = pan_view(
            ((0.0, 10.0, 0.0, 10.0), None),
            area_100(),
            vec2(10.0, 0.0),
            Scale::Linear,
            Scale::Linear,
        );
        assert!(close(out, (-1.0, 9.0, 0.0, 10.0)), "{out:?}");
        assert_eq!(y2, None);
    }

    #[test]
    fn zoom_view_about_scales_y2_about_right_axis_centre() {
        // silx `applyZoomToPlot` (_utils/panzoom.py:132-176) scales y2 about
        // the zoom centre mapped through the right axis. Shrink to half span
        // about the middle of both axes: left y [0,10] -> [2.5,7.5], y2
        // [0,100] about 50 -> [25,75].
        let (out, y2) = zoom_view_about(
            ((0.0, 10.0, 0.0, 10.0), Some((0.0, 100.0))),
            0.5,
            (5.0, 5.0),
            Some(50.0),
            Scale::Linear,
            Scale::Linear,
            (true, true),
        );
        assert!(close(out, (2.5, 7.5, 2.5, 7.5)), "{out:?}");
        let (lo, hi) = y2.expect("y2 present");
        assert!(
            (lo - 25.0).abs() <= 1e-9 && (hi - 75.0).abs() <= 1e-9,
            "{lo} {hi}"
        );
    }

    #[test]
    fn zoom_view_about_disabled_axis_keeps_its_range() {
        // silx `applyZoomToPlot` skips a disabled axis (`if enabled.xaxis:`,
        // _utils/panzoom.py:150-166); the y2 axis follows the Y flag (silx's
        // separate y2axis flag folds into it, see `Plot::zoom_y_enabled`).
        let view = ((0.0, 10.0, 0.0, 10.0), Some((0.0, 100.0)));
        // X disabled: X kept, Y and y2 scaled.
        let (out, y2) = zoom_view_about(
            view,
            0.5,
            (5.0, 5.0),
            Some(50.0),
            Scale::Linear,
            Scale::Linear,
            (false, true),
        );
        assert!(close(out, (0.0, 10.0, 2.5, 7.5)), "{out:?}");
        assert_eq!(y2, Some((25.0, 75.0)));
        // Y disabled: Y and y2 kept, X scaled.
        let (out, y2) = zoom_view_about(
            view,
            0.5,
            (5.0, 5.0),
            Some(50.0),
            Scale::Linear,
            Scale::Linear,
            (true, false),
        );
        assert!(close(out, (2.5, 7.5, 0.0, 10.0)), "{out:?}");
        assert_eq!(y2, Some((0.0, 100.0)));
    }

    #[test]
    fn zoom_view_about_keeps_y2_anchor_fraction_fixed() {
        // The y2 anchor keeps its fractional position within the y2 range,
        // exactly like the left axes' anchor (silx scale1DRange invariant).
        let y2_before = (0.0, 100.0);
        let cy2 = 80.0;
        let (_, y2) = zoom_view_about(
            ((0.0, 10.0, 0.0, 10.0), Some(y2_before)),
            0.3,
            (5.0, 5.0),
            Some(cy2),
            Scale::Linear,
            Scale::Linear,
            (true, true),
        );
        let (lo, hi) = y2.expect("y2 present");
        let frac_before = (cy2 - y2_before.0) / (y2_before.1 - y2_before.0);
        let frac_after = (cy2 - lo) / (hi - lo);
        assert!((frac_before - frac_after).abs() <= 1e-9, "{lo} {hi}");
    }

    #[test]
    fn zoom_view_about_without_centre_leaves_y2_unchanged() {
        // No right-axis transform this frame (`cy2 == None`): y2 unchanged.
        let (out, y2) = zoom_view_about(
            ((0.0, 10.0, 0.0, 10.0), Some((0.0, 100.0))),
            0.5,
            (5.0, 5.0),
            None,
            Scale::Linear,
            Scale::Linear,
            (true, true),
        );
        assert!(close(out, (2.5, 7.5, 2.5, 7.5)), "{out:?}");
        assert_eq!(y2, Some((0.0, 100.0)));
    }

    #[test]
    fn apply_pan_linear_offsets_by_fraction() {
        // Linear: pan 10% of the [0, 10] span to the right.
        let (lo, hi) = apply_pan(0.0, 10.0, 0.1, false);
        assert!(
            (lo - 1.0).abs() <= 1e-12 && (hi - 11.0).abs() <= 1e-12,
            "{lo} {hi}"
        );
    }

    #[test]
    fn apply_pan_log_round_trips() {
        // Boundary: log pan +f then -f returns to the start in log space.
        let (lo, hi) = apply_pan(1.0, 100.0, 0.25, true);
        let (lo2, hi2) = apply_pan(lo, hi, -0.25, true);
        assert!(
            (lo2 - 1.0).abs() <= 1e-9 && (hi2 - 100.0).abs() <= 1e-9,
            "{lo2} {hi2}"
        );
        // Forward step moved by 0.25 decade: 1 -> 10^0.5, 100 -> 10^2.5.
        assert!((lo - 10f64.powf(0.5)).abs() <= 1e-9, "{lo}");
        assert!((hi - 10f64.powf(2.5)).abs() <= 1e-9, "{hi}");
    }

    #[test]
    fn apply_pan_log_nonpositive_min_falls_back_to_linear() {
        // Boundary: a non-positive min on a log axis takes silx's linear branch.
        let (lo, hi) = apply_pan(-1.0, 10.0, 0.1, true);
        // Linear offset: 0.1 * (10 - -1) = 1.1.
        assert!(
            (lo - 0.1).abs() <= 1e-12 && (hi - 11.1).abs() <= 1e-12,
            "{lo} {hi}"
        );
    }

    #[test]
    fn box_zoom_orders_corners() {
        let out = box_zoom(8.0, 1.0, 2.0, 9.0);
        assert!(close(out, (2.0, 8.0, 1.0, 9.0)), "{out:?}");
    }

    #[test]
    fn constrain_zoom_axes_keeps_disabled_axis_range() {
        let zoomed = (2.0, 8.0, 1.0, 9.0);
        let current = (0.0, 10.0, 0.0, 100.0);
        // Both enabled: the box result is unchanged (silx default, all axes on).
        assert!(close(
            constrain_zoom_axes(zoomed, current, true, true),
            zoomed
        ));
        // X disabled: keep current X, take zoomed Y.
        assert!(close(
            constrain_zoom_axes(zoomed, current, false, true),
            (0.0, 10.0, 1.0, 9.0)
        ));
        // Y disabled: take zoomed X, keep current Y.
        assert!(close(
            constrain_zoom_axes(zoomed, current, true, false),
            (2.0, 8.0, 0.0, 100.0)
        ));
        // Both disabled: a box zoom is a full no-op (keeps current view).
        assert!(close(
            constrain_zoom_axes(zoomed, current, false, false),
            current
        ));
    }

    #[test]
    fn wheel_factor_direction_and_neutral() {
        assert!(wheel_zoom_factor(100.0) < 1.0);
        assert!(wheel_zoom_factor(-100.0) > 1.0);
        assert!((wheel_zoom_factor(0.0) - 1.0).abs() <= 1e-12);
    }

    #[test]
    fn wheel_factor_one_notch_is_exactly_1_1() {
        // silx `_onWheel`: one wheel step is exactly x1.1
        // (PlotInteraction.py:1912-1913). One egui notch = 40.0 points of
        // smooth_scroll_delta (Line 1.0 x line_scroll_speed native default).
        let notch = 40.0_f32;
        // Scroll up one notch: span factor 1/1.1 (a x1.1 zoom-in).
        assert!((wheel_zoom_factor(notch) - 1.0 / 1.1).abs() <= 1e-12);
        // Scroll down one notch: span factor 1.1 (a x1.1 zoom-out).
        assert!((wheel_zoom_factor(-notch) - 1.1).abs() <= 1e-12);
        // N notches compose to 1.1^N.
        assert!((wheel_zoom_factor(-3.0 * notch) - 1.1f64.powi(3)).abs() <= 1e-9);
    }

    #[test]
    fn wheel_factor_split_deltas_compose_to_one_notch() {
        // egui's scroll smoothing splits a notch's 40.0 points across frames
        // sum-conservingly; the exponential factor must compose to exactly
        // one silx step regardless of the split.
        let parts = [13.0_f32, 9.5, 11.5, 6.0]; // sums to 40.0
        let composed: f64 = parts.iter().map(|&p| wheel_zoom_factor(p)).product();
        assert!(
            (composed - 1.0 / 1.1).abs() <= 1e-12,
            "composed {composed} != 1/1.1"
        );
    }

    #[test]
    fn validity_rejects_collapsed_or_inverted() {
        assert!(is_valid((0.0, 1.0, 0.0, 1.0)));
        assert!(!is_valid((1.0, 1.0, 0.0, 1.0)));
        assert!(!is_valid((0.0, 1.0, 2.0, 1.0)));
    }

    use crate::core::transform::Transform;

    // 100×100 px area mapping data [0,10]×[0,10]; 1 data unit = 10 px.
    fn pick_transform() -> Transform {
        Transform::new(0.0, 10.0, 0.0, 10.0, area_100())
    }

    fn di(data: (f64, f64), pixel: (f32, f32)) -> DrawInput {
        DrawInput { data, pixel }
    }

    #[test]
    fn rectangle_two_point_bounds() {
        // Drag from (8,1) to (2,9): finished rectangle is the ordered lower-left
        // corner plus width/height (silx prepareDrawingSignal "rectangle").
        let mut s = DrawState::new(DrawMode::Rectangle);
        // A rectangle press starts the draw but emits nothing (silx beginSelect).
        assert!(s.on_press(di((8.0, 1.0), (80.0, 90.0))).is_none());
        assert!(matches!(
            s.on_move(di((2.0, 9.0), (20.0, 10.0))),
            Some(DrawEvent::InProgress {
                mode: DrawMode::Rectangle,
                ..
            })
        ));
        let fin = s
            .on_release(di((2.0, 9.0), (20.0, 10.0)))
            .expect("finished");
        match fin {
            DrawEvent::Finished {
                mode: DrawMode::Rectangle,
                params:
                    DrawParams::Rectangle {
                        x,
                        y,
                        width,
                        height,
                    },
            } => {
                assert_eq!((x, y), (2.0, 1.0));
                assert_eq!((width, height), (6.0, 8.0));
            }
            other => panic!("{other:?}"),
        }
        assert!(!s.is_active());
    }

    #[test]
    fn line_two_point_endpoints() {
        let mut s = DrawState::new(DrawMode::Line);
        s.on_press(di((1.0, 2.0), (10.0, 20.0)));
        let fin = s
            .on_release(di((3.0, 4.0), (30.0, 40.0)))
            .expect("finished");
        assert_eq!(
            fin,
            DrawEvent::Finished {
                mode: DrawMode::Line,
                params: DrawParams::Line {
                    start: (1.0, 2.0),
                    end: (3.0, 4.0),
                },
            }
        );
    }

    #[test]
    fn ellipse_params_from_drag() {
        // Axis-aligned drag (center to a point straight along X): degenerate
        // ellipse returns the raw offsets (silx early return when y offset 0).
        let (a, b) = ellipse_semi_axes((0.0, 0.0), (5.0, 0.0));
        assert_eq!((a, b), (5.0, 0.0));
        // A real off-axis point: the point lies on the resulting ellipse, i.e.
        // x^2/a^2 + y^2/b^2 == 1.
        let center = (1.0, 2.0);
        let point = (4.0, 6.0);
        let (a, b) = ellipse_semi_axes(center, point);
        let dx = point.0 - center.0;
        let dy = point.1 - center.1;
        let on_ellipse = dx * dx / (a * a) + dy * dy / (b * b);
        assert!(
            (on_ellipse - 1.0).abs() <= 1e-9,
            "a={a} b={b} -> {on_ellipse}"
        );
        // The longer semi-axis follows the larger offset: here |dy| (4) > |dx|
        // (3), so the Y semi-axis b is the larger one.
        assert!(b > a, "a={a} b={b}");

        // Through the state machine the finished event carries center + semi-axes.
        let mut s = DrawState::new(DrawMode::Ellipse);
        s.on_press(di(center, (10.0, 20.0)));
        let fin = s.on_release(di(point, (40.0, 60.0))).expect("finished");
        match fin {
            DrawEvent::Finished {
                mode: DrawMode::Ellipse,
                params:
                    DrawParams::Ellipse {
                        center: c,
                        semi_axes,
                    },
            } => {
                assert_eq!(c, center);
                assert!((semi_axes.0 - a).abs() <= 1e-12 && (semi_axes.1 - b).abs() <= 1e-12);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ellipse_preview_has_full_ring() {
        // The in-progress preview is a 27-point sampled ring around the center.
        let mut s = DrawState::new(DrawMode::Ellipse);
        s.on_press(di((0.0, 0.0), (0.0, 0.0)));
        let ev = s.on_move(di((4.0, 6.0), (40.0, 60.0))).expect("progress");
        match ev {
            DrawEvent::InProgress { points, .. } => {
                assert_eq!(points.len(), 27);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn hline_vline_capture_one_coordinate() {
        // HLine captures the data Y of the release.
        let mut s = DrawState::new(DrawMode::HLine);
        assert!(matches!(
            s.on_press(di((3.0, 7.0), (30.0, 70.0))),
            Some(DrawEvent::InProgress { .. })
        ));
        let fin = s
            .on_release(di((9.0, 7.5), (90.0, 75.0)))
            .expect("finished");
        assert_eq!(
            fin,
            DrawEvent::Finished {
                mode: DrawMode::HLine,
                params: DrawParams::HLine { y: 7.5 },
            }
        );
        // VLine captures the data X of the release.
        let mut s = DrawState::new(DrawMode::VLine);
        s.on_press(di((3.0, 7.0), (30.0, 70.0)));
        let fin = s
            .on_release(di((4.2, 1.0), (42.0, 10.0)))
            .expect("finished");
        assert_eq!(
            fin,
            DrawEvent::Finished {
                mode: DrawMode::VLine,
                params: DrawParams::VLine { x: 4.2 },
            }
        );
    }

    #[test]
    fn polygon_accumulates_vertices_and_closes_on_first_point() {
        let mut s = DrawState::new(DrawMode::Polygon).with_close_threshold(4.0);
        // First press anchors the polygon at (0,0)/pixel(0,0).
        s.on_press(di((0.0, 0.0), (0.0, 0.0)));
        // Release far from start -> appends a vertex (now 3 entries: seed pair
        // updated + appended).
        s.on_release(di((10.0, 0.0), (100.0, 0.0)));
        s.on_release(di((10.0, 10.0), (100.0, 100.0)));
        // Move the cursor near the first point (within 4px) -> snaps to first.
        s.on_move(di((0.05, 0.05), (2.0, 3.0)));
        // Release near the first point with >2 points -> closes.
        let fin = s.on_release(di((0.05, 0.05), (2.0, 3.0))).expect("closed");
        match fin {
            DrawEvent::Finished {
                mode: DrawMode::Polygon,
                params: DrawParams::Polygon { vertices },
            } => {
                // Open ring: the three distinct corners, first not duplicated.
                assert_eq!(vertices, vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]);
            }
            other => panic!("{other:?}"),
        }
        assert!(!s.is_active());
    }

    #[test]
    fn validate_closes_in_progress_polygon_anywhere() {
        // silx ClosePolygonInteractionAction / _validate: closes the polygon at
        // the committed vertices even though the cursor is nowhere near the first
        // point.
        let mut s = DrawState::new(DrawMode::Polygon).with_close_threshold(4.0);
        s.on_press(di((0.0, 0.0), (0.0, 0.0)));
        s.on_release(di((10.0, 0.0), (100.0, 0.0)));
        s.on_release(di((10.0, 10.0), (100.0, 100.0)));
        // Cursor parked far from the first point; validate still closes.
        s.on_move(di((5.0, 5.0), (50.0, 50.0)));
        let fin = s.validate().expect("validate closes");
        match fin {
            DrawEvent::Finished {
                mode: DrawMode::Polygon,
                params: DrawParams::Polygon { vertices },
            } => {
                // The committed ring (cursor tail dropped), like the snap-close path.
                assert_eq!(vertices, vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]);
            }
            other => panic!("{other:?}"),
        }
        assert!(!s.is_active());
    }

    #[test]
    fn validate_is_noop_without_a_real_polygon_or_in_other_modes() {
        // Idle polygon: nothing to close.
        let mut s = DrawState::new(DrawMode::Polygon);
        assert!(s.validate().is_none());

        // Only the seeded pair (len == 2, no appended vertex): same len > 2 gate
        // as the snap-close path, so validate does not close and leaves it active.
        s.on_press(di((0.0, 0.0), (0.0, 0.0)));
        assert!(s.validate().is_none());
        assert!(s.is_active());

        // Wrong mode: a rectangle draw is never closed by validate.
        let mut r = DrawState::new(DrawMode::Rectangle);
        r.on_press(di((0.0, 0.0), (0.0, 0.0)));
        assert!(r.validate().is_none());
        assert!(r.is_active());
    }

    #[test]
    fn polygon_does_not_close_with_two_points() {
        // Boundary: a release near the first point but with only the seed pair
        // (len == 2, no appended vertex) must NOT close (silx len > 2 gate).
        let mut s = DrawState::new(DrawMode::Polygon).with_close_threshold(4.0);
        s.on_press(di((0.0, 0.0), (0.0, 0.0)));
        // Release exactly on the first point: len is still 2, so no close; it is
        // treated as a near-previous replace, not an append.
        let ev = s.on_release(di((0.0, 0.0), (0.0, 0.0))).expect("progress");
        assert!(matches!(ev, DrawEvent::InProgress { .. }));
        assert!(s.is_active());
    }

    #[test]
    fn polygon_replaces_near_previous_vertex() {
        // A release within threshold of the previous committed vertex replaces
        // the tracked last vertex instead of appending (silx 581-588).
        let mut s = DrawState::new(DrawMode::Polygon).with_close_threshold(4.0);
        s.on_press(di((0.0, 0.0), (0.0, 0.0)));
        // First real release far from the seed -> append: the seeded tail is
        // overwritten with (10,0) and a new (10,0) tail is pushed, so the ring is
        // [first, (10,0), (10,0)] (silx's enterState seeds the pair, onRelease
        // appends the cursor tail).
        s.on_release(di((10.0, 0.0), (100.0, 0.0)));
        // Second release within 4px of the previous committed vertex (100,0) ->
        // replace the cursor tail in place, no append.
        s.on_release(di((10.2, 0.1), (102.0, 1.0)));
        let preview = s.preview().expect("active");
        // Ring length unchanged at 3; the tail was replaced, not appended.
        assert_eq!(preview.len(), 3);
        assert_eq!(preview[1], (10.0, 0.0));
        assert_eq!(preview[2], (10.2, 0.1));
    }

    #[test]
    fn freehand_accumulates_and_dedups() {
        let mut s = DrawState::new(DrawMode::FreeHand);
        // Press seeds the first vertex.
        assert!(matches!(
            s.on_press(di((0.0, 0.0), (0.0, 0.0))),
            Some(DrawEvent::InProgress {
                mode: DrawMode::FreeHand,
                ..
            })
        ));
        s.on_move(di((1.0, 1.0), (10.0, 10.0)));
        // Repeated identical point is skipped.
        s.on_move(di((1.0, 1.0), (10.0, 10.0)));
        s.on_move(di((2.0, 0.0), (20.0, 0.0)));
        let fin = s
            .on_release(di((3.0, 1.0), (30.0, 10.0)))
            .expect("finished");
        match fin {
            DrawEvent::Finished {
                mode: DrawMode::FreeHand,
                params: DrawParams::FreeHand { vertices },
            } => {
                assert_eq!(
                    vertices,
                    vec![(0.0, 0.0), (1.0, 1.0), (2.0, 0.0), (3.0, 1.0)]
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn freehand_release_does_not_duplicate_last() {
        // Boundary: releasing at the same point as the last accumulated vertex
        // does not duplicate it (silx isLast append-if-different).
        let mut s = DrawState::new(DrawMode::FreeHand);
        s.on_press(di((0.0, 0.0), (0.0, 0.0)));
        s.on_move(di((1.0, 1.0), (10.0, 10.0)));
        let fin = s
            .on_release(di((1.0, 1.0), (10.0, 10.0)))
            .expect("finished");
        match fin {
            DrawEvent::Finished {
                params: DrawParams::FreeHand { vertices },
                ..
            } => assert_eq!(vertices, vec![(0.0, 0.0), (1.0, 1.0)]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn cancel_drops_in_progress_draw() {
        let mut s = DrawState::new(DrawMode::Polygon);
        s.on_press(di((0.0, 0.0), (0.0, 0.0)));
        assert!(s.is_active());
        s.cancel();
        assert!(!s.is_active());
        assert!(s.preview().is_none());
    }

    #[test]
    fn idle_move_and_release_are_noops() {
        // Before any press, move/release emit nothing for two-point modes.
        let mut s = DrawState::new(DrawMode::Rectangle);
        assert!(s.on_move(di((1.0, 1.0), (10.0, 10.0))).is_none());
        assert!(s.on_release(di((1.0, 1.0), (10.0, 10.0))).is_none());
    }

    #[test]
    fn fill_mode_and_style_defaults() {
        assert_eq!(FillMode::default(), FillMode::Hatch);
        let s = SelectionStyle::default();
        assert_eq!(s.fill, FillMode::Hatch);
        let s = SelectionStyle::new(FillMode::Solid, egui::Color32::RED);
        assert_eq!(s.fill, FillMode::Solid);
        assert_eq!(s.color, egui::Color32::RED);
    }

    #[test]
    fn hatch_lines_cover_rect() {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(40.0, 40.0));
        let lines = hatch_lines(rect, 10.0);
        // Diagonal lines spanning a 40x40 box at 10px spacing produce several
        // segments, each with both endpoints on the rect boundary.
        assert!(!lines.is_empty());
        for (a, b) in &lines {
            assert!(rect.contains(*a) && rect.contains(*b), "{a:?} {b:?}");
            // Slope +1 within tolerance (segment is a 45-degree line).
            let dx = b.x - a.x;
            let dy = b.y - a.y;
            assert!((dx.abs() - dy.abs()).abs() <= 1e-3, "dx={dx} dy={dy}");
        }
    }

    #[test]
    fn hatch_lines_degenerate_inputs_empty() {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(40.0, 40.0));
        // Non-positive spacing -> no lines.
        assert!(hatch_lines(rect, 0.0).is_empty());
        assert!(hatch_lines(rect, -5.0).is_empty());
        // Degenerate rect -> no lines.
        let zero = Rect::from_min_max(pos2(0.0, 0.0), pos2(0.0, 0.0));
        assert!(hatch_lines(zero, 10.0).is_empty());
    }

    #[test]
    fn cursor_shape_per_edge() {
        // Non-inverted axes: data orientation == screen orientation.
        let t = pick_transform();
        // Horizontal-only edges -> SizeHor.
        assert_eq!(cursor_for_edge(RoiEdge::Left, &t), CursorShape::SizeHor);
        assert_eq!(cursor_for_edge(RoiEdge::Right, &t), CursorShape::SizeHor);
        // Vertical-only edges -> SizeVer.
        assert_eq!(cursor_for_edge(RoiEdge::Top, &t), CursorShape::SizeVer);
        assert_eq!(cursor_for_edge(RoiEdge::Bottom, &t), CursorShape::SizeVer);
        // Diagonal corners: TL/BR share the ↘↖ axis, TR/BL the ↗↙ axis.
        assert_eq!(cursor_for_edge(RoiEdge::TopLeft, &t), CursorShape::SizeNwse);
        assert_eq!(
            cursor_for_edge(RoiEdge::BottomRight, &t),
            CursorShape::SizeNwse
        );
        assert_eq!(
            cursor_for_edge(RoiEdge::TopRight, &t),
            CursorShape::SizeNesw
        );
        assert_eq!(
            cursor_for_edge(RoiEdge::BottomLeft, &t),
            CursorShape::SizeNesw
        );
        // Free vertex -> SizeAll.
        assert_eq!(
            cursor_for_edge(RoiEdge::Vertex(0), &t),
            CursorShape::SizeAll
        );
        assert_eq!(
            cursor_for_edge(RoiEdge::Vertex(7), &t),
            CursorShape::SizeAll
        );
    }

    #[test]
    fn cursor_shape_corner_diagonal_flips_under_single_axis_inversion() {
        // The corner cursor reflects the SCREEN diagonal. On an inverted-Y image
        // plot the data TopLeft corner (x.min, y.max) is drawn at screen
        // bottom-left, whose diagonal is ↗↙ (SizeNesw), not ↘↖ — so the corner
        // cursors swap. Sides stay axis-symmetric. (This was the user-reported
        // "코너 화살표 방향이 90도 틀어짐" under inverted Y.)
        let mut inv_y = pick_transform();
        inv_y.y.inverted = true;
        assert_eq!(
            cursor_for_edge(RoiEdge::TopLeft, &inv_y),
            CursorShape::SizeNesw
        );
        assert_eq!(
            cursor_for_edge(RoiEdge::BottomRight, &inv_y),
            CursorShape::SizeNesw
        );
        assert_eq!(
            cursor_for_edge(RoiEdge::TopRight, &inv_y),
            CursorShape::SizeNwse
        );
        assert_eq!(
            cursor_for_edge(RoiEdge::BottomLeft, &inv_y),
            CursorShape::SizeNwse
        );
        // Sides unaffected by inversion.
        assert_eq!(cursor_for_edge(RoiEdge::Left, &inv_y), CursorShape::SizeHor);
        assert_eq!(cursor_for_edge(RoiEdge::Top, &inv_y), CursorShape::SizeVer);

        // Both axes inverted: the two mirrors cancel, diagonals return to the
        // non-inverted mapping.
        let mut inv_xy = pick_transform();
        inv_xy.x.inverted = true;
        inv_xy.y.inverted = true;
        assert_eq!(
            cursor_for_edge(RoiEdge::TopLeft, &inv_xy),
            CursorShape::SizeNwse
        );
        assert_eq!(
            cursor_for_edge(RoiEdge::TopRight, &inv_xy),
            CursorShape::SizeNesw
        );
    }

    #[test]
    fn cursor_for_grab_defaults_when_nothing_grabbed() {
        let t = pick_transform();
        // None -> Default (nothing under the cursor).
        assert_eq!(cursor_for_grab(None, &t), CursorShape::Default);
        // Some(edge) -> that edge's shape.
        assert_eq!(
            cursor_for_grab(Some(RoiEdge::Left), &t),
            CursorShape::SizeHor
        );
    }

    #[test]
    fn cursor_shape_maps_to_egui_icon() {
        assert_eq!(
            CursorShape::SizeHor.to_egui(),
            egui::CursorIcon::ResizeHorizontal
        );
        assert_eq!(
            CursorShape::SizeVer.to_egui(),
            egui::CursorIcon::ResizeVertical
        );
        assert_eq!(
            CursorShape::SizeNwse.to_egui(),
            egui::CursorIcon::ResizeNwSe
        );
        assert_eq!(
            CursorShape::SizeNesw.to_egui(),
            egui::CursorIcon::ResizeNeSw
        );
        assert_eq!(CursorShape::SizeAll.to_egui(), egui::CursorIcon::Move);
        assert_eq!(CursorShape::Default.to_egui(), egui::CursorIcon::Default);
    }

    #[test]
    fn mouse_button_maps_from_egui() {
        assert_eq!(
            MouseButton::from_egui(egui::PointerButton::Primary),
            MouseButton::Left
        );
        assert_eq!(
            MouseButton::from_egui(egui::PointerButton::Middle),
            MouseButton::Middle
        );
        assert_eq!(
            MouseButton::from_egui(egui::PointerButton::Secondary),
            MouseButton::Right
        );
        // egui's extra buttons collapse to Right (silx has only three buttons).
        assert_eq!(
            MouseButton::from_egui(egui::PointerButton::Extra1),
            MouseButton::Right
        );
    }

    #[test]
    fn pointer_event_maps_pixel_to_data() {
        // 100x100 px over data [0,10]: center pixel (50,50) -> data (5,5).
        let t = pick_transform();
        let ev = PlotPointerEvent::clicked(MouseButton::Left, &t, pos2(50.0, 50.0));
        match ev {
            PlotPointerEvent::Clicked {
                button,
                data,
                pixel,
            } => {
                assert_eq!(button, MouseButton::Left);
                assert!(
                    (data.0 - 5.0).abs() <= 1e-9 && (data.1 - 5.0).abs() <= 1e-9,
                    "{data:?}"
                );
                assert_eq!(pixel, (50.0, 50.0));
            }
            other => panic!("expected Clicked, got {other:?}"),
        }
        // Corner: bottom-left pixel (0,100) -> data (0,0).
        let ev = PlotPointerEvent::double_clicked(MouseButton::Left, &t, pos2(0.0, 100.0));
        match ev {
            PlotPointerEvent::DoubleClicked { data, pixel, .. } => {
                assert!(data.0.abs() <= 1e-9 && data.1.abs() <= 1e-9, "{data:?}");
                assert_eq!(pixel, (0.0, 100.0));
            }
            other => panic!("expected DoubleClicked, got {other:?}"),
        }
    }

    #[test]
    fn pointer_event_moved_carries_optional_button() {
        let t = pick_transform();
        // Bare hover: no held button.
        let ev = PlotPointerEvent::moved(None, &t, pos2(50.0, 50.0));
        assert!(matches!(ev, PlotPointerEvent::Moved { button: None, .. }));
        // Held button during a move.
        let ev = PlotPointerEvent::moved(Some(MouseButton::Left), &t, pos2(50.0, 50.0));
        assert!(matches!(
            ev,
            PlotPointerEvent::Moved {
                button: Some(MouseButton::Left),
                ..
            }
        ));
    }

    #[test]
    fn limits_changed_carries_ranges() {
        let ev = PlotPointerEvent::limits_changed((0.0, 10.0), (1.0, 5.0), Some((2.0, 8.0)));
        assert_eq!(
            ev,
            PlotPointerEvent::LimitsChanged {
                x: (0.0, 10.0),
                y: (1.0, 5.0),
                y2: Some((2.0, 8.0)),
            }
        );
        // No y2 axis -> None.
        let ev = PlotPointerEvent::limits_changed((0.0, 10.0), (1.0, 5.0), None);
        assert!(matches!(
            ev,
            PlotPointerEvent::LimitsChanged { y2: None, .. }
        ));
    }

    #[test]
    fn nearest_point_picks_closest_within_threshold() {
        let t = pick_transform();
        let pts = [(0.0, 0.0), (5.0, 5.0), (10.0, 10.0)];
        // (5,5) -> pixel (50, 50). Cursor a few px away picks index 1.
        let pick = nearest_point(&pts, &t, pos2(52.0, 47.0), 6.0).expect("a pick");
        assert_eq!(pick.index, 1);
        assert_eq!((pick.x, pick.y), (5.0, 5.0));
        // Nothing within threshold -> None.
        assert!(nearest_point(&pts, &t, pos2(52.0, 47.0), 2.0).is_none());
        assert!(nearest_point(&[], &t, pos2(0.0, 0.0), 100.0).is_none());
    }

    #[test]
    fn clamp_axis_leaves_normal_range_untouched() {
        // A normal in-range linear range is returned unchanged.
        assert_eq!(clamp_axis_limits(-3.0, 5.0, false), (-3.0, 5.0));
        // A normal in-range positive log range is returned unchanged.
        assert_eq!(clamp_axis_limits(1.0, 1000.0, true), (1.0, 1000.0));
    }

    #[test]
    fn clamp_axis_clamps_beyond_safe_values() {
        // Boundary: a max beyond FLOAT32_SAFE_MAX clamps to it.
        let (lo, hi) = clamp_axis_limits(0.0, 1e40, false);
        assert_eq!((lo, hi), (0.0, FLOAT32_SAFE_MAX));
        // Boundary: a min below FLOAT32_SAFE_MIN clamps to it (linear).
        let (lo, hi) = clamp_axis_limits(-1e40, 5.0, false);
        assert_eq!((lo, hi), (FLOAT32_SAFE_MIN, 5.0));
        // Boundary: a non-positive min on a log axis clamps up to FLOAT32_MINPOS.
        let (lo, hi) = clamp_axis_limits(-10.0, 1000.0, true);
        assert_eq!((lo, hi), (FLOAT32_MINPOS, 1000.0));
    }

    #[test]
    fn clamp_axis_swaps_inverted_bounds() {
        // Boundary: max < min after clamping is swapped to ordered.
        let (lo, hi) = clamp_axis_limits(5.0, -3.0, false);
        assert_eq!((lo, hi), (-3.0, 5.0));
    }

    #[test]
    fn clamp_axis_expands_equal_bounds() {
        // v == 0 -> (-0.1, 0.1).
        assert_eq!(clamp_axis_limits(0.0, 0.0, false), (-0.1, 0.1));
        // v > 0 -> (v*0.9, v*1.1).
        let (lo, hi) = clamp_axis_limits(10.0, 10.0, false);
        assert!(
            (lo - 9.0).abs() <= 1e-12 && (hi - 11.0).abs() <= 1e-12,
            "{lo},{hi}"
        );
        // v < 0 -> (v*1.1, v*0.9).
        let (lo, hi) = clamp_axis_limits(-10.0, -10.0, false);
        assert!(
            (lo - -11.0).abs() <= 1e-12 && (hi - -9.0).abs() <= 1e-12,
            "{lo},{hi}"
        );
    }

    #[test]
    fn clamp_axis_nan_falls_to_lower_bound() {
        // Boundary: a NaN bound maps to the lower bound, keeping the range finite.
        let (lo, hi) = clamp_axis_limits(f64::NAN, 5.0, false);
        assert!(lo.is_finite() && hi.is_finite());
        assert_eq!((lo, hi), (FLOAT32_SAFE_MIN, 5.0));
        // Both NaN -> both fall to lower, then equal-expansion kicks in.
        let (lo, hi) = clamp_axis_limits(f64::NAN, f64::NAN, true);
        assert!(lo.is_finite() && hi.is_finite() && hi > lo, "{lo},{hi}");
    }

    #[test]
    fn clamp_limits_clamps_both_axes() {
        let out = clamp_limits((-1e40, 1e40, 0.0, 0.0), false, false);
        assert_eq!(out.0, FLOAT32_SAFE_MIN);
        assert_eq!(out.1, FLOAT32_SAFE_MAX);
        // Degenerate y expands.
        assert_eq!((out.2, out.3), (-0.1, 0.1));
    }

    #[test]
    fn image_index_maps_cursor_to_pixel() {
        // 10×10 image, origin (0,0), unit scale, over data [0,10] in a 100px area.
        let t = pick_transform();
        // Data (0,0) is bottom-left -> pixel (0, 100). Pixel (5,95) -> data ~(0.5, 0.5)
        // -> col 0, row 0.
        assert_eq!(
            image_index(&t, (0.0, 0.0), (1.0, 1.0), (10, 10), pos2(5.0, 95.0)),
            Some((0, 0))
        );
        // Center pixel (55, 45) -> data (5.5, 5.5) -> col 5, row 5.
        assert_eq!(
            image_index(&t, (0.0, 0.0), (1.0, 1.0), (10, 10), pos2(55.0, 45.0)),
            Some((5, 5))
        );
        // Outside the data area maps outside the image.
        assert!(image_index(&t, (0.0, 0.0), (1.0, 1.0), (10, 10), pos2(-5.0, 50.0)).is_none());
    }

    #[test]
    fn marker_cursor_reflects_drag_dof() {
        // VLine moves in X only -> SizeHor.
        assert_eq!(marker_cursor(&Marker::vline(3.0)), CursorShape::SizeHor);
        // HLine moves in Y only -> SizeVer.
        assert_eq!(marker_cursor(&Marker::hline(3.0)), CursorShape::SizeVer);
        // Free point moves in both -> SizeAll.
        let p = Marker::point(1.0, 2.0);
        assert_eq!(marker_cursor(&p), CursorShape::SizeAll);
        // Point + Horizontal constraint pins X, leaving Y free -> SizeVer.
        let ph = Marker::point(1.0, 2.0).with_constraint(MarkerConstraint::Horizontal);
        assert_eq!(marker_cursor(&ph), CursorShape::SizeVer);
        // Point + Vertical constraint pins Y, leaving X free -> SizeHor.
        let pv = Marker::point(1.0, 2.0).with_constraint(MarkerConstraint::Vertical);
        assert_eq!(marker_cursor(&pv), CursorShape::SizeHor);
    }

    #[test]
    fn marker_at_returns_topmost_draggable_index() {
        let t = pick_transform();
        // Two draggable points stacked at the same spot (data (5,5) -> pixel
        // (50,50)); the later one (higher z, drawn last) wins.
        let markers = vec![
            Marker::point(5.0, 5.0).with_draggable(true),
            Marker::point(5.0, 5.0).with_draggable(true),
        ];
        assert_eq!(marker_at(&markers, &t, pos2(50.0, 50.0)), Some(1));
    }

    #[test]
    fn marker_at_skips_non_draggable_even_when_hit() {
        let t = pick_transform();
        // The topmost marker is hit but not draggable; it is skipped and the
        // draggable one below it is returned.
        let markers = vec![
            Marker::point(5.0, 5.0).with_draggable(true),
            Marker::point(5.0, 5.0), // is_draggable == false
        ];
        assert_eq!(marker_at(&markers, &t, pos2(50.0, 50.0)), Some(0));
    }

    #[test]
    fn marker_at_none_when_nothing_hit() {
        let t = pick_transform();
        let markers = vec![Marker::point(5.0, 5.0).with_draggable(true)];
        // Cursor far from the marker: no hit.
        assert_eq!(marker_at(&markers, &t, pos2(90.0, 10.0)), None);
        // Empty list: no hit.
        assert_eq!(marker_at(&[], &t, pos2(50.0, 50.0)), None);
    }

    // --- on-plot ROI creation: roi_draw_mode + roi_from_draw + DrawMode::Point ---

    #[test]
    fn roi_draw_mode_per_kind() {
        use DrawMode as D;
        use RoiDrawKind as K;
        assert_eq!(roi_draw_mode(K::Rect), D::Rectangle);
        assert_eq!(roi_draw_mode(K::Ellipse), D::Ellipse);
        assert_eq!(roi_draw_mode(K::Polygon), D::Polygon);
        assert_eq!(roi_draw_mode(K::Point), D::Point);
        assert_eq!(roi_draw_mode(K::Cross), D::Point);
        // The six 2-point "line"-drag kinds.
        assert_eq!(roi_draw_mode(K::Line), D::Line);
        assert_eq!(roi_draw_mode(K::Circle), D::Line);
        assert_eq!(roi_draw_mode(K::HRange), D::Line);
        assert_eq!(roi_draw_mode(K::VRange), D::Line);
        assert_eq!(roi_draw_mode(K::HLine), D::HLine);
        assert_eq!(roi_draw_mode(K::VLine), D::VLine);
        assert_eq!(roi_draw_mode(K::Arc), D::Line);
        assert_eq!(roi_draw_mode(K::Band), D::Line);
    }

    #[test]
    fn draw_mode_point_finishes_on_press() {
        // silx _plotShape "point": a single press finishes immediately with the
        // captured data position; no move/release needed, phase stays idle.
        let mut s = DrawState::new(DrawMode::Point);
        let ev = s.on_press(di((3.5, 7.25), (35.0, 27.5))).expect("finished");
        assert_eq!(
            ev,
            DrawEvent::Finished {
                mode: DrawMode::Point,
                params: DrawParams::Point { x: 3.5, y: 7.25 },
            }
        );
        // Not active afterwards; move/release are no-ops; no preview.
        assert!(!s.is_active());
        assert!(s.on_move(di((9.0, 9.0), (90.0, 90.0))).is_none());
        assert!(s.on_release(di((9.0, 9.0), (90.0, 90.0))).is_none());
        assert!(s.preview().is_none());
    }

    // roi_from_draw: ONE assertion per kind (all 11), exact geometry.

    #[test]
    fn roi_from_draw_rect() {
        let p = DrawParams::Rectangle {
            x: 2.0,
            y: 1.0,
            width: 6.0,
            height: 8.0,
        };
        assert_eq!(
            roi_from_draw(RoiDrawKind::Rect, &p),
            Some(Roi::Rect {
                x: (2.0, 8.0),
                y: (1.0, 9.0),
            })
        );
    }

    #[test]
    fn roi_from_draw_line() {
        let p = DrawParams::Line {
            start: (1.0, 2.0),
            end: (3.0, 4.0),
        };
        assert_eq!(
            roi_from_draw(RoiDrawKind::Line, &p),
            Some(Roi::Line {
                start: (1.0, 2.0),
                end: (3.0, 4.0),
            })
        );
    }

    #[test]
    fn roi_from_draw_polygon() {
        let p = DrawParams::Polygon {
            vertices: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0)],
        };
        assert_eq!(
            roi_from_draw(RoiDrawKind::Polygon, &p),
            Some(Roi::Polygon {
                vertices: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0)],
            })
        );
    }

    #[test]
    fn roi_from_draw_point() {
        let p = DrawParams::Point { x: 5.0, y: 6.0 };
        assert_eq!(
            roi_from_draw(RoiDrawKind::Point, &p),
            Some(Roi::Point { x: 5.0, y: 6.0 })
        );
    }

    #[test]
    fn roi_from_draw_cross() {
        // Cross consumes the same Point params, producing a Cross center.
        let p = DrawParams::Point { x: 5.0, y: 6.0 };
        assert_eq!(
            roi_from_draw(RoiDrawKind::Cross, &p),
            Some(Roi::Cross { center: (5.0, 6.0) })
        );
    }

    #[test]
    fn roi_from_draw_ellipse_radii_are_semi_axes() {
        let p = DrawParams::Ellipse {
            center: (1.0, 2.0),
            semi_axes: (4.0, 2.5),
        };
        assert_eq!(
            roi_from_draw(RoiDrawKind::Ellipse, &p),
            Some(Roi::Ellipse {
                center: (1.0, 2.0),
                radii: (4.0, 2.5),
                orientation: 0.0,
            })
        );
    }

    #[test]
    fn roi_from_draw_circle_radius_is_distance() {
        // silx CircleROI._setRay: center = start, radius = |end - start|.
        let p = DrawParams::Line {
            start: (1.0, 1.0),
            end: (4.0, 5.0), // distance = sqrt(9+16) = 5
        };
        match roi_from_draw(RoiDrawKind::Circle, &p).expect("circle") {
            Roi::Circle { center, radius } => {
                assert_eq!(center, (1.0, 1.0));
                assert!((radius - 5.0).abs() <= 1e-12, "{radius}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn roi_from_draw_hrange_orders_ys() {
        // y descending in the drag -> ordered (min, max).
        let p = DrawParams::Line {
            start: (1.0, 7.0),
            end: (9.0, 3.0),
        };
        assert_eq!(
            roi_from_draw(RoiDrawKind::HRange, &p),
            Some(Roi::HRange { y: (3.0, 7.0) })
        );
    }

    #[test]
    fn roi_from_draw_vrange_orders_xs() {
        // x descending in the drag -> ordered (min, max).
        let p = DrawParams::Line {
            start: (8.0, 1.0),
            end: (2.0, 9.0),
        };
        assert_eq!(
            roi_from_draw(RoiDrawKind::VRange, &p),
            Some(Roi::VRange { x: (2.0, 8.0) })
        );
    }

    #[test]
    fn roi_from_draw_hline_vline_capture_the_single_position() {
        // HLine: the captured row y becomes a single-position line.
        assert_eq!(
            roi_from_draw(RoiDrawKind::HLine, &DrawParams::HLine { y: 4.5 }),
            Some(Roi::HLine { y: 4.5 })
        );
        // VLine: the captured column x.
        assert_eq!(
            roi_from_draw(RoiDrawKind::VLine, &DrawParams::VLine { x: -2.0 }),
            Some(Roi::VLine { x: -2.0 })
        );
        // Mismatched params (HLine kind with a Line drag) cannot occur and is dropped.
        assert_eq!(
            roi_from_draw(
                RoiDrawKind::HLine,
                &DrawParams::Line {
                    start: (0.0, 0.0),
                    end: (1.0, 1.0)
                }
            ),
            None
        );
    }

    #[test]
    fn roi_from_draw_band_default_width_is_tenth_of_length() {
        // silx BandGeometry.create default width = 0.1 * |end - begin|.
        let p = DrawParams::Line {
            start: (0.0, 0.0),
            end: (10.0, 0.0), // length 10 -> width 1.0
        };
        match roi_from_draw(RoiDrawKind::Band, &p).expect("band") {
            Roi::Band { begin, end, width } => {
                assert_eq!(begin, (0.0, 0.0));
                assert_eq!(end, (10.0, 0.0));
                assert!((width - 1.0).abs() <= 1e-12, "{width}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn roi_from_draw_arc_matches_silx_default() {
        // Faithful silx ArcROI default from diameter points (0,0)->(4,0).
        // Reference values computed by replaying ArcROI.setFirstShapePoints +
        // _createGeometryFromControlPoints + getGeometry (items/_arc_roi.py).
        let p = DrawParams::Line {
            start: (0.0, 0.0),
            end: (4.0, 0.0),
        };
        match roi_from_draw(RoiDrawKind::Arc, &p).expect("arc") {
            Roi::Arc {
                center,
                radius,
                weight,
                start_angle,
                end_angle,
            } => {
                assert!((center.0 - 2.0).abs() <= 1e-9, "cx={}", center.0);
                assert!(
                    (center.1 - (-0.9632309002009949)).abs() <= 1e-9,
                    "cy={}",
                    center.1
                );
                // silx stores (radius, weight); the reported inner/outer above
                // (1.8198679616369122, 2.619867961636912) derive as radius ∓ w/2.
                assert!(
                    (radius - 2.219867961636912).abs() <= 1e-9,
                    "radius={radius}"
                );
                assert!((weight - 0.8).abs() <= 1e-9, "weight={weight}");
                assert!(
                    (start_angle - 2.692760559012144).abs() <= 1e-9,
                    "start={start_angle}"
                );
                assert!(
                    (end_angle - 0.4488320945776491).abs() <= 1e-9,
                    "end={end_angle}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn arc_from_three_points_fits_the_circumcircle() {
        // Three points on the unit circle: (1,0), (0,1), (-1,0). weight 0 -> a
        // zero-thickness arc, center origin, radius 1, swept 0 -> pi through the
        // mid at pi/2 (silx _createGeometryFromControlPoints general branch).
        match arc_from_three_points((1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), 0.0) {
            Roi::Arc {
                center,
                radius,
                weight,
                start_angle,
                end_angle,
            } => {
                assert!(
                    center.0.abs() <= 1e-9 && center.1.abs() <= 1e-9,
                    "{center:?}"
                );
                assert!((radius - 1.0).abs() <= 1e-9, "radius={radius}");
                assert!(weight.abs() <= 1e-9, "weight={weight}");
                assert!(start_angle.abs() <= 1e-9, "start={start_angle}");
                assert!(
                    (end_angle - std::f64::consts::PI).abs() <= 1e-9,
                    "end={end_angle}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn arc_from_three_points_closed_circle_when_start_equals_end() {
        // start == end -> silx closed-circle branch: center at (start+mid)/2,
        // full 2pi sweep.
        match arc_from_three_points((2.0, 0.0), (-2.0, 0.0), (2.0, 0.0), 0.0) {
            Roi::Arc {
                center,
                radius,
                start_angle,
                end_angle,
                ..
            } => {
                assert!(
                    center.0.abs() <= 1e-9 && center.1.abs() <= 1e-9,
                    "{center:?}"
                );
                assert!((radius - 2.0).abs() <= 1e-9, "radius={radius}");
                assert!(
                    (end_angle - start_angle - std::f64::consts::TAU).abs() <= 1e-9,
                    "sweep={}",
                    end_angle - start_angle
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn arc_from_three_points_collinear_degenerates() {
        // Collinear triple -> silx makes a center-less rect; Roi::Arc can't hold
        // that, so it degenerates to a zero-radius arc at the start-end midpoint.
        match arc_from_three_points((0.0, 0.0), (1.0, 0.0), (2.0, 0.0), 0.5) {
            Roi::Arc {
                center,
                radius,
                weight,
                ..
            } => {
                assert_eq!(center, (1.0, 0.0));
                assert_eq!(radius, 0.0);
                assert_eq!(weight, 0.0);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn arc_three_point_drag_refits_center_radius_and_keeps_weight() {
        // Arc through (1,0),(0,1),(-1,0) with weight 0.4: center origin, radius 1.
        let arc = arc_from_three_points((1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), 0.4);
        // Dragging the mid handle from (0,1) up to (0,2) re-fits the circle
        // through (1,0),(0,2),(-1,0): center (0, 0.75), radius 1.25.
        let reshaped = arc_three_point_drag(&arc, ArcControlPoint::Mid, (0.0, 2.0));
        match reshaped {
            Roi::Arc {
                center,
                radius,
                weight,
                ..
            } => {
                assert!(center.0.abs() <= 1e-9, "cx={}", center.0);
                assert!((center.1 - 0.75).abs() <= 1e-9, "cy={}", center.1);
                assert!((radius - 1.25).abs() <= 1e-9, "r={radius}");
                // weight is preserved across the reshape.
                assert!((weight - 0.4).abs() <= 1e-9, "weight={weight}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn arc_three_point_drag_is_noop_on_non_arc() {
        let rect = Roi::Rect {
            x: (0.0, 1.0),
            y: (0.0, 1.0),
        };
        assert_eq!(
            arc_three_point_drag(&rect, ArcControlPoint::Start, (5.0, 5.0)),
            rect
        );
    }

    #[test]
    fn roi_key_action_maps_silx_bindings_only() {
        use egui::Key;
        // Enter validates; Delete/Backspace undo the last ROI.
        assert_eq!(
            roi_key_action(Key::Enter, false),
            Some(RoiKeyAction::Validate)
        );
        assert_eq!(
            roi_key_action(Key::Delete, false),
            Some(RoiKeyAction::UndoLast)
        );
        assert_eq!(
            roi_key_action(Key::Backspace, false),
            Some(RoiKeyAction::UndoLast)
        );
        // Z undoes only with the command modifier (silx Ctrl+Z).
        assert_eq!(roi_key_action(Key::Z, true), Some(RoiKeyAction::UndoLast));
        assert_eq!(roi_key_action(Key::Z, false), None);
        // silx binds no per-shape keys: "R for Rect" and friends are unbound.
        assert_eq!(roi_key_action(Key::R, false), None);
        assert_eq!(roi_key_action(Key::E, true), None);
    }

    #[test]
    fn roi_from_draw_rejects_impossible_pairings() {
        // roi_draw_mode never pairs Rect with a Line params, etc. — such a pair
        // is dropped rather than mis-built.
        let line = DrawParams::Line {
            start: (0.0, 0.0),
            end: (1.0, 1.0),
        };
        assert_eq!(roi_from_draw(RoiDrawKind::Rect, &line), None);
        assert_eq!(roi_from_draw(RoiDrawKind::Point, &line), None);
        // An HLine params (no creation kind ever produces it) is dropped.
        let hline = DrawParams::HLine { y: 3.0 };
        assert_eq!(roi_from_draw(RoiDrawKind::HRange, &hline), None);
    }

    // --- roi_grab_at: edge wins over body, topmost wins, outside -> None ---

    #[test]
    fn roi_grab_at_edge_then_body_then_none() {
        let t = pick_transform();
        // Rect data x[2,8] y[3,7] -> screen left 20, right 80, top 30, bottom 70.
        let rois = vec![ManagedRoi::new(Roi::Rect {
            x: (2.0, 8.0),
            y: (3.0, 7.0),
        })];
        // Near the left edge -> Edge(Left).
        assert_eq!(
            roi_grab_at(&rois, &t, pos2(21.0, 50.0), 4.0),
            Some((0, RoiGrab::Edge(RoiEdge::Left)))
        );
        // Inside the body, away from any edge -> Translate.
        assert_eq!(
            roi_grab_at(&rois, &t, pos2(50.0, 50.0), 4.0),
            Some((0, RoiGrab::Translate))
        );
        // Fully outside the shape -> None.
        assert_eq!(roi_grab_at(&rois, &t, pos2(95.0, 95.0), 4.0), None);
    }

    #[test]
    fn roi_grab_at_topmost_wins_with_overlap() {
        let t = pick_transform();
        // Two overlapping rects covering the cursor's body region; the second
        // (drawn last, highest z) wins the translate grab.
        let rois = vec![
            ManagedRoi::new(Roi::Rect {
                x: (1.0, 9.0),
                y: (1.0, 9.0),
            }),
            ManagedRoi::new(Roi::Rect {
                x: (2.0, 8.0),
                y: (2.0, 8.0),
            }),
        ];
        // Cursor at data (5,5) -> pixel (50,50): inside both, away from edges.
        assert_eq!(
            roi_grab_at(&rois, &t, pos2(50.0, 50.0), 4.0),
            Some((1, RoiGrab::Translate))
        );
    }

    #[test]
    fn roi_grab_at_skips_hidden_rois() {
        let t = pick_transform();
        // Same overlap as above, but the topmost rect is hidden (silx
        // setVisible(False), _roi_base.py:479-489): neither its edges nor its
        // body may grab, so the visible one underneath wins.
        let mut rois = vec![
            ManagedRoi::new(Roi::Rect {
                x: (1.0, 9.0),
                y: (1.0, 9.0),
            }),
            ManagedRoi::new(Roi::Rect {
                x: (2.0, 8.0),
                y: (2.0, 8.0),
            }),
        ];
        rois[1].visible = false;
        assert_eq!(
            roi_grab_at(&rois, &t, pos2(50.0, 50.0), 4.0),
            Some((0, RoiGrab::Translate))
        );
        // Near the hidden rect's left edge (data x=2 -> px 20): no edge grab.
        assert_eq!(
            roi_grab_at(&rois, &t, pos2(21.0, 50.0), 4.0),
            Some((0, RoiGrab::Translate)),
            "a hidden ROI's handles must not grab"
        );
        // Hide both: nothing grabs anywhere.
        rois[0].visible = false;
        assert_eq!(roi_grab_at(&rois, &t, pos2(50.0, 50.0), 4.0), None);
    }

    // --- Arc ThreePointMode vs PolarMode handle dispatch (rows 1180/1184) ---

    /// A managed Arc centered at (5,5), central radius 2 (inner 1, outer 3),
    /// sweeping 0..π — control points (7,5)/(5,7)/(3,5), polar weight handle at
    /// (5,8). `new` seeds the silx default ThreePointMode; `mode` overrides it.
    fn managed_arc(mode: RoiInteractionMode) -> ManagedRoi {
        let mut m = ManagedRoi::new(Roi::Arc {
            center: (5.0, 5.0),
            radius: 2.0,
            weight: 2.0,
            start_angle: 0.0,
            end_angle: std::f64::consts::PI,
        });
        assert!(m.set_interaction_mode(mode));
        m
    }

    #[test]
    fn arc_is_three_point_only_for_threepoint_arc() {
        assert!(arc_is_three_point(&managed_arc(
            RoiInteractionMode::ArcThreePoint
        )));
        assert!(!arc_is_three_point(&managed_arc(
            RoiInteractionMode::ArcPolar
        )));
        assert!(!arc_is_three_point(&ManagedRoi::new(Roi::Rect {
            x: (0.0, 1.0),
            y: (0.0, 1.0),
        })));
    }

    #[test]
    fn arc_three_point_handles_are_the_control_points() {
        let arc = Roi::Arc {
            center: (5.0, 5.0),
            radius: 2.0,
            weight: 2.0,
            start_angle: 0.0,
            end_angle: std::f64::consts::PI,
        };
        let h = arc_three_point_handles(&arc).expect("arc has three-point handles");
        assert!(h.iter().all(|h| h.kind == HandleKind::Vertex));
        let near =
            |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9;
        assert!(near(h[0].pos, [7.0, 5.0]), "start {:?}", h[0].pos);
        assert!(near(h[1].pos, [5.0, 7.0]), "mid {:?}", h[1].pos);
        assert!(near(h[2].pos, [3.0, 5.0]), "end {:?}", h[2].pos);
        assert!(
            arc_three_point_handles(&Roi::Rect {
                x: (0.0, 1.0),
                y: (0.0, 1.0),
            })
            .is_none()
        );
    }

    #[test]
    fn roi_grab_at_threepoint_grabs_control_points_polar_grabs_polar_handles() {
        let t = pick_transform();
        // Mid control point at data (5,7) -> pixel (50,30).
        let tp = vec![managed_arc(RoiInteractionMode::ArcThreePoint)];
        assert_eq!(
            roi_grab_at(&tp, &t, pos2(50.0, 30.0), 4.0),
            Some((0, RoiGrab::Edge(RoiEdge::Vertex(1)))),
            "ThreePointMode grabs the mid control point"
        );
        // The polar weight handle (outer radius, mid angle) at data (5,8) ->
        // pixel (50,20) is NOT a ThreePoint handle: the cursor falls through to
        // a body translate.
        assert_eq!(
            roi_grab_at(&tp, &t, pos2(50.0, 20.0), 4.0),
            Some((0, RoiGrab::Translate)),
            "ThreePointMode does not expose the polar weight handle"
        );
        // PolarMode grabs the weight handle at that same pixel.
        let polar = vec![managed_arc(RoiInteractionMode::ArcPolar)];
        assert_eq!(
            roi_grab_at(&polar, &t, pos2(50.0, 20.0), 4.0),
            Some((0, RoiGrab::Edge(RoiEdge::Vertex(1)))),
            "PolarMode grabs the polar weight handle"
        );
    }

    #[test]
    fn roi_apply_edge_drag_dispatches_on_interaction_mode() {
        let to = (5.0, 9.0); // drag the grabbed vertex upward

        // ThreePointMode: Vertex(1) is the mid control point; the edit re-fits
        // the circumcircle (matching `arc_three_point_drag`).
        let mut tp = managed_arc(RoiInteractionMode::ArcThreePoint);
        let base = tp.roi.clone();
        roi_apply_edge_drag(&mut tp, RoiEdge::Vertex(1), to);
        assert_eq!(
            tp.roi,
            arc_three_point_drag(&base, ArcControlPoint::Mid, to),
            "ThreePointMode reshapes via the circumcircle"
        );

        // PolarMode: Vertex(1) is the weight handle; the edit applies the polar
        // `move_edge` (changes the band thickness, fixed center).
        let mut polar = managed_arc(RoiInteractionMode::ArcPolar);
        let mut expected = polar.roi.clone();
        roi_apply_edge_drag(&mut polar, RoiEdge::Vertex(1), to);
        expected.move_edge(RoiEdge::Vertex(1), to);
        assert_eq!(polar.roi, expected, "PolarMode edits via move_edge");

        // The same grab + drag yields different geometry under the two modes.
        assert_ne!(
            tp.roi, polar.roi,
            "the interaction mode gates which edit a handle drag performs"
        );
    }
}
