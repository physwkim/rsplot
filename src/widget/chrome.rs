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
use crate::core::dtime_ticks;
use crate::core::items::LineStyle;
use crate::core::marker::{Marker, MarkerKind, MarkerSymbol};
use crate::core::plot::{GraphGrid, TickMode};
use crate::core::roi::{HandleKind, Roi};
use crate::core::shape::{Shape, ShapeKind};
use crate::core::transform::{Axis, Scale, Transform, YAxis};
use crate::core::triangles::Triangles;

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
            grid: Color32::from_rgba_unmultiplied(text.r(), text.g(), text.b(), 28),
            text,
            readout_bg: Color32::from_rgba_unmultiplied(fill.r(), fill.g(), fill.b(), 210),
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
            self.grid = Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 28);
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
// Extra gutter claimed by an axis title / label when present.
const TITLE_H: f32 = 18.0;
const LABEL_H: f32 = 16.0;

/// What chrome the plot needs space reserved for. Drives [`layout`]'s gutter
/// sizes so titles/labels, a colorbar, and a y2 axis all get room.
#[derive(Clone, Copy, Default)]
pub struct ChromeRequest {
    /// A vertical colorbar in the right gutter.
    pub colorbar: bool,
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
}

/// Reserve gutters for axis labels (a colorbar and/or a right y2 axis, if
/// requested) and return the resulting data area and colorbar rects. A colorbar
/// and a y2 axis both claim the right gutter; the colorbar takes precedence when
/// both are requested. Titles and axis labels each grow their own gutter.
pub fn layout(full: Rect, req: &ChromeRequest) -> ChromeLayout {
    let right_axis = if req.colorbar {
        GUTTER_RIGHT + CBAR_WIDTH + CBAR_LABELS
    } else if req.y2 {
        GUTTER_Y2
    } else {
        GUTTER_RIGHT
    };
    // A y2 label adds rotated text outside the y2 ticks.
    let right = right_axis + if req.y2 && req.y2_label { LABEL_H } else { 0.0 };
    let left = GUTTER_LEFT + if req.y_label { LABEL_H } else { 0.0 };
    let top = GUTTER_TOP + if req.title { TITLE_H } else { 0.0 };
    let bottom = GUTTER_BOTTOM + if req.x_label { LABEL_H } else { 0.0 };

    let data_area = Rect::from_min_max(
        pos2(full.left() + left, full.top() + top),
        pos2(full.right() - right, full.bottom() - bottom),
    );
    let colorbar = req.colorbar.then(|| {
        let x0 = data_area.right() + GUTTER_RIGHT;
        Rect::from_min_max(
            pos2(x0, data_area.top()),
            pos2(x0 + CBAR_WIDTH, data_area.bottom()),
        )
    });
    ChromeLayout {
        data_area,
        colorbar,
    }
}

/// "Nice" rounding of a span to {1, 2, 5} × 10ⁿ — the classic axis-tick
/// heuristic (Heckbert, *Graphics Gems*).
fn nice_num(range: f64, round: bool) -> f64 {
    if range <= 0.0 {
        return 1.0;
    }
    let exp = range.log10().floor();
    let frac = range / 10f64.powf(exp);
    let nice = if round {
        if frac < 1.5 {
            1.0
        } else if frac < 3.0 {
            2.0
        } else if frac < 7.0 {
            5.0
        } else {
            10.0
        }
    } else if frac <= 1.0 {
        1.0
    } else if frac <= 2.0 {
        2.0
    } else if frac <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * 10f64.powf(exp)
}

