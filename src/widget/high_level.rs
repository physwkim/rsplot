//! High-level silx-style plot widgets.
//!
//! These types own backend state and expose the user-facing plotting API:
//! callers add data items, tune axes/labels/colors, then call [`PlotWidget::show`]
//! from their egui app. The low-level stateless renderer remains
//! [`crate::PlotView`].

use std::fmt;
use std::mem;
use std::ops::{Deref, DerefMut};
use std::path::Path;

use egui::Color32;
use egui_wgpu::RenderState;

use crate::core::backend::{
    Backend, CurveColor, CurveSpec, ImagePixelsSpec, ImageSpec, ItemHandle, MarkerSpec, PickResult,
    ShapeSpec, TriangleSpec,
};
use crate::core::colormap::Colormap;
use crate::core::items::{Baseline, LineStyle, Symbol};
use crate::core::marker::{Marker, MarkerKind, MarkerSymbol};
use crate::core::plot::{GraphGrid, Plot, PlotId};
use crate::core::roi::Roi;
use crate::core::shape::{Shape, ShapeKind};
use crate::core::transform::{Margins, Scale, YAxis};
use crate::core::triangles::Triangles;
use crate::render::backend_wgpu::WgpuBackend;
use crate::render::gpu_curve::CurveData;
use crate::render::gpu_image::{ImageData, ImagePixels};
use crate::render::save::SaveError;
use crate::widget::plot_widget::{PlotInteractionMode, PlotResponse, PlotView};

/// Data validation failures returned by helper APIs that build derived items.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlotDataError {
    /// A row-major image-like buffer did not have `width * height` values.
    ImageDataLength { expected: usize, actual: usize },
    /// Histogram counts require exactly one more edge than bin count.
    HistogramLength { bins: usize, edges: usize },
    /// Requested profile row is outside the image height.
    ProfileRow { row: u32, height: u32 },
    /// Requested profile column is outside the image width.
    ProfileColumn { column: u32, width: u32 },
}

impl fmt::Display for PlotDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ImageDataLength { expected, actual } => {
                write!(
                    f,
                    "image data length {actual} does not match expected {expected}"
                )
            }
            Self::HistogramLength { bins, edges } => {
                write!(
                    f,
                    "histogram with {bins} bins requires {bins_plus_one} edges",
                    bins_plus_one = bins + 1
                )?;
                write!(f, ", got {edges}")
            }
            Self::ProfileRow { row, height } => {
                write!(f, "profile row {row} is outside image height {height}")
            }
            Self::ProfileColumn { column, width } => {
                write!(f, "profile column {column} is outside image width {width}")
            }
        }
    }
}

impl std::error::Error for PlotDataError {}

/// Summary statistics over finite values.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ValueStats {
    /// Number of input values.
    pub count: usize,
    /// Number of finite values used for min/max/mean.
    pub finite_count: usize,
    /// Minimum finite value.
    pub min: Option<f64>,
    /// Maximum finite value.
    pub max: Option<f64>,
    /// Mean of finite values.
    pub mean: Option<f64>,
}

impl ValueStats {
    /// Compute statistics from `f64` values, ignoring non-finite values for
    /// min/max/mean while still counting them in [`Self::count`].
    pub fn from_f64(values: &[f64]) -> Self {
        let mut stats = Self {
            count: values.len(),
            ..Self::default()
        };
        let mut sum = 0.0;
        for value in values.iter().copied().filter(|value| value.is_finite()) {
            stats.finite_count += 1;
            stats.min = Some(stats.min.map_or(value, |min| min.min(value)));
            stats.max = Some(stats.max.map_or(value, |max| max.max(value)));
            sum += value;
        }
        if stats.finite_count > 0 {
            stats.mean = Some(sum / stats.finite_count as f64);
        }
        stats
    }

    /// Compute statistics from `f32` values, ignoring non-finite values for
    /// min/max/mean while still counting them in [`Self::count`].
    pub fn from_f32(values: &[f32]) -> Self {
        let mut stats = Self {
            count: values.len(),
            ..Self::default()
        };
        let mut sum = 0.0;
        for value in values.iter().copied().filter(|value| value.is_finite()) {
            let value = value as f64;
            stats.finite_count += 1;
            stats.min = Some(stats.min.map_or(value, |min| min.min(value)));
            stats.max = Some(stats.max.map_or(value, |max| max.max(value)));
            sum += value;
        }
        if stats.finite_count > 0 {
            stats.mean = Some(sum / stats.finite_count as f64);
        }
        stats
    }
}

/// Statistics for curve-like items.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurveStats {
    pub x: ValueStats,
    pub y: ValueStats,
    pub y_axis: YAxis,
}

/// Statistics for image-like items.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageStats {
    pub width: u32,
    pub height: u32,
    pub pixel_count: usize,
    /// Scalar pixel statistics. `None` for direct RGBA images and masks.
    pub scalar: Option<ValueStats>,
}

/// Geometry shared by image-like items.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageGeometry {
    pub origin: (f64, f64),
    pub scale: (f64, f64),
    pub alpha: f32,
}

impl Default for ImageGeometry {
    fn default() -> Self {
        Self {
            origin: (0.0, 0.0),
            scale: (1.0, 1.0),
            alpha: 1.0,
        }
    }
}

/// Per-item statistics retained by [`PlotWidget`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ItemStats {
    Curve(CurveStats),
    Image(ImageStats),
}

/// High-level item family tracked by [`PlotWidget`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlotItemKind {
    Curve,
    Histogram,
    Scatter,
    Image,
    Mask,
    Triangles,
    Shape,
    Marker,
}

impl PlotItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Curve => "curve",
            Self::Histogram => "histogram",
            Self::Scatter => "scatter",
            Self::Image => "image",
            Self::Mask => "mask",
            Self::Triangles => "triangles",
            Self::Shape => "shape",
            Self::Marker => "marker",
        }
    }

    pub fn is_curve_like(self) -> bool {
        matches!(self, Self::Curve | Self::Histogram | Self::Scatter)
    }

    pub fn is_image_like(self) -> bool {
        matches!(self, Self::Image | Self::Mask)
    }
}

/// High-level events queued by [`PlotWidget`] for application code to drain.
#[derive(Clone, Debug, PartialEq)]
pub enum PlotEvent {
    ItemAdded {
        handle: ItemHandle,
        kind: PlotItemKind,
    },
    ItemUpdated {
        handle: ItemHandle,
        kind: PlotItemKind,
    },
    ItemRemoved {
        handle: ItemHandle,
        kind: PlotItemKind,
    },
    ActiveItemChanged {
        previous: Option<ItemHandle>,
        current: Option<ItemHandle>,
    },
    LimitsChanged,
    RoiChanged {
        index: usize,
    },
    RoisCleared,
}

/// Return value of [`PlotWidget::show_legend`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LegendResponse {
    pub selected: Option<ItemHandle>,
    pub activated: Option<ItemHandle>,
}

/// Return value of [`PlotWidget::show_toolbar`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolbarResponse {
    pub reset_zoom: bool,
    pub interaction_mode_changed: bool,
    pub cursor_changed: bool,
    pub grid_changed: bool,
    pub minor_grid_changed: bool,
    pub aspect_changed: bool,
    pub x_log_changed: bool,
    pub y_log_changed: bool,
    pub x_inverted_changed: bool,
    pub y_inverted_changed: bool,
}

/// Return value of [`PlotWidget::show_with_toolbar`].
pub struct PlotWithToolbarResponse {
    pub toolbar: ToolbarResponse,
    pub plot: PlotResponse,
}

/// Silx-style name for a standalone high-level plot surface.
///
/// In egui the native application owns the actual OS window, so `PlotWindow`
/// is an API alias for [`PlotWidget`] with the same retained item and toolbar
/// behavior.
pub type PlotWindow = PlotWidget;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolbarIcon {
    Home,
    Select,
    Pan,
    Zoom,
    Cursor,
    Grid,
    MinorGrid,
    Aspect,
    LogX,
    LogY,
    InvertX,
    InvertY,
}

impl ToolbarIcon {
    fn size(self) -> egui::Vec2 {
        match self {
            Self::LogX | Self::LogY => egui::vec2(34.0, 24.0),
            _ => egui::vec2(28.0, 24.0),
        }
    }
}

fn expected_image_len(width: u32, height: u32) -> usize {
    (width as usize).saturating_mul(height as usize)
}

fn validate_image_len(width: u32, height: u32, actual: usize) -> Result<usize, PlotDataError> {
    let expected = expected_image_len(width, height);
    if actual == expected {
        Ok(expected)
    } else {
        Err(PlotDataError::ImageDataLength { expected, actual })
    }
}

fn toolbar_icon_button(
    ui: &mut egui::Ui,
    icon: ToolbarIcon,
    selected: bool,
    tooltip: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(icon.size(), egui::Sense::click());
    let response = response.on_hover_text(tooltip);
    if ui.is_rect_visible(rect) {
        draw_toolbar_button(ui, rect, &response, selected, icon);
    }
    response
}

fn draw_toolbar_button(
    ui: &egui::Ui,
    rect: egui::Rect,
    response: &egui::Response,
    selected: bool,
    icon: ToolbarIcon,
) {
    let visuals = ui.style().interact_selectable(response, selected);
    let painter = ui.painter();
    let button_rect = rect.shrink(1.0);
    if selected || response.hovered() || response.has_focus() {
        painter.rect_filled(button_rect, 2.0, visuals.weak_bg_fill);
        painter.rect_stroke(
            button_rect,
            2.0,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
    }

    let color = if !ui.is_enabled() {
        ui.visuals().weak_text_color()
    } else if selected {
        ui.visuals().selection.stroke.color
    } else {
        visuals.fg_stroke.color
    };
    draw_toolbar_icon(painter, rect.shrink(5.0), icon, color);
}

fn draw_toolbar_icon(painter: &egui::Painter, rect: egui::Rect, icon: ToolbarIcon, color: Color32) {
    let stroke = egui::Stroke::new(1.6, color);
    match icon {
        ToolbarIcon::Home => draw_home_icon(painter, rect, stroke),
        ToolbarIcon::Select => draw_select_icon(painter, rect, stroke),
        ToolbarIcon::Pan => draw_pan_icon(painter, rect, stroke),
        ToolbarIcon::Zoom => draw_zoom_icon(painter, rect, stroke),
        ToolbarIcon::Cursor => draw_cursor_icon(painter, rect, stroke),
        ToolbarIcon::Grid => draw_grid_icon(painter, rect, stroke, 3),
        ToolbarIcon::MinorGrid => draw_grid_icon(painter, rect, stroke, 4),
        ToolbarIcon::Aspect => draw_center_text(painter, rect, "1:1", 11.0, color),
        ToolbarIcon::LogX => draw_log_icon(painter, rect, "X", color),
        ToolbarIcon::LogY => draw_log_icon(painter, rect, "Y", color),
        ToolbarIcon::InvertX => draw_axis_icon(painter, rect, "X", false, stroke),
        ToolbarIcon::InvertY => draw_axis_icon(painter, rect, "Y", true, stroke),
    }
}

fn draw_home_icon(painter: &egui::Painter, rect: egui::Rect, stroke: egui::Stroke) {
    let top = egui::pos2(rect.center().x, rect.top());
    let left_roof = egui::pos2(rect.left(), rect.center().y - 1.0);
    let right_roof = egui::pos2(rect.right(), rect.center().y - 1.0);
    painter.line_segment([left_roof, top], stroke);
    painter.line_segment([top, right_roof], stroke);
    let house = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 3.0, rect.center().y - 1.0),
        egui::pos2(rect.right() - 3.0, rect.bottom()),
    );
    painter.rect_stroke(house, 1.0, stroke, egui::StrokeKind::Inside);
}

fn draw_select_icon(painter: &egui::Painter, rect: egui::Rect, stroke: egui::Stroke) {
    let points = vec![
        egui::pos2(rect.left() + 2.0, rect.top() + 1.0),
        egui::pos2(rect.left() + 2.0, rect.bottom() - 2.0),
        egui::pos2(rect.left() + 7.0, rect.bottom() - 6.0),
        egui::pos2(rect.left() + 10.0, rect.bottom() - 1.0),
        egui::pos2(rect.left() + 13.0, rect.bottom() - 2.5),
        egui::pos2(rect.left() + 10.0, rect.bottom() - 7.0),
        egui::pos2(rect.right() - 2.0, rect.bottom() - 7.0),
    ];
    painter.add(egui::Shape::convex_polygon(
        points,
        Color32::TRANSPARENT,
        stroke,
    ));
}

