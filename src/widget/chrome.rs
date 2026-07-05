//! Plot chrome drawn with egui's painter: frame, grid, ticks, tick labels, and
//! a vertical colorbar.
//!
//! Everything here derives from the same [`Transform`] that feeds the wgpu data
//! layer, so the axes and the image cannot drift apart (`doc/design.md` §4·§8).
//! Layout reserves fixed-pixel gutters for the labels and (optionally) the
//! colorbar; this is the chrome counterpart of silx's `_PlotWidget` margins.

use egui::epaint::TextShape;
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, Visuals, pos2, vec2};

use crate::core::colormap::{Colormap, Normalization};
use crate::core::dtime_ticks::{self, DateTime, TimeZone};
use crate::core::items::{LineStyle, Symbol};
use crate::core::marker::{Marker, MarkerKind, TextAnchor};
use crate::core::plot::{GraphGrid, TickMode};
use crate::core::roi::{
    HandleKind, ManagedRoi, Roi, RoiInteractionMode, arc_inner_radius, arc_outer_radius,
};
use crate::core::shape::{Line, Shape, ShapeKind, triangulate_simple_polygon};
use crate::core::ticklayout::{
    TICK_LABELS_PER_INCH_MICROSECONDS, adaptive_n_ticks, adaptive_n_ticks_density, nice_num,
};
use crate::core::transform::{Axis, AxisSide, Scale, Transform, YAxis};
use crate::core::triangles::Triangles;
use crate::widget::interaction;

/// Colors used to draw the chrome, derived from the active egui visuals so the
/// chrome follows light/dark theme.
pub struct Style {
    /// Frame, tick marks, and colorbar border.
    pub axis: Color32,
    /// Grid lines inside the data area (faint).
    pub grid: Color32,
    /// Tick label text.
    pub text: Color32,
    /// Background fill behind the crosshair coordinate readout.
    pub readout_bg: Color32,
}

impl Style {
    /// Build a chrome style from egui visuals (axis/text = text color, grid = a
    /// faint tint of it, readout = the themed window fill).
    pub fn from_visuals(v: &Visuals) -> Self {
        let text = v.text_color();
        let fill = v.window_fill();
        Self {
            axis: text,
            grid: crate::core::color::with_alpha(text, 28),
            text,
            readout_bg: crate::core::color::with_alpha(fill, 210),
        }
    }

    /// Apply the plot's color overrides: `fg` (when set) recolors the axes,
    /// frame, ticks, and label text; `grid` (when set) recolors the grid lines
    /// (silx `setForegroundColor` / `setGridColor`).
    pub fn with_overrides(mut self, fg: Option<Color32>, grid: Option<Color32>) -> Self {
        if let Some(c) = fg {
            self.axis = c;
            self.text = c;
            // Default the grid to a faint tint of the new foreground unless the
            // caller also overrides it below.
            self.grid = crate::core::color::with_alpha(c, 28);
        }
        if let Some(g) = grid {
            self.grid = g;
        }
        self
    }
}

/// Where the data area and (optional) colorbar sit inside the widget rect.
pub struct ChromeLayout {
    /// Rect the data layer (image/curve) and axes occupy.
    pub data_area: Rect,
    /// Colorbar strip rect, or `None` when the plot has no colormap.
    pub colorbar: Option<Rect>,
    /// Tick/label placement for each extra Y axis, aligned by index to the
    /// requested [`ChromeRequest::extra`] (and thus to `Plot::extra`). Empty when
    /// no extra axes are requested or the axes are hidden.
    pub extra: Vec<ExtraAxisSlot>,
}

/// One extra Y axis to reserve gutter space for, in [`ChromeRequest::extra`].
#[derive(Clone, Copy, Debug)]
pub struct ExtraAxisChrome {
    /// Which gutter the axis stacks into.
    pub side: AxisSide,
    /// Whether the axis has a label (reserves extra outer space for it).
    pub label: bool,
}

/// Where an extra Y axis' spine, ticks, and label are drawn, computed by
/// [`layout`]. `baseline_x` is the x of the vertical spine (ticks extend outward
/// from it); `label_x` is the x of the rotated axis-label center.
#[derive(Clone, Copy, Debug)]
pub struct ExtraAxisSlot {
    /// Which gutter the axis is drawn in.
    pub side: AxisSide,
    /// X of the axis spine (data-area-facing edge of the slot).
    pub baseline_x: f32,
    /// X of the rotated axis-label center (outer edge of the slot).
    pub label_x: f32,
}

// Fixed-pixel gutters. Left holds Y tick labels; bottom holds X tick labels;
// top/right are breathing room. With a colorbar the right gutter also holds the
// strip and its value labels.
const GUTTER_LEFT: f32 = 52.0;
const GUTTER_BOTTOM: f32 = 30.0;
const GUTTER_TOP: f32 = 12.0;
const GUTTER_RIGHT: f32 = 12.0;
const GUTTER_Y2: f32 = 52.0;
const CBAR_WIDTH: f32 = 16.0;
const CBAR_LABELS: f32 = 46.0;
// An interactive (histogram) colorbar reserves a wider gutter: the whole
// HistogramColorBar (value histogram + gradient strip + level labels) is laid
// out inside the colorbar rect, not just the strip with labels painted beside
// it. Matches `INTERACTIVE_COLORBAR_WIDTH` in `high_level.rs`.
const CBAR_INTERACTIVE_WIDTH: f32 = 175.0;
// Extra gutter claimed by an axis title / label when present.
const TITLE_H: f32 = 18.0;
const LABEL_H: f32 = 16.0;

/// What chrome the plot needs space reserved for. Drives [`layout`]'s gutter
/// sizes so titles/labels, a colorbar, and a y2 axis all get room.
#[derive(Clone, Default)]
pub struct ChromeRequest {
    /// A vertical colorbar in the right gutter.
    pub colorbar: bool,
    /// The colorbar is an interactive histogram colorbar (drag-to-set levels),
    /// which claims a wider gutter than a static strip. Only honored with
    /// `colorbar`.
    pub colorbar_interactive: bool,
    /// A secondary right (y2) axis with ticks in the right gutter.
    pub y2: bool,
    /// A graph title above the data area.
    pub title: bool,
    /// An X-axis label below the X tick labels.
    pub x_label: bool,
    /// A (left) Y-axis label at the far left.
    pub y_label: bool,
    /// A right (y2) Y-axis label at the far right (only honored with `y2`).
    pub y2_label: bool,
    /// Whether the axes (frame/ticks/labels) are *hidden* (the inverse of silx
    /// `isAxesDisplayed`). When `true` the axis gutters collapse to zero so the
    /// data area fills the whole rect, mirroring silx `setAxesDisplayed(False)`
    /// -> `setAxesMargins(0, 0, 0, 0)` (`PlotWidget.py:2838-2851`). Defaults to
    /// `false` (axes shown, normal gutters), so a `ChromeRequest::default()`
    /// reserves the usual gutters. The widget sets it from
    /// `!Plot::axes_displayed()`.
    pub axes_hidden: bool,
    /// Extra Y axes to reserve stacked gutter space for, in `Plot::extra` order.
    /// Each reserves a slot on its side outside the built-in gutters; honored
    /// only when the axes are shown (`axes_hidden == false`).
    pub extra: Vec<ExtraAxisChrome>,
}

/// Reserve gutters for axis labels (a colorbar and/or a right y2 axis, if
/// requested) and return the resulting data area and colorbar rects. A colorbar
/// and a y2 axis both claim the right gutter; the colorbar takes precedence when
/// both are requested. Titles and axis labels each grow their own gutter.
pub fn layout(full: Rect, req: &ChromeRequest) -> ChromeLayout {
    // An interactive colorbar lays the whole HistogramColorBar inside its rect, so
    // its rect width == its reservation width; a static strip is `CBAR_WIDTH` wide
    // with `CBAR_LABELS` painted beside it (rect is just the strip).
    let (cbar_reserve, cbar_width) = if req.colorbar_interactive {
        (CBAR_INTERACTIVE_WIDTH, CBAR_INTERACTIVE_WIDTH)
    } else {
        (CBAR_WIDTH + CBAR_LABELS, CBAR_WIDTH)
    };

    // Axes hidden: collapse every axis gutter to zero so the data area fills the
    // whole rect (silx setAxesDisplayed(False) -> setAxesMargins(0,0,0,0)). A
    // colorbar still claims its right strip, matching silx where the colorbar is
    // a separate widget unaffected by the axes-margins toggle.
    if req.axes_hidden {
        let right = if req.colorbar {
            GUTTER_RIGHT + cbar_reserve
        } else {
            0.0
        };
        let data_area = Rect::from_min_max(
            pos2(full.left(), full.top()),
            pos2(full.right() - right, full.bottom()),
        );
        let colorbar = req.colorbar.then(|| {
            let x0 = data_area.right() + GUTTER_RIGHT;
            Rect::from_min_max(
                pos2(x0, data_area.top()),
                pos2(x0 + cbar_width, data_area.bottom()),
            )
        });
        return ChromeLayout {
            data_area,
            colorbar,
            extra: Vec::new(),
        };
    }

    let right_axis = if req.colorbar {
        GUTTER_RIGHT + cbar_reserve
    } else if req.y2 {
        GUTTER_Y2
    } else {
        GUTTER_RIGHT
    };
    // A y2 label adds rotated text outside the y2 ticks.
    let base_right = right_axis + if req.y2 && req.y2_label { LABEL_H } else { 0.0 };
    let base_left = GUTTER_LEFT + if req.y_label { LABEL_H } else { 0.0 };
    let top = GUTTER_TOP + if req.title { TITLE_H } else { 0.0 };
    let bottom = GUTTER_BOTTOM + if req.x_label { LABEL_H } else { 0.0 };

    // Extra axes stack outward beyond the base gutters: each reserves a tick
    // slot (`GUTTER_Y2`) on its side plus room for its rotated label.
    let extra_slot = |label: bool| GUTTER_Y2 + if label { LABEL_H } else { 0.0 };
    let mut extra_left_reserve = 0.0;
    let mut extra_right_reserve = 0.0;
    for ax in &req.extra {
        match ax.side {
            AxisSide::Left => extra_left_reserve += extra_slot(ax.label),
            AxisSide::Right => extra_right_reserve += extra_slot(ax.label),
        }
    }
    let left = base_left + extra_left_reserve;
    let right = base_right + extra_right_reserve;

    let data_area = Rect::from_min_max(
        pos2(full.left() + left, full.top() + top),
        pos2(full.right() - right, full.bottom() - bottom),
    );
    let colorbar = req.colorbar.then(|| {
        let x0 = data_area.right() + GUTTER_RIGHT;
        Rect::from_min_max(
            pos2(x0, data_area.top()),
            pos2(x0 + cbar_width, data_area.bottom()),
        )
    });

    // Position each extra axis just outside the base gutter on its side, then
    // step the per-side cursor outward (toward the widget edge) for the next.
    let mut right_cursor = data_area.right() + base_right;
    let mut left_cursor = data_area.left() - base_left;
    let mut extra = Vec::with_capacity(req.extra.len());
    for ax in &req.extra {
        let slot = extra_slot(ax.label);
        let entry = match ax.side {
            AxisSide::Right => {
                let baseline_x = right_cursor;
                let label_x = baseline_x + GUTTER_Y2 + LABEL_H * 0.5;
                right_cursor += slot;
                ExtraAxisSlot {
                    side: ax.side,
                    baseline_x,
                    label_x,
                }
            }
            AxisSide::Left => {
                let baseline_x = left_cursor;
                let label_x = baseline_x - GUTTER_Y2 - LABEL_H * 0.5;
                left_cursor -= slot;
                ExtraAxisSlot {
                    side: ax.side,
                    baseline_x,
                    label_x,
                }
            }
        };
        extra.push(entry);
    }

    ChromeLayout {
        data_area,
        colorbar,
        extra,
    }
}

/// "Nice" tick values within `[min, max]` plus the step between them. `n_ticks`
/// is silx's target tick count (`ticklayout.niceNumbers`' `nTicks`): the span is
/// divided by `n_ticks` (silx `vrange / nTicks`), NOT by `n_ticks − 1` — silx
/// deviates from Heckbert's classic `/(nTicks−1)` here, so matching it needs the
/// bare `n_ticks` divisor.
pub fn nice_ticks(min: f64, max: f64, n_ticks: usize) -> (Vec<f64>, f64) {
    // partial_cmp (not `max > min`) so NaN limits fall through to "no ticks".
    let ascending = matches!(max.partial_cmp(&min), Some(std::cmp::Ordering::Greater));
    if !ascending || n_ticks < 2 {
        return (Vec::new(), 1.0);
    }
    let range = nice_num(max - min, false);
    let step = nice_num(range / n_ticks as f64, true);
    let start = (min / step).floor() * step;
    let end = (max / step).ceil() * step;
    let n = ((end - start) / step).round() as i64;
    let mut ticks = Vec::new();
    for i in 0..=n {
        let v = start + i as f64 * step;
        if v >= min - step * 1e-6 && v <= max + step * 1e-6 {
            ticks.push(v);
        }
    }
    (ticks, step)
}

/// Format a tick value with enough decimals for the step size.
fn format_tick(v: f64, step: f64) -> String {
    let decimals = (-step.log10().floor()).clamp(0.0, 6.0) as usize;
    format!("{v:.decimals$}")
}

