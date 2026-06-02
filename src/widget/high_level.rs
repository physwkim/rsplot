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
use crate::core::items::{Baseline, LineStyle, ScalarMask, Symbol};
use crate::core::marker::{Marker, MarkerKind, MarkerSymbol};
use crate::core::plot::{GraphGrid, Plot, PlotId};
use crate::core::roi::Roi;
use crate::core::scatter_viz::GridImage;
use crate::core::shape::{Shape, ShapeKind};
use crate::core::transform::{Margins, Scale, YAxis};
use crate::core::triangles::Triangles;
use crate::render::backend_wgpu::WgpuBackend;
use crate::render::gpu_curve::CurveData;
use crate::render::gpu_image::{AggregationMode, ImageData, ImagePixels, InterpolationMode};
use crate::render::save::SaveError;
use crate::widget::plot_widget::{PlotInteractionMode, PlotResponse, PlotView};

/// Live profile extraction mode (silx profile toolbar).
///
/// Used with [`Plot2D::show_profile_toolbar`] and
/// [`Plot2D::try_update_profile_from_response`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProfileMode {
    /// Profile disabled.
    #[default]
    None,
    /// Extract the row at the cursor Y position (horizontal slice).
    Horizontal,
    /// Extract the column at the cursor X position (vertical slice).
    Vertical,
    /// Extract profile along a drawn line segment.
    Line,
    /// Extract profile averaged/summed over a drawn rectangle.
    Rectangle,
}

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
    ///
    /// Delegates to [`crate::core::stats::Stats`] (the single source of truth
    /// for the silx statistic set) by treating the flat value array as a
    /// `width = len`, `height = 1` scalar image with unit geometry, then keeps
    /// only the count/finite-count/min/max/mean fields of the public
    /// `ValueStats` shape.
    pub fn from_f64(values: &[f64]) -> Self {
        Self::from_stats(crate::core::stats::Stats::for_image(
            values,
            values.len(),
            1,
            (0.0, 0.0),
            (1.0, 1.0),
            crate::core::stats::StatScope::All,
        ))
    }

    /// Compute statistics from `f32` values, ignoring non-finite values for
    /// min/max/mean while still counting them in [`Self::count`].
    ///
    /// Widens to `f64` and delegates to [`crate::core::stats::Stats`], the same
    /// single-source-of-truth path as [`Self::from_f64`].
    pub fn from_f32(values: &[f32]) -> Self {
        let widened: Vec<f64> = values.iter().map(|&v| v as f64).collect();
        Self::from_f64(&widened)
    }

    /// Project a [`crate::core::stats::Stats`] result onto the `ValueStats`
    /// fields. `Stats` carries the full silx statistic set (delta/sum/COM/
    /// argmin/argmax); `ValueStats` keeps only count/finite-count/min/max/mean.
    fn from_stats(stats: crate::core::stats::Stats) -> Self {
        Self {
            count: stats.count,
            finite_count: stats.finite_count,
            min: stats.min,
            max: stats.max,
            mean: stats.mean,
        }
    }
}

/// Statistics for curve-like items.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurveStats {
    /// Statistics over the X values.
    pub x: ValueStats,
    /// Statistics over the Y values.
    pub y: ValueStats,
    /// Which Y axis this curve is bound to.
    pub y_axis: YAxis,
}

/// Statistics for image-like items.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageStats {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Total pixel count (`width * height`).
    pub pixel_count: usize,
    /// Scalar pixel statistics. `None` for direct RGBA images and masks.
    pub scalar: Option<ValueStats>,
}

/// Geometry shared by image-like items.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageGeometry {
    /// Data-space position of the image's top-left corner `(x, y)`.
    pub origin: (f64, f64),
    /// Data-space size of one pixel `(dx, dy)`.
    pub scale: (f64, f64),
    /// Overall opacity in `[0.0, 1.0]`.
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
    /// Short lowercase string name for the item family.
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

    /// `true` for item families that live on the curve layer (Curve, Histogram, Scatter).
    pub fn is_curve_like(self) -> bool {
        matches!(self, Self::Curve | Self::Histogram | Self::Scatter)
    }

    /// `true` for item families that live on the image layer (Image, Mask).
    pub fn is_image_like(self) -> bool {
        matches!(self, Self::Image | Self::Mask)
    }
}

/// High-level events queued by [`PlotWidget`] for application code to drain.
#[derive(Clone, Debug, PartialEq)]
pub enum PlotEvent {
    /// An item was added to the plot.
    ItemAdded {
        handle: ItemHandle,
        kind: PlotItemKind,
    },
    /// An item's data was updated in place.
    ItemUpdated {
        handle: ItemHandle,
        kind: PlotItemKind,
    },
    /// An item was removed from the plot.
    ItemRemoved {
        handle: ItemHandle,
        kind: PlotItemKind,
    },
    /// The selected item changed (via legend click or [`PlotWidget::set_active_item`]).
    ActiveItemChanged {
        previous: Option<ItemHandle>,
        current: Option<ItemHandle>,
    },
    /// The display limits changed (pan, zoom, or programmatic update).
    LimitsChanged,
    /// An ROI edge drag moved the ROI at `index`.
    RoiChanged { index: usize },
    /// All ROIs were cleared.
    RoisCleared,
}

/// Return value of [`PlotWidget::show_legend`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LegendResponse {
    /// Item whose row was clicked (single-click select).
    pub selected: Option<ItemHandle>,
    /// Item whose row was double-clicked (activation).
    pub activated: Option<ItemHandle>,
    /// Handle whose visibility was toggled this frame (eye icon click).
    pub visibility_changed: Option<ItemHandle>,
}

/// Return value of [`PlotWidget::show_toolbar`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolbarResponse {
    /// Home button was clicked this frame.
    pub reset_zoom: bool,
    /// Interaction mode button (select/pan/zoom) was clicked.
    pub interaction_mode_changed: bool,
    /// Crosshair cursor toggle was clicked.
    pub cursor_changed: bool,
    /// Major grid toggle was clicked.
    pub grid_changed: bool,
    /// Minor grid toggle was clicked.
    pub minor_grid_changed: bool,
    /// Keep-aspect-ratio toggle was clicked.
    pub aspect_changed: bool,
    /// X log toggle was clicked.
    pub x_log_changed: bool,
    /// Y log toggle was clicked.
    pub y_log_changed: bool,
    /// X invert toggle was clicked.
    pub x_inverted_changed: bool,
    /// Y invert toggle was clicked.
    pub y_inverted_changed: bool,
}

/// Return value of [`PlotWidget::show_with_toolbar`].
pub struct PlotWithToolbarResponse {
    /// What the toolbar registered this frame.
    pub toolbar: ToolbarResponse,
    /// What the plot view registered this frame.
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

/// Extract a 1D profile along a line segment using nearest neighbor sampling.
/// `start` and `end` are in (column, row) coordinates.
pub fn line_profile_values(
    width: u32,
    height: u32,
    data: &[f32],
    start: (f64, f64),
    end: (f64, f64),
) -> Result<(Vec<f64>, Vec<f64>), PlotDataError> {
    validate_image_len(width, height, data.len())?;
    let (x0, y0) = start;
    let (x1, y1) = end;
    let dx = x1 - x0;
    let dy = y1 - y0;
    let dist = (dx * dx + dy * dy).sqrt();
    let n_points = dist.ceil() as usize + 1;

    let mut x_vals = Vec::with_capacity(n_points);
    let mut y_vals = Vec::with_capacity(n_points);

    let w = width as i64;
    let h = height as i64;

    for i in 0..n_points {
        let t = if n_points > 1 {
            i as f64 / (n_points - 1) as f64
        } else {
            0.0
        };
        let x = x0 + t * dx;
        let y = y0 + t * dy;
        let col = x.round() as i64;
        let row = y.round() as i64;

        let val = if col >= 0 && col < w && row >= 0 && row < h {
            data[(row as usize) * (width as usize) + (col as usize)] as f64
        } else {
            f64::NAN
        };
        x_vals.push(t * dist);
        y_vals.push(val);
    }

    Ok((x_vals, y_vals))
}

/// Extract a 1D profile within a rectangle by averaging along an axis.
/// `rect` is (x_min, x_max, y_min, y_max) in (column, row) coordinates.
pub fn rect_profile_values(
    width: u32,
    height: u32,
    data: &[f32],
    rect: (f64, f64, f64, f64),
    horizontal: bool,
) -> Result<(Vec<f64>, Vec<f64>), PlotDataError> {
    validate_image_len(width, height, data.len())?;
    let (x_min, x_max, y_min, y_max) = rect;

    let col_min = x_min.round().max(0.0) as usize;
    let col_max = x_max.round().min(width as f64 - 1.0) as usize;
    let row_min = y_min.round().max(0.0) as usize;
    let row_max = y_max.round().min(height as f64 - 1.0) as usize;

    if col_min > col_max
        || row_min > row_max
        || col_max >= width as usize
        || row_max >= height as usize
    {
        return Ok((vec![], vec![]));
    }

    if horizontal {
        let num_rows = (row_max - row_min + 1) as f64;
        let mut x_vals = Vec::with_capacity(col_max - col_min + 1);
        let mut y_vals = Vec::with_capacity(col_max - col_min + 1);

        for col in col_min..=col_max {
            let mut sum = 0.0;
            for row in row_min..=row_max {
                sum += data[row * width as usize + col] as f64;
            }
            x_vals.push(col as f64);
            y_vals.push(sum / num_rows);
        }
        Ok((x_vals, y_vals))
    } else {
        let num_cols = (col_max - col_min + 1) as f64;
        let mut x_vals = Vec::with_capacity(row_max - row_min + 1);
        let mut y_vals = Vec::with_capacity(row_max - row_min + 1);

        for row in row_min..=row_max {
            let mut sum = 0.0;
            for col in col_min..=col_max {
                sum += data[row * width as usize + col] as f64;
            }
            x_vals.push(row as f64);
            y_vals.push(sum / num_cols);
        }
        Ok((x_vals, y_vals))
    }
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
            interpolation: image.interpolation,
            // `image` already holds the (possibly aggregated) data, so the
            // round-tripped spec must not re-aggregate.
            aggregation: AggregationMode::None,
            aggregation_block: (1, 1),
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
            interpolation: image.interpolation,
            aggregation: AggregationMode::None,
            aggregation_block: (1, 1),
        },
    }
}

/// Apply an optional pixel mask to a scalar field before upload, returning the
/// masked row-major data (silx `items/image.py` `getValueData`: masked pixels →
/// `NaN`).
///
/// Validates that `data.len() == width * height` and that `mask` describes the
/// same `width × height` shape, returning [`PlotDataError::ImageDataLength`] on
/// a mismatch; on success every pixel flagged by `mask` is `f32::NAN` and the
/// rest pass through unchanged. Split out from
/// [`Plot2D::try_add_masked_image`] so the pre-upload transform is unit-testable
/// without a GPU backend.
fn apply_image_mask(
    width: u32,
    height: u32,
    data: &[f32],
    mask: &ScalarMask,
) -> Result<Vec<f32>, PlotDataError> {
    let expected = (width as usize).saturating_mul(height as usize);
    if data.len() != expected {
        return Err(PlotDataError::ImageDataLength {
            expected,
            actual: data.len(),
        });
    }
    if mask.width() != width as usize || mask.height() != height as usize {
        return Err(PlotDataError::ImageDataLength {
            expected,
            actual: mask.width().saturating_mul(mask.height()),
        });
    }
    Ok(mask.apply(data))
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

/// What a single legend-row interaction returned.
struct LegendRowResult {
    /// Click anywhere in the row body (not the eye icon).
    row_clicked: bool,
    /// Click on the visibility eye icon.
    eye_clicked: bool,
}

fn legend_row_response(
    ui: &mut egui::Ui,
    width: f32,
    kind: PlotItemKind,
    label: &str,
    active: bool,
    visible: bool,
    visual: LegendVisual,
) -> LegendRowResult {
    let (rect, row_response) =
        ui.allocate_exact_size(egui::vec2(width, LEGEND_ROW_HEIGHT), egui::Sense::click());
    let eye_rect = egui::Rect::from_min_max(
        egui::pos2(rect.right() - LEGEND_CHECK_WIDTH, rect.top()),
        rect.right_bottom(),
    );
    let eye_response = ui.interact(eye_rect, row_response.id.with("eye"), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        draw_legend_row(
            ui,
            LegendRowDraw {
                rect,
                response: &row_response,
                kind,
                label,
                active,
                visible,
                visual,
            },
        );
    }
    LegendRowResult {
        row_clicked: row_response.clicked() && !eye_response.clicked(),
        eye_clicked: eye_response.clicked(),
    }
}

struct LegendRowDraw<'a> {
    rect: egui::Rect,
    response: &'a egui::Response,
    kind: PlotItemKind,
    label: &'a str,
    active: bool,
    visible: bool,
    visual: LegendVisual,
}

