//! Plot chrome drawn with egui's painter: frame, grid, ticks, tick labels, and
//! a vertical colorbar.
//!
//! Everything here derives from the same [`Transform`] that feeds the wgpu data
//! layer, so the axes and the image cannot drift apart (`doc/design.md` §4·§8).
//! Layout reserves fixed-pixel gutters for the labels and (optionally) the
//! colorbar; this is the chrome counterpart of silx's `_PlotWidget` margins.

use egui::epaint::TextShape;
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, Visuals, pos2};

use crate::core::colormap::Colormap;
use crate::core::roi::Roi;
use crate::core::transform::{Axis, Scale, Transform};

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

/// Tick values plus their formatted labels for one axis: "nice" numbers on a
/// linear axis, one-per-decade on a log axis.
fn axis_ticks(axis: &Axis, max_ticks: usize) -> Vec<(f64, String)> {
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

/// Draw the frame, grid, ticks, and tick labels around the data area.
pub fn draw_axes(painter: &Painter, t: &Transform, style: &Style) {
    let area = t.area;
    let axis = Stroke::new(1.0, style.axis);
    let grid = Stroke::new(1.0, style.grid);
    let font = FontId::proportional(11.0);
    let tick_len = 4.0;

    let xticks = axis_ticks(&t.x, 8);
    let yticks = axis_ticks(&t.y, 6);

    // Grid lines first, so the frame and ticks sit on top of them.
    for (xv, _) in &xticks {
        let px = t.data_to_pixel(*xv, t.y.min).x;
        painter.vline(px, area.y_range(), grid);
    }
    for (yv, _) in &yticks {
        let py = t.data_to_pixel(t.x.min, *yv).y;
        painter.hline(area.x_range(), py, grid);
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

/// Draw each region of interest: a translucent fill, a border, and a small
/// square handle at every draggable edge midpoint (`doc/design.md` §13 C3).
pub fn draw_rois(painter: &Painter, t: &Transform, rois: &[Roi], style: &Style) {
    let fill = Color32::from_rgba_unmultiplied(style.axis.r(), style.axis.g(), style.axis.b(), 24);
    let border = Stroke::new(1.0, style.axis);
    for roi in rois {
        let r = roi.screen_rect(t);
        painter.rect_filled(r, egui::CornerRadius::ZERO, fill);
        painter.rect_stroke(
            r,
            egui::CornerRadius::ZERO,
            border,
            egui::StrokeKind::Inside,
        );
        for c in roi.handle_centers(t) {
            let h = Rect::from_center_size(c, egui::vec2(6.0, 6.0));
            painter.rect_filled(h, egui::CornerRadius::ZERO, style.axis);
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

    let (ticks, step) = nice_ticks(cmap.vmin, cmap.vmax, 6);
    let font = FontId::proportional(11.0);
    let axis = Stroke::new(1.0, style.axis);
    let span = cmap.vmax - cmap.vmin;
    if span <= 0.0 {
        return;
    }
    for v in ticks {
        let frac = ((v - cmap.vmin) / span) as f32; // 0 at vmin, 1 at vmax
        let py = rect.bottom() - frac * rect.height(); // vmin at bottom
        painter.line_segment([pos2(rect.right(), py), pos2(rect.right() + 3.0, py)], axis);
        painter.text(
            pos2(rect.right() + 5.0, py),
            Align2::LEFT_CENTER,
            format_tick(v, step),
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