/// silx `niceNumbersForLog10`'s default tick count (`ticklayout.py:205`).
const LOG_NUM_TICKS: usize = 5;

/// Tolerance for the decade-boundary inclusion test, so a decade landing exactly
/// on `log_min`/`log_max` (e.g. `log10(1000) == 3`) is not dropped by float error.
const LOG_TICK_EPS: f64 = 1e-9;

/// silx `niceNumbersForLog10` (`ticklayout.py:193-218`) plus the `dataMin <= 0`
/// clamp (`GLPlotFrame.py:371-375`): resolve `[min, max]` into integer log10 tick
/// bounds `(tick_min_log, tick_max_log, spacing)` and the clamped data bounds
/// `(lo, hi)`. Wide ranges coarsen — for `> LOG_NUM_TICKS` decades the spacing is
/// `floor(rangelog / LOG_NUM_TICKS)` with the bounds re-anchored to spacing
/// multiples. A non-positive lower bound is clamped to 1.0 (and `hi` pulled up to
/// match) so a log axis over non-positive limits still yields ticks. `None` for a
/// non-finite or degenerate (`lo == hi`) range.
fn log10_tick_layout(min: f64, max: f64) -> Option<(i32, i32, i32, f64, f64)> {
    if !min.is_finite() || !max.is_finite() {
        return None;
    }
    let mut lo = min;
    let mut hi = max;
    if lo <= 0.0 {
        lo = 1.0;
        if hi < lo {
            hi = 1.0;
        }
    }
    if lo == hi {
        return None;
    }
    let mut graph_min_log = lo.log10().floor();
    let mut graph_max_log = hi.log10().ceil();
    let range_log = graph_max_log - graph_min_log;
    let spacing = if range_log <= LOG_NUM_TICKS as f64 {
        1.0
    } else {
        let s = (range_log / LOG_NUM_TICKS as f64).floor();
        graph_min_log = (graph_min_log / s).floor() * s;
        graph_max_log = (graph_max_log / s).ceil() * s;
        s
    };
    Some((
        graph_min_log as i32,
        graph_max_log as i32,
        spacing as i32,
        lo,
        hi,
    ))
}

/// Decade tick values within `[min, max]` for a log10 axis — silx
/// `niceNumbersForLog10` layout (see [`log10_tick_layout`]): one tick per
/// `spacing` decades, coarsening on wide ranges, yielded where
/// `log_min <= log_pos <= log_max`. Empty for a non-finite or degenerate range;
/// a non-positive lower bound is clamped to 1.0 rather than rendering blank.
fn log_decade_ticks(min: f64, max: f64) -> Vec<f64> {
    let Some((tick_min, tick_max, spacing, lo, hi)) = log10_tick_layout(min, max) else {
        return Vec::new();
    };
    let (log_min, log_max) = (lo.log10(), hi.log10());
    let mut ticks = Vec::new();
    let mut logpos = tick_min;
    while logpos <= tick_max {
        let lp = logpos as f64;
        if lp >= log_min - LOG_TICK_EPS && lp <= log_max + LOG_TICK_EPS {
            ticks.push(10f64.powi(logpos));
        }
        logpos += spacing;
    }
    ticks
}

/// Format a log-axis decade tick VALUE: plain decimal in the everyday range,
/// scientific notation outside it (e.g. 1e-6, 1e9). Used by the inline colorbar,
/// which labels values like silx's colorbar; the data axes use
/// [`format_axis_log_tick`] instead.
fn format_log_tick(v: f64) -> String {
    if (1e-4..1e6).contains(&v) {
        format!("{v}")
    } else {
        format!("{v:e}")
    }
}

/// Axis log-decade tick label — silx `GLPlotFrame.py:395` `"1e%+03d" % logPos`:
/// the base-10 exponent, signed, zero-padded to at least two digits, e.g.
/// `1e+02`, `1e-06`, `1e+12`. The data axes label the exponent (matching silx's
/// GL frame); the inline colorbar keeps value labels via [`format_log_tick`].
fn format_axis_log_tick(v: f64) -> String {
    let logpos = v.log10().round() as i32;
    format!("1e{logpos:+03}")
}

/// Tick values plus their formatted labels for one axis: "nice" numbers on a
/// linear axis, one-per-decade on a log axis. The default [`TickMode::Numeric`]
/// path is unchanged.
fn axis_ticks(axis: &Axis, max_ticks: usize) -> Vec<(f64, String)> {
    axis_ticks_with_mode(axis, max_ticks, TickMode::Numeric, TimeZone::Utc, 0.0)
}

/// As [`axis_ticks`] but honoring the axis [`TickMode`]. With
/// [`TickMode::TimeSeries`] the axis data values are treated as epoch seconds
/// (UTC) and tick positions + labels are produced by [`dtime_ticks`] laid out
/// in `tz`'s wall-clock calendar (`calc_ticks_tz` / `format_ticks_tz`),
/// mirroring silx `NiceDateLocator` + `NiceAutoDateFormatter`
/// (`backends/BackendMatplotlib.py:153-242`). A TimeSeries tick mode is honored
/// only on a [`Scale::Linear`] axis (silx ties the time locator to the
/// linear/numeric axis); a log axis falls back to the numeric decade ticks.
/// `tz` is ignored outside the TimeSeries-on-linear path.
///
/// `time_offset` is the epoch corresponding to axis value `0` (rsplot extension;
/// see [`Plot::set_x_time_offset`](crate::Plot::set_x_time_offset)): under
/// TimeSeries the ticks are laid out over the absolute-epoch window
/// `[min + time_offset, max + time_offset]` and labeled as wall-clock, then each
/// tick position is shifted back by `time_offset` so it lands at the right pixel
/// in the axis's (possibly relative) value space. `0.0` (the default) means the
/// axis values already are epoch seconds — unchanged behavior. Ignored outside
/// the TimeSeries-on-linear path.
fn axis_ticks_with_mode(
    axis: &Axis,
    max_ticks: usize,
    tick_mode: TickMode,
    tz: TimeZone,
    time_offset: f64,
) -> Vec<(f64, String)> {
    if tick_mode == TickMode::TimeSeries && axis.scale == Scale::Linear {
        let (lo, hi) = if axis.max >= axis.min {
            (axis.min, axis.max)
        } else {
            (axis.max, axis.min)
        };
        let (lo_epoch, hi_epoch) = (lo + time_offset, hi + time_offset);
        // `max_ticks` is the adaptive (pixel-density) count, mirroring silx's
        // time axis, which runs the SAME `calcTicksAdaptive` density path as the
        // numeric axis (`GLPlotFrame.py:450-459`) — not a fixed count.
        let (ticks, spacing, unit) = dtime_ticks::calc_ticks_tz(lo_epoch, hi_epoch, max_ticks, tz);
        // `calc_ticks_tz` brackets one tick beyond each end (`include_first_beyond`
        // in `date_range`), so cull to the visible epoch window before formatting:
        // otherwise a bracket tick + label (and, with grid on, a grid line) paints
        // in the frame gutter, and the µs zero-strip in `format_ticks_tz` spans the
        // out-of-range labels. Mirrors silx `GLPlotFrame.py:460-462`
        // (`visibleDatetimes = (dt for dt in tickDateTimes if dtMin <= dt <= dtMax)`,
        // then `formatDatetimes` over the visible set). The numeric path already
        // culls inside `nice_ticks`.
        let visible: Vec<f64> = ticks
            .into_iter()
            .filter(|&epoch| epoch >= lo_epoch && epoch <= hi_epoch)
            .collect();
        let labels = dtime_ticks::format_ticks_tz(&visible, spacing, unit, tz);
        return visible
            .into_iter()
            .map(|epoch| epoch - time_offset)
            .zip(labels)
            .collect();
    }
    match axis.scale {
        Scale::Linear => {
            let (ticks, step) = nice_ticks(axis.min, axis.max, max_ticks);
            ticks
                .into_iter()
                .map(|v| (v, format_tick(v, step)))
                .collect()
        }
        Scale::Log10 => log_decade_ticks(axis.min, axis.max)
            .into_iter()
            .map(|v| (v, format_axis_log_tick(v)))
            .collect(),
    }
}

fn linear_minor_ticks(axis: &Axis, major: &[(f64, String)]) -> Vec<f64> {
    if major.len() < 2 {
        return Vec::new();
    }
    let major_step = (major[1].0 - major[0].0).abs();
    if !major_step.is_finite() || major_step <= 0.0 {
        return Vec::new();
    }
    let minor_step = major_step / 5.0;
    let start = (axis.min / minor_step).ceil() as i64 - 1;
    let end = (axis.max / minor_step).floor() as i64 + 1;
    let major_eps = minor_step * 1e-6;
    let mut ticks = Vec::new();
    for i in start..=end {
        let v = i as f64 * minor_step;
        if v <= axis.min || v >= axis.max {
            continue;
        }
        let major_multiple = ((v - major[0].0) / major_step).round();
        let nearest_major = major[0].0 + major_multiple * major_step;
        if (v - nearest_major).abs() <= major_eps {
            continue;
        }
        ticks.push(v);
    }
    ticks
}

fn log_minor_ticks(axis: &Axis) -> Vec<f64> {
    let Some((tick_min, tick_max, spacing, lo, hi)) = log10_tick_layout(axis.min, axis.max) else {
        return Vec::new();
    };
    // silx draws log sub-ticks only when the decade spacing is 1, i.e. no
    // coarsening (`GLPlotFrame.py:398`: `if step == 1`).
    if spacing != 1 {
        return Vec::new();
    }
    // frange(tickMin, tickMax, 1)[:-1] → decades tick_min..tick_max (drop the top
    // decade), each contributing 2..9 × 10^k within the clamped [lo, hi].
    let mut ticks = Vec::new();
    for k in tick_min..tick_max {
        let decade = 10f64.powi(k);
        for m in 2..10 {
            let v = m as f64 * decade;
            if lo <= v && v <= hi {
                ticks.push(v);
            }
        }
    }
    ticks
}

fn minor_ticks(axis: &Axis, major: &[(f64, String)]) -> Vec<f64> {
    match axis.scale {
        Scale::Linear => linear_minor_ticks(axis, major),
        Scale::Log10 => log_minor_ticks(axis),
    }
}

/// Draw the frame, optional grid, ticks, and tick labels around the data area.
///
/// `x_max_ticks` / `y_max_ticks` cap the number of major ticks on each axis.
/// `None` adapts the count to the axis's pixel size (silx `niceNumbersAdaptative`,
/// ~1.3 labels per inch) rather than a fixed default.
pub fn draw_axes(
    painter: &Painter,
    t: &Transform,
    style: &Style,
    grid_mode: GraphGrid,
    x_max_ticks: Option<usize>,
    y_max_ticks: Option<usize>,
) {
    draw_axes_with_x_tick_mode(
        painter,
        t,
        style,
        grid_mode,
        x_max_ticks,
        y_max_ticks,
        TickMode::Numeric,
        TimeZone::Utc,
        0.0,
    );
}