fn draw_pan_icon(painter: &egui::Painter, rect: egui::Rect, stroke: egui::Stroke) {
    let c = rect.center();
    let arrow = 3.0;
    painter.line_segment(
        [egui::pos2(rect.left(), c.y), egui::pos2(rect.right(), c.y)],
        stroke,
    );
    painter.line_segment(
        [egui::pos2(c.x, rect.top()), egui::pos2(c.x, rect.bottom())],
        stroke,
    );
    for (tip, a, b) in [
        (
            egui::pos2(rect.left(), c.y),
            egui::pos2(rect.left() + arrow, c.y - arrow),
            egui::pos2(rect.left() + arrow, c.y + arrow),
        ),
        (
            egui::pos2(rect.right(), c.y),
            egui::pos2(rect.right() - arrow, c.y - arrow),
            egui::pos2(rect.right() - arrow, c.y + arrow),
        ),
        (
            egui::pos2(c.x, rect.top()),
            egui::pos2(c.x - arrow, rect.top() + arrow),
            egui::pos2(c.x + arrow, rect.top() + arrow),
        ),
        (
            egui::pos2(c.x, rect.bottom()),
            egui::pos2(c.x - arrow, rect.bottom() - arrow),
            egui::pos2(c.x + arrow, rect.bottom() - arrow),
        ),
    ] {
        painter.line_segment([tip, a], stroke);
        painter.line_segment([tip, b], stroke);
    }
}

fn draw_zoom_icon(painter: &egui::Painter, rect: egui::Rect, stroke: egui::Stroke) {
    let radius = rect.width().min(rect.height()) * 0.28;
    let center = egui::pos2(rect.left() + radius + 2.0, rect.top() + radius + 2.0);
    painter.circle_stroke(center, radius, stroke);
    painter.line_segment(
        [
            center + egui::vec2(radius * 0.7, radius * 0.7),
            egui::pos2(rect.right() - 2.0, rect.bottom() - 2.0),
        ],
        stroke,
    );
}

fn draw_cursor_icon(painter: &egui::Painter, rect: egui::Rect, stroke: egui::Stroke) {
    let center = rect.center();
    painter.line_segment(
        [
            egui::pos2(rect.left(), center.y),
            egui::pos2(rect.right(), center.y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x, rect.top()),
            egui::pos2(center.x, rect.bottom()),
        ],
        stroke,
    );
    painter.circle_stroke(center, rect.width().min(rect.height()) * 0.28, stroke);
}

fn draw_grid_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    stroke: egui::Stroke,
    divisions: usize,
) {
    painter.rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Inside);
    for index in 1..divisions {
        let t = index as f32 / divisions as f32;
        let x = egui::lerp(rect.left()..=rect.right(), t);
        let y = egui::lerp(rect.top()..=rect.bottom(), t);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            stroke,
        );
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            stroke,
        );
    }
}

fn draw_log_icon(painter: &egui::Painter, rect: egui::Rect, axis: &str, color: Color32) {
    painter.text(
        egui::pos2(rect.center().x, rect.top() + 3.0),
        egui::Align2::CENTER_CENTER,
        "Log",
        egui::FontId::proportional(8.5),
        color,
    );
    painter.text(
        egui::pos2(rect.center().x, rect.bottom() - 4.0),
        egui::Align2::CENTER_CENTER,
        axis,
        egui::FontId::proportional(11.0),
        color,
    );
}

fn draw_axis_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    axis: &str,
    vertical: bool,
    stroke: egui::Stroke,
) {
    let center = rect.center();
    let arrow = 3.0;
    if vertical {
        painter.line_segment(
            [
                egui::pos2(center.x, rect.top()),
                egui::pos2(center.x, rect.bottom()),
            ],
            stroke,
        );
        painter.line_segment(
            [
                egui::pos2(center.x, rect.top()),
                egui::pos2(center.x - arrow, rect.top() + arrow),
            ],
            stroke,
        );
        painter.line_segment(
            [
                egui::pos2(center.x, rect.top()),
                egui::pos2(center.x + arrow, rect.top() + arrow),
            ],
            stroke,
        );
        painter.line_segment(
            [
                egui::pos2(center.x, rect.bottom()),
                egui::pos2(center.x - arrow, rect.bottom() - arrow),
            ],
            stroke,
        );
        painter.line_segment(
            [
                egui::pos2(center.x, rect.bottom()),
                egui::pos2(center.x + arrow, rect.bottom() - arrow),
            ],
            stroke,
        );
        painter.text(
            egui::pos2(rect.right() - 2.0, center.y),
            egui::Align2::RIGHT_CENTER,
            axis,
            egui::FontId::proportional(11.0),
            stroke.color,
        );
    } else {
        painter.line_segment(
            [
                egui::pos2(rect.left(), center.y),
                egui::pos2(rect.right(), center.y),
            ],
            stroke,
        );
        painter.line_segment(
            [
                egui::pos2(rect.left(), center.y),
                egui::pos2(rect.left() + arrow, center.y - arrow),
            ],
            stroke,
        );
        painter.line_segment(
            [
                egui::pos2(rect.left(), center.y),
                egui::pos2(rect.left() + arrow, center.y + arrow),
            ],
            stroke,
        );
        painter.line_segment(
            [
                egui::pos2(rect.right(), center.y),
                egui::pos2(rect.right() - arrow, center.y - arrow),
            ],
            stroke,
        );
        painter.line_segment(
            [
                egui::pos2(rect.right(), center.y),
                egui::pos2(rect.right() - arrow, center.y + arrow),
            ],
            stroke,
        );
        painter.text(
            egui::pos2(center.x, rect.top() + 1.0),
            egui::Align2::CENTER_TOP,
            axis,
            egui::FontId::proportional(11.0),
            stroke.color,
        );
    }
}

fn draw_center_text(
    painter: &egui::Painter,
    rect: egui::Rect,
    text: &str,
    size: f32,
    color: Color32,
) {
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(size),
        color,
    );
}

fn mask_rgba_pixels(mask: &[bool], color: Color32) -> Vec<[u8; 4]> {
    let rgba = color.to_srgba_unmultiplied();
    mask.iter()
        .map(|masked| if *masked { rgba } else { [0, 0, 0, 0] })
        .collect()
}

/// Build a step-line outline for histogram `counts` and bin `edges`.
pub fn histogram_step_values(
    edges: &[f64],
    counts: &[f64],
) -> Result<(Vec<f64>, Vec<f64>), PlotDataError> {
    if edges.len() != counts.len() + 1 {
        return Err(PlotDataError::HistogramLength {
            bins: counts.len(),
            edges: edges.len(),
        });
    }
    if counts.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let mut x = Vec::with_capacity(counts.len() * 2 + 2);
    let mut y = Vec::with_capacity(counts.len() * 2 + 2);
    x.push(edges[0]);
    y.push(0.0);
    for (index, count) in counts.iter().copied().enumerate() {
        x.push(edges[index]);
        y.push(count);
        x.push(edges[index + 1]);
        y.push(count);
    }
    x.push(edges[counts.len()]);
    y.push(0.0);
    Ok((x, y))
}

/// Extract one image row as a 1D profile.
pub fn horizontal_profile_values(
    width: u32,
    height: u32,
    data: &[f32],
    row: u32,
) -> Result<Vec<f64>, PlotDataError> {
    validate_image_len(width, height, data.len())?;
    if row >= height {
        return Err(PlotDataError::ProfileRow { row, height });
    }
    let width = width as usize;
    let start = row as usize * width;
    Ok(data[start..start + width]
        .iter()
        .map(|value| *value as f64)
        .collect())
}

/// Extract one image column as a 1D profile.
pub fn vertical_profile_values(
    width: u32,
    height: u32,
    data: &[f32],
    column: u32,
) -> Result<Vec<f64>, PlotDataError> {
    validate_image_len(width, height, data.len())?;
    if column >= width {
        return Err(PlotDataError::ProfileColumn { column, width });
    }
    let width = width as usize;
    let column = column as usize;
    Ok((0..height as usize)
        .map(|row| data[row * width + column] as f64)
        .collect())
}

fn fmt_stat(value: Option<f64>) -> String {
    value.map_or_else(|| "n/a".to_owned(), |value| format!("{value:.6}"))
}

fn show_value_stats(ui: &mut egui::Ui, label: &str, stats: ValueStats) {
    ui.label(format!(
        "{label}: n={} finite={} min={} max={} mean={}",
        stats.count,
        stats.finite_count,
        fmt_stat(stats.min),
        fmt_stat(stats.max),
        fmt_stat(stats.mean)
    ));
}

#[derive(Clone, Copy, Debug, Default)]
struct Bounds1D {
    min: f64,
    max: f64,
}

impl Bounds1D {
    fn new(min: f64, max: f64) -> Option<Self> {
        (min.is_finite() && max.is_finite()).then(|| Self {
            min: min.min(max),
            max: min.max(max),
        })
    }

    fn include(&mut self, other: Self) {
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
    }

