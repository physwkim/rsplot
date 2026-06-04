//! Interaction math: pure functions mapping pointer input to new data limits.
//!
//! The widget reads egui input, converts it through the *current* on-screen
//! [`Transform`](crate::core::transform::Transform), and applies one of these to
//! produce the next limits. Because everything downstream (the wgpu ortho matrix
//! and the egui chrome) derives from those limits, the image, curve, and axes
//! move together with no extra bookkeeping (`doc/design.md` §4·§8·§11.6).
//!
//! Pointer-mode mapping lives in the widget; this module is just the geometry
//! for pan/zoom/pick math, kept pure so it is unit-testable.

use egui::{Pos2, Rect, Vec2};

use crate::core::marker::{Marker, MarkerConstraint, MarkerKind};
use crate::core::roi::RoiEdge;
use crate::core::transform::{Scale, Transform};

/// Data limits `(x_min, x_max, y_min, y_max)`.
pub type Limits = (f64, f64, f64, f64);

/// Float32 safe lower bound, mirroring silx `_utils/panzoom.py`
/// `FLOAT32_SAFE_MIN`. Linear-axis limits are kept inside `[FLOAT32_SAFE_MIN,
/// FLOAT32_SAFE_MAX]` so that span subtractions (`max - min`) do not overflow
/// float32 downstream in the shaders.
pub const FLOAT32_SAFE_MIN: f64 = -1e37;
/// Float32 safe upper bound, mirroring silx `FLOAT32_SAFE_MAX`.
pub const FLOAT32_SAFE_MAX: f64 = 1e37;
/// Smallest positive normal float32 (`numpy.finfo(numpy.float32).tiny`),
/// mirroring silx `FLOAT32_MINPOS`. The lower clamp bound on a log axis (where
/// the min must stay strictly positive).
pub const FLOAT32_MINPOS: f64 = 1.1754943508222875e-38;

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

/// Convert an egui wheel delta (`smooth_scroll_delta.y`, pixels) to a zoom
/// factor for [`zoom_about`]. Scrolling up (`> 0`) zooms in (`factor < 1`).
pub fn wheel_zoom_factor(scroll_y: f32) -> f64 {
    // Exponential so repeated notches compose multiplicatively and symmetrically.
    (-(scroll_y as f64) * 0.0015).exp()
}

/// Whether `limits` are non-degenerate (both spans strictly positive). The
/// widget keeps the previous limits when a candidate fails this.
pub fn is_valid(limits: Limits) -> bool {
    let (x_min, x_max, y_min, y_max) = limits;
    x_max > x_min && y_max > y_min
}

/// Clamp one axis range into the float32-safe window and repair degenerate
/// ranges, mirroring silx `_utils/panzoom.checkAxisLimits` (panzoom.py:51-77).
///
/// Both bounds are clamped to `[lower, FLOAT32_SAFE_MAX]`, where `lower` is
/// [`FLOAT32_MINPOS`] on a log axis (`is_log == true`) and [`FLOAT32_SAFE_MIN`]
/// otherwise. If the clamp leaves `max < min` the two are swapped; if it leaves
/// `max == min` the range is expanded the way silx does:
/// - `v == 0` → `(-0.1, 0.1)`
/// - `v < 0`  → `(max(v * 1.1, FLOAT32_SAFE_MIN), v * 0.9)`
/// - `v > 0`  → `(v * 0.9, min(v * 1.1, FLOAT32_SAFE_MAX))`
///
/// A `NaN` bound clamps to `lower` (matching `numpy.clip`'s NaN→bound on the
/// platforms silx targets), so the result is always finite and ordered.
pub fn clamp_axis_limits(min: f64, max: f64, is_log: bool) -> (f64, f64) {
    let lower = if is_log {
        FLOAT32_MINPOS
    } else {
        FLOAT32_SAFE_MIN
    };
    let clip = |v: f64| -> f64 {
        // numpy.clip with a NaN input yields the NaN, but silx's downstream
        // expects a finite ordered range; map NaN to the lower bound so the
        // window is always usable.
        if v.is_nan() {
            lower
        } else {
            v.clamp(lower, FLOAT32_SAFE_MAX)
        }
    };
    let mut vmin = clip(min);
    let mut vmax = clip(max);

    if vmax < vmin {
        std::mem::swap(&mut vmin, &mut vmax);
    } else if vmax == vmin {
        let v = vmin;
        if v == 0.0 {
            vmin = -0.1;
            vmax = 0.1;
        } else if v < 0.0 {
            vmax = v * 0.9;
            vmin = (v * 1.1).max(FLOAT32_SAFE_MIN);
        } else {
            vmax = (v * 1.1).min(FLOAT32_SAFE_MAX);
            vmin = v * 0.9;
        }
    }
    (vmin, vmax)
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
        }
    }

    /// Cancel any in-progress draw, returning to idle. Mirrors silx `cancel` /
    /// `cancelSelect` (drops the rubber band without a finished event).
    pub fn cancel(&mut self) {
        self.phase = Phase::Idle;
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
/// - [`RoiEdge::Vertex`] moves in both axes → [`CursorShape::SizeAll`].
pub fn cursor_for_edge(edge: RoiEdge) -> CursorShape {
    match edge {
        RoiEdge::Left | RoiEdge::Right => CursorShape::SizeHor,
        RoiEdge::Top | RoiEdge::Bottom => CursorShape::SizeVer,
        RoiEdge::Vertex(_) => CursorShape::SizeAll,
    }
}

/// Cursor shape for an optional grabbed edge: the edge's shape when `Some`, the
/// default arrow when `None` (nothing grabbable under the cursor). This is the
/// shape the widget passes to egui each hover frame.
pub fn cursor_for_grab(edge: Option<RoiEdge>) -> CursorShape {
    edge.map(cursor_for_edge).unwrap_or_default()
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
    fn wheel_factor_direction_and_neutral() {
        assert!(wheel_zoom_factor(100.0) < 1.0);
        assert!(wheel_zoom_factor(-100.0) > 1.0);
        assert!((wheel_zoom_factor(0.0) - 1.0).abs() <= 1e-12);
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
        // Horizontal-only edges -> SizeHor.
        assert_eq!(cursor_for_edge(RoiEdge::Left), CursorShape::SizeHor);
        assert_eq!(cursor_for_edge(RoiEdge::Right), CursorShape::SizeHor);
        // Vertical-only edges -> SizeVer.
        assert_eq!(cursor_for_edge(RoiEdge::Top), CursorShape::SizeVer);
        assert_eq!(cursor_for_edge(RoiEdge::Bottom), CursorShape::SizeVer);
        // Free vertex -> SizeAll.
        assert_eq!(cursor_for_edge(RoiEdge::Vertex(0)), CursorShape::SizeAll);
        assert_eq!(cursor_for_edge(RoiEdge::Vertex(7)), CursorShape::SizeAll);
    }

    #[test]
    fn cursor_for_grab_defaults_when_nothing_grabbed() {
        // None -> Default (nothing under the cursor).
        assert_eq!(cursor_for_grab(None), CursorShape::Default);
        // Some(edge) -> that edge's shape.
        assert_eq!(cursor_for_grab(Some(RoiEdge::Left)), CursorShape::SizeHor);
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
}