/// As [`draw_axes`] but honoring the X-axis [`TickMode`]: with
/// [`TickMode::TimeSeries`] the X tick positions and labels are produced by
/// [`dtime_ticks`] (epoch-seconds data values) laid out in `x_time_zone`'s
/// wall-clock calendar, mirroring silx's `NiceDateLocator` time-axis path
/// (`backends/BackendMatplotlib.py:153-242`). silx supports the time-series
/// mode on the X axis only, so the Y axis always uses numeric ticks. The
/// default `Numeric` + [`TimeZone::Utc`] matches [`draw_axes`] exactly.
#[allow(clippy::too_many_arguments)]
pub fn draw_axes_with_x_tick_mode(
    painter: &Painter,
    t: &Transform,
    style: &Style,
    grid_mode: GraphGrid,
    x_max_ticks: Option<usize>,
    y_max_ticks: Option<usize>,
    x_tick_mode: TickMode,
    x_time_zone: TimeZone,
    x_time_offset: f64,
) {
    let area = t.area;
    let axis = Stroke::new(1.0, style.axis);
    let grid = Stroke::new(1.0, style.grid);
    // `style.grid` is itself translucent (alpha 28), so a premultiplied read +
    // rewrap would crush the minor-grid RGB toward black; `with_alpha` keeps the
    // straight RGB and just halves the alpha.
    let minor_grid = Stroke::new(
        1.0,
        crate::core::color::with_alpha(style.grid, style.grid.a() / 2),
    );
    let font = FontId::proportional(11.0);
    let tick_len = 4.0;

    // With no explicit cap, the tick count adapts to each axis's physical pixel
    // length (silx `niceNumbersAdaptative`, `GLPlotFrame.py:414-425`): 1.3 labels
    // per inch. egui `Rect` extents are logical points, so scale by
    // `pixels_per_point` to device pixels, matching silx's physical-pixel input.
    let ppp = f64::from(painter.ctx().pixels_per_point());
    let x_ticks_n = x_max_ticks.unwrap_or_else(|| {
        let len_px = f64::from(area.width()) * ppp;
        // silx reduces the time axis to 1.0 label/inch (from 1.3) in the
        // microseconds regime — span <= ~2 s so `bestUnit == MICRO_SECONDS` —
        // before `calcTicksAdaptive` (`GLPlotFrame.py:451-457`). Only when the
        // TimeSeries-on-linear path is actually taken (a log axis falls back to
        // numeric decades). `time_offset` cancels in the span difference.
        if x_tick_mode == TickMode::TimeSeries
            && t.x.scale == Scale::Linear
            && dtime_ticks::best_unit((t.x.max - t.x.min).abs()).1
                == dtime_ticks::DtUnit::MicroSeconds
        {
            adaptive_n_ticks_density(len_px, TICK_LABELS_PER_INCH_MICROSECONDS)
        } else {
            adaptive_n_ticks(len_px)
        }
    });
    let y_ticks_n = y_max_ticks.unwrap_or_else(|| adaptive_n_ticks(f64::from(area.height()) * ppp));
    let xticks = axis_ticks_with_mode(&t.x, x_ticks_n, x_tick_mode, x_time_zone, x_time_offset);
    let yticks = axis_ticks_with_mode(&t.y, y_ticks_n, TickMode::Numeric, TimeZone::Utc, 0.0);

    if grid_mode.minor() {
        for xv in minor_ticks(&t.x, &xticks) {
            let px = t.data_to_pixel(xv, t.y.min).x;
            painter.vline(px, area.y_range(), minor_grid);
        }
        for yv in minor_ticks(&t.y, &yticks) {
            let py = t.data_to_pixel(t.x.min, yv).y;
            painter.hline(area.x_range(), py, minor_grid);
        }
    }

    if grid_mode.major() {
        // Grid lines first, so the frame and ticks sit on top of them.
        for (xv, _) in &xticks {
            let px = t.data_to_pixel(*xv, t.y.min).x;
            painter.vline(px, area.y_range(), grid);
        }
        for (yv, _) in &yticks {
            let py = t.data_to_pixel(t.x.min, *yv).y;
            painter.hline(area.x_range(), py, grid);
        }
    }

    painter.rect_stroke(
        area,
        egui::CornerRadius::ZERO,
        axis,
        egui::StrokeKind::Inside,
    );

    // X ticks + labels below the bottom edge.
    for (xv, label) in &xticks {
        let px = t.data_to_pixel(*xv, t.y.min).x;
        painter.line_segment(
            [pos2(px, area.bottom()), pos2(px, area.bottom() + tick_len)],
            axis,
        );
        painter.text(
            pos2(px, area.bottom() + tick_len + 2.0),
            Align2::CENTER_TOP,
            label,
            font.clone(),
            style.text,
        );
    }
    // Y ticks + labels left of the left edge.
    for (yv, label) in &yticks {
        let py = t.data_to_pixel(t.x.min, *yv).y;
        painter.line_segment(
            [pos2(area.left() - tick_len, py), pos2(area.left(), py)],
            axis,
        );
        painter.text(
            pos2(area.left() - tick_len - 3.0, py),
            Align2::RIGHT_CENTER,
            label,
            font.clone(),
            style.text,
        );
    }
}

/// Draw the secondary right (y2) axis: tick marks and value labels just outside
/// the right edge of the data area. `t` is the y2 transform (shared X, y2 as Y);
/// no grid lines are drawn, to keep the right axis from cluttering the data area
/// (`doc/design.md` §13 A5).
pub fn draw_y2_ticks(painter: &Painter, t: &Transform, style: &Style) {
    let area = t.area;
    let axis = Stroke::new(1.0, style.axis);
    let font = FontId::proportional(11.0);
    let tick_len = 4.0;

    for (yv, label) in axis_ticks(&t.y, 6) {
        let py = t.data_to_pixel(t.x.min, yv).y;
        painter.line_segment(
            [pos2(area.right(), py), pos2(area.right() + tick_len, py)],
            axis,
        );
        painter.text(
            pos2(area.right() + tick_len + 3.0, py),
            Align2::LEFT_CENTER,
            label,
            font.clone(),
            style.text,
        );
    }
}

/// Draw one extra (stacked) Y axis: a vertical spine at `baseline_x`, tick marks
/// and value labels extending outward on `side`, and the optional rotated axis
/// label centered at `label_x`. `t` is the axis' transform (shared X, the extra
/// axis as Y) and `baseline_x` / `label_x` come from the matching
/// [`ExtraAxisSlot`]. Like [`draw_y2_ticks`], no grid lines are drawn. The
/// multi-axis sibling of [`draw_y2_ticks`] (`doc/design.md` §13 A5 extension).
#[allow(clippy::too_many_arguments)]
pub fn draw_extra_y_ticks(
    painter: &Painter,
    t: &Transform,
    side: AxisSide,
    baseline_x: f32,
    label_x: f32,
    label: Option<&str>,
    style: &Style,
) {
    let area = t.area;
    let axis = Stroke::new(1.0, style.axis);
    let font = FontId::proportional(11.0);
    let tick_len = 4.0;

    // Spine: the offset stacked axis has no plot-frame edge to sit on (unlike
    // y2, whose spine is the data-area frame), so draw one.
    painter.vline(baseline_x, area.y_range(), axis);

    for (yv, label) in axis_ticks(&t.y, 6) {
        let py = t.data_to_pixel(t.x.min, yv).y;
        match side {
            AxisSide::Right => {
                painter.line_segment(
                    [pos2(baseline_x, py), pos2(baseline_x + tick_len, py)],
                    axis,
                );
                painter.text(
                    pos2(baseline_x + tick_len + 3.0, py),
                    Align2::LEFT_CENTER,
                    label,
                    font.clone(),
                    style.text,
                );
            }
            AxisSide::Left => {
                painter.line_segment(
                    [pos2(baseline_x - tick_len, py), pos2(baseline_x, py)],
                    axis,
                );
                painter.text(
                    pos2(baseline_x - tick_len - 3.0, py),
                    Align2::RIGHT_CENTER,
                    label,
                    font.clone(),
                    style.text,
                );
            }
        }
    }

    if let Some(text) = label {
        // Left axes read bottom→top (−90°), right axes top→bottom (+90°),
        // matching the built-in left / y2 labels in `draw_labels`.
        let angle = match side {
            AxisSide::Left => -std::f32::consts::FRAC_PI_2,
            AxisSide::Right => std::f32::consts::FRAC_PI_2,
        };
        draw_rotated_label(
            painter,
            pos2(label_x, area.center().y),
            angle,
            text,
            FontId::proportional(12.0),
            style.text,
        );
    }
}

/// The title and axis-label strings to draw in the reserved gutters; any field
/// may be `None`.
#[derive(Clone, Copy, Default)]
pub struct Labels<'a> {
    /// Graph title, centered above the data area.
    pub title: Option<&'a str>,
    /// X-axis label, centered below the X tick labels.
    pub x: Option<&'a str>,
    /// Left Y-axis label, rotated at the far left.
    pub y: Option<&'a str>,
    /// Right (y2) Y-axis label, rotated at the far right (gated by `with_y2`).
    pub y2: Option<&'a str>,
}

/// Draw the graph title and axis labels in the gutters reserved by [`layout`].
/// `full` is the whole widget rect, `area` the data area; `with_y2` gates the
/// y2 label. Y labels are rotated a quarter turn (silx `setGraphYLabel`).
pub fn draw_labels(
    painter: &Painter,
    full: Rect,
    area: Rect,
    labels: &Labels,
    with_y2: bool,
    style: &Style,
) {
    let title_font = FontId::proportional(14.0);
    let label_font = FontId::proportional(12.0);

    if let Some(t) = labels.title {
        painter.text(
            pos2(area.center().x, full.top() + 2.0),
            Align2::CENTER_TOP,
            t,
            title_font,
            style.text,
        );
    }
    if let Some(t) = labels.x {
        painter.text(
            pos2(area.center().x, full.bottom() - 2.0),
            Align2::CENTER_BOTTOM,
            t,
            label_font.clone(),
            style.text,
        );
    }
    // Left Y label: rotate a quarter turn counter-clockwise (reads bottom→top),
    // centered in the left gutter strip.
    if let Some(t) = labels.y {
        draw_rotated_label(
            painter,
            pos2(full.left() + LABEL_H * 0.5, area.center().y),
            -std::f32::consts::FRAC_PI_2,
            t,
            label_font.clone(),
            style.text,
        );
    }
    // Right y2 label: rotate a quarter turn clockwise (reads top→bottom),
    // centered in the right gutter strip.
    if with_y2 && let Some(t) = labels.y2 {
        draw_rotated_label(
            painter,
            pos2(full.right() - LABEL_H * 0.5, area.center().y),
            std::f32::consts::FRAC_PI_2,
            t,
            label_font,
            style.text,
        );
    }
}

/// Draw `text` rotated by `angle` (a quarter turn) so its visual center lands
/// exactly at `center`.
///
/// epaint's [`TextShape::with_angle_and_anchor`] with [`Align2::CENTER_CENTER`]
/// lands the galley center at `pos + galley_center`, not at `pos` (the `a1`
/// rotation term cancels, leaving a `+galley_center` offset). Left uncorrected,
/// that pushes a long left label into the data area and a long right (y2) label
/// past the gutter and out of the clip rect entirely. Pre-subtracting the galley
/// center cancels the offset so both axis labels sit centered in their gutters
/// regardless of length.
fn draw_rotated_label(
    painter: &Painter,
    center: Pos2,
    angle: f32,
    text: &str,
    font: FontId,
    color: Color32,
) {
    let galley = painter.layout_no_wrap(text.to_owned(), font, color);
    let pos = center - galley.rect.center().to_vec2();
    painter.add(egui::Shape::Text(
        TextShape::new(pos, galley, color).with_angle_and_anchor(angle, Align2::CENTER_CENTER),
    ));
}

/// Format a coordinate with decimals scaled to the visible span.
fn format_coord(v: f64, lo: f64, hi: f64) -> String {
    let span = (hi - lo).abs();
    let decimals = if span > 0.0 {
        (2.0 - span.log10().floor()).clamp(0.0, 6.0) as usize
    } else {
        3
    };
    format!("{v:.decimals$}")
}

/// Per-ROI drawing overrides supplied by the ROI manager. Keeps the geometry
/// [`Roi`] pure: color, name, selection, and outline styling live alongside it,
/// not inside it.
#[derive(Clone, Default)]
pub struct RoiAppearance<'a> {
    /// Outline/handle color; falls back to the chrome axis color when `None`
    /// (silx `RegionOfInterest.getColor`, default red applied by the manager).
    pub color: Option<Color32>,
    /// Optional name drawn as a small label near the ROI (silx
    /// `RegionOfInterest.getName`).
    pub name: Option<&'a str>,
    /// Whether this ROI is the highlighted/current one: drawn with a thicker
    /// outline (silx highlight style `linewidth=2` vs the default `1`).
    pub selected: bool,
    /// Outline width in logical points; `None` uses the default (silx
    /// `RegionOfInterest.getLineWidth`). A `selected` ROI uses `max(width, 2)`
    /// (silx highlight `linewidth=2`).
    pub line_width: Option<f32>,
    /// Outline stroke style (silx `getLineStyle`). `None` is solid; a dashed or
    /// dotted style is emitted as manual dash segments.
    pub line_style: Option<LineStyle>,
    /// Color filling the gaps between dashes/dots of the outline (silx
    /// `getLineGapColor`). `None` leaves the gaps transparent; only visible on a
    /// dashed/dotted `line_style`.
    pub gap_color: Option<Color32>,
    /// Whether the ROI interior is filled with a translucent tint. `None` keeps
    /// the legacy faint fill; `Some(false)` draws no fill (silx `setFill(False)`).
    pub fill: Option<bool>,
}

/// Draw each region of interest honoring its per-ROI appearance: the resolved
/// color (`managed.color` or `default_color`), a name label, a thicker outline
/// when selected, and its line width / style / fill (silx
/// `RegionOfInterest`). `default_color` is the manager's color (silx
/// `RegionOfInterestManager.getColor`, default red) applied to ROIs without an
/// explicit override (`doc/design.md` §13 C3).
pub fn draw_rois(
    painter: &Painter,
    t: &Transform,
    rois: &[ManagedRoi],
    default_color: Color32,
    style: &Style,
) {
    for r in rois {
        // silx setVisible(False) hides the ROI's backing plot items
        // (_roi_base.py:479-489) — a hidden ROI is not drawn.
        if !r.visible {
            continue;
        }
        let appearance = roi_appearance(r, default_color);
        draw_roi(painter, t, &r.roi, &appearance, style, r.interaction_mode());
    }
}

/// Resolve a [`ManagedRoi`]'s drawing overrides into a [`RoiAppearance`] (silx
/// `RegionOfInterest` → draw style): the color falls back to `default_color`
/// when the ROI has no override; an empty name becomes no label; width / style /
/// fill / selection pass through. The `selected`-thicker-outline rule lives in
/// [`draw_roi`], not here. Pure, so the resolution is unit-testable without a
/// `Painter`.
fn roi_appearance(managed: &ManagedRoi, default_color: Color32) -> RoiAppearance<'_> {
    RoiAppearance {
        color: Some(managed.color.unwrap_or(default_color)),
        name: (!managed.name.is_empty()).then_some(managed.name.as_str()),
        selected: managed.selected,
        line_width: Some(managed.line_width),
        line_style: Some(managed.line_style.to_line_style()),
        gap_color: managed.gap_color,
        fill: Some(managed.fill),
    }
}