    fn as_non_degenerate(self) -> (f64, f64) {
        if self.max > self.min {
            (self.min, self.max)
        } else {
            let pad = (self.min.abs() * 0.05).max(0.5);
            (self.min - pad, self.max + pad)
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct DataBounds {
    x: Option<Bounds1D>,
    y_left: Option<Bounds1D>,
    y_right: Option<Bounds1D>,
}

impl DataBounds {
    fn include(&mut self, x: Bounds1D, y: Bounds1D, axis: YAxis) {
        include_axis(&mut self.x, x);
        match axis {
            YAxis::Left => include_axis(&mut self.y_left, y),
            YAxis::Right => include_axis(&mut self.y_right, y),
        }
    }

    fn include_bounds(&mut self, other: Self) {
        if let Some(x) = other.x {
            include_axis(&mut self.x, x);
        }
        if let Some(y) = other.y_left {
            include_axis(&mut self.y_left, y);
        }
        if let Some(y) = other.y_right {
            include_axis(&mut self.y_right, y);
        }
    }
}

fn include_axis(slot: &mut Option<Bounds1D>, bounds: Bounds1D) {
    match slot {
        Some(existing) => existing.include(bounds),
        None => *slot = Some(bounds),
    }
}

fn finite_bounds(values: &[f64]) -> Option<Bounds1D> {
    values
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(None, |bounds, value| match bounds {
            Some(mut bounds) => {
                bounds.include(Bounds1D::new(value, value).expect("finite value"));
                Some(bounds)
            }
            None => Bounds1D::new(value, value),
        })
}

fn image_bounds(image: &ImageSpec<'_>) -> Option<(Bounds1D, Bounds1D)> {
    let (width, height) = match image {
        ImageSpec {
            pixels: ImagePixelsSpec::Scalar { width, height, .. },
            ..
        }
        | ImageSpec {
            pixels: ImagePixelsSpec::Rgba { width, height, .. },
            ..
        } => (*width, *height),
    };
    let x0 = image.origin.0;
    let y0 = image.origin.1;
    let x1 = x0 + image.scale.0 * width as f64;
    let y1 = y0 + image.scale.1 * height as f64;
    Some((Bounds1D::new(x0, x1)?, Bounds1D::new(y0, y1)?))
}

fn curve_spec_bounds(spec: &CurveSpec<'_>) -> DataBounds {
    let mut bounds = DataBounds::default();
    if let (Some(x), Some(y)) = (finite_bounds(spec.x), finite_bounds(spec.y)) {
        bounds.include(x, y, spec.y_axis);
    }
    bounds
}

fn curve_spec_stats(spec: &CurveSpec<'_>) -> ItemStats {
    ItemStats::Curve(CurveStats {
        x: ValueStats::from_f64(spec.x),
        y: ValueStats::from_f64(spec.y),
        y_axis: spec.y_axis,
    })
}

fn image_spec_bounds(spec: &ImageSpec<'_>) -> DataBounds {
    let mut bounds = DataBounds::default();
    if let Some((x, y)) = image_bounds(spec) {
        bounds.include(x, y, YAxis::Left);
    }
    bounds
}

fn image_spec_stats(spec: &ImageSpec<'_>) -> ItemStats {
    let (width, height, scalar) = match &spec.pixels {
        ImagePixelsSpec::Scalar {
            width,
            height,
            data,
            ..
        } => (*width, *height, Some(ValueStats::from_f32(data))),
        ImagePixelsSpec::Rgba { width, height, .. } => (*width, *height, None),
    };
    ItemStats::Image(ImageStats {
        width,
        height,
        pixel_count: expected_image_len(width, height),
        scalar,
    })
}

fn rgba_to_color32(rgba: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3])
}

fn fallback_legend_color(kind: PlotItemKind) -> Color32 {
    match kind {
        PlotItemKind::Curve => Color32::LIGHT_BLUE,
        PlotItemKind::Histogram => Color32::LIGHT_GREEN,
        PlotItemKind::Scatter => Color32::LIGHT_BLUE,
        PlotItemKind::Image => Color32::GRAY,
        PlotItemKind::Mask => Color32::from_rgba_unmultiplied(255, 80, 80, 160),
        PlotItemKind::Triangles => Color32::LIGHT_BLUE,
        PlotItemKind::Shape => Color32::WHITE,
        PlotItemKind::Marker => Color32::YELLOW,
    }
}

fn curve_spec_legend_visual(spec: &CurveSpec<'_>, kind: PlotItemKind) -> LegendVisual {
    let color = match spec.color {
        CurveColor::Uniform(color) => color,
        CurveColor::PerVertex(colors) => colors
            .first()
            .copied()
            .unwrap_or_else(|| fallback_legend_color(kind)),
    };
    LegendVisual::new(color)
}

fn image_spec_legend_visual(spec: &ImageSpec<'_>, kind: PlotItemKind) -> LegendVisual {
    match &spec.pixels {
        ImagePixelsSpec::Scalar { colormap, .. } => LegendVisual::with_secondary(
            rgba_to_color32(colormap.lut[48]),
            rgba_to_color32(colormap.lut[208]),
        ),
        ImagePixelsSpec::Rgba { data, .. } => {
            let color = data
                .iter()
                .copied()
                .find(|rgba| rgba[3] != 0)
                .map(rgba_to_color32)
                .unwrap_or_else(|| fallback_legend_color(kind));
            LegendVisual::new(color)
        }
    }
}

fn triangle_spec_legend_visual(spec: &TriangleSpec<'_>) -> LegendVisual {
    LegendVisual::new(
        spec.colors
            .first()
            .copied()
            .unwrap_or_else(|| fallback_legend_color(PlotItemKind::Triangles)),
    )
}

fn shape_spec_legend_visual(spec: &ShapeSpec<'_>) -> LegendVisual {
    LegendVisual::new(spec.color)
}

fn marker_spec_legend_visual(spec: &MarkerSpec<'_>) -> LegendVisual {
    LegendVisual::new(spec.color)
}

fn xy_bounds(x: &[f64], y: &[f64], axis: YAxis) -> DataBounds {
    let mut bounds = DataBounds::default();
    if let (Some(x), Some(y)) = (finite_bounds(x), finite_bounds(y)) {
        bounds.include(x, y, axis);
    }
    bounds
}

fn curve_spec_from_data(curve: &CurveData) -> CurveSpec<'_> {
    CurveSpec {
        x: &curve.x,
        y: &curve.y,
        color: curve
            .colors
            .as_deref()
            .map_or(CurveColor::Uniform(curve.color), CurveColor::PerVertex),
        gap_color: curve.gap_color,
        symbol: curve.symbol,
        line_width: curve.width,
        line_style: curve.line_style.clone(),
        y_axis: curve.y_axis,
        x_error: curve.x_error.clone(),
        y_error: curve.y_error.clone(),
        fill: curve.fill,
        alpha: 1.0,
        symbol_size: curve.marker_size,
        baseline: curve.baseline.clone(),
    }
}

fn image_spec_from_data(image: &ImageData) -> ImageSpec<'_> {
    match &image.pixels {
        ImagePixels::Scalar { data, colormap } => ImageSpec {
            pixels: ImagePixelsSpec::Scalar {
                width: image.width,
                height: image.height,
                data,
                colormap: colormap.clone(),
            },
            origin: image.origin,
            scale: image.scale,
            alpha: image.alpha,
        },
        ImagePixels::Rgba { data } => ImageSpec {
            pixels: ImagePixelsSpec::Rgba {
                width: image.width,
                height: image.height,
                data,
            },
            origin: image.origin,
            scale: image.scale,
            alpha: image.alpha,
        },
    }
}

fn triangle_spec_from_data(triangles: &Triangles) -> TriangleSpec<'_> {
    TriangleSpec {
        x: &triangles.x,
        y: &triangles.y,
        triangles: &triangles.indices,
        colors: &triangles.colors,
        alpha: triangles.alpha,
    }
}

fn shape_spec_from_data(shape: &Shape) -> ShapeSpec<'_> {
    ShapeSpec {
        x: &shape.x,
        y: &shape.y,
        kind: shape.kind,
        color: shape.color,
        fill: shape.fill,
        overlay: false,
        line_style: shape.line_style.clone(),
        line_width: shape.line_width,
        gap_color: shape.gap_color,
    }
}

fn marker_spec_from_data(marker: &Marker) -> MarkerSpec<'_> {
    let (x, y, symbol, symbol_size) = match marker.kind {
        MarkerKind::Point { x, y, symbol, size } => (Some(x), Some(y), Some(symbol), size),
        MarkerKind::VLine { x } => (Some(x), None, None, 0.0),
        MarkerKind::HLine { y } => (None, Some(y), None, 0.0),
    };
    MarkerSpec {
        x,
        y,
        text: marker.text.as_deref(),
        color: marker.color,
        symbol,
        symbol_size,
        line_style: marker.line_style.clone(),
        line_width: marker.line_width,
        y_axis: marker.y_axis,
        bg_color: marker.bgcolor,
    }
}

#[derive(Clone, Debug)]
struct ItemRecord {
    handle: ItemHandle,
    kind: PlotItemKind,
    bounds: DataBounds,
    legend: Option<String>,
    stats: Option<ItemStats>,
    visual: LegendVisual,
}

#[derive(Clone, Copy, Debug)]
struct LegendVisual {
    color: Color32,
    secondary: Option<Color32>,
}

const LEGEND_ROW_HEIGHT: f32 = 24.0;
const LEGEND_SWATCH_WIDTH: f32 = 54.0;
const LEGEND_CHECK_WIDTH: f32 = 22.0;
const LEGEND_ROW_MIN_WIDTH: f32 = 1.0;

impl LegendVisual {
    fn new(color: Color32) -> Self {
        Self {
            color,
            secondary: None,
        }
    }

    fn with_secondary(color: Color32, secondary: Color32) -> Self {
        Self {
            color,
            secondary: Some(secondary),
        }
    }
}

fn legend_row_width(available_width: f32) -> f32 {
    available_width.max(LEGEND_ROW_MIN_WIDTH)
}

fn legend_row_response(
    ui: &mut egui::Ui,
    width: f32,
    kind: PlotItemKind,
    label: &str,
    active: bool,
    visual: LegendVisual,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, LEGEND_ROW_HEIGHT), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        draw_legend_row(ui, rect, &response, kind, label, active, visual);
    }
    response
}

fn draw_legend_row(
    ui: &egui::Ui,
    rect: egui::Rect,
    response: &egui::Response,
    kind: PlotItemKind,
    label: &str,
    active: bool,
    visual: LegendVisual,
) {
    let visuals = ui.visuals();
    let row_rect = rect.shrink2(egui::vec2(1.0, 0.0));
    let row_clip = row_rect.intersect(ui.clip_rect());
    let painter = ui.painter().with_clip_rect(row_clip);

    let fill = if active {
        visuals.selection.bg_fill
    } else if response.hovered() {
        visuals.widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(row_rect, 0.0, fill);

    let check_rect = egui::Rect::from_min_max(
        egui::pos2(row_rect.right() - LEGEND_CHECK_WIDTH, row_rect.top()),
        row_rect.right_bottom(),
    );
    let swatch_right = (row_rect.left() + 4.0 + LEGEND_SWATCH_WIDTH)
        .min(check_rect.left() - 4.0)
        .max(row_rect.left() + 4.0);
    let swatch_rect = egui::Rect::from_min_max(
        egui::pos2(row_rect.left() + 4.0, row_rect.top() + 4.0),
        egui::pos2(swatch_right, row_rect.bottom() - 4.0),
    );
    if swatch_rect.width() >= 8.0 {
        draw_legend_swatch(&painter, swatch_rect, kind, visual);
    }

    let text_color = if active {
        visuals.selection.stroke.color
    } else {
        visuals.text_color()
    };
    let text_left = swatch_rect.right() + 6.0;
    let text_right = check_rect.left() - 2.0;
    if text_right > text_left {
        let text_clip = egui::Rect::from_min_max(
            egui::pos2(text_left, row_rect.top()),
            egui::pos2(text_right, row_rect.bottom()),
        )
        .intersect(row_clip);
        painter.with_clip_rect(text_clip).text(
            egui::pos2(text_left, row_rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(12.0),
            text_color,
        );
    }

    draw_legend_check(
        &painter,
        check_rect,
        active,
        visuals.widgets.inactive.fg_stroke.color,
    );
    painter.line_segment(
        [
            egui::pos2(row_rect.left(), row_rect.bottom()),
            egui::pos2(row_rect.right(), row_rect.bottom()),
        ],
        egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
    );
}

fn draw_legend_swatch(
    painter: &egui::Painter,
    rect: egui::Rect,
    kind: PlotItemKind,
    visual: LegendVisual,
) {
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0, Color32::from_gray(110)),
        egui::StrokeKind::Inside,
    );
    match kind {
        PlotItemKind::Curve => {
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 4.0, rect.center().y),
                    egui::pos2(rect.right() - 4.0, rect.center().y),
                ],
                egui::Stroke::new(2.0, visual.color),
            );
        }
        PlotItemKind::Histogram => {
            let fill = visual.color.linear_multiply(0.45);
            let bar_width = rect.width() / 5.0;
            for (index, height) in [0.35, 0.65, 0.9, 0.55].iter().copied().enumerate() {
                let left = rect.left() + 4.0 + index as f32 * bar_width;
                let right = left + bar_width * 0.7;
                let top = rect.bottom() - 3.0 - rect.height() * height;
                let bar = egui::Rect::from_min_max(
                    egui::pos2(left, top),
                    egui::pos2(right, rect.bottom() - 3.0),
                );
                painter.rect_filled(bar, 0.0, fill);
                painter.rect_stroke(
                    bar,
                    0.0,
                    egui::Stroke::new(1.0, visual.color),
                    egui::StrokeKind::Inside,
                );
            }
        }
        PlotItemKind::Scatter => {
            for t in [0.25, 0.5, 0.75] {
                let center = egui::pos2(
                    egui::lerp(rect.left() + 6.0..=rect.right() - 6.0, t),
                    egui::lerp(rect.bottom() - 4.0..=rect.top() + 4.0, t),
                );
                painter.circle_filled(center, 3.0, visual.color);
            }
        }
        PlotItemKind::Image | PlotItemKind::Mask => {
            if let Some(secondary) = visual.secondary {
                let split = rect.center().x;
                painter.rect_filled(
                    egui::Rect::from_min_max(rect.left_top(), egui::pos2(split, rect.bottom())),
                    0.0,
                    visual.color,
                );
                painter.rect_filled(
                    egui::Rect::from_min_max(egui::pos2(split, rect.top()), rect.right_bottom()),
                    0.0,
                    secondary,
                );
            } else {
                painter.rect_filled(rect.shrink(2.0), 0.0, visual.color);
            }
        }
        PlotItemKind::Triangles => {
            let points = vec![
                egui::pos2(rect.left() + 7.0, rect.bottom() - 4.0),
                egui::pos2(rect.center().x, rect.top() + 4.0),
                egui::pos2(rect.right() - 7.0, rect.bottom() - 4.0),
            ];
            painter.add(egui::Shape::convex_polygon(
                points,
                visual.color.linear_multiply(0.45),
                egui::Stroke::new(1.0, visual.color),
            ));
        }
        PlotItemKind::Shape => {
            let shape = rect.shrink2(egui::vec2(12.0, 3.0));
            painter.rect_stroke(
                shape,
                0.0,
                egui::Stroke::new(1.5, visual.color),
                egui::StrokeKind::Inside,
            );
        }
        PlotItemKind::Marker => {
            let center = rect.center();
            let stroke = egui::Stroke::new(1.8, visual.color);
            painter.line_segment(
                [
                    egui::pos2(center.x - 7.0, center.y),
                    egui::pos2(center.x + 7.0, center.y),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(center.x, center.y - 7.0),
                    egui::pos2(center.x, center.y + 7.0),
                ],
                stroke,
            );
        }
    }
}