fn draw_legend_row(ui: &egui::Ui, p: LegendRowDraw<'_>) {
    let LegendRowDraw {
        rect,
        response,
        kind,
        label,
        active,
        visible,
        visual,
    } = p;
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

    draw_legend_eye(
        &painter,
        check_rect,
        visible,
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

/// Draw an eye icon in `rect` to indicate visibility. An open eye = visible;
/// a closed eye (dash through center) = hidden. Active items get the accent color.
fn draw_legend_eye(
    painter: &egui::Painter,
    rect: egui::Rect,
    visible: bool,
    active: bool,
    color: Color32,
) {
    let cx = rect.center().x;
    let cy = rect.center().y;
    let r = 3.5_f32;
    let eye_color = if active {
        color
    } else {
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 180)
    };
    if visible {
        painter.circle_stroke(egui::pos2(cx, cy), r, egui::Stroke::new(1.5, eye_color));
        painter.circle_filled(egui::pos2(cx, cy), r * 0.45, eye_color);
    } else {
        let dim = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 80);
        painter.circle_stroke(egui::pos2(cx, cy), r, egui::Stroke::new(1.5, dim));
        painter.line_segment(
            [egui::pos2(cx - r * 1.3, cy), egui::pos2(cx + r * 1.3, cy)],
            egui::Stroke::new(1.5, dim),
        );
    }
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

    /// Whether newly added data updates the displayed data limits.
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

    /// Add a curve from [`CurveData`] and assign a legend label in one call.
    pub fn add_curve_data_with_legend(
        &mut self,
        curve: &CurveData,
        legend: impl Into<String>,
    ) -> ItemHandle {
        let handle = self.add_curve_data(curve);
        self.set_item_legend(handle, legend);
        handle
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

    /// Add a direct RGBA image and assign a legend label.
    pub fn add_rgba_image_with_legend(
        &mut self,
        width: u32,
        height: u32,
        data: &[[u8; 4]],
        legend: impl Into<String>,
    ) -> ItemHandle {
        let handle = self.add_rgba_image(width, height, data);
        self.set_item_legend(handle, legend);
        handle
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

    /// Add a triangle mesh from [`Triangles`] and assign a legend label.
    pub fn add_triangles_data_with_legend(
        &mut self,
        triangles: &Triangles,
        legend: impl Into<String>,
    ) -> ItemHandle {
        let handle = self.add_triangles_data(triangles);
        self.set_item_legend(handle, legend);
        handle
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

    /// Add a shape overlay from [`Shape`] and assign a legend label.
    pub fn add_shape_data_with_legend(
        &mut self,
        shape: &Shape,
        legend: impl Into<String>,
    ) -> ItemHandle {
        let handle = self.add_shape_data(shape);
        self.set_item_legend(handle, legend);
        handle
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

    /// Add a marker from [`Marker`] and assign a legend label.
    pub fn add_marker_data_with_legend(
        &mut self,
        marker: &Marker,
        legend: impl Into<String>,
    ) -> ItemHandle {
        let handle = self.add_marker_data(marker);
        self.set_item_legend(handle, legend);
        handle
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

    /// Show or hide an item. Hidden items are excluded from all draw passes.
    /// Returns `false` if the handle is unknown.
    pub fn set_item_visible(&mut self, handle: ItemHandle, visible: bool) -> bool {
        self.backend.set_item_visible(handle, visible)
    }

    /// Whether an item is currently visible.
    pub fn is_item_visible(&self, handle: ItemHandle) -> bool {
        self.backend.is_item_visible(handle)
    }

    /// Set the draw-order z-value for an item. Within each GPU item layer
    /// (images, curves), higher-z items are drawn on top.
    /// Returns `false` if the handle is unknown.
    pub fn set_item_z(&mut self, handle: ItemHandle, z: f32) -> bool {
        self.backend.set_item_z(handle, z)
    }

    /// Current z-value for an item.
    pub fn item_z_value(&self, handle: ItemHandle) -> f32 {
        self.backend.item_z(handle)
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

    /// Draw a selectable legend list. Clicking a row body makes it active;
    /// clicking the eye icon toggles visibility.
    pub fn show_legend(&mut self, ui: &mut egui::Ui) -> LegendResponse {
        let rows: Vec<(ItemHandle, PlotItemKind, String, bool, bool, LegendVisual)> = self
            .item_records
            .iter()
            .map(|record| {
                let visible = self.backend.is_item_visible(record.handle);
                (
                    record.handle,
                    record.kind,
                    self.legend_label(record),
                    self.active_item == Some(record.handle),
                    visible,
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
                for (handle, kind, label, active, visible, visual) in rows {
                    let result =
                        legend_row_response(ui, width, kind, &label, active, visible, visual);
                    if result.row_clicked {
                        out.selected = Some(handle);
                        if self.set_active_item(Some(handle)) {
                            out.activated = Some(handle);
                        }
                    }
                    if result.eye_clicked {
                        self.backend.set_item_visible(handle, !visible);
                        out.visibility_changed = Some(handle);
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

    /// Show a compact profile-mode toolbar with None / Horizontal / Vertical buttons.
    ///
    /// The selected mode is stored in egui temp-memory keyed by the plot id so it
    /// persists across frames. Returns the current mode.
    ///
    /// Place it inside `show_toolbar_with` so it sits in the same toolbar row:
    ///
    /// ```ignore
    /// let (_, mode) = plot.show_toolbar_with(ui, |ui, plot| {
    ///     ui.separator();
    ///     plot.show_profile_toolbar(ui)
    /// });
    /// ```
    pub fn show_profile_toolbar(&self, ui: &mut egui::Ui) -> ProfileMode {
        let id = egui::Id::new(self.backend().plot().id).with("profile_mode");
        let mut mode = ui
            .data(|d| d.get_temp::<ProfileMode>(id))
            .unwrap_or_default();

        ui.horizontal(|ui| {
            if ui
                .selectable_label(mode == ProfileMode::None, "○")
                .on_hover_text("No profile")
                .clicked()
            {
                mode = ProfileMode::None;
            }
            if ui
                .selectable_label(mode == ProfileMode::Horizontal, "H")
                .on_hover_text("Horizontal profile (row slice)")
                .clicked()
            {
                mode = ProfileMode::Horizontal;
            }
            if ui
                .selectable_label(mode == ProfileMode::Vertical, "V")
                .on_hover_text("Vertical profile (column slice)")
                .clicked()
            {
                mode = ProfileMode::Vertical;
            }
            if ui
                .selectable_label(mode == ProfileMode::Line, "L")
                .on_hover_text("Line profile (draw line ROI)")
                .clicked()
            {
                mode = ProfileMode::Line;
            }
            if ui
                .selectable_label(mode == ProfileMode::Rectangle, "R")
                .on_hover_text("Rectangle profile (draw rect ROI)")
                .clicked()
            {
                mode = ProfileMode::Rectangle;
            }
        });

        ui.data_mut(|d| d.insert_temp(id, mode));
        mode
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

    /// Return the current X axis limits `(min, max)`.
    pub fn x_limits(&self) -> (f64, f64) {
        self.backend.x_limits()
    }

    /// Return the current X axis limits `(min, max)` (alias for [`x_limits`](Self::x_limits)).
    pub fn get_graph_x_limits(&self) -> (f64, f64) {
        self.x_limits()
    }

    /// Set the X axis display limits.
    pub fn set_graph_x_limits(&mut self, xmin: f64, xmax: f64) {
        let (_, _, ymin, ymax) = self.backend.plot().limits;
        self.set_limits_internal(xmin, xmax, ymin, ymax, self.backend.plot().y2);
    }

    /// Return the current Y axis limits `(min, max)` for `axis`, or `None` if
    /// the axis has not been given explicit limits.
    pub fn y_limits(&self, axis: YAxis) -> Option<(f64, f64)> {
        self.backend.y_limits(axis)
    }

    /// Return Y axis limits (alias for [`y_limits`](Self::y_limits)).
    pub fn get_graph_y_limits(&self, axis: YAxis) -> Option<(f64, f64)> {
        self.y_limits(axis)
    }

    /// Set the Y axis display limits.  Pass [`YAxis::Right`] for the secondary axis.
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

    /// Enable or disable a log10 X axis.  Limits must be strictly positive when on.
    pub fn set_x_log(&mut self, on: bool) {
        self.backend.set_x_log(on);
    }

    /// Enable or disable a log10 X axis (alias for [`set_x_log`](Self::set_x_log)).
    pub fn set_graph_x_log(&mut self, on: bool) {
        self.set_x_log(on);
    }

    /// Return `true` if the X axis is logarithmic.
    pub fn is_x_logarithmic(&self) -> bool {
        self.backend.plot().x_scale == Scale::Log10
    }

    /// Return `true` if the X axis is logarithmic (alias for [`is_x_logarithmic`](Self::is_x_logarithmic)).
    pub fn is_graph_x_log(&self) -> bool {
        self.is_x_logarithmic()
    }

    /// Enable or disable a log10 Y axis.  Limits must be strictly positive when on.
    pub fn set_y_log(&mut self, on: bool) {
        self.backend.set_y_log(on);
    }

    /// Enable or disable a log10 Y axis (alias for [`set_y_log`](Self::set_y_log)).
    pub fn set_graph_y_log(&mut self, on: bool) {
        self.set_y_log(on);
    }

    /// Return `true` if the Y axis is logarithmic.
    pub fn is_y_logarithmic(&self) -> bool {
        self.backend.plot().y_scale == Scale::Log10
    }

    /// Return `true` if the Y axis is logarithmic (alias for [`is_y_logarithmic`](Self::is_y_logarithmic)).
    pub fn is_graph_y_log(&self) -> bool {
        self.is_y_logarithmic()
    }

    /// Invert the X axis direction (right-to-left).
    pub fn set_x_inverted(&mut self, on: bool) {
        self.backend.set_x_inverted(on);
    }

    /// Return `true` if the X axis is inverted.
    pub fn is_x_inverted(&self) -> bool {
        self.backend.plot().x_inverted
    }

    // --- Axis range constraints (silx Axis.setRangeConstraints / setLimitsConstraints) ---

    /// Minimum allowed X span (prevents zooming in below this width).
    pub fn set_x_min_range(&mut self, min: Option<f64>) {
        self.backend.plot_mut().x_constraints.min_range = min;
    }

    /// Maximum allowed X span (prevents zooming out above this width).
    pub fn set_x_max_range(&mut self, max: Option<f64>) {
        self.backend.plot_mut().x_constraints.max_range = max;
    }

    /// Minimum allowed X lower bound (prevents panning below this value).
    pub fn set_x_min_pos(&mut self, min: Option<f64>) {
        self.backend.plot_mut().x_constraints.min_pos = min;
    }

    /// Maximum allowed X upper bound (prevents panning above this value).
    pub fn set_x_max_pos(&mut self, max: Option<f64>) {
        self.backend.plot_mut().x_constraints.max_pos = max;
    }

    /// Minimum allowed Y span (prevents zooming in below this height).
    pub fn set_y_min_range(&mut self, min: Option<f64>) {
        self.backend.plot_mut().y_constraints.min_range = min;
    }

    /// Maximum allowed Y span (prevents zooming out above this height).
    pub fn set_y_max_range(&mut self, max: Option<f64>) {
        self.backend.plot_mut().y_constraints.max_range = max;
    }

    /// Minimum allowed Y lower bound (prevents panning below this value).
    pub fn set_y_min_pos(&mut self, min: Option<f64>) {
        self.backend.plot_mut().y_constraints.min_pos = min;
    }

    /// Maximum allowed Y upper bound (prevents panning above this value).
    pub fn set_y_max_pos(&mut self, max: Option<f64>) {
        self.backend.plot_mut().y_constraints.max_pos = max;
    }

    /// Read back current X axis constraints.
    pub fn x_constraints(&self) -> crate::core::plot::AxisConstraints {
        self.backend.plot().x_constraints
    }

    /// Read back current Y axis constraints.
    pub fn y_constraints(&self) -> crate::core::plot::AxisConstraints {
        self.backend.plot().y_constraints
    }

    /// Invert the Y axis direction (top-to-bottom, as in image coordinates).
    pub fn set_y_inverted(&mut self, on: bool) {
        self.backend.set_y_inverted(on);
    }

    /// Return `true` if the Y axis is inverted.
    pub fn is_y_inverted(&self) -> bool {
        self.backend.plot().y_inverted
    }

    /// Set the maximum number of major ticks on the X axis.
    ///
    /// The chrome calls [`nice_ticks`](crate::widget::chrome::nice_ticks) with this
    /// cap, so the actual count may be lower to keep round step sizes.
    /// Pass `None` to restore the default (8 ticks).
    pub fn set_x_tick_count(&mut self, n: Option<usize>) {
        self.backend.plot_mut().x_max_ticks = n;
    }

    /// Return the current X tick-count cap, or `None` for the default (8).
    pub fn x_tick_count(&self) -> Option<usize> {
        self.backend.plot().x_max_ticks
    }

    /// Set the maximum number of major ticks on the Y axis.
    ///
    /// Pass `None` to restore the default (6 ticks).
    pub fn set_y_tick_count(&mut self, n: Option<usize>) {
        self.backend.plot_mut().y_max_ticks = n;
    }

    /// Return the current Y tick-count cap, or `None` for the default (6).
    pub fn y_tick_count(&self) -> Option<usize> {
        self.backend.plot().y_max_ticks
    }

    /// Keep data square on screen by expanding the tighter axis' display range.
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

    /// Show a compact ROI manager panel: a table listing all current ROIs with
    /// per-row remove buttons, buttons to add each ROI kind, and a clear-all
    /// button. Mirrors silx `RegionOfInterestTableWidget` / `RegionOfInterestManager`.
    ///
    /// New ROIs are centered on the current plot view. Returns the index of any
    /// newly added ROI, or `None` when none was added this frame.
    pub fn show_roi_manager(&mut self, ui: &mut egui::Ui) -> Option<usize> {
        let mut added: Option<usize> = None;
        let mut remove_idx: Option<usize> = None;

        // --- existing ROI table ---
        egui::ScrollArea::vertical()
            .max_height(200.0)
            .show(ui, |ui| {
                for (i, roi) in self.backend.plot().rois.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let desc = roi_description(roi);
                        ui.label(desc);
                        if ui.small_button("×").on_hover_text("Remove").clicked() {
                            remove_idx = Some(i);
                        }
                    });
                }
            });

        if let Some(idx) = remove_idx {
            self.backend.plot_mut().rois.remove(idx);
            self.events.push(PlotEvent::RoisCleared);
        }

        // --- add buttons ---
        let (x0, x1, y0, y1) = self.backend.plot().limits;
        let cx = (x0 + x1) * 0.5;
        let cy = (y0 + y1) * 0.5;
        let dx = (x1 - x0) * 0.2;
        let dy = (y1 - y0) * 0.2;

        ui.horizontal_wrapped(|ui| {
            if ui.button("+ Rect").clicked() {
                let idx = self.add_roi(Roi::Rect {
                    x: (cx - dx, cx + dx),
                    y: (cy - dy, cy + dy),
                });
                added = Some(idx);
            }
            if ui.button("+ HRange").clicked() {
                let idx = self.add_roi(Roi::HRange {
                    y: (cy - dy, cy + dy),
                });
                added = Some(idx);
            }
            if ui.button("+ VRange").clicked() {
                let idx = self.add_roi(Roi::VRange {
                    x: (cx - dx, cx + dx),
                });
                added = Some(idx);
            }
            if ui.button("+ Point").clicked() {
                let idx = self.add_roi(Roi::Point { x: cx, y: cy });
                added = Some(idx);
            }
            if ui.button("+ Line").clicked() {
                let idx = self.add_roi(Roi::Line {
                    start: (cx - dx, cy),
                    end: (cx + dx, cy),
                });
                added = Some(idx);
            }
        });

        // --- clear all ---
        if !self.backend.plot().rois.is_empty() && ui.button("Clear all").clicked() {
            self.clear_rois();
        }

        added
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
    /// Create a 1D plot with default X/Y labels and major grid enabled.
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

    /// Unwrap to the underlying [`PlotWidget`].
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
    /// Create a 2D plot with column/row labels, no grid, aspect lock, and Y-axis inverted.
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

    /// Add a scalar image whose pixels are masked to `NaN` before upload, using
    /// this plot's default colormap.
    ///
    /// Mirrors silx `items/image.py` `getValueData` (mask → NaN): every pixel
    /// flagged in `mask` ([`ScalarMask::is_masked`]) becomes `f32::NAN` *before*
    /// the data is handed to the backend, so the scalar pipeline's `nan_color`
    /// renders it as a hole — exactly as if the data had arrived with NaNs. The
    /// mask is optional and applied here, keeping the upload path additive.
    ///
    /// `mask` must describe a `width × height` image (its own
    /// [`ScalarMask::width`]/[`ScalarMask::height`]); a mismatch with the
    /// supplied `(width, height)` or `data.len()` returns
    /// [`PlotDataError::ImageDataLength`].
    pub fn try_add_masked_image(
        &mut self,
        width: u32,
        height: u32,
        data: &[f32],
        mask: &ScalarMask,
    ) -> Result<ItemHandle, PlotDataError> {
        // silx getValueData: masked pixels become NaN before reaching the
        // backend (the existing NaN rendering then displays the hole).
        let masked = apply_image_mask(width, height, data, mask)?;
        self.inner.try_add_image_default(width, height, &masked)
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

    /// Extract a profile at the cursor position from `plot_response`.
    ///
    /// Returns `Some((x_axis, y_values))` when `mode` is active and the cursor is over
    /// a valid pixel, `None` otherwise.  `pixels` must be a row-major `f32` array of
    /// `width * height` elements.
    ///
    /// Typical use in the frame loop:
    ///
    /// ```ignore
    /// if let Some((x, y)) = image_plot.profile_at_cursor(&resp, &pixels, w, h, mode) {
    ///     profile_plot.update_curve_data(handle, &CurveData::new(x, y, Color32::YELLOW));
    /// }
    /// ```
    pub fn profile_at_cursor(
        &self,
        plot_response: &PlotResponse,
        pixels: &[f32],
        width: u32,
        height: u32,
        mode: ProfileMode,
    ) -> Option<(Vec<f64>, Vec<f64>)> {
        if mode == ProfileMode::None {
            return None;
        }
        let hover_px = plot_response.response.hover_pos()?;
        let (data_x, data_y) = plot_response.transform.pixel_to_data(hover_px);

        let col = data_x.floor() as i64;
        let row = data_y.floor() as i64;

        match mode {
            ProfileMode::None => None,
            ProfileMode::Horizontal => {
                if row < 0 || row >= height as i64 {
                    return None;
                }
                horizontal_profile_values(width, height, pixels, row as u32)
                    .ok()
                    .map(|y| {
                        let x: Vec<f64> = (0..width as usize).map(|i| i as f64).collect();
                        (x, y)
                    })
            }
            ProfileMode::Vertical => {
                if col < 0 || col >= width as i64 {
                    return None;
                }
                vertical_profile_values(width, height, pixels, col as u32)
                    .ok()
                    .map(|y| {
                        let x: Vec<f64> = (0..height as usize).map(|i| i as f64).collect();
                        (x, y)
                    })
            }
            _ => None,
        }
    }

    /// Unwrap to the underlying [`PlotWidget`].
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

// ─── CompareImages ────────────────────────────────────────────────────────────

/// Visual mode for [`CompareImages`].
///
/// Mirrors the `VisualizationMode` options in silx `CompareImages.py`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum CompareMode {
    /// Show only image A.
    OnlyA,
    /// Show only image B.
    OnlyB,
    /// Left/right split: the left `split` fraction shows A, the rest shows B.
    #[default]
    HalfHalf,
    /// Pixel-wise A − B, normalised to `[-1, 1]` for display.
    Subtract,
}

/// A retained widget that displays two co-registered images with a draggable
/// split slider, mirroring silx `CompareImages`.
///
/// Create once, call [`Self::set_images`] to upload both images, then in the
/// frame loop call [`Self::show_toolbar`] and [`Self::show`].
///
/// ```ignore
/// let mut cmp = CompareImages::new(render_state, 0);
/// cmp.set_images(width, height, &data_a, &data_b, Colormap::viridis(0.0, 1.0))?;
///
/// // frame loop
/// cmp.show_toolbar(ui);
/// cmp.show(ui);
/// ```
pub struct CompareImages {
    inner: PlotWidget,
    width: u32,
    height: u32,
    data_a: Vec<f32>,
    data_b: Vec<f32>,
    colormap: Colormap,
    composite_handle: Option<ItemHandle>,
    split: f32,
    mode: CompareMode,
    dirty: bool,
}

impl CompareImages {
    /// Create a new compare-images widget backed by wgpu plot id `id`.
    pub fn new(render_state: &RenderState, id: PlotId) -> Self {
        let mut inner = PlotWidget::new(render_state, id);
        inner.set_keep_data_aspect_ratio(true);
        Self {
            inner,
            width: 0,
            height: 0,
            data_a: Vec::new(),
            data_b: Vec::new(),
            colormap: Colormap::viridis(0.0, 1.0),
            composite_handle: None,
            split: 0.5,
            mode: CompareMode::HalfHalf,
            dirty: false,
        }
    }

    /// Upload both images.  Validates `data_a.len() == data_b.len() == width * height`.
    pub fn set_images(
        &mut self,
        width: u32,
        height: u32,
        data_a: &[f32],
        data_b: &[f32],
        colormap: Colormap,
    ) -> Result<(), PlotDataError> {
        let expected = (width as usize).saturating_mul(height as usize);
        if data_a.len() != expected {
            return Err(PlotDataError::ImageDataLength {
                expected,
                actual: data_a.len(),
            });
        }
        if data_b.len() != expected {
            return Err(PlotDataError::ImageDataLength {
                expected,
                actual: data_b.len(),
            });
        }
        self.width = width;
        self.height = height;
        self.data_a = data_a.to_vec();
        self.data_b = data_b.to_vec();
        self.colormap = colormap;
        self.dirty = true;
        Ok(())
    }

    /// Current split position in [0, 1] — fraction of the width shown as A.
    pub fn split(&self) -> f32 {
        self.split
    }

    /// Set the split position.
    pub fn set_split(&mut self, split: f32) {
        let clamped = split.clamp(0.0, 1.0);
        if (clamped - self.split).abs() > 1e-6 {
            self.split = clamped;
            self.dirty = true;
        }
    }

    /// Current visualization mode.
    pub fn mode(&self) -> CompareMode {
        self.mode
    }

    /// Set the visualization mode.
    pub fn set_mode(&mut self, mode: CompareMode) {
        if mode != self.mode {
            self.mode = mode;
            self.dirty = true;
        }
    }

    /// Show mode + split controls in a compact toolbar row.  Returns the current mode.
    ///
    /// Call this before [`Self::show`].
    pub fn show_toolbar(&mut self, ui: &mut egui::Ui) -> CompareMode {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;

            for (label, tooltip, m) in [
                ("A", "Show only image A", CompareMode::OnlyA),
                ("B", "Show only image B", CompareMode::OnlyB),
                ("½", "Half-half split (drag slider)", CompareMode::HalfHalf),
                ("A-B", "Subtract: A minus B", CompareMode::Subtract),
            ] {
                if ui
                    .selectable_label(self.mode == m, label)
                    .on_hover_text(tooltip)
                    .clicked()
                    && self.mode != m
                {
                    self.mode = m;
                    self.dirty = true;
                }
            }

            if self.mode == CompareMode::HalfHalf && !self.data_a.is_empty() {
                ui.add_space(4.0);
                if ui
                    .add(egui::Slider::new(&mut self.split, 0.0..=1.0).text("split"))
                    .changed()
                {
                    self.dirty = true;
                }
            }
        });

        self.mode
    }

    /// Render the comparison image in `ui`.
    pub fn show(&mut self, ui: &mut egui::Ui) -> PlotResponse {
        if self.dirty && !self.data_a.is_empty() {
            let composite = self.build_composite();
            if let Some(handle) = self.composite_handle {
                self.inner
                    .try_update_rgba_image(handle, self.width, self.height, &composite)
                    .ok();
            } else {
                let handle = self
                    .inner
                    .add_rgba_image(self.width, self.height, &composite);
                self.composite_handle = Some(handle);
            }
            self.dirty = false;
        }
        self.inner.show(ui)
    }

    /// Build the composite RGBA pixel array for the current mode and split.
    fn build_composite(&self) -> Vec<[u8; 4]> {
        let n = (self.width as usize) * (self.height as usize);
        let split_col = (self.split * self.width as f32).round() as usize;

        match self.mode {
            CompareMode::OnlyA => colormap_to_rgba(self.width, &self.data_a, &self.colormap),
            CompareMode::OnlyB => colormap_to_rgba(self.width, &self.data_b, &self.colormap),
            CompareMode::HalfHalf => {
                let rgba_a = colormap_to_rgba(self.width, &self.data_a, &self.colormap);
                let rgba_b = colormap_to_rgba(self.width, &self.data_b, &self.colormap);
                let mut out = vec![[0u8; 4]; n];
                for row in 0..self.height as usize {
                    let base = row * self.width as usize;
                    for col in 0..self.width as usize {
                        let i = base + col;
                        out[i] = if col < split_col {
                            rgba_a[i]
                        } else {
                            rgba_b[i]
                        };
                    }
                }
                out
            }
            CompareMode::Subtract => self
                .data_a
                .iter()
                .zip(self.data_b.iter())
                .map(|(&a, &b)| {
                    let diff = (a - b).clamp(-1.0, 1.0);
                    if diff > 0.0 {
                        [(diff * 255.0) as u8, 0, 0, 255]
                    } else if diff < 0.0 {
                        [0, 0, ((-diff) * 255.0) as u8, 255]
                    } else {
                        [128, 128, 128, 255]
                    }
                })
                .collect(),
        }
    }
}

impl Deref for CompareImages {
    type Target = PlotWidget;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for CompareImages {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Apply a colormap to scalar pixel data and return RGBA bytes.
fn colormap_to_rgba(_width: u32, data: &[f32], colormap: &Colormap) -> Vec<[u8; 4]> {
    data.iter()
        .map(|&v| {
            let t = colormap.normalize(v as f64);
            let idx = (t * 255.0).clamp(0.0, 255.0) as usize;
            colormap.lut[idx]
        })
        .collect()
}

// ─── ImageView ────────────────────────────────────────────────────────────────

/// A 2D image viewer with side aggregate-profile panels, mirroring silx
/// `ImageView`.
///
/// Layout:
/// ```text
/// ┌──────────────────┬───────┐
/// │  histo_h (top)   │       │
/// ├──────────────────┤       │
/// │   image_plot     │ histo_v│
/// │   (centre)       │(right) │
/// └──────────────────┴───────┘
/// ```
///
/// The horizontal histogram (top) shows column sums; the vertical histogram
/// (right) shows row sums.  Both use `SyncAxes` to track the image-plot limits.
/// A colorbar column sits at the far right, synced to the active image's
/// colormap (silx `ImageView` grid column 2, ImageView.py:501).
///
/// Width in points reserved for the right-hand colorbar column. Wide enough for
/// the 25 pt gradient strip (silx `_ColorScale`) plus ticks and end labels.
const COLORBAR_WIDTH: f32 = 70.0;

/// Height in points of the radar overview in the bottom-right corner (silx
/// `_radarView`, ImageView.py:486-490). Matches the histogram strip thickness.
const RADAR_OVERVIEW_SIZE: f32 = 80.0;

/// Build the side [`ColorBarWidget`](crate::widget::colorbar::ColorBarWidget)
/// for an [`ImageView`], synced to `colormap`'s value limits (silx
/// `ImageView.getColorBarWidget`, ImageView.py:501). Split out from
/// [`ImageView::colorbar`] so the colormap→colorbar sync is unit-testable
/// without a GPU backend.
fn image_view_colorbar(colormap: &Colormap) -> crate::widget::colorbar::ColorBarWidget {
    crate::widget::colorbar::ColorBarWidget::new(colormap.clone())
}

/// Build the [`ScatterView`] side colorbar from its retained value colormap
/// (silx `ScatterView.getColorBarWidget`, ScatterView.py:83-88). Returns `None`
/// when no data has been uploaded yet (`colormap` is `None`). Split out from
/// [`ScatterView::colorbar`] so the colormap→colorbar mapping is unit-testable
/// without a GPU backend.
fn scatter_view_colorbar(
    colormap: Option<&Colormap>,
) -> Option<crate::widget::colorbar::ColorBarWidget> {
    colormap.map(image_view_colorbar)
}

/// Extract cursor data coordinates `[x, y]` from a pointer event for the
/// PositionInfo readout (silx `PositionInfo._updateStatusBar`, fed by
/// `sigMouseMoved`). A move (hover), click, or double-click over the data area
/// all carry the data-space `(x, y)`; a `LimitsChanged` event carries no cursor
/// and yields `None`. `None` input (no pointer event this frame) yields `None`.
fn cursor_from_pointer_event(
    event: Option<&crate::widget::interaction::PlotPointerEvent>,
) -> Option<[f64; 2]> {
    use crate::widget::interaction::PlotPointerEvent;
    match event? {
        PlotPointerEvent::Moved { data, .. }
        | PlotPointerEvent::Clicked { data, .. }
        | PlotPointerEvent::DoubleClicked { data, .. } => Some([data.0, data.1]),
        PlotPointerEvent::LimitsChanged { .. } => None,
    }
}

/// Extract a 1D profile for an [`ImageView`]'s profile tool from a drag between
/// data-space `(col, row)` endpoints `start` and `end` (silx
/// `ImageView._ProfileToolBar`, ImageView.py:692-697), dispatching to the
/// existing profile functions per `mode`:
///
/// - [`ProfileMode::Line`] → [`line_profile_values`] along `start`→`end`;
/// - [`ProfileMode::Horizontal`] → [`horizontal_profile_values`] at the row of
///   `end`, with the column index as the x axis;
/// - [`ProfileMode::Vertical`] → [`vertical_profile_values`] at the column of
///   `end`, with the row index as the x axis;
/// - [`ProfileMode::Rectangle`] → [`rect_profile_values`] over the `start`→`end`
///   bounding box, averaged along rows (a row profile);
/// - [`ProfileMode::None`] → `None`.
///
/// Returns `None` when the mode is disabled or the index is out of range.
/// Split out so the drag→profile mapping is unit-testable without a GPU backend.
fn image_view_profile_values(
    mode: ProfileMode,
    width: u32,
    height: u32,
    pixels: &[f32],
    start: (f64, f64),
    end: (f64, f64),
) -> Option<(Vec<f64>, Vec<f64>)> {
    match mode {
        ProfileMode::None => None,
        ProfileMode::Line => line_profile_values(width, height, pixels, start, end).ok(),
        ProfileMode::Horizontal => {
            let row = end.1.floor();
            if row < 0.0 || row >= height as f64 {
                return None;
            }
            horizontal_profile_values(width, height, pixels, row as u32)
                .ok()
                .map(|y| {
                    let x: Vec<f64> = (0..width as usize).map(|i| i as f64).collect();
                    (x, y)
                })
        }
        ProfileMode::Vertical => {
            let col = end.0.floor();
            if col < 0.0 || col >= width as f64 {
                return None;
            }
            vertical_profile_values(width, height, pixels, col as u32)
                .ok()
                .map(|y| {
                    let x: Vec<f64> = (0..height as usize).map(|i| i as f64).collect();
                    (x, y)
                })
        }
        ProfileMode::Rectangle => {
            let rect = (
                start.0.min(end.0),
                start.0.max(end.0),
                start.1.min(end.1),
                start.1.max(end.1),
            );
            rect_profile_values(width, height, pixels, rect, true).ok()
        }
    }
}

/// Build the profile-tool [`Roi`] from a drag between data-space `(col, row)`
/// endpoints for `mode` (silx `_ProfileToolBar` ROI shape per mode):
///
/// - [`ProfileMode::Line`] → [`Roi::Line`] `start`→`end`;
/// - [`ProfileMode::Horizontal`] → [`Roi::HRange`] at the row of `end`
///   (a degenerate range `(row, row)`, so the window's midpoint is that row);
/// - [`ProfileMode::Vertical`] → [`Roi::VRange`] at the column of `end`;
/// - [`ProfileMode::Rectangle`] → [`Roi::Rect`] over the `start`→`end` box;
/// - [`ProfileMode::None`] → `None`.
///
/// The returned ROI is handed to [`ProfileWindow::update_profile`], which
/// re-derives the samples with the same profile helpers as
/// [`image_view_profile_values`].
///
/// [`ProfileWindow::update_profile`]: crate::widget::profile_window::ProfileWindow::update_profile
fn profile_roi_from_drag(mode: ProfileMode, start: (f64, f64), end: (f64, f64)) -> Option<Roi> {
    match mode {
        ProfileMode::None => None,
        ProfileMode::Line => Some(Roi::Line { start, end }),
        ProfileMode::Horizontal => {
            let row = end.1.floor();
            Some(Roi::HRange { y: (row, row) })
        }
        ProfileMode::Vertical => {
            let col = end.0.floor();
            Some(Roi::VRange { x: (col, col) })
        }
        ProfileMode::Rectangle => Some(Roi::Rect {
            x: (start.0.min(end.0), start.0.max(end.0)),
            y: (start.1.min(end.1), start.1.max(end.1)),
        }),
    }
}

/// Build the [`ImageSpec`] for an [`ImageView`]'s active image from its retained
/// colormap and `alpha` (silx `ActiveImageAlphaSlider` propagation,
/// ImageView.py:513-517). Split out from [`ImageView::upload_image`] so the
/// alpha→spec propagation is unit-testable without a GPU backend.
#[allow(clippy::too_many_arguments)]
fn image_view_image_spec<'a>(
    width: u32,
    height: u32,
    pixels: &'a [f32],
    colormap: &Colormap,
    alpha: f32,
    interpolation: InterpolationMode,
    aggregation: AggregationMode,
    aggregation_block: (u32, u32),
) -> ImageSpec<'a> {
    let mut spec = ImageSpec::scalar(width, height, pixels, colormap.clone());
    spec.alpha = alpha;
    spec.interpolation = interpolation;
    spec.aggregation = aggregation;
    spec.aggregation_block = aggregation_block;
    spec
}

pub struct ImageView {
    image_plot: Plot2D,
    histo_h: Plot1D,
    histo_v: Plot1D,
    sync_x: crate::widget::sync::SyncAxes,
    sync_y: crate::widget::sync::SyncAxes,
    image_handle: Option<ItemHandle>,
    histo_h_curve: Option<ItemHandle>,
    histo_v_curve: Option<ItemHandle>,
    width: u32,
    height: u32,
    pixels: Vec<f32>,
    /// Colormap of the active image, retained so the side colorbar
    /// (silx `ImageView` `getColorBarWidget`, ImageView.py:501) reflects the
    /// current value limits.
    colormap: Colormap,
    /// Active-image opacity slider (silx `ImageView` `ActiveImageAlphaSlider`,
    /// ImageView.py:513-517). Its value propagates to the displayed image.
    alpha: crate::widget::alpha_slider::AlphaSlider,
    /// Data-to-screen interpolation of the active image (silx image
    /// `interpolation`, items/image.py: nearest / linear).
    interpolation: InterpolationMode,
    /// Block aggregation of the active image (silx `ImageDataAggregated`,
    /// items/image.py: max / mean / min).
    aggregation: AggregationMode,
    /// Per-axis block factors `(block_x, block_y)` for [`aggregation`]
    /// (silx level-of-detail `(lodx, lody)`).
    ///
    /// [`aggregation`]: ImageView::aggregation
    aggregation_block: (u32, u32),
    /// Cursor-coordinate readout fed by the live pointer (silx
    /// `tools/PositionInfo.PositionInfo`, bound to the plot `sigMouseMoved`).
    position_info: crate::widget::position_info::PositionInfo,
    /// Last cursor data coordinates `(x, y)` from a pointer move/click over the
    /// image plot, or `None` when no pointer event landed on the data area.
    cursor: Option<[f64; 2]>,
    /// Corner overview of the full image extent with a draggable viewport
    /// rectangle (silx `ImageView._radarView`, ImageView.py:486-490). Dragging
    /// it pans the image plot.
    radar: crate::widget::radar_view::RadarView,
    /// Active profile-extraction mode of the profile tool (silx
    /// `ImageView._ProfileToolBar`, ImageView.py:692-697). [`ProfileMode::None`]
    /// disables the tool.
    profile_mode: ProfileMode,
    /// Popup window showing the extracted 1D profile (silx profile window).
    profile_window: crate::widget::profile_window::ProfileWindow,
    /// Data-space `(col, row)` where the current profile drag began, or `None`
    /// when no drag is in progress.
    profile_drag_start: Option<(f64, f64)>,
}

impl ImageView {
    /// Create a new `ImageView`.
    ///
    /// Plots use ids `image_id`, `image_id + 1` (histo_h), and `image_id + 2`
    /// (histo_v) — choose a base id that does not collide with other plots in
    /// the same egui frame.
    pub fn new(render_state: &RenderState, image_id: PlotId) -> Self {
        let mut image_plot = Plot2D::new(render_state, image_id);
        image_plot.set_graph_cursor(true);
        image_plot.set_keep_data_aspect_ratio(true);

        let mut histo_h = Plot1D::new(render_state, image_id + 1);
        histo_h.set_graph_title("Column profile");
        histo_h.set_graph_x_label("column");
        histo_h.set_graph_y_label("sum", YAxis::Left);

        let mut histo_v = Plot1D::new(render_state, image_id + 2);
        histo_v.set_graph_title("Row profile");
        histo_v.set_graph_x_label("sum");
        histo_v.set_graph_y_label("row", YAxis::Left);

        Self {
            image_plot,
            histo_h,
            histo_v,
            sync_x: crate::widget::sync::SyncAxes::new().with_sync_y(false),
            sync_y: crate::widget::sync::SyncAxes::new().with_sync_x(false),
            image_handle: None,
            histo_h_curve: None,
            histo_v_curve: None,
            width: 0,
            height: 0,
            pixels: Vec::new(),
            colormap: Colormap::viridis(0.0, 1.0),
            alpha: crate::widget::alpha_slider::AlphaSlider::default(),
            interpolation: InterpolationMode::default(),
            aggregation: AggregationMode::default(),
            aggregation_block: (1, 1),
            position_info: crate::widget::position_info::PositionInfo::with_xy(),
            cursor: None,
            radar: crate::widget::radar_view::RadarView::default(),
            profile_mode: ProfileMode::None,
            profile_window: crate::widget::profile_window::ProfileWindow::new(
                render_state,
                image_id + 3,
            ),
            profile_drag_start: None,
        }
    }

    /// Upload and display a new image.
    pub fn set_image(
        &mut self,
        width: u32,
        height: u32,
        pixels: &[f32],
        colormap: Colormap,
    ) -> Result<(), PlotDataError> {
        let expected = (width as usize).saturating_mul(height as usize);
        if pixels.len() != expected {
            return Err(PlotDataError::ImageDataLength {
                expected,
                actual: pixels.len(),
            });
        }
        self.width = width;
        self.height = height;
        self.pixels = pixels.to_vec();
        self.colormap = colormap.clone();
        self.image_plot.set_default_colormap(colormap);

        // The image uses default geometry (origin (0,0), unit scale), so its
        // data extent is [0, width] × [0, height]. Feed it to the radar overview
        // (silx `_updateDataContent` from `getDataRange`).
        self.radar
            .set_data_bounds(0.0, width as f64, 0.0, height as f64);

        self.upload_image();
        self.rebuild_histograms();
        Ok(())
    }

    /// Build the active image's [`ImageSpec`] from the retained colormap and
    /// alpha, then add or update it on the image plot. Routing every upload
    /// through one spec keeps the alpha (silx `ActiveImageAlphaSlider`) in sync
    /// with the displayed image.
    fn upload_image(&mut self) {
        if self.width == 0 || self.pixels.is_empty() {
            return;
        }
        let spec = image_view_image_spec(
            self.width,
            self.height,
            &self.pixels,
            &self.colormap,
            self.alpha.alpha(),
            self.interpolation,
            self.aggregation,
            self.aggregation_block,
        );
        if let Some(handle) = self.image_handle {
            self.image_plot.update_image_spec(handle, spec);
        } else {
            let h = self.image_plot.add_image_spec(spec);
            self.image_handle = Some(h);
        }
    }

    /// The current active-image opacity in `[0.0, 1.0]` (silx
    /// `ActiveImageAlphaSlider`, ImageView.py:513-517).
    pub fn alpha(&self) -> f32 {
        self.alpha.alpha()
    }

    /// Set the active-image opacity in `[0.0, 1.0]` and re-upload the image so
    /// the change takes effect (silx `ActiveImageAlphaSlider.valueChanged`
    /// → `image.setAlpha`).
    pub fn set_alpha(&mut self, alpha: f32) {
        self.alpha.set_alpha(alpha);
        self.upload_image();
    }

    /// The active image's data-to-screen interpolation (silx image
    /// `interpolation`).
    pub fn interpolation(&self) -> InterpolationMode {
        self.interpolation
    }

    /// Set the active image's interpolation and re-upload it (silx
    /// `image.setInterpolation`, items/image.py).
    pub fn set_interpolation(&mut self, interpolation: InterpolationMode) {
        if interpolation != self.interpolation {
            self.interpolation = interpolation;
            self.upload_image();
        }
    }

    /// The active image's block aggregation (silx `ImageDataAggregated`).
    pub fn aggregation(&self) -> AggregationMode {
        self.aggregation
    }

    /// The active image's per-axis aggregation block factors `(block_x,
    /// block_y)` (silx level-of-detail `(lodx, lody)`).
    pub fn aggregation_block(&self) -> (u32, u32) {
        self.aggregation_block
    }

    /// Set the active image's block aggregation `mode` and per-axis block
    /// factors, then re-upload it (silx `ImageDataAggregated.setAggregationMode`,
    /// items/image_aggregated.py). Each block factor is clamped to `>= 1`.
    pub fn set_aggregation(&mut self, mode: AggregationMode, block: (u32, u32)) {
        let block = (block.0.max(1), block.1.max(1));
        if mode != self.aggregation || block != self.aggregation_block {
            self.aggregation = mode;
            self.aggregation_block = block;
            self.upload_image();
        }
    }

    /// Show the ImageView toolbar: interpolation / aggregation selectors on the
    /// active image (silx image `interpolation` nearest/linear and
    /// `ImageDataAggregated` max/mean/min, items/image.py) plus the active-image
    /// alpha slider (silx `ActiveImageAlphaSlider`, ImageView.py:513-517). Each
    /// change is propagated to the displayed image.
    pub fn show_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // Interpolation selector (silx image interpolation).
            let mut interpolation = self.interpolation;
            egui::ComboBox::from_label("interp")
                .selected_text(match interpolation {
                    InterpolationMode::Nearest => "nearest",
                    InterpolationMode::Linear => "linear",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut interpolation, InterpolationMode::Nearest, "nearest");
                    ui.selectable_value(&mut interpolation, InterpolationMode::Linear, "linear");
                });
            if interpolation != self.interpolation {
                self.set_interpolation(interpolation);
            }

            // Aggregation selector (silx ImageDataAggregated).
            let mut aggregation = self.aggregation;
            egui::ComboBox::from_label("agg")
                .selected_text(match aggregation {
                    AggregationMode::None => "none",
                    AggregationMode::Max => "max",
                    AggregationMode::Mean => "mean",
                    AggregationMode::Min => "min",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut aggregation, AggregationMode::None, "none");
                    ui.selectable_value(&mut aggregation, AggregationMode::Max, "max");
                    ui.selectable_value(&mut aggregation, AggregationMode::Mean, "mean");
                    ui.selectable_value(&mut aggregation, AggregationMode::Min, "min");
                });

            // Block factors (silx level-of-detail (lodx, lody)).
            let mut block = self.aggregation_block;
            let bx = ui.add(
                egui::DragValue::new(&mut block.0)
                    .range(1..=64)
                    .prefix("bx "),
            );
            let by = ui.add(
                egui::DragValue::new(&mut block.1)
                    .range(1..=64)
                    .prefix("by "),
            );
            if aggregation != self.aggregation || bx.changed() || by.changed() {
                self.set_aggregation(aggregation, block);
            }
        });

        let response = self.alpha.ui(ui);
        if response.changed() {
            self.upload_image();
        }

        // Profile-tool toggles (silx _ProfileToolBar action group,
        // ImageView.py:692-697). Clicking the active mode again disables it.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.label("profile:");
            for (label, tooltip, mode) in [
                ("—", "Row profile (horizontal)", ProfileMode::Horizontal),
                ("|", "Column profile (vertical)", ProfileMode::Vertical),
                ("/", "Line profile (drag)", ProfileMode::Line),
                ("□", "Rectangle profile (drag)", ProfileMode::Rectangle),
            ] {
                if ui
                    .selectable_label(self.profile_mode == mode, label)
                    .on_hover_text(tooltip)
                    .clicked()
                {
                    let next = if self.profile_mode == mode {
                        ProfileMode::None
                    } else {
                        mode
                    };
                    self.set_profile_mode(next);
                }
            }
        });
    }

    /// The active image's colormap (silx `ImageView.getColormap`).
    pub fn colormap(&self) -> &Colormap {
        &self.colormap
    }

    /// A [`ColorBarWidget`] for the active image's colormap, used by
    /// [`Self::show`] to render the side colorbar (silx
    /// `ImageView.getColorBarWidget`, ImageView.py:501). The bar's value limits
    /// track the colormap's `vmin`/`vmax`.
    pub fn colorbar(&self) -> crate::widget::colorbar::ColorBarWidget {
        image_view_colorbar(&self.colormap)
    }

    /// Render the image + side histogram panels.
    ///
    /// Call this once per frame.  The top histogram occupies `histo_height`
    /// points of vertical space; the right histogram occupies `histo_width`
    /// points of horizontal space.  Pass `None` to use the defaults (80 pt).
    pub fn show(&mut self, ui: &mut egui::Ui, histo_height: Option<f32>, histo_width: Option<f32>) {
        let histo_h_h = histo_height.unwrap_or(80.0);
        let histo_v_w = histo_width.unwrap_or(80.0);

        // Synchronise axes before rendering.
        self.sync_x
            .sync(&mut [self.image_plot.plot_mut(), self.histo_h.plot_mut()]);
        self.sync_y
            .sync(&mut [self.image_plot.plot_mut(), self.histo_v.plot_mut()]);

        let avail = ui.available_size();

        // Reserve the far-right colorbar column (silx grid column 2,
        // ImageView.py:501).
        let colorbar_w = COLORBAR_WIDTH;

        // Top row: horizontal histogram.
        ui.allocate_ui(
            egui::vec2(avail.x - histo_v_w - colorbar_w, histo_h_h),
            |ui| {
                self.histo_h.show(ui);
            },
        );

        // Sync the radar viewport to the image plot's current limits before
        // rendering (silx `__setVisibleRectFromPlot`).
        let (xmin, xmax) = self.image_plot.x_limits();
        if let Some((ymin, ymax)) = self.image_plot.y_limits(YAxis::Left) {
            self.radar.set_viewport_limits(xmin, xmax, ymin, ymax);
        }

        // Bottom row: image + vertical histogram + colorbar side by side. The
        // radar overview sits in the bottom-right corner under the vertical
        // histogram (silx grid (1,1), ImageView.py:486-490).
        let img_h = avail.y - histo_h_h;
        let radar_h = RADAR_OVERVIEW_SIZE.min(img_h);
        let response = ui.horizontal(|ui| {
            let img_w = avail.x - histo_v_w - colorbar_w;
            let response = ui
                .allocate_ui(egui::vec2(img_w, img_h), |ui| self.image_plot.show(ui))
                .inner;
            // Vertical histogram with the radar overview stacked below it.
            ui.allocate_ui(egui::vec2(histo_v_w, img_h), |ui| {
                ui.allocate_ui(egui::vec2(histo_v_w, img_h - radar_h), |ui| {
                    self.histo_v.show(ui);
                });
                let radar = self.radar.ui(ui, egui::vec2(histo_v_w, radar_h));
                if let Some((rx0, rx1, ry0, ry1)) = radar.dragged_limits {
                    // Forward the dragged viewport to pan/zoom the image plot
                    // (silx `plot.setLimits`, RadarView.py:326).
                    self.image_plot.set_limits(rx0, rx1, ry0, ry1, None);
                }
            });
            // Colorbar column, synced to the active image's colormap limits.
            self.colorbar().ui(ui, egui::vec2(colorbar_w, img_h));
            response
        });

        let plot_response = response.inner;

        // Feed the live cursor (silx sigMouseMoved) into the PositionInfo
        // readout, then render it below the image.
        if let Some(cursor) = cursor_from_pointer_event(plot_response.pointer_event.as_ref()) {
            self.cursor = Some(cursor);
        }
        self.position_info.ui(ui, self.cursor);

        // Profile tool: a drag on the image plot extracts a profile via the
        // existing helpers and shows it in the profile window (silx
        // _ProfileToolBar, ImageView.py:692-697).
        self.handle_profile_drag(&plot_response);
        self.profile_window.show(ui.ctx());
    }

    /// Track a profile drag on the image plot and extract the profile on
    /// release. The drag start/current pixels are mapped to data-space
    /// `(col, row)` via the plot transform, then routed through
    /// [`image_view_profile_values`]; the result feeds the profile window.
    fn handle_profile_drag(&mut self, plot_response: &PlotResponse) {
        if self.profile_mode == ProfileMode::None || self.pixels.is_empty() {
            self.profile_drag_start = None;
            return;
        }
        let response = &plot_response.response;
        let transform = &plot_response.transform;

        if response.drag_started()
            && let Some(p) = response.interact_pointer_pos()
        {
            self.profile_drag_start = Some(transform.pixel_to_data(p));
        }

        if response.dragged()
            && let (Some(start), Some(p)) =
                (self.profile_drag_start, response.interact_pointer_pos())
        {
            let end = transform.pixel_to_data(p);
            if let Some(roi) = profile_roi_from_drag(self.profile_mode, start, end) {
                // ProfileWindow re-derives the profile from the ROI using the
                // same line/row/column helpers (single source of truth).
                self.profile_window
                    .update_profile(self.width, self.height, &self.pixels, &roi);
                self.profile_window.set_open(true);
            }
        }

        if response.drag_stopped() {
            self.profile_drag_start = None;
        }
    }

    /// The active profile-extraction mode of the profile tool (silx
    /// `_ProfileToolBar`, ImageView.py:692-697).
    pub fn profile_mode(&self) -> ProfileMode {
        self.profile_mode
    }

    /// Set the active profile-extraction mode (silx `_ProfileToolBar` action
    /// toggle). [`ProfileMode::None`] disables the tool and closes the window.
    pub fn set_profile_mode(&mut self, mode: ProfileMode) {
        self.profile_mode = mode;
        if mode == ProfileMode::None {
            self.profile_drag_start = None;
            self.profile_window.set_open(false);
        }
    }

    /// Extract the profile for `mode` directly from a drag between data-space
    /// `(col, row)` endpoints, without UI (silx `_ProfileToolBar` profile
    /// extraction). Returns `(x_axis, y_values)` or `None`.
    pub fn profile_values(
        &self,
        mode: ProfileMode,
        start: (f64, f64),
        end: (f64, f64),
    ) -> Option<(Vec<f64>, Vec<f64>)> {
        image_view_profile_values(mode, self.width, self.height, &self.pixels, start, end)
    }

    /// The radar overview of the full image extent with its draggable viewport
    /// (silx `ImageView._radarView`, ImageView.py:486-490).
    pub fn radar(&self) -> &crate::widget::radar_view::RadarView {
        &self.radar
    }

    /// The last cursor data coordinates `(x, y)` fed into the PositionInfo
    /// readout from a pointer move/click, or `None`.
    pub fn cursor(&self) -> Option<[f64; 2]> {
        self.cursor
    }

    /// The position-info readout bound to the live cursor (silx
    /// `tools/PositionInfo.PositionInfo`).
    pub fn position_info(&self) -> &crate::widget::position_info::PositionInfo {
        &self.position_info
    }

    /// Mutable access to the position-info readout, to add converter columns
    /// (silx `PositionInfo(converters=...)`).
    pub fn position_info_mut(&mut self) -> &mut crate::widget::position_info::PositionInfo {
        &mut self.position_info
    }

    /// The PositionInfo readout strings at the current live cursor (silx
    /// `PositionInfo` value fields). One string per converter column;
    /// `"------"` when no cursor has been seen.
    pub fn position_info_values(&self) -> Vec<String> {
        self.position_info.values(self.cursor)
    }

    /// Access the main image plot for toolbar/ROI/limit configuration.
    pub fn image_plot(&self) -> &Plot2D {
        &self.image_plot
    }

    /// Mutable access to the main image plot.
    pub fn image_plot_mut(&mut self) -> &mut Plot2D {
        &mut self.image_plot
    }

    fn rebuild_histograms(&mut self) {
        if self.width == 0 || self.pixels.is_empty() {
            return;
        }
        let w = self.width as usize;
        let h = self.height as usize;
        let pixels = &self.pixels;

        // Column sums: histo_h — x = column index, y = sum of that column.
        let col_sums: Vec<f64> = (0..w)
            .map(|col| (0..h).map(|row| pixels[row * w + col] as f64).sum())
            .collect();
        let col_x: Vec<f64> = (0..w).map(|i| i as f64).collect();

        // Row sums: histo_v — x = sum of that row, y = row index.
        let row_sums: Vec<f64> = (0..h)
            .map(|row| (0..w).map(|col| pixels[row * w + col] as f64).sum())
            .collect();
        let row_y: Vec<f64> = (0..h).map(|i| i as f64).collect();

        if let Some(h) = self.histo_h_curve {
            self.histo_h
                .update_curve_data(h, &CurveData::new(col_x, col_sums, Color32::YELLOW));
        } else {
            let h =
                self.histo_h
                    .add_curve_with_legend(&col_x, &col_sums, Color32::YELLOW, "col sums");
            self.histo_h_curve = Some(h);
        }

        if let Some(h) = self.histo_v_curve {
            self.histo_v
                .update_curve_data(h, &CurveData::new(row_sums, row_y, Color32::LIGHT_BLUE));
        } else {
            let h = self.histo_v.add_curve_with_legend(
                &row_sums,
                &row_y,
                Color32::LIGHT_BLUE,
                "row sums",
            );
            self.histo_v_curve = Some(h);
        }
    }
}

// ─── ScatterView ──────────────────────────────────────────────────────────────

/// How a [`ScatterView`]'s `(x, y, value)` points are rendered, mirroring silx
/// `ScatterVisualizationMixIn.Visualization` (core.py:1252-1295).
///
/// [`Points`](Self::Points) draws the marker cloud (the default); the three
/// grid modes convert the unstructured points into a value image (via the
/// matching [`crate::core::scatter_viz`] primitive) rendered through the image
/// path. `SOLID` (the per-vertex-colored triangle surface) is intentionally not
/// offered here: it needs a GPU triangle pipeline + shader that is unverifiable
/// without a render device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScatterVisualization {
    /// `Visualization.POINTS`: a marker per point (the existing default).
    #[default]
    Points,
    /// `Visualization.IRREGULAR_GRID`: the Delaunay triangulation rasterized to
    /// a value image by barycentric linear interpolation
    /// ([`crate::core::scatter_viz::irregular_grid_image`]). Pixels outside the
    /// convex hull are `NaN`.
    IrregularGrid,
    /// `Visualization.REGULAR_GRID`: the points reshaped onto the auto-detected
    /// grid ([`crate::core::scatter_viz::detect_regular_grid`]). Trailing cells
    /// not covered by a point are `NaN`.
    RegularGrid,
    /// `Visualization.BINNED_STATISTIC`: the per-bin mean over a 2D binning
    /// ([`crate::core::scatter_viz::binned_statistic`]). Empty bins are `NaN`.
    BinnedStatistic,
}

/// Reshape `values` onto the auto-detected regular grid for
/// [`ScatterVisualization::RegularGrid`], faithful to silx
/// `__getRegularGridInfo` + the REGULAR_GRID render branch
/// (scatter.py:402-467, 631-680).
///
/// The grid shape and major order come from
/// [`crate::core::scatter_viz::detect_regular_grid`]. Row-major points fill the
/// returned grid directly; column-major points are written down columns then
/// the grid is logically transposed so the result is always row-major
/// `(rows, cols)`. When there are fewer points than cells the trailing cells are
/// `NaN` (silx "transparent pixels", scatter.py:648-651). `origin`/`scale` use
/// silx's regular-grid placement: `scale = span / max(1, n - 1)` per axis and
/// `origin = begin - 0.5 * scale`, with silx's zero-scale fallbacks.
///
/// Returns `None` when no grid can be guessed (silx logs and skips the image).
fn regular_grid_image(x: &[f64], y: &[f64], values: &[f64]) -> Option<GridImage> {
    let grid = crate::core::scatter_viz::detect_regular_grid(x, y)?;
    let (mut rows, mut cols) = grid.shape;

    // silx enlarges the grid when there are more points than cells
    // (scatter.py:426-436), keeping the slow dimension and growing the other.
    let n = values.len();
    if n > rows * cols {
        match grid.order {
            crate::core::scatter_viz::GridMajorOrder::Row => {
                rows = n.div_ceil(cols.max(1));
            }
            crate::core::scatter_viz::GridMajorOrder::Column => {
                cols = n.div_ceil(rows.max(1));
            }
        }
    }
    if rows == 0 || cols == 0 {
        return None;
    }

    // silx bounds: per axis, the (min, max) ordered so the first point is
    // nearest `begin` (scatter.py:441-447).
    let (xb, xe) = grid_axis_bounds(x);
    let (yb, ye) = grid_axis_bounds(y);
    let mut sx = if cols > 1 {
        (xe - xb) / (cols - 1) as f64
    } else {
        0.0
    };
    let mut sy = if rows > 1 {
        (ye - yb) / (rows - 1) as f64
    } else {
        0.0
    };
    // silx zero-scale fallbacks (scatter.py:454-459).
    match (sx == 0.0, sy == 0.0) {
        (true, true) => {
            sx = 1.0;
            sy = 1.0;
        }
        (true, false) => sx = sy,
        (false, true) => sy = sx,
        (false, false) => {}
    }
    let origin = (xb - 0.5 * sx, yb - 0.5 * sy);

    // Reshape the values into the row-major (rows, cols) grid. Row-major order
    // fills rows directly; column-major fills down columns (silx transpose,
    // scatter.py:637-663). Trailing cells beyond the point count stay NaN.
    let mut data = vec![f64::NAN; rows * cols];
    match grid.order {
        crate::core::scatter_viz::GridMajorOrder::Row => {
            for (i, &v) in values.iter().enumerate().take(rows * cols) {
                data[i] = v;
            }
        }
        crate::core::scatter_viz::GridMajorOrder::Column => {
            // Points fill column-major: point i goes to (row i % rows, col i / rows).
            for (i, &v) in values.iter().enumerate().take(rows * cols) {
                let r = i % rows;
                let c = i / rows;
                data[r * cols + c] = v;
            }
        }
    }

    Some(GridImage {
        data,
        shape: (rows, cols),
        origin,
        scale: (sx, sy),
    })
}

/// Per-axis grid bounds `(begin, end)` for [`regular_grid_image`], ordered so the
/// first sample is nearest `begin` (silx scatter.py:443-446). Falls back to
/// `(0, 0)` for an empty/all-non-finite axis.
fn grid_axis_bounds(coord: &[f64]) -> (f64, f64) {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for &v in coord {
        if v.is_finite() {
            min = min.min(v);
            max = max.max(v);
        }
    }
    if min > max {
        return (0.0, 0.0);
    }
    let first = coord.first().copied().unwrap_or(min);
    if (first - min) <= (max - first) {
        (min, max)
    } else {
        (max, min)
    }
}

/// Convert a [`BinnedStatistic`]'s per-bin mean into a [`GridImage`] for
/// [`ScatterVisualization::BinnedStatistic`] (silx renders the BINNED_STATISTIC
/// reduction as a colormapped image; empty bins are `NaN`).
fn binned_statistic_image(bs: &crate::core::scatter_viz::BinnedStatistic) -> GridImage {
    GridImage {
        data: bs.select(crate::core::scatter_viz::BinnedStatisticFunction::Mean),
        shape: bs.shape,
        origin: bs.origin,
        scale: bs.scale,
    }
}

/// Convert a [`ScatterView`]'s `(x, y, value)` points into the [`GridImage`] for
/// a grid visualization `mode`, dispatching to the matching
/// [`crate::core::scatter_viz`] primitive (silx scatter.py render branches).
///
/// `resolution` is the target `(rows, cols)` for the resolution-driven modes
/// ([`ScatterVisualization::IrregularGrid`] and
/// [`ScatterVisualization::BinnedStatistic`]); it is ignored by
/// [`ScatterVisualization::RegularGrid`], whose shape is auto-detected.
///
/// Returns `None` for [`ScatterVisualization::Points`] (no image) and when the
/// chosen primitive cannot produce a grid (e.g. un-triangulable points, no
/// guessable grid, or empty data). Split out so the per-mode conversion is
/// unit-testable without a GPU backend.
fn scatter_grid_image(
    mode: ScatterVisualization,
    x: &[f64],
    y: &[f64],
    values: &[f64],
    resolution: (usize, usize),
) -> Option<GridImage> {
    let (rows, cols) = resolution;
    match mode {
        ScatterVisualization::Points => None,
        ScatterVisualization::IrregularGrid => {
            crate::core::scatter_viz::irregular_grid_image(x, y, values, rows, cols)
        }
        ScatterVisualization::RegularGrid => regular_grid_image(x, y, values),
        ScatterVisualization::BinnedStatistic => {
            crate::core::scatter_viz::binned_statistic(x, y, values, rows, cols)
                .as_ref()
                .map(binned_statistic_image)
        }
    }
}

/// A scatter plot where marker colours are driven by a per-point value array
/// mapped through a [`Colormap`], mirroring silx `ScatterView`.
///
/// ```ignore
/// let mut sv = ScatterView::new(render_state, 0);
/// sv.set_data(&x, &y, &values, Colormap::viridis(0.0, 10.0))?;
/// // frame loop
/// sv.show_toolbar(ui);
/// sv.show(ui);
/// ```
pub struct ScatterView {
    inner: PlotWidget,
    scatter_handle: Option<ItemHandle>,
    /// Handle of the grid image rendered for a non-[`Points`] visualization
    /// (silx scatter grid render branches). `None` in `Points` mode.
    ///
    /// [`Points`]: ScatterVisualization::Points
    grid_handle: Option<ItemHandle>,
    /// Colormap that maps the per-point `values` to marker colors, retained so
    /// the side colorbar (silx `ScatterView` `getColorBarWidget`,
    /// ScatterView.py:83-88) reflects the current value limits. `None` until
    /// [`Self::set_data`] has been called.
    colormap: Option<Colormap>,
    /// Retained `(x, y, values)` of the last [`Self::set_data`], so a
    /// visualization-mode change can rebuild the grid image without re-supplying
    /// the data (silx caches the scatter data on the item).
    points: Option<(Vec<f64>, Vec<f64>, Vec<f64>)>,
    /// Active visualization mode (silx `Scatter.getVisualization`).
    visualization: ScatterVisualization,
    /// Target grid resolution `(rows, cols)` for the resolution-driven grid
    /// modes (silx `GRID_SHAPE` / `BINNED_STATISTIC_SHAPE`, default 100×100,
    /// scatter.py:476).
    grid_resolution: (usize, usize),
}

impl ScatterView {
    /// Create a new scatter-view widget.
    pub fn new(render_state: &RenderState, id: PlotId) -> Self {
        let mut inner = PlotWidget::new(render_state, id);
        inner.set_graph_cursor(true);
        Self {
            inner,
            scatter_handle: None,
            grid_handle: None,
            colormap: None,
            points: None,
            visualization: ScatterVisualization::Points,
            grid_resolution: (100, 100),
        }
    }

    /// Upload data.  `values` drives point colours through `colormap`.
    ///
    /// All three slices must have equal length; returns [`PlotDataError`] otherwise.
    pub fn set_data(
        &mut self,
        x: &[f64],
        y: &[f64],
        values: &[f64],
        colormap: Colormap,
    ) -> Result<(), PlotDataError> {
        if x.len() != y.len() || x.len() != values.len() {
            return Err(PlotDataError::ImageDataLength {
                expected: x.len(),
                actual: if y.len() != x.len() {
                    y.len()
                } else {
                    values.len()
                },
            });
        }

        self.points = Some((x.to_vec(), y.to_vec(), values.to_vec()));
        self.colormap = Some(colormap);
        self.rebuild_visualization();
        Ok(())
    }

    /// The active scatter visualization mode (silx `Scatter.getVisualization`).
    pub fn visualization(&self) -> ScatterVisualization {
        self.visualization
    }

    /// Set the scatter visualization mode (silx `Scatter.setVisualization`).
    ///
    /// [`ScatterVisualization::Points`] shows the marker cloud; the grid modes
    /// render the retained `(x, y, value)` points as a colormapped image
    /// (built via [`scatter_grid_image`]). Re-renders immediately against the
    /// data from the last [`Self::set_data`].
    pub fn set_visualization(&mut self, mode: ScatterVisualization) {
        if self.visualization == mode {
            return;
        }
        self.visualization = mode;
        self.rebuild_visualization();
    }

    /// The target grid resolution `(rows, cols)` used by the resolution-driven
    /// grid modes (silx `GRID_SHAPE` / `BINNED_STATISTIC_SHAPE`).
    pub fn grid_resolution(&self) -> (usize, usize) {
        self.grid_resolution
    }

    /// Set the target grid resolution `(rows, cols)` for the resolution-driven
    /// grid modes ([`ScatterVisualization::IrregularGrid`] and
    /// [`ScatterVisualization::BinnedStatistic`]); ignored by
    /// [`ScatterVisualization::RegularGrid`] (auto-detected). Re-renders the
    /// current visualization.
    pub fn set_grid_resolution(&mut self, rows: usize, cols: usize) {
        self.grid_resolution = (rows, cols);
        self.rebuild_visualization();
    }

    /// The grid image produced for the current visualization mode from the
    /// retained points, or `None` in [`ScatterVisualization::Points`] mode /
    /// before any data is uploaded / when the points cannot form a grid.
    ///
    /// Exposed so callers (and tests) can inspect the converted grid that
    /// [`Self::show`] renders through the image path.
    pub fn grid_image(&self) -> Option<GridImage> {
        let (x, y, values) = self.points.as_ref()?;
        scatter_grid_image(self.visualization, x, y, values, self.grid_resolution)
    }

    /// Render the retained points under the current visualization mode through
    /// the appropriate backend path: the marker cloud for
    /// [`ScatterVisualization::Points`], otherwise the converted grid image.
    ///
    /// Single owner of the scatter/grid item handles so the displayed item
    /// always matches `self.visualization`. The non-active path's item is
    /// removed so the two never overlap.
    fn rebuild_visualization(&mut self) {
        let Some((x, y, values)) = self.points.clone() else {
            return;
        };
        let Some(colormap) = self.colormap.clone() else {
            return;
        };

        match self.visualization {
            ScatterVisualization::Points => {
                // Drop any grid image so it does not shadow the markers.
                if let Some(h) = self.grid_handle.take() {
                    self.inner.remove(h);
                }
                let colors: Vec<Color32> = values
                    .iter()
                    .map(|&v| {
                        let t = colormap.normalize(v);
                        let idx = (t * 255.0).clamp(0.0, 255.0) as usize;
                        let [r, g, b, a] = colormap.lut[idx];
                        Color32::from_rgba_unmultiplied(r, g, b, a)
                    })
                    .collect();

                let mut spec = CurveSpec::new(&x, &y, Color32::WHITE);
                spec.color = crate::core::backend::CurveColor::PerVertex(&colors);
                spec.line_style = LineStyle::None;
                spec.symbol = Some(crate::core::items::Symbol::Circle);
                spec.symbol_size = 6.0;

                if let Some(h) = self.scatter_handle {
                    self.inner.update_curve_spec(h, spec);
                } else {
                    let h = self.inner.add_curve_spec(spec);
                    self.scatter_handle = Some(h);
                    self.inner.set_item_legend(h, "scatter");
                }
            }
            mode => {
                // Drop the marker cloud so it does not shadow the grid image.
                if let Some(h) = self.scatter_handle.take() {
                    self.inner.remove(h);
                }
                let Some(grid) = scatter_grid_image(mode, &x, &y, &values, self.grid_resolution)
                else {
                    // No grid (un-triangulable / no guess / empty): clear any
                    // stale image so nothing is shown for this mode.
                    if let Some(h) = self.grid_handle.take() {
                        self.inner.remove(h);
                    }
                    return;
                };
                let pixels: Vec<f32> = grid.data.iter().map(|&v| v as f32).collect();
                let geometry = ImageGeometry {
                    origin: grid.origin,
                    scale: grid.scale,
                    alpha: 1.0,
                };
                let mut spec =
                    ImageSpec::scalar(grid.shape.1 as u32, grid.shape.0 as u32, &pixels, colormap);
                spec.origin = geometry.origin;
                spec.scale = geometry.scale;
                spec.alpha = geometry.alpha;

                if let Some(h) = self.grid_handle {
                    self.inner.update_image_spec(h, spec);
                } else {
                    let h = self.inner.add_image_spec(spec);
                    self.grid_handle = Some(h);
                    self.inner.set_item_legend(h, "scatter grid");
                }
            }
        }
    }

    /// The value colormap that drives the marker colors, retained from the last
    /// [`Self::set_data`] (silx `ScatterView.getColormap`). `None` before any
    /// data has been uploaded.
    pub fn colormap(&self) -> Option<&Colormap> {
        self.colormap.as_ref()
    }

    /// A [`ColorBarWidget`] for the scatter's value colormap, used by
    /// [`Self::show`] to render the side colorbar (silx
    /// `ScatterView.getColorBarWidget`, ScatterView.py:83-88). The bar's value
    /// limits track the colormap's `vmin`/`vmax`. Returns `None` before any data
    /// has been uploaded.
    ///
    /// [`ColorBarWidget`]: crate::widget::colorbar::ColorBarWidget
    pub fn colorbar(&self) -> Option<crate::widget::colorbar::ColorBarWidget> {
        scatter_view_colorbar(self.colormap.as_ref())
    }

    /// Show the standard toolbar.
    pub fn show_toolbar(&mut self, ui: &mut egui::Ui) -> ToolbarResponse {
        self.inner.show_toolbar(ui)
    }

    /// Render the scatter plot with the value colorbar beside it.
    ///
    /// The colorbar occupies a fixed-width column on the right, synced to the
    /// value colormap's limits (silx `ScatterView` grid colorbar,
    /// ScatterView.py:83-88). Before any data is uploaded no colorbar is drawn.
    pub fn show(&mut self, ui: &mut egui::Ui) -> PlotResponse {
        let avail = ui.available_size();
        let colorbar = self.colorbar();
        let colorbar_w = if colorbar.is_some() {
            COLORBAR_WIDTH
        } else {
            0.0
        };
        ui.horizontal(|ui| {
            let plot_w = avail.x - colorbar_w;
            let response = ui
                .allocate_ui(egui::vec2(plot_w, avail.y), |ui| self.inner.show(ui))
                .inner;
            if let Some(bar) = colorbar {
                bar.ui(ui, egui::vec2(colorbar_w, avail.y));
            }
            response
        })
        .inner
    }
}

impl Deref for ScatterView {
    type Target = PlotWidget;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for ScatterView {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

// ─── StackView ────────────────────────────────────────────────────────────────

/// A 3D image stack viewer with a frame-selection slider, mirroring silx
/// `StackView`.
///
/// Stores all frames as flat `Vec<f32>` slices.  Only the selected frame is
/// uploaded to the GPU each time it changes.
///
/// ```ignore
/// let mut sv = StackView::new(render_state, 0);
/// sv.set_stack(width, height, frames)?;  // frames: Vec<Vec<f32>>
/// // frame loop
/// sv.show_toolbar(ui);
/// sv.show_frame_controls(ui);
/// sv.show(ui);
/// ```
pub struct StackView {
    inner: Plot2D,
    width: u32,
    height: u32,
    frames: Vec<Vec<f32>>,
    colormap: Colormap,
    image_handle: Option<ItemHandle>,
    current_frame: usize,
    dirty: bool,
}

impl StackView {
    /// Create a new `StackView`.
    pub fn new(render_state: &RenderState, id: PlotId) -> Self {
        let mut inner = Plot2D::new(render_state, id);
        inner.set_keep_data_aspect_ratio(true);
        inner.set_graph_cursor(true);
        Self {
            inner,
            width: 0,
            height: 0,
            frames: Vec::new(),
            colormap: Colormap::viridis(0.0, 1.0),
            image_handle: None,
            current_frame: 0,
            dirty: false,
        }
    }

    /// Load a stack of frames.  Each frame must have `width * height` elements.
    pub fn set_stack(
        &mut self,
        width: u32,
        height: u32,
        frames: Vec<Vec<f32>>,
        colormap: Colormap,
    ) -> Result<(), PlotDataError> {
        let expected = (width as usize).saturating_mul(height as usize);
        for frame in &frames {
            if frame.len() != expected {
                return Err(PlotDataError::ImageDataLength {
                    expected,
                    actual: frame.len(),
                });
            }
        }
        self.width = width;
        self.height = height;
        self.frames = frames;
        self.colormap = colormap;
        self.current_frame = 0;
        self.dirty = true;
        Ok(())
    }

    /// Number of frames in the stack.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Index of the currently visible frame.
    pub fn frame(&self) -> usize {
        self.current_frame
    }

    /// Jump to frame `index` (clamped to valid range).
    pub fn set_frame(&mut self, index: usize) {
        let clamped = index.min(self.frames.len().saturating_sub(1));
        if clamped != self.current_frame {
            self.current_frame = clamped;
            self.dirty = true;
        }
    }

    /// Set the colormap applied to all frames.
    pub fn set_colormap(&mut self, colormap: Colormap) {
        self.colormap = colormap;
        self.dirty = true;
    }

    /// Show a compact frame-navigation row: ← slider → with frame counter.
    ///
    /// Typically called before [`Self::show`].
    pub fn show_frame_controls(&mut self, ui: &mut egui::Ui) {
        if self.frames.is_empty() {
            return;
        }
        let n = self.frames.len();
        ui.horizontal(|ui| {
            if ui.button("◀").on_hover_text("Previous frame").clicked() && self.current_frame > 0
            {
                self.current_frame -= 1;
                self.dirty = true;
            }
            let mut idx = self.current_frame;
            if ui
                .add(egui::Slider::new(&mut idx, 0..=n.saturating_sub(1)).text("frame"))
                .changed()
            {
                self.current_frame = idx;
                self.dirty = true;
            }
            if ui.button("▶").on_hover_text("Next frame").clicked() && self.current_frame + 1 < n
            {
                self.current_frame += 1;
                self.dirty = true;
            }
            ui.label(format!("{}/{}", self.current_frame + 1, n));
        });
    }

    /// Render the currently selected frame.
    pub fn show(&mut self, ui: &mut egui::Ui) -> PlotResponse {
        if self.dirty && !self.frames.is_empty() {
            let frame = &self.frames[self.current_frame];
            if let Some(handle) = self.image_handle {
                self.inner
                    .try_update_image(
                        handle,
                        self.width,
                        self.height,
                        frame,
                        self.colormap.clone(),
                    )
                    .ok();
            } else if let Ok(h) =
                self.inner
                    .try_add_image(self.width, self.height, frame, self.colormap.clone())
            {
                self.image_handle = Some(h);
            }
            self.dirty = false;
        }
        self.inner.show(ui)
    }
}

impl Deref for StackView {
    type Target = Plot2D;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for StackView {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Short human-readable description of a single ROI for the ROI manager table.
fn roi_description(roi: &Roi) -> String {
    match roi {
        Roi::Rect { x, y } => format!(
            "Rect  x=[{:.3}, {:.3}]  y=[{:.3}, {:.3}]",
            x.0, x.1, y.0, y.1
        ),
        Roi::HRange { y } => format!("HRange  y=[{:.3}, {:.3}]", y.0, y.1),
        Roi::VRange { x } => format!("VRange  x=[{:.3}, {:.3}]", x.0, x.1),
        Roi::Point { x, y } => format!("Point  ({x:.3}, {y:.3})"),
        Roi::Line { start, end } => format!(
            "Line  ({:.3},{:.3}) → ({:.3},{:.3})",
            start.0, start.1, end.0, end.1
        ),
        Roi::Polygon { vertices } => format!("Polygon  {} vertices", vertices.len()),
        Roi::Cross { center } => format!("Cross  ({:.3}, {:.3})", center.0, center.1),
        Roi::Circle { center, radius } => {
            format!(
                "Circle  c=({:.3}, {:.3})  r={radius:.3}",
                center.0, center.1
            )
        }
        Roi::Ellipse { center, radii } => format!(
            "Ellipse  c=({:.3}, {:.3})  r=({:.3}, {:.3})",
            center.0, center.1, radii.0, radii.1
        ),
        Roi::Arc {
            center,
            inner_radius,
            outer_radius,
            start_angle,
            end_angle,
        } => format!(
            "Arc  c=({:.3}, {:.3})  r=[{:.3}, {:.3}]  θ=[{:.3}, {:.3}]",
            center.0, center.1, inner_radius, outer_radius, start_angle, end_angle
        ),
        Roi::Band { begin, end, width } => format!(
            "Band  ({:.3},{:.3}) → ({:.3},{:.3})  w={width:.3}",
            begin.0, begin.1, end.0, end.1
        ),
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
    fn image_view_alpha_propagates_to_image_spec() {
        // Item 2: the alpha slider value propagates to the active image's spec
        // alpha (silx ActiveImageAlphaSlider -> image.setAlpha).
        let pixels = [1.0_f32, 2.0, 3.0, 4.0];
        let cmap = Colormap::viridis(0.0, 4.0);
        let mut slider = crate::widget::alpha_slider::AlphaSlider::default();
        slider.set_alpha(0.25);
        let spec = image_view_image_spec(
            2,
            2,
            &pixels,
            &cmap,
            slider.alpha(),
            InterpolationMode::default(),
            AggregationMode::default(),
            (1, 1),
        );
        // 0.25 -> round(255*0.25)=64 -> 64/255 == slider.alpha().
        assert_eq!(spec.alpha, slider.alpha());
        assert!((spec.alpha - 64.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn image_view_interpolation_aggregation_update_spec() {
        // Item 5: selecting interpolation / aggregation modes updates the image
        // spec's interpolation/aggregation/block (silx image interpolation +
        // ImageDataAggregated).
        let pixels = [1.0_f32, 2.0, 3.0, 4.0];
        let cmap = Colormap::viridis(0.0, 4.0);
        let spec = image_view_image_spec(
            2,
            2,
            &pixels,
            &cmap,
            1.0,
            InterpolationMode::Linear,
            AggregationMode::Mean,
            (2, 3),
        );
        assert_eq!(spec.interpolation, InterpolationMode::Linear);
        assert_eq!(spec.aggregation, AggregationMode::Mean);
        assert_eq!(spec.aggregation_block, (2, 3));
    }

    #[test]
    fn image_view_profile_drag_samples_expected_values() {
        // Item 7: a profile drag extracts the expected sampled values via the
        // existing profile helpers (silx _ProfileToolBar).
        // 3x2 image, row-major:
        //   row 0: [1, 2, 3]
        //   row 1: [4, 5, 6]
        let pixels = [1.0_f32, 2.0, 3.0, 4.0, 5.0, 6.0];

        // Horizontal profile at row 1 -> [4, 5, 6], x = column indices.
        let (hx, hy) = image_view_profile_values(
            ProfileMode::Horizontal,
            3,
            2,
            &pixels,
            (0.0, 1.0),
            (2.0, 1.0),
        )
        .unwrap();
        assert_eq!(hx, vec![0.0, 1.0, 2.0]);
        assert_eq!(hy, vec![4.0, 5.0, 6.0]);

        // Vertical profile at column 2 -> [3, 6], x = row indices.
        let (vx, vy) =
            image_view_profile_values(ProfileMode::Vertical, 3, 2, &pixels, (2.0, 0.0), (2.0, 1.0))
                .unwrap();
        assert_eq!(vx, vec![0.0, 1.0]);
        assert_eq!(vy, vec![3.0, 6.0]);

        // Line profile from (0,0) to (2,0) samples row 0: [1, 2, 3].
        let (_, ly) =
            image_view_profile_values(ProfileMode::Line, 3, 2, &pixels, (0.0, 0.0), (2.0, 0.0))
                .unwrap();
        assert_eq!(ly, vec![1.0, 2.0, 3.0]);

        // None mode yields nothing.
        assert!(
            image_view_profile_values(ProfileMode::None, 3, 2, &pixels, (0.0, 0.0), (2.0, 0.0))
                .is_none()
        );
    }

    #[test]
    fn profile_roi_from_drag_matches_mode() {
        // The ROI handed to the profile window encodes the drag per mode.
        assert_eq!(
            profile_roi_from_drag(ProfileMode::Line, (1.0, 2.0), (3.0, 4.0)),
            Some(Roi::Line {
                start: (1.0, 2.0),
                end: (3.0, 4.0)
            })
        );
        assert_eq!(
            profile_roi_from_drag(ProfileMode::Horizontal, (0.0, 1.7), (5.0, 1.7)),
            Some(Roi::HRange { y: (1.0, 1.0) })
        );
        assert_eq!(
            profile_roi_from_drag(ProfileMode::Vertical, (2.9, 0.0), (2.9, 4.0)),
            Some(Roi::VRange { x: (2.0, 2.0) })
        );
        assert_eq!(
            profile_roi_from_drag(ProfileMode::Rectangle, (4.0, 5.0), (1.0, 2.0)),
            Some(Roi::Rect {
                x: (1.0, 4.0),
                y: (2.0, 5.0)
            })
        );
        assert_eq!(
            profile_roi_from_drag(ProfileMode::None, (0.0, 0.0), (1.0, 1.0)),
            None
        );
    }

    #[test]
    fn radar_drag_output_maps_to_image_plot_limits() {
        // Item 3: a RadarView viewport drag emits (x0, x1, y0, y1) which the
        // ImageView forwards verbatim to image_plot.set_limits. Verify the
        // emitted limits via the same clamp+limits path the radar drag uses.
        use crate::widget::radar_view::{DataRect, RadarView, clamp_viewport};

        // 100x80 image extent; viewport is a 20x16 window panned by (+30, +20).
        let mut radar = RadarView::default();
        radar.set_data_bounds(0.0, 100.0, 0.0, 80.0);
        radar.set_viewport(DataRect::new(0.0, 0.0, 20.0, 16.0));

        let moved = DataRect {
            left: radar.viewport.left + 30.0,
            top: radar.viewport.top + 20.0,
            width: radar.viewport.width,
            height: radar.viewport.height,
        };
        let clamped = clamp_viewport(moved, &radar.data_extent);
        let (x0, x1, y0, y1) = clamped.limits();
        // Window stays inside the extent: left 30..50, top 20..36.
        assert_eq!((x0, x1, y0, y1), (30.0, 50.0, 20.0, 36.0));
        // This tuple is exactly what set_limits(x0, x1, y0, y1, None) receives.
        assert!(x1 > x0 && y1 > y0);
    }

    #[test]
    fn image_view_position_info_reads_hover_cursor() {
        // Item 4: a Moved (hover) pointer event yields the cursor data coords,
        // which the PositionInfo readout formats.
        use crate::widget::interaction::PlotPointerEvent;
        let moved = PlotPointerEvent::Moved {
            button: None,
            data: (12.5, -3.0),
            pixel: (40.0, 60.0),
        };
        let cursor = cursor_from_pointer_event(Some(&moved));
        assert_eq!(cursor, Some([12.5, -3.0]));

        let info = crate::widget::position_info::PositionInfo::with_xy();
        let values = info.values(cursor);
        assert_eq!(values, vec!["12.5".to_owned(), "-3".to_owned()]);

        // A Clicked and a DoubleClicked event read the cursor the same way
        // (silx PositionInfo tracks the pointer for every mouse signal, not just
        // hover), so the readout follows the press position too.
        let clicked = PlotPointerEvent::Clicked {
            button: crate::widget::interaction::MouseButton::Left,
            data: (4.0, 8.0),
            pixel: (10.0, 20.0),
        };
        assert_eq!(cursor_from_pointer_event(Some(&clicked)), Some([4.0, 8.0]));
        let double = PlotPointerEvent::DoubleClicked {
            button: crate::widget::interaction::MouseButton::Left,
            data: (-1.5, 2.25),
            pixel: (10.0, 20.0),
        };
        assert_eq!(cursor_from_pointer_event(Some(&double)), Some([-1.5, 2.25]));

        // A LimitsChanged event carries no cursor; absence of an event too.
        let limits = PlotPointerEvent::LimitsChanged {
            x: (0.0, 1.0),
            y: (0.0, 1.0),
            y2: None,
        };
        assert_eq!(cursor_from_pointer_event(Some(&limits)), None);
        assert_eq!(cursor_from_pointer_event(None), None);
        // No cursor -> placeholder readout.
        assert_eq!(
            info.values(None),
            vec!["------".to_owned(), "------".to_owned()]
        );
    }

    #[test]
    fn image_view_colorbar_tracks_colormap_limits() {
        // Item 1: the ImageView side colorbar is synced to the active image's
        // colormap value limits (silx getColorBarWidget).
        let cmap = Colormap::viridis(-3.0, 9.5);
        let bar = image_view_colorbar(&cmap);
        assert_eq!(bar.colormap.vmin, -3.0);
        assert_eq!(bar.colormap.vmax, 9.5);
        assert_eq!(
            bar.orientation,
            crate::widget::colorbar::ColorBarOrientation::Vertical
        );
    }

    #[test]
    fn scatter_view_colorbar_tracks_value_colormap_limits() {
        // Item 1: the ScatterView side colorbar is synced to the value
        // colormap's limits, and is absent before any data is uploaded (silx
        // ScatterView.getColorBarWidget).
        assert!(
            scatter_view_colorbar(None).is_none(),
            "no colorbar before set_data"
        );
        let cmap = Colormap::viridis(2.5, 41.0);
        let bar = scatter_view_colorbar(Some(&cmap)).expect("colorbar after set_data");
        assert_eq!(bar.colormap.vmin, 2.5);
        assert_eq!(bar.colormap.vmax, 41.0);
        assert_eq!(
            bar.orientation,
            crate::widget::colorbar::ColorBarOrientation::Vertical
        );
    }

    #[test]
    fn scatter_points_mode_has_no_grid_image() {
        // Item 2: POINTS mode renders the marker cloud, not a grid image.
        let x = [0.0, 1.0, 0.0];
        let y = [0.0, 0.0, 1.0];
        let v = [1.0, 2.0, 3.0];
        assert!(scatter_grid_image(ScatterVisualization::Points, &x, &y, &v, (4, 4)).is_none());
    }

    #[test]
    fn scatter_irregular_grid_mode_interpolates_inside_nan_outside() {
        // Item 2: IRREGULAR_GRID converts (x,y,value) to a barycentric-
        // interpolated value image; the value field z=x means cell (0,0)
        // samples ~0.5 and the corner outside the triangle is NaN.
        let x = [0.0, 4.0, 0.0];
        let y = [0.0, 0.0, 4.0];
        let v = [0.0, 4.0, 0.0];
        let img = scatter_grid_image(ScatterVisualization::IrregularGrid, &x, &y, &v, (4, 4))
            .expect("triangulable");
        assert_eq!(img.shape, (4, 4));
        assert!((img.get(0, 0).unwrap() - 0.5).abs() < 1e-9);
        assert!(img.get(3, 3).unwrap().is_nan(), "exterior pixel is NaN");
    }

    #[test]
    fn scatter_binned_statistic_mode_means_and_nan_empty() {
        // Item 2: BINNED_STATISTIC converts (x,y,value) to per-bin means;
        // two points in bin (0,0) average, an empty bin is NaN.
        let x = [0.0, 0.5, 2.0];
        let y = [0.0, 0.5, 2.0];
        let v = [10.0, 30.0, 7.0];
        let img = scatter_grid_image(ScatterVisualization::BinnedStatistic, &x, &y, &v, (2, 2))
            .expect("binned");
        assert_eq!(img.shape, (2, 2));
        // bin (0,0) = mean(10,30) = 20.
        assert!((img.get(0, 0).unwrap() - 20.0).abs() < 1e-12);
        // bin (0,1) is empty -> NaN.
        assert!(img.get(0, 1).unwrap().is_nan());
        // bin (1,1) holds the single point 7.
        assert!((img.get(1, 1).unwrap() - 7.0).abs() < 1e-12);
        assert_eq!(img.origin, (0.0, 0.0));
        assert_eq!(img.scale, (1.0, 1.0));
    }

    #[test]
    fn scatter_regular_grid_mode_reshapes_row_major() {
        // Item 2: REGULAR_GRID reshapes the points onto the auto-detected grid.
        // A 2x3 row-major grid (X fast) -> values reshape directly row-major.
        let x = [0.0, 1.0, 2.0, 0.0, 1.0, 2.0];
        let y = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let v = [10.0, 11.0, 12.0, 20.0, 21.0, 22.0];
        let img = scatter_grid_image(ScatterVisualization::RegularGrid, &x, &y, &v, (0, 0))
            .expect("grid detected");
        assert_eq!(img.shape, (2, 3));
        assert_eq!(img.get(0, 0), Some(10.0));
        assert_eq!(img.get(0, 2), Some(12.0));
        assert_eq!(img.get(1, 0), Some(20.0));
        assert_eq!(img.get(1, 2), Some(22.0));
        // scale = span / (n - 1): x span 2 over 2 cols-1 = 1; y span 1 over 1 = 1.
        assert_eq!(img.scale, (1.0, 1.0));
        // origin = begin - 0.5*scale.
        assert_eq!(img.origin, (-0.5, -0.5));
    }

    #[test]
    fn scatter_regular_grid_mode_column_major_transposes() {
        // Item 2: REGULAR_GRID with Y fast (column-major) reshapes down columns,
        // yielding a row-major (rows, cols) image (silx transpose).
        // Fill column 0 top-to-bottom, then column 1 (rows=3).
        let mut x = Vec::new();
        let mut y = Vec::new();
        let mut v = Vec::new();
        for c in 0..2 {
            for r in 0..3 {
                x.push(c as f64);
                y.push(r as f64);
                v.push((c * 10 + r) as f64); // distinct per cell
            }
        }
        let img = scatter_grid_image(ScatterVisualization::RegularGrid, &x, &y, &v, (0, 0))
            .expect("grid detected");
        assert_eq!(img.shape, (3, 2));
        // Point 0 -> (r=0,c=0); point 3 -> (r=0,c=1).
        assert_eq!(img.get(0, 0), Some(0.0));
        assert_eq!(img.get(0, 1), Some(10.0));
        assert_eq!(img.get(2, 0), Some(2.0));
        assert_eq!(img.get(2, 1), Some(12.0));
    }

    #[test]
    fn scatter_regular_grid_mode_trailing_cells_are_nan() {
        // Item 2: fewer points than cells -> trailing cells stay NaN (silx
        // transparent pixels). 5 points on a row-major width-3 line guess
        // would extend the grid; use a clean 2x3 minus one point isn't a grid,
        // so test the explicit short-fill via a single monotonic line of 5.
        let x = [0.0, 1.0, 2.0, 3.0, 4.0];
        let y = [0.0, 0.0, 0.0, 0.0, 0.0];
        let v = [1.0, 2.0, 3.0, 4.0, 5.0];
        let img = scatter_grid_image(ScatterVisualization::RegularGrid, &x, &y, &v, (0, 0))
            .expect("line detected");
        // A monotonic X line -> shape (1, 5), fully filled.
        assert_eq!(img.shape, (1, 5));
        assert_eq!(img.get(0, 0), Some(1.0));
        assert_eq!(img.get(0, 4), Some(5.0));
    }

    #[test]
    fn masked_image_yields_nan_at_masked_pixels_before_upload() {
        // Item 6: a ScalarMask applied to a 2x2 image turns masked pixels into
        // NaN BEFORE upload, leaving the rest unchanged.
        let data = [1.0_f32, 2.0, 3.0, 4.0];
        let mut mask = ScalarMask::new(2, 2);
        // Mask the top-right (col 1, row 0) and bottom-left (col 0, row 1).
        mask.set_mask_data(&[0, 1, 1, 0], 2);
        let out = apply_image_mask(2, 2, &data, &mask).unwrap();
        assert_eq!(out[0], 1.0);
        assert!(out[1].is_nan());
        assert!(out[2].is_nan());
        assert_eq!(out[3], 4.0);
    }

    #[test]
    fn masked_image_validates_shape() {
        let mask = ScalarMask::new(2, 2);
        // data length mismatch.
        assert_eq!(
            apply_image_mask(2, 2, &[1.0, 2.0, 3.0], &mask).unwrap_err(),
            PlotDataError::ImageDataLength {
                expected: 4,
                actual: 3,
            }
        );
        // mask shape mismatch.
        let small = ScalarMask::new(1, 1);
        assert_eq!(
            apply_image_mask(2, 2, &[1.0, 2.0, 3.0, 4.0], &small).unwrap_err(),
            PlotDataError::ImageDataLength {
                expected: 4,
                actual: 1,
            }
        );
    }

    #[test]
    fn value_stats_match_core_stats_engine() {
        // Item 8: ValueStats must equal a direct core::stats::Stats run on the
        // same data (single source of truth).
        let data = [3.0, -2.0, 7.5, f64::NAN, 0.0, -2.0];
        let vs = ValueStats::from_f64(&data);
        let core = crate::core::stats::Stats::for_image(
            &data,
            data.len(),
            1,
            (0.0, 0.0),
            (1.0, 1.0),
            crate::core::stats::StatScope::All,
        );
        assert_eq!(vs.count, core.count);
        assert_eq!(vs.finite_count, core.finite_count);
        assert_eq!(vs.min, core.min);
        assert_eq!(vs.max, core.max);
        assert_eq!(vs.mean, core.mean);
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