/// The four data-space corners of a band ROI (silx `BandGeometry.corners`):
/// `begin±offset, end±offset` with `offset = 0.5·width·normal`, `normal =
/// (-vy/len, vx/len)`. A zero-length band yields a degenerate quad at the point.
fn band_corners_data(begin: (f64, f64), end: (f64, f64), width: f64) -> [(f64, f64); 4] {
    let (vx, vy) = (end.0 - begin.0, end.1 - begin.1);
    let len = (vx * vx + vy * vy).sqrt();
    let n = if len == 0.0 {
        (0.0, 0.0)
    } else {
        (-vy / len, vx / len)
    };
    let off = (0.5 * width * n.0, 0.5 * width * n.1);
    [
        (begin.0 - off.0, begin.1 - off.1),
        (begin.0 + off.0, begin.1 + off.1),
        (end.0 + off.0, end.1 + off.1),
        (end.0 - off.0, end.1 - off.1),
    ]
}

/// Boundary polygon (data space) of an annular sector for drawing (silx
/// `ArcROI._createShapeFromGeometry`): the outer arc from `start` to `end`
/// followed by the inner arc back (or the center, when `inner_radius == 0`,
/// giving a "camembert" wedge). A full `2π` sweep yields the outer circle.
fn arc_outline(
    center: (f64, f64),
    inner_radius: f64,
    outer_radius: f64,
    start_angle: f64,
    end_angle: f64,
) -> Vec<(f64, f64)> {
    let sweep = end_angle - start_angle;
    // Match silx: at most ~100 angular samples, at least a couple.
    let steps = ((sweep.abs() / std::f64::consts::TAU * 100.0).ceil() as usize).clamp(2, 100);
    let at = |r: f64, a: f64| (center.0 + r * a.cos(), center.1 + r * a.sin());
    let mut pts = Vec::with_capacity(steps * 2 + 2);
    // Outer arc start -> end.
    for i in 0..=steps {
        let a = start_angle + sweep * (i as f64 / steps as f64);
        pts.push(at(outer_radius, a));
    }
    if inner_radius <= 0.0 {
        // Camembert wedge: close through the center.
        pts.push(center);
    } else {
        // Inner arc end -> start.
        for i in 0..=steps {
            let a = end_angle - sweep * (i as f64 / steps as f64);
            pts.push(at(inner_radius, a));
        }
    }
    pts
}

/// Draw the handle glyphs of a ROI in `color`, one per [`RoiHandle`]. Mirrors
/// the silx handle markers (`items/_roi_base.py` `addHandle`/`addTranslateHandle`):
/// a translate or center handle is a `+` (silx `"+"`), every other handle (shape
/// vertex / band edge) is a small filled square (silx default `"s"`).
///
/// In silx **ThreePointMode** an Arc shows its three control-point handles
/// instead of the four polar handles ([`RoiInteractionMode::ArcThreePoint`]); the
/// handle set is otherwise [`Roi::handles`].
fn draw_roi_handles(
    painter: &Painter,
    t: &Transform,
    roi: &Roi,
    color: Color32,
    mode: Option<RoiInteractionMode>,
) {
    let three_point = mode == Some(RoiInteractionMode::ArcThreePoint);
    let handles = match three_point
        .then(|| interaction::arc_three_point_handles(roi))
        .flatten()
    {
        Some(h) => h.to_vec(),
        None => roi.handles(),
    };
    for handle in handles {
        let p = t.data_to_pixel(handle.pos[0], handle.pos[1]);
        match handle.kind {
            HandleKind::Translate | HandleKind::Center => {
                // '+' glyph (silx translate handle symbol).
                let r = 4.0;
                let stroke = Stroke::new(1.5, color);
                painter.line_segment([pos2(p.x - r, p.y), pos2(p.x + r, p.y)], stroke);
                painter.line_segment([pos2(p.x, p.y - r), pos2(p.x, p.y + r)], stroke);
            }
            HandleKind::Vertex | HandleKind::Edge => {
                let h = Rect::from_center_size(p, vec2(6.0, 6.0));
                painter.rect_filled(h, egui::CornerRadius::ZERO, color);
            }
        }
    }
}

/// Draw one ROI honoring `appearance`: the override color recolors the outline,
/// fill, and handles; a selected ROI uses a thicker border (silx highlight
/// `linewidth=2`); and a non-empty name is drawn as a label near the ROI.
///
/// `mode` is the ROI's interaction mode (silx `InteractionModeMixIn`): a
/// [`RoiInteractionMode::BandUnbounded`] band draws as three view-spanning
/// parallel lines (silx UnboundedMode) instead of the bounded corner polygon;
/// every other mode (and `None`) draws the default geometry.
pub fn draw_roi(
    painter: &Painter,
    t: &Transform,
    roi: &Roi,
    appearance: &RoiAppearance,
    style: &Style,
    mode: Option<RoiInteractionMode>,
) {
    let color = appearance.color.unwrap_or(style.axis);
    // Base width from the appearance (silx default 1.0); a selected/current ROI
    // gets at least the silx highlight width 2.0.
    let base_width = appearance.line_width.unwrap_or(1.0);
    let width = if appearance.selected {
        base_width.max(2.0)
    } else {
        base_width
    };
    // Fill: `Some(false)` means no fill (silx `setFill(False)`); `Some(true)` and
    // the default both draw the translucent tint.
    let fill_enabled = appearance.fill.unwrap_or(true);
    let fill = fill_enabled.then(|| crate::core::color::with_alpha(color, 24));
    let line_style = appearance.line_style.clone().unwrap_or(LineStyle::Solid);
    // Gap fill color for dashed/dotted outlines (silx `getLineGapColor`); a
    // no-op on solid lines.
    let gap_color = appearance.gap_color;

    // Draw a closed outline through `path` honoring width and dash style; the
    // path is closed back to its first point before stroking.
    let outline = |mut path: Vec<Pos2>| {
        if let Some(&first) = path.first() {
            path.push(first);
            draw_styled_line(painter, path, color, width, &line_style, gap_color);
        }
    };

    // A representative anchor (in screen pixels) used to place the name label.
    let label_anchor: Option<Pos2> = match roi {
        Roi::Point { x, y } => {
            let p = t.data_to_pixel(*x, *y);
            if let Some(fc) = fill {
                painter.circle_filled(p, 5.0, fc);
            }
            painter.circle_stroke(p, 5.0, Stroke::new(width, color));
            Some(p)
        }
        Roi::Cross { center } => {
            // Full-span cross-hairs through the center (silx CrossROI markers).
            let p = t.data_to_pixel(center.0, center.1);
            let area = t.area;
            draw_styled_line(
                painter,
                vec![pos2(p.x, area.top()), pos2(p.x, area.bottom())],
                color,
                width,
                &line_style,
                gap_color,
            );
            draw_styled_line(
                painter,
                vec![pos2(area.left(), p.y), pos2(area.right(), p.y)],
                color,
                width,
                &line_style,
                gap_color,
            );
            Some(p)
        }
        Roi::Line { start, end } => {
            let a = t.data_to_pixel(start.0, start.1);
            let b = t.data_to_pixel(end.0, end.1);
            draw_styled_line(painter, vec![a, b], color, width, &line_style, gap_color);
            Some(a)
        }
        // Single full-span horizontal/vertical line (silx Horizontal/VerticalLineROI).
        Roi::HLine { y } => {
            let area = t.area;
            let py = t.data_to_pixel(t.x.min, *y).y;
            draw_styled_line(
                painter,
                vec![pos2(area.left(), py), pos2(area.right(), py)],
                color,
                width,
                &line_style,
                gap_color,
            );
            Some(pos2(area.center().x, py))
        }
        Roi::VLine { x } => {
            let area = t.area;
            let px = t.data_to_pixel(*x, t.y.min).x;
            draw_styled_line(
                painter,
                vec![pos2(px, area.top()), pos2(px, area.bottom())],
                color,
                width,
                &line_style,
                gap_color,
            );
            Some(pos2(px, area.top()))
        }
        Roi::Polygon { vertices } if !vertices.is_empty() => {
            let pts: Vec<Pos2> = vertices
                .iter()
                .map(|&(x, y)| t.data_to_pixel(x, y))
                .collect();
            if let Some(fc) = fill {
                fill_polygon(painter, &pts, fc);
            }
            let anchor = pts.first().copied();
            outline(pts);
            anchor
        }
        Roi::Polygon { .. } => None, // empty polygon, skip
        Roi::Circle { center, radius } => {
            // Center pixel and an X-axis perimeter pixel give the screen radius
            // (the transform may differ per axis, so derive from data points).
            let c = t.data_to_pixel(center.0, center.1);
            let edge = t.data_to_pixel(center.0 + radius, center.1);
            let rpx = (edge.x - c.x).abs();
            if let Some(fc) = fill {
                painter.circle_filled(c, rpx, fc);
            }
            // Outline as a 64-gon so dash/dot styling applies (egui has no dashed
            // circle stroke); solid styles still look round at this segment count.
            let n = 64usize;
            let pts: Vec<Pos2> = (0..n)
                .map(|i| {
                    let a = i as f32 * std::f32::consts::TAU / n as f32;
                    egui::pos2(c.x + rpx * a.cos(), c.y + rpx * a.sin())
                })
                .collect();
            outline(pts);
            Some(egui::pos2(c.x, c.y - rpx))
        }
        Roi::Ellipse {
            center,
            radii,
            orientation,
        } => {
            // 27-point outline (silx's segment count) built in DATA space with
            // silx's rotated parametric form
            //   X = r0·cos a·cosθ − r1·sin a·sinθ
            //   Y = r0·cos a·sinθ + r1·sin a·cosθ
            // then mapped through the data→pixel transform, so both the
            // orientation and any non-uniform axis scaling are honored.
            let (coso, sino) = (orientation.cos(), orientation.sin());
            let (r0, r1) = *radii;
            let n = 27usize;
            let pts: Vec<Pos2> = (0..n)
                .map(|i| {
                    let a = i as f64 * std::f64::consts::TAU / n as f64;
                    let (ca, sa) = (a.cos(), a.sin());
                    let dx = r0 * ca * coso - r1 * sa * sino;
                    let dy = r0 * ca * sino + r1 * sa * coso;
                    t.data_to_pixel(center.0 + dx, center.1 + dy)
                })
                .collect();
            if let Some(fc) = fill {
                painter.add(egui::Shape::convex_polygon(pts.clone(), fc, Stroke::NONE));
            }
            // Label anchor at the visual-top (min-y pixel) outline point.
            let anchor = pts
                .iter()
                .copied()
                .min_by(|a, b| a.y.total_cmp(&b.y))
                .unwrap_or_else(|| t.data_to_pixel(center.0, center.1));
            outline(pts);
            Some(anchor)
        }
        Roi::Arc {
            center,
            radius,
            weight,
            start_angle,
            end_angle,
        } => {
            // Annular-sector outline in data space, then mapped to pixels (the
            // transform may scale axes differently, so sample in data space —
            // silx samples the arc with up to ~100 angular steps). The sector is
            // non-convex, so it is drawn as the closed outline only (silx draws
            // the arc shape with `setFill(False)`). The reported inner radius is
            // clamped at 0 (arc_inner_radius); the stored weight is untouched.
            let inner = arc_inner_radius(*radius, *weight);
            let outer = arc_outer_radius(*radius, *weight);
            let pts: Vec<Pos2> = arc_outline(*center, inner, outer, *start_angle, *end_angle)
                .into_iter()
                .map(|(x, y)| t.data_to_pixel(x, y))
                .collect();
            outline(pts);
            // Label anchor at the top of the outer circle.
            Some(t.data_to_pixel(center.0, center.1 + outer))
        }
        Roi::Band {
            begin,
            end,
            width: bw,
        } => {
            if mode == Some(RoiInteractionMode::BandUnbounded) {
                // silx UnboundedMode: draw the three parallel lines spanned across
                // the view instead of the bounded corner polygon, and no fill (the
                // unbounded region is infinite).
                if let Some(segs) =
                    roi.band_unbounded_segments((t.x.min, t.x.max), (t.y.min, t.y.max))
                {
                    for seg in segs {
                        let line: Vec<Pos2> =
                            seg.iter().map(|&(x, y)| t.data_to_pixel(x, y)).collect();
                        draw_styled_line(painter, line, color, width, &line_style, gap_color);
                    }
                }
                // Label anchor at the begin handle.
                Some(t.data_to_pixel(begin.0, begin.1))
            } else {
                // BoundedMode (default): the four band corners form a convex
                // quadrilateral (rotated rect).
                let corners = band_corners_data(*begin, *end, *bw);
                let pts: Vec<Pos2> = corners
                    .iter()
                    .map(|&(x, y)| t.data_to_pixel(x, y))
                    .collect();
                if let Some(fc) = fill {
                    painter.add(egui::Shape::convex_polygon(pts.clone(), fc, Stroke::NONE));
                }
                let anchor = pts.first().copied();
                outline(pts);
                anchor
            }
        }
        _ => {
            // Rect, HRange, VRange
            let r = roi.screen_rect(t);
            if let Some(fc) = fill {
                painter.rect_filled(r, egui::CornerRadius::ZERO, fc);
            }
            outline(vec![
                r.left_top(),
                r.right_top(),
                r.right_bottom(),
                r.left_bottom(),
            ]);
            Some(egui::pos2(r.center().x, r.top()))
        }
    };

    // Handle glyphs (silx `HandleBasedROI` markers): one per `roi.handles()`.
    // A PointROI's own symbol doubles as its handle (silx `PointROI` is a single
    // marker, not a `HandleBasedROI`), so it is not drawn over again.
    if !matches!(roi, Roi::Point { .. }) {
        draw_roi_handles(painter, t, roi, color, mode);
    }

    if let (Some(name), Some(anchor)) = (appearance.name.filter(|s| !s.is_empty()), label_anchor) {
        draw_marker_label(
            painter,
            anchor,
            TextAnchor::Bottom,
            (0.0, 3.0),
            name,
            color,
            Some(style.readout_bg),
        );
    }
}