fn draw_legend_check(painter: &egui::Painter, rect: egui::Rect, active: bool, color: Color32) {
    if !active {
        return;
    }
    let stroke = egui::Stroke::new(2.0, color);
    painter.line_segment(
        [
            egui::pos2(rect.left() + 5.0, rect.center().y + 1.0),
            egui::pos2(rect.center().x - 1.0, rect.bottom() - 6.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(rect.center().x - 1.0, rect.bottom() - 6.0),
            egui::pos2(rect.right() - 4.0, rect.top() + 5.0),
        ],
        stroke,
    );
}

type LimitsSnapshot = ((f64, f64, f64, f64), Option<(f64, f64)>);

/// High-level plot widget matching silx `PlotWidget`'s role.
///
/// It owns a [`WgpuBackend`] and offers item/axis methods. Use [`Self::show`] to
/// render it in an egui UI.
pub struct PlotWidget {
    backend: WgpuBackend,
    item_records: Vec<ItemRecord>,
    data_bounds: DataBounds,
    default_colormap: Colormap,
    auto_reset_zoom: bool,
    interaction_mode: PlotInteractionMode,
    active_item: Option<ItemHandle>,
    events: Vec<PlotEvent>,
}

impl PlotWidget {
    /// Create a high-level plot widget backed by wgpu resources.
    pub fn new(render_state: &RenderState, id: PlotId) -> Self {
        Self::from_backend(WgpuBackend::new(render_state, id))
    }

    /// Build from an existing backend.
    pub fn from_backend(backend: WgpuBackend) -> Self {
        Self {
            backend,
            item_records: Vec::new(),
            data_bounds: DataBounds::default(),
            default_colormap: Colormap::viridis(0.0, 1.0),
            auto_reset_zoom: true,
            interaction_mode: PlotInteractionMode::Zoom,
            active_item: None,
            events: Vec::new(),
        }
    }

    /// Render the widget in `ui`, handling interaction and plot item selection.
    pub fn show(&mut self, ui: &mut egui::Ui) -> PlotResponse {
        let before = self.limits_snapshot();
        let response = PlotView::new().show_with_interaction(
            ui,
            self.backend.plot_mut(),
            self.interaction_mode,
        );
        self.backend
            .set_plot_bounds_in_pixels(response.transform.area);
        self.select_item_from_plot_response(&response);
        self.push_limits_changed_if(before);
        if let Some(index) = response.roi_changed {
            self.events.push(PlotEvent::RoiChanged { index });
        }
        response
    }

    /// Access the underlying plot model.
    pub fn plot(&self) -> &Plot {
        self.backend.plot()
    }

    /// Mutably access the underlying plot model.
    pub fn plot_mut(&mut self) -> &mut Plot {
        self.backend.plot_mut()
    }

    /// Access the underlying backend.
    pub fn backend(&self) -> &WgpuBackend {
        &self.backend
    }

    /// Mutably access the underlying backend.
    pub fn backend_mut(&mut self) -> &mut WgpuBackend {
        &mut self.backend
    }

    /// Toggle whether newly added data updates the displayed data limits.
    pub fn set_auto_reset_zoom(&mut self, on: bool) {
        self.auto_reset_zoom = on;
    }

    pub fn auto_reset_zoom(&self) -> bool {
        self.auto_reset_zoom
    }

    /// Set the primary pointer interaction mode used by [`Self::show`].
    pub fn set_interaction_mode(&mut self, mode: PlotInteractionMode) {
        self.interaction_mode = mode;
    }

    /// Primary pointer interaction mode used by [`Self::show`].
    pub fn interaction_mode(&self) -> PlotInteractionMode {
        self.interaction_mode
    }

    /// Queued plot events since the last drain.
    pub fn events(&self) -> &[PlotEvent] {
        &self.events
    }

    /// Take queued plot events.
    pub fn drain_events(&mut self) -> Vec<PlotEvent> {
        mem::take(&mut self.events)
    }

    fn limits_snapshot(&self) -> LimitsSnapshot {
        (self.backend.plot().limits, self.backend.plot().y2)
    }

    fn push_limits_changed_if(&mut self, before: LimitsSnapshot) {
        if before != self.limits_snapshot() {
            self.events.push(PlotEvent::LimitsChanged);
        }
    }

    fn select_item_from_plot_response(&mut self, response: &PlotResponse) {
        if !response.response.clicked_by(egui::PointerButton::Primary) {
            return;
        }
        let Some(pos) = response.response.interact_pointer_pos() else {
            return;
        };
        if !response.transform.area.contains(pos) {
            return;
        }
        if let Some(handle) = self.pick_topmost_item(pos) {
            self.set_active_item(Some(handle));
        }
    }

    fn pick_topmost_item(&self, pos: egui::Pos2) -> Option<ItemHandle> {
        self.backend
            .items_back_to_front()
            .into_iter()
            .rev()
            .find(|&handle| self.backend.pick_item(pos, handle).is_some())
    }

    fn set_limits_internal(
        &mut self,
        xmin: f64,
        xmax: f64,
        ymin: f64,
        ymax: f64,
        y2: Option<(f64, f64)>,
    ) {
        let before = self.limits_snapshot();
        self.backend.set_limits(xmin, xmax, ymin, ymax, y2);
        self.push_limits_changed_if(before);
    }

    fn record_item(
        &mut self,
        handle: ItemHandle,
        kind: PlotItemKind,
        bounds: DataBounds,
        stats: Option<ItemStats>,
        visual: LegendVisual,
    ) {
        self.item_records.push(ItemRecord {
            handle,
            kind,
            bounds,
            legend: None,
            stats,
            visual,
        });
        self.events.push(PlotEvent::ItemAdded { handle, kind });
        if self.active_item.is_none() {
            self.set_active_item(Some(handle));
        }
        self.recompute_data_bounds();
        self.apply_auto_limits();
    }

    fn update_item_record(
        &mut self,
        handle: ItemHandle,
        kind: PlotItemKind,
        bounds: DataBounds,
        stats: Option<ItemStats>,
        visual: LegendVisual,
    ) {
        if let Some(record) = self
            .item_records
            .iter_mut()
            .find(|record| record.handle == handle)
        {
            record.kind = kind;
            record.bounds = bounds;
            record.stats = stats;
            record.visual = visual;
            self.events.push(PlotEvent::ItemUpdated { handle, kind });
        } else {
            self.item_records.push(ItemRecord {
                handle,
                kind,
                bounds,
                legend: None,
                stats,
                visual,
            });
            self.events.push(PlotEvent::ItemAdded { handle, kind });
        }
        self.recompute_data_bounds();
        self.apply_auto_limits();
    }

    fn recompute_data_bounds(&mut self) {
        let mut bounds = DataBounds::default();
        for record in &self.item_records {
            bounds.include_bounds(record.bounds);
        }
        self.data_bounds = bounds;
    }

    fn remove_records_by_kinds(&mut self, predicate: impl Fn(PlotItemKind) -> bool) {
        let removed: Vec<(ItemHandle, PlotItemKind)> = self
            .item_records
            .iter()
            .filter_map(|record| predicate(record.kind).then_some((record.handle, record.kind)))
            .collect();
        for (handle, _) in &removed {
            self.backend.remove(*handle);
        }
        self.item_records.retain(|record| !predicate(record.kind));
        for (handle, kind) in removed {
            self.events.push(PlotEvent::ItemRemoved { handle, kind });
        }
        self.clear_active_if_missing();
        self.recompute_data_bounds();
        self.apply_auto_limits();
    }

    fn has_item(&self, handle: ItemHandle) -> bool {
        self.item_records
            .iter()
            .any(|record| record.handle == handle)
    }

    fn item_record(&self, handle: ItemHandle) -> Option<&ItemRecord> {
        self.item_records
            .iter()
            .find(|record| record.handle == handle)
    }

    fn item_record_mut(&mut self, handle: ItemHandle) -> Option<&mut ItemRecord> {
        self.item_records
            .iter_mut()
            .find(|record| record.handle == handle)
    }

    fn clear_active_if_missing(&mut self) {
        if self
            .active_item
            .is_some_and(|handle| !self.has_item(handle))
        {
            let previous = self.active_item.take();
            self.events.push(PlotEvent::ActiveItemChanged {
                previous,
                current: None,
            });
        }
    }

    /// Add a curve with default silx-like styling.
    pub fn add_curve(&mut self, x: &[f64], y: &[f64], color: Color32) -> ItemHandle {
        self.add_curve_spec(CurveSpec::new(x, y, color))
    }

    /// Add a curve and assign a legend label.
    pub fn add_curve_with_legend(
        &mut self,
        x: &[f64],
        y: &[f64],
        color: Color32,
        legend: impl Into<String>,
    ) -> ItemHandle {
        let handle = self.add_curve(x, y, color);
        self.set_item_legend(handle, legend);
        handle
    }

    /// Add a curve from an existing [`CurveData`] value.
    pub fn add_curve_data(&mut self, curve: &CurveData) -> ItemHandle {
        self.add_curve_spec(curve_spec_from_data(curve))
    }

    /// Add a curve from the full backend spec.
    pub fn add_curve_spec(&mut self, spec: CurveSpec<'_>) -> ItemHandle {
        self.add_curve_spec_as_kind(spec, PlotItemKind::Curve)
    }

    fn add_curve_spec_as_kind(&mut self, spec: CurveSpec<'_>, kind: PlotItemKind) -> ItemHandle {
        let bounds = curve_spec_bounds(&spec);
        let stats = Some(curve_spec_stats(&spec));
        let visual = curve_spec_legend_visual(&spec, kind);
        let handle = self.backend.add_curve(spec);
        self.record_item(handle, kind, bounds, stats, visual);
        handle
    }

    /// Replace an existing curve by handle.
    pub fn update_curve_spec(&mut self, handle: ItemHandle, spec: CurveSpec<'_>) -> bool {
        let bounds = curve_spec_bounds(&spec);
        let stats = Some(curve_spec_stats(&spec));
        let kind = self
            .item_kind(handle)
            .filter(|kind| kind.is_curve_like())
            .unwrap_or(PlotItemKind::Curve);
        let visual = curve_spec_legend_visual(&spec, kind);
        if self.backend.update_curve(handle, spec) {
            self.update_item_record(handle, kind, bounds, stats, visual);
            true
        } else {
            false
        }
    }

    /// Replace an existing curve by handle from [`CurveData`].
    pub fn update_curve_data(&mut self, handle: ItemHandle, curve: &CurveData) -> bool {
        self.update_curve_spec(handle, curve_spec_from_data(curve))
    }

    /// Add a scatter item: markers at every `(x, y)` point, with no connecting line.
    pub fn add_scatter(&mut self, x: &[f64], y: &[f64], color: Color32) -> ItemHandle {
        self.add_scatter_with_symbol(x, y, color, Symbol::Circle, 7.0)
    }

    /// Add a scatter item and assign a legend label.
    pub fn add_scatter_with_legend(
        &mut self,
        x: &[f64],
        y: &[f64],
        color: Color32,
        legend: impl Into<String>,
    ) -> ItemHandle {
        let handle = self.add_scatter(x, y, color);
        self.set_item_legend(handle, legend);
        handle
    }

    /// Add a scatter item with explicit marker symbol and size.
    pub fn add_scatter_with_symbol(
        &mut self,
        x: &[f64],
        y: &[f64],
        color: Color32,
        symbol: Symbol,
        symbol_size: f32,
    ) -> ItemHandle {
        let mut spec = CurveSpec::new(x, y, color);
        spec.line_style = LineStyle::None;
        spec.line_width = 0.0;
        spec.symbol = Some(symbol);
        spec.symbol_size = symbol_size;
        self.add_curve_spec_as_kind(spec, PlotItemKind::Scatter)
    }

    /// Add a histogram from bin edges and bin counts.
    pub fn add_histogram(
        &mut self,
        edges: &[f64],
        counts: &[f64],
        color: Color32,
    ) -> Result<ItemHandle, PlotDataError> {
        let (x, y) = histogram_step_values(edges, counts)?;
        let mut spec = CurveSpec::new(&x, &y, color);
        spec.fill = true;
        spec.baseline = Baseline::Scalar(0.0);
        Ok(self.add_curve_spec_as_kind(spec, PlotItemKind::Histogram))
    }

    /// Add a histogram and assign a legend label.
    pub fn add_histogram_with_legend(
        &mut self,
        edges: &[f64],
        counts: &[f64],
        color: Color32,
        legend: impl Into<String>,
    ) -> Result<ItemHandle, PlotDataError> {
        let handle = self.add_histogram(edges, counts, color)?;
        self.set_item_legend(handle, legend);
        Ok(handle)
    }

    /// Add a scalar image with unit scale and origin `(0, 0)`.
    pub fn add_image(
        &mut self,
        width: u32,
        height: u32,
        data: &[f32],
        colormap: Colormap,
    ) -> ItemHandle {
        self.add_image_spec(ImageSpec::scalar(width, height, data, colormap))
    }

    /// Add a scalar image, returning an error instead of panicking on length mismatch.
    pub fn try_add_image(
        &mut self,
        width: u32,
        height: u32,
        data: &[f32],
        colormap: Colormap,
    ) -> Result<ItemHandle, PlotDataError> {
        validate_image_len(width, height, data.len())?;
        Ok(self.add_image(width, height, data, colormap))
    }

    /// Add a scalar image using the widget's default colormap.
    pub fn add_image_default(&mut self, width: u32, height: u32, data: &[f32]) -> ItemHandle {
        self.add_image(width, height, data, self.default_colormap.clone())
    }

    /// Add a scalar image using the widget's default colormap, returning an
    /// error instead of panicking on length mismatch.
    pub fn try_add_image_default(
        &mut self,
        width: u32,
        height: u32,
        data: &[f32],
    ) -> Result<ItemHandle, PlotDataError> {
        self.try_add_image(width, height, data, self.default_colormap.clone())
    }

    /// Add a scalar image with explicit origin/scale/alpha.
    pub fn add_image_with_geometry(
        &mut self,
        width: u32,
        height: u32,
        data: &[f32],
        colormap: Colormap,
        geometry: ImageGeometry,
    ) -> Result<ItemHandle, PlotDataError> {
        validate_image_len(width, height, data.len())?;
        let mut spec = ImageSpec::scalar(width, height, data, colormap);
        spec.origin = geometry.origin;
        spec.scale = geometry.scale;
        spec.alpha = geometry.alpha;
        Ok(self.add_image_spec(spec))
    }

    /// Add a scalar image using the widget's default colormap and assign a legend label.
    pub fn add_image_with_legend(
        &mut self,
        width: u32,
        height: u32,
        data: &[f32],
        legend: impl Into<String>,
    ) -> ItemHandle {
        let handle = self.add_image_default(width, height, data);
        self.set_item_legend(handle, legend);
        handle
    }

    /// Add a direct RGBA image with unit scale and origin `(0, 0)`.
    pub fn add_rgba_image(&mut self, width: u32, height: u32, data: &[[u8; 4]]) -> ItemHandle {
        self.add_image_spec(ImageSpec::rgba(width, height, data))
    }

    /// Add a direct RGBA image, returning an error instead of panicking on length mismatch.
    pub fn try_add_rgba_image(
        &mut self,
        width: u32,
        height: u32,
        data: &[[u8; 4]],
    ) -> Result<ItemHandle, PlotDataError> {
        validate_image_len(width, height, data.len())?;
        Ok(self.add_rgba_image(width, height, data))
    }

    /// Add a direct RGBA image with explicit origin/scale/alpha.
    pub fn add_rgba_image_with_geometry(
        &mut self,
        width: u32,
        height: u32,
        data: &[[u8; 4]],
        geometry: ImageGeometry,
    ) -> Result<ItemHandle, PlotDataError> {
        validate_image_len(width, height, data.len())?;
        let mut spec = ImageSpec::rgba(width, height, data);
        spec.origin = geometry.origin;
        spec.scale = geometry.scale;
        spec.alpha = geometry.alpha;
        Ok(self.add_image_spec(spec))
    }

    /// Add an image from an existing [`ImageData`] value.
    pub fn add_image_data(&mut self, image: &ImageData) -> ItemHandle {
        self.add_image_spec(image_spec_from_data(image))
    }

    /// Add an image from the full backend spec.
    pub fn add_image_spec(&mut self, spec: ImageSpec<'_>) -> ItemHandle {
        self.add_image_spec_as_kind(spec, PlotItemKind::Image)
    }

    fn add_image_spec_as_kind(&mut self, spec: ImageSpec<'_>, kind: PlotItemKind) -> ItemHandle {
        let bounds = image_spec_bounds(&spec);
        let stats = Some(image_spec_stats(&spec));
        let visual = image_spec_legend_visual(&spec, kind);
        let handle = self.backend.add_image(spec);
        self.record_item(handle, kind, bounds, stats, visual);
        handle
    }

    /// Replace an existing image by handle.
    pub fn update_image_spec(&mut self, handle: ItemHandle, spec: ImageSpec<'_>) -> bool {
        let bounds = image_spec_bounds(&spec);
        let stats = Some(image_spec_stats(&spec));
        let kind = self
            .item_kind(handle)
            .filter(|kind| kind.is_image_like())
            .unwrap_or(PlotItemKind::Image);
        let visual = image_spec_legend_visual(&spec, kind);
        if self.backend.update_image(handle, spec) {
            self.update_item_record(handle, kind, bounds, stats, visual);
            true
        } else {
            false
        }
    }

    /// Replace an existing scalar image, returning an error instead of panicking
    /// on length mismatch.
    pub fn try_update_image(
        &mut self,
        handle: ItemHandle,
        width: u32,
        height: u32,
        data: &[f32],
        colormap: Colormap,
    ) -> Result<bool, PlotDataError> {
        validate_image_len(width, height, data.len())?;
        Ok(self.update_image_spec(handle, ImageSpec::scalar(width, height, data, colormap)))
    }

    /// Replace an existing direct RGBA image, returning an error instead of
    /// panicking on length mismatch.
    pub fn try_update_rgba_image(
        &mut self,
        handle: ItemHandle,
        width: u32,
        height: u32,
        data: &[[u8; 4]],
    ) -> Result<bool, PlotDataError> {
        validate_image_len(width, height, data.len())?;
        Ok(self.update_image_spec(handle, ImageSpec::rgba(width, height, data)))
    }

    /// Replace an existing image by handle from [`ImageData`].
    pub fn update_image_data(&mut self, handle: ItemHandle, image: &ImageData) -> bool {
        self.update_image_spec(handle, image_spec_from_data(image))
    }

    /// Add a boolean mask as a transparent RGBA overlay.
    pub fn add_mask(
        &mut self,
        width: u32,
        height: u32,
        mask: &[bool],
        color: Color32,
    ) -> Result<ItemHandle, PlotDataError> {
        self.add_mask_with_geometry(width, height, mask, color, ImageGeometry::default())
    }

    /// Add a boolean mask as a transparent RGBA overlay with explicit geometry.
    pub fn add_mask_with_geometry(
        &mut self,
        width: u32,
        height: u32,
        mask: &[bool],
        color: Color32,
        geometry: ImageGeometry,
    ) -> Result<ItemHandle, PlotDataError> {
        validate_image_len(width, height, mask.len())?;
        let rgba = mask_rgba_pixels(mask, color);
        let mut spec = ImageSpec::rgba(width, height, &rgba);
        spec.origin = geometry.origin;
        spec.scale = geometry.scale;
        spec.alpha = geometry.alpha;
        Ok(self.add_image_spec_as_kind(spec, PlotItemKind::Mask))
    }

    /// Add a boolean mask overlay and assign a legend label.
    pub fn add_mask_with_legend(
        &mut self,
        width: u32,
        height: u32,
        mask: &[bool],
        color: Color32,
        legend: impl Into<String>,
    ) -> Result<ItemHandle, PlotDataError> {
        let handle = self.add_mask(width, height, mask, color)?;
        self.set_item_legend(handle, legend);
        Ok(handle)
    }

    /// Add a horizontal image profile as a curve.
    pub fn add_horizontal_profile_curve(
        &mut self,
        width: u32,
        height: u32,
        data: &[f32],
        row: u32,
        color: Color32,
    ) -> Result<ItemHandle, PlotDataError> {
        let y = horizontal_profile_values(width, height, data, row)?;
        let x: Vec<f64> = (0..width).map(|col| col as f64).collect();
        Ok(self.add_curve(&x, &y, color))
    }

    /// Add a vertical image profile as a curve.
    pub fn add_vertical_profile_curve(
        &mut self,
        width: u32,
        height: u32,
        data: &[f32],
        column: u32,
        color: Color32,
    ) -> Result<ItemHandle, PlotDataError> {
        let y = vertical_profile_values(width, height, data, column)?;
        let x: Vec<f64> = (0..height).map(|row| row as f64).collect();
        Ok(self.add_curve(&x, &y, color))
    }

    /// Add a triangle mesh.
    pub fn add_triangles(&mut self, spec: TriangleSpec<'_>) -> ItemHandle {
        let bounds = xy_bounds(spec.x, spec.y, YAxis::Left);
        let visual = triangle_spec_legend_visual(&spec);
        let handle = self.backend.add_triangles(spec);
        self.record_item(handle, PlotItemKind::Triangles, bounds, None, visual);
        handle
    }

    /// Add a triangle mesh from an existing [`Triangles`] value.
    pub fn add_triangles_data(&mut self, triangles: &Triangles) -> ItemHandle {
        self.add_triangles(triangle_spec_from_data(triangles))
    }

    /// Add a shape overlay.
    pub fn add_shape(&mut self, spec: ShapeSpec<'_>) -> ItemHandle {
        let bounds = xy_bounds(spec.x, spec.y, YAxis::Left);
        let visual = shape_spec_legend_visual(&spec);
        let handle = self.backend.add_shape(spec);
        self.record_item(handle, PlotItemKind::Shape, bounds, None, visual);
        handle
    }

    /// Add a shape overlay from an existing [`Shape`] value.
    pub fn add_shape_data(&mut self, shape: &Shape) -> ItemHandle {
        self.add_shape(shape_spec_from_data(shape))
    }

    /// Add a rectangle shape.
    pub fn add_rectangle(
        &mut self,
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        color: Color32,
        fill: bool,
    ) -> ItemHandle {
        let x = [x0, x1];
        let y = [y0, y1];
        self.add_shape(ShapeSpec {
            x: &x,
            y: &y,
            kind: ShapeKind::Rectangle,
            color,
            fill,
            overlay: false,
            line_style: LineStyle::Solid,
            line_width: 1.0,
            gap_color: None,
        })
    }

    /// Add a point or line marker.
    pub fn add_marker(&mut self, spec: MarkerSpec<'_>) -> ItemHandle {
        let visual = marker_spec_legend_visual(&spec);
        let handle = self.backend.add_marker(spec);
        self.record_item(
            handle,
            PlotItemKind::Marker,
            DataBounds::default(),
            None,
            visual,
        );
        handle
    }

    /// Add a marker from an existing [`Marker`] value.
    pub fn add_marker_data(&mut self, marker: &Marker) -> ItemHandle {
        self.add_marker(marker_spec_from_data(marker))
    }

    /// Add a point marker.
    pub fn add_point_marker(
        &mut self,
        x: f64,
        y: f64,
        color: Color32,
        symbol: MarkerSymbol,
    ) -> ItemHandle {
        self.add_marker(MarkerSpec {
            x: Some(x),
            y: Some(y),
            text: None,
            color,
            symbol: Some(symbol),
            symbol_size: 8.0,
            line_style: LineStyle::Solid,
            line_width: 1.0,
            y_axis: YAxis::Left,
            bg_color: None,
        })
    }

    /// Add a vertical marker line.
    pub fn add_x_marker(&mut self, x: f64, color: Color32) -> ItemHandle {
        self.add_marker(MarkerSpec {
            x: Some(x),
            y: None,
            text: None,
            color,
            symbol: None,
            symbol_size: 0.0,
            line_style: LineStyle::Solid,
            line_width: 1.0,
            y_axis: YAxis::Left,
            bg_color: None,
        })
    }

    /// Add a horizontal marker line.
    pub fn add_y_marker(&mut self, y: f64, color: Color32, axis: YAxis) -> ItemHandle {
        self.add_marker(MarkerSpec {
            x: None,
            y: Some(y),
            text: None,
            color,
            symbol: None,
            symbol_size: 0.0,
            line_style: LineStyle::Solid,
            line_width: 1.0,
            y_axis: axis,
            bg_color: None,
        })
    }

    /// Remove an item by handle.
    pub fn remove(&mut self, handle: ItemHandle) -> bool {
        let kind = self.item_record(handle).map(|record| record.kind);
        let removed = self.backend.remove(handle);
        if removed {
            self.item_records.retain(|record| record.handle != handle);
            if let Some(kind) = kind {
                self.events.push(PlotEvent::ItemRemoved { handle, kind });
            }
            self.clear_active_if_missing();
            self.recompute_data_bounds();
            self.apply_auto_limits();
        }
        removed
    }

    /// Remove all items.
    pub fn clear(&mut self) {
        let removed: Vec<(ItemHandle, PlotItemKind)> = self
            .item_records
            .iter()
            .map(|record| (record.handle, record.kind))
            .collect();
        self.backend.clear_items();
        self.item_records.clear();
        for (handle, kind) in removed {
            self.events.push(PlotEvent::ItemRemoved { handle, kind });
        }
        self.clear_active_if_missing();
        self.recompute_data_bounds();
    }

    /// Remove all curve-like items.
    pub fn clear_curves(&mut self) {
        self.remove_records_by_kinds(PlotItemKind::is_curve_like);
    }

    /// Remove all image-like items.
    pub fn clear_images(&mut self) {
        self.remove_records_by_kinds(PlotItemKind::is_image_like);
    }

    /// Remove all shape and triangle overlay items.
    pub fn clear_items(&mut self) {
        self.remove_records_by_kinds(|kind| {
            matches!(kind, PlotItemKind::Shape | PlotItemKind::Triangles)
        });
    }

    /// Remove all marker items.
    pub fn clear_markers(&mut self) {
        self.remove_records_by_kinds(|kind| kind == PlotItemKind::Marker);
    }

    /// Remove all histogram items.
    pub fn clear_histograms(&mut self) {
        self.remove_records_by_kinds(|kind| kind == PlotItemKind::Histogram);
    }

    /// Remove all scatter items.
    pub fn clear_scatters(&mut self) {
        self.remove_records_by_kinds(|kind| kind == PlotItemKind::Scatter);
    }

    /// Remove all mask overlay items.
    pub fn clear_masks(&mut self) {
        self.remove_records_by_kinds(|kind| kind == PlotItemKind::Mask);
    }

    fn remove_if_kind(
        &mut self,
        handle: ItemHandle,
        predicate: impl Fn(PlotItemKind) -> bool,
    ) -> bool {
        if self.item_kind(handle).is_some_and(predicate) {
            self.remove(handle)
        } else {
            false
        }
    }

    /// Remove a curve-like item by handle.
    pub fn remove_curve(&mut self, handle: ItemHandle) -> bool {
        self.remove_if_kind(handle, PlotItemKind::is_curve_like)
    }

    /// Remove an image-like item by handle.
    pub fn remove_image(&mut self, handle: ItemHandle) -> bool {
        self.remove_if_kind(handle, PlotItemKind::is_image_like)
    }

    /// Remove a histogram item by handle.
    pub fn remove_histogram(&mut self, handle: ItemHandle) -> bool {
        self.remove_if_kind(handle, |kind| kind == PlotItemKind::Histogram)
    }

    /// Remove a scatter item by handle.
    pub fn remove_scatter(&mut self, handle: ItemHandle) -> bool {
        self.remove_if_kind(handle, |kind| kind == PlotItemKind::Scatter)
    }

    /// Remove a mask item by handle.
    pub fn remove_mask(&mut self, handle: ItemHandle) -> bool {
        self.remove_if_kind(handle, |kind| kind == PlotItemKind::Mask)
    }

    /// Remove a marker item by handle.
    pub fn remove_marker(&mut self, handle: ItemHandle) -> bool {
        self.remove_if_kind(handle, |kind| kind == PlotItemKind::Marker)
    }

    /// Remove a shape or triangle overlay item by handle.
    pub fn remove_overlay_item(&mut self, handle: ItemHandle) -> bool {
        self.remove_if_kind(handle, |kind| {
            matches!(kind, PlotItemKind::Shape | PlotItemKind::Triangles)
        })
    }

    /// Return every backend item handle in draw order.
    pub fn get_items(&self) -> Vec<ItemHandle> {
        self.backend.items_back_to_front()
    }

    /// Return curve-like handles in draw order.
    pub fn get_all_curves(&self) -> Vec<ItemHandle> {
        self.handles_by_predicate(PlotItemKind::is_curve_like)
    }

    /// Return image-like handles in draw order.
    pub fn get_all_images(&self) -> Vec<ItemHandle> {
        self.handles_by_predicate(PlotItemKind::is_image_like)
    }

    /// Return marker handles in draw order.
    pub fn get_all_markers(&self) -> Vec<ItemHandle> {
        self.handles_by_kind(PlotItemKind::Marker)
    }

    /// Return histogram handles in draw order.
    pub fn get_all_histograms(&self) -> Vec<ItemHandle> {
        self.handles_by_kind(PlotItemKind::Histogram)
    }

    /// Return scatter handles in draw order.
    pub fn get_all_scatters(&self) -> Vec<ItemHandle> {
        self.handles_by_kind(PlotItemKind::Scatter)
    }

    /// Return mask handles in draw order.
    pub fn get_all_masks(&self) -> Vec<ItemHandle> {
        self.handles_by_kind(PlotItemKind::Mask)
    }

    fn handles_by_kind(&self, kind: PlotItemKind) -> Vec<ItemHandle> {
        self.handles_by_predicate(|record_kind| record_kind == kind)
    }

    fn handles_by_predicate(&self, predicate: impl Fn(PlotItemKind) -> bool) -> Vec<ItemHandle> {
        self.item_records
            .iter()
            .filter_map(|record| predicate(record.kind).then_some(record.handle))
            .collect()
    }

    fn handle_by_legend_and_kind(
        &self,
        legend: &str,
        predicate: impl Fn(PlotItemKind) -> bool,
    ) -> Option<ItemHandle> {
        self.item_records.iter().find_map(|record| {
            (record.legend.as_deref() == Some(legend) && predicate(record.kind))
                .then_some(record.handle)
        })
    }

    /// Return the first item handle with this legend label.
    pub fn item_by_legend(&self, legend: &str) -> Option<ItemHandle> {
        self.handle_by_legend_and_kind(legend, |_| true)
    }

    /// Return the first curve-like item handle with this legend label.
    pub fn curve_by_legend(&self, legend: &str) -> Option<ItemHandle> {
        self.handle_by_legend_and_kind(legend, PlotItemKind::is_curve_like)
    }

    /// Return the first image-like item handle with this legend label.
    pub fn image_by_legend(&self, legend: &str) -> Option<ItemHandle> {
        self.handle_by_legend_and_kind(legend, PlotItemKind::is_image_like)
    }

    /// Return the first histogram item handle with this legend label.
    pub fn histogram_by_legend(&self, legend: &str) -> Option<ItemHandle> {
        self.handle_by_legend_and_kind(legend, |kind| kind == PlotItemKind::Histogram)
    }

    /// Return the first scatter item handle with this legend label.
    pub fn scatter_by_legend(&self, legend: &str) -> Option<ItemHandle> {
        self.handle_by_legend_and_kind(legend, |kind| kind == PlotItemKind::Scatter)
    }

    /// Return the first mask item handle with this legend label.
    pub fn mask_by_legend(&self, legend: &str) -> Option<ItemHandle> {
        self.handle_by_legend_and_kind(legend, |kind| kind == PlotItemKind::Mask)
    }

    /// Return the high-level family of an item.
    pub fn item_kind(&self, handle: ItemHandle) -> Option<PlotItemKind> {
        self.item_record(handle).map(|record| record.kind)
    }

    /// Attach or replace the legend label for an item.
    pub fn set_item_legend(&mut self, handle: ItemHandle, legend: impl Into<String>) -> bool {
        let Some(record) = self.item_record_mut(handle) else {
            return false;
        };
        record.legend = Some(legend.into());
        let kind = record.kind;
        self.events.push(PlotEvent::ItemUpdated { handle, kind });
        true
    }

    /// Remove the legend label from an item.
    pub fn clear_item_legend(&mut self, handle: ItemHandle) -> bool {
        let Some(record) = self.item_record_mut(handle) else {
            return false;
        };
        record.legend = None;
        let kind = record.kind;
        self.events.push(PlotEvent::ItemUpdated { handle, kind });
        true
    }

    /// Legend label assigned to an item.
    pub fn item_legend(&self, handle: ItemHandle) -> Option<&str> {
        self.item_record(handle)
            .and_then(|record| record.legend.as_deref())
    }

    fn legend_label(&self, record: &ItemRecord) -> String {
        record
            .legend
            .clone()
            .unwrap_or_else(|| format!("{} #{}", record.kind.as_str(), record.handle))
    }

    /// Currently active item.
    pub fn active_item(&self) -> Option<ItemHandle> {
        self.active_item
    }

    /// Set the active item, emitting [`PlotEvent::ActiveItemChanged`] when it changes.
    pub fn set_active_item(&mut self, item: Option<ItemHandle>) -> bool {
        if item.is_some_and(|handle| !self.has_item(handle)) {
            return false;
        }
        if self.active_item == item {
            return true;
        }
        let previous = self.active_item;
        self.active_item = item;
        self.events.push(PlotEvent::ActiveItemChanged {
            previous,
            current: item,
        });
        true
    }

    /// Currently active curve-like item.
    pub fn active_curve(&self) -> Option<ItemHandle> {
        self.active_item.filter(|handle| {
            self.item_kind(*handle)
                .is_some_and(PlotItemKind::is_curve_like)
        })
    }

    /// Set the active curve-like item.
    pub fn set_active_curve(&mut self, item: Option<ItemHandle>) -> bool {
        if item.is_some_and(|handle| {
            !self
                .item_kind(handle)
                .is_some_and(PlotItemKind::is_curve_like)
        }) {
            return false;
        }
        self.set_active_item(item)
    }

    /// Currently active image-like item.
    pub fn active_image(&self) -> Option<ItemHandle> {
        self.active_item.filter(|handle| {
            self.item_kind(*handle)
                .is_some_and(PlotItemKind::is_image_like)
        })
    }

    /// Set the active image-like item.
    pub fn set_active_image(&mut self, item: Option<ItemHandle>) -> bool {
        if item.is_some_and(|handle| {
            !self
                .item_kind(handle)
                .is_some_and(PlotItemKind::is_image_like)
        }) {
            return false;
        }
        self.set_active_item(item)
    }

    /// Return retained statistics for an item.
    pub fn item_stats(&self, handle: ItemHandle) -> Option<&ItemStats> {
        self.item_record(handle)
            .and_then(|record| record.stats.as_ref())
    }

    /// Return retained statistics for a curve-like item.
    pub fn curve_stats(&self, handle: ItemHandle) -> Option<&CurveStats> {
        match self.item_stats(handle)? {
            ItemStats::Curve(stats) => Some(stats),
            ItemStats::Image(_) => None,
        }
    }

    /// Return retained statistics for an image-like item.
    pub fn image_stats(&self, handle: ItemHandle) -> Option<&ImageStats> {
        match self.item_stats(handle)? {
            ItemStats::Curve(_) => None,
            ItemStats::Image(stats) => Some(stats),
        }
    }

    /// Draw a selectable legend list. Clicking an entry makes it active.
    pub fn show_legend(&mut self, ui: &mut egui::Ui) -> LegendResponse {
        let rows: Vec<(ItemHandle, PlotItemKind, String, bool, LegendVisual)> = self
            .item_records
            .iter()
            .map(|record| {
                (
                    record.handle,
                    record.kind,
                    self.legend_label(record),
                    self.active_item == Some(record.handle),
                    record.visual,
                )
            })
            .collect();

        let mut out = LegendResponse::default();
        if rows.is_empty() {
            ui.label("no items");
            return out;
        }

        egui::Frame::new()
            .inner_margin(1)
            .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                let width = legend_row_width(ui.available_width());
                for (handle, kind, label, active, visual) in rows {
                    let response = legend_row_response(ui, width, kind, &label, active, visual);
                    if response.clicked() {
                        out.selected = Some(handle);
                        if self.set_active_item(Some(handle)) {
                            out.activated = Some(handle);
                        }
                    }
                }
            });
        out
    }

    /// Draw retained statistics for an item. Returns `false` if the handle is unknown.
    pub fn show_stats(&self, ui: &mut egui::Ui, handle: ItemHandle) -> bool {
        let Some(record) = self.item_record(handle) else {
            return false;
        };
        ui.label(self.legend_label(record));
        match record.stats {
            Some(ItemStats::Curve(stats)) => {
                show_value_stats(ui, "x", stats.x);
                show_value_stats(ui, "y", stats.y);
                ui.label(format!("axis: {:?}", stats.y_axis));
            }
            Some(ItemStats::Image(stats)) => {
                ui.label(format!("size: {} x {}", stats.width, stats.height));
                ui.label(format!("pixels: {}", stats.pixel_count));
                if let Some(scalar) = stats.scalar {
                    show_value_stats(ui, "value", scalar);
                }
            }
            None => {
                ui.label("no retained statistics");
            }
        }
        true
    }

    /// Draw retained statistics for the active item.
    pub fn show_active_stats(&self, ui: &mut egui::Ui) -> bool {
        self.active_item
            .is_some_and(|handle| self.show_stats(ui, handle))
    }

    /// Draw an egui-native plot toolbar.
    pub fn show_toolbar(&mut self, ui: &mut egui::Ui) -> ToolbarResponse {
        let mut out = ToolbarResponse::default();
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                self.show_toolbar_controls(ui, &mut out);
            });
        });
        out
    }

    /// Draw the standard toolbar and append caller-provided controls in the
    /// same toolbar row.
    ///
    /// The closure receives this plot after the built-in controls have been
    /// drawn, so custom actions can mutate plot state while still sharing the
    /// standard toolbar layout.
    pub fn show_toolbar_with<R>(
        &mut self,
        ui: &mut egui::Ui,
        add_contents: impl FnOnce(&mut egui::Ui, &mut Self) -> R,
    ) -> (ToolbarResponse, R) {
        let mut out = ToolbarResponse::default();
        let mut extra = None;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                self.show_toolbar_controls(ui, &mut out);
                ui.separator();
                extra = Some(add_contents(ui, self));
            });
        });
        (
            out,
            extra.expect("egui horizontal layout closure should run exactly once"),
        )
    }

    /// Draw toolbar then plot in the remaining UI.
    pub fn show_with_toolbar(&mut self, ui: &mut egui::Ui) -> PlotWithToolbarResponse {
        let toolbar = self.show_toolbar(ui);
        let plot = self.show(ui);
        PlotWithToolbarResponse { toolbar, plot }
    }

    /// Draw the standard toolbar plus caller controls, then draw the plot.
    pub fn show_with_toolbar_with<R>(
        &mut self,
        ui: &mut egui::Ui,
        add_toolbar_contents: impl FnOnce(&mut egui::Ui, &mut Self) -> R,
    ) -> (PlotWithToolbarResponse, R) {
        let (toolbar, extra) = self.show_toolbar_with(ui, add_toolbar_contents);
        let plot = self.show(ui);
        (PlotWithToolbarResponse { toolbar, plot }, extra)
    }

    fn show_toolbar_controls(&mut self, ui: &mut egui::Ui, out: &mut ToolbarResponse) {
        if toolbar_icon_button(ui, ToolbarIcon::Home, false, "Reset zoom").clicked() {
            self.reset_zoom();
            out.reset_zoom = true;
        }

        ui.separator();

        let mode = self.interaction_mode();
        if toolbar_icon_button(
            ui,
            ToolbarIcon::Select,
            mode == PlotInteractionMode::Select,
            "Select items and edit handles",
        )
        .clicked()
        {
            self.set_interaction_mode(PlotInteractionMode::Select);
            out.interaction_mode_changed = true;
        }
        if toolbar_icon_button(
            ui,
            ToolbarIcon::Zoom,
            mode == PlotInteractionMode::Zoom,
            "Box zoom",
        )
        .clicked()
        {
            self.set_interaction_mode(PlotInteractionMode::Zoom);
            out.interaction_mode_changed = true;
        }
        if toolbar_icon_button(
            ui,
            ToolbarIcon::Pan,
            mode == PlotInteractionMode::Pan,
            "Pan",
        )
        .clicked()
        {
            self.set_interaction_mode(PlotInteractionMode::Pan);
            out.interaction_mode_changed = true;
        }

        ui.separator();

        let mut x_inv = self.is_x_inverted();
        if toolbar_icon_button(ui, ToolbarIcon::InvertX, x_inv, "Invert X axis").clicked() {
            x_inv = !x_inv;
            self.set_x_inverted(x_inv);
            out.x_inverted_changed = true;
        }

        let mut y_inv = self.is_y_inverted();
        if toolbar_icon_button(ui, ToolbarIcon::InvertY, y_inv, "Invert Y axis").clicked() {
            y_inv = !y_inv;
            self.set_y_inverted(y_inv);
            out.y_inverted_changed = true;
        }

        ui.separator();

        let mut x_log = self.is_x_logarithmic();
        if toolbar_icon_button(ui, ToolbarIcon::LogX, x_log, "Toggle X log scale").clicked() {
            x_log = !x_log;
            self.set_x_log(x_log);
            out.x_log_changed = true;
        }

        let mut y_log = self.is_y_logarithmic();
        if toolbar_icon_button(ui, ToolbarIcon::LogY, y_log, "Toggle Y log scale").clicked() {
            y_log = !y_log;
            self.set_y_log(y_log);
            out.y_log_changed = true;
        }

        ui.separator();

        let mut grid = self.graph_grid();
        if toolbar_icon_button(ui, ToolbarIcon::Grid, grid, "Toggle grid").clicked() {
            grid = !grid;
            self.set_graph_grid(grid);
            out.grid_changed = true;
        }

        let mut minor = self.graph_minor_grid();
        let minor_response = ui
            .add_enabled_ui(grid, |ui| {
                toolbar_icon_button(ui, ToolbarIcon::MinorGrid, minor, "Toggle minor grid")
            })
            .inner;
        if minor_response.clicked() {
            minor = !minor;
            self.set_graph_minor_grid(minor);
            out.minor_grid_changed = true;
        }

        ui.separator();

        let mut aspect = self.is_keep_data_aspect_ratio();
        if toolbar_icon_button(ui, ToolbarIcon::Aspect, aspect, "Keep data aspect ratio").clicked()
        {
            aspect = !aspect;
            self.set_keep_data_aspect_ratio(aspect);
            out.aspect_changed = true;
        }

        let mut cursor = self.graph_cursor();
        if toolbar_icon_button(ui, ToolbarIcon::Cursor, cursor, "Show cursor coordinates").clicked()
        {
            cursor = !cursor;
            self.set_graph_cursor(cursor);
            out.cursor_changed = true;
        }
    }

    /// Set graph limits.
    pub fn set_limits(
        &mut self,
        xmin: f64,
        xmax: f64,
        ymin: f64,
        ymax: f64,
        y2: Option<(f64, f64)>,
    ) {
        self.set_limits_internal(xmin, xmax, ymin, ymax, y2);
    }

    /// Reset the displayed limits from accumulated data bounds.
    pub fn reset_zoom(&mut self) {
        self.reset_zoom_to_data();
    }

    pub fn x_limits(&self) -> (f64, f64) {
        self.backend.x_limits()
    }

    pub fn get_graph_x_limits(&self) -> (f64, f64) {
        self.x_limits()
    }

    pub fn set_graph_x_limits(&mut self, xmin: f64, xmax: f64) {
        let (_, _, ymin, ymax) = self.backend.plot().limits;
        self.set_limits_internal(xmin, xmax, ymin, ymax, self.backend.plot().y2);
    }

    pub fn y_limits(&self, axis: YAxis) -> Option<(f64, f64)> {
        self.backend.y_limits(axis)
    }

    pub fn get_graph_y_limits(&self, axis: YAxis) -> Option<(f64, f64)> {
        self.y_limits(axis)
    }

    pub fn set_graph_y_limits(&mut self, ymin: f64, ymax: f64, axis: YAxis) {
        match axis {
            YAxis::Left => {
                let (xmin, xmax, _, _) = self.backend.plot().limits;
                self.set_limits_internal(xmin, xmax, ymin, ymax, self.backend.plot().y2);
            }
            YAxis::Right => {
                let before = self.limits_snapshot();
                self.backend.plot_mut().y2 = Some((ymin, ymax));
                self.push_limits_changed_if(before);
            }
        }
    }

    pub fn set_x_log(&mut self, on: bool) {
        self.backend.set_x_log(on);
    }

    pub fn is_x_logarithmic(&self) -> bool {
        self.backend.plot().x_scale == Scale::Log10
    }

    pub fn set_y_log(&mut self, on: bool) {
        self.backend.set_y_log(on);
    }

    pub fn is_y_logarithmic(&self) -> bool {
        self.backend.plot().y_scale == Scale::Log10
    }

    pub fn set_x_inverted(&mut self, on: bool) {
        self.backend.set_x_inverted(on);
    }

    pub fn is_x_inverted(&self) -> bool {
        self.backend.plot().x_inverted
    }

    pub fn set_y_inverted(&mut self, on: bool) {
        self.backend.set_y_inverted(on);
    }

    pub fn is_y_inverted(&self) -> bool {
        self.backend.plot().y_inverted
    }

    pub fn set_keep_data_aspect_ratio(&mut self, on: bool) {
        self.backend.set_keep_data_aspect_ratio(on);
    }

    pub fn is_keep_data_aspect_ratio(&self) -> bool {
        self.backend.plot().keep_aspect
    }

    pub fn set_axes_margins(&mut self, margins: Margins) {
        self.backend.set_axes_margins(margins);
    }

    pub fn axes_margins(&self) -> Margins {
        self.backend.plot().margins
    }

    pub fn set_graph_title(&mut self, title: impl Into<String>) {
        let title = title.into();
        self.backend.set_title(Some(&title));
    }

    pub fn graph_title(&self) -> Option<&str> {
        self.backend.plot().title.as_deref()
    }

    pub fn clear_graph_title(&mut self) {
        self.backend.set_title(None);
    }

    pub fn set_graph_x_label(&mut self, label: impl Into<String>) {
        let label = label.into();
        self.backend.set_x_label(Some(&label));
    }

    pub fn graph_x_label(&self) -> Option<&str> {
        self.backend.plot().x_label.as_deref()
    }

    pub fn clear_graph_x_label(&mut self) {
        self.backend.set_x_label(None);
    }

    pub fn set_graph_y_label(&mut self, label: impl Into<String>, axis: YAxis) {
        let label = label.into();
        self.backend.set_y_label(Some(&label), axis);
    }

    pub fn graph_y_label(&self, axis: YAxis) -> Option<&str> {
        match axis {
            YAxis::Left => self.backend.plot().y_label.as_deref(),
            YAxis::Right => self.backend.plot().y2_label.as_deref(),
        }
    }

    pub fn clear_graph_y_label(&mut self, axis: YAxis) {
        self.backend.set_y_label(None, axis);
    }

    pub fn set_foreground_colors(&mut self, foreground: Color32, grid: Color32) {
        self.backend.set_foreground_colors(foreground, grid);
    }

    pub fn set_background_colors(&mut self, background: Color32, data_background: Color32) {
        self.backend
            .set_background_colors(background, data_background);
    }

    pub fn data_background_color(&self) -> Color32 {
        self.backend.plot().data_background
    }

    pub fn foreground_color(&self) -> Option<Color32> {
        self.backend.plot().foreground
    }

    pub fn grid_color(&self) -> Option<Color32> {
        self.backend.plot().grid_color
    }

    pub fn set_graph_grid(&mut self, on: bool) {
        self.backend.plot_mut().grid = if on {
            GraphGrid::Major
        } else {
            GraphGrid::None
        };
    }

    pub fn graph_grid(&self) -> bool {
        self.backend.plot().grid.major()
    }

    pub fn set_graph_grid_mode(&mut self, mode: GraphGrid) {
        self.backend.plot_mut().grid = mode;
    }

    pub fn graph_grid_mode(&self) -> GraphGrid {
        self.backend.plot().grid
    }

    pub fn set_graph_minor_grid(&mut self, on: bool) {
        self.backend.plot_mut().grid = if on {
            GraphGrid::MajorAndMinor
        } else if self.graph_grid() {
            GraphGrid::Major
        } else {
            GraphGrid::None
        };
    }

    pub fn graph_minor_grid(&self) -> bool {
        self.backend.plot().grid.minor()
    }

    pub fn default_colormap(&self) -> &Colormap {
        &self.default_colormap
    }

    pub fn set_default_colormap(&mut self, colormap: Colormap) {
        self.default_colormap = colormap;
    }

    pub fn colorbar_colormap(&self) -> Option<&Colormap> {
        self.backend.plot().colormap.as_ref()
    }

    pub fn set_graph_cursor(&mut self, on: bool) {
        self.backend.plot_mut().crosshair = on;
    }

    pub fn graph_cursor(&self) -> bool {
        self.backend.plot().crosshair
    }

    pub fn data_to_pixel(&self, x: f64, y: f64, axis: YAxis) -> Option<egui::Pos2> {
        self.backend.data_to_pixel(x, y, axis)
    }

    pub fn pixel_to_data(&self, p: egui::Pos2, axis: YAxis) -> Option<(f64, f64)> {
        self.backend.pixel_to_data(p, axis)
    }

    pub fn plot_bounds_in_pixels(&self) -> Option<egui::Rect> {
        self.backend.plot_bounds_in_pixels()
    }

    pub fn add_roi(&mut self, roi: Roi) -> usize {
        self.backend.plot_mut().rois.push(roi);
        let index = self.backend.plot().rois.len() - 1;
        self.events.push(PlotEvent::RoiChanged { index });
        index
    }

    pub fn rois(&self) -> &[Roi] {
        &self.backend.plot().rois
    }

    pub fn rois_mut(&mut self) -> &mut [Roi] {
        &mut self.backend.plot_mut().rois
    }

    pub fn clear_rois(&mut self) {
        self.backend.plot_mut().rois.clear();
        self.events.push(PlotEvent::RoisCleared);
    }

    pub fn pick_item(&self, p: egui::Pos2, item: ItemHandle) -> Option<PickResult> {
        self.backend.pick_item(p, item)
    }

    pub fn items_back_to_front(&self) -> Vec<ItemHandle> {
        self.backend.items_back_to_front()
    }

    pub fn replot(&mut self) {
        self.backend.replot();
    }

    pub fn save_graph(&self, path: &Path, size: (u32, u32)) -> Result<(), SaveError> {
        self.backend.save_graph(path, size)
    }

    /// Apply accumulated data bounds to the current view.
    pub fn reset_zoom_to_data(&mut self) {
        self.apply_limits_from_data_bounds();
    }

    fn apply_auto_limits(&mut self) {
        if self.auto_reset_zoom {
            self.apply_limits_from_data_bounds();
        }
    }

    fn apply_limits_from_data_bounds(&mut self) {
        let Some(x) = self.data_bounds.x else {
            return;
        };
        let Some(y_left) = self.data_bounds.y_left else {
            return;
        };
        let (xmin, xmax) = x.as_non_degenerate();
        let (ymin, ymax) = y_left.as_non_degenerate();
        let y2 = self.data_bounds.y_right.map(Bounds1D::as_non_degenerate);
        self.set_limits_internal(xmin, xmax, ymin, ymax, y2);
    }
}