/// "Nice" tick values within `[min, max]` plus the step between them.
pub fn nice_ticks(min: f64, max: f64, max_ticks: usize) -> (Vec<f64>, f64) {
    // partial_cmp (not `max > min`) so NaN limits fall through to "no ticks".
    let ascending = matches!(max.partial_cmp(&min), Some(std::cmp::Ordering::Greater));
    if !ascending || max_ticks < 2 {
        return (Vec::new(), 1.0);
    }
    let range = nice_num(max - min, false);
    let step = nice_num(range / (max_ticks - 1) as f64, true);
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

/// Decade tick values within `[min, max]` for a log10 axis: one tick per power
/// of ten (…, 0.1, 1, 10, 100, …). Empty if the range is not strictly positive.
fn log_decade_ticks(min: f64, max: f64) -> Vec<f64> {
    let valid = min.is_finite() && max.is_finite() && min > 0.0 && max > min;
    if !valid {
        return Vec::new();
    }
    let k0 = min.log10().ceil() as i32;
    let k1 = max.log10().floor() as i32;
    (k0..=k1).map(|k| 10f64.powi(k)).collect()
}

/// Format a log-axis decade tick: plain decimal in the everyday range,
/// scientific notation outside it (e.g. 1e-6, 1e9).
fn format_log_tick(v: f64) -> String {
    if (1e-4..1e6).contains(&v) {
        format!("{v}")
    } else {
        format!("{v:e}")
    }
}

/// Default target tick count for a date-time (TimeSeries) axis, mirroring silx
/// `NiceDateLocator(numTicks=5)` (`backends/BackendMatplotlib.py:162`).
const TIME_SERIES_NUM_TICKS: usize = 5;

/// Tick values plus their formatted labels for one axis: "nice" numbers on a
/// linear axis, one-per-decade on a log axis. The default [`TickMode::Numeric`]
/// path is unchanged.
fn axis_ticks(axis: &Axis, max_ticks: usize) -> Vec<(f64, String)> {
    axis_ticks_with_mode(axis, max_ticks, TickMode::Numeric)
}

/// As [`axis_ticks`] but honoring the axis [`TickMode`]. With
/// [`TickMode::TimeSeries`] the axis data values are treated as epoch seconds
/// (UTC) and tick positions + labels are produced by [`dtime_ticks`]
/// (`calc_ticks` / `format_ticks`), mirroring silx `NiceDateLocator` +
/// `NiceAutoDateFormatter` (`backends/BackendMatplotlib.py:153-242`). A
/// TimeSeries tick mode is honored only on a [`Scale::Linear`] axis (silx ties
/// the time locator to the linear/numeric axis); a log axis falls back to the
/// numeric decade ticks.
fn axis_ticks_with_mode(axis: &Axis, max_ticks: usize, tick_mode: TickMode) -> Vec<(f64, String)> {
    if tick_mode == TickMode::TimeSeries && axis.scale == Scale::Linear {
        let (lo, hi) = if axis.max >= axis.min {
            (axis.min, axis.max)
        } else {
            (axis.max, axis.min)
        };
        let (ticks, spacing, unit) = dtime_ticks::calc_ticks(lo, hi, TIME_SERIES_NUM_TICKS);
        let labels = dtime_ticks::format_ticks(&ticks, spacing, unit);
        return ticks.into_iter().zip(labels).collect();
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
            .map(|v| (v, format_log_tick(v)))
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
    let valid =
        axis.min.is_finite() && axis.max.is_finite() && axis.min > 0.0 && axis.max > axis.min;
    if !valid {
        return Vec::new();
    }
    let k0 = axis.min.log10().floor() as i32;
    let k1 = axis.max.log10().ceil() as i32;
    let mut ticks = Vec::new();
    for k in k0..=k1 {
        let decade = 10f64.powi(k);
        for m in 2..10 {
            let v = m as f64 * decade;
            if v > axis.min && v < axis.max {
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
/// `None` falls back to the defaults (8 for X, 6 for Y).
pub fn draw_axes(
    painter: &Painter,
    t: &Transform,
    style: &Style,
    grid_mode: GraphGrid,
    x_max_ticks: Option<usize>,
    y_max_ticks: Option<usize>,
) {
    draw_axes_with_tick_modes(
        painter,
        t,
        style,
        grid_mode,
        x_max_ticks,
        y_max_ticks,
        TickMode::Numeric,
        TickMode::Numeric,
    );
}

/// As [`draw_axes`] but honoring a per-axis [`TickMode`]: with
/// [`TickMode::TimeSeries`] that axis' tick positions and labels are produced
/// by [`dtime_ticks`] (epoch-seconds data values, UTC), mirroring silx's
/// `NiceDateLocator` time-axis path (`backends/BackendMatplotlib.py:153-242`).
/// The default `Numeric`/`Numeric` matches [`draw_axes`] exactly.
#[allow(clippy::too_many_arguments)]
pub fn draw_axes_with_tick_modes(
    painter: &Painter,
    t: &Transform,
    style: &Style,
    grid_mode: GraphGrid,
    x_max_ticks: Option<usize>,
    y_max_ticks: Option<usize>,
    x_tick_mode: TickMode,
    y_tick_mode: TickMode,
) {
    let area = t.area;
    let axis = Stroke::new(1.0, style.axis);
    let grid = Stroke::new(1.0, style.grid);
    let minor_grid = Stroke::new(
        1.0,
        Color32::from_rgba_unmultiplied(
            style.grid.r(),
            style.grid.g(),
            style.grid.b(),
            style.grid.a() / 2,
        ),
    );
    let font = FontId::proportional(11.0);
    let tick_len = 4.0;

    let xticks = axis_ticks_with_mode(&t.x, x_max_ticks.unwrap_or(8), x_tick_mode);
    let yticks = axis_ticks_with_mode(&t.y, y_max_ticks.unwrap_or(6), y_tick_mode);

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
    // Left Y label: rotate a quarter turn counter-clockwise (reads bottom→top).
    if let Some(t) = labels.y {
        let galley = painter.layout_no_wrap(t.to_owned(), label_font.clone(), style.text);
        let pos = pos2(full.left() + LABEL_H * 0.5, area.center().y);
        painter.add(egui::Shape::Text(
            TextShape::new(pos, galley, style.text)
                .with_angle_and_anchor(-std::f32::consts::FRAC_PI_2, Align2::CENTER_CENTER),
        ));
    }
    // Right y2 label: rotate a quarter turn clockwise (reads top→bottom).
    if with_y2 && let Some(t) = labels.y2 {
        let galley = painter.layout_no_wrap(t.to_owned(), label_font, style.text);
        let pos = pos2(full.right() - LABEL_H * 0.5, area.center().y);
        painter.add(egui::Shape::Text(
            TextShape::new(pos, galley, style.text)
                .with_angle_and_anchor(std::f32::consts::FRAC_PI_2, Align2::CENTER_CENTER),
        ));
    }
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
    /// Whether the ROI interior is filled with a translucent tint. `None` keeps
    /// the legacy faint fill; `Some(false)` draws no fill (silx `setFill(False)`).
    pub fill: Option<bool>,
}

/// Draw each region of interest with default (axis-color, unnamed, unselected)
/// appearance: a translucent fill, a border, and a small square handle at every
/// draggable edge midpoint (`doc/design.md` §13 C3).
pub fn draw_rois(painter: &Painter, t: &Transform, rois: &[Roi], style: &Style) {
    for roi in rois {
        draw_roi(painter, t, roi, &RoiAppearance::default(), style);
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

/// Draw the handle glyphs of a ROI in `color`, one per [`RoiHandle`] from
/// [`Roi::handles`]. Mirrors the silx handle markers (`items/_roi_base.py`
/// `addHandle`/`addTranslateHandle`): a translate or center handle is a `+`
/// (silx `"+"`), every other handle (shape vertex / band edge) is a small filled
/// square (silx default `"s"`).
fn draw_roi_handles(painter: &Painter, t: &Transform, roi: &Roi, color: Color32) {
    for handle in roi.handles() {
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
pub fn draw_roi(
    painter: &Painter,
    t: &Transform,
    roi: &Roi,
    appearance: &RoiAppearance,
    style: &Style,
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
    let fill =
        fill_enabled.then(|| Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 24));
    let line_style = appearance.line_style.clone().unwrap_or(LineStyle::Solid);

    // Draw a closed outline through `path` honoring width and dash style; the
    // path is closed back to its first point before stroking.
    let outline = |mut path: Vec<Pos2>| {
        if let Some(&first) = path.first() {
            path.push(first);
            draw_styled_line(painter, path, color, width, &line_style, None);
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
                None,
            );
            draw_styled_line(
                painter,
                vec![pos2(area.left(), p.y), pos2(area.right(), p.y)],
                color,
                width,
                &line_style,
                None,
            );
            Some(p)
        }
        Roi::Line { start, end } => {
            let a = t.data_to_pixel(start.0, start.1);
            let b = t.data_to_pixel(end.0, end.1);
            draw_styled_line(painter, vec![a, b], color, width, &line_style, None);
            Some(a)
        }
        Roi::Polygon { vertices } if !vertices.is_empty() => {
            let pts: Vec<Pos2> = vertices
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
        Roi::Ellipse { center, radii } => {
            let c = t.data_to_pixel(center.0, center.1);
            let ex = t.data_to_pixel(center.0 + radii.0, center.1);
            let ey = t.data_to_pixel(center.0, center.1 + radii.1);
            let rx = (ex.x - c.x).abs();
            let ry = (ey.y - c.y).abs();
            // Approximate the ellipse outline with a polygon (silx draws it with
            // 27 points; match that segment count).
            let n = 27usize;
            let pts: Vec<Pos2> = (0..n)
                .map(|i| {
                    let a = i as f32 * std::f32::consts::TAU / n as f32;
                    egui::pos2(c.x + rx * a.cos(), c.y + ry * a.sin())
                })
                .collect();
            if let Some(fc) = fill {
                painter.add(egui::Shape::convex_polygon(pts.clone(), fc, Stroke::NONE));
            }
            outline(pts);
            Some(egui::pos2(c.x, c.y - ry))
        }
        Roi::Arc {
            center,
            inner_radius,
            outer_radius,
            start_angle,
            end_angle,
        } => {
            // Annular-sector outline in data space, then mapped to pixels (the
            // transform may scale axes differently, so sample in data space —
            // silx samples the arc with up to ~100 angular steps). The sector is
            // non-convex, so it is drawn as the closed outline only (silx draws
            // the arc shape with `setFill(False)`).
            let pts: Vec<Pos2> = arc_outline(
                *center,
                *inner_radius,
                *outer_radius,
                *start_angle,
                *end_angle,
            )
            .into_iter()
            .map(|(x, y)| t.data_to_pixel(x, y))
            .collect();
            outline(pts);
            // Label anchor at the top of the outer circle.
            Some(t.data_to_pixel(center.0, center.1 + outer_radius))
        }
        Roi::Band {
            begin,
            end,
            width: bw,
        } => {
            // The four band corners form a convex quadrilateral (rotated rect).
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
        draw_roi_handles(painter, t, roi, color);
    }

    if let (Some(name), Some(anchor)) = (appearance.name.filter(|s| !s.is_empty()), label_anchor) {
        draw_marker_label(
            painter,
            anchor + vec2(0.0, -3.0),
            Align2::CENTER_BOTTOM,
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
/// points). Filled glyphs use `color`; the stroked glyphs (+ ×) use a line
/// weight scaled from the size.
fn draw_marker_symbol(painter: &Painter, c: Pos2, symbol: MarkerSymbol, size: f32, color: Color32) {
    let r = (size * 0.5).max(1.0);
    let stroke = Stroke::new((size * 0.18).max(1.0), color);
    match symbol {
        MarkerSymbol::Circle => {
            painter.add(egui::Shape::circle_filled(c, r, color));
        }
        MarkerSymbol::Point => {
            painter.add(egui::Shape::circle_filled(c, (r * 0.4).max(1.5), color));
        }
        MarkerSymbol::Pixel => {
            painter.add(egui::Shape::rect_filled(
                Rect::from_center_size(c, vec2(1.0, 1.0)),
                egui::CornerRadius::ZERO,
                color,
            ));
        }
        MarkerSymbol::Square => {
            painter.add(egui::Shape::rect_filled(
                Rect::from_center_size(c, vec2(size, size)),
                egui::CornerRadius::ZERO,
                color,
            ));
        }
        MarkerSymbol::Diamond => {
            let pts = vec![
                pos2(c.x, c.y - r),
                pos2(c.x + r, c.y),
                pos2(c.x, c.y + r),
                pos2(c.x - r, c.y),
            ];
            painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        MarkerSymbol::Plus => {
            painter.line_segment([pos2(c.x - r, c.y), pos2(c.x + r, c.y)], stroke);
            painter.line_segment([pos2(c.x, c.y - r), pos2(c.x, c.y + r)], stroke);
        }
        MarkerSymbol::Cross => {
            painter.line_segment([pos2(c.x - r, c.y - r), pos2(c.x + r, c.y + r)], stroke);
            painter.line_segment([pos2(c.x - r, c.y + r), pos2(c.x + r, c.y - r)], stroke);
        }
    }
}

/// Draw marker label `text` anchored at `pos`, optionally over a filled `bg` box.
fn draw_marker_label(
    painter: &Painter,
    pos: Pos2,
    anchor: Align2,
    text: &str,
    color: Color32,
    bg: Option<Color32>,
) {
    let font = FontId::proportional(11.0);
    let galley = painter.layout_no_wrap(text.to_owned(), font, color);
    let rect = anchor.anchor_size(pos, galley.size());
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
                    draw_marker_label(
                        painter,
                        pos + vec2(size * 0.5 + 3.0, 0.0),
                        Align2::LEFT_CENTER,
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
                    draw_marker_label(
                        painter,
                        pos2(px + 3.0, area.top() + 2.0),
                        Align2::LEFT_TOP,
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
                    draw_marker_label(
                        painter,
                        pos2(area.left() + 3.0, py - 2.0),
                        Align2::LEFT_BOTTOM,
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

/// Draw each shape over the data area (silx `addShape`): filled and/or outlined
/// polygons and rectangles, open polylines, and full-span horizontal/vertical
/// lines, in the shape's line style. Drawing is clipped to the data area. Fill
/// is convex-only (egui's `convex_polygon`): correct for rectangles and convex
/// polygons (`doc/design.md` §8).
pub fn draw_shapes(painter: &Painter, t: &Transform, shapes: &[Shape]) {
    let painter = painter.with_clip_rect(t.area);
    let area = t.area;
    for s in shapes {
        match s.kind {
            ShapeKind::Polygon | ShapeKind::Rectangle => {
                let pts = s.screen_points(t);
                if pts.len() < 2 {
                    continue;
                }
                if s.fill {
                    painter.add(egui::Shape::convex_polygon(
                        pts.clone(),
                        s.color,
                        Stroke::NONE,
                    ));
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

/// Draw a crosshair through `pos` (clipped to the data area) and a readout box
/// with the data coordinates under the pointer (`doc/design.md` §13 C1). `pos`
/// is expected to lie within `t.area`.
pub fn draw_crosshair(painter: &Painter, t: &Transform, pos: Pos2, style: &Style) {
    let area = t.area;
    let line = Stroke::new(1.0, style.axis);
    painter.vline(pos.x, area.y_range(), line);
    painter.hline(area.x_range(), pos.y, line);

    let (x, y) = t.pixel_to_data(pos);
    let label = format!(
        "{}, {}",
        format_coord(x, t.x.min, t.x.max),
        format_coord(y, t.y.min, t.y.max),
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
        painter.text(
            pos2(rect.right() + 5.0, py),
            Align2::LEFT_CENTER,
            label,
            font.clone(),
            style.text,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Non-positive or inverted limits yield no ticks (log undefined).
        assert!(log_decade_ticks(0.0, 100.0).is_empty());
        assert!(log_decade_ticks(-1.0, 100.0).is_empty());
        assert!(log_decade_ticks(100.0, 1.0).is_empty());
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
        let ticks = axis_ticks_with_mode(&axis, 8, TickMode::TimeSeries);
        assert!(!ticks.is_empty(), "time-series ticks empty");
        // The week window selects the Days unit -> ISO date labels "YYYY-MM-DD".
        for (_, label) in &ticks {
            assert_eq!(label.len(), 10, "label {label:?} not an ISO date");
            let parts: Vec<&str> = label.split('-').collect();
            assert_eq!(parts.len(), 3, "label {label:?} not Y-M-D");
            assert_eq!(parts[0], "2021", "year wrong in {label:?}");
        }
        // The positions bracket the range (calc_ticks brackets [min, max]).
        assert!(ticks.first().unwrap().0 <= min + 1e-6);
        assert!(ticks.last().unwrap().0 >= max - 1e-6);
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
        let numeric = axis_ticks_with_mode(&axis, 8, TickMode::Numeric);
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
        let ts = axis_ticks_with_mode(&axis, 8, TickMode::TimeSeries);
        let log = axis_ticks_with_mode(&axis, 8, TickMode::Numeric);
        assert_eq!(ts, log, "log axis should ignore TimeSeries");
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
}