/// Draw a polyline `path` honoring a [`LineStyle`]: solid for `Solid`, dashes
/// (via egui's dashed-line builder) for the dashed styles, nothing for `None`.
/// When `gap_color` is set on a dashed line, the gaps are first filled with a
/// solid line in that color (silx `gapcolor`), then the dashes drawn on top.
fn draw_styled_line(
    painter: &Painter,
    path: Vec<Pos2>,
    color: Color32,
    width: f32,
    line_style: &LineStyle,
    gap_color: Option<Color32>,
) {
    if path.len() < 2 || !line_style.draws_line() {
        return;
    }
    let stroke = Stroke::new(width, color);
    match line_style.painter_dashes(width) {
        None => {
            painter.add(egui::Shape::line(path, stroke));
        }
        Some((dashes, gaps, offset)) => {
            if let Some(gc) = gap_color {
                painter.add(egui::Shape::line(path.clone(), Stroke::new(width, gc)));
            }
            for shape in egui::Shape::dashed_line_with_offset(&path, stroke, &dashes, &gaps, offset)
            {
                painter.add(shape);
            }
        }
    }
}

/// Draw one marker symbol centered at `c` with full extent `size` (logical
/// points). Filled glyphs use `color`; the stroked glyphs (+ × lines ticks
/// carets) use a line weight scaled from the size. Covers the full silx
/// `_SUPPORTED_SYMBOLS` catalog ([`Symbol`]) — markers share it with curve
/// vertices (silx `Marker` is a `SymbolMixIn`); the on-plot sizing here is the
/// backend analogue of [`crate::widget::high_level`]'s legend-icon painter,
/// which uses fixed-size geometry instead.
fn draw_marker_symbol(painter: &Painter, c: Pos2, symbol: Symbol, size: f32, color: Color32) {
    let r = (size * 0.5).max(1.0);
    let stroke = Stroke::new((size * 0.18).max(1.0), color);
    // Apex-then-arms helper for the open carets (mirrors the legend painter).
    let caret = |apex: Pos2, arm_a: Pos2, arm_b: Pos2| {
        painter.line_segment([apex, arm_a], stroke);
        painter.line_segment([apex, arm_b], stroke);
    };
    match symbol {
        Symbol::Circle => {
            painter.add(egui::Shape::circle_filled(c, r, color));
        }
        Symbol::Point => {
            painter.add(egui::Shape::circle_filled(c, (r * 0.4).max(1.5), color));
        }
        Symbol::Pixel => {
            painter.add(egui::Shape::rect_filled(
                Rect::from_center_size(c, vec2(1.0, 1.0)),
                egui::CornerRadius::ZERO,
                color,
            ));
        }
        Symbol::Square => {
            painter.add(egui::Shape::rect_filled(
                Rect::from_center_size(c, vec2(size, size)),
                egui::CornerRadius::ZERO,
                color,
            ));
        }
        Symbol::Diamond => {
            let pts = vec![
                pos2(c.x, c.y - r),
                pos2(c.x + r, c.y),
                pos2(c.x, c.y + r),
                pos2(c.x - r, c.y),
            ];
            painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        Symbol::Triangle => {
            let pts = vec![
                pos2(c.x, c.y - r),
                pos2(c.x + r, c.y + r),
                pos2(c.x - r, c.y + r),
            ];
            painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        Symbol::Plus => {
            painter.line_segment([pos2(c.x - r, c.y), pos2(c.x + r, c.y)], stroke);
            painter.line_segment([pos2(c.x, c.y - r), pos2(c.x, c.y + r)], stroke);
        }
        Symbol::Cross => {
            painter.line_segment([pos2(c.x - r, c.y - r), pos2(c.x + r, c.y + r)], stroke);
            painter.line_segment([pos2(c.x - r, c.y + r), pos2(c.x + r, c.y - r)], stroke);
        }
        Symbol::VerticalLine => {
            painter.line_segment([pos2(c.x, c.y - r), pos2(c.x, c.y + r)], stroke);
        }
        Symbol::HorizontalLine => {
            painter.line_segment([pos2(c.x - r, c.y), pos2(c.x + r, c.y)], stroke);
        }
        Symbol::TickLeft => {
            painter.line_segment([pos2(c.x - r, c.y), c], stroke);
        }
        Symbol::TickRight => {
            painter.line_segment([c, pos2(c.x + r, c.y)], stroke);
        }
        Symbol::TickUp => {
            painter.line_segment([pos2(c.x, c.y - r), c], stroke);
        }
        Symbol::TickDown => {
            painter.line_segment([c, pos2(c.x, c.y + r)], stroke);
        }
        Symbol::CaretLeft => caret(
            pos2(c.x - r, c.y),
            pos2(c.x + r, c.y - r),
            pos2(c.x + r, c.y + r),
        ),
        Symbol::CaretRight => caret(
            pos2(c.x + r, c.y),
            pos2(c.x - r, c.y - r),
            pos2(c.x - r, c.y + r),
        ),
        Symbol::CaretUp => caret(
            pos2(c.x, c.y - r),
            pos2(c.x - r, c.y + r),
            pos2(c.x + r, c.y + r),
        ),
        Symbol::CaretDown => caret(
            pos2(c.x, c.y + r),
            pos2(c.x - r, c.y - r),
            pos2(c.x + r, c.y - r),
        ),
        Symbol::Heart => {
            // Two upper humps + a lower point: the egui-painter heart standing in
            // for the GPU cardioid marker (silx '♥'), sized to the marker extent.
            let hump = r * 0.5;
            let hy = c.y - r * 0.35;
            painter.add(egui::Shape::circle_filled(
                pos2(c.x - r * 0.45, hy),
                hump,
                color,
            ));
            painter.add(egui::Shape::circle_filled(
                pos2(c.x + r * 0.45, hy),
                hump,
                color,
            ));
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(c.x - r * 0.95, hy),
                    pos2(c.x + r * 0.95, hy),
                    pos2(c.x, c.y + r * 0.95),
                ],
                color,
                Stroke::NONE,
            ));
        }
    }
}

/// Draw marker label `text` attached to the marker point `attach`, honoring the
/// marker's [`TextAnchor`] and the backend's fixed `pixel_padding` (silx
/// `pixel_offset`), optionally over a filled `bg` box.
///
/// Placement goes through the pure, headlessly-tested core: the sign-adjusted
/// pixel padding ([`TextAnchor::pixel_offset`]) shifts the anchor point, then the
/// galley's rect is positioned so the named anchor ([`TextAnchor::rect_offset`])
/// lands on that shifted point. The painter call itself is GPU/UI and so is not
/// covered by those tests.
fn draw_marker_label(
    painter: &Painter,
    attach: Pos2,
    anchor: TextAnchor,
    pixel_padding: (f32, f32),
    text: &str,
    color: Color32,
    bg: Option<Color32>,
) {
    let font = FontId::proportional(11.0);
    let galley = painter.layout_no_wrap(text.to_owned(), font, color);
    let size = galley.size();
    let (ox, oy) = anchor.pixel_offset(pixel_padding);
    let (rx, ry) = anchor.rect_offset((size.x, size.y));
    let top_left = attach + vec2(ox + rx, oy + ry);
    let rect = Rect::from_min_size(top_left, size);
    if let Some(bg) = bg {
        painter.rect_filled(
            rect.expand2(vec2(3.0, 1.0)),
            egui::CornerRadius::same(2),
            bg,
        );
    }
    painter.galley(rect.min, galley, color);
}

/// Draw each marker over the data area (silx `addMarker`): point markers as a
/// symbol, vertical/horizontal markers as a full-span line in the marker's line
/// style, each with optional label text. A marker bound to the right (y2) axis
/// uses `t_right` when present (`doc/design.md` §8).
pub fn draw_markers(
    painter: &Painter,
    t_left: &Transform,
    t_right: Option<&Transform>,
    markers: &[Marker],
) {
    for m in markers {
        let t = match (m.y_axis, t_right) {
            (YAxis::Right, Some(tr)) => tr,
            _ => t_left,
        };
        let area = t.area;
        match m.kind {
            MarkerKind::Point { symbol, size, .. } => {
                let pos = m.screen_point(t).expect("point marker has a screen point");
                if !area.contains(pos) {
                    continue;
                }
                draw_marker_symbol(painter, pos, symbol, size, m.color);
                if let Some(text) = &m.text {
                    // silx point pixel_offset is a fixed (10, 3); widen the X
                    // pad by the symbol radius so the label clears larger glyphs.
                    draw_marker_label(
                        painter,
                        pos,
                        m.text_anchor,
                        (size * 0.5 + 3.0, 3.0),
                        text,
                        m.color,
                        m.bgcolor,
                    );
                }
            }
            MarkerKind::VLine { .. } => {
                let px = m.screen_x(t).expect("vline marker has a screen x");
                if px < area.left() || px > area.right() {
                    continue;
                }
                draw_styled_line(
                    painter,
                    vec![pos2(px, area.top()), pos2(px, area.bottom())],
                    m.color,
                    m.line_width,
                    &m.line_style,
                    None,
                );
                if let Some(text) = &m.text {
                    // silx XMarker: text at the top of the line, ha="left",
                    // va="top", pixel_offset (5, 3).
                    draw_marker_label(
                        painter,
                        pos2(px, area.top()),
                        m.text_anchor,
                        (5.0, 3.0),
                        text,
                        m.color,
                        m.bgcolor,
                    );
                }
            }
            MarkerKind::HLine { .. } => {
                let py = m.screen_y(t).expect("hline marker has a screen y");
                if py < area.top() || py > area.bottom() {
                    continue;
                }
                draw_styled_line(
                    painter,
                    vec![pos2(area.left(), py), pos2(area.right(), py)],
                    m.color,
                    m.line_width,
                    &m.line_style,
                    None,
                );
                if let Some(text) = &m.text {
                    // silx YMarker: text at the right edge of the line,
                    // ha="right", va="top", pixel_offset (5, 3).
                    draw_marker_label(
                        painter,
                        pos2(area.right(), py),
                        m.text_anchor,
                        (5.0, 3.0),
                        text,
                        m.color,
                        m.bgcolor,
                    );
                }
            }
        }
    }
}

/// Draw each per-vertex-colored triangle mesh in the data layer (silx
/// `addTriangles`), clipped to the data area. Each vertex is transformed
/// data→pixel via the shared transform, so the mesh follows pan/zoom and any
/// log / inverted / aspect axes (`doc/design.md` §8).
pub fn draw_triangles(painter: &Painter, t: &Transform, triangles: &[Triangles]) {
    let painter = painter.with_clip_rect(t.area);
    for tris in triangles {
        if tris.indices.is_empty() {
            continue;
        }
        painter.add(egui::Shape::mesh(tris.mesh(t)));
    }
}