/// High-level 1D plot. Methods are inherited from [`PlotWidget`] via `Deref`.
pub struct Plot1D {
    inner: PlotWidget,
}

impl Plot1D {
    pub fn new(render_state: &RenderState, id: PlotId) -> Self {
        let mut inner = PlotWidget::new(render_state, id);
        inner.set_graph_x_label("X");
        inner.set_graph_y_label("Y", YAxis::Left);
        inner.set_graph_grid(true);
        Self { inner }
    }

    /// Add a histogram to this 1D plot.
    pub fn add_histogram(
        &mut self,
        edges: &[f64],
        counts: &[f64],
        color: Color32,
    ) -> Result<ItemHandle, PlotDataError> {
        self.inner.add_histogram(edges, counts, color)
    }

    /// Add a marker-only scatter item to this 1D plot.
    pub fn add_scatter(&mut self, x: &[f64], y: &[f64], color: Color32) -> ItemHandle {
        self.inner.add_scatter(x, y, color)
    }

    /// Extract and add a horizontal image profile as a 1D curve.
    pub fn add_horizontal_profile_curve(
        &mut self,
        width: u32,
        height: u32,
        data: &[f32],
        row: u32,
        color: Color32,
    ) -> Result<ItemHandle, PlotDataError> {
        self.inner
            .add_horizontal_profile_curve(width, height, data, row, color)
    }