/// Fill a (possibly concave) simple polygon with a solid `color` by drawing its
/// ear-clipping triangulation as a [`egui::Mesh`].
///
/// egui's [`egui::Shape::convex_polygon`] fills the convex interpretation of its
/// vertices, so a concave polygon's fill spills across the concavity; routing
/// through [`triangulate_simple_polygon`] makes the fill match the actual
/// outline (silx's matplotlib / pygfx backends fill concave polygons correctly).
/// Convex polygons triangulate to a fan, so this is correct for them too.
fn fill_polygon(painter: &Painter, pts: &[Pos2], color: Color32) {
    let tris = triangulate_simple_polygon(pts);
    if tris.is_empty() {
        return;
    }
    let mut mesh = egui::Mesh::default();
    for &p in pts {
        mesh.colored_vertex(p, color);
    }
    for [a, b, c] in tris {
        mesh.add_triangle(a, b, c);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Draw the shapes whose [`Shape::is_overlay`] matches `overlay` over the data
/// area (silx `addShape`): filled and/or outlined polygons and rectangles, open
/// polylines, and full-span horizontal/vertical lines, in the shape's line
/// style. Drawing is clipped to the data area. Polygon/rectangle fill is drawn
/// as a triangulated [`egui::Mesh`] via `fill_polygon`, so concave polygons
/// fill correctly (`doc/design.md` §8).
///
/// The `overlay` filter is the silx `isOverlay` split (items/shape.py:54-73):
/// non-overlay shapes (`overlay = false`) belong to the base data layer and are
/// drawn under the overlay items (ROIs, markers, crosshair); overlay shapes
/// (`overlay = true`) belong to the overlay layer drawn on top of the chrome
/// (silx `_drawOverlays` / `set_animated`). The caller drives the two passes.
pub fn draw_shapes(painter: &Painter, t: &Transform, shapes: &[Shape], overlay: bool) {
    let painter = painter.with_clip_rect(t.area);
    let area = t.area;
    for s in shapes.iter().filter(|s| s.is_overlay == overlay) {
        match s.kind {
            ShapeKind::Polygon | ShapeKind::Rectangle => {
                let pts = s.screen_points(t);
                if pts.len() < 2 {
                    continue;
                }
                if s.fill {
                    fill_polygon(&painter, &pts, s.color);
                }
                // Close the outline back to the first vertex.
                let mut path = pts;
                path.push(path[0]);
                draw_styled_line(
                    &painter,
                    path,
                    s.color,
                    s.line_width,
                    &s.line_style,
                    s.gap_color,
                );
            }
            ShapeKind::Polyline => {
                draw_styled_line(
                    &painter,
                    s.screen_points(t),
                    s.color,
                    s.line_width,
                    &s.line_style,
                    s.gap_color,
                );
            }
            ShapeKind::HLine => {
                for &yv in &s.y {
                    let py = t.data_to_pixel(t.x.min, yv).y;
                    if py < area.top() || py > area.bottom() {
                        continue;
                    }
                    draw_styled_line(
                        &painter,
                        vec![pos2(area.left(), py), pos2(area.right(), py)],
                        s.color,
                        s.line_width,
                        &s.line_style,
                        s.gap_color,
                    );
                }
            }
            ShapeKind::VLine => {
                for &xv in &s.x {
                    let px = t.data_to_pixel(xv, t.y.min).x;
                    if px < area.left() || px > area.right() {
                        continue;
                    }
                    draw_styled_line(
                        &painter,
                        vec![pos2(px, area.top()), pos2(px, area.bottom())],
                        s.color,
                        s.line_width,
                        &s.line_style,
                        s.gap_color,
                    );
                }
            }
        }
    }
}

/// Draw the infinite line items whose [`Line::is_overlay`] matches `overlay`
/// over the data area (silx `Line`, `items/shape.py:289-393`). Per line,
/// [`Line::clipped_segment`] computes the visible segment in data coordinates
/// against the current viewport (the data `(x_min, x_max)` × `(y_min, y_max)`
/// window from `t`); the two endpoints are then mapped data→pixel via the shared
/// transform and drawn with the line's style. A line that does not cross the
/// viewport produces no segment and is skipped (silx `__updatePoints` sets
/// `coordinates = None`). Drawing is clipped to the data area.
///
/// `Line` is a silx `_OverlayItem` (items/shape.py:289), so the `overlay` filter
/// is the same base-vs-overlay-layer split as [`draw_shapes`].
pub fn draw_lines(painter: &Painter, t: &Transform, lines: &[Line], overlay: bool) {
    let painter = painter.with_clip_rect(t.area);
    // The data-space viewport window the line is clipped against (silx uses the
    // axes' current limits). `Rect::min` is (x_min, y_min), `max` is (x_max,
    // y_max); the transform's per-axis min/max already fold in any aspect
    // expansion. silx Line is a numeric overlay (no log modeling), so the raw
    // window is used.
    let bounds = Rect::from_min_max(
        pos2(t.x.min as f32, t.y.min as f32),
        pos2(t.x.max as f32, t.y.max as f32),
    );
    for line in lines.iter().filter(|l| l.is_overlay == overlay) {
        if let Some((a, b)) = line.clipped_segment(bounds) {
            // `a`/`b` are data coordinates; map to pixels.
            let pa = t.data_to_pixel(a.x as f64, a.y as f64);
            let pb = t.data_to_pixel(b.x as f64, b.y as f64);
            draw_styled_line(
                &painter,
                vec![pa, pb],
                line.color,
                line.line_width,
                &line.line_style,
                line.gap_color,
            );
        }
    }
}

/// Format the cursor's wall-clock time for the crosshair readout: the epoch
/// `epoch` decomposed in `tz` as `YYYY-MM-DD HH:MM:SS.mmm`. Used when the X axis
/// is in [`TickMode::TimeSeries`] so the readout reads the same absolute time the
/// X tick labels do (silx `setTimeZone`), not the raw relative axis coordinate.
fn format_crosshair_time(epoch: f64, tz: TimeZone) -> String {
    let dt = DateTime::from_epoch_seconds_tz(epoch, tz);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        dt.year,
        dt.month,
        dt.day,
        dt.hour,
        dt.minute,
        dt.second,
        dt.microsecond / 1000
    )
}

/// The crosshair readout label `"<x>, <y>"` for cursor data coordinates `(x, y)`.
///
/// The X half honors the X-axis [`TickMode`]: [`TickMode::Numeric`] formats with
/// span-scaled decimals ([`format_coord`]); [`TickMode::TimeSeries`] formats the
/// wall-clock time of `x + x_time_offset` in `x_time_zone`, so the readout stays
/// consistent with the X tick labels (which are laid out over the same offset
/// epoch window). Y is always numeric.
fn crosshair_label(
    x: f64,
    y: f64,
    x_range: (f64, f64),
    y_range: (f64, f64),
    x_tick_mode: TickMode,
    x_time_offset: f64,
    x_time_zone: TimeZone,
) -> String {
    let x_str = match x_tick_mode {
        TickMode::TimeSeries => format_crosshair_time(x + x_time_offset, x_time_zone),
        TickMode::Numeric => format_coord(x, x_range.0, x_range.1),
    };
    format!("{}, {}", x_str, format_coord(y, y_range.0, y_range.1))
}

/// Draw a crosshair through `pos` (clipped to the data area) and a readout box
/// with the data coordinates under the pointer (`doc/design.md` §13 C1). `pos`
/// is expected to lie within `t.area`. The X coordinate is formatted per
/// `x_tick_mode` (numeric, or wall-clock time of `x + x_time_offset` in
/// `x_time_zone` under [`TickMode::TimeSeries`]) so the readout agrees with the X
/// tick labels.
pub fn draw_crosshair(
    painter: &Painter,
    t: &Transform,
    pos: Pos2,
    style: &Style,
    x_tick_mode: TickMode,
    x_time_offset: f64,
    x_time_zone: TimeZone,
) {
    let area = t.area;
    let line = Stroke::new(1.0, style.axis);
    painter.vline(pos.x, area.y_range(), line);
    painter.hline(area.x_range(), pos.y, line);

    let (x, y) = t.pixel_to_data(pos);
    let label = crosshair_label(
        x,
        y,
        (t.x.min, t.x.max),
        (t.y.min, t.y.max),
        x_tick_mode,
        x_time_offset,
        x_time_zone,
    );
    let font = FontId::proportional(11.0);
    let galley = painter.layout_no_wrap(label, font, style.text);
    let pad = egui::vec2(4.0, 2.0);
    let size = galley.size() + pad * 2.0;
    // Prefer the lower-right of the cursor; flip to stay inside the data area.
    let mut min = pos + egui::vec2(10.0, 10.0);
    if min.x + size.x > area.right() {
        min.x = pos.x - 10.0 - size.x;
    }
    if min.y + size.y > area.bottom() {
        min.y = pos.y - 10.0 - size.y;
    }
    painter.rect_filled(
        Rect::from_min_size(min, size),
        egui::CornerRadius::same(2),
        style.readout_bg,
    );
    painter.galley(min + pad, galley, style.text);
}

/// Clamp a colorbar label's center coordinate along the bar's long axis so the
/// full glyph extent (`half` = half the laid-out label size on that axis) stays
/// within the bar span `[lo_edge, hi_edge]`. The tick *mark* still sits at the
/// true value position; only the text is nudged inward at the extremes — a value
/// landing on a bar edge would otherwise center its label on the edge and
/// overhang into a gutter that may itself be clipped (e.g. ScatterView's
/// colorbar reaching the content edge, or `draw_colorbar`'s bar bottom with no
/// x-axis label below it). A no-op for interior ticks; falls back to the bar
/// center when the bar is shorter than the label (`lo > hi`, which would make
/// `clamp` panic). Shared by both colorbar renderers (`draw_colorbar` here and
/// [`crate::widget::colorbar::ColorBarWidget`]).
pub(crate) fn clamp_label_center(center: f32, lo_edge: f32, hi_edge: f32, half: f32) -> f32 {
    let lo = lo_edge + half;
    let hi = hi_edge - half;
    if lo <= hi {
        center.clamp(lo, hi)
    } else {
        0.5 * (lo_edge + hi_edge)
    }
}

/// Draw a vertical colorbar matching `cmap` (top = vmax, bottom = vmin), with a
/// border and value labels on its right edge.
pub fn draw_colorbar(painter: &Painter, rect: Rect, cmap: &Colormap, style: &Style) {
    // Fill with horizontal strips top→bottom; the +0.5 height overlap avoids
    // hairline gaps from rounding strip boundaries to pixels.
    let n = 64usize;
    let strip_h = rect.height() / n as f32;
    for i in 0..n {
        // i = 0 at the top maps to LUT 255 (vmax); i = n-1 to LUT 0 (vmin).
        let lut_idx = (255 * (n - 1 - i) / (n - 1)).min(255);
        let c = cmap.lut[lut_idx];
        let y0 = rect.top() + i as f32 * strip_h;
        let strip = Rect::from_min_max(
            pos2(rect.left(), y0),
            pos2(rect.right(), y0 + strip_h + 0.5),
        );
        painter.rect_filled(
            strip,
            egui::CornerRadius::ZERO,
            Color32::from_rgb(c[0], c[1], c[2]),
        );
    }
    painter.rect_stroke(
        rect,
        egui::CornerRadius::ZERO,
        Stroke::new(1.0, style.axis),
        egui::StrokeKind::Inside,
    );

    let font = FontId::proportional(11.0);
    let axis = Stroke::new(1.0, style.axis);
    if cmap.vmax <= cmap.vmin {
        return;
    }
    // Ticks and their labels follow the normalization, like the data axes:
    // decade ticks under log, nice ticks otherwise. Each tick is placed at the
    // bar fraction the image colors that value at (`Colormap::normalize`), so
    // the colorbar matches the image under any normalization (`doc/design.md` §5).
    let labeled: Vec<(f64, String)> = match cmap.normalization {
        Normalization::Log => log_decade_ticks(cmap.vmin, cmap.vmax)
            .into_iter()
            .map(|v| (v, format_log_tick(v)))
            .collect(),
        _ => {
            let (ticks, step) = nice_ticks(cmap.vmin, cmap.vmax, 6);
            ticks
                .into_iter()
                .map(|v| (v, format_tick(v, step)))
                .collect()
        }
    };
    for (v, label) in labeled {
        let frac = cmap.normalize(v); // 0 at vmin, 1 at vmax, under the normalization
        let py = rect.bottom() - frac * rect.height(); // vmin at bottom
        painter.line_segment([pos2(rect.right(), py), pos2(rect.right() + 3.0, py)], axis);
        // Keep the whole label within the bar's vertical span: the extreme
        // labels are centered on the bar edges and would otherwise overhang into
        // a gutter that may be clipped (see `clamp_label_center`).
        let galley = painter.layout_no_wrap(label, font.clone(), style.text);
        let half_h = galley.size().y * 0.5;
        let cy = clamp_label_center(py, rect.top(), rect.bottom(), half_h);
        painter.galley(pos2(rect.right() + 5.0, cy - half_h), galley, style.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roi_appearance_resolves_color_fallback_and_name() {
        use crate::core::roi::{ManagedRoi, RoiLineStyle};
        let roi = Roi::Point { x: 0.0, y: 0.0 };

        // No override: color falls back to default_color; empty name -> no label.
        let bare = ManagedRoi::new(roi.clone());
        let a = roi_appearance(&bare, Color32::RED);
        assert_eq!(a.color, Some(Color32::RED));
        assert_eq!(a.name, None);
        assert!(!a.selected);
        assert_eq!(a.line_width, Some(1.0));
        assert_eq!(a.line_style, Some(LineStyle::Solid));
        assert_eq!(a.gap_color, None);
        assert_eq!(a.fill, Some(false));

        // Explicit overrides pass through; the per-ROI color wins over default.
        let mut styled = ManagedRoi::new(roi);
        styled.color = Some(Color32::GREEN);
        styled.name = "band".to_string();
        styled.selected = true;
        styled.line_width = 3.5;
        styled.line_style = RoiLineStyle::Dashed;
        styled.gap_color = Some(Color32::BLUE);
        styled.fill = true;
        let b = roi_appearance(&styled, Color32::RED);
        assert_eq!(b.color, Some(Color32::GREEN));
        assert_eq!(b.name, Some("band"));
        assert!(b.selected);
        assert_eq!(b.line_width, Some(3.5));
        assert_eq!(b.line_style, Some(LineStyle::Dashed));
        assert_eq!(b.gap_color, Some(Color32::BLUE));
        assert_eq!(b.fill, Some(true));
    }

    #[test]
    fn colorbar_label_center_stays_within_bar() {
        // Interior tick: unchanged (the clamp is a no-op).
        assert_eq!(clamp_label_center(50.0, 0.0, 100.0, 7.0), 50.0);
        // vmin at the bottom edge: nudged up by half the label height so the
        // full glyph fits above the bar bottom instead of overhanging.
        assert_eq!(clamp_label_center(100.0, 0.0, 100.0, 7.0), 93.0);
        // vmax at the top edge: nudged down by half the label height.
        assert_eq!(clamp_label_center(0.0, 0.0, 100.0, 7.0), 7.0);
        // Degenerate bar shorter than the label: falls back to the bar center
        // (a raw clamp would panic on lo > hi).
        assert_eq!(clamp_label_center(0.0, 40.0, 50.0, 7.0), 45.0);
    }

    #[test]
    fn nice_ticks_lie_within_range_and_are_evenly_spaced() {
        let (ticks, step) = nice_ticks(0.0, 256.0, 8);
        assert!(!ticks.is_empty());
        for &t in &ticks {
            assert!((-1e-6..=256.0 + 1e-6).contains(&t), "{t} out of range");
        }
        for w in ticks.windows(2) {
            assert!((w[1] - w[0] - step).abs() <= step * 1e-6, "uneven spacing");
        }
    }

    #[test]
    fn degenerate_or_inverted_range_yields_no_ticks() {
        assert!(nice_ticks(5.0, 5.0, 8).0.is_empty());
        assert!(nice_ticks(5.0, 1.0, 8).0.is_empty());
    }

    #[test]
    fn nice_ticks_divides_span_by_n_ticks_like_silx() {
        // silx niceNumbers divides by nTicks (not nTicks-1): [0, 100] with
        // nTicks=5 → niceNum(100/5=20, round) = 20 (the finding's example).
        let (_ticks, step) = nice_ticks(0.0, 100.0, 5);
        assert_eq!(step, 20.0);
    }

    #[test]
    fn format_tick_uses_step_appropriate_decimals() {
        assert_eq!(format_tick(2.0, 1.0), "2");
        assert_eq!(format_tick(0.5, 0.5), "0.5");
        assert_eq!(format_tick(0.25, 0.05), "0.25");
    }

    #[test]
    fn log_decade_ticks_are_one_per_power_of_ten() {
        // [1, 1000] spans decades 0..=3 → 1, 10, 100, 1000.
        assert_eq!(
            log_decade_ticks(1.0, 1000.0),
            vec![1.0, 10.0, 100.0, 1000.0]
        );
        // Sub-decade range [2, 9] has no integer power of ten inside it.
        assert!(log_decade_ticks(2.0, 9.0).is_empty());
        // R2-39: a non-positive lower bound is clamped to 1.0 and still draws
        // (silx GLPlotFrame.py:371-375), instead of rendering tickless.
        assert_eq!(log_decade_ticks(0.0, 100.0), vec![1.0, 10.0, 100.0]);
        assert_eq!(log_decade_ticks(-1.0, 100.0), vec![1.0, 10.0, 100.0]);
        // Inverted limits still yield no ticks.
        assert!(log_decade_ticks(100.0, 1.0).is_empty());
    }

    #[test]
    fn log_decade_ticks_coarsen_wide_ranges() {
        // R2-39: 1e0..1e12 spans 12 decades > nTicks(5) → spacing = floor(12/5) =
        // 2, so ticks land every 2 decades (silx niceNumbersForLog10), 7 ticks
        // instead of 13.
        assert_eq!(
            log_decade_ticks(1.0, 1e12),
            vec![1.0, 100.0, 1e4, 1e6, 1e8, 1e10, 1e12]
        );
    }

    #[test]
    fn format_axis_log_tick_uses_silx_exponent_format() {
        // R2-39: silx GLPlotFrame.py:395 "1e%+03d" — signed exponent, ≥2 digits.
        assert_eq!(format_axis_log_tick(100.0), "1e+02");
        assert_eq!(format_axis_log_tick(1e9), "1e+09");
        assert_eq!(format_axis_log_tick(1e-6), "1e-06");
        assert_eq!(format_axis_log_tick(1e12), "1e+12");
    }

    #[test]
    fn log_minor_ticks_gated_on_unit_spacing() {
        // R2-39: sub-ticks only when the decade spacing is 1 (silx step==1 gate).
        let narrow = Axis {
            min: 1.0,
            max: 100.0,
            scale: Scale::Log10,
            inverted: false,
        };
        // 2..9 for decade 1 and 2..9×10 for decade 10 (the top decade 100 is
        // dropped like silx's frange[:-1]); all within [1, 100].
        let minor = log_minor_ticks(&narrow);
        assert!(minor.contains(&2.0) && minor.contains(&90.0));
        assert!(!minor.contains(&200.0));
        // A wide, coarsened range (spacing > 1) draws no sub-ticks.
        let wide = Axis {
            min: 1.0,
            max: 1e12,
            scale: Scale::Log10,
            inverted: false,
        };
        assert!(log_minor_ticks(&wide).is_empty());
    }

    #[test]
    fn format_coord_scales_decimals_to_span() {
        // Wide span → few decimals; narrow span → more.
        assert_eq!(format_coord(123.456, 0.0, 1000.0), "123");
        assert_eq!(format_coord(1.2345, 0.0, 10.0), "1.2");
        assert_eq!(format_coord(0.012345, 0.0, 0.1), "0.012");
        // Degenerate span falls back to 3 decimals.
        assert_eq!(format_coord(1.5, 5.0, 5.0), "1.500");
    }

    #[test]
    fn crosshair_label_numeric_uses_span_scaled_decimals() {
        // Numeric X axis: both halves go through format_coord; the time offset and
        // zone are inert.
        let label = crosshair_label(
            1.2345,
            0.012345,
            (0.0, 10.0),
            (0.0, 0.1),
            TickMode::Numeric,
            1_600_000_000.0, // ignored under Numeric
            TimeZone::Utc,
        );
        assert_eq!(label, "1.2, 0.012");
    }

    #[test]
    fn crosshair_label_time_series_reads_wall_clock_of_x_plus_offset() {
        // TimeSeries X axis: the readout shows the absolute wall-clock time of the
        // relative cursor X plus the epoch offset (the f32-safe strip-chart path),
        // so it matches the X tick labels. Y stays numeric.
        let offset = DateTime::from_civil(2021, 1, 4, 9, 30, 0, 0).to_epoch_seconds();
        // 125.5 s into the window -> 09:32:05.500.
        let label = crosshair_label(
            125.5,
            42.0,
            (0.0, 300.0),
            (0.0, 100.0),
            TickMode::TimeSeries,
            offset,
            TimeZone::Utc,
        );
        assert_eq!(label, "2021-01-04 09:32:05.500, 42");
    }

    #[test]
    fn crosshair_label_time_series_applies_time_zone() {
        // A +9h fixed-offset zone (KST) shifts the displayed wall clock east by 9h.
        let offset = DateTime::from_civil(2021, 1, 4, 0, 0, 0, 0).to_epoch_seconds();
        let label = crosshair_label(
            0.0,
            1.0,
            (0.0, 60.0),
            (0.0, 100.0),
            TickMode::TimeSeries,
            offset,
            TimeZone::FixedOffset {
                seconds_east: 32400,
            },
        );
        assert_eq!(label, "2021-01-04 09:00:00.000, 1");
    }

    #[test]
    fn format_log_tick_plain_in_range_scientific_outside() {
        assert_eq!(format_log_tick(10.0), "10");
        assert_eq!(format_log_tick(0.01), "0.01");
        assert_eq!(format_log_tick(1e8), "1e8");
    }

    #[test]
    fn axis_ticks_time_series_yields_datetime_labels() {
        // A one-week window in epoch seconds (2021-01-04 .. 2021-01-11 UTC).
        let min = crate::core::dtime_ticks::DateTime::from_civil(2021, 1, 4, 0, 0, 0, 0)
            .to_epoch_seconds();
        let max = crate::core::dtime_ticks::DateTime::from_civil(2021, 1, 11, 0, 0, 0, 0)
            .to_epoch_seconds();
        let axis = Axis {
            min,
            max,
            scale: Scale::Linear,
            inverted: false,
        };
        let ticks = axis_ticks_with_mode(&axis, 8, TickMode::TimeSeries, TimeZone::Utc, 0.0);
        assert!(!ticks.is_empty(), "time-series ticks empty");
        // The week window selects the Days unit -> ISO date labels "YYYY-MM-DD".
        for (_, label) in &ticks {
            assert_eq!(label.len(), 10, "label {label:?} not an ISO date");
            let parts: Vec<&str> = label.split('-').collect();
            assert_eq!(parts.len(), 3, "label {label:?} not Y-M-D");
            assert_eq!(parts[0], "2021", "year wrong in {label:?}");
        }
        // Every tick lies within [min, max]: the bracket ticks calc_ticks emits
        // one beyond each end are culled (R2-37, silx visibleDatetimes filter).
        // This window is day-aligned so the endpoints coincide with min/max.
        for (pos, _) in &ticks {
            assert!(
                *pos >= min - 1e-6 && *pos <= max + 1e-6,
                "tick {pos} outside [{min}, {max}]"
            );
        }
        assert!((ticks.first().unwrap().0 - min).abs() <= 1e-6);
        assert!((ticks.last().unwrap().0 - max).abs() <= 1e-6);
    }

    #[test]
    fn axis_ticks_time_series_culls_bracket_ticks_outside_range() {
        // A window whose ends fall BETWEEN day boundaries: 2021-01-04 12:00 ..
        // 2021-01-10 12:00 UTC. calc_ticks brackets one day-tick beyond each end
        // (01-04 and 01-11), which R2-37 must cull so no tick paints in the gutter.
        let min = crate::core::dtime_ticks::DateTime::from_civil(2021, 1, 4, 12, 0, 0, 0)
            .to_epoch_seconds();
        let max = crate::core::dtime_ticks::DateTime::from_civil(2021, 1, 10, 12, 0, 0, 0)
            .to_epoch_seconds();
        let axis = Axis {
            min,
            max,
            scale: Scale::Linear,
            inverted: false,
        };
        let ticks = axis_ticks_with_mode(&axis, 8, TickMode::TimeSeries, TimeZone::Utc, 0.0);
        assert!(!ticks.is_empty(), "time-series ticks empty");
        for (pos, label) in &ticks {
            assert!(
                *pos >= min && *pos <= max,
                "bracket tick {label:?} at {pos} not culled to [{min}, {max}]"
            );
        }
        // The first in-range day boundary is 01-05, the last is 01-10 (01-04 and
        // 01-11 are the culled brackets).
        let first_day = crate::core::dtime_ticks::DateTime::from_civil(2021, 1, 5, 0, 0, 0, 0)
            .to_epoch_seconds();
        assert!((ticks.first().unwrap().0 - first_day).abs() <= 1e-6);
    }

    #[test]
    fn axis_ticks_time_series_offset_shifts_positions_keeps_absolute_labels() {
        // A caller storing relative X (epoch - offset) must get the SAME wall-clock
        // labels as the absolute-epoch layout, with every tick position shifted
        // back by the offset so it lands at the right pixel. This is the f32-safe
        // strip-chart path: small relative vertices, absolute wall-clock ticks.
        let offset = crate::core::dtime_ticks::DateTime::from_civil(2021, 1, 4, 0, 0, 0, 0)
            .to_epoch_seconds();
        let span = 6.0 * 86400.0; // a one-week window
        // Absolute layout: the X values ARE epoch, offset 0.
        let abs_axis = Axis {
            min: offset,
            max: offset + span,
            scale: Scale::Linear,
            inverted: false,
        };
        let abs = axis_ticks_with_mode(&abs_axis, 8, TickMode::TimeSeries, TimeZone::Utc, 0.0);
        // Relative layout: the X values are epoch - offset (start at 0); the offset
        // is supplied so the labels still read absolute.
        let rel_axis = Axis {
            min: 0.0,
            max: span,
            scale: Scale::Linear,
            inverted: false,
        };
        let rel = axis_ticks_with_mode(&rel_axis, 8, TickMode::TimeSeries, TimeZone::Utc, offset);
        assert_eq!(abs.len(), rel.len());
        assert!(!rel.is_empty());
        for ((abs_pos, abs_label), (rel_pos, rel_label)) in abs.iter().zip(&rel) {
            assert_eq!(
                abs_label, rel_label,
                "labels must match the absolute layout"
            );
            assert!(
                (abs_pos - rel_pos - offset).abs() < 1e-6,
                "position {abs_pos} should be {rel_pos} + {offset}"
            );
        }
        // The relative positions stay small (f32-safe), not ~1.6e9.
        assert!(rel.last().unwrap().0 <= span + 1.0);
    }

    #[test]
    fn axis_ticks_time_series_honors_max_ticks_count() {
        // R3-5: the TimeSeries arm must scale with the adaptive `max_ticks`
        // (like the numeric arm), not a fixed count. A 20-hour window with a
        // larger tick budget yields more ticks — before the fix both budgets
        // used the hardcoded 5 and produced identical output.
        let min = crate::core::dtime_ticks::DateTime::from_civil(2021, 1, 4, 0, 0, 0, 0)
            .to_epoch_seconds();
        let max = crate::core::dtime_ticks::DateTime::from_civil(2021, 1, 4, 20, 0, 0, 0)
            .to_epoch_seconds();
        let axis = Axis {
            min,
            max,
            scale: Scale::Linear,
            inverted: false,
        };
        let few = axis_ticks_with_mode(&axis, 5, TickMode::TimeSeries, TimeZone::Utc, 0.0);
        let many = axis_ticks_with_mode(&axis, 15, TickMode::TimeSeries, TimeZone::Utc, 0.0);
        assert!(
            many.len() > few.len(),
            "TimeSeries must honor max_ticks: 15 -> {} ticks vs 5 -> {} ticks",
            many.len(),
            few.len()
        );
    }

    #[test]
    fn axis_ticks_numeric_mode_matches_default_path() {
        // The Numeric tick mode must produce exactly the pre-existing numeric
        // ticks (zero behavior change).
        let axis = Axis {
            min: 0.0,
            max: 256.0,
            scale: Scale::Linear,
            inverted: false,
        };
        let numeric = axis_ticks_with_mode(&axis, 8, TickMode::Numeric, TimeZone::Utc, 0.0);
        let default_path = axis_ticks(&axis, 8);
        assert_eq!(numeric, default_path);
        // And the labels are plain numbers, not dates (no '-' separators after a
        // possible leading sign).
        for (_, label) in &numeric {
            assert!(
                !label.trim_start_matches('-').contains('-'),
                "numeric label {label:?} looks like a date"
            );
        }
    }

    #[test]
    fn axis_ticks_time_series_log_axis_falls_back_to_numeric_decades() {
        // A TimeSeries request on a log axis is ignored (silx ties the time
        // locator to the linear/numeric axis): decade ticks are produced.
        let axis = Axis {
            min: 1.0,
            max: 1000.0,
            scale: Scale::Log10,
            inverted: false,
        };
        let ts = axis_ticks_with_mode(&axis, 8, TickMode::TimeSeries, TimeZone::Utc, 0.0);
        let log = axis_ticks_with_mode(&axis, 8, TickMode::Numeric, TimeZone::Utc, 0.0);
        assert_eq!(ts, log, "log axis should ignore TimeSeries");
    }

    #[test]
    fn axis_ticks_time_series_honors_time_zone() {
        // Same epoch window, laid out in UTC+09:00: daily ticks land on zone
        // midnight and the labels read as the zone-local dates, differing from
        // the UTC layout.
        let jst = TimeZone::FixedOffset {
            seconds_east: 32400,
        };
        let min = crate::core::dtime_ticks::DateTime::from_civil(2021, 1, 4, 0, 0, 0, 0)
            .to_epoch_seconds_tz(jst);
        let max = crate::core::dtime_ticks::DateTime::from_civil(2021, 1, 11, 0, 0, 0, 0)
            .to_epoch_seconds_tz(jst);
        let axis = Axis {
            min,
            max,
            scale: Scale::Linear,
            inverted: false,
        };
        let jst_ticks = axis_ticks_with_mode(&axis, 8, TickMode::TimeSeries, jst, 0.0);
        let utc_ticks = axis_ticks_with_mode(&axis, 8, TickMode::TimeSeries, TimeZone::Utc, 0.0);
        assert!(!jst_ticks.is_empty(), "zoned ticks empty");
        // Every tick sits at local midnight in the zone, and the first label is
        // the zone-local ISO date.
        for (pos, _) in &jst_ticks {
            let d = crate::core::dtime_ticks::DateTime::from_epoch_seconds_tz(*pos, jst);
            assert_eq!((d.hour, d.minute, d.second), (0, 0, 0));
        }
        assert_eq!(jst_ticks.first().unwrap().1, "2021-01-04");
        // The zone offset actually moved the positions vs the UTC layout.
        let jst_pos: Vec<f64> = jst_ticks.iter().map(|(p, _)| *p).collect();
        let utc_pos: Vec<f64> = utc_ticks.iter().map(|(p, _)| *p).collect();
        assert_ne!(jst_pos, utc_pos);
    }

    #[test]
    fn draw_lines_clips_vertical_and_horizontal_to_viewport() {
        // Reproduce the data-space bounds draw_lines builds from a transform and
        // assert the infinite lines clip to the viewport edges. The transform
        // maps data [0,10]x[0,10] over a 100x100 area.
        let area = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0));
        let t = Transform::new(0.0, 10.0, 0.0, 10.0, area);
        let bounds = Rect::from_min_max(
            pos2(t.x.min as f32, t.y.min as f32),
            pos2(t.x.max as f32, t.y.max as f32),
        );

        // Vertical infinite line x = 4 (slope inf, intercept 4).
        let vline = Line::new(f64::INFINITY, 4.0);
        let (a, b) = vline
            .clipped_segment(bounds)
            .expect("vline crosses viewport");
        // Endpoints land on the top/bottom data-y edges at x = 4.
        assert_eq!(a.x, 4.0);
        assert_eq!(b.x, 4.0);
        assert_eq!(a.y.min(b.y), 0.0); // y_min edge
        assert_eq!(a.y.max(b.y), 10.0); // y_max edge

        // Horizontal infinite line y = 7 (slope 0, intercept 7).
        let hline = Line::new(0.0, 7.0);
        let (a, b) = hline
            .clipped_segment(bounds)
            .expect("hline crosses viewport");
        // Endpoints land on the left/right data-x edges at y = 7.
        assert_eq!(a.x.min(b.x), 0.0); // x_min edge
        assert_eq!(a.x.max(b.x), 10.0); // x_max edge
        assert_eq!(a.y, 7.0);
        assert_eq!(b.y, 7.0);

        // A line outside the viewport yields no segment (skipped by draw_lines).
        let outside = Line::new(f64::INFINITY, 99.0);
        assert!(outside.clipped_segment(bounds).is_none());
    }

    #[test]
    fn layout_axes_hidden_zeroes_all_gutters() {
        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 300.0));
        // Axes shown (default): non-zero gutters reserve space inside the rect.
        let shown = layout(full, &ChromeRequest::default());
        assert!(shown.data_area.left() > full.left());
        assert!(shown.data_area.bottom() < full.bottom());
        // Axes hidden: every axis gutter collapses to zero; the data area is the
        // whole rect (silx setAxesDisplayed(False) -> setAxesMargins(0,0,0,0)).
        let hidden = layout(
            full,
            &ChromeRequest {
                axes_hidden: true,
                ..Default::default()
            },
        );
        assert_eq!(hidden.data_area, full);
        assert!(hidden.colorbar.is_none());
    }

    #[test]
    fn layout_axes_hidden_still_reserves_colorbar_strip() {
        // Hiding the axes zeroes the axis gutters but the colorbar (a separate
        // silx widget) still claims its right strip.
        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 300.0));
        let hidden_cbar = layout(
            full,
            &ChromeRequest {
                axes_hidden: true,
                colorbar: true,
                ..Default::default()
            },
        );
        let bar = hidden_cbar.colorbar.expect("colorbar rect");
        // Top/bottom/left gutters are zero; only the right is reduced for the bar.
        assert_eq!(hidden_cbar.data_area.left(), full.left());
        assert_eq!(hidden_cbar.data_area.top(), full.top());
        assert_eq!(hidden_cbar.data_area.bottom(), full.bottom());
        assert!(hidden_cbar.data_area.right() < full.right());
        assert!(bar.left() >= hidden_cbar.data_area.right());
    }

    #[test]
    fn layout_reserves_right_gutter_only_with_colorbar() {
        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 300.0));
        let no_bar = layout(full, &ChromeRequest::default());
        assert!(no_bar.colorbar.is_none());
        let with_bar = layout(
            full,
            &ChromeRequest {
                colorbar: true,
                ..Default::default()
            },
        );
        let bar = with_bar.colorbar.expect("colorbar rect");
        // The colorbar sits to the right of the (narrower) data area.
        assert!(bar.left() >= with_bar.data_area.right());
        assert!(with_bar.data_area.right() < no_bar.data_area.right());
    }

    #[test]
    fn layout_interactive_colorbar_reserves_wider_gutter() {
        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(600.0, 300.0));
        let static_bar = layout(
            full,
            &ChromeRequest {
                colorbar: true,
                ..Default::default()
            },
        );
        let interactive = layout(
            full,
            &ChromeRequest {
                colorbar: true,
                colorbar_interactive: true,
                ..Default::default()
            },
        );
        let s = static_bar.colorbar.expect("static colorbar rect");
        let i = interactive.colorbar.expect("interactive colorbar rect");
        // The interactive bar's rect spans the whole HistogramColorBar width
        // (histogram + strip + labels), so it is much wider than the static strip
        // and leaves a correspondingly narrower data area.
        assert!(i.width() > s.width());
        assert!((i.width() - CBAR_INTERACTIVE_WIDTH).abs() < 1e-3);
        assert!(interactive.data_area.right() < static_bar.data_area.right());
        // Both still pin the bar vertically to the data area (shared frame).
        assert_eq!(i.top(), interactive.data_area.top());
        assert_eq!(i.bottom(), interactive.data_area.bottom());
    }

    #[test]
    fn layout_reserves_right_gutter_for_y2_without_colorbar() {
        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 300.0));
        let plain = layout(
            full,
            &ChromeRequest {
                ..Default::default()
            },
        );
        let with_y2 = layout(
            full,
            &ChromeRequest {
                y2: true,
                ..Default::default()
            },
        );
        // A y2 axis narrows the data area (right gutter holds y2 labels) but
        // adds no colorbar rect.
        assert!(with_y2.colorbar.is_none());
        assert!(with_y2.data_area.right() < plain.data_area.right());
    }

    #[test]
    fn layout_stacks_extra_axes_outward_per_side() {
        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 300.0));
        let plain = layout(full, &ChromeRequest::default());
        let req = ChromeRequest {
            extra: vec![
                ExtraAxisChrome {
                    side: AxisSide::Right,
                    label: false,
                },
                ExtraAxisChrome {
                    side: AxisSide::Right,
                    label: false,
                },
                ExtraAxisChrome {
                    side: AxisSide::Left,
                    label: false,
                },
            ],
            ..Default::default()
        };
        let l = layout(full, &req);
        // Extra right axes narrow the data area on the right, the left one on the
        // left, beyond the plain gutters.
        assert!(l.data_area.right() < plain.data_area.right());
        assert!(l.data_area.left() > plain.data_area.left());
        assert_eq!(l.extra.len(), 3);
        // Two right axes stack outward (second baseline is further right), and the
        // left axis sits left of the data area.
        assert_eq!(l.extra[0].side, AxisSide::Right);
        assert_eq!(l.extra[1].side, AxisSide::Right);
        assert!(l.extra[1].baseline_x > l.extra[0].baseline_x);
        assert!(l.extra[0].baseline_x >= l.data_area.right());
        assert_eq!(l.extra[2].side, AxisSide::Left);
        assert!(l.extra[2].baseline_x <= l.data_area.left());
    }

    #[test]
    fn layout_hidden_axes_drop_extra_slots() {
        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 300.0));
        let l = layout(
            full,
            &ChromeRequest {
                axes_hidden: true,
                extra: vec![ExtraAxisChrome {
                    side: AxisSide::Right,
                    label: true,
                }],
                ..Default::default()
            },
        );
        // Hidden axes zero the gutters and draw no extra-axis chrome.
        assert!(l.extra.is_empty());
        assert_eq!(l.data_area, full);
    }

    #[test]
    fn layout_grows_each_gutter_for_its_label() {
        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 300.0));
        let base = layout(full, &ChromeRequest::default()).data_area;

        let titled = layout(
            full,
            &ChromeRequest {
                title: true,
                ..Default::default()
            },
        )
        .data_area;
        assert!(titled.top() > base.top(), "title grows the top gutter");

        let xlab = layout(
            full,
            &ChromeRequest {
                x_label: true,
                ..Default::default()
            },
        )
        .data_area;
        assert!(
            xlab.bottom() < base.bottom(),
            "x label grows the bottom gutter"
        );

        let ylab = layout(
            full,
            &ChromeRequest {
                y_label: true,
                ..Default::default()
            },
        )
        .data_area;
        assert!(ylab.left() > base.left(), "y label grows the left gutter");

        // A y2 label only claims space when the y2 axis is present.
        let y2lab = layout(
            full,
            &ChromeRequest {
                y2: true,
                y2_label: true,
                ..Default::default()
            },
        )
        .data_area;
        let y2_only = layout(
            full,
            &ChromeRequest {
                y2: true,
                ..Default::default()
            },
        )
        .data_area;
        assert!(
            y2lab.right() < y2_only.right(),
            "y2 label grows the right gutter"
        );
    }

    /// A rotated axis label must land its *visual* center exactly at the target
    /// gutter point — for both the left (−90°) and right/y2 (+90°) turns, at any
    /// label length. Guards the [`draw_rotated_label`] correction: epaint's
    /// `with_angle_and_anchor(_, CENTER_CENTER)` otherwise offsets the center by
    /// `+galley_center`, which pushes a long y2 label out of the clip rect.
    #[test]
    fn rotated_label_visual_center_lands_at_target() {
        use egui::epaint::TextShape;

        let ctx = egui::Context::default();
        let mut galley = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            galley = Some(ui.painter().layout_no_wrap(
                "Temperature [\u{b0}C]".to_owned(),
                FontId::proportional(12.0),
                Color32::WHITE,
            ));
        });
        let galley = galley.expect("run closure executes once");
        // A non-trivial label so the offset is large enough to matter.
        assert!(galley.rect.width() > 40.0, "fixture label should be wide");

        let target = Pos2::new(1124.0, 456.0);
        for angle in [std::f32::consts::FRAC_PI_2, -std::f32::consts::FRAC_PI_2] {
            // Corrected placement (mirrors `draw_rotated_label`).
            let pos = target - galley.rect.center().to_vec2();
            let fixed = TextShape::new(pos, galley.clone(), Color32::WHITE)
                .with_angle_and_anchor(angle, Align2::CENTER_CENTER);
            let c = fixed.visual_bounding_rect().center();
            assert!(
                (c.x - target.x).abs() < 1.0 && (c.y - target.y).abs() < 1.0,
                "angle={angle}: corrected center {c:?} should equal target {target:?}"
            );

            // The naive placement (pos == target) is offset by +galley_center
            // (here ~half the label width, tens of px), i.e. it does NOT land at
            // the target — that was the rendered bug.
            let naive = TextShape::new(target, galley.clone(), Color32::WHITE)
                .with_angle_and_anchor(angle, Align2::CENTER_CENTER);
            let nc = naive.visual_bounding_rect().center();
            assert!(
                (nc - target).length() > 10.0,
                "angle={angle}: naive center {nc:?} must be offset from target {target:?}"
            );
        }
    }
}