    /// Extract and add a vertical image profile as a 1D curve.
    pub fn add_vertical_profile_curve(
        &mut self,
        width: u32,
        height: u32,
        data: &[f32],
        column: u32,
        color: Color32,
    ) -> Result<ItemHandle, PlotDataError> {
        self.inner
            .add_vertical_profile_curve(width, height, data, column, color)
    }

    pub fn into_inner(self) -> PlotWidget {
        self.inner
    }
}

impl Deref for Plot1D {
    type Target = PlotWidget;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Plot1D {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// High-level 2D plot. Methods are inherited from [`PlotWidget`] via `Deref`.
pub struct Plot2D {
    inner: PlotWidget,
}

impl Plot2D {
    pub fn new(render_state: &RenderState, id: PlotId) -> Self {
        let mut inner = PlotWidget::new(render_state, id);
        inner.set_graph_x_label("Columns");
        inner.set_graph_y_label("Rows", YAxis::Left);
        inner.set_graph_grid(false);
        inner.set_keep_data_aspect_ratio(true);
        inner.set_y_inverted(true);
        Self { inner }
    }

    /// Add a scalar image using this plot's default colormap.
    pub fn add_default_image(&mut self, width: u32, height: u32, data: &[f32]) -> ItemHandle {
        self.inner.add_image_default(width, height, data)
    }

    /// Add a scalar image using this plot's default colormap, returning an
    /// error instead of panicking on length mismatch.
    pub fn try_add_default_image(
        &mut self,
        width: u32,
        height: u32,
        data: &[f32],
    ) -> Result<ItemHandle, PlotDataError> {
        self.inner.try_add_image_default(width, height, data)
    }

    /// Add a boolean mask overlay.
    pub fn add_mask(
        &mut self,
        width: u32,
        height: u32,
        mask: &[bool],
        color: Color32,
    ) -> Result<ItemHandle, PlotDataError> {
        self.inner.add_mask(width, height, mask, color)
    }

    /// Add a boolean mask overlay with explicit image geometry.
    pub fn add_mask_with_geometry(
        &mut self,
        width: u32,
        height: u32,
        mask: &[bool],
        color: Color32,
        geometry: ImageGeometry,
    ) -> Result<ItemHandle, PlotDataError> {
        self.inner
            .add_mask_with_geometry(width, height, mask, color, geometry)
    }

    /// Extract a row profile from image data.
    pub fn horizontal_profile(
        &self,
        width: u32,
        height: u32,
        data: &[f32],
        row: u32,
    ) -> Result<Vec<f64>, PlotDataError> {
        horizontal_profile_values(width, height, data, row)
    }

    /// Extract a column profile from image data.
    pub fn vertical_profile(
        &self,
        width: u32,
        height: u32,
        data: &[f32],
        column: u32,
    ) -> Result<Vec<f64>, PlotDataError> {
        vertical_profile_values(width, height, data, column)
    }

    pub fn into_inner(self) -> PlotWidget {
        self.inner
    }
}

impl Deref for Plot2D {
    type Target = PlotWidget;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Plot2D {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_bounds_ignores_non_finite_values() {
        let bounds = finite_bounds(&[f64::NAN, 2.0, -1.0, f64::INFINITY]).unwrap();
        assert_eq!(bounds.as_non_degenerate(), (-1.0, 2.0));
    }

    #[test]
    fn degenerate_bounds_get_padded() {
        assert_eq!(
            Bounds1D::new(2.0, 2.0).unwrap().as_non_degenerate(),
            (1.5, 2.5)
        );
    }

    #[test]
    fn data_bounds_tracks_left_and_right_y_separately() {
        let mut bounds = DataBounds::default();
        bounds.include(
            Bounds1D::new(0.0, 10.0).unwrap(),
            Bounds1D::new(-1.0, 1.0).unwrap(),
            YAxis::Left,
        );
        bounds.include(
            Bounds1D::new(5.0, 20.0).unwrap(),
            Bounds1D::new(100.0, 200.0).unwrap(),
            YAxis::Right,
        );
        assert_eq!(bounds.x.unwrap().as_non_degenerate(), (0.0, 20.0));
        assert_eq!(bounds.y_left.unwrap().as_non_degenerate(), (-1.0, 1.0));
        assert_eq!(bounds.y_right.unwrap().as_non_degenerate(), (100.0, 200.0));
    }

    #[test]
    fn histogram_step_values_builds_closed_outline() {
        let (x, y) = histogram_step_values(&[0.0, 1.0, 3.0], &[2.0, 4.0]).unwrap();
        assert_eq!(x, vec![0.0, 0.0, 1.0, 1.0, 3.0, 3.0]);
        assert_eq!(y, vec![0.0, 2.0, 2.0, 4.0, 4.0, 0.0]);
    }

    #[test]
    fn histogram_step_values_validates_edges() {
        assert_eq!(
            histogram_step_values(&[0.0, 1.0], &[2.0, 4.0]).unwrap_err(),
            PlotDataError::HistogramLength { bins: 2, edges: 2 }
        );
    }

    #[test]
    fn profile_helpers_extract_rows_and_columns() {
        let data = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        assert_eq!(
            horizontal_profile_values(3, 2, &data, 1).unwrap(),
            vec![4.0, 5.0, 6.0]
        );
        assert_eq!(
            vertical_profile_values(3, 2, &data, 2).unwrap(),
            vec![3.0, 6.0]
        );
    }

    #[test]
    fn profile_helpers_validate_shape_and_index() {
        assert_eq!(
            horizontal_profile_values(2, 2, &[1.0, 2.0, 3.0], 0).unwrap_err(),
            PlotDataError::ImageDataLength {
                expected: 4,
                actual: 3,
            }
        );
        assert_eq!(
            horizontal_profile_values(2, 2, &[1.0, 2.0, 3.0, 4.0], 2).unwrap_err(),
            PlotDataError::ProfileRow { row: 2, height: 2 }
        );
        assert_eq!(
            vertical_profile_values(2, 2, &[1.0, 2.0, 3.0, 4.0], 2).unwrap_err(),
            PlotDataError::ProfileColumn {
                column: 2,
                width: 2,
            }
        );
    }

    #[test]
    fn value_stats_ignore_non_finite_values() {
        let stats = ValueStats::from_f64(&[1.0, f64::NAN, 4.0, f64::INFINITY]);
        assert_eq!(stats.count, 4);
        assert_eq!(stats.finite_count, 2);
        assert_eq!(stats.min, Some(1.0));
        assert_eq!(stats.max, Some(4.0));
        assert_eq!(stats.mean, Some(2.5));
    }

    #[test]
    fn value_stats_from_f32_matches_f64_semantics() {
        let stats = ValueStats::from_f32(&[1.0, f32::NAN, 4.0, f32::INFINITY]);
        assert_eq!(stats.count, 4);
        assert_eq!(stats.finite_count, 2);
        assert_eq!(stats.min, Some(1.0));
        assert_eq!(stats.max, Some(4.0));
        assert_eq!(stats.mean, Some(2.5));
    }

    #[test]
    fn mask_rgba_pixels_are_transparent_where_false() {
        let color = Color32::from_rgba_unmultiplied(10, 20, 30, 40);
        let pixels = mask_rgba_pixels(&[true, false, true], color);
        let rgba = color.to_srgba_unmultiplied();
        assert_eq!(pixels, vec![rgba, [0, 0, 0, 0], rgba]);
    }

    #[test]
    fn legend_row_width_stays_within_parent_width() {
        assert_eq!(legend_row_width(180.0), 180.0);
        assert_eq!(legend_row_width(80.0), 80.0);
        assert_eq!(legend_row_width(0.0), LEGEND_ROW_MIN_WIDTH);
    }
}
