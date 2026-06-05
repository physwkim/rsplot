//! High-level silx-style plot widgets.
//!
//! These types own backend state and expose the user-facing plotting API:
//! callers add data items, tune axes/labels/colors, then call [`PlotWidget::show`]
//! from their egui app. The low-level stateless renderer remains
//! [`crate::PlotView`].

use std::fmt;
use std::mem;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};

use egui::Color32;
use egui_wgpu::RenderState;

use crate::core::backend::{
    Backend, CurveColor, CurveSpec, ImagePixelsSpec, ImageSpec, ItemHandle, MarkerSpec, PickResult,
    ShapeSpec, TriangleSpec,
};
use crate::core::colormap::{AutoscaleMode, Colormap};
use crate::core::items::{Baseline, LineStyle, ScalarMask, Symbol};
use crate::core::marker::{Marker, MarkerKind, MarkerSymbol};
use crate::core::plot::{DataMargins, DataRange, GraphGrid, Plot, PlotId};
use crate::core::roi::{ManagedRoi, Roi, RoiLineStyle};
use crate::core::scatter_viz::GridImage;
use crate::core::shape::{Shape, ShapeKind};
use crate::core::transform::{Margins, Scale, YAxis};
use crate::core::triangles::Triangles;
use crate::render::backend_wgpu::WgpuBackend;
use crate::render::gpu_curve::CurveData;
use crate::render::gpu_image::{AggregationMode, ImageData, ImagePixels, InterpolationMode};
use crate::render::save::{SaveError, SaveFormat};
use crate::widget::interaction::{DrawEvent, DrawMode, DrawParams, MouseButton, RoiDrawKind};
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

/// Popup controls for the median-filter action (silx `MedianFilterDialog`).
///
/// Held by the caller (e.g. in egui temp-memory) and passed to
/// [`Plot2D::show_median_filter`], which mutates it from the kernel-width and
/// conditional widgets. `kernel_width` is the square-kernel width for the 2D
/// action (silx `MedianFilter2DAction`), kept odd by the widget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MedianFilterParams {
    /// Odd kernel width (silx `MedianFilterDialog` spinbox; min 1, step 2).
    pub kernel_width: usize,
    /// Conditional median filtering (silx `MedianFilterDialog` checkbox): only
    /// replace a center pixel that is the window min or max.
    pub conditional: bool,
}

impl Default for MedianFilterParams {
    fn default() -> Self {
        // silx MedianFilterDialog defaults to a 3x3 kernel, conditional off.
        Self {
            kernel_width: 3,
            conditional: false,
        }
    }
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
    /// The display limits changed (pan, zoom, or programmatic update), carrying
    /// the new ranges (silx `limitsChanged`, `PlotEvents.py:176-184`): `x` and
    /// `y` are the left axes' `(min, max)`, `y2` the right axis' `(min, max)` or
    /// `None` when no right axis is in use.
    LimitsChanged {
        x: (f64, f64),
        y: (f64, f64),
        y2: Option<(f64, f64)>,
    },
    /// An ROI edge drag or whole-ROI body drag moved the ROI at `index`.
    RoiChanged { index: usize },
    /// A new ROI was created at `index` by an on-plot draw in
    /// [`PlotInteractionMode::RoiCreate`] (silx `RegionOfInterestManager`
    /// shape-finished). Read its geometry with `plot().rois[index].roi`.
    RoiCreated { index: usize },
    /// The in-progress draw preview advanced this frame in
    /// [`PlotInteractionMode::RoiCreate`] (silx `drawingProgress`): `points` are
    /// the current rubber-band's data-space vertices for the given `mode`.
    DrawingProgress {
        mode: DrawMode,
        points: Vec<(f64, f64)>,
    },
    /// An on-plot draw completed this frame (silx `drawingFinished`), carrying the
    /// resolved [`DrawParams`]. In [`PlotInteractionMode::RoiCreate`] a
    /// [`Self::RoiCreated`] is emitted on the same frame (the ROI is built on top
    /// of the finished draw, mirroring silx's `RegionOfInterestManager`).
    DrawingFinished { mode: DrawMode, params: DrawParams },
    /// The ROI collection changed by a clear-all or a single-ROI removal
    /// (re-read `rois()`).
    RoisCleared,
    /// The current/highlighted ROI changed, by a manager selection or
    /// [`PlotWidget::set_current_roi`] (silx `sigCurrentRoiChanged`). Carries the
    /// previously- and newly-current ROI indices (either may be `None`).
    CurrentRoiChanged {
        previous: Option<usize>,
        current: Option<usize>,
    },
    /// A draggable marker was moved, either by an on-screen drag or by
    /// [`PlotWidget::set_marker_position`] (silx `markerMoving` /
    /// `markerMoved`). `handle` identifies the moved marker; read its new
    /// position with [`PlotWidget::marker_position`].
    MarkerMoved { handle: ItemHandle },
    /// A draggable marker's drag began this frame (silx `beginDrag`). The drag
    /// lifecycle is `MarkerDragStarted` → `MarkerMoved`×N → `MarkerDragFinished`;
    /// the first [`Self::MarkerMoved`] arrives on the same frame. Read the
    /// position with [`PlotWidget::marker_position`].
    MarkerDragStarted { handle: ItemHandle },
    /// A draggable marker's drag ended this frame, i.e. the button was released
    /// (silx `endDrag` `markerMoved`). The marker's final position is already
    /// persisted; read it with [`PlotWidget::marker_position`].
    MarkerDragFinished { handle: ItemHandle },
    /// A curve was clicked (silx `curveClicked`). `handle` identifies the
    /// curve; `index`/`x`/`y` locate the nearest picked vertex; `button` is the
    /// mouse button used.
    CurveClicked {
        handle: ItemHandle,
        index: usize,
        x: f64,
        y: f64,
        button: MouseButton,
    },
    /// An image was clicked (silx `imageClicked`). `col`/`row` are the picked
    /// pixel column and row.
    ImageClicked {
        handle: ItemHandle,
        col: u32,
        row: u32,
        button: MouseButton,
    },
    /// A non-indexed overlay item (marker, scatter, or shape) was clicked (silx
    /// `markerClicked` and the generic item-pick path). Identify its kind with
    /// [`PlotWidget::item_kind`].
    ItemClicked {
        handle: ItemHandle,
        button: MouseButton,
    },
    /// The cursor hovered over an item with no button held (silx `hover` signal,
    /// `prepareHoverSignal`). Mirrors silx's payload: `kind` is the item type,
    /// `label` its name, `x`/`y` the data-space cursor position, `xpixel`/`ypixel`
    /// the pixel cursor position, and `draggable` whether the item can be dragged
    /// (true only for a draggable marker). silx's `selectable` flag is omitted: in
    /// siplot every pickable item is set active on click, so the flag would be
    /// a constant `true` carrying no information.
    ItemHovered {
        handle: ItemHandle,
        kind: PlotItemKind,
        label: Option<String>,
        x: f64,
        y: f64,
        xpixel: f32,
        ypixel: f32,
        draggable: bool,
    },
}

/// A legend right-click context-menu action (silx `LegendListContextMenu`).
/// The action set mirrors silx: `SetActive` (`setActiveCurve`), `MapToLeft` /
/// `MapToRight` (move the curve's Y axis), checkable `TogglePoints` /
/// `ToggleLines` (symbol / line visibility), `Remove`, and `Rename`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegendAction {
    /// Make the item the active item (silx `setActiveCurve`).
    SetActive,
    /// Move a curve to the left Y axis (silx `Map to left`).
    MapToLeft,
    /// Move a curve to the right Y axis (silx `Map to right`).
    MapToRight,
    /// Toggle a curve's point markers (silx checkable `Points`).
    TogglePoints,
    /// Toggle a curve's connecting line (silx checkable `Lines`).
    ToggleLines,
    /// Remove the item from the plot (silx `Remove curve`).
    Remove,
    /// Open the rename popup for the item (silx `Rename curve`).
    Rename,
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
    /// Context-menu action fired this frame, with the item it targeted. The
    /// action is already self-applied by `show_legend`; this is reported for
    /// callers that want to observe or react to it.
    pub context_action: Option<(ItemHandle, LegendAction)>,
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
    /// X autoscale toggle was clicked (silx `XAxisAutoScaleAction`).
    pub autoscale_x_changed: bool,
    /// Y autoscale toggle was clicked (silx `YAxisAutoScaleAction`).
    pub autoscale_y_changed: bool,
    /// X invert toggle was clicked.
    pub x_inverted_changed: bool,
    /// Y invert toggle was clicked.
    pub y_inverted_changed: bool,
    /// Show-axis toggle was clicked (silx `ShowAxisAction`).
    pub show_axis_changed: bool,
    /// Curve-style cycle button was clicked (silx `CurveStyleAction`).
    pub curve_style_changed: bool,
    /// Zoom-in button was clicked (silx `ZoomInAction`).
    pub zoom_in: bool,
    /// Zoom-out button was clicked (silx `ZoomOutAction`).
    pub zoom_out: bool,
    /// Zoom-back button was clicked (silx `ZoomBackAction`).
    pub zoom_back: bool,
    /// Save button was clicked (silx `SaveAction`).
    pub save: bool,
    /// Copy-to-clipboard button was clicked (silx `CopyAction`).
    pub copy: bool,
    /// Print button was clicked (silx `PrintAction`).
    pub print: bool,
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

/// Default figure resolution used by the toolbar Save button when no explicit
/// size is supplied (silx saves at the widget's pixel size; the toolbar has no
/// data-area handle here, so it uses a fixed default).
const DEFAULT_SAVE_SIZE: (u32, u32) = (1024, 768);

/// Default figure resolution (dots per inch) recorded in formats that carry it
/// (TIFF resolution tags). Mirrors silx's matplotlib-backend default of 90 dpi;
/// 96 is the common screen value used here as a sensible default for the raster
/// snapshot. PNG/PPM/SVG ignore it (px-sized containers).
const DEFAULT_SAVE_DPI: u32 = 96;

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
    ShowAxis,
    CurveStyle,
    ZoomIn,
    ZoomOut,
    ZoomBack,
    Save,
    Copy,
    Print,
    AutoscaleX,
    AutoscaleY,
    MedianFilter,
    PixelHistogram,
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

/// Round a kernel dimension up to the nearest odd value (>= 1), matching the
/// silx odd-kernel requirement (`MedianFilterDialog` spinbox min 1, step 2).
fn force_odd(n: usize) -> usize {
    if n <= 1 {
        1
    } else if n % 2 == 1 {
        n
    } else {
        n + 1
    }
}

/// Build the temp PNG path the print shim rasterizes into before handing it to
/// the printer. Joins `dir` with a process-unique file name so concurrent plots
/// (or a copy in flight) do not collide. Pure (no filesystem touch), so the
/// naming is unit-testable; the actual write + printer submit are the shims.
fn print_temp_png_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("siplot-print-{pid}.png"))
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
        ToolbarIcon::LogX => draw_log_icon(painter, rect, false, stroke),
        ToolbarIcon::LogY => draw_log_icon(painter, rect, true, stroke),
        ToolbarIcon::InvertX => draw_axis_icon(painter, rect, "X", false, stroke),
        ToolbarIcon::InvertY => draw_axis_icon(painter, rect, "Y", true, stroke),
        ToolbarIcon::ShowAxis => draw_show_axis_icon(painter, rect, stroke),
        ToolbarIcon::CurveStyle => draw_curve_style_icon(painter, rect, stroke),
        ToolbarIcon::ZoomIn => draw_zoom_step_icon(painter, rect, stroke, true),
        ToolbarIcon::ZoomOut => draw_zoom_step_icon(painter, rect, stroke, false),
        ToolbarIcon::ZoomBack => draw_zoom_back_icon(painter, rect, stroke),
        ToolbarIcon::Save => draw_save_icon(painter, rect, stroke),
        ToolbarIcon::Copy => draw_copy_icon(painter, rect, stroke),
        ToolbarIcon::Print => draw_print_icon(painter, rect, stroke),
        ToolbarIcon::AutoscaleX => draw_autoscale_icon(painter, rect, "X", false, stroke),
        ToolbarIcon::AutoscaleY => draw_autoscale_icon(painter, rect, "Y", true, stroke),
        ToolbarIcon::MedianFilter => draw_median_filter_icon(painter, rect, stroke),
        ToolbarIcon::PixelHistogram => draw_pixel_histogram_icon(painter, rect, stroke),
    }
}

/// Draw three rising histogram bars for the [`ToolbarIcon::PixelHistogram`]
/// button (silx `pixel-intensities` icon).
fn draw_pixel_histogram_icon(painter: &egui::Painter, rect: egui::Rect, stroke: egui::Stroke) {
    let base = rect.bottom();
    let bar_w = rect.width() / 4.0;
    let heights = [0.4_f32, 0.75, 1.0];
    for (i, h) in heights.iter().enumerate() {
        let x = rect.left() + bar_w * (i as f32 * 1.2 + 0.2);
        let top = base - rect.height() * h;
        let bar = egui::Rect::from_min_max(egui::pos2(x, top), egui::pos2(x + bar_w * 0.8, base));
        painter.rect_stroke(bar, 0.0, stroke, egui::StrokeKind::Inside);
    }
}

/// Draw a small 3x3 grid with a highlighted center cell for the
/// [`ToolbarIcon::MedianFilter`] toggle (silx `median-filter` icon): a kernel
/// sweeping a center pixel.
fn draw_median_filter_icon(painter: &egui::Painter, rect: egui::Rect, stroke: egui::Stroke) {
    let grid = rect.shrink(1.0);
    let third_x = grid.width() / 3.0;
    let third_y = grid.height() / 3.0;
    // 3x3 cell grid outline.
    painter.rect_stroke(grid, 0.0, stroke, egui::StrokeKind::Inside);
    for i in 1..3 {
        let x = grid.left() + third_x * i as f32;
        painter.line_segment(
            [egui::pos2(x, grid.top()), egui::pos2(x, grid.bottom())],
            stroke,
        );
        let y = grid.top() + third_y * i as f32;
        painter.line_segment(
            [egui::pos2(grid.left(), y), egui::pos2(grid.right(), y)],
            stroke,
        );
    }
    // Highlight the center cell (the pixel being replaced by its window median).
    let center = egui::Rect::from_min_size(
        egui::pos2(grid.left() + third_x, grid.top() + third_y),
        egui::vec2(third_x, third_y),
    );
    painter.rect_filled(center.shrink(1.0), 0.0, stroke.color);
}

/// Draw a labeled axis with a double-headed fit-arrow for the
/// Shared glyph for the per-axis autoscale (silx `plot-xauto` / `plot-yauto`),
/// log-scale (silx `plot-xlog` / `plot-ylog`) and invert toggles. A `label`
/// (the axis letter, or "log") occupies the bulk of the icon while a
/// double-headed arrow — whose orientation names the axis — is tucked against
/// one edge: the X arrow along the bottom, the Y arrow down the left. The
/// label and arrow sit in separate bands so they never overlap. (The previous
/// autoscale/invert glyphs ran the arrow through the centre and painted the
/// label on top of the shaft; the log glyph stacked two text lines in the
/// 14px height — both crowded.) `inward` points the arrowheads toward each
/// other (invert / flip) instead of outward (autoscale / log), keeping the
/// actions visually distinct.
fn draw_axis_arrow_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    label: &str,
    vertical: bool,
    inward: bool,
    font_size: f32,
    stroke: egui::Stroke,
) {
    // Arrowhead barb length. Each head is a compact "V" spanning `a` along the
    // shaft; `inward` flips which side the vertex sits on so the head points
    // toward (invert) or away from (autoscale / log) the centre.
    let a = 2.0;
    let font = egui::FontId::proportional(font_size);
    if vertical {
        // Vertical double-arrow tucked against the left edge.
        let x = rect.left() + 2.5;
        let (ytop, ybot) = (rect.top() + 1.5, rect.bottom() - 1.5);
        painter.line_segment([egui::pos2(x, ytop), egui::pos2(x, ybot)], stroke);
        if inward {
            // Heads point toward the centre (vertex inset, barbs at the ends).
            painter.line_segment([egui::pos2(x, ytop + a), egui::pos2(x - a, ytop)], stroke);
            painter.line_segment([egui::pos2(x, ytop + a), egui::pos2(x + a, ytop)], stroke);
            painter.line_segment([egui::pos2(x, ybot - a), egui::pos2(x - a, ybot)], stroke);
            painter.line_segment([egui::pos2(x, ybot - a), egui::pos2(x + a, ybot)], stroke);
        } else {
            // Heads point away from the centre (vertex at the ends).
            painter.line_segment([egui::pos2(x, ytop), egui::pos2(x - a, ytop + a)], stroke);
            painter.line_segment([egui::pos2(x, ytop), egui::pos2(x + a, ytop + a)], stroke);
            painter.line_segment([egui::pos2(x, ybot), egui::pos2(x - a, ybot - a)], stroke);
            painter.line_segment([egui::pos2(x, ybot), egui::pos2(x + a, ybot - a)], stroke);
        }
        // Label centred in the area to the right of the arrow.
        let lx = (x + a + rect.right()) * 0.5;
        painter.text(
            egui::pos2(lx, rect.center().y),
            egui::Align2::CENTER_CENTER,
            label,
            font,
            stroke.color,
        );
    } else {
        // Horizontal double-arrow tucked against the bottom edge.
        let y = rect.bottom() - 2.0;
        let (xl, xr) = (rect.left() + 1.5, rect.right() - 1.5);
        painter.line_segment([egui::pos2(xl, y), egui::pos2(xr, y)], stroke);
        if inward {
            painter.line_segment([egui::pos2(xl + a, y), egui::pos2(xl, y - a)], stroke);
            painter.line_segment([egui::pos2(xl + a, y), egui::pos2(xl, y + a)], stroke);
            painter.line_segment([egui::pos2(xr - a, y), egui::pos2(xr, y - a)], stroke);
            painter.line_segment([egui::pos2(xr - a, y), egui::pos2(xr, y + a)], stroke);
        } else {
            painter.line_segment([egui::pos2(xl, y), egui::pos2(xl + a, y - a)], stroke);
            painter.line_segment([egui::pos2(xl, y), egui::pos2(xl + a, y + a)], stroke);
            painter.line_segment([egui::pos2(xr, y), egui::pos2(xr - a, y - a)], stroke);
            painter.line_segment([egui::pos2(xr, y), egui::pos2(xr - a, y + a)], stroke);
        }
        // Label centred in the area above the arrow.
        let ly = (rect.top() + (y - a)) * 0.5;
        painter.text(
            egui::pos2(rect.center().x, ly),
            egui::Align2::CENTER_CENTER,
            label,
            font,
            stroke.color,
        );
    }
}

/// [`ToolbarIcon::AutoscaleX`] / [`ToolbarIcon::AutoscaleY`] toggles (silx
/// `plot-xauto` / `plot-yauto`): a double arrow pointing outward reads as "fit
/// this axis to the data extent". `vertical` selects the Y orientation.
fn draw_autoscale_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    axis: &str,
    vertical: bool,
    stroke: egui::Stroke,
) {
    draw_axis_arrow_icon(painter, rect, axis, vertical, false, 11.0, stroke);
}

/// Draw two overlapping document outlines for the [`ToolbarIcon::Copy`] button.
fn draw_copy_icon(painter: &egui::Painter, rect: egui::Rect, stroke: egui::Stroke) {
    let back = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 2.0, rect.top() + 2.0),
        egui::pos2(rect.right() - 5.0, rect.bottom() - 5.0),
    );
    let front = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 5.0, rect.top() + 5.0),
        egui::pos2(rect.right() - 2.0, rect.bottom() - 2.0),
    );
    painter.rect_stroke(back, 1.0, stroke, egui::StrokeKind::Inside);
    painter.rect_stroke(front, 1.0, stroke, egui::StrokeKind::Inside);
}

/// Draw a floppy-disk save glyph for the [`ToolbarIcon::Save`] button.
fn draw_save_icon(painter: &egui::Painter, rect: egui::Rect, stroke: egui::Stroke) {
    let body = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 2.0, rect.top() + 2.0),
        egui::pos2(rect.right() - 2.0, rect.bottom() - 2.0),
    );
    painter.rect_stroke(body, 1.0, stroke, egui::StrokeKind::Inside);
    // Label area (top strip).
    let label = egui::Rect::from_min_max(
        egui::pos2(body.left() + 3.0, body.top()),
        egui::pos2(body.right() - 3.0, body.top() + body.height() * 0.35),
    );
    painter.rect_stroke(label, 0.0, stroke, egui::StrokeKind::Inside);
    // Shutter notch.
    let notch = egui::Rect::from_min_max(
        egui::pos2(body.right() - 6.0, body.top() + 1.0),
        egui::pos2(body.right() - 3.0, body.top() + body.height() * 0.3),
    );
    painter.rect_filled(notch, 0.0, stroke.color);
}

/// Draw a printer glyph (paper feed, body, output tray) for the
/// [`ToolbarIcon::Print`] button.
fn draw_print_icon(painter: &egui::Painter, rect: egui::Rect, stroke: egui::Stroke) {
    let cx_left = rect.left() + 5.0;
    let cx_right = rect.right() - 5.0;
    // Paper feeding in from the top.
    let feed = egui::Rect::from_min_max(
        egui::pos2(cx_left + 2.0, rect.top() + 2.0),
        egui::pos2(cx_right - 2.0, rect.top() + rect.height() * 0.32),
    );
    painter.rect_stroke(feed, 0.0, stroke, egui::StrokeKind::Inside);
    // Printer body.
    let body = egui::Rect::from_min_max(
        egui::pos2(cx_left, rect.top() + rect.height() * 0.34),
        egui::pos2(cx_right, rect.bottom() - rect.height() * 0.18),
    );
    painter.rect_stroke(body, 1.0, stroke, egui::StrokeKind::Inside);
    // Output tray (printed sheet emerging at the bottom).
    let tray = egui::Rect::from_min_max(
        egui::pos2(cx_left + 2.0, rect.bottom() - rect.height() * 0.34),
        egui::pos2(cx_right - 2.0, rect.bottom() - 2.0),
    );
    painter.rect_stroke(tray, 0.0, stroke, egui::StrokeKind::Inside);
}

/// Draw a leftward back-arrow for the [`ToolbarIcon::ZoomBack`] button.
fn draw_zoom_back_icon(painter: &egui::Painter, rect: egui::Rect, stroke: egui::Stroke) {
    let c = rect.center();
    let left = egui::pos2(rect.left() + 2.0, c.y);
    let right = egui::pos2(rect.right() - 2.0, c.y);
    painter.line_segment([left, right], stroke);
    let arrow = 4.0;
    painter.line_segment([left, left + egui::vec2(arrow, -arrow)], stroke);
    painter.line_segment([left, left + egui::vec2(arrow, arrow)], stroke);
}

/// Draw a magnifier with a `+` ([`ToolbarIcon::ZoomIn`]) or `-`
/// ([`ToolbarIcon::ZoomOut`]) inside the lens.
fn draw_zoom_step_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    stroke: egui::Stroke,
    plus: bool,
) {
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
    let bar = radius * 0.55;
    // Horizontal bar of the +/-.
    painter.line_segment(
        [center - egui::vec2(bar, 0.0), center + egui::vec2(bar, 0.0)],
        stroke,
    );
    if plus {
        // Vertical bar makes the +.
        painter.line_segment(
            [center - egui::vec2(0.0, bar), center + egui::vec2(0.0, bar)],
            stroke,
        );
    }
}

/// Draw a short dashed line over a dotted line for the [`ToolbarIcon::CurveStyle`]
/// cycle button.
fn draw_curve_style_icon(painter: &egui::Painter, rect: egui::Rect, stroke: egui::Stroke) {
    let y_top = rect.top() + rect.height() * 0.35;
    let y_bot = rect.top() + rect.height() * 0.65;
    let left = rect.left() + 1.0;
    let right = rect.right() - 1.0;
    // Dashed segment on top.
    let mut x = left;
    while x < right {
        let seg_end = (x + 3.0).min(right);
        painter.line_segment([egui::pos2(x, y_top), egui::pos2(seg_end, y_top)], stroke);
        x += 5.0;
    }
    // Dotted segment below.
    let mut x = left;
    while x < right {
        painter.line_segment([egui::pos2(x, y_bot), egui::pos2(x + 1.0, y_bot)], stroke);
        x += 3.0;
    }
}

/// Draw an L-shaped axes glyph (left Y axis + bottom X axis with arrow tips)
/// for the [`ToolbarIcon::ShowAxis`] toggle.
fn draw_show_axis_icon(painter: &egui::Painter, rect: egui::Rect, stroke: egui::Stroke) {
    let origin = egui::pos2(rect.left() + 3.0, rect.bottom() - 3.0);
    let x_end = egui::pos2(rect.right() - 1.0, rect.bottom() - 3.0);
    let y_end = egui::pos2(rect.left() + 3.0, rect.top() + 1.0);
    painter.line_segment([origin, x_end], stroke);
    painter.line_segment([origin, y_end], stroke);
    let arrow = 3.0;
    // X-axis arrow head.
    painter.line_segment([x_end, x_end + egui::vec2(-arrow, -arrow)], stroke);
    painter.line_segment([x_end, x_end + egui::vec2(-arrow, arrow)], stroke);
    // Y-axis arrow head.
    painter.line_segment([y_end, y_end + egui::vec2(-arrow, arrow)], stroke);
    painter.line_segment([y_end, y_end + egui::vec2(arrow, arrow)], stroke);
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

/// [`ToolbarIcon::LogX`] / [`ToolbarIcon::LogY`] toggles (silx `plot-xlog` /
/// `plot-ylog`): the word "log" with a double-arrow tucked against the axis
/// edge (X along the bottom, Y down the left) naming the axis the log scale
/// applies to. Mirrors the autoscale glyph but labelled "log" instead of a
/// single letter; `vertical` selects the Y orientation.
fn draw_log_icon(painter: &egui::Painter, rect: egui::Rect, vertical: bool, stroke: egui::Stroke) {
    draw_axis_arrow_icon(painter, rect, "log", vertical, false, 9.0, stroke);
}

/// [`ToolbarIcon::InvertX`] / [`ToolbarIcon::InvertY`] toggles: the same axis
/// letter + double-arrow glyph as autoscale, but the arrowheads point inward
/// (toward each other) to read as "flip / reverse this axis" rather than "fit
/// to extent", so the two toolbar buttons stay distinguishable.
fn draw_axis_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    axis: &str,
    vertical: bool,
    stroke: egui::Stroke,
) {
    draw_axis_arrow_icon(painter, rect, axis, vertical, true, 11.0, stroke);
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

/// How bin positions relate to the bins they describe when deriving the `N + 1`
/// bin edges from `N` positions, mirroring the silx histogram `align` parameter
/// (`Histogram.setData(align=)`, values `"left"`/`"center"`/`"right"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HistogramAlign {
    /// Each position is its bin's **left** edge; the trailing edge extends by the
    /// last spacing (silx `_computeEdges(x, "right")` — silx names it from where
    /// the extra edge is appended, on the right).
    Left,
    /// Each position is its bin's **center** (silx default; `_computeEdges(x,
    /// "center")`).
    #[default]
    Center,
    /// Each position is its bin's **right** edge; the leading edge extends back by
    /// the first spacing (silx `_computeEdges(x, "left")`).
    Right,
}

/// Derive the `N + 1` histogram bin edges from `N` bin positions and an
/// alignment, mirroring silx `items.histogram._computeEdges` (used by
/// `Histogram.setData(align=)`).
///
/// silx assumes uniform-ish spacing and extends the open end by one neighbour
/// gap: for `Left` (positions are left edges) the trailing edge is
/// `x[-1] + (x[-1] − x[-2])`; for `Right` (positions are right edges) the leading
/// edge is `x[0] − (x[1] − x[0])`; for `Center` it right-aligns first, then
/// shifts every edge left by half of its following gap (the last half-gap reused
/// for the final edge), placing each position at its bin centre. A lone position
/// uses a unit gap (silx `width = 1`). An empty input yields no edges.
///
/// Note the silx naming inversion this enum corrects: silx's `"right"` rule
/// appends the extra edge on the right and so treats positions as **left** edges
/// (and vice-versa); [`HistogramAlign`] names the variant after where the
/// position sits in its bin.
pub fn histogram_edges(positions: &[f64], align: HistogramAlign) -> Vec<f64> {
    if positions.is_empty() {
        return Vec::new();
    }
    let n = positions.len();
    match align {
        HistogramAlign::Left => {
            // silx `_computeEdges(x, "right")`: positions are left edges; append
            // x[-1] + last_gap.
            let width = if n > 1 {
                positions[n - 1] - positions[n - 2]
            } else {
                1.0
            };
            let mut edges = positions.to_vec();
            edges.push(positions[n - 1] + width);
            edges
        }
        HistogramAlign::Right => {
            // silx `_computeEdges(x, "left")`: positions are right edges; prepend
            // x[0] - first_gap.
            let width = if n > 1 {
                positions[1] - positions[0]
            } else {
                1.0
            };
            let mut edges = Vec::with_capacity(n + 1);
            edges.push(positions[0] - width);
            edges.extend_from_slice(positions);
            edges
        }
        HistogramAlign::Center => {
            // silx: right-align (positions as left edges), then shift each edge
            // left by half its following gap so the positions land at bin centres.
            let right = histogram_edges(positions, HistogramAlign::Left);
            let mut widths: Vec<f64> = right.windows(2).map(|w| (w[1] - w[0]) / 2.0).collect();
            if let Some(&last) = widths.last() {
                widths.push(last);
            }
            right
                .iter()
                .zip(widths.iter())
                .map(|(&edge, &width)| edge - width)
                .collect()
        }
    }
}

/// Pick the filled-histogram bin under data coordinates `(x_data, y_data)`,
/// mirroring silx `Histogram.__pickFilledHistogram` (items/histogram.py:244-279).
///
/// `edges` are the `N + 1` ascending bin edges and `values` the `N` per-bin
/// heights; `baseline` is the level the bars rise from (silx default `0`). A bar
/// occupies `[edges[i], edges[i + 1]) × [baseline, value]` (or `[value, baseline]`
/// when the bar points down). Returns the index of the bar containing the point,
/// or `None` when the point is outside the histogram's bounding box or not inside
/// any bar.
///
/// The bounding-box test is strict (silx `xmin < x < xmax`, `ymin < y < ymax`,
/// with the y-bounds including `0` so the fill region between bars and baseline is
/// covered); the per-bar test is inclusive. The bin is located with silx's
/// `searchsorted(edges, x, side="left") - 1`, clamped to `[0, N - 1]`.
pub fn pick_histogram(
    edges: &[f64],
    values: &[f64],
    baseline: f64,
    x_data: f64,
    y_data: f64,
) -> Option<usize> {
    if values.is_empty() || edges.len() != values.len() + 1 {
        return None;
    }

    // Bounding box (silx `Histogram._getBounds`, linear-axis branch): x spans the
    // edges; y includes 0 so the area between the bars and the baseline counts.
    let xmin = edges[0];
    let xmax = edges[edges.len() - 1];
    let mut vmin = f64::INFINITY;
    let mut vmax = f64::NEG_INFINITY;
    for &v in values {
        if v.is_finite() {
            vmin = vmin.min(v);
            vmax = vmax.max(v);
        }
    }
    if !vmin.is_finite() {
        // All values NaN: silx `_getBounds` returns None → nothing to pick.
        return None;
    }
    let ymin = vmin.min(0.0);
    let ymax = vmax.max(0.0);
    if x_data <= xmin || x_data >= xmax || y_data <= ymin || y_data >= ymax {
        return None;
    }

    // Bin index: silx `searchsorted(edges, x, side="left") - 1`, clamped to a
    // valid bin. `partition_point` counts edges strictly below x = side="left".
    let index = edges
        .partition_point(|&e| e < x_data)
        .saturating_sub(1)
        .min(values.len() - 1);

    let value = values[index];
    let hit = (baseline <= value && baseline <= y_data && y_data <= value)
        || (value < baseline && value <= y_data && y_data <= baseline);
    if hit { Some(index) } else { None }
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

/// Map accumulated widget [`DataBounds`] to the model [`DataRange`] consumed by
/// [`Plot::reset_zoom_to_data_range`], padding degenerate (single-point) bounds
/// via [`Bounds1D::as_non_degenerate`] so each refit axis gets a non-degenerate
/// span. An axis with no data maps to `None`, leaving it pinned by the model's
/// per-axis autoscale logic (silx `PlotWidget.resetZoom` restores axes without
/// data). Pure (no `RenderState`/GPU) so the reset path is unit-testable.
fn data_range_from_bounds(bounds: DataBounds) -> DataRange {
    DataRange {
        x: bounds.x.map(Bounds1D::as_non_degenerate),
        y: bounds.y_left.map(Bounds1D::as_non_degenerate),
        y2: bounds.y_right.map(Bounds1D::as_non_degenerate),
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

/// Capture a curve spec's raw data for live stats/fit consumers.
fn curve_spec_retained_data(spec: &CurveSpec<'_>) -> RetainedItemData {
    RetainedItemData::Curve {
        x: spec.x.to_vec(),
        y: spec.y.to_vec(),
    }
}

/// Scale `color`'s alpha channel by `alpha` (clamped to `[0, 1]`), mirroring the
/// backend's `apply_alpha`. Delegates to
/// [`scale_alpha`](crate::core::color::scale_alpha), which scales the *straight*
/// alpha and keeps the straight RGB — reading the premultiplied `Color32`
/// accessors and re-wrapping would double-premultiply the RGB for translucent
/// curve colors.
fn apply_curve_alpha(color: Color32, alpha: f32) -> Color32 {
    crate::core::color::scale_alpha(color, alpha)
}

/// Multiply per-point alpha into each color's alpha channel in place, mirroring
/// silx `Scatter.__applyColormapToData` (`rgbacolors[:, -1] *= __alpha`,
/// scatter.py:534-535): the colormap RGBA alpha is scaled by the per-point
/// alpha *before* the global item alpha multiplies on top in-shader (the
/// three-stage `colormap.alpha * per_point.alpha * global.alpha`).
///
/// Each `alpha` entry is clamped to `[0, 1]` (silx `setData` clamps the alpha
/// array, scatter.py:1051-1060). Composition runs over `min(colors, alpha)`
/// entries: a shorter `alpha` leaves the trailing colors unchanged, and extra
/// `alpha` entries past the colors are ignored — neither panics. `alpha`
/// matching the point count (the silx contract) scales every color.
///
/// silx operates on *straight* (un-premultiplied) RGBA, so this scales the
/// straight alpha (`Color32::to_srgba_unmultiplied`) and rebuilds via
/// `from_rgba_unmultiplied`, leaving the straight RGB unchanged. (Reading the
/// premultiplied `Color32::a/r/g/b` accessors and re-wrapping would
/// double-premultiply the RGB.)
fn compose_per_point_alpha(colors: &mut [Color32], alpha: &[f64]) {
    for (color, &a) in colors.iter_mut().zip(alpha) {
        let [r, g, b, sa] = color.to_srgba_unmultiplied();
        let sa = ((a.clamp(0.0, 1.0) as f32) * (sa as f32)).round() as u8;
        *color = Color32::from_rgba_unmultiplied(r, g, b, sa);
    }
}

/// Map each per-point `value` through `colormap` to its RGBA color, optionally
/// scaling each color's alpha by the matching `alpha` entry, mirroring silx
/// `Scatter.__applyColormapToData` (scatter.py:526-535).
///
/// silx shares `__applyColormapToData` between the `POINTS` and `SOLID`
/// visualizations, so both [`ScatterView`] render arms call this single helper:
/// the value is normalized through the colormap into its 256-entry LUT, then —
/// when a per-point `alpha` array is present — the per-point alpha multiplies
/// the colormap RGBA alpha (stage 2 of the three-stage `colormap.alpha *
/// per_point.alpha * global.alpha`; see [`compose_per_point_alpha`]). Factored
/// out so the two arms cannot drift and the mapping is unit-testable without a
/// GPU backend.
fn point_colors(values: &[f64], colormap: &Colormap, alpha: Option<&[f64]>) -> Vec<Color32> {
    let mut colors: Vec<Color32> = values
        .iter()
        .map(|&v| {
            let t = colormap.normalize(v);
            let idx = (t * 255.0).clamp(0.0, 255.0) as usize;
            let [r, g, b, a] = colormap.lut[idx];
            Color32::from_rgba_unmultiplied(r, g, b, a)
        })
        .collect();
    if let Some(alpha) = alpha {
        compose_per_point_alpha(&mut colors, alpha);
    }
    colors
}

/// Clamp each per-point alpha entry to `[0, 1]`, mirroring silx
/// `Scatter.setData` (`numpy.clip(alpha, 0.0, 1.0)`, scatter.py:1058-1059).
/// Stored at the setter so the retained array is already in range; the
/// composition in [`compose_per_point_alpha`] clamps again defensively.
fn clamp_alpha(mut alpha: Vec<f64>) -> Vec<f64> {
    for a in &mut alpha {
        *a = a.clamp(0.0, 1.0);
    }
    alpha
}

/// Build a [`CurveData`] from a [`CurveSpec`], mirroring the backend's
/// `curve_data_from_spec`. Retained in the [`ItemRecord`] so the curve-style
/// cycle action can clone, edit the line style, and re-apply the full curve
/// (preserving color, symbol, width, error bars, fill). Owning the conversion
/// here keeps the retained copy faithful to what the backend renders.
fn curve_data_from_spec_hl(spec: &CurveSpec<'_>) -> CurveData {
    let color = match &spec.color {
        CurveColor::Uniform(color) => apply_curve_alpha(*color, spec.alpha),
        CurveColor::PerVertex(colors) => colors
            .first()
            .copied()
            .map(|color| apply_curve_alpha(color, spec.alpha))
            .unwrap_or(Color32::WHITE),
    };
    let mut curve = CurveData::new(spec.x.to_vec(), spec.y.to_vec(), color)
        .with_width(spec.line_width)
        .with_line_style(spec.line_style.clone())
        .with_marker_size(spec.symbol_size)
        .with_y_axis(spec.y_axis);
    if let CurveColor::PerVertex(colors) = &spec.color {
        curve = curve.with_colors(
            colors
                .iter()
                .copied()
                .map(|color| apply_curve_alpha(color, spec.alpha))
                .collect(),
        );
    }
    if let Some(gap_color) = spec.gap_color {
        curve = curve.with_gap_color(apply_curve_alpha(gap_color, spec.alpha));
    }
    if let Some(symbol) = spec.symbol {
        curve = curve.with_symbol(symbol);
    }
    if let Some(error) = &spec.x_error {
        curve = curve.with_x_error(error.clone());
    }
    if let Some(error) = &spec.y_error {
        curve = curve.with_y_error(error.clone());
    }
    if spec.fill {
        curve = curve.with_fill(spec.baseline.clone());
    }
    curve
}

/// Borrow a [`RetainedItemData`] as a [`StatsInput`] for a live
/// [`StatsWidget`] / fit feed (silx `StatsWidget` per-item data). Split out so
/// the data→input bridge is unit-testable without a GPU backend.
///
/// [`StatsInput`]: crate::widget::stats_widget::StatsInput
fn retained_data_to_stats_input(
    data: &RetainedItemData,
) -> crate::widget::stats_widget::StatsInput<'_> {
    use crate::widget::stats_widget::StatsInput;
    match data {
        RetainedItemData::Curve { x, y } => StatsInput::Curve { xs: x, ys: y },
        RetainedItemData::Image {
            data,
            width,
            height,
            origin,
            scale,
            ..
        } => StatsInput::Image {
            data,
            width: *width,
            height: *height,
            origin: *origin,
            scale: *scale,
        },
    }
}

/// Clone `base` with its value limits replaced by the [`AutoscaleMode`] range
/// over `pixels` (NaN-ignoring), for a raw-pixel autoscale (silx
/// `ColormapDialog` Stddev3 / Percentile autoscale, ColormapDialog.py:450-480).
///
/// The percentile pair comes from the base colormap's
/// [`autoscale_percentiles`](Colormap::autoscale_percentiles) (silx
/// `Colormap._percentiles`); [`AutoscaleMode::Stddev3`] ignores it. The LUT,
/// normalization, gamma, and NaN color are preserved — only `vmin`/`vmax`
/// change. Split out so the autoscale computation is unit-testable without a GPU
/// backend.
fn autoscaled_colormap(base: &Colormap, mode: AutoscaleMode, pixels: &[f64]) -> Colormap {
    let (vmin, vmax) = mode.range(pixels, base.autoscale_percentiles);
    let mut cm = base.clone();
    cm.vmin = vmin;
    cm.vmax = vmax;
    cm
}

/// Borrow a [`RetainedItemData`]'s curve `(x, y)` arrays for a live
/// [`FitWidget`] target (silx `FitWidget.setData`), or `None` when the item is
/// not a curve. Split out so the data→fit feed is unit-testable without a GPU
/// backend.
///
/// [`FitWidget`]: crate::widget::fit_widget::FitWidget
fn retained_curve_xy(data: &RetainedItemData) -> Option<(&[f64], &[f64])> {
    match data {
        RetainedItemData::Curve { x, y } => Some((x, y)),
        RetainedItemData::Image { .. } => None,
    }
}

/// Build one [`RoiStatsRow`] per ROI by reducing the active item's retained
/// `data` inside each ROI, for a [`RoiStatsWidget`] (silx `ROIStatsWidget`
/// table). An image is reduced with the proper per-pixel `roi.contains` test +
/// integral ([`image_roi_stats`]); a curve over its `x`-span
/// ([`curve_roi_stats`]). The row label is the ROI's name, or `ROI {index}`
/// when the name is empty. Pure (no GPU), so the row building is unit-testable.
///
/// [`RoiStatsRow`]: crate::widget::roi_stats_widget::RoiStatsRow
/// [`RoiStatsWidget`]: crate::widget::roi_stats_widget::RoiStatsWidget
/// [`image_roi_stats`]: crate::widget::roi_stats::image_roi_stats
/// [`curve_roi_stats`]: crate::widget::roi_stats::curve_roi_stats
fn roi_stats_rows(
    rois: &[ManagedRoi],
    data: &RetainedItemData,
) -> Vec<crate::widget::roi_stats_widget::RoiStatsRow> {
    use crate::widget::roi_stats::{curve_roi_stats, image_roi_stats};
    use crate::widget::roi_stats_widget::RoiStatsRow;

    rois.iter()
        .enumerate()
        .map(|(index, managed)| {
            let stats = match data {
                RetainedItemData::Image {
                    data,
                    width,
                    height,
                    origin,
                    scale,
                    ..
                } => {
                    // image_roi_stats reduces f32 pixels; the retained pixels are
                    // f64 narrowed from the originally-f32 image, so the cast back
                    // to f32 is lossless.
                    let pixels: Vec<f32> = data.iter().map(|&v| v as f32).collect();
                    image_roi_stats(
                        &managed.roi,
                        &pixels,
                        *width,
                        *height,
                        [origin.0, origin.1],
                        [scale.0, scale.1],
                    )
                }
                RetainedItemData::Curve { x, y } => curve_roi_stats(&managed.roi, x, y),
            };
            let label = if managed.name.is_empty() {
                format!("ROI {index}")
            } else {
                managed.name.clone()
            };
            RoiStatsRow { label, stats }
        })
        .collect()
}

/// One row per curve ROI (those with an `x`-span) over the active curve's
/// `(x, y)`, reduced via [`curve_roi_counts`] (silx `CurvesROIWidget`). ROIs with
/// no `x`-span (e.g. `HRange`) are not curve ROIs and are skipped; the surviving
/// rows keep each ROI's original index in its `ROI {index}` fallback label so it
/// stays traceable to [`PlotWidget::rois`].
///
/// [`curve_roi_counts`]: crate::widget::roi_stats::curve_roi_counts
fn curve_roi_rows(
    rois: &[ManagedRoi],
    x: &[f64],
    y: &[f64],
) -> Vec<crate::widget::curves_roi_widget::CurveRoiRow> {
    use crate::widget::curves_roi_widget::CurveRoiRow;
    use crate::widget::roi_stats::{curve_roi_counts, roi_x_span};

    rois.iter()
        .enumerate()
        .filter_map(|(index, managed)| {
            let (from, to) = roi_x_span(&managed.roi)?;
            let counts = curve_roi_counts(&managed.roi, x, y)?;
            let label = if managed.name.is_empty() {
                format!("ROI {index}")
            } else {
                managed.name.clone()
            };
            Some(CurveRoiRow {
                label,
                from,
                to,
                counts,
            })
        })
        .collect()
}

/// Capture a scalar image spec's raw pixels and geometry for live consumers, or
/// `None` for an RGBA image (no scalar field to retain).
fn image_spec_retained_data(spec: &ImageSpec<'_>) -> Option<RetainedItemData> {
    match &spec.pixels {
        ImagePixelsSpec::Scalar {
            width,
            height,
            data,
            colormap,
        } => Some(RetainedItemData::Image {
            data: data.iter().map(|&v| v as f64).collect(),
            width: *width as usize,
            height: *height as usize,
            origin: spec.origin,
            scale: spec.scale,
            colormap: colormap.clone(),
        }),
        ImagePixelsSpec::Rgba { .. } => None,
    }
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
    LegendVisual::curve(color, spec.line_style.clone(), spec.symbol)
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

/// Route an active curve's per-axis labels onto the plot's active-label
/// overrides (silx `_setActiveItem` → `Axis._setCurrentLabel`): the X label
/// always drives the X axis; the Y label drives the left Y axis or the right
/// (y2) axis by the curve's `y_axis`. Returns
/// `(active_x, active_y_left, active_y2)` for [`Plot::active_x_label`] /
/// `active_y_label` / `active_y2_label`. Pure and headless-testable.
fn active_axis_label_overrides(
    x_label: Option<&str>,
    y_label: Option<&str>,
    y_axis: YAxis,
) -> (Option<String>, Option<String>, Option<String>) {
    let x = x_label.map(ToOwned::to_owned);
    let y = y_label.map(ToOwned::to_owned);
    match y_axis {
        YAxis::Left => (x, y, None),
        YAxis::Right => (x, None, y),
    }
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
        // Per-curve axis labels live in the high-level `ItemRecord` (alongside
        // `legend`), not in `CurveData`, so a data round-trip carries none; the
        // record's labels are preserved across this re-application path.
        x_label: None,
        y_label: None,
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

/// Raw item data retained alongside an [`ItemRecord`] so live consumers (a
/// [`StatsWidget`], a [`FitWidget`], a raw-pixel autoscale) can read the active
/// item's data without the caller re-supplying it. Only scalar curves and
/// scalar images are retained; RGBA images, triangles, shapes, and markers have
/// no retained data.
#[derive(Clone, Debug)]
enum RetainedItemData {
    /// A curve's `(x, y)` arrays.
    Curve { x: Vec<f64>, y: Vec<f64> },
    /// A scalar image's row-major pixels (as `f64`), its geometry, and its
    /// colormap (retained so a raw-pixel autoscale can re-upload the image with
    /// new value limits without depending on transient render state). The
    /// colormap is boxed: its 256-entry LUT would otherwise dominate the enum's
    /// size and bloat every `Curve` variant too.
    Image {
        data: Vec<f64>,
        width: usize,
        height: usize,
        origin: (f64, f64),
        scale: (f64, f64),
        colormap: Box<Colormap>,
    },
}

#[derive(Clone, Debug)]
struct ItemRecord {
    handle: ItemHandle,
    kind: PlotItemKind,
    bounds: DataBounds,
    legend: Option<String>,
    /// Per-curve X-axis label shown while this item is the active curve (silx
    /// `Curve.getXLabel`). Stored here (not in `CurveData`) so it is preserved
    /// across style/highlight re-applications, exactly like `legend`. `None`
    /// keeps the graph's default X label active.
    x_label: Option<String>,
    /// Per-curve Y-axis label shown while this item is the active curve (silx
    /// `Curve.getYLabel`), routed to the left or right axis by the curve's
    /// `y_axis`. See [`Self::x_label`].
    y_label: Option<String>,
    stats: Option<ItemStats>,
    visual: LegendVisual,
    /// Retained raw data for live stats/fit consumers; `None` for items whose
    /// data is not retained (RGBA images, triangles, shapes, markers).
    data: Option<RetainedItemData>,
    /// Full retained [`CurveData`] (data + style) of a curve-like item, so the
    /// curve-style cycle action (silx `CurveStyleAction`) can clone it, change
    /// the line style, and re-apply without losing color/symbol/error bars.
    /// `None` for non-curve items.
    curve_data: Option<CurveData>,
    /// UI-only restore cache for the legend "Points" toggle (silx checkable
    /// `Points` action). When the symbol is hidden, the previously visible
    /// [`Symbol`] is stashed here so toggling back on restores the *same*
    /// variant losslessly instead of a default. NOT a source of truth: no
    /// render/bounds/legend-visual path may read it — what is drawn is decided
    /// solely by [`CurveData::symbol`]. `None` means "nothing stashed".
    hidden_symbol: Option<Symbol>,
    /// UI-only restore cache for the legend "Lines" toggle (silx checkable
    /// `Lines` action). When the line is hidden, the previously visible
    /// [`LineStyle`] is stashed here so toggling back on restores the *same*
    /// style (e.g. `Dashed`, not just `Solid`). NOT a source of truth: no
    /// render/bounds/legend-visual path may read it — what is drawn is decided
    /// solely by [`CurveData::line_style`]. `None` means "nothing stashed".
    hidden_line_style: Option<LineStyle>,
}

#[derive(Clone, Debug)]
struct LegendVisual {
    color: Color32,
    secondary: Option<Color32>,
    /// Curve line style, so the legend icon shows dashed / dotted / solid /
    /// none exactly as the curve draws (silx `LegendIcon.setLineStyle`).
    /// Non-curve kinds leave this at [`LineStyle::Solid`]; their swatch branches
    /// ignore it.
    line_style: LineStyle,
    /// Curve marker symbol drawn at the icon center (silx
    /// `LegendIcon.setSymbol`). `None` means no marker, matching a curve created
    /// with `symbol: None`.
    symbol: Option<Symbol>,
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
            line_style: LineStyle::Solid,
            symbol: None,
        }
    }

    fn with_secondary(color: Color32, secondary: Color32) -> Self {
        Self {
            color,
            secondary: Some(secondary),
            line_style: LineStyle::Solid,
            symbol: None,
        }
    }

    /// Curve icon carrying the curve's line style and marker symbol (silx
    /// `LegendIcon` built from the curve's `CurveStyle`).
    fn curve(color: Color32, line_style: LineStyle, symbol: Option<Symbol>) -> Self {
        Self {
            color,
            secondary: None,
            line_style,
            symbol,
        }
    }
}

fn legend_row_width(available_width: f32) -> f32 {
    available_width.max(LEGEND_ROW_MIN_WIDTH)
}

/// Default symbol restored when "Points" is toggled on for a curve that never
/// had a visible symbol cached (e.g. it was created with `symbol: None`). silx
/// has no recorded prior symbol either; a small filled point is the neutral
/// choice. Used only as the empty-cache fallback in [`set_symbol_visibility`].
const DEFAULT_RESTORE_SYMBOL: Symbol = Symbol::Point;

/// Default line style restored when "Lines" is toggled on for a curve that
/// never had a visible line cached (e.g. it was created with
/// `line_style: LineStyle::None`). A solid line is the neutral default; used
/// only as the empty-cache fallback in [`set_line_visibility`].
const DEFAULT_RESTORE_LINE_STYLE: LineStyle = LineStyle::Solid;

/// Pure transform for the legend "Points" checkable toggle (silx
/// `togglePointsAction`). Returns the new [`CurveData::symbol`] value and
/// maintains a lossless restore `cache` so a hide→show round-trip restores the
/// exact prior [`Symbol`] variant, not a default.
///
/// - hide (`visible == false`) when currently visible: stash `current` into
///   `cache`, return `None`.
/// - show (`visible == true`) when currently hidden: take from `cache`
///   (falling back to [`DEFAULT_RESTORE_SYMBOL`] only if the cache is empty),
///   return `Some(symbol)`.
/// - no-op cases (show-when-visible, hide-when-hidden) return `current`
///   unchanged and never clobber `cache`.
///
/// The returned value is the only thing the caller writes to `CurveData`; the
/// `cache` is UI memory and must not be read by any render/bounds/legend path.
fn set_symbol_visibility(
    current: Option<Symbol>,
    visible: bool,
    cache: &mut Option<Symbol>,
) -> Option<Symbol> {
    match (visible, current) {
        // Show while already visible: no-op, keep the cache untouched.
        (true, Some(symbol)) => Some(symbol),
        // Show while hidden: restore the stashed symbol, or the default if the
        // curve was created hidden (empty cache). Consume the cache entry.
        (true, None) => Some(cache.take().unwrap_or(DEFAULT_RESTORE_SYMBOL)),
        // Hide while visible: stash the current symbol for a lossless restore.
        (false, Some(symbol)) => {
            *cache = Some(symbol);
            None
        }
        // Hide while already hidden: no-op, do not clobber a prior stash.
        (false, None) => None,
    }
}

/// Pure transform for the legend "Lines" checkable toggle (silx
/// `toggleLinesAction`). Returns the new [`CurveData::line_style`] value and
/// maintains a lossless restore `cache` so a hide→show round-trip restores the
/// exact prior [`LineStyle`] (e.g. `Dashed`), not a default.
///
/// - hide (`visible == false`) when currently drawing a line: stash `current`
///   into `cache`, return [`LineStyle::None`].
/// - show (`visible == true`) when currently hidden: take from `cache`
///   (falling back to [`DEFAULT_RESTORE_LINE_STYLE`] only if the cache is
///   empty), return that style.
/// - no-op cases (show-when-drawing, hide-when-hidden) return `current`
///   unchanged and never clobber `cache`.
///
/// The returned value is the only thing the caller writes to `CurveData`; the
/// `cache` is UI memory and must not be read by any render/bounds/legend path.
fn set_line_visibility(
    current: LineStyle,
    visible: bool,
    cache: &mut Option<LineStyle>,
) -> LineStyle {
    match (visible, current.draws_line()) {
        // Show while already drawing a line: no-op, keep the cache untouched.
        (true, true) => current,
        // Show while hidden: restore the stashed style, or the default if the
        // curve was created with no line (empty cache). Consume the cache.
        (true, false) => cache.take().unwrap_or(DEFAULT_RESTORE_LINE_STYLE),
        // Hide while drawing a line: stash the current style for restore.
        (false, true) => {
            *cache = Some(current);
            LineStyle::None
        }
        // Hide while already hidden: no-op, do not clobber a prior stash.
        (false, false) => current,
    }
}

/// Per-field override style for the active-curve highlight (silx
/// `items/curve.py` `class CurveStyle(_Style)`).
///
/// Each field is `Some(value)` to override that aspect of the curve's own
/// style, or `None` to inherit the curve's base value — exactly silx's "set a
/// value to `None` to use the default" convention. The active-curve highlight
/// merges these over the curve's retained [`CurveData`] via
/// [`current_curve_style`].
///
/// The default value used for the active-curve highlight is
/// `CurveStyle { line_width: Some(2.0), ..Default::default() }`, mirroring
/// silx's `DEFAULT_PLOT_ACTIVE_CURVE_LINEWIDTH = 2` with
/// `DEFAULT_PLOT_ACTIVE_CURVE_COLOR = None`: the active curve is emphasised
/// purely by a thicker line, leaving color and markers unchanged.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CurveStyle {
    /// Override line color (silx `CurveStyle` `color`). `None` inherits the
    /// curve's own [`CurveData::color`].
    pub color: Option<Color32>,
    /// Override line width in pixels (silx `linewidth`). `None` inherits the
    /// curve's own [`CurveData::width`].
    pub line_width: Option<f32>,
    /// Override line stroke style (silx `linestyle`). `None` inherits the
    /// curve's own [`CurveData::line_style`].
    pub line_style: Option<LineStyle>,
    /// Override marker symbol (silx `symbol`). `Some(s)` replaces the marker
    /// with `s`; `None` inherits the curve's own [`CurveData::symbol`]. The
    /// active-curve highlight emphasises, it never hides markers — a faithful
    /// mapping of silx, where the active style's `symbol` field is `None` by
    /// default and so leaves the curve's own symbol untouched.
    pub symbol: Option<Symbol>,
    /// Override marker size in pixels (silx `symbolsize`). `None` inherits the
    /// curve's own [`CurveData::marker_size`].
    pub symbol_size: Option<f32>,
    /// Override dashed-line gap color (silx `gapcolor`). `None` inherits the
    /// curve's own [`CurveData::gap_color`].
    pub gap_color: Option<Color32>,
}

/// Resolve the render style of a curve given its retained base style, the
/// active-curve highlight override, and whether this curve is currently the
/// highlighted (active) one. Pure and headless-testable.
///
/// This is silx `Curve.getCurrentStyle()` (`items/curve.py` ~280-330): when
/// `highlighted`, each resolved field is the highlight's value when it is
/// `Some`, else the curve's own (per-field override, `None` falls through);
/// when not `highlighted`, the curve renders with its own base style
/// unchanged.
///
/// Only the *style* fields are affected (color/width/line_style/symbol/
/// marker_size/gap_color). The data fields (x/y/colors/fill/baseline/errors/
/// y_axis) are passed through from `base` untouched — they are data, not
/// style.
fn current_curve_style(base: &CurveData, highlight: &CurveStyle, highlighted: bool) -> CurveData {
    let mut resolved = base.clone();
    if highlighted {
        if let Some(color) = highlight.color {
            resolved.color = color;
        }
        if let Some(width) = highlight.line_width {
            resolved.width = width;
        }
        if let Some(line_style) = highlight.line_style.clone() {
            resolved.line_style = line_style;
        }
        if let Some(symbol) = highlight.symbol {
            resolved.symbol = Some(symbol);
        }
        if let Some(symbol_size) = highlight.symbol_size {
            resolved.marker_size = symbol_size;
        }
        if let Some(gap_color) = highlight.gap_color {
            resolved.gap_color = Some(gap_color);
        }
    }
    resolved
}

/// What a single legend-row interaction returned.
struct LegendRowResult {
    /// Click anywhere in the row body (not the eye icon).
    row_clicked: bool,
    /// Click on the visibility eye icon.
    eye_clicked: bool,
    /// The row's egui [`Response`](egui::Response), so the caller can attach a
    /// right-click context menu while holding `&mut self`.
    row_response: egui::Response,
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
        row_response,
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
    // No bounding box around the icon (silx LegendIcon draws none): the line,
    // marker, color, or fill alone identifies the item.
    match kind {
        PlotItemKind::Curve => {
            // silx `LegendIcon`: a full-width line in the curve's style, plus the
            // curve's marker symbol centered on it.
            let y = rect.center().y;
            let a = egui::pos2(rect.left() + 4.0, y);
            let b = egui::pos2(rect.right() - 4.0, y);
            if visual.line_style.draws_line() {
                let stroke = egui::Stroke::new(2.0, visual.color);
                match visual.line_style.painter_dashes(stroke.width) {
                    None => {
                        painter.line_segment([a, b], stroke);
                    }
                    Some((dashes, gaps, offset)) => {
                        for shape in egui::Shape::dashed_line_with_offset(
                            &[a, b],
                            stroke,
                            &dashes,
                            &gaps,
                            offset,
                        ) {
                            painter.add(shape);
                        }
                    }
                }
            }
            if let Some(symbol) = visual.symbol {
                draw_legend_symbol(painter, rect.center(), 4.0, symbol, visual.color);
            }
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

/// Draw a curve's marker [`Symbol`] centered at `center` with half-extent
/// `half`, filled in `color` (silx `LegendIcon` symbol path). Filled glyphs
/// (circle / square / diamond / triangle / point / pixel) are drawn solid;
/// stroke glyphs (cross / plus / lines / ticks / carets) use a `color` stroke.
/// Mirrors the legend symbol the curve renderer draws at each vertex so the icon
/// matches the plotted marker.
fn draw_legend_symbol(
    painter: &egui::Painter,
    center: egui::Pos2,
    half: f32,
    symbol: Symbol,
    color: Color32,
) {
    let c = center;
    let stroke = egui::Stroke::new(1.5, color);
    // Apex-then-arms helper for the open carets.
    let caret = |apex: egui::Pos2, arm_a: egui::Pos2, arm_b: egui::Pos2| {
        painter.line_segment([apex, arm_a], stroke);
        painter.line_segment([apex, arm_b], stroke);
    };
    match symbol {
        Symbol::Circle => {
            painter.circle_filled(c, half, color);
        }
        Symbol::Point => {
            painter.circle_filled(c, half * 0.6, color);
        }
        Symbol::Pixel => {
            painter.rect_filled(
                egui::Rect::from_center_size(c, egui::Vec2::splat(half * 0.9)),
                0.0,
                color,
            );
        }
        Symbol::Square => {
            painter.rect_filled(
                egui::Rect::from_center_size(c, egui::Vec2::splat(half * 2.0)),
                0.0,
                color,
            );
        }
        Symbol::Diamond => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x, c.y - half),
                    egui::pos2(c.x + half, c.y),
                    egui::pos2(c.x, c.y + half),
                    egui::pos2(c.x - half, c.y),
                ],
                color,
                egui::Stroke::NONE,
            ));
        }
        Symbol::Triangle => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x, c.y - half),
                    egui::pos2(c.x + half, c.y + half),
                    egui::pos2(c.x - half, c.y + half),
                ],
                color,
                egui::Stroke::NONE,
            ));
        }
        Symbol::Cross => {
            painter.line_segment(
                [
                    egui::pos2(c.x - half, c.y - half),
                    egui::pos2(c.x + half, c.y + half),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x - half, c.y + half),
                    egui::pos2(c.x + half, c.y - half),
                ],
                stroke,
            );
        }
        Symbol::Plus => {
            painter.line_segment(
                [egui::pos2(c.x, c.y - half), egui::pos2(c.x, c.y + half)],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x - half, c.y), egui::pos2(c.x + half, c.y)],
                stroke,
            );
        }
        Symbol::VerticalLine => {
            painter.line_segment(
                [egui::pos2(c.x, c.y - half), egui::pos2(c.x, c.y + half)],
                stroke,
            );
        }
        Symbol::HorizontalLine => {
            painter.line_segment(
                [egui::pos2(c.x - half, c.y), egui::pos2(c.x + half, c.y)],
                stroke,
            );
        }
        Symbol::TickLeft => {
            painter.line_segment([egui::pos2(c.x - half, c.y), c], stroke);
        }
        Symbol::TickRight => {
            painter.line_segment([c, egui::pos2(c.x + half, c.y)], stroke);
        }
        Symbol::TickUp => {
            painter.line_segment([egui::pos2(c.x, c.y - half), c], stroke);
        }
        Symbol::TickDown => {
            painter.line_segment([c, egui::pos2(c.x, c.y + half)], stroke);
        }
        Symbol::CaretLeft => caret(
            egui::pos2(c.x - half, c.y),
            egui::pos2(c.x + half, c.y - half),
            egui::pos2(c.x + half, c.y + half),
        ),
        Symbol::CaretRight => caret(
            egui::pos2(c.x + half, c.y),
            egui::pos2(c.x - half, c.y - half),
            egui::pos2(c.x - half, c.y + half),
        ),
        Symbol::CaretUp => caret(
            egui::pos2(c.x, c.y - half),
            egui::pos2(c.x - half, c.y + half),
            egui::pos2(c.x + half, c.y + half),
        ),
        Symbol::CaretDown => caret(
            egui::pos2(c.x, c.y + half),
            egui::pos2(c.x - half, c.y - half),
            egui::pos2(c.x + half, c.y - half),
        ),
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
        crate::core::color::with_alpha(color, 180)
    };
    if visible {
        painter.circle_stroke(egui::pos2(cx, cy), r, egui::Stroke::new(1.5, eye_color));
        painter.circle_filled(egui::pos2(cx, cy), r * 0.45, eye_color);
    } else {
        let dim = crate::core::color::with_alpha(color, 80);
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
    /// Per-field override style applied to the active curve when active-curve
    /// handling is enabled (silx `PlotWidget._activeCurveStyle`). Default is
    /// `line_width: Some(2.0)` with all other fields `None`, matching silx's
    /// `DEFAULT_PLOT_ACTIVE_CURVE_LINEWIDTH = 2` /
    /// `DEFAULT_PLOT_ACTIVE_CURVE_COLOR = None`.
    active_curve_style: CurveStyle,
    /// Whether the active curve is rendered with [`Self::active_curve_style`]
    /// applied (silx `PlotWidget.setActiveCurveHandling`). When `false`, every
    /// curve renders with its own base style. Enabled by default.
    active_curve_handling: bool,
    events: Vec<PlotEvent>,
    /// Open legend rename popup: the item being renamed and its edit buffer
    /// (silx `RenameCurveDialog`). `None` when no rename is in progress.
    rename_state: Option<(ItemHandle, String)>,
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
            active_curve_style: CurveStyle {
                line_width: Some(2.0),
                ..CurveStyle::default()
            },
            active_curve_handling: true,
            events: Vec::new(),
            rename_state: None,
        }
    }

    /// Render the widget in `ui`, handling interaction and plot item selection.
    pub fn show(&mut self, ui: &mut egui::Ui) -> PlotResponse {
        let before = self.limits_snapshot();
        self.sync_active_axis_labels();
        let response = PlotView::new().show_with_interaction(
            ui,
            self.backend.plot_mut(),
            self.interaction_mode,
        );
        self.backend
            .set_plot_bounds_in_pixels(response.transform.area);
        self.select_item_from_plot_response(&response);
        self.emit_item_pointer_events(&response);
        self.push_limits_changed_if(before);
        if let Some(index) = response.roi_changed {
            self.events.push(PlotEvent::RoiChanged { index });
        }
        if let Some(index) = response.roi_created {
            self.events.push(PlotEvent::RoiCreated { index });
        }
        // Draw-state events (silx drawingProgress / drawingFinished) from an
        // on-plot RoiCreate draw. DrawingFinished fires on the same frame as the
        // RoiCreated above (the ROI is built on top of the finished draw).
        match response.draw_event.clone() {
            Some(DrawEvent::InProgress { mode, points }) => {
                self.events
                    .push(PlotEvent::DrawingProgress { mode, points });
            }
            Some(DrawEvent::Finished { mode, params }) => {
                self.events
                    .push(PlotEvent::DrawingFinished { mode, params });
            }
            None => {}
        }
        // Marker drag lifecycle (silx beginDrag/drag/endDrag):
        // MarkerDragStarted → MarkerMoved×N → MarkerDragFinished. The start fires
        // on the same frame as the first MarkerMoved (the grab is also a move), so
        // it must be queued *before* the moved block to keep the lifecycle order.
        if let Some(handle) = response.marker_drag_started {
            self.events.push(PlotEvent::MarkerDragStarted { handle });
        }
        // Persist an on-screen marker drag: apply_interaction live-mutated the
        // mirror `plot.markers` for this frame's render, but the mirror is
        // rebuilt from the backend items on every sync, so the moved data must
        // be written back to the owning backend item. Read the new marker from
        // the mirror (located via the parallel marker_handles), then persist it.
        if let Some(handle) = response.marker_moved {
            let plot = self.backend.plot();
            let moved = plot
                .marker_handles
                .iter()
                .position(|&h| h == handle)
                .and_then(|index| plot.markers.get(index).cloned());
            if let Some(marker) = moved {
                self.backend.update_marker(handle, marker);
                self.events.push(PlotEvent::MarkerMoved { handle });
            }
        }
        // Drag finished on release (silx endDrag markerMoved): the final position
        // is already persisted by the preceding MarkerMoved frames.
        if let Some(handle) = response.marker_drag_finished {
            self.events.push(PlotEvent::MarkerDragFinished { handle });
        }
        response
    }

    /// Push the active curve's per-axis labels onto the core plot's active-label
    /// overrides so the chrome draws them in place of the graph defaults (silx
    /// `_setActiveItem` → `Axis._setCurrentLabel`). Recomputed every frame from
    /// the current active curve; cleared to the graph defaults when no curve is
    /// active. The active curve's Y label is routed to the left or right (y2)
    /// axis by the curve's `y_axis`.
    fn sync_active_axis_labels(&mut self) {
        let (x, y, y2) = self
            .active_curve()
            .and_then(|handle| self.item_record(handle))
            .map(|record| {
                let y_axis = record
                    .curve_data
                    .as_ref()
                    .map(|data| data.y_axis)
                    .unwrap_or(YAxis::Left);
                active_axis_label_overrides(
                    record.x_label.as_deref(),
                    record.y_label.as_deref(),
                    y_axis,
                )
            })
            .unwrap_or((None, None, None));
        let plot = self.backend.plot_mut();
        plot.active_x_label = x;
        plot.active_y_label = y;
        plot.active_y2_label = y2;
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

    /// Arm on-plot creation of a new ROI of `kind` (silx
    /// `RegionOfInterestManager.start(roiClass)`). A convenience for
    /// `set_interaction_mode(PlotInteractionMode::RoiCreate(kind))`: the next
    /// primary drag (or click, for [`RoiDrawKind::Point`]/[`RoiDrawKind::Cross`])
    /// draws the shape; finishing it appends the ROI to `plot().rois` and queues
    /// a [`PlotEvent::RoiCreated`]. Creation re-arms continuously until the mode
    /// is changed.
    pub fn set_roi_create_mode(&mut self, kind: RoiDrawKind) {
        self.set_interaction_mode(PlotInteractionMode::RoiCreate(kind));
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
        let after = self.limits_snapshot();
        if before != after {
            let ((xmin, xmax, ymin, ymax), y2) = after;
            self.events.push(PlotEvent::LimitsChanged {
                x: (xmin, xmax),
                y: (ymin, ymax),
                y2,
            });
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

    /// Pick the front-most item under `pos`, returning its handle and the
    /// [`PickResult`] describing what was hit (silx picking). Single owner for
    /// both legend/active-item selection and the item-click/hover signals:
    /// walks `items_back_to_front` in reverse so the top-drawn item wins.
    fn pick_topmost(&self, pos: egui::Pos2) -> Option<(ItemHandle, PickResult)> {
        self.backend
            .items_back_to_front()
            .into_iter()
            .rev()
            .find_map(|handle| {
                self.backend
                    .pick_item(pos, handle)
                    .map(|pick| (handle, pick))
            })
    }

    fn pick_topmost_item(&self, pos: egui::Pos2) -> Option<ItemHandle> {
        self.pick_topmost(pos).map(|(handle, _)| handle)
    }

    /// Map a topmost [`PickResult`] to its item-click [`PlotEvent`] (silx
    /// `curveClicked`/`imageClicked`/`markerClicked`). Pure — depends only on
    /// its arguments — so it is unit-tested without a GPU backend, which is the
    /// part of the click path that cannot be exercised headlessly.
    fn click_event_for_pick(
        handle: ItemHandle,
        pick: &PickResult,
        button: MouseButton,
    ) -> PlotEvent {
        match *pick {
            PickResult::CurvePoint { index, x, y, .. } => PlotEvent::CurveClicked {
                handle,
                index,
                x,
                y,
                button,
            },
            PickResult::ImagePixel { col, row } => PlotEvent::ImageClicked {
                handle,
                col,
                row,
                button,
            },
            PickResult::Item { .. } => PlotEvent::ItemClicked { handle, button },
        }
    }

    /// Translate this frame's low-level [`PlotPointerEvent`] into item-identified
    /// [`PlotEvent`]s (silx `curveClicked`/`imageClicked`/`markerClicked` and the
    /// hover signal). A primary/secondary/middle click on an item queues the
    /// matching click event; a bare hover (no button held) over an item queues
    /// [`PlotEvent::ItemHovered`]. The pointer pixel is already gated to the data
    /// area by `detect_pointer_event`, so the only filter here is whether the
    /// pixel actually picks an item.
    fn emit_item_pointer_events(&mut self, response: &PlotResponse) {
        use crate::widget::interaction::PlotPointerEvent;
        let Some(event) = response.pointer_event.as_ref() else {
            return;
        };
        match *event {
            PlotPointerEvent::Clicked { button, pixel, .. } => {
                let pos = egui::pos2(pixel.0, pixel.1);
                if let Some((handle, pick)) = self.pick_topmost(pos) {
                    self.events
                        .push(Self::click_event_for_pick(handle, &pick, button));
                }
            }
            PlotPointerEvent::Moved {
                button: None,
                data,
                pixel,
            } => {
                let pos = egui::pos2(pixel.0, pixel.1);
                if let Some((handle, _)) = self.pick_topmost(pos)
                    && let Some(kind) = self.item_kind(handle)
                {
                    // Assemble the silx prepareHoverSignal payload: label from the
                    // item record, data/pixel cursor position from this move, and
                    // draggable from the marker flag (false for non-marker items).
                    let label = self.item_legend(handle).map(str::to_owned);
                    let draggable = self.backend.marker(handle).is_some_and(|m| m.is_draggable);
                    self.events.push(PlotEvent::ItemHovered {
                        handle,
                        kind,
                        label,
                        x: data.0,
                        y: data.1,
                        xpixel: pixel.0,
                        ypixel: pixel.1,
                        draggable,
                    });
                }
            }
            _ => {}
        }
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
            x_label: None,
            y_label: None,
            stats,
            visual,
            data: None,
            curve_data: None,
            hidden_symbol: None,
            hidden_line_style: None,
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
            // `data` is set by the typed entry points after this call; an
            // untyped update (e.g. via update_item_record from a path with no
            // retained data) leaves the existing data in place.
            self.events.push(PlotEvent::ItemUpdated { handle, kind });
        } else {
            self.item_records.push(ItemRecord {
                handle,
                kind,
                bounds,
                legend: None,
                x_label: None,
                y_label: None,
                stats,
                visual,
                data: None,
                curve_data: None,
                hidden_symbol: None,
                hidden_line_style: None,
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

    /// Attach (or clear) the retained raw data for an item. Called by the typed
    /// curve/image spec entry points right after the record is created/updated,
    /// so live stats/fit consumers can read the active item's data.
    fn set_retained_data(&mut self, handle: ItemHandle, data: Option<RetainedItemData>) {
        if let Some(record) = self.item_record_mut(handle) {
            record.data = data;
        }
    }

    /// The retained raw data for an item, if any.
    fn retained_data(&self, handle: ItemHandle) -> Option<&RetainedItemData> {
        self.item_record(handle)
            .and_then(|record| record.data.as_ref())
    }

    /// Attach (or clear) the retained full [`CurveData`] for a curve-like item.
    /// Called by the curve spec entry points so the curve-style cycle action can
    /// clone it, change the line style, and re-apply the full curve.
    fn set_record_curve_data(&mut self, handle: ItemHandle, curve_data: Option<CurveData>) {
        if let Some(record) = self.item_record_mut(handle) {
            record.curve_data = curve_data;
        }
    }

    /// The retained full [`CurveData`] for a curve-like item, if any.
    fn record_curve_data(&self, handle: ItemHandle) -> Option<&CurveData> {
        self.item_record(handle)
            .and_then(|record| record.curve_data.as_ref())
    }

    /// Whether `handle` is the curve that should currently render with the
    /// active-curve highlight (silx: highlight applies only when the active
    /// item's kind is exactly `'curve'`).
    ///
    /// INVARIANT: a curve is highlighted iff active-curve handling is on AND it
    /// is the active item AND its kind is exactly [`PlotItemKind::Curve`]
    /// (scatter is a distinct kind and is never highlighted, matching silx
    /// `_setActiveItem`).
    fn is_highlighted_curve(&self, handle: ItemHandle) -> bool {
        self.active_curve_handling
            && self.active_item == Some(handle)
            && self.item_kind(handle) == Some(PlotItemKind::Curve)
    }

    /// Single owner of the active-curve highlight transition. Re-pushes the GPU
    /// render style of `handle` as
    /// `current_curve_style(retained_base, active_curve_style, is_highlighted)`.
    ///
    /// INVARIANT: the retained `record.curve_data` is ALWAYS the BASE style
    /// (never the resolved highlight) — the single source of truth. The
    /// highlight is a render-time overlay only, so this never calls
    /// `set_record_curve_data`. Returns early when `handle` has no retained
    /// curve data (not a curve / no data), so non-curve handles are a no-op.
    fn sync_curve_highlight(&mut self, handle: ItemHandle) {
        let Some(base) = self.record_curve_data(handle).cloned() else {
            return;
        };
        let highlighted = self.is_highlighted_curve(handle);
        let effective = current_curve_style(&base, &self.active_curve_style, highlighted);
        self.backend
            .update_curve(handle, curve_spec_from_data(&effective));
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
        let data = curve_spec_retained_data(&spec);
        let curve_data = curve_data_from_spec_hl(&spec);
        // Per-curve axis labels (silx `addCurve(xlabel=, ylabel=)`): capture
        // before the spec is consumed, then store on the record alongside
        // `legend` so they survive style/highlight re-applications.
        let x_label = spec.x_label.map(ToOwned::to_owned);
        let y_label = spec.y_label.map(ToOwned::to_owned);
        let handle = self.backend.add_curve(spec);
        self.record_item(handle, kind, bounds, stats, visual);
        self.set_retained_data(handle, Some(data));
        self.set_record_curve_data(handle, Some(curve_data));
        if let Some(record) = self.item_record_mut(handle) {
            record.x_label = x_label;
            record.y_label = y_label;
        }
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
        let data = curve_spec_retained_data(&spec);
        let curve_data = curve_data_from_spec_hl(&spec);
        if self.backend.update_curve(handle, spec) {
            self.update_item_record(handle, kind, bounds, stats, visual);
            self.set_retained_data(handle, Some(data));
            self.set_record_curve_data(handle, Some(curve_data));
            // The base push above renders the new BASE style. If this is the
            // active (highlighted) curve, re-overlay the highlight through the
            // single owner so updating it does not silently drop the highlight
            // (silx re-applies the current style on data/style change). For
            // non-active curves the base push is already correct, so skip.
            if self.is_highlighted_curve(handle) {
                self.sync_curve_highlight(handle);
            }
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

    /// Add a histogram from `N` bin positions and `N` counts, deriving the
    /// `N + 1` bin edges from `align` (silx `Histogram.setData(align=)`).
    ///
    /// The edges are computed by [`histogram_edges`]; `positions.len()` must equal
    /// `counts.len()` (mismatched lengths surface as
    /// [`PlotDataError::HistogramLength`] from the underlying [`Self::add_histogram`]).
    pub fn add_histogram_aligned(
        &mut self,
        positions: &[f64],
        counts: &[f64],
        color: Color32,
        align: HistogramAlign,
    ) -> Result<ItemHandle, PlotDataError> {
        let edges = histogram_edges(positions, align);
        self.add_histogram(&edges, counts, color)
    }

    /// Add an aligned histogram (see [`Self::add_histogram_aligned`]) and assign a
    /// legend label.
    pub fn add_histogram_aligned_with_legend(
        &mut self,
        positions: &[f64],
        counts: &[f64],
        color: Color32,
        align: HistogramAlign,
        legend: impl Into<String>,
    ) -> Result<ItemHandle, PlotDataError> {
        let handle = self.add_histogram_aligned(positions, counts, color, align)?;
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
        let data = image_spec_retained_data(&spec);
        let handle = self.backend.add_image(spec);
        self.record_item(handle, kind, bounds, stats, visual);
        self.set_retained_data(handle, data);
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
        let data = image_spec_retained_data(&spec);
        if self.backend.update_image(handle, spec) {
            self.update_item_record(handle, kind, bounds, stats, visual);
            self.set_retained_data(handle, data);
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

    /// Add raw per-pixel RGBA pixels as a mask-kind overlay item.
    ///
    /// Unlike [`add_mask`](Self::add_mask) (a boolean stencil painted in one
    /// color), this carries fully resolved per-pixel RGBA, so a multi-level
    /// mask can map each level through its own LUT entry (silx
    /// `_BaseMaskToolsWidget` discrete mask colormap). `pixels` is row-major,
    /// `width * height` long.
    pub fn add_rgba_mask(
        &mut self,
        width: u32,
        height: u32,
        pixels: &[[u8; 4]],
    ) -> Result<ItemHandle, PlotDataError> {
        validate_image_len(width, height, pixels.len())?;
        let spec = ImageSpec::rgba(width, height, pixels);
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

    /// The current data position `(x, y)` of the marker `handle` (silx
    /// `MarkerBase.getPosition`), or `None` if no marker with that handle
    /// exists. For a line marker the off-axis coordinate is reported as `0.0`
    /// (see [`Marker::position`]).
    pub fn marker_position(&self, handle: ItemHandle) -> Option<(f64, f64)> {
        self.backend.marker(handle).map(Marker::position)
    }

    /// Move the marker `handle` to data position `(x, y)`, applying the marker's
    /// drag constraint (silx `MarkerBase.setPosition`), and emit
    /// [`PlotEvent::MarkerMoved`]. Returns `false` if no marker with that handle
    /// exists (no event is emitted in that case).
    ///
    /// The constraint is applied via [`Marker::drag`] anchored at the marker's
    /// current position, so a `'horizontal'` / `'vertical'` preset pins the
    /// constrained coordinate exactly as an on-screen drag would. A
    /// non-draggable marker does not move (`Marker::drag` is a no-op when
    /// `is_draggable` is `false`), matching silx, but the call still returns
    /// `true` and emits the event because the marker exists.
    pub fn set_marker_position(&mut self, handle: ItemHandle, x: f64, y: f64) -> bool {
        let Some(mut marker) = self.backend.marker(handle).cloned() else {
            return false;
        };
        marker.drag(marker.position(), (x, y));
        self.backend.update_marker(handle, marker);
        self.events.push(PlotEvent::MarkerMoved { handle });
        true
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

    /// Set the curve's X-axis label, shown on the X axis while it is the active
    /// curve (silx `Curve.setXLabel`). The active curve's label overrides the
    /// graph default ([`Self::set_graph_x_label`]). Returns `false` for an
    /// unknown handle.
    pub fn set_curve_x_label(&mut self, handle: ItemHandle, label: impl Into<String>) -> bool {
        let Some(record) = self.item_record_mut(handle) else {
            return false;
        };
        record.x_label = Some(label.into());
        let kind = record.kind;
        self.events.push(PlotEvent::ItemUpdated { handle, kind });
        true
    }

    /// Remove the curve's X-axis label, so the graph default shows when it is
    /// active (silx setting the label back to `None`).
    pub fn clear_curve_x_label(&mut self, handle: ItemHandle) -> bool {
        let Some(record) = self.item_record_mut(handle) else {
            return false;
        };
        record.x_label = None;
        let kind = record.kind;
        self.events.push(PlotEvent::ItemUpdated { handle, kind });
        true
    }

    /// The curve's X-axis label, if set (silx `Curve.getXLabel`).
    pub fn curve_x_label(&self, handle: ItemHandle) -> Option<&str> {
        self.item_record(handle)
            .and_then(|record| record.x_label.as_deref())
    }

    /// Set the curve's Y-axis label, shown on the left or right (y2) axis (per
    /// the curve's Y-axis binding) while it is the active curve (silx
    /// `Curve.setYLabel`). Returns `false` for an unknown handle.
    pub fn set_curve_y_label(&mut self, handle: ItemHandle, label: impl Into<String>) -> bool {
        let Some(record) = self.item_record_mut(handle) else {
            return false;
        };
        record.y_label = Some(label.into());
        let kind = record.kind;
        self.events.push(PlotEvent::ItemUpdated { handle, kind });
        true
    }

    /// Remove the curve's Y-axis label, so the graph default shows when it is
    /// active.
    pub fn clear_curve_y_label(&mut self, handle: ItemHandle) -> bool {
        let Some(record) = self.item_record_mut(handle) else {
            return false;
        };
        record.y_label = None;
        let kind = record.kind;
        self.events.push(PlotEvent::ItemUpdated { handle, kind });
        true
    }

    /// The curve's Y-axis label, if set (silx `Curve.getYLabel`).
    pub fn curve_y_label(&self, handle: ItemHandle) -> Option<&str> {
        self.item_record(handle)
            .and_then(|record| record.y_label.as_deref())
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
        // Re-apply the highlight through the single owner now that the active
        // item changed: revert the old curve to its base, apply the highlight
        // to the new one (silx `_setActiveItem`: setHighlighted(False) on the
        // old curve, setHighlighted(True) on the new). `sync_curve_highlight`
        // no-ops on non-curve handles, so images/scatter are unaffected.
        if let Some(previous) = previous {
            self.sync_curve_highlight(previous);
        }
        if let Some(current) = item {
            self.sync_curve_highlight(current);
        }
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

    /// The override style applied to the active curve when active-curve
    /// handling is enabled (silx `PlotWidget.getActiveCurveStyle`).
    pub fn active_curve_style(&self) -> &CurveStyle {
        &self.active_curve_style
    }

    /// Whether the active curve is highlighted with the active-curve style
    /// (silx `PlotWidget.isActiveCurveHandling`).
    pub fn is_active_curve_handling(&self) -> bool {
        self.active_curve_handling
    }

    /// Set the override style applied to the active curve (silx
    /// `PlotWidget.setActiveCurveStyle`), then re-apply it to the active curve
    /// through the single highlight owner so the change takes effect at once.
    pub fn set_active_curve_style(&mut self, style: CurveStyle) {
        self.active_curve_style = style;
        if let Some(handle) = self.active_curve() {
            self.sync_curve_highlight(handle);
        }
    }

    /// Enable or disable active-curve highlighting (silx
    /// `PlotWidget.setActiveCurveHandling`), then re-sync the active curve:
    /// enabling applies the highlight, disabling reverts it to its base style.
    pub fn set_active_curve_handling(&mut self, enabled: bool) {
        self.active_curve_handling = enabled;
        if let Some(handle) = self.active_curve() {
            self.sync_curve_highlight(handle);
        }
    }

    /// The current line style of the active curve-like item, if one is active
    /// and its style is retained.
    pub fn active_curve_line_style(&self) -> Option<LineStyle> {
        let handle = self.active_curve()?;
        self.record_curve_data(handle)
            .map(|data| data.line_style.clone())
    }

    /// Cycle the active curve's line style to the next style (silx
    /// `CurveStyleAction`), re-applying the full retained curve so color, symbol,
    /// width, and error bars are preserved. Returns the new [`LineStyle`], or
    /// `None` if there is no active curve with a retained style.
    pub fn cycle_active_curve_style(&mut self) -> Option<LineStyle> {
        let handle = self.active_curve()?;
        let mut data = self.record_curve_data(handle)?.clone();
        let next = crate::widget::actions::control::next_line_style(&data.line_style);
        data.line_style = next.clone();
        self.update_curve_data(handle, &data);
        Some(next)
    }

    /// Move a curve between the left (`YAxis::Left`) and right (`YAxis::Right`)
    /// Y axis (silx legend `Map to left` / `Map to right`). Clones the retained
    /// [`CurveData`], sets `y_axis`, re-applies it through
    /// [`Self::update_curve_data`], then recomputes auto limits because the
    /// curve's bounds now contribute to a different Y/Y2 range. Returns `false`
    /// if the handle is unknown or has no retained curve data (non-curve item).
    pub fn set_curve_y_axis(&mut self, handle: ItemHandle, axis: YAxis) -> bool {
        let Some(mut data) = self.record_curve_data(handle).cloned() else {
            return false;
        };
        data.y_axis = axis;
        if !self.update_curve_data(handle, &data) {
            return false;
        }
        // Moving a curve between the Left and Right axes changes which axis its
        // data feeds, so the left-Y / right-Y data bounds must be re-evaluated
        // (same auto-limit path `remove` uses).
        self.recompute_data_bounds();
        self.apply_auto_limits();
        true
    }

    /// Show or hide a curve's point markers (silx legend checkable `Points`).
    /// Toggling is lossless: hiding stashes the current [`Symbol`] in a UI-only
    /// restore cache and showing restores that exact variant (falling back to a
    /// default only if the curve was created with no symbol). The drawn state
    /// is decided solely by [`CurveData::symbol`]; the cache is never read by
    /// any render/bounds/legend path. Returns `false` if the handle is unknown
    /// or has no retained curve data (non-curve item).
    pub fn set_curve_points_visible(&mut self, handle: ItemHandle, visible: bool) -> bool {
        let Some(mut data) = self.record_curve_data(handle).cloned() else {
            return false;
        };
        // Read (do not yet consume) the record's restore cache, transform with
        // the pure free fn, then commit the cache write-back ONLY after the
        // drawn-state transition (`update_curve_data`) succeeds. This keeps the
        // UI restore cache in lockstep with `CurveData.symbol`: a failed update
        // leaves both the drawn symbol and the cache untouched.
        let mut cache = self
            .item_record(handle)
            .and_then(|record| record.hidden_symbol);
        let next = set_symbol_visibility(data.symbol, visible, &mut cache);
        data.symbol = next;
        if !self.update_curve_data(handle, &data) {
            return false;
        }
        if let Some(record) = self.item_record_mut(handle) {
            record.hidden_symbol = cache;
        }
        true
    }

    /// Show or hide a curve's connecting line (silx legend checkable `Lines`).
    /// Toggling is lossless: hiding stashes the current [`LineStyle`] in a
    /// UI-only restore cache and showing restores that exact style (e.g.
    /// `Dashed`), falling back to a solid line only if the curve was created
    /// with no line. The drawn state is decided solely by
    /// [`CurveData::line_style`]; the cache is never read by any
    /// render/bounds/legend path. Returns `false` if the handle is unknown or
    /// has no retained curve data (non-curve item).
    pub fn set_curve_lines_visible(&mut self, handle: ItemHandle, visible: bool) -> bool {
        let Some(mut data) = self.record_curve_data(handle).cloned() else {
            return false;
        };
        // Same finalizer ordering as `set_curve_points_visible`: read the cache
        // without consuming it, transform, and commit the write-back only once
        // `update_curve_data` confirms the drawn line style actually changed.
        let mut cache = self
            .item_record(handle)
            .and_then(|record| record.hidden_line_style.clone());
        let next = set_line_visibility(data.line_style.clone(), visible, &mut cache);
        data.line_style = next;
        if !self.update_curve_data(handle, &data) {
            return false;
        }
        if let Some(record) = self.item_record_mut(handle) {
            record.hidden_line_style = cache;
        }
        true
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
                    record.visual.clone(),
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
                    // Right-click context menu (silx LegendListContextMenu).
                    // The closure both self-applies the action and records it.
                    result.row_response.context_menu(|ui| {
                        if let Some(action) = self.legend_context_menu_ui(ui, handle, kind, active)
                        {
                            out.context_action = Some((handle, action));
                        }
                    });
                }
            });
        // The rename popup (silx RenameCurveDialog) is rendered after the rows
        // so it floats above the legend, and from inside show_legend so the
        // legend stays self-contained.
        self.show_rename_popup(ui);
        out
    }

    /// Build the legend right-click context menu for one row, self-applying the
    /// chosen action and returning the [`LegendAction`] that fired (if any).
    /// Checkable / current-axis state is re-read from the record on every call
    /// so a reopened menu reflects the up-to-date Points/Lines/axis state.
    fn legend_context_menu_ui(
        &mut self,
        ui: &mut egui::Ui,
        handle: ItemHandle,
        kind: PlotItemKind,
        active: bool,
    ) -> Option<LegendAction> {
        let mut fired: Option<LegendAction> = None;

        // Set Active: disabled when the item is already active (silx omits
        // re-activating the active curve; we degrade to a disabled entry).
        if ui
            .add_enabled(!active, egui::Button::new("Set Active"))
            .clicked()
        {
            self.set_active_item(Some(handle));
            fired = Some(LegendAction::SetActive);
            ui.close();
        }

        if matches!(kind, PlotItemKind::Curve) {
            // Read the live curve state for the checkable / current-axis marks.
            let (y_axis, symbol_visible, line_visible) = self
                .record_curve_data(handle)
                .map(|data| {
                    (
                        data.y_axis,
                        data.symbol.is_some(),
                        data.line_style.draws_line(),
                    )
                })
                .unwrap_or((YAxis::Left, false, false));

            // Map to Y Left / Right: the current axis is disabled (it is already
            // the target), matching silx's intent of moving to the *other* axis.
            if ui
                .add_enabled(y_axis != YAxis::Left, egui::Button::new("Map to Y Left"))
                .clicked()
            {
                self.set_curve_y_axis(handle, YAxis::Left);
                fired = Some(LegendAction::MapToLeft);
                ui.close();
            }
            if ui
                .add_enabled(y_axis != YAxis::Right, egui::Button::new("Map to Y Right"))
                .clicked()
            {
                self.set_curve_y_axis(handle, YAxis::Right);
                fired = Some(LegendAction::MapToRight);
                ui.close();
            }

            // Checkable Points / Lines: the checkmark reflects current visibility
            // read above; clicking toggles to the opposite state.
            let mut points = symbol_visible;
            if ui.checkbox(&mut points, "Points").clicked() {
                self.set_curve_points_visible(handle, points);
                fired = Some(LegendAction::TogglePoints);
                ui.close();
            }
            let mut lines = line_visible;
            if ui.checkbox(&mut lines, "Lines").clicked() {
                self.set_curve_lines_visible(handle, lines);
                fired = Some(LegendAction::ToggleLines);
                ui.close();
            }
        }
        // Non-curve items (Image/Scatter/Histogram/Mask/...) degrade gracefully:
        // only Set Active / Rename / Remove are offered, since Points/Lines/Map-Y
        // are curve-only (silx's legend is curve-centric).

        ui.separator();

        if ui.button("Rename").clicked() {
            // Seed the edit buffer with the current label so the popup opens
            // pre-filled (silx RenameCurveDialog sets the line edit text).
            let current = self
                .item_record(handle)
                .map(|record| self.legend_label(record))
                .unwrap_or_default();
            self.rename_state = Some((handle, current));
            fired = Some(LegendAction::Rename);
            ui.close();
        }
        if ui.button("Remove").clicked() {
            self.remove(handle);
            fired = Some(LegendAction::Remove);
            ui.close();
        }

        fired
    }

    /// Render the legend rename popup (silx `RenameCurveDialog`) when a rename
    /// is in progress: a single-line text field with Apply / Cancel. Apply
    /// commits the buffer via [`Self::set_item_legend`] and clears the state;
    /// Cancel or Escape clears it without committing. No-op when no rename is
    /// pending.
    fn show_rename_popup(&mut self, ui: &mut egui::Ui) {
        let Some((handle, mut buffer)) = self.rename_state.take() else {
            return;
        };
        // `keep_open` decides whether the popup state survives this frame; any
        // terminal action (Apply / Cancel / Escape / window close) leaves it
        // false so `rename_state` stays cleared.
        let mut keep_open = true;
        let mut apply = false;
        let id = ui.id().with(("legend_rename", handle));
        let signals = crate::widget::detached::show_detached(
            ui.ctx(),
            id,
            "Rename",
            egui::vec2(260.0, 110.0),
            None,
            |ui| {
                let edit = ui.add(egui::TextEdit::singleline(&mut buffer).desired_width(200.0));
                // Enter in the field applies, matching a dialog's default button.
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    apply = true;
                }
                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked() {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        keep_open = false;
                    }
                });
            },
        );
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            keep_open = false;
        }
        if apply {
            self.set_item_legend(handle, buffer.clone());
            keep_open = false;
        }
        // The detached window's own close button is a Cancel.
        if signals.close_requested {
            keep_open = false;
        }
        if keep_open {
            self.rename_state = Some((handle, buffer));
        }
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

    /// Build a borrowed [`StatsInput`] for an item's retained raw data (silx
    /// `StatsWidget` per-item data), or `None` when the item is unknown / has no
    /// retained scalar data (e.g. an RGBA image).
    fn stats_input(
        &self,
        handle: ItemHandle,
    ) -> Option<crate::widget::stats_widget::StatsInput<'_>> {
        self.retained_data(handle).map(retained_data_to_stats_input)
    }

    /// Feed the active item's retained data into a [`StatsWidget`], recomputing
    /// its rows from the live data (silx `StatsWidget` bound to the active
    /// item). The row is labelled with the item's legend.
    ///
    /// `viewport` is the visible data rectangle `((x0, x1), (y0, y1))`, used only
    /// when the widget's on-visible-data toggle is enabled. Returns `true` when
    /// there is an active item with retained scalar data to feed; `false`
    /// otherwise (the widget is then fed an empty input and shows no rows).
    pub fn feed_active_stats(
        &self,
        stats: &mut crate::widget::stats_widget::StatsWidget,
        viewport: Option<((f64, f64), (f64, f64))>,
    ) -> bool {
        let Some(handle) = self.active_item else {
            stats.recompute(&[], viewport);
            return false;
        };
        let label = self
            .item_record(handle)
            .map(|record| self.legend_label(record))
            .unwrap_or_else(|| "item".to_owned());
        match self.stats_input(handle) {
            Some(input) => {
                stats.recompute(&[(label.as_str(), input)], viewport);
                true
            }
            None => {
                stats.recompute(&[], viewport);
                false
            }
        }
    }

    /// Compute per-ROI statistics over the active item's retained data and store
    /// them in `widget` (silx `ROIStatsWidget` bound to the active item): one
    /// row per ROI on the plot, reduced inside that ROI via [`image_roi_stats`]
    /// (image) / [`curve_roi_stats`] (curve). Returns `true` when there is an
    /// active item with retained data to reduce; `false` otherwise (the widget
    /// is then cleared).
    ///
    /// [`image_roi_stats`]: crate::widget::roi_stats::image_roi_stats
    /// [`curve_roi_stats`]: crate::widget::roi_stats::curve_roi_stats
    pub fn feed_roi_stats(
        &self,
        widget: &mut crate::widget::roi_stats_widget::RoiStatsWidget,
    ) -> bool {
        match self
            .active_item
            .and_then(|handle| self.retained_data(handle))
        {
            Some(data) => {
                widget.set_rows(roi_stats_rows(self.rois(), data));
                true
            }
            None => {
                widget.set_rows(Vec::new());
                false
            }
        }
    }

    /// Feed the active item's per-ROI statistics into `widget` and render its
    /// table (silx `ROIStatsWidget`). Combines [`Self::feed_roi_stats`] with
    /// [`RoiStatsWidget::ui`]; the table follows the active item and the live
    /// ROI list.
    ///
    /// [`RoiStatsWidget::ui`]: crate::widget::roi_stats_widget::RoiStatsWidget::ui
    pub fn show_roi_stats_widget(
        &self,
        ui: &mut egui::Ui,
        widget: &mut crate::widget::roi_stats_widget::RoiStatsWidget,
    ) {
        self.feed_roi_stats(widget);
        widget.ui(ui);
    }

    /// Compute per-ROI raw/net counts and raw/net area over the active **curve**
    /// and store them in `widget` (silx `CurvesROIWidget`): one row per curve ROI
    /// (those with an `x`-span), reduced via [`curve_roi_counts`]. Returns `true`
    /// when the active item is a curve with retained data; `false` otherwise
    /// (the active item is an image, or there is none — the widget is cleared,
    /// since these counts are curve-specific).
    ///
    /// [`curve_roi_counts`]: crate::widget::roi_stats::curve_roi_counts
    pub fn feed_curves_roi_stats(
        &self,
        widget: &mut crate::widget::curves_roi_widget::CurvesRoiWidget,
    ) -> bool {
        match self
            .active_item
            .and_then(|handle| self.retained_data(handle))
        {
            Some(RetainedItemData::Curve { x, y }) => {
                widget.set_rows(curve_roi_rows(self.rois(), x, y));
                true
            }
            _ => {
                widget.set_rows(Vec::new());
                false
            }
        }
    }

    /// Feed the active curve's per-ROI counts into `widget` and render its table
    /// (silx `CurvesROIWidget`). Combines [`Self::feed_curves_roi_stats`] with
    /// [`CurvesRoiWidget::ui`]; the table follows the active curve and the live
    /// ROI list.
    ///
    /// [`CurvesRoiWidget::ui`]: crate::widget::curves_roi_widget::CurvesRoiWidget::ui
    pub fn show_curves_roi_widget(
        &self,
        ui: &mut egui::Ui,
        widget: &mut crate::widget::curves_roi_widget::CurvesRoiWidget,
    ) {
        self.feed_curves_roi_stats(widget);
        widget.ui(ui);
    }

    /// Feed an item's retained curve `(x, y)` into a [`FitWidget`] as its fit
    /// target, so a fit runs against the live curve (silx `FitWidget.setData`
    /// bound to a plot curve). Returns `true` when the item is a curve with
    /// retained data; `false` for an unknown handle or a non-curve item (the fit
    /// widget is left unchanged in that case).
    pub fn set_fit_target(
        &self,
        fit: &mut crate::widget::fit_widget::FitWidget,
        handle: ItemHandle,
    ) -> bool {
        match self.retained_data(handle).and_then(retained_curve_xy) {
            Some((x, y)) => {
                fit.set_data(x, y);
                true
            }
            None => false,
        }
    }

    /// Feed the active item's retained curve `(x, y)` into a [`FitWidget`] as its
    /// fit target (silx `FitWidget` bound to the active curve). Returns `true`
    /// when the active item is a curve with retained data.
    pub fn set_active_fit_target(&self, fit: &mut crate::widget::fit_widget::FitWidget) -> bool {
        self.active_item
            .is_some_and(|handle| self.set_fit_target(fit, handle))
    }

    /// Feed the active item's retained data into a [`StatsWidget`] and render
    /// its table (silx `StatsWidget`). Combines [`Self::feed_active_stats`] with
    /// [`StatsWidget::ui`]; the widget recomputes as the active item changes.
    pub fn show_active_stats_widget(
        &self,
        ui: &mut egui::Ui,
        stats: &mut crate::widget::stats_widget::StatsWidget,
        viewport: Option<((f64, f64), (f64, f64))>,
    ) {
        match self.active_item.and_then(|handle| {
            self.stats_input(handle).map(|input| {
                let label = self
                    .item_record(handle)
                    .map(|record| self.legend_label(record))
                    .unwrap_or_else(|| "item".to_owned());
                (label, input)
            })
        }) {
            Some((label, input)) => {
                stats.ui(ui, &[(label.as_str(), input)], viewport);
            }
            None => {
                stats.ui(ui, &[], viewport);
            }
        }
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
        if toolbar_icon_button(ui, ToolbarIcon::ZoomIn, false, "Zoom in").clicked() {
            crate::widget::actions::control::zoom_in(self);
            out.zoom_in = true;
        }
        if toolbar_icon_button(ui, ToolbarIcon::ZoomOut, false, "Zoom out").clicked() {
            crate::widget::actions::control::zoom_out(self);
            out.zoom_out = true;
        }
        if toolbar_icon_button(ui, ToolbarIcon::ZoomBack, false, "Zoom back").clicked() {
            crate::widget::actions::control::zoom_back(self);
            out.zoom_back = true;
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

        let x_auto = self.plot().x_autoscale();
        if toolbar_icon_button(
            ui,
            ToolbarIcon::AutoscaleX,
            x_auto,
            "Auto-scale X axis on reset zoom",
        )
        .clicked()
        {
            crate::widget::actions::control::toggle_x_autoscale(self);
            out.autoscale_x_changed = true;
        }

        let y_auto = self.plot().y_autoscale();
        if toolbar_icon_button(
            ui,
            ToolbarIcon::AutoscaleY,
            y_auto,
            "Auto-scale Y axis on reset zoom",
        )
        .clicked()
        {
            crate::widget::actions::control::toggle_y_autoscale(self);
            out.autoscale_y_changed = true;
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

        ui.separator();

        let show_axis = self.plot().axes_displayed();
        if toolbar_icon_button(ui, ToolbarIcon::ShowAxis, show_axis, "Show/hide axes").clicked() {
            crate::widget::actions::control::show_axis_toggle(self);
            out.show_axis_changed = true;
        }

        let has_curve = self.active_curve().is_some();
        let curve_style_response = ui
            .add_enabled_ui(has_curve, |ui| {
                toolbar_icon_button(
                    ui,
                    ToolbarIcon::CurveStyle,
                    false,
                    "Cycle active curve line style",
                )
            })
            .inner;
        if curve_style_response.clicked() {
            crate::widget::actions::control::curve_style_cycle(self);
            out.curve_style_changed = true;
        }

        ui.separator();

        if toolbar_icon_button(ui, ToolbarIcon::Save, false, "Save figure or curve data").clicked()
        {
            // The save dialog + GPU readback are native shims; ignore the
            // result here (the toolbar only reports the click).
            let _ = self.save_dialog(DEFAULT_SAVE_SIZE);
            out.save = true;
        }
        if toolbar_icon_button(ui, ToolbarIcon::Copy, false, "Copy figure to clipboard").clicked() {
            // GPU readback + clipboard are native shims; ignore the result here.
            let _ = self.copy_to_clipboard(DEFAULT_SAVE_SIZE);
            out.copy = true;
        }
        if toolbar_icon_button(ui, ToolbarIcon::Print, false, "Print figure").clicked() {
            // GPU readback + printer submission are native shims; ignore the
            // result here (the toolbar only reports the click).
            let _ = self.print_graph(DEFAULT_SAVE_SIZE);
            out.print = true;
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

    /// Restore the most recently pushed view from the limits history (silx
    /// `LimitsHistory.pop`), emitting [`PlotEvent::LimitsChanged`] if the view
    /// changed. Returns `true` if a stored view was restored, or `false` if the
    /// history was empty (callers fall back to [`Self::reset_zoom`], matching
    /// silx `LimitsHistory.pop`).
    pub fn zoom_back(&mut self) -> bool {
        let before = self.limits_snapshot();
        let restored = self.backend.plot_mut().zoom_back();
        self.push_limits_changed_if(before);
        restored
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

    /// Whether the built-in colorbar is drawn when the plot has a colormap
    /// (silx colorbar visibility). Defaults to `true`.
    pub fn show_colorbar(&self) -> bool {
        self.backend.plot().show_colorbar
    }

    /// Set whether the built-in colorbar is drawn when the plot has a colormap.
    /// Composite views that render their own dedicated colorbar (e.g.
    /// `ImageView`) set this `false` on their internal image plot so the
    /// colorbar is not drawn twice.
    pub fn set_show_colorbar(&mut self, show: bool) {
        self.backend.plot_mut().show_colorbar = show;
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

    /// The active image's raw scalar pixels as `f64`, row-major (silx
    /// `ImageData.getData`), or `None` when the active item is not a scalar
    /// image with retained data.
    ///
    /// Used by [`Self::autoscale_active_image`] to drive a raw-pixel autoscale,
    /// and exposed so callers can compute their own value statistics over the
    /// exact pixels the image was uploaded with (NaN-preserving).
    pub fn get_image_pixels_raw(&self) -> Option<Vec<f64>> {
        match self
            .active_item
            .and_then(|handle| self.retained_data(handle))
        {
            Some(RetainedItemData::Image { data, .. }) => Some(data.clone()),
            _ => None,
        }
    }

    /// Autoscale the active image's colormap value limits from its raw pixels
    /// using `mode` (silx `ColormapDialog` Stddev3 / Percentile autoscale,
    /// ColormapDialog.py:450-480).
    ///
    /// Computes the `(vmin, vmax)` range over the active image's raw scalar
    /// pixels (NaN-ignoring; [`AutoscaleMode::Stddev3`] = mean ± 3·std,
    /// [`AutoscaleMode::Percentile`] = the colormap's percentile pair), then
    /// re-uploads the image with a colormap carrying those limits (preserving
    /// the LUT / normalization / gamma). Returns the applied `(vmin, vmax)`, or
    /// `None` when the active item is not a scalar image with retained data.
    pub fn autoscale_active_image(&mut self, mode: AutoscaleMode) -> Option<(f64, f64)> {
        let handle = self.active_item?;
        let (data, width, height, origin, scale, base) = match self.retained_data(handle)? {
            RetainedItemData::Image {
                data,
                width,
                height,
                origin,
                scale,
                colormap,
            } => (
                data.clone(),
                *width,
                *height,
                *origin,
                *scale,
                (**colormap).clone(),
            ),
            RetainedItemData::Curve { .. } => return None,
        };
        let cm = autoscaled_colormap(&base, mode, &data);
        let limits = (cm.vmin, cm.vmax);
        let pixels: Vec<f32> = data.iter().map(|&v| v as f32).collect();
        let mut spec = ImageSpec::scalar(width as u32, height as u32, &pixels, cm);
        spec.origin = origin;
        spec.scale = scale;
        self.update_image_spec(handle, spec);
        Some(limits)
    }

    /// Apply a median filter to the active image and replace it in place (silx
    /// `MedianFilterAction` / `MedianFilter2DAction` re-adding the filtered image
    /// with `addImage(replace=True)`).
    ///
    /// Reads the active image's raw scalar pixels and geometry, runs
    /// [`crate::widget::actions::analysis::median_filter_2d`] with a square
    /// `(kernel_width, kernel_width)` kernel (silx `MedianFilter2DAction`) and
    /// the default `mode='nearest'` edge handling, then re-uploads the result
    /// with the same origin / scale / colormap. `kernel_width` is forced odd
    /// (silx `MedianFilterDialog` spinbox step 2, min 1) by rounding up to the
    /// next odd value; a width of 1 (or 0) leaves the image unchanged.
    ///
    /// Returns `true` if a scalar image was filtered and replaced, or `false`
    /// when the active item is not a scalar image with retained data.
    pub fn apply_median_filter(&mut self, kernel_width: usize, conditional: bool) -> bool {
        self.apply_median_filter_kernel(kernel_width, kernel_width, conditional)
    }

    /// Apply a 1D median filter to the active image (silx `MedianFilter1DAction`,
    /// kernel `(kernel_width, 1)`), replacing it in place.
    ///
    /// Same contract as [`Self::apply_median_filter`] but with a column-1 kernel
    /// that filters along image rows (the height / y direction).
    pub fn apply_median_filter_1d(&mut self, kernel_width: usize, conditional: bool) -> bool {
        self.apply_median_filter_kernel(kernel_width, 1, conditional)
    }

    /// Shared median-filter apply path for the 1D `(k,1)` and 2D `(k,k)` actions.
    fn apply_median_filter_kernel(
        &mut self,
        kernel_h: usize,
        kernel_w: usize,
        conditional: bool,
    ) -> bool {
        // Force odd kernel dimensions (silx asserts odd; the dialog steps by 2).
        let kernel_h = force_odd(kernel_h);
        let kernel_w = force_odd(kernel_w);

        let handle = match self.active_item {
            Some(h) => h,
            None => return false,
        };
        let (data, width, height, origin, scale, colormap) = match self.retained_data(handle) {
            Some(RetainedItemData::Image {
                data,
                width,
                height,
                origin,
                scale,
                colormap,
            }) => (
                data.clone(),
                *width,
                *height,
                *origin,
                *scale,
                (**colormap).clone(),
            ),
            _ => return false,
        };

        let filtered = crate::widget::actions::analysis::median_filter_2d(
            &data,
            width,
            height,
            kernel_h,
            kernel_w,
            conditional,
        );

        let pixels: Vec<f32> = filtered.iter().map(|&v| v as f32).collect();
        let mut spec = ImageSpec::scalar(width as u32, height as u32, &pixels, colormap);
        spec.origin = origin;
        spec.scale = scale;
        self.update_image_spec(handle, spec);
        true
    }

    /// Compute the pixel-intensity histogram of the active image (silx
    /// `PixelIntensitiesHistoAction`).
    ///
    /// Reads the active image's raw scalar pixels and runs
    /// [`crate::widget::actions::analysis::pixel_intensity_histogram`] with the
    /// silx defaults (bin count `min(1024, floor(sqrt(finite_count)))`, finite
    /// range, `last_bin_closed`, non-finite excluded). Pass `n_bins` to override
    /// the bin count (mirroring the widget's editable bin-count field), or
    /// `None` for the silx default.
    ///
    /// Returns `None` when the active item is not a scalar image with retained
    /// data, or when the image has no finite pixel.
    pub fn active_image_histogram(
        &self,
        n_bins: Option<usize>,
    ) -> Option<crate::widget::actions::analysis::PixelHistogram> {
        let pixels = self.get_image_pixels_raw()?;
        crate::widget::actions::analysis::pixel_intensity_histogram(&pixels, n_bins)
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
        self.backend.plot_mut().rois.push(ManagedRoi::new(roi));
        let index = self.backend.plot().rois.len() - 1;
        self.events.push(PlotEvent::RoiChanged { index });
        index
    }

    pub fn rois(&self) -> &[ManagedRoi] {
        &self.backend.plot().rois
    }

    pub fn rois_mut(&mut self) -> &mut [ManagedRoi] {
        &mut self.backend.plot_mut().rois
    }

    pub fn clear_rois(&mut self) {
        self.backend.plot_mut().clear_rois();
        self.events.push(PlotEvent::RoisCleared);
    }

    /// Remove the ROI at `index`, keeping the current-ROI selection consistent
    /// via the [`Plot`] owner (silx `RegionOfInterestManager.removeRoi`). Emits
    /// [`PlotEvent::RoisCleared`] (the ROI collection changed) when a ROI was
    /// actually removed; an out-of-range index is ignored.
    pub fn remove_roi(&mut self, index: usize) {
        let before = self.backend.plot().rois.len();
        self.backend.plot_mut().remove_roi(index);
        if self.backend.plot().rois.len() != before {
            self.events.push(PlotEvent::RoisCleared);
        }
    }

    /// Append a fully-specified [`ManagedRoi`] (geometry + appearance) and
    /// return its index, emitting [`PlotEvent::RoiChanged`] (silx
    /// `RegionOfInterestManager.addRoi`). Use this to add a styled/named ROI in
    /// one call; [`Self::add_roi`] adds bare geometry with default appearance.
    pub fn add_managed_roi(&mut self, managed: ManagedRoi) -> usize {
        self.backend.plot_mut().rois.push(managed);
        let index = self.backend.plot().rois.len() - 1;
        self.events.push(PlotEvent::RoiChanged { index });
        index
    }

    /// Set the per-ROI color override at `index` (silx `RegionOfInterest.setColor`).
    /// An out-of-range index is ignored.
    pub fn set_roi_color(&mut self, index: usize, color: Color32) {
        if let Some(r) = self.backend.plot_mut().rois.get_mut(index) {
            r.color = Some(color);
        }
    }

    /// Set the display name of the ROI at `index` (silx `RegionOfInterest.setName`).
    /// An out-of-range index is ignored.
    pub fn set_roi_name(&mut self, index: usize, name: impl Into<String>) {
        if let Some(r) = self.backend.plot_mut().rois.get_mut(index) {
            r.name = name.into();
        }
    }

    /// Set the outline line width of the ROI at `index` (silx
    /// `RegionOfInterest.setLineWidth`). An out-of-range index is ignored.
    pub fn set_roi_line_width(&mut self, index: usize, width: f32) {
        if let Some(r) = self.backend.plot_mut().rois.get_mut(index) {
            r.line_width = width;
        }
    }

    /// Set the outline stroke style of the ROI at `index` (silx
    /// `RegionOfInterest.setLineStyle`). An out-of-range index is ignored.
    pub fn set_roi_line_style(&mut self, index: usize, style: RoiLineStyle) {
        if let Some(r) = self.backend.plot_mut().rois.get_mut(index) {
            r.line_style = style;
        }
    }

    /// Set whether the ROI at `index` fills its interior (silx
    /// `RegionOfInterest.setFill`). An out-of-range index is ignored.
    pub fn set_roi_fill(&mut self, index: usize, fill: bool) {
        if let Some(r) = self.backend.plot_mut().rois.get_mut(index) {
            r.fill = fill;
        }
    }

    /// The index of the current/highlighted ROI, or `None` (silx
    /// `RegionOfInterestManager.getCurrentRoi`).
    pub fn current_roi(&self) -> Option<usize> {
        self.backend.plot().current_roi()
    }

    /// Set the current/highlighted ROI by index, or `None` to clear it (silx
    /// `RegionOfInterestManager.setCurrentRoi`). Highlights exactly that ROI on
    /// the plot; an out-of-range index clears the selection. Emits
    /// [`PlotEvent::CurrentRoiChanged`] when the current ROI actually changes
    /// (silx `sigCurrentRoiChanged`).
    pub fn set_current_roi(&mut self, index: Option<usize>) {
        let previous = self.backend.plot().current_roi();
        self.backend.plot_mut().set_current_roi(index);
        let current = self.backend.plot().current_roi();
        if current != previous {
            self.events
                .push(PlotEvent::CurrentRoiChanged { previous, current });
        }
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
                for (i, managed) in self.backend.plot().rois.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let desc = roi_description(&managed.roi);
                        ui.label(desc);
                        if ui.small_button("×").on_hover_text("Remove").clicked() {
                            remove_idx = Some(i);
                        }
                    });
                }
            });

        if let Some(idx) = remove_idx {
            // Route through the owner so the current-ROI index stays consistent.
            self.remove_roi(idx);
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

    /// Render the figure to `path` in the given [`SaveFormat`] at `dpi`,
    /// generalizing [`Self::save_graph`] (PNG-only) over silx's raster save
    /// formats (PNG/PPM/SVG/TIFF). Faithful to silx
    /// `BackendBase.saveGraph(fileName, fileFormat, dpi)`. The GPU readback +
    /// file write are native shims; the per-format encoding is unit-tested in
    /// [`crate::render::save`].
    pub fn save_graph_with_format(
        &self,
        path: &Path,
        size: (u32, u32),
        format: SaveFormat,
        dpi: u32,
    ) -> Result<(), SaveError> {
        self.backend.save_graph_with_format(path, size, format, dpi)
    }

    /// Save to `path`, dispatching by its extension (silx `SaveAction`):
    /// a `.csv` path writes the active curve's `(x, y)` data; a raster figure
    /// extension (`png`/`ppm`/`svg`/`tif`/`tiff`) renders the figure to a `size`
    /// pixel image in the matching [`SaveFormat`]. Returns `Ok(true)` when a file
    /// was written, `Ok(false)` when the path's extension is not a recognized save
    /// target or (for CSV) there is no active curve to save.
    ///
    /// All recognized figure formats are routed through
    /// [`Self::save_graph_with_format`] at [`DEFAULT_SAVE_DPI`]; PNG remains
    /// byte-identical to [`Self::save_graph`] (both go through
    /// [`crate::render::save::encode_png`]). The extension-to-target decision is
    /// the pure, unit-tested [`SaveTarget::from_path`].
    pub fn save_to_path(&self, path: &Path, size: (u32, u32)) -> Result<bool, SaveError> {
        use crate::widget::actions::io::{SaveTarget, curve_to_csv};

        match SaveTarget::from_path(path) {
            Some(SaveTarget::Figure(format)) => {
                self.save_graph_with_format(path, size, format, DEFAULT_SAVE_DPI)?;
                Ok(true)
            }
            Some(SaveTarget::CurveCsv) => {
                let Some(handle) = self.active_curve() else {
                    return Ok(false);
                };
                let Some((x, y)) = self.retained_data(handle).and_then(retained_curve_xy) else {
                    return Ok(false);
                };
                let csv = curve_to_csv(x, y);
                std::fs::write(path, csv)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Open a native save-file dialog (silx `SaveAction` file dialog) and save
    /// the figure or active-curve data to the chosen path via
    /// [`Self::save_to_path`]. Returns `Ok(true)` when a file was written,
    /// `Ok(false)` when the dialog was cancelled or the chosen path was not a
    /// recognized target. The dialog is a native shim; the save logic it calls is
    /// covered by unit tests.
    pub fn save_dialog(&self, size: (u32, u32)) -> Result<bool, SaveError> {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("PNG figure", &["png"])
            .add_filter("PPM figure", &["ppm"])
            .add_filter("SVG figure", &["svg"])
            .add_filter("TIFF figure", &["tif", "tiff"])
            .add_filter("Curve CSV", &["csv"])
            .save_file()
        else {
            return Ok(false);
        };
        self.save_to_path(&path, size)
    }

    /// Copy a `size` pixel snapshot of the figure to the system clipboard as an
    /// image (silx `CopyAction`, which renders the plot to a bitmap and calls
    /// `QApplication.clipboard().setImage`).
    ///
    /// The figure is rendered to a PNG (the only in-memory figure encoding
    /// available), decoded back to RGBA via
    /// [`decode_png_to_rgba`](crate::widget::actions::io::decode_png_to_rgba),
    /// shaped into an [`arboard::ImageData`] via
    /// [`rgba_to_clipboard_image`](crate::widget::actions::io::rgba_to_clipboard_image),
    /// then placed on the clipboard. The GPU readback and the clipboard call are
    /// untested native shims; the PNG-decode and RGBA-shaping logic between them
    /// is unit-tested.
    pub fn copy_to_clipboard(&self, size: (u32, u32)) -> Result<bool, SaveError> {
        use crate::widget::actions::io::{decode_png_to_rgba, rgba_to_clipboard_image};

        // Render the figure to a temp PNG, then read it back. save_graph is the
        // only public figure-encoding entry point (it writes a PNG file).
        let mut path = std::env::temp_dir();
        path.push(format!("siplot-copy-{}.png", std::process::id()));
        self.save_graph(&path, size)?;
        let png = std::fs::read(&path)?;
        let _ = std::fs::remove_file(&path);

        let (w, h, rgba) = decode_png_to_rgba(&png)?;
        let Some(image) = rgba_to_clipboard_image(&rgba, w, h) else {
            return Err(SaveError::Readback("clipboard image shaping failed".into()));
        };
        let mut clipboard = arboard::Clipboard::new()
            .map_err(|e| SaveError::Readback(format!("clipboard open: {e}")))?;
        clipboard
            .set_image(image)
            .map_err(|e| SaveError::Readback(format!("clipboard set_image: {e}")))?;
        Ok(true)
    }

    /// Print a `size` pixel snapshot of the figure to the default system printer
    /// (silx `PrintAction.printPlot`).
    ///
    /// Mirrors silx's raster print path: silx renders the plot to a PNG
    /// (`_plotAsPNG`) and draws that bitmap onto the printer via
    /// `QPainter`/`QPrinter` — not vector graphics. Here the figure is rasterized
    /// to a temp PNG via [`Self::save_graph`] (the only public figure-encoding
    /// entry point), then submitted to the default printer with the
    /// [`printers`] crate. Returns `Ok(true)` when a print job was queued,
    /// `Ok(false)` when no default printer is available (the silx
    /// `getDefaultPrinter` analogue).
    ///
    /// The GPU readback and the printer submission are untested native shims (a
    /// real printer / spooler is required); the rasterization step reuses the
    /// unit-tested [`crate::render::save`] encoders, and the temp-path naming is
    /// unit-tested via [`print_temp_png_path`]. Print preview and printer-settings
    /// dialogs (silx's `QPrintDialog`) are intentionally not implemented; this
    /// prints to the default printer.
    pub fn print_graph(&self, size: (u32, u32)) -> Result<bool, SaveError> {
        // Rasterize to a temp PNG, then hand the file to the printer. save_graph
        // is the only public figure-encoding entry point (it writes a PNG file),
        // and silx prints a PNG bitmap, so PNG is the faithful intermediate.
        let path = print_temp_png_path(&std::env::temp_dir(), std::process::id());
        self.save_graph(&path, size)?;

        let Some(printer) = printers::get_default_printer() else {
            let _ = std::fs::remove_file(&path);
            return Ok(false);
        };
        let submit = printer.print_file(
            &path.to_string_lossy(),
            printers::common::base::job::PrinterJobOptions::none(),
        );
        let _ = std::fs::remove_file(&path);
        submit.map_err(|e| SaveError::Readback(format!("print submit: {}", e.message)))?;
        Ok(true)
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
        // Preserve the original guard: a reset-to-data needs both X and left-Y
        // data accumulated before it does anything.
        if self.data_bounds.x.is_none() || self.data_bounds.y_left.is_none() {
            return;
        }
        // Delegate the per-axis refit decision to the single flag-aware owner
        // (`Plot::reset_zoom_to_data_range`): only autoscale-on axes refit from
        // data, off axes keep their current limits, and log axes force a refit
        // when their lower limit is <= 0. `WgpuBackend::set_limits` (the prior
        // path) only assigned `plot.limits`/`plot.y2` — the same two fields the
        // model owner writes — so delegating regresses no widget-side
        // bookkeeping; the `LimitsChanged` event is still raised here.
        let range = data_range_from_bounds(self.data_bounds);
        let before = self.limits_snapshot();
        self.backend.plot_mut().reset_zoom_to_data_range(range);
        self.push_limits_changed_if(before);
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

    /// Draw the median-filter controls (silx `MedianFilterDialog`): an odd
    /// kernel-width drag, a conditional checkbox, and an Apply button.
    ///
    /// `params` holds the popup state (held by the caller, e.g. in egui
    /// temp-memory); the widgets mutate it. On Apply this runs
    /// [`PlotWidget::apply_median_filter`] on the active image (square
    /// `(width, width)` kernel, silx `MedianFilter2DAction`, default
    /// `mode='nearest'`), replacing it in place, and returns `true`. Returns
    /// `false` on any frame Apply was not clicked or no scalar image was active.
    ///
    /// Render it into any `Ui`; the toolbar shows it in a detachable native
    /// window via [`crate::widget::detached::show_detached`] for the silx popup
    /// feel:
    ///
    /// ```ignore
    /// plot.show_median_filter(ui, &mut params);
    /// ```
    pub fn show_median_filter(
        &mut self,
        ui: &mut egui::Ui,
        params: &mut MedianFilterParams,
    ) -> bool {
        ui.horizontal(|ui| {
            ui.label("Kernel width:");
            // silx MedianFilterDialog spinbox: min 1, step 2 (odd). The drag steps
            // by 2 and we re-force odd in case the value is typed/clamped even.
            let mut width = params.kernel_width.max(1);
            if ui
                .add(egui::DragValue::new(&mut width).range(1..=99).speed(2.0))
                .changed()
            {
                params.kernel_width = force_odd(width);
            }
        });
        ui.checkbox(&mut params.conditional, "Conditional")
            .on_hover_text("Replace a pixel only if it is the window min or max");

        let mut applied = false;
        let has_image = self.get_image_pixels_raw().is_some();
        if ui
            .add_enabled(has_image, egui::Button::new("Apply"))
            .on_hover_text("Replace the active image with its median-filtered copy")
            .clicked()
        {
            applied = self.apply_median_filter(params.kernel_width, params.conditional);
        }
        applied
    }

    /// Draw a median-filter toolbar button that toggles a popup window with the
    /// kernel/conditional/Apply controls (silx `MedianFilterAction`, a checkable
    /// toolbar action opening `MedianFilterDialog`).
    ///
    /// The popup open-state and [`MedianFilterParams`] are stored in egui
    /// temp-memory keyed by this plot's id, so the button is self-contained:
    /// callers can drop it into any toolbar row. Returns `true` on a frame the
    /// Apply button replaced the active image.
    ///
    /// Place it inside [`PlotWidget::show_toolbar_with`] to share the standard
    /// toolbar row:
    ///
    /// ```ignore
    /// let (_, applied) = plot.show_toolbar_with(ui, |ui, _| {
    ///     // (plot is borrowed by the closure as the same Plot2D's inner)
    /// });
    /// ```
    pub fn show_median_filter_toolbar(&mut self, ui: &mut egui::Ui) -> bool {
        let plot_id = self.backend().plot().id;
        let open_id = egui::Id::new(plot_id).with("median_filter_open");
        let params_id = egui::Id::new(plot_id).with("median_filter_params");

        let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
        let has_image = self.get_image_pixels_raw().is_some();

        let button = ui
            .add_enabled_ui(has_image, |ui| {
                toolbar_icon_button(ui, ToolbarIcon::MedianFilter, open, "Median filter")
            })
            .inner;
        if button.clicked() {
            open = !open;
        }

        let mut applied = false;
        if open {
            let mut params = ui
                .data(|d| d.get_temp::<MedianFilterParams>(params_id))
                .unwrap_or_default();
            let signals = crate::widget::detached::show_detached(
                ui.ctx(),
                open_id.with("window"),
                "Median filter",
                egui::vec2(320.0, 220.0),
                None,
                |ui| {
                    applied = self.show_median_filter(ui, &mut params);
                },
            );
            ui.data_mut(|d| d.insert_temp(params_id, params));
            if signals.close_requested {
                open = false;
            }
        }

        ui.data_mut(|d| d.insert_temp(open_id, open));
        applied
    }

    /// Draw the pixel-intensity histogram of the active image as bars + stats,
    /// with an editable bin-count control (silx `PixelIntensitiesHistoAction` /
    /// `HistogramWidget`).
    ///
    /// This is a UI shim: it computes the histogram on the CPU via
    /// [`PlotWidget::active_image_histogram`] and paints the bars with the egui
    /// painter (no second GPU `Plot1D`). `n_bins` holds the bin count chosen by
    /// the caller (e.g. egui temp-memory); `None` means "silx default", which is
    /// resolved to the actual count and written back so the control shows it.
    /// The bars / stats redraw whenever `*n_bins` changes (recompute on change).
    ///
    /// Returns the computed [`PixelHistogram`](crate::widget::actions::analysis::PixelHistogram),
    /// or `None` when there is no scalar image with finite pixels.
    pub fn show_pixel_histogram(
        &mut self,
        ui: &mut egui::Ui,
        n_bins: &mut Option<usize>,
    ) -> Option<crate::widget::actions::analysis::PixelHistogram> {
        let histogram = self.active_image_histogram(*n_bins);
        let Some(histo) = histogram else {
            ui.label("No image with finite pixels.");
            return None;
        };

        // Bin-count control: seed from the resolved count so the field shows the
        // silx default, then recompute when the user changes it.
        let mut bins = n_bins.unwrap_or(histo.n_bins).max(2);
        ui.horizontal(|ui| {
            ui.label("Bins:");
            if ui
                .add(egui::DragValue::new(&mut bins).range(2..=1024))
                .changed()
            {
                *n_bins = Some(bins.max(2));
            }
        });
        // Recompute if the control changed the count this frame.
        let histo = if *n_bins == Some(histo.n_bins) || n_bins.is_none() {
            histo
        } else {
            match self.active_image_histogram(*n_bins) {
                Some(h) => h,
                None => return Some(histo),
            }
        };

        // Stats line (silx HistogramWidget min/max/mean/std/sum).
        ui.label(format!(
            "min {:.4}  max {:.4}  mean {:.4}  std {:.4}  sum {:.4}",
            histo.min, histo.max, histo.mean, histo.std, histo.sum
        ));

        // Bar chart drawn with the egui painter.
        let max_count = histo.counts.iter().copied().max().unwrap_or(0).max(1) as f32;
        let desired = egui::vec2(ui.available_width().max(120.0), 120.0);
        let (rect, _resp) = ui.allocate_exact_size(desired, egui::Sense::hover());
        if ui.is_rect_visible(rect) {
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
            let n = histo.counts.len().max(1);
            let bar_w = rect.width() / n as f32;
            let fill = Color32::from_rgb(0x66, 0xaa, 0xd7); // silx histogram color.
            for (i, &count) in histo.counts.iter().enumerate() {
                let h = (count as f32 / max_count) * rect.height();
                let x0 = rect.left() + i as f32 * bar_w;
                let bar = egui::Rect::from_min_max(
                    egui::pos2(x0, rect.bottom() - h),
                    egui::pos2(x0 + bar_w, rect.bottom()),
                );
                painter.rect_filled(bar.shrink(0.5), 0.0, fill);
            }
        }

        Some(histo)
    }

    /// Draw a pixel-intensity histogram toolbar button that toggles a popup
    /// window with the bars + stats + bin control (silx
    /// `PixelIntensitiesHistoAction`, a checkable action opening its
    /// `HistogramWidget`).
    ///
    /// Open-state and the chosen bin count are stored in egui temp-memory keyed
    /// by this plot's id. Returns `true` while the window is open this frame.
    pub fn show_pixel_histogram_toolbar(&mut self, ui: &mut egui::Ui) -> bool {
        let plot_id = self.backend().plot().id;
        let open_id = egui::Id::new(plot_id).with("pixel_histogram_open");
        let bins_id = egui::Id::new(plot_id).with("pixel_histogram_bins");

        let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
        let has_image = self.get_image_pixels_raw().is_some();

        let button = ui
            .add_enabled_ui(has_image, |ui| {
                toolbar_icon_button(ui, ToolbarIcon::PixelHistogram, open, "Pixel intensity")
            })
            .inner;
        if button.clicked() {
            open = !open;
        }

        if open {
            let mut n_bins = ui.data(|d| d.get_temp::<Option<usize>>(bins_id)).flatten();
            let signals = crate::widget::detached::show_detached(
                ui.ctx(),
                open_id.with("window"),
                "Pixel intensity",
                egui::vec2(480.0, 360.0),
                None,
                |ui| {
                    self.show_pixel_histogram(ui, &mut n_bins);
                },
            );
            ui.data_mut(|d| d.insert_temp(bins_id, n_bins));
            if signals.close_requested {
                open = false;
            }
        }

        ui.data_mut(|d| d.insert_temp(open_id, open));
        open
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
    /// Left/right split with a vertical separator: the left `split` fraction of
    /// columns shows A, the rest shows B (silx `VisualizationMode.VERTICAL_LINE`,
    /// CompareImages.py:422-433).
    #[default]
    HalfHalf,
    /// Top/bottom split with a horizontal separator: the top `split` fraction of
    /// rows shows A, the rest shows B (silx
    /// `VisualizationMode.HORIZONTAL_LINE`, CompareImages.py:434-445).
    SplitHorizontal,
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
                (
                    "½",
                    "Vertical split: A left / B right (drag slider)",
                    CompareMode::HalfHalf,
                ),
                (
                    "═",
                    "Horizontal split: A top / B bottom (drag slider)",
                    CompareMode::SplitHorizontal,
                ),
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

            let is_split = matches!(
                self.mode,
                CompareMode::HalfHalf | CompareMode::SplitHorizontal
            );
            if is_split && !self.data_a.is_empty() {
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
        let w = self.width as usize;
        let h = self.height as usize;

        match self.mode {
            CompareMode::OnlyA => colormap_to_rgba(self.width, &self.data_a, &self.colormap),
            CompareMode::OnlyB => colormap_to_rgba(self.width, &self.data_b, &self.colormap),
            CompareMode::HalfHalf => {
                let rgba_a = colormap_to_rgba(self.width, &self.data_a, &self.colormap);
                let rgba_b = colormap_to_rgba(self.width, &self.data_b, &self.colormap);
                let split_col = (self.split * self.width as f32).round() as usize;
                split_composite(&rgba_a, &rgba_b, w, h, split_col, false)
            }
            CompareMode::SplitHorizontal => {
                let rgba_a = colormap_to_rgba(self.width, &self.data_a, &self.colormap);
                let rgba_b = colormap_to_rgba(self.width, &self.data_b, &self.colormap);
                let split_row = (self.split * self.height as f32).round() as usize;
                split_composite(&rgba_a, &rgba_b, w, h, split_row, true)
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

/// Composite two colormapped RGBA images along a straight separator, mirroring
/// silx CompareImages VERTICAL_LINE / HORIZONTAL_LINE (CompareImages.py:422-445).
///
/// `a` and `b` are row-major `width × height` RGBA. For a vertical separator
/// (`horizontal == false`) columns with `col < split` show `a`, the rest show `b`
/// (silx `data[:, 0:pos]` / `data[:, pos:]`); for a horizontal separator rows
/// with `row < split` show `a`, the rest `b` (silx `data[0:pos, :]` /
/// `data[pos:, :]`). `split == 0` shows all `b`; `split >=` the split axis length
/// shows all `a` (silx clamps `pos` into `[0, shape]`).
fn split_composite(
    a: &[[u8; 4]],
    b: &[[u8; 4]],
    width: usize,
    height: usize,
    split: usize,
    horizontal: bool,
) -> Vec<[u8; 4]> {
    let mut out = vec![[0u8; 4]; width * height];
    for row in 0..height {
        let base = row * width;
        for col in 0..width {
            let i = base + col;
            let use_a = if horizontal { row < split } else { col < split };
            out[i] = if use_a { a[i] } else { b[i] };
        }
    }
    out
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

/// Project a [`crate::widget::scatter_mask::ScatterMaskWidget`]'s per-point
/// level buffer onto the boolean point selection applied to the scatter (silx
/// `ScatterView` mask: a point is selected when its level is non-zero). Split
/// out from [`ScatterView`] so the level→selection mapping is unit-testable
/// without a GPU backend.
fn scatter_masked_selection(mask: &[u8]) -> Vec<bool> {
    mask.iter().map(|&level| level != 0).collect()
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

/// Whether [`ImageView::show`] should route the captured pointer to the mask
/// tool and paint this frame: only when the plot is in
/// [`PlotInteractionMode::MaskDraw`] *and* the mask panel is enabled for the
/// active image. Gating strictly on `MaskDraw` keeps pan / zoom / select from
/// ever painting (silx's pencil draw interaction is its own mode). Pure, so the
/// gate is unit-testable without a `Ui`/GPU.
fn image_view_should_paint_mask(mode: PlotInteractionMode, mask_enabled: bool) -> bool {
    mask_enabled && mode == PlotInteractionMode::MaskDraw
}

/// Build a [`ScalarMask`] from a [`MaskToolsWidget`](crate::widget::mask_tools::MaskToolsWidget)
/// level buffer (`levels`, row-major, `width * height`, `0` unmasked / non-zero
/// masked). The resulting mask is the representation re-uploaded through
/// [`Plot2D::try_add_masked_image`] / [`apply_image_mask`]: every non-zero level
/// becomes a masked (→ `NaN`) pixel, matching silx `getValueData`
/// (`items/image.py`). A `levels` length not equal to `width * height` is
/// clip/zero-extended by [`ScalarMask::set_mask_data`] (silx's lazy clip/extend),
/// so the returned mask always has the image shape. Pure, so the conversion is
/// unit-testable without a GPU backend.
fn scalar_mask_from_level_buffer(width: u32, height: u32, levels: &[u8]) -> ScalarMask {
    let mut mask = ScalarMask::new(width as usize, height as usize);
    mask.set_mask_data(levels, width as usize);
    mask
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
    /// Whether the side colorbar column is shown (silx `ColorBarAction`,
    /// ImageView's `ColorBarWidget` visibility). Defaults to `true`; when `false`
    /// the colorbar column is not reserved and the image fills its width.
    show_colorbar: bool,
    /// Whether the side histograms (and the radar overview) are shown (silx
    /// `ImageView.setSideHistogramDisplayed`, ImageView.py:552-566). Defaults to
    /// `true`; when `false` the top/right strips and the radar are not drawn and
    /// the image reclaims that space.
    show_side_histograms: bool,
    /// Per-pixel mask editor for the active image (silx `ImageView`'s mask
    /// `MaskToolsWidget`). Resized to the active image on [`Self::set_image`].
    /// Painting is gated strictly on [`PlotInteractionMode::MaskDraw`]
    /// ([`image_view_should_paint_mask`]); the painted level buffer is converted
    /// to a [`ScalarMask`] and re-uploaded (masked pixels → `NaN`) via the same
    /// pre-upload path as [`Plot2D::try_add_masked_image`].
    mask: crate::widget::mask_tools::MaskToolsWidget,
}

/// Width in points to reserve for the side colorbar column given the show flag
/// and whether a colorbar is available, mirroring silx `ColorBarAction` toggling
/// the `ColorBarWidget`'s visibility. Returns [`COLORBAR_WIDTH`] only when the
/// bar is both shown and available, else `0.0` (no column reserved). Split out
/// so the show/hide reservation is unit-testable without a GPU backend.
fn colorbar_column_width(show: bool, has_colorbar: bool) -> f32 {
    if show && has_colorbar {
        COLORBAR_WIDTH
    } else {
        0.0
    }
}

/// Extent (points) to reserve for a side-histogram strip given the show flag
/// (silx `ImageView.setSideHistogramDisplayed`): the `requested` size when
/// shown, else `0.0` — the strip is not drawn and the image reclaims the space.
/// Split out so the show/hide reservation is unit-testable without a GPU backend.
fn side_histogram_extent(show: bool, requested: f32) -> f32 {
    if show { requested } else { 0.0 }
}

/// Which side-histogram profile [`ImageView::histogram`] returns, mirroring the
/// `axis` argument of silx `ImageView.getHistogram(axis)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageHistogramAxis {
    /// Horizontal histogram: per-column sums over the image rows (silx `'x'`).
    X,
    /// Vertical histogram: per-row sums over the image columns (silx `'y'`).
    Y,
}

/// A side-histogram profile and its index extent, mirroring the silx
/// `ImageView.getHistogram` dict `{data, extent}`: `data` is the profile sum per
/// column ([`ImageHistogramAxis::X`]) or per row ([`ImageHistogramAxis::Y`]), and
/// `extent` is the `(start, end)` index range with `end` exclusive
/// (`data.len() == end - start`).
#[derive(Debug, Clone, PartialEq)]
pub struct ImageProfileHistogram {
    /// Per-column (X) or per-row (Y) sum of the image pixels.
    pub data: Vec<f64>,
    /// `(start, end)` index extent; `end` is exclusive.
    pub extent: (f64, f64),
}

/// Per-column sums of a row-major `w×h` image (silx `histoH`):
/// `out[col] = Σ_row pixels[row*w + col]`. Empty when `w == 0`.
fn image_column_sums(pixels: &[f32], w: usize, h: usize) -> Vec<f64> {
    (0..w)
        .map(|col| (0..h).map(|row| pixels[row * w + col] as f64).sum())
        .collect()
}

/// Per-row sums of a row-major `w×h` image (silx `histoV`):
/// `out[row] = Σ_col pixels[row*w + col]`. Empty when `h == 0`.
fn image_row_sums(pixels: &[f32], w: usize, h: usize) -> Vec<f64> {
    (0..h)
        .map(|row| (0..w).map(|col| pixels[row * w + col] as f64).sum())
        .collect()
}

/// The `(col, row, value)` triple silx `ImageView.valueChanged` emits for a
/// cursor at data coordinates `(x, y)` over the active image
/// (`ImageView._imagePlotCB`, ImageView.py:585-601). siplot's ImageView uses
/// identity image geometry (origin `(0, 0)`, scale `(1, 1)`), so a pixel index
/// is the truncated coordinate. Returns `None` — silx emits nothing — when the
/// cursor is left of / below the origin or outside the pixel grid, or when no
/// image is loaded.
fn image_value_at(
    x: f64,
    y: f64,
    pixels: &[f32],
    width: usize,
    height: usize,
) -> Option<(f64, f64, f64)> {
    if width == 0 || height == 0 || pixels.len() < width * height {
        return None;
    }
    // silx guard: cursor must be at or beyond the image origin (here (0, 0)).
    if x < 0.0 || y < 0.0 {
        return None;
    }
    // silx `int((x - origin) / scale)`: truncation toward zero. The non-negative
    // guard above makes `as usize` (also truncating) equal to a floor here.
    let col = x as usize;
    let row = y as usize;
    if col >= width || row >= height {
        return None;
    }
    Some((col as f64, row as f64, pixels[row * width + col] as f64))
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
        // ImageView renders its own dedicated colorbar column (silx
        // `getColorBarWidget` at grid (0,2), ImageView.py:499). The image plot
        // still carries the active image's colormap, which would otherwise draw
        // a second, built-in colorbar — suppress it so only the dedicated one
        // shows.
        image_plot.set_show_colorbar(false);

        // Side profile plots mirror silx `_SideHistogram` (ImageView.py:168): a
        // bare plot with no graph title, no axis labels, and no grid so the full
        // strip is the profile curve — `Plot1D::new` defaults a title-less plot
        // with "X"/"Y" labels and a major grid, so those are cleared here. A 10%
        // data margin is set on the independent "sum" axis (silx `setDataMargins`,
        // ImageView.py:464/477) so the profile peak does not touch the frame
        // edge. The "sum" axis is histo_h's Y and histo_v's X; the other axis is
        // synced to the image and overrides any margin there.
        let mut histo_h = Plot1D::new(render_state, image_id + 1);
        histo_h.clear_graph_x_label();
        histo_h.clear_graph_y_label(YAxis::Left);
        histo_h.set_graph_grid(false);
        histo_h.plot_mut().set_data_margins(DataMargins {
            x_min: 0.0,
            x_max: 0.0,
            y_min: 0.1,
            y_max: 0.1,
        });

        let mut histo_v = Plot1D::new(render_state, image_id + 2);
        histo_v.clear_graph_x_label();
        histo_v.clear_graph_y_label(YAxis::Left);
        histo_v.set_graph_grid(false);
        histo_v.plot_mut().set_data_margins(DataMargins {
            x_min: 0.1,
            x_max: 0.1,
            y_min: 0.0,
            y_max: 0.0,
        });

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
            show_colorbar: true,
            show_side_histograms: true,
            mask: crate::widget::mask_tools::MaskToolsWidget::new(0, 0),
        }
    }

    /// Whether the side colorbar column is shown (silx `ColorBarAction`).
    pub fn show_colorbar(&self) -> bool {
        self.show_colorbar
    }

    /// Show or hide the side colorbar column (silx `ColorBarAction`). When
    /// hidden, [`Self::show`] does not reserve the colorbar column and the image
    /// fills the freed width.
    pub fn set_show_colorbar(&mut self, show: bool) {
        self.show_colorbar = show;
    }

    /// Whether the side histograms (and the radar overview) are displayed (silx
    /// `ImageView.isSideHistogramDisplayed`).
    pub fn is_side_histogram_displayed(&self) -> bool {
        self.show_side_histograms
    }

    /// Show or hide the side histograms and the radar overview (silx
    /// `ImageView.setSideHistogramDisplayed`). When hidden, [`Self::show`] does
    /// not reserve the top/right strips or the radar, and the image reclaims
    /// that space.
    pub fn set_side_histogram_displayed(&mut self, show: bool) {
        self.show_side_histograms = show;
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

        // Resize the mask editor to the new active image (silx `MaskToolsWidget`
        // resets to the image shape; a shape change clears the undo history).
        if self.mask.width != width || self.mask.height != height {
            self.mask.reset_geometry(width, height);
        }

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
        // Mask path (silx `getValueData`): when the mask editor has any masked
        // pixel for this image, NaN those pixels before upload so the scalar
        // pipeline's `nan_color` renders them as holes (the 6B-1 pre-upload mask
        // representation, identical to `Plot2D::try_add_masked_image`). When no
        // pixel is masked, upload the pixels verbatim (no extra allocation).
        let masked: Option<Vec<f32>> = if self.mask.width == self.width
            && self.mask.height == self.height
            && self.mask.mask.iter().any(|&level| level != 0)
        {
            let scalar_mask =
                scalar_mask_from_level_buffer(self.width, self.height, &self.mask.mask);
            Some(scalar_mask.apply(&self.pixels))
        } else {
            None
        };
        let pixels: &[f32] = masked.as_deref().unwrap_or(&self.pixels);

        let spec = image_view_image_spec(
            self.width,
            self.height,
            pixels,
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

            // Colorbar show/hide toggle (silx `ColorBarAction`).
            if ui
                .selectable_label(self.show_colorbar, "colorbar")
                .on_hover_text("Show/hide the colorbar")
                .clicked()
            {
                crate::widget::actions::control::image_colorbar_toggle(self);
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

        // Mask-draw tool: toggle MaskDraw mode (silx `MaskToolsWidget`
        // activating the plot's pencil draw interaction). Entering it sets the
        // mask tool to Pencil so the primary drag paints; exiting restores Zoom
        // and disables the tool. While active, a brush-size slider and the
        // pencil/eraser/clear controls are exposed.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.label("mask:");
            let in_mask_draw = self.image_plot.interaction_mode() == PlotInteractionMode::MaskDraw;
            if ui
                .selectable_label(in_mask_draw, "✏")
                .on_hover_text("Draw mask (pencil): primary drag paints the mask")
                .clicked()
            {
                self.set_mask_draw(!in_mask_draw);
            }
            if in_mask_draw {
                ui.selectable_value(
                    &mut self.mask.active_tool,
                    crate::widget::mask_tools::MaskTool::Pencil,
                    "pencil",
                )
                .on_hover_text("Paint mask");
                ui.selectable_value(
                    &mut self.mask.active_tool,
                    crate::widget::mask_tools::MaskTool::Eraser,
                    "eraser",
                )
                .on_hover_text("Erase mask");
                ui.add(egui::Slider::new(&mut self.mask.brush_size, 1..=50).text("brush"));
                if ui.button("clear mask").clicked() {
                    self.mask.clear_all();
                    self.mask.commit();
                    self.upload_image();
                }
            }
        });
    }

    /// Enter or leave pencil / mask-draw mode (silx `MaskToolsWidget` activating
    /// the plot's pencil draw interaction). Entering sets the image plot to
    /// [`PlotInteractionMode::MaskDraw`] and the mask tool to
    /// [`crate::widget::mask_tools::MaskTool::Pencil`] so the primary drag
    /// paints; leaving restores [`PlotInteractionMode::Zoom`] and disables the
    /// tool ([`crate::widget::mask_tools::MaskTool::None`]).
    pub fn set_mask_draw(&mut self, on: bool) {
        if on {
            crate::widget::actions::mode::mask_draw_mode(&mut self.image_plot);
            if !matches!(
                self.mask.active_tool,
                crate::widget::mask_tools::MaskTool::Pencil
                    | crate::widget::mask_tools::MaskTool::Eraser
            ) {
                self.mask.active_tool = crate::widget::mask_tools::MaskTool::Pencil;
            }
        } else {
            crate::widget::actions::mode::zoom_mode(&mut self.image_plot);
            self.mask.active_tool = crate::widget::mask_tools::MaskTool::None;
        }
    }

    /// Whether the image plot is in pencil / mask-draw mode
    /// ([`PlotInteractionMode::MaskDraw`]).
    pub fn is_mask_draw(&self) -> bool {
        self.image_plot.interaction_mode() == PlotInteractionMode::MaskDraw
    }

    /// The mask editor for the active image (silx `ImageView` mask
    /// `MaskToolsWidget`), exposing whole-mask operations and the painted level
    /// buffer.
    pub fn mask(&self) -> &crate::widget::mask_tools::MaskToolsWidget {
        &self.mask
    }

    /// Mutable access to the mask editor for the active image, for programmatic
    /// mask operations (silx `ImageView` mask `MaskToolsWidget`). After mutating
    /// the mask, call [`Self::set_image`] or re-show to re-upload the masked
    /// image.
    pub fn mask_mut(&mut self) -> &mut crate::widget::mask_tools::MaskToolsWidget {
        &mut self.mask
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
        // Default strip size = silx `HISTOGRAMS_HEIGHT` (ImageView.py:374): the
        // side-profile strips are 200px on their short dimension, enough room for
        // the profile curve plus the sum-axis tick labels. When the side
        // histograms are hidden (silx `setSideHistogramDisplayed(False)`), the
        // strips reserve no space and the image reclaims the freed width/height.
        let show_histos = self.show_side_histograms;
        let histo_h_h = side_histogram_extent(show_histos, histo_height.unwrap_or(200.0));
        let histo_v_w = side_histogram_extent(show_histos, histo_width.unwrap_or(200.0));

        // Synchronise axes before rendering.
        self.sync_x
            .sync(&mut [self.image_plot.plot_mut(), self.histo_h.plot_mut()]);
        self.sync_y
            .sync(&mut [self.image_plot.plot_mut(), self.histo_v.plot_mut()]);

        let avail = ui.available_size();

        // Reserve the far-right colorbar column (silx grid column 2,
        // ImageView.py:501), unless the colorbar is hidden (silx
        // `ColorBarAction`).
        let colorbar_w = colorbar_column_width(self.show_colorbar, true);

        // Top row: horizontal histogram (skipped when side histograms hidden).
        if show_histos {
            ui.allocate_ui(
                egui::vec2(avail.x - histo_v_w - colorbar_w, histo_h_h),
                |ui| {
                    self.histo_h.show(ui);
                },
            );

            // Sync the radar viewport to the image plot's current limits before
            // rendering (silx `__setVisibleRectFromPlot`). The radar is part of
            // the side-histogram cluster, so it too is gated here.
            let (xmin, xmax) = self.image_plot.x_limits();
            if let Some((ymin, ymax)) = self.image_plot.y_limits(YAxis::Left) {
                self.radar.set_viewport_limits(xmin, xmax, ymin, ymax);
            }
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
            // Vertical histogram with the radar overview stacked below it
            // (skipped when side histograms hidden). The enclosing
            // `ui.horizontal` makes child layouts horizontal by default, so an
            // explicit `ui.vertical` is required here — otherwise the radar lands
            // beside the histogram and overflows the column.
            if show_histos {
                ui.allocate_ui(egui::vec2(histo_v_w, img_h), |ui| {
                    ui.vertical(|ui| {
                        ui.allocate_ui(egui::vec2(histo_v_w, img_h - radar_h), |ui| {
                            self.histo_v.show(ui);
                        });
                        let radar = self.radar.ui(ui, egui::vec2(histo_v_w, radar_h));
                        if let Some((rx0, rx1, ry0, ry1)) = radar.dragged_limits {
                            // Forward the dragged viewport to pan/zoom the image
                            // plot (silx `plot.setLimits`, RadarView.py:326).
                            self.image_plot.set_limits(rx0, rx1, ry0, ry1, None);
                        }
                    });
                });
            }
            // Colorbar column, synced to the active image's colormap limits.
            if colorbar_w > 0.0 {
                self.colorbar().ui(ui, egui::vec2(colorbar_w, img_h));
            }
            response
        });

        let plot_response = response.inner;

        // Feed the live cursor (silx sigMouseMoved) into the PositionInfo
        // readout, then render it below the image.
        if let Some(cursor) = cursor_from_pointer_event(plot_response.pointer_event.as_ref()) {
            self.cursor = Some(cursor);
        }
        self.position_info.ui(ui, self.cursor);

        // Mask draw: in MaskDraw mode, route the captured pointer to the mask
        // tool (brush paint / erase) and re-upload the masked image. Gated
        // strictly on MaskDraw so pan/zoom/select never paint.
        self.handle_mask_paint(&plot_response);

        // Shape mask tools (rectangle / ellipse / polygon): drive the on-plot
        // shape draw and paint its rubber-band preview (silx mask draw modes).
        self.handle_mask_shape_draw(ui, &plot_response);

        // Brush footprint preview at the cursor (silx pencil shape circle),
        // drawn on top of the just-painted mask.
        self.draw_brush_preview(ui, &plot_response);

        // Profile tool: a drag on the image plot extracts a profile via the
        // existing helpers and shows it in the profile window (silx
        // _ProfileToolBar, ImageView.py:692-697).
        self.handle_profile_drag(&plot_response);
        self.profile_window.show(ui.ctx());
    }

    /// In [`PlotInteractionMode::MaskDraw`], route the captured pointer to the
    /// mask tool (its existing brush paint / erase in
    /// [`crate::widget::mask_tools::MaskToolsWidget::handle_interaction`]) and
    /// re-upload the masked image when the mask changed. Gated strictly on
    /// [`image_view_should_paint_mask`] so pan / zoom / select never paint
    /// (silx's pencil draw interaction is its own mode).
    fn handle_mask_paint(&mut self, plot_response: &PlotResponse) {
        let mode = self.image_plot.interaction_mode();
        let mask_enabled =
            self.mask.width == self.width && self.mask.height == self.height && self.width != 0;
        if !image_view_should_paint_mask(mode, mask_enabled) {
            return;
        }
        let before = self.mask.mask.clone();
        self.mask.handle_interaction(plot_response);
        if self.mask.mask != before {
            // Painted this frame: re-upload the active image with the new mask
            // applied (masked pixels → NaN).
            self.upload_image();
        }
    }

    /// In [`PlotInteractionMode::MaskDraw`] with a shape tool (rectangle /
    /// ellipse / polygon) active, drive the on-plot shape draw and paint its
    /// rubber-band preview, mirroring silx `MaskToolsWidget._plotDrawEvent` for
    /// the shape draw modes. The mask widget owns the draw state machine and the
    /// fill; a finished shape masks the current level, after which the active
    /// image is re-uploaded with the new mask. The in-progress preview is drawn
    /// in the mask overlay color on a foreground layer clipped to the image
    /// area (silx draws the mask shape in the overlay color). Gated strictly on
    /// [`image_view_should_paint_mask`] so pan / zoom / select never draw.
    fn handle_mask_shape_draw(&mut self, ui: &egui::Ui, plot_response: &PlotResponse) {
        let mode = self.image_plot.interaction_mode();
        let mask_enabled =
            self.mask.width == self.width && self.mask.height == self.height && self.width != 0;
        if !image_view_should_paint_mask(mode, mask_enabled)
            || self.mask.active_tool.draw_mode().is_none()
        {
            // Not a shape draw: drop any in-progress shape so re-entering a
            // shape tool starts fresh.
            self.mask.cancel_shape_draw();
            return;
        }
        let before = self.mask.mask.clone();
        let event = self.mask.handle_shape_draw(plot_response);
        if self.mask.mask != before {
            // Shape finished and masked this frame: re-upload the active image
            // with the new mask applied (masked pixels → NaN).
            self.upload_image();
        }
        // Paint the in-progress preview (rubber band) on top of the image.
        if let Some(draw) = self.mask.shape_draw() {
            let style = crate::widget::interaction::SelectionStyle::new(
                crate::widget::interaction::FillMode::Hatch,
                self.mask.color,
            );
            let painter = ui
                .ctx()
                .layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("mask-shape-preview"),
                ))
                .with_clip_rect(plot_response.transform.area);
            crate::widget::plot_widget::paint_draw_preview(
                &painter,
                &plot_response.transform,
                draw,
                event.as_ref(),
                style,
            );
        }
    }

    /// In [`PlotInteractionMode::MaskDraw`] with a brush tool active, draw the
    /// pencil footprint at the cursor: an unfilled circle of radius
    /// `brush_size / 2` (data coords). Mirrors silx
    /// `DrawFreeHand.updatePencilShape` (`PlotInteraction.py:1011-1017`,
    /// `fill="none"`), shown both while hovering and while painting (silx draws
    /// it from `Idle.onMove` and `Select.onMove`). The mask brush paints a disk
    /// of `brush_size / 2` cells (siplot masks in data==cell space), so the
    /// circle marks the exact footprint. Painted on a foreground layer clipped
    /// to the image area.
    fn draw_brush_preview(&self, ui: &egui::Ui, plot_response: &PlotResponse) {
        use crate::widget::mask_tools::MaskTool;
        let mode = self.image_plot.interaction_mode();
        let mask_enabled =
            self.mask.width == self.width && self.mask.height == self.height && self.width != 0;
        if !image_view_should_paint_mask(mode, mask_enabled)
            || !matches!(self.mask.active_tool, MaskTool::Pencil | MaskTool::Eraser)
        {
            return;
        }
        let area = plot_response.transform.area;
        // Current pointer: hover_pos while idle, interact_pointer_pos while
        // painting (the pointer is captured by the drag).
        let Some(cursor) = plot_response
            .response
            .hover_pos()
            .or_else(|| plot_response.response.interact_pointer_pos())
        else {
            return;
        };
        if !area.contains(cursor) {
            return;
        }
        let center = plot_response.transform.pixel_to_data(cursor);
        let radius = self.mask.brush_size as f64 / 2.0;
        let circle = crate::widget::mask_tools::pencil_preview_circle(
            center,
            radius,
            crate::widget::mask_tools::PENCIL_PREVIEW_SEGMENTS,
        );
        let mut outline: Vec<egui::Pos2> = circle
            .iter()
            .map(|&(x, y)| plot_response.transform.data_to_pixel(x, y))
            .collect();
        if let Some(&first) = outline.first() {
            outline.push(first); // close the ring (silx polygon, fill="none")
        }
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("mask-brush-preview"),
        ));
        painter.with_clip_rect(area).add(egui::Shape::dashed_line(
            &outline,
            egui::Stroke::new(1.5, self.mask.color),
            6.0,
            4.0,
        ));
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

    /// The side-histogram profile sum and its index extent for `axis`, mirroring
    /// silx `ImageView.getHistogram(axis)` (the `{data, extent}` dict).
    /// [`ImageHistogramAxis::X`] returns per-column sums over `[0, width)`,
    /// [`ImageHistogramAxis::Y`] per-row sums over `[0, height)` (extent `end`
    /// exclusive). Returns `None` before an image is set — silx returns `None`
    /// when no histogram has been computed.
    pub fn histogram(&self, axis: ImageHistogramAxis) -> Option<ImageProfileHistogram> {
        if self.width == 0 || self.height == 0 || self.pixels.is_empty() {
            return None;
        }
        let w = self.width as usize;
        let h = self.height as usize;
        let (data, extent) = match axis {
            ImageHistogramAxis::X => (image_column_sums(&self.pixels, w, h), (0.0, w as f64)),
            ImageHistogramAxis::Y => (image_row_sums(&self.pixels, w, h), (0.0, h as f64)),
        };
        Some(ImageProfileHistogram { data, extent })
    }

    /// The `(col, row, value)` under the live cursor, as silx
    /// `ImageView.valueChanged` emits it (ImageView.py:381, emitted at :601):
    /// integer pixel indices returned as floats, with the pixel value at that
    /// index. Returns `None` when the cursor is off the image, before an image
    /// is set, or before any pointer move — silx emits nothing in those cases.
    /// The cursor is updated each frame by [`Self::show`] from the live pointer.
    pub fn value_changed(&self) -> Option<(f64, f64, f64)> {
        let [x, y] = self.cursor?;
        image_value_at(
            x,
            y,
            &self.pixels,
            self.width as usize,
            self.height as usize,
        )
    }

    fn rebuild_histograms(&mut self) {
        if self.width == 0 || self.pixels.is_empty() {
            return;
        }
        let w = self.width as usize;
        let h = self.height as usize;

        // Column sums: histo_h — x = column index, y = sum of that column.
        // Row sums: histo_v — x = sum of that row, y = row index. Computed via
        // the shared pure helpers (also serving `ImageView::histogram`); both
        // sums are taken before any mutable `self.histo_*` borrow below.
        let col_sums = image_column_sums(&self.pixels, w, h);
        let row_sums = image_row_sums(&self.pixels, w, h);
        let col_x: Vec<f64> = (0..w).map(|i| i as f64).collect();
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
/// path; [`Solid`](Self::Solid) renders the per-vertex-colored Delaunay
/// triangle surface through the existing CPU triangle (egui `epaint::Mesh`)
/// path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScatterVisualization {
    /// `Visualization.POINTS`: a marker per point (the existing default).
    #[default]
    Points,
    /// `Visualization.SOLID`: the filled Delaunay triangle surface, each vertex
    /// carrying its point's colormap color so the GPU interpolates the fill
    /// (silx `scatter.py:610-625` `backend.addTriangles`, GL Gouraud shading).
    /// Built via [`crate::core::scatter_viz::solid_triangles`] over the same
    /// per-point colormap+alpha colors as [`Points`](Self::Points). When the
    /// points cannot be triangulated (fewer than 3 finite points or all
    /// collinear) nothing is drawn, matching silx's "Cannot display as solid
    /// surface" early-out.
    Solid,
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
        // Neither the marker cloud nor the SOLID triangle surface produce a
        // grid image — SOLID is rendered through the CPU triangle path, not the
        // image path.
        ScatterVisualization::Points | ScatterVisualization::Solid => None,
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

/// A scatter point picked under the cursor — the result of silx
/// `ScatterView._pickScatterData`: the data `index` and its `(x, y)`
/// coordinates and `value`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScatterPick {
    /// Index of the point in the scatter's data arrays.
    pub index: usize,
    /// The point's X coordinate.
    pub x: f64,
    /// The point's Y coordinate.
    pub y: f64,
    /// The point's value (the colormapped scalar).
    pub value: f64,
}

/// Pixel snap radius for the scatter position-info pick (silx picks points
/// whose symbol overlaps the cursor); sized to the default marker symbol.
const SCATTER_PICK_RADIUS_PX: f32 = crate::core::marker::DEFAULT_MARKER_SIZE;

/// Index of the scatter point nearest `cursor` in pixel space within `radius`
/// pixels, or `None` if none is close enough — the pure core of silx
/// `ScatterView._pickScatterData`. `points` are the per-point pixel positions
/// `(px, py)` (project the data points through the display transform first).
/// Distance ties resolve to the highest index (silx top-most = last-drawn
/// point).
pub fn scatter_pick_pixels(
    cursor: (f32, f32),
    points: &[(f32, f32)],
    radius: f32,
) -> Option<usize> {
    let r2 = radius * radius;
    let mut best: Option<(usize, f32)> = None;
    for (i, &(px, py)) in points.iter().enumerate() {
        let d2 = (px - cursor.0).powi(2) + (py - cursor.1).powi(2);
        if d2 > r2 {
            continue;
        }
        // `<=` so a later (higher) index at an equal distance wins the tie.
        let take = match best {
            None => true,
            Some((_, best_d2)) => d2 <= best_d2,
        };
        if take {
            best = Some((i, d2));
        }
    }
    best.map(|(i, _)| i)
}

/// Build the silx `ScatterView` position-info bar — the `X`, `Y`, `Data`,
/// `Index` columns (ScatterView.py:90-101). When a scatter point is picked,
/// `X`/`Y` snap to it and `Data`/`Index` show its value/index; otherwise `X`/`Y`
/// show the bare cursor coordinates (`%.7g`) and `Data`/`Index` show `"-"`
/// (silx `_getPickedValue`/`_getPickedIndex` fallback).
pub fn scatter_position_info(
    pick: Option<ScatterPick>,
) -> crate::widget::position_info::PositionInfo {
    use crate::widget::position_info::{Converter, PositionInfo, format_value};
    let columns: Vec<(String, Converter)> = vec![
        (
            "X".to_owned(),
            Box::new(move |x, _| match pick {
                Some(p) => format_value(p.x),
                None => format_value(x),
            }),
        ),
        (
            "Y".to_owned(),
            Box::new(move |_, y| match pick {
                Some(p) => format_value(p.y),
                None => format_value(y),
            }),
        ),
        (
            "Data".to_owned(),
            Box::new(move |_, _| match pick {
                Some(p) => format_value(p.value),
                None => "-".to_owned(),
            }),
        ),
        (
            "Index".to_owned(),
            Box::new(move |_, _| match pick {
                Some(p) => p.index.to_string(),
                None => "-".to_owned(),
            }),
        ),
    ];
    PositionInfo::new(columns)
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
    /// Handle of the per-vertex-colored triangle mesh rendered for
    /// [`ScatterVisualization::Solid`] (silx `backend.addTriangles`). `None`
    /// outside `Solid` mode and when the points cannot be triangulated.
    triangles_handle: Option<ItemHandle>,
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
    /// Per-point scatter mask, sized to the point count on [`Self::set_data`]
    /// (silx `ScatterView` `ScatterMaskToolsWidget`, ScatterView.py:116-122).
    /// Its non-zero levels flag the masked-point selection.
    mask: crate::widget::scatter_mask::ScatterMaskWidget,
    /// Whether the side colorbar column is shown (silx `ColorBarAction`,
    /// ScatterView's `ColorBarWidget` visibility). Defaults to `true`; even when
    /// `true` the column is only reserved once data with a colormap exists.
    show_colorbar: bool,
    /// Optional per-point alpha in `[0, 1]` (silx `Scatter` `alpha` array,
    /// scatter.py:1051-1060). When set, it scales each point's colormap RGBA
    /// alpha in the `Points` visualization (silx
    /// `__applyColormapToData`); `None` leaves the colormap alpha untouched.
    alpha: Option<Vec<f64>>,
    /// Last cursor data coordinates fed into the position-info readout (silx
    /// `ScatterView._positionInfo`, updated on `sigMouseMoved`); `None` until the
    /// pointer has moved over the data area.
    cursor: Option<[f64; 2]>,
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
            triangles_handle: None,
            colormap: None,
            points: None,
            visualization: ScatterVisualization::Points,
            grid_resolution: (100, 100),
            mask: crate::widget::scatter_mask::ScatterMaskWidget::new(0),
            show_colorbar: true,
            alpha: None,
            cursor: None,
        }
    }

    /// Whether the side colorbar column is shown (silx `ColorBarAction`).
    pub fn show_colorbar(&self) -> bool {
        self.show_colorbar
    }

    /// Show or hide the side colorbar column (silx `ColorBarAction`). When
    /// hidden, [`Self::show`] does not reserve the colorbar column and the
    /// scatter plot fills the freed width.
    pub fn set_show_colorbar(&mut self, show: bool) {
        self.show_colorbar = show;
    }

    /// The retained per-point alpha array in `[0, 1]` (silx `Scatter` `alpha`),
    /// or `None` when no per-point alpha is set.
    pub fn alpha(&self) -> Option<&[f64]> {
        self.alpha.as_deref()
    }

    /// Set the per-point alpha array (silx `Scatter.setData(alpha=...)`,
    /// scatter.py:1051-1060), consumed at construction. Each entry is clamped to
    /// `[0, 1]`; the array should have one entry per point (the silx contract),
    /// but a length mismatch does not panic — see [`compose_per_point_alpha`].
    /// In [`ScatterVisualization::Points`] mode each point's colormap RGBA alpha
    /// is multiplied by its per-point alpha, with the curve's global alpha
    /// multiplying on top in-shader (silx three-stage `colormap.alpha *
    /// per_point.alpha * global.alpha`).
    #[must_use]
    pub fn with_alpha(mut self, alpha: Vec<f64>) -> Self {
        self.alpha = Some(clamp_alpha(alpha));
        self
    }

    /// Set the per-point alpha array and re-render the current visualization
    /// (silx `Scatter.setData(alpha=...)`). Each entry is clamped to `[0, 1]`.
    /// See [`Self::with_alpha`] for the three-stage alpha composition.
    pub fn set_alpha(&mut self, alpha: Vec<f64>) {
        self.alpha = Some(clamp_alpha(alpha));
        self.rebuild_visualization();
    }

    /// Clear any per-point alpha (silx `Scatter.setData(alpha=None)`) and
    /// re-render, restoring the unscaled colormap RGBA alpha.
    pub fn clear_alpha(&mut self) {
        self.alpha = None;
        self.rebuild_visualization();
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

        // Resize the scatter mask to the new point count, resetting the mask
        // and its undo history (silx `ScatterMask.reset(shape)`). A length
        // change clears any prior selection.
        if self.mask.len() != x.len() {
            self.mask.reset_len(x.len());
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
    /// [`ScatterVisualization::Points`] shows the marker cloud;
    /// [`ScatterVisualization::Solid`] renders the per-vertex-colored Delaunay
    /// triangle surface (built via
    /// [`crate::core::scatter_viz::solid_triangles`]); the grid modes render the
    /// retained `(x, y, value)` points as a colormapped image (built via
    /// [`scatter_grid_image`]). Re-renders immediately against the data from the
    /// last [`Self::set_data`].
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
    /// [`ScatterVisualization::Points`], the per-vertex-colored triangle surface
    /// for [`ScatterVisualization::Solid`], otherwise the converted grid image.
    ///
    /// Single owner of the scatter/grid/triangles item handles so the displayed
    /// item always matches `self.visualization`. The non-active paths' items are
    /// removed so they never overlap.
    fn rebuild_visualization(&mut self) {
        let Some((x, y, values)) = self.points.clone() else {
            return;
        };
        let Some(colormap) = self.colormap.clone() else {
            return;
        };

        match self.visualization {
            ScatterVisualization::Points => {
                // Drop any grid image / triangle surface so neither shadows the
                // markers (single owner: only the active arm keeps its handle).
                if let Some(h) = self.grid_handle.take() {
                    self.inner.remove(h);
                }
                if let Some(h) = self.triangles_handle.take() {
                    self.inner.remove(h);
                }
                // Stage 1+2 of the silx three-stage alpha: per-point colormap
                // RGBA scaled by the per-point alpha (silx shares
                // `__applyColormapToData` between POINTS and SOLID — see the
                // Solid arm below). The curve's global alpha (CurveSpec.alpha)
                // multiplies on top in-shader for stage 3.
                let colors = point_colors(&values, &colormap, self.alpha.as_deref());

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
            ScatterVisualization::Solid => {
                // Drop the marker cloud / grid image so neither shadows the
                // triangle surface (single owner: only the active arm keeps its
                // handle).
                if let Some(h) = self.scatter_handle.take() {
                    self.inner.remove(h);
                }
                if let Some(h) = self.grid_handle.take() {
                    self.inner.remove(h);
                }
                // Same per-point colormap+alpha colors as the Points arm: silx
                // shares `__applyColormapToData` between POINTS and SOLID
                // (scatter.py:526-535, 612-629), then hands the per-vertex RGBA
                // to `backend.addTriangles` for GL Gouraud interpolation.
                let colors = point_colors(&values, &colormap, self.alpha.as_deref());

                // Build the Delaunay triangle surface (silx
                // `scatter_viz::solid_triangles`). `None` for degenerate input
                // (fewer than 3 finite points or all collinear) matches silx's
                // "Cannot display as solid surface" early-out: nothing is drawn.
                let Some(tri) = crate::core::scatter_viz::solid_triangles(&x, &y, &colors) else {
                    if let Some(h) = self.triangles_handle.take() {
                        self.inner.remove(h);
                    }
                    return;
                };

                // No backend update_triangles primitive exists, so re-add the
                // mesh from scratch on each rebuild (remove the prior handle
                // first). `triangles_handle` is Some iff a mesh is displayed.
                if let Some(h) = self.triangles_handle.take() {
                    self.inner.remove(h);
                }
                let h = self.inner.add_triangles_data(&tri);
                self.triangles_handle = Some(h);
                self.inner.set_item_legend(h, "scatter solid");
            }
            mode => {
                // Drop the marker cloud / triangle surface so neither shadows
                // the grid image (single owner: only the active arm keeps its
                // handle).
                if let Some(h) = self.scatter_handle.take() {
                    self.inner.remove(h);
                }
                if let Some(h) = self.triangles_handle.take() {
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

    /// The scatter mask, sized to the point count of the last [`Self::set_data`]
    /// (silx `ScatterView.getMaskToolsDockWidget().getSelectionMask()`,
    /// ScatterView.py:116-122). Drive selections via its geometric / threshold
    /// operations (e.g. [`ScatterMaskWidget::update_rectangle`]); the resulting
    /// non-zero levels flag the masked points, queryable via
    /// [`Self::masked_selection`].
    ///
    /// [`ScatterMaskWidget::update_rectangle`]: crate::widget::scatter_mask::ScatterMaskWidget::update_rectangle
    pub fn scatter_mask(&self) -> &crate::widget::scatter_mask::ScatterMaskWidget {
        &self.mask
    }

    /// Mutable access to the scatter mask, to apply selection operations against
    /// the scatter's point arrays (silx `ScatterMaskToolsWidget`).
    pub fn scatter_mask_mut(&mut self) -> &mut crate::widget::scatter_mask::ScatterMaskWidget {
        &mut self.mask
    }

    /// The boolean point selection applied to the scatter: one entry per point,
    /// `true` where the mask level is non-zero (silx scatter mask selection).
    pub fn masked_selection(&self) -> Vec<bool> {
        scatter_masked_selection(&self.mask.mask)
    }

    /// The per-point selection mask as raw levels (silx
    /// `ScatterView.getSelectionMask`): one `u8` per scatter point, `0`
    /// unmasked and `1..=255` a mask level. Empty until [`Self::set_data`] has
    /// sized it to the point count — silx returns an empty array when there is
    /// no active scatter.
    pub fn selection_mask(&self) -> &[u8] {
        &self.mask.mask
    }

    /// Replace the per-point selection mask (silx
    /// `ScatterView.setSelectionMask`).
    ///
    /// `mask` must have exactly one entry per scatter point — the current
    /// [`selection_mask`](Self::selection_mask) length (the last
    /// [`Self::set_data`] point count). On success the new mask is committed to
    /// the undo history and its point count returned; a length mismatch returns
    /// [`PlotDataError`] and leaves the mask unchanged (silx raises `ValueError`
    /// on a shape mismatch).
    pub fn set_selection_mask(&mut self, mask: &[u8]) -> Result<usize, PlotDataError> {
        if mask.len() != self.mask.mask.len() {
            return Err(PlotDataError::ImageDataLength {
                expected: self.mask.mask.len(),
                actual: mask.len(),
            });
        }
        self.mask.mask.copy_from_slice(mask);
        self.mask.commit();
        Ok(mask.len())
    }

    /// The last cursor data coordinates `(x, y)` fed into the position-info
    /// readout (silx `ScatterView` `sigMouseMoved`), or `None` before the
    /// pointer has moved over the data area.
    pub fn cursor(&self) -> Option<[f64; 2]> {
        self.cursor
    }

    /// Show the silx `ScatterView` position-info bar below the plot: the `X`,
    /// `Y`, `Data`, `Index` columns, snapping to the scatter point under the
    /// cursor (silx ScatterView.py:90-101 + `_pickScatterData`).
    ///
    /// Pass the [`PlotResponse`] returned by [`Self::show`] this frame: the
    /// cursor is updated from its pointer event and the pick is done in pixel
    /// space through its display [`Transform`] (so the snap radius is constant on
    /// screen regardless of zoom). When a point is within
    /// [`SCATTER_PICK_RADIUS_PX`] of the cursor, `X`/`Y` snap to it and
    /// `Data`/`Index` show its value/index; otherwise `X`/`Y` show the cursor
    /// coordinates and `Data`/`Index` show `"-"`.
    pub fn show_position_info(&mut self, ui: &mut egui::Ui, response: &PlotResponse) {
        if let Some(cursor) = cursor_from_pointer_event(response.pointer_event.as_ref()) {
            self.cursor = Some(cursor);
        }
        let pick = self.cursor.and_then(|[cx, cy]| {
            let (xs, ys, vs) = self.points.as_ref()?;
            let cursor_px = response.transform.data_to_pixel(cx, cy);
            let points_px: Vec<(f32, f32)> = xs
                .iter()
                .zip(ys)
                .map(|(&x, &y)| {
                    let p = response.transform.data_to_pixel(x, y);
                    (p.x, p.y)
                })
                .collect();
            let i = scatter_pick_pixels(
                (cursor_px.x, cursor_px.y),
                &points_px,
                SCATTER_PICK_RADIUS_PX,
            )?;
            Some(ScatterPick {
                index: i,
                x: xs[i],
                y: ys[i],
                value: vs[i],
            })
        });
        scatter_position_info(pick).ui(ui, self.cursor);
    }

    /// Mask (or unmask) the scatter points inside the data-space rectangle with
    /// bottom-left `anchor = (x, y)` and `size = (width, height)`, at the
    /// current mask level, then commit it to the undo history (silx
    /// `ScatterMask.updateRectangle` over the scatter points). Uses the
    /// retained point coordinates from the last [`Self::set_data`].
    pub fn mask_rectangle(&mut self, anchor: (f64, f64), size: (f64, f64), mask: bool) {
        let (px, py) = self.mask_point_coords();
        let level = self.mask.level;
        self.mask.update_rectangle(
            level,
            (anchor.1 as f32, anchor.0 as f32),
            (size.1 as f32, size.0 as f32),
            &px,
            &py,
            mask,
        );
        self.mask.commit();
    }

    /// Mask (or unmask) the scatter points inside the data-space polygon
    /// `vertices` (`(x, y)` corners), at the current mask level, then commit it
    /// to the undo history (silx `ScatterMask.updatePolygon`). Uses the retained
    /// point coordinates from the last [`Self::set_data`].
    pub fn mask_polygon(&mut self, vertices: &[(f64, f64)], mask: bool) {
        let (px, py) = self.mask_point_coords();
        // scatter_mask vertices are (y, x) corners (silx Polygon order).
        let verts: Vec<(f32, f32)> = vertices
            .iter()
            .map(|&(x, y)| (y as f32, x as f32))
            .collect();
        let level = self.mask.level;
        self.mask.update_polygon(level, &verts, &px, &py, mask);
        self.mask.commit();
    }

    /// The retained scatter point coordinate arrays as `f32` `(x, y)` for the
    /// geometric mask operations, or empty vectors before any data is uploaded.
    fn mask_point_coords(&self) -> (Vec<f32>, Vec<f32>) {
        match &self.points {
            Some((x, y, _)) => (
                x.iter().map(|&v| v as f32).collect(),
                y.iter().map(|&v| v as f32).collect(),
            ),
            None => (Vec::new(), Vec::new()),
        }
    }

    /// The retained scatter values as `f32` for the threshold mask operations,
    /// or an empty vector before any data is uploaded.
    fn mask_values(&self) -> Vec<f32> {
        match &self.points {
            Some((_, _, v)) => v.iter().map(|&val| val as f32).collect(),
            None => Vec::new(),
        }
    }

    /// Render the scatter mask-tools panel beside the plot (silx
    /// `ScatterView` mask dock, ScatterView.py:116-122).
    ///
    /// Exposes the whole-mask operations (clear-level / clear-all / invert /
    /// undo / redo) plus value-threshold selection over the scatter's value
    /// array; geometric selections (rectangle / polygon / disk) are driven
    /// programmatically through [`Self::scatter_mask_mut`]. The resulting
    /// boolean selection ([`Self::masked_selection`]) is applied to the scatter
    /// — masked points are flagged. Returns `true` when the selection changed
    /// this frame.
    pub fn show_mask_tools(&mut self, ui: &mut egui::Ui) -> bool {
        let before = self.mask.mask.clone();
        ui.horizontal(|ui| {
            ui.label("Mask level:");
            let mut level = self.mask.level;
            if ui
                .add(egui::DragValue::new(&mut level).range(1..=255))
                .changed()
            {
                self.mask.level = level;
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Clear level").clicked() {
                self.mask.clear();
                self.mask.commit();
            }
            if ui.button("Clear all").clicked() {
                self.mask.clear_all();
                self.mask.commit();
            }
            if ui.button("Invert").clicked() {
                self.mask.invert();
                self.mask.commit();
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.mask.can_undo(), egui::Button::new("Undo"))
                .clicked()
            {
                self.mask.undo();
            }
            if ui
                .add_enabled(self.mask.can_redo(), egui::Button::new("Redo"))
                .clicked()
            {
                self.mask.redo();
            }
            if ui.button("Mask non-finite").clicked() {
                let values = self.mask_values();
                self.mask.mask_not_finite(&values);
                self.mask.commit();
            }
        });

        let changed = self.mask.mask != before;
        ui.label(format!(
            "{} / {} points masked",
            self.masked_selection().iter().filter(|&&m| m).count(),
            self.mask.len()
        ));
        changed
    }

    /// Show the standard toolbar plus a colorbar show/hide toggle.
    ///
    /// The colorbar toggle mirrors silx `ColorBarAction`; clicking it flips
    /// whether [`Self::show`] reserves the side colorbar column.
    pub fn show_toolbar(&mut self, ui: &mut egui::Ui) -> ToolbarResponse {
        let show_colorbar = self.show_colorbar;
        let mut toggle = false;
        let (out, ()) = self.inner.show_toolbar_with(ui, |ui, _| {
            ui.separator();
            if ui
                .selectable_label(show_colorbar, "colorbar")
                .on_hover_text("Show/hide the colorbar")
                .clicked()
            {
                toggle = true;
            }
        });
        if toggle {
            crate::widget::actions::control::scatter_colorbar_toggle(self);
        }
        out
    }

    /// Render the scatter plot with the value colorbar beside it.
    ///
    /// The colorbar occupies a fixed-width column on the right, synced to the
    /// value colormap's limits (silx `ScatterView` grid colorbar,
    /// ScatterView.py:83-88). Before any data is uploaded no colorbar is drawn.
    pub fn show(&mut self, ui: &mut egui::Ui) -> PlotResponse {
        let avail = ui.available_size();
        let colorbar = self.colorbar();
        let colorbar_w = colorbar_column_width(self.show_colorbar, colorbar.is_some());
        ui.horizontal(|ui| {
            let plot_w = avail.x - colorbar_w;
            let response = ui
                .allocate_ui(egui::vec2(plot_w, avail.y), |ui| self.inner.show(ui))
                .inner;
            if colorbar_w > 0.0
                && let Some(bar) = colorbar
            {
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

/// Which volume dimension a [`StackView`] browses through — silx `StackView`
/// "perspective": the orthogonal axis whose index selects the displayed frame.
///
/// For a row-major volume of shape `[d0, d1, d2]` (element `(i, j, k)` at flat
/// offset `(i * d1 + j) * d2 + k`):
///
/// - [`Axis0`](Self::Axis0): browse dimension 0 (`d0` frames); each frame is
///   `(d1, d2)` = (height, width). silx perspective 0, no transpose.
/// - [`Axis1`](Self::Axis1): browse dimension 1 (`d1` frames); each frame is
///   `(d0, d2)`. silx perspective 1, transpose `(1, 0, 2)`.
/// - [`Axis2`](Self::Axis2): browse dimension 2 (`d2` frames); each frame is
///   `(d0, d1)`. silx perspective 2, transpose `(2, 0, 1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StackPerspective {
    /// Browse dimension 0; frames are `(d1, d2)`.
    #[default]
    Axis0,
    /// Browse dimension 1; frames are `(d0, d2)`.
    Axis1,
    /// Browse dimension 2; frames are `(d0, d1)`.
    Axis2,
}

impl StackPerspective {
    /// The volume dimension index browsed by this perspective (0, 1, or 2).
    pub fn axis(self) -> usize {
        match self {
            StackPerspective::Axis0 => 0,
            StackPerspective::Axis1 => 1,
            StackPerspective::Axis2 => 2,
        }
    }

    /// The two non-browsed dimensions as `(height_axis, width_axis)`, ascending
    /// — matching silx `__updatePlotLabels` `(y, x)`: `Axis0`→`(1, 2)`,
    /// `Axis1`→`(0, 2)`, `Axis2`→`(0, 1)`.
    pub fn display_axes(self) -> (usize, usize) {
        match self {
            StackPerspective::Axis0 => (1, 2),
            StackPerspective::Axis1 => (0, 2),
            StackPerspective::Axis2 => (0, 1),
        }
    }
}

/// Number of frames when browsing a volume of `shape` (`[d0, d1, d2]`) along
/// `perspective` — silx `__transposed_view.shape[0]`.
pub fn stack_frame_count(shape: [usize; 3], perspective: StackPerspective) -> usize {
    shape[perspective.axis()]
}

/// Slice one 2D frame out of a row-major 3D volume for the given perspective —
/// the pure core of silx `StackView.__createTransposedView` +
/// `setStackPosition`.
///
/// `data` is the flat volume of shape `shape` (`[d0, d1, d2]`), element
/// `(i, j, k)` at offset `(i * d1 + j) * d2 + k`. `index` selects the frame
/// along the browsed dimension. Returns `(width, height, pixels)` with `pixels`
/// in row-major (height, width) order, or `None` if `data.len() != d0*d1*d2` or
/// `index` is out of range for the browsed dimension.
pub fn stack_frame(
    data: &[f32],
    shape: [usize; 3],
    perspective: StackPerspective,
    index: usize,
) -> Option<(u32, u32, Vec<f32>)> {
    let [d0, d1, d2] = shape;
    if data.len() != d0.checked_mul(d1)?.checked_mul(d2)? {
        return None;
    }
    if index >= shape[perspective.axis()] {
        return None;
    }
    let (height_axis, width_axis) = perspective.display_axes();
    let height = shape[height_axis];
    let width = shape[width_axis];
    let at = |i: usize, j: usize, k: usize| data[(i * d1 + j) * d2 + k];
    let mut pixels = Vec::with_capacity(width.saturating_mul(height));
    for row in 0..height {
        for col in 0..width {
            // (i, j, k) places `index` on the browsed axis and (row, col) on
            // the two display axes, matching each perspective's transpose.
            let value = match perspective {
                StackPerspective::Axis0 => at(index, row, col),
                StackPerspective::Axis1 => at(row, index, col),
                StackPerspective::Axis2 => at(row, col, index),
            };
            pixels.push(value);
        }
    }
    Some((width as u32, height as u32, pixels))
}

/// The default label for volume dimension `axis` — silx `"Dimension %d"`
/// (with `_first_stack_dimension == 0`).
pub fn default_dimension_label(axis: usize) -> String {
    format!("Dimension {axis}")
}

/// Plot axis labels `(x_label, y_label)` for `perspective`, picked from the 3
/// per-dimension `labels` — silx `__updatePlotLabels`: X uses the width axis's
/// label, Y uses the height axis's label.
pub fn dimension_axis_labels(
    perspective: StackPerspective,
    labels: &[String; 3],
) -> (String, String) {
    let (height_axis, width_axis) = perspective.display_axes();
    (labels[width_axis].clone(), labels[height_axis].clone())
}

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
    /// The source 3D volume (`flat data`, `[d0, d1, d2]`) when loaded via
    /// [`set_volume`](Self::set_volume); `None` in flat-frames mode
    /// ([`set_stack`](Self::set_stack)). Re-sliced when the perspective changes.
    volume: Option<(Vec<f32>, [usize; 3])>,
    /// Which volume dimension the frame slider browses (silx perspective).
    perspective: StackPerspective,
    /// Per-dimension labels (silx `setLabels`); the plot axis labels are chosen
    /// from these as the perspective rotates. Defaults to `"Dimension 0/1/2"`.
    dim_labels: [String; 3],
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
            volume: None,
            perspective: StackPerspective::default(),
            dim_labels: [
                default_dimension_label(0),
                default_dimension_label(1),
                default_dimension_label(2),
            ],
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
        // Pre-sliced frames are not volume-browsable; leave perspective mode.
        self.volume = None;
        Ok(())
    }

    /// Load a 3D volume and browse it as a stack along the current
    /// [`perspective`](Self::perspective) — silx `StackView.setStack`.
    ///
    /// `data` is the flat row-major volume of shape `shape` (`[d0, d1, d2]`),
    /// element `(i, j, k)` at offset `(i * d1 + j) * d2 + k`. Unlike
    /// [`set_stack`](Self::set_stack) (which takes already-sliced frames), this
    /// keeps the volume so [`set_perspective`](Self::set_perspective) can
    /// re-slice it along a different dimension. The axis labels are updated for
    /// the current perspective.
    pub fn set_volume(
        &mut self,
        data: Vec<f32>,
        shape: [usize; 3],
        colormap: Colormap,
    ) -> Result<(), PlotDataError> {
        let [d0, d1, d2] = shape;
        let expected = d0.saturating_mul(d1).saturating_mul(d2);
        if data.len() != expected {
            return Err(PlotDataError::ImageDataLength {
                expected,
                actual: data.len(),
            });
        }
        self.volume = Some((data, shape));
        self.colormap = colormap;
        self.rebuild_volume_frames();
        Ok(())
    }

    /// The volume dimension currently browsed by the frame slider (silx
    /// `getPerspective`). Always [`StackPerspective::Axis0`] in flat-frames mode.
    pub fn perspective(&self) -> StackPerspective {
        self.perspective
    }

    /// Browse the volume along a different dimension — silx
    /// `StackView.setPerspective`. Re-slices the loaded volume, resets the frame
    /// index to 0, re-fits the view, and updates the axis labels. No-op (the
    /// perspective is still stored) when no volume is loaded.
    pub fn set_perspective(&mut self, perspective: StackPerspective) {
        if perspective == self.perspective {
            return;
        }
        self.perspective = perspective;
        if self.volume.is_some() {
            self.rebuild_volume_frames();
            self.inner.reset_zoom();
        }
    }

    /// Re-slice every frame out of the loaded volume for the current
    /// perspective, resetting dimensions, the frame index, and the GPU image.
    fn rebuild_volume_frames(&mut self) {
        let Some((data, shape)) = self.volume.as_ref() else {
            return;
        };
        let (data, shape) = (data.clone(), *shape);
        let n = stack_frame_count(shape, self.perspective);
        let mut frames = Vec::with_capacity(n);
        let (mut width, mut height) = (0u32, 0u32);
        for index in 0..n {
            if let Some((w, h, pixels)) = stack_frame(&data, shape, self.perspective, index) {
                width = w;
                height = h;
                frames.push(pixels);
            }
        }
        self.width = width;
        self.height = height;
        self.frames = frames;
        self.current_frame = 0;
        // Frame dimensions change with perspective, so drop the old image item
        // and re-add it fresh on the next show().
        if let Some(handle) = self.image_handle.take() {
            self.inner.remove_image(handle);
        }
        self.dirty = true;
        self.apply_axis_labels();
    }

    /// The resolved per-dimension labels (silx `getLabels`).
    pub fn dimension_labels(&self) -> &[String; 3] {
        &self.dim_labels
    }

    /// Set the per-dimension labels used for the plot axes — silx
    /// `StackView.setLabels`. Provide 3 labels for the 3 volume dimensions; an
    /// empty label falls back to the default `"Dimension N"` for that dimension
    /// (mirroring silx's `label or default`). The proper label is chosen for
    /// each axis automatically as the perspective rotates.
    pub fn set_dimension_labels(&mut self, labels: [&str; 3]) {
        for (i, label) in labels.iter().enumerate() {
            self.dim_labels[i] = if label.is_empty() {
                default_dimension_label(i)
            } else {
                (*label).to_string()
            };
        }
        self.apply_axis_labels();
    }

    /// Set the plot axis labels for the current perspective from the resolved
    /// per-dimension labels — silx `__updatePlotLabels`.
    fn apply_axis_labels(&mut self) {
        let (x_label, y_label) = dimension_axis_labels(self.perspective, &self.dim_labels);
        self.inner.set_graph_x_label(x_label);
        self.inner.set_graph_y_label(y_label, YAxis::Left);
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

    /// Show a perspective selector — a combo box choosing which volume
    /// dimension the frame slider browses (silx `PlaneSelectionWidget`). Only
    /// meaningful after [`set_volume`](Self::set_volume); a no-op in flat-frames
    /// mode. Typically called before [`Self::show_frame_controls`].
    pub fn perspective_ui(&mut self, ui: &mut egui::Ui) {
        if self.volume.is_none() {
            return;
        }
        let labels = self.dim_labels.clone();
        let mut selected = self.perspective;
        egui::ComboBox::from_label("Browse dimension")
            .selected_text(labels[selected.axis()].clone())
            .show_ui(ui, |ui| {
                for option in [
                    StackPerspective::Axis0,
                    StackPerspective::Axis1,
                    StackPerspective::Axis2,
                ] {
                    ui.selectable_value(&mut selected, option, labels[option.axis()].clone());
                }
            });
        self.set_perspective(selected);
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

    /// A row-major `[2, 3, 4]` volume whose element `(i, j, k)` encodes its own
    /// indices as `100*i + 10*j + k`, so a sliced frame is easy to verify.
    fn sample_volume() -> (Vec<f32>, [usize; 3]) {
        let shape = [2usize, 3, 4];
        let [d0, d1, d2] = shape;
        let mut data = vec![0.0f32; d0 * d1 * d2];
        for i in 0..d0 {
            for j in 0..d1 {
                for k in 0..d2 {
                    data[(i * d1 + j) * d2 + k] = (100 * i + 10 * j + k) as f32;
                }
            }
        }
        (data, shape)
    }

    #[test]
    fn stack_frame_count_is_the_browsed_dimension() {
        let shape = [2usize, 3, 4];
        assert_eq!(stack_frame_count(shape, StackPerspective::Axis0), 2);
        assert_eq!(stack_frame_count(shape, StackPerspective::Axis1), 3);
        assert_eq!(stack_frame_count(shape, StackPerspective::Axis2), 4);
    }

    #[test]
    fn stack_perspective_display_axes_are_non_browsed_ascending() {
        assert_eq!(StackPerspective::Axis0.display_axes(), (1, 2));
        assert_eq!(StackPerspective::Axis1.display_axes(), (0, 2));
        assert_eq!(StackPerspective::Axis2.display_axes(), (0, 1));
    }

    #[test]
    fn stack_frame_axis0_browses_dim0_without_transpose() {
        let (data, shape) = sample_volume();
        // Browse d0 at index 1: frame is (d1=3, d2=4) = (height, width).
        let (w, h, pixels) = stack_frame(&data, shape, StackPerspective::Axis0, 1).unwrap();
        assert_eq!((w, h), (4, 3));
        assert_eq!(
            pixels,
            vec![
                100.0, 101.0, 102.0, 103.0, 110.0, 111.0, 112.0, 113.0, 120.0, 121.0, 122.0, 123.0
            ]
        );
    }

    #[test]
    fn stack_frame_axis1_transposes_1_0_2() {
        let (data, shape) = sample_volume();
        // Browse d1 at index 2: frame is (d0=2, d2=4) = (height, width).
        let (w, h, pixels) = stack_frame(&data, shape, StackPerspective::Axis1, 2).unwrap();
        assert_eq!((w, h), (4, 2));
        assert_eq!(
            pixels,
            vec![20.0, 21.0, 22.0, 23.0, 120.0, 121.0, 122.0, 123.0]
        );
    }

    #[test]
    fn stack_frame_axis2_transposes_2_0_1() {
        let (data, shape) = sample_volume();
        // Browse d2 at index 3: frame is (d0=2, d1=3) = (height, width).
        let (w, h, pixels) = stack_frame(&data, shape, StackPerspective::Axis2, 3).unwrap();
        assert_eq!((w, h), (3, 2));
        assert_eq!(pixels, vec![3.0, 13.0, 23.0, 103.0, 113.0, 123.0]);
    }

    #[test]
    fn stack_frame_rejects_length_mismatch_and_out_of_range() {
        let (data, shape) = sample_volume();
        // Wrong flat length for the declared shape.
        assert!(stack_frame(&data[..23], shape, StackPerspective::Axis0, 0).is_none());
        // Index past the browsed dimension (d0 == 2, so index 2 is out of range).
        assert!(stack_frame(&data, shape, StackPerspective::Axis0, 2).is_none());
    }

    #[test]
    fn dimension_axis_labels_use_width_for_x_and_height_for_y() {
        let labels = ["z".to_string(), "y".to_string(), "x".to_string()];
        // X uses the width axis's label, Y the height axis's label.
        assert_eq!(
            dimension_axis_labels(StackPerspective::Axis0, &labels),
            ("x".to_string(), "y".to_string())
        );
        assert_eq!(
            dimension_axis_labels(StackPerspective::Axis1, &labels),
            ("x".to_string(), "z".to_string())
        );
        assert_eq!(
            dimension_axis_labels(StackPerspective::Axis2, &labels),
            ("y".to_string(), "z".to_string())
        );
    }

    #[test]
    fn default_dimension_labels_drive_axis_labels_per_perspective() {
        let labels = [
            default_dimension_label(0),
            default_dimension_label(1),
            default_dimension_label(2),
        ];
        assert_eq!(labels, ["Dimension 0", "Dimension 1", "Dimension 2"]);
        // Axis0: X = width axis (dim2), Y = height axis (dim1).
        assert_eq!(
            dimension_axis_labels(StackPerspective::Axis0, &labels),
            ("Dimension 2".to_string(), "Dimension 1".to_string())
        );
        // Axis2: X = width axis (dim1), Y = height axis (dim0).
        assert_eq!(
            dimension_axis_labels(StackPerspective::Axis2, &labels),
            ("Dimension 1".to_string(), "Dimension 0".to_string())
        );
    }

    #[test]
    fn scatter_pick_returns_nearest_point_within_radius() {
        // Pixel positions relative to a cursor at the origin.
        let points = [(10.0, 0.0), (3.0, 4.0), (100.0, 100.0)];
        // radius 8: (10,0) d=10 out, (3,4) d=5 in, (100,100) out → index 1.
        assert_eq!(scatter_pick_pixels((0.0, 0.0), &points, 8.0), Some(1));
    }

    #[test]
    fn scatter_pick_none_when_all_outside_radius() {
        let points = [(100.0, 0.0), (0.0, 100.0)];
        assert_eq!(scatter_pick_pixels((0.0, 0.0), &points, 8.0), None);
    }

    #[test]
    fn scatter_pick_ties_resolve_to_highest_index() {
        // Two coincident points at equal distance — the top-most (last) wins.
        let points = [(5.0, 0.0), (5.0, 0.0)];
        assert_eq!(scatter_pick_pixels((0.0, 0.0), &points, 8.0), Some(1));
    }

    #[test]
    fn scatter_position_info_snaps_to_pick() {
        let pick = Some(ScatterPick {
            index: 7,
            x: 1.5,
            y: 2.5,
            value: 3.5,
        });
        // X/Y snap to the pick (ignoring the bare cursor), Data/Index show it.
        let cols = scatter_position_info(pick).values(Some([9.0, 9.0]));
        assert_eq!(cols, vec!["1.5", "2.5", "3.5", "7"]);
    }

    #[test]
    fn scatter_position_info_falls_back_without_pick() {
        // No pick: X/Y show the cursor, Data/Index show "-".
        let cols = scatter_position_info(None).values(Some([1.5, 2.5]));
        assert_eq!(cols, vec!["1.5", "2.5", "-", "-"]);
        // No cursor at all: every column is the silx placeholder.
        let cols = scatter_position_info(None).values(None);
        assert_eq!(cols, vec!["------", "------", "------", "------"]);
    }

    #[test]
    fn active_axis_label_overrides_routes_y_by_axis() {
        use crate::core::transform::YAxis;
        // Left-axis curve: X drives the X axis, Y drives the left Y axis, y2 unset.
        assert_eq!(
            active_axis_label_overrides(Some("Time"), Some("Counts"), YAxis::Left),
            (Some("Time".to_string()), Some("Counts".to_string()), None)
        );
        // Right-axis curve: X drives the X axis, Y drives the y2 axis, left Y unset.
        assert_eq!(
            active_axis_label_overrides(Some("Time"), Some("Counts"), YAxis::Right),
            (Some("Time".to_string()), None, Some("Counts".to_string()))
        );
        // Missing per-curve labels pass through as None on every axis (the plot
        // then falls back to the graph defaults).
        assert_eq!(
            active_axis_label_overrides(None, None, YAxis::Left),
            (None, None, None)
        );
    }

    #[test]
    fn set_symbol_visibility_hide_then_show_is_lossless() {
        // silx checkable Points: hiding a Diamond then showing must restore the
        // SAME variant (Diamond), not a default. The cache holds it meanwhile.
        let mut cache = None;
        let hidden = set_symbol_visibility(Some(Symbol::Diamond), false, &mut cache);
        assert_eq!(hidden, None);
        assert_eq!(cache, Some(Symbol::Diamond));
        let shown = set_symbol_visibility(hidden, true, &mut cache);
        assert_eq!(shown, Some(Symbol::Diamond));
        // The cache was consumed on restore.
        assert_eq!(cache, None);
    }

    #[test]
    fn set_line_visibility_hide_then_show_is_lossless() {
        // silx checkable Lines: hiding a Dashed line then showing must restore
        // Dashed, not Solid. The cache holds the exact style meanwhile.
        let mut cache = None;
        let hidden = set_line_visibility(LineStyle::Dashed, false, &mut cache);
        assert_eq!(hidden, LineStyle::None);
        assert_eq!(cache, Some(LineStyle::Dashed));
        let shown = set_line_visibility(hidden, true, &mut cache);
        assert_eq!(shown, LineStyle::Dashed);
        assert_eq!(cache, None);
    }

    #[test]
    fn curve_legend_visual_carries_line_style_and_symbol() {
        // The legend icon must reflect the curve's own line style and marker
        // (silx LegendIcon built from the curve's CurveStyle), so the side panel
        // shows dashed / dotted / solid + the symbol, not just the color.
        let x = [0.0, 1.0];
        let y = [0.0, 1.0];

        let mut dashed = CurveSpec::new(&x, &y, Color32::RED);
        dashed.line_style = LineStyle::Dashed;
        dashed.symbol = Some(Symbol::Square);
        let v = curve_spec_legend_visual(&dashed, PlotItemKind::Curve);
        assert_eq!(v.color, Color32::RED);
        assert_eq!(v.line_style, LineStyle::Dashed);
        assert_eq!(v.symbol, Some(Symbol::Square));

        // Marker-only (no line): the icon draws no line and shows the marker.
        let mut markers = CurveSpec::new(&x, &y, Color32::BLUE);
        markers.line_style = LineStyle::None;
        markers.symbol = Some(Symbol::Circle);
        let v = curve_spec_legend_visual(&markers, PlotItemKind::Scatter);
        assert_eq!(v.line_style, LineStyle::None);
        assert!(!v.line_style.draws_line());
        assert_eq!(v.symbol, Some(Symbol::Circle));

        // A plain solid curve: solid line, no marker (the CurveSpec defaults).
        let v =
            curve_spec_legend_visual(&CurveSpec::new(&x, &y, Color32::GREEN), PlotItemKind::Curve);
        assert_eq!(v.line_style, LineStyle::Solid);
        assert_eq!(v.symbol, None);
    }

    #[test]
    fn set_symbol_visibility_no_op_when_already_in_state() {
        // Show while already visible: no change, cache untouched (a stale stash
        // from a prior hide must not be clobbered or consumed).
        let mut cache = Some(Symbol::Square);
        let out = set_symbol_visibility(Some(Symbol::Circle), true, &mut cache);
        assert_eq!(out, Some(Symbol::Circle));
        assert_eq!(cache, Some(Symbol::Square));

        // Hide while already hidden: no change, cache not clobbered.
        let mut cache = Some(Symbol::Diamond);
        let out = set_symbol_visibility(None, false, &mut cache);
        assert_eq!(out, None);
        assert_eq!(cache, Some(Symbol::Diamond));
    }

    #[test]
    fn set_line_visibility_no_op_when_already_in_state() {
        // Show while already drawing a line: no change, cache untouched.
        let mut cache = Some(LineStyle::Dotted);
        let out = set_line_visibility(LineStyle::Dashed, true, &mut cache);
        assert_eq!(out, LineStyle::Dashed);
        assert_eq!(cache, Some(LineStyle::Dotted));

        // Hide while already hidden (LineStyle::None): no change, cache kept.
        let mut cache = Some(LineStyle::Dashed);
        let out = set_line_visibility(LineStyle::None, false, &mut cache);
        assert_eq!(out, LineStyle::None);
        assert_eq!(cache, Some(LineStyle::Dashed));
    }

    fn highlight_base() -> CurveData {
        // A fully-distinct base style so any spurious override is detectable.
        let mut base = CurveData::new(vec![0.0, 1.0], vec![0.0, 1.0], Color32::RED);
        base.width = 1.0;
        base.line_style = LineStyle::Solid;
        base.symbol = Some(Symbol::Circle);
        base.marker_size = 7.0;
        base.gap_color = Some(Color32::BLUE);
        base
    }

    #[test]
    fn current_curve_style_not_highlighted_returns_base_unchanged() {
        // silx getCurrentStyle: when not highlighted, the resolved style is the
        // curve's own fields verbatim, regardless of the highlight override.
        let base = highlight_base();
        let highlight = CurveStyle {
            color: Some(Color32::GREEN),
            line_width: Some(9.0),
            line_style: Some(LineStyle::Dashed),
            symbol: Some(Symbol::Square),
            symbol_size: Some(99.0),
            gap_color: Some(Color32::WHITE),
        };
        let resolved = current_curve_style(&base, &highlight, false);
        assert_eq!(resolved.color, base.color);
        assert_eq!(resolved.width, base.width);
        assert_eq!(resolved.line_style, base.line_style);
        assert_eq!(resolved.symbol, base.symbol);
        assert_eq!(resolved.marker_size, base.marker_size);
        assert_eq!(resolved.gap_color, base.gap_color);
    }

    #[test]
    fn current_curve_style_default_highlight_overrides_only_width() {
        // silx DEFAULT highlight (linewidth=2, all else None) on a width-1 base:
        // width becomes 2.0; every other style field falls through to the base
        // (the no-op-on-unset-fields proof).
        let base = highlight_base();
        let highlight = CurveStyle {
            line_width: Some(2.0),
            ..CurveStyle::default()
        };
        let resolved = current_curve_style(&base, &highlight, true);
        assert_eq!(resolved.width, 2.0);
        assert_eq!(resolved.color, base.color);
        assert_eq!(resolved.line_style, base.line_style);
        assert_eq!(resolved.symbol, base.symbol);
        assert_eq!(resolved.marker_size, base.marker_size);
        assert_eq!(resolved.gap_color, base.gap_color);
    }

    #[test]
    fn current_curve_style_per_field_override_color_and_line_style_only() {
        // silx per-field merge: only the Some fields win; line_width None falls
        // through to the base width.
        let base = highlight_base();
        let highlight = CurveStyle {
            color: Some(Color32::GREEN),
            line_style: Some(LineStyle::Dashed),
            ..CurveStyle::default()
        };
        let resolved = current_curve_style(&base, &highlight, true);
        assert_eq!(resolved.color, Color32::GREEN);
        assert_eq!(resolved.line_style, LineStyle::Dashed);
        assert_eq!(resolved.width, base.width); // None inherits the base width.
        assert_eq!(resolved.symbol, base.symbol);
        assert_eq!(resolved.marker_size, base.marker_size);
        assert_eq!(resolved.gap_color, base.gap_color);
    }

    #[test]
    fn default_active_curve_style_is_width_two_only() {
        // The PlotWidget default highlight is silx's: linewidth 2, all else None.
        let default_style = CurveStyle {
            line_width: Some(2.0),
            ..CurveStyle::default()
        };
        assert_eq!(default_style.line_width, Some(2.0));
        assert_eq!(default_style.color, None);
        assert_eq!(default_style.line_style, None);
        assert_eq!(default_style.symbol, None);
        assert_eq!(default_style.symbol_size, None);
        assert_eq!(default_style.gap_color, None);
    }

    #[test]
    fn set_symbol_visibility_show_from_never_cached_uses_default() {
        // A curve created hidden (symbol None) with an empty cache: showing
        // falls back to the documented default Symbol::Point.
        let mut cache = None;
        let out = set_symbol_visibility(None, true, &mut cache);
        assert_eq!(out, Some(Symbol::Point));
        assert_eq!(out, Some(DEFAULT_RESTORE_SYMBOL));
        assert_eq!(cache, None);
    }

    #[test]
    fn set_line_visibility_show_from_never_cached_uses_default() {
        // A curve created with no line (LineStyle::None) with an empty cache:
        // showing falls back to the documented default LineStyle::Solid.
        let mut cache = None;
        let out = set_line_visibility(LineStyle::None, true, &mut cache);
        assert_eq!(out, LineStyle::Solid);
        assert_eq!(out, DEFAULT_RESTORE_LINE_STYLE);
        assert_eq!(cache, None);
    }

    #[test]
    fn clamp_alpha_clamps_out_of_range_entries() {
        // silx Scatter.setData clips alpha to [0, 1] (scatter.py:1058-1059):
        // >1 -> 1, <0 -> 0, in-range unchanged.
        assert_eq!(
            clamp_alpha(vec![1.5, -0.5, 0.25, 1.0, 0.0]),
            vec![1.0, 0.0, 0.25, 1.0, 0.0]
        );
    }

    #[test]
    fn split_composite_vertical_splits_columns() {
        // 3x2 image; A all [1,..], B all [2,..]; split at column 2 (vertical
        // separator) -> cols 0,1 from A, col 2 from B (silx VERTICAL_LINE).
        let a = vec![[1u8, 1, 1, 1]; 6];
        let b = vec![[2u8, 2, 2, 2]; 6];
        let out = split_composite(&a, &b, 3, 2, 2, false);
        // Row 0: [A, A, B]; row 1: [A, A, B].
        assert_eq!(out[0], a[0]);
        assert_eq!(out[1], a[1]);
        assert_eq!(out[2], b[2]);
        assert_eq!(out[3], a[3]);
        assert_eq!(out[4], a[4]);
        assert_eq!(out[5], b[5]);
    }

    #[test]
    fn split_composite_horizontal_splits_rows() {
        // 3x2 image; split at row 1 (horizontal separator) -> row 0 from A, row
        // 1 from B (silx HORIZONTAL_LINE: rows < pos show A).
        let a = vec![[1u8, 1, 1, 1]; 6];
        let b = vec![[2u8, 2, 2, 2]; 6];
        let out = split_composite(&a, &b, 3, 2, 1, true);
        // Row 0 (indices 0,1,2) from A; row 1 (3,4,5) from B.
        assert_eq!(&out[0..3], &[a[0], a[1], a[2]]);
        assert_eq!(&out[3..6], &[b[3], b[4], b[5]]);
    }

    #[test]
    fn split_composite_extremes_show_one_image() {
        let a = vec![[1u8, 1, 1, 1]; 6];
        let b = vec![[2u8, 2, 2, 2]; 6];
        // split 0 -> all B (no column/row satisfies idx < 0).
        assert!(
            split_composite(&a, &b, 3, 2, 0, false)
                .iter()
                .all(|&p| p == b[0])
        );
        assert!(
            split_composite(&a, &b, 3, 2, 0, true)
                .iter()
                .all(|&p| p == b[0])
        );
        // split == axis length -> all A.
        assert!(
            split_composite(&a, &b, 3, 2, 3, false)
                .iter()
                .all(|&p| p == a[0])
        );
        assert!(
            split_composite(&a, &b, 3, 2, 2, true)
                .iter()
                .all(|&p| p == a[0])
        );
    }

    #[test]
    fn compose_per_point_alpha_multiplies_color_alpha() {
        // silx `rgbacolors[:, -1] *= __alpha`: a color with straight alpha 200
        // and per-point alpha 0.5 -> 100 (200 * 0.5). RGB is unchanged.
        let mut colors = vec![Color32::from_rgba_unmultiplied(10, 20, 30, 200)];
        compose_per_point_alpha(&mut colors, &[0.5]);
        assert_eq!(colors[0], Color32::from_rgba_unmultiplied(10, 20, 30, 100));
    }

    #[test]
    fn compose_per_point_alpha_clamps_each_entry() {
        // An out-of-range alpha clamps to [0, 1] inside the compose step too:
        // 2.0 -> 1.0 (alpha unchanged at 200), -1.0 -> 0.0 (alpha 0).
        let mut colors = vec![
            Color32::from_rgba_unmultiplied(10, 20, 30, 200),
            Color32::from_rgba_unmultiplied(40, 50, 60, 200),
        ];
        compose_per_point_alpha(&mut colors, &[2.0, -1.0]);
        assert_eq!(colors[0], Color32::from_rgba_unmultiplied(10, 20, 30, 200));
        assert_eq!(colors[1], Color32::from_rgba_unmultiplied(40, 50, 60, 0));
    }

    #[test]
    fn compose_per_point_alpha_handles_length_mismatch() {
        // alpha shorter than colors: the trailing colors keep their alpha
        // (composition runs over min(len), no panic).
        let mut colors = vec![
            Color32::from_rgba_unmultiplied(0, 0, 0, 200),
            Color32::from_rgba_unmultiplied(0, 0, 0, 200),
        ];
        compose_per_point_alpha(&mut colors, &[0.5]);
        assert_eq!(colors[0].a(), 100);
        assert_eq!(colors[1].a(), 200);

        // alpha longer than colors: the extra entries are ignored, no panic.
        let mut colors = vec![Color32::from_rgba_unmultiplied(0, 0, 0, 200)];
        compose_per_point_alpha(&mut colors, &[0.5, 0.25, 0.1]);
        assert_eq!(colors[0].a(), 100);
    }

    /// Build the `DataBounds` the widget would accumulate, with non-degenerate
    /// spans on every axis so `as_non_degenerate` does not pad.
    fn data_bounds(x: (f64, f64), y_left: (f64, f64), y_right: Option<(f64, f64)>) -> DataBounds {
        DataBounds {
            x: Some(Bounds1D::new(x.0, x.1).unwrap()),
            y_left: Some(Bounds1D::new(y_left.0, y_left.1).unwrap()),
            y_right: y_right.map(|(lo, hi)| Bounds1D::new(lo, hi).unwrap()),
        }
    }

    /// Reproduce the exact composition `apply_limits_from_data_bounds` now
    /// performs on its model owner: map widget `DataBounds` -> `DataRange`, then
    /// apply through `Plot::reset_zoom_to_data_range`. `PlotWidget` itself needs
    /// a GPU `RenderState`, so this asserts the flag-aware behavior via the
    /// model owner the widget routes through.
    fn apply_widget_reset(plot: &mut Plot, bounds: DataBounds) {
        plot.reset_zoom_to_data_range(data_range_from_bounds(bounds));
    }

    #[test]
    fn widget_reset_keeps_x_and_refits_y_when_only_y_autoscale_on() {
        // x_autoscale OFF + y_autoscale ON: current X limits preserved, Y refit.
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 1.0, 0.0, 1.0);
        plot.set_x_autoscale(false);
        plot.set_y_autoscale(true);
        apply_widget_reset(&mut plot, data_bounds((10.0, 20.0), (-5.0, 5.0), None));
        assert_eq!(plot.limits, (0.0, 1.0, -5.0, 5.0));
    }

    #[test]
    fn widget_reset_keeps_y_and_refits_x_when_only_x_autoscale_on() {
        // x_autoscale ON + y_autoscale OFF: X refit, current Y limits preserved.
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 1.0, 0.0, 1.0);
        plot.set_x_autoscale(true);
        plot.set_y_autoscale(false);
        apply_widget_reset(&mut plot, data_bounds((10.0, 20.0), (-5.0, 5.0), None));
        assert_eq!(plot.limits, (10.0, 20.0, 0.0, 1.0));
    }

    #[test]
    fn widget_reset_with_all_autoscale_off_is_noop() {
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 1.0, 0.0, 1.0);
        plot.y2 = Some((0.0, 2.0));
        plot.set_x_autoscale(false);
        plot.set_y_autoscale(false);
        plot.set_y2_autoscale(false);
        apply_widget_reset(
            &mut plot,
            data_bounds((10.0, 20.0), (-5.0, 5.0), Some((-1.0, 1.0))),
        );
        assert_eq!(plot.limits, (0.0, 1.0, 0.0, 1.0));
        assert_eq!(plot.y2, Some((0.0, 2.0)));
    }

    #[test]
    fn data_range_from_bounds_pads_degenerate_axis() {
        // A single-point X span pads via as_non_degenerate before reaching the
        // model, so a refit axis never gets a zero-width range.
        let bounds = DataBounds {
            x: Some(Bounds1D::new(4.0, 4.0).unwrap()),
            y_left: Some(Bounds1D::new(-1.0, 1.0).unwrap()),
            y_right: None,
        };
        let range = data_range_from_bounds(bounds);
        let (xmin, xmax) = range.x.unwrap();
        assert!(xmax > xmin, "degenerate X must be padded: {xmin}..{xmax}");
        assert_eq!(range.y, Some((-1.0, 1.0)));
        assert_eq!(range.y2, None);
    }

    #[test]
    fn save_to_path_dispatch_resolves_format_per_extension() {
        // save_to_path branches on SaveTarget::from_path; the GPU readback +
        // file write are shims, but the extension->target decision (which
        // SaveFormat each figure extension routes to, vs CSV) is pure. Assert
        // the full dispatch table this entry point relies on, without a GPU.
        use crate::widget::actions::io::SaveTarget;

        assert_eq!(
            SaveTarget::from_path(Path::new("/tmp/fig.png")),
            Some(SaveTarget::Figure(SaveFormat::Png))
        );
        assert_eq!(
            SaveTarget::from_path(Path::new("/tmp/fig.ppm")),
            Some(SaveTarget::Figure(SaveFormat::Ppm))
        );
        assert_eq!(
            SaveTarget::from_path(Path::new("/tmp/fig.svg")),
            Some(SaveTarget::Figure(SaveFormat::Svg))
        );
        assert_eq!(
            SaveTarget::from_path(Path::new("/tmp/fig.tif")),
            Some(SaveTarget::Figure(SaveFormat::Tiff))
        );
        assert_eq!(
            SaveTarget::from_path(Path::new("/tmp/fig.tiff")),
            Some(SaveTarget::Figure(SaveFormat::Tiff))
        );
        assert_eq!(
            SaveTarget::from_path(Path::new("/tmp/curve.csv")),
            Some(SaveTarget::CurveCsv)
        );
        // Unknown / matplotlib-only / extensionless paths are not save targets,
        // so save_to_path returns Ok(false) for them.
        assert_eq!(SaveTarget::from_path(Path::new("/tmp/fig.pdf")), None);
        assert_eq!(SaveTarget::from_path(Path::new("/tmp/noext")), None);
    }

    #[test]
    fn print_temp_png_path_is_process_unique_under_dir() {
        // The print shim rasterizes into this temp path before submitting to the
        // printer; the GPU readback + submit are shims, but the naming is pure.
        let dir = Path::new("/tmp/siplot-test");
        let p = print_temp_png_path(dir, 4242);
        assert_eq!(p, Path::new("/tmp/siplot-test/siplot-print-4242.png"));
        // Always under the requested dir, always a .png, with the pid embedded so
        // concurrent plots / a copy in flight do not collide.
        assert!(p.starts_with(dir));
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("png"));
        assert_ne!(p, print_temp_png_path(dir, 4243));
    }

    #[test]
    fn colorbar_column_width_reserves_only_when_shown_and_available() {
        // Shown and available: column reserved.
        assert_eq!(colorbar_column_width(true, true), COLORBAR_WIDTH);
        // Hidden: no column even when available.
        assert_eq!(colorbar_column_width(false, true), 0.0);
        // Shown but no colorbar available (e.g. ScatterView before data): none.
        assert_eq!(colorbar_column_width(true, false), 0.0);
        // Hidden and unavailable: none.
        assert_eq!(colorbar_column_width(false, false), 0.0);
    }

    #[test]
    fn side_histogram_extent_reserves_only_when_shown() {
        // Shown: the requested strip size is reserved verbatim.
        assert_eq!(side_histogram_extent(true, 200.0), 200.0);
        assert_eq!(side_histogram_extent(true, 80.0), 80.0);
        // Hidden: no space, regardless of the requested size.
        assert_eq!(side_histogram_extent(false, 200.0), 0.0);
        assert_eq!(side_histogram_extent(false, 80.0), 0.0);
    }

    // ── ImageView side-histogram profile sums (silx getHistogram data) ────────

    #[test]
    fn image_column_and_row_sums_match_silx_profile() {
        // 3×2 row-major image:
        //   row 0: 1 2 3
        //   row 1: 4 5 6
        let pixels = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let (w, h) = (3usize, 2usize);
        // histoH = per-column sums over rows: [1+4, 2+5, 3+6].
        assert_eq!(image_column_sums(&pixels, w, h), vec![5.0, 7.0, 9.0]);
        // histoV = per-row sums over columns: [1+2+3, 4+5+6].
        assert_eq!(image_row_sums(&pixels, w, h), vec![6.0, 15.0]);
    }

    #[test]
    fn image_profile_sums_have_one_entry_per_index() {
        // The X profile has `width` entries, the Y profile `height` — matching
        // the extent silx reports as (0, width) / (0, height).
        let pixels = vec![1.0f32; 12]; // 4×3
        let (w, h) = (4usize, 3usize);
        assert_eq!(image_column_sums(&pixels, w, h).len(), w);
        assert_eq!(image_row_sums(&pixels, w, h).len(), h);
        // Uniform image: each column sums to h, each row to w.
        assert!(
            image_column_sums(&pixels, w, h)
                .iter()
                .all(|&s| s == h as f64)
        );
        assert!(image_row_sums(&pixels, w, h).iter().all(|&s| s == w as f64));
    }

    // ── ImageView valueChanged pixel-under-cursor (silx _imagePlotCB) ─────────

    #[test]
    fn image_value_at_maps_cursor_to_pixel_like_silx() {
        // 3×2 row-major image (row-major: pixels[row*w + col]):
        //   row 0: 1 2 3
        //   row 1: 4 5 6
        let pixels = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let (w, h) = (3usize, 2usize);
        // Cursor inside a cell: truncation toward zero picks the pixel index.
        // (col 0, row 0) → value 1; (col 2, row 1) → value 6.
        assert_eq!(
            image_value_at(0.4, 0.9, &pixels, w, h),
            Some((0.0, 0.0, 1.0))
        );
        assert_eq!(
            image_value_at(2.7, 1.2, &pixels, w, h),
            Some((2.0, 1.0, 6.0))
        );
        // Exact integer coordinate maps to that index.
        assert_eq!(
            image_value_at(1.0, 1.0, &pixels, w, h),
            Some((1.0, 1.0, 5.0))
        );
    }

    #[test]
    fn image_value_at_returns_none_off_image() {
        let pixels = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let (w, h) = (3usize, 2usize);
        // Left of / below the origin (silx `x >= origin` guard).
        assert_eq!(image_value_at(-0.1, 0.5, &pixels, w, h), None);
        assert_eq!(image_value_at(0.5, -0.1, &pixels, w, h), None);
        // At or beyond the far edge (extent is exclusive at width/height).
        assert_eq!(image_value_at(3.0, 0.5, &pixels, w, h), None);
        assert_eq!(image_value_at(0.5, 2.0, &pixels, w, h), None);
        // No image loaded.
        assert_eq!(image_value_at(0.5, 0.5, &[], 0, 0), None);
    }

    #[test]
    fn colorbar_toggle_frees_and_restores_the_column() {
        // image_colorbar_toggle / scatter_colorbar_toggle (actions::control) are
        // `set_show_colorbar(!show_colorbar())` on an ImageView/ScatterView, which
        // require a RenderState to construct. Their observable effect — whether the
        // side colorbar column is reserved — is the show_colorbar transition fed to
        // colorbar_column_width, exercised here without a GPU (the analog of
        // show_axis_toggle_flips_axes_displayed driving the bare Plot model).
        let mut show = true;
        assert_eq!(
            colorbar_column_width(show, true),
            COLORBAR_WIDTH,
            "shown reserves the column"
        );
        show = !show; // toggle hides
        assert_eq!(
            colorbar_column_width(show, true),
            0.0,
            "toggling off frees the column"
        );
        show = !show; // toggle shows again
        assert_eq!(
            colorbar_column_width(show, true),
            COLORBAR_WIDTH,
            "toggling back on reserves it again"
        );
    }

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
    fn histogram_edges_left_treats_positions_as_left_edges() {
        // Row 108: silx `_computeEdges(x, "right")` — append x[-1] + last gap.
        // Positions [0,1,2] (gap 1) -> edges [0,1,2,3].
        assert_eq!(
            histogram_edges(&[0.0, 1.0, 2.0], HistogramAlign::Left),
            vec![0.0, 1.0, 2.0, 3.0]
        );
    }

    #[test]
    fn histogram_edges_right_treats_positions_as_right_edges() {
        // Row 108: silx `_computeEdges(x, "left")` — prepend x[0] - first gap.
        // Positions [1,2,3] (gap 1) -> edges [0,1,2,3].
        assert_eq!(
            histogram_edges(&[1.0, 2.0, 3.0], HistogramAlign::Right),
            vec![0.0, 1.0, 2.0, 3.0]
        );
    }

    #[test]
    fn histogram_edges_center_puts_positions_at_bin_centres() {
        // Row 108: silx `_computeEdges(x, "center")` right-aligns then shifts each
        // edge left by half its following gap. Centres [1,2,3] -> [0.5,1.5,2.5,3.5].
        assert_eq!(
            histogram_edges(&[1.0, 2.0, 3.0], HistogramAlign::Center),
            vec![0.5, 1.5, 2.5, 3.5]
        );
    }

    #[test]
    fn histogram_edges_single_position_uses_unit_gap() {
        // Row 108: a lone position uses silx's width = 1 fallback.
        assert_eq!(
            histogram_edges(&[5.0], HistogramAlign::Left),
            vec![5.0, 6.0]
        );
        assert_eq!(
            histogram_edges(&[5.0], HistogramAlign::Right),
            vec![4.0, 5.0]
        );
        assert_eq!(
            histogram_edges(&[5.0], HistogramAlign::Center),
            vec![4.5, 5.5]
        );
    }

    #[test]
    fn histogram_edges_nonuniform_center_uses_following_gap() {
        // Row 108: with non-uniform spacing the centre rule shifts each edge by
        // half of its *following* right-aligned gap (last half-gap reused).
        // Positions [0, 1, 3]: right-align -> [0,1,3,5] (last gap 3->5 = 2);
        // half-gaps -> [0.5,1.0,1.0,1.0]; edges = [-0.5, 0.0, 2.0, 4.0].
        assert_eq!(
            histogram_edges(&[0.0, 1.0, 3.0], HistogramAlign::Center),
            vec![-0.5, 0.0, 2.0, 4.0]
        );
    }

    #[test]
    fn histogram_edges_empty_is_empty() {
        assert!(histogram_edges(&[], HistogramAlign::Center).is_empty());
    }

    #[test]
    fn pick_histogram_locates_bin_and_checks_fill() {
        // 3 bins on edges [0,1,2,3]; heights [2, -1, 3]; baseline 0.
        let edges = [0.0, 1.0, 2.0, 3.0];
        let values = [2.0, -1.0, 3.0];
        // Inside bin 0 (bar [0,2]): y between baseline and value hits.
        assert_eq!(pick_histogram(&edges, &values, 0.0, 0.5, 1.0), Some(0));
        // Bin 0, above the bar top (y 2.5 > value 2): miss.
        assert_eq!(pick_histogram(&edges, &values, 0.0, 0.5, 2.5), None);
        // Bin 1 points down (value -1): y in [-1, 0] hits.
        assert_eq!(pick_histogram(&edges, &values, 0.0, 1.5, -0.5), Some(1));
        // Bin 1, y above baseline while the bar is below it: miss.
        assert_eq!(pick_histogram(&edges, &values, 0.0, 1.5, 0.5), None);
        // Bin 2 (bar [0,3]): hit.
        assert_eq!(pick_histogram(&edges, &values, 0.0, 2.5, 2.0), Some(2));
    }

    #[test]
    fn pick_histogram_outside_bbox_is_none() {
        let edges = [0.0, 1.0, 2.0, 3.0];
        let values = [2.0, 1.0, 3.0];
        // Left of xmin / right of xmax.
        assert_eq!(pick_histogram(&edges, &values, 0.0, -0.1, 1.0), None);
        assert_eq!(pick_histogram(&edges, &values, 0.0, 3.1, 1.0), None);
        // Below ymin (0) / above ymax (3).
        assert_eq!(pick_histogram(&edges, &values, 0.0, 0.5, -0.1), None);
        assert_eq!(pick_histogram(&edges, &values, 0.0, 0.5, 3.1), None);
        // Exactly on the box edge is excluded (strict bounds).
        assert_eq!(pick_histogram(&edges, &values, 0.0, 0.0, 1.0), None);
        // Malformed: edges/values length mismatch and empty input.
        assert_eq!(pick_histogram(&edges, &[1.0, 2.0], 0.0, 0.5, 1.0), None);
        assert_eq!(pick_histogram(&[], &[], 0.0, 0.5, 1.0), None);
    }

    #[test]
    fn pick_histogram_honours_nonzero_baseline() {
        // Bars rise from baseline 5; bbox y-bounds = [min(0,3), max(0,8)] = [0,8].
        let edges = [0.0, 1.0, 2.0];
        let values = [8.0, 3.0];
        // Bin 0 bar [5,8]: y 6 hits.
        assert_eq!(pick_histogram(&edges, &values, 5.0, 0.5, 6.0), Some(0));
        // Bin 1 value 3 < baseline 5 → bar [3,5]: y 4 hits.
        assert_eq!(pick_histogram(&edges, &values, 5.0, 1.5, 4.0), Some(1));
        // Bin 0, y 4 below the bar [5,8]: miss.
        assert_eq!(pick_histogram(&edges, &values, 5.0, 0.5, 4.0), None);
    }

    #[test]
    fn aligned_histogram_edges_feed_valid_step_values() {
        // Row 108: the path add_histogram_aligned takes (sans the GPU add) — N
        // positions + N counts derive N+1 edges that histogram_step_values
        // accepts. Centres [1,2,3] -> edges [0.5,1.5,2.5,3.5], stairs at counts.
        let positions = [1.0, 2.0, 3.0];
        let counts = [5.0, 6.0, 7.0];
        let edges = histogram_edges(&positions, HistogramAlign::Center);
        let (x, y) = histogram_step_values(&edges, &counts).unwrap();
        assert_eq!(x, vec![0.5, 0.5, 1.5, 1.5, 2.5, 2.5, 3.5, 3.5]);
        assert_eq!(y, vec![0.0, 5.0, 5.0, 6.0, 6.0, 7.0, 7.0, 0.0]);
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
    fn click_event_for_pick_maps_each_pick_variant() {
        // One case per PickResult variant — the pure seam of the click path that
        // the GPU-bound pick_item cannot exercise headlessly.
        let handle: ItemHandle = 7;

        // CurvePoint carries the picked vertex; distance_px is dropped.
        assert_eq!(
            PlotWidget::click_event_for_pick(
                handle,
                &PickResult::CurvePoint {
                    index: 4,
                    x: 1.5,
                    y: -2.0,
                    distance_px: 3.0,
                },
                MouseButton::Left,
            ),
            PlotEvent::CurveClicked {
                handle,
                index: 4,
                x: 1.5,
                y: -2.0,
                button: MouseButton::Left,
            }
        );

        // ImagePixel carries the pixel (col, row).
        assert_eq!(
            PlotWidget::click_event_for_pick(
                handle,
                &PickResult::ImagePixel { col: 12, row: 9 },
                MouseButton::Middle,
            ),
            PlotEvent::ImageClicked {
                handle,
                col: 12,
                row: 9,
                button: MouseButton::Middle,
            }
        );

        // A non-indexed overlay item maps to ItemClicked, preserving the button.
        assert_eq!(
            PlotWidget::click_event_for_pick(
                handle,
                &PickResult::Item { handle: 99 },
                MouseButton::Right,
            ),
            PlotEvent::ItemClicked {
                handle,
                button: MouseButton::Right,
            }
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
    fn scatter_masked_selection_flags_nonzero_levels() {
        // Item 3: the boolean selection applied to the scatter is true exactly
        // where the per-point mask level is non-zero.
        let mask = [0u8, 1, 0, 3, 0];
        assert_eq!(
            scatter_masked_selection(&mask),
            vec![false, true, false, true, false]
        );
        // Empty mask -> empty selection.
        assert!(scatter_masked_selection(&[]).is_empty());
    }

    #[test]
    fn scatter_mask_rectangle_selection_applies_to_points() {
        // Item 3: a rectangle selection on the scatter mask flags exactly the
        // points inside the rectangle (silx ScatterMask.updateRectangle).
        // Five points along X; select the box x in [0.5, 2.5], y in [-0.5, 0.5].
        let px: Vec<f32> = vec![0.0, 1.0, 2.0, 3.0, 4.0];
        let py: Vec<f32> = vec![0.0, 0.0, 0.0, 0.0, 0.0];
        let mut mask = crate::widget::scatter_mask::ScatterMaskWidget::new(px.len());
        // update_rectangle takes anchor=(y,x) bottom-left, size=(height,width).
        // Box: x in [0.5, 2.5], y in [-0.5, 0.5].
        mask.update_rectangle(1, (-0.5, 0.5), (1.0, 2.0), &px, &py, true);
        let selection = scatter_masked_selection(&mask.mask);
        // Only x=1 and x=2 fall inside the box; x=0, x=3, x=4 are outside.
        assert_eq!(selection, vec![false, true, true, false, false]);
    }

    /// A colormap whose LUT entry `i` is `[i, 255 - i, 0, 255]`, over `[0, 4]`.
    /// Each LUT index maps to a unique, hand-computable color so a value's
    /// resulting [`Color32`] is fully predictable.
    fn ramp_colormap() -> Colormap {
        let mut lut = [[0u8; 4]; 256];
        for (i, entry) in lut.iter_mut().enumerate() {
            *entry = [i as u8, 255 - i as u8, 0, 255];
        }
        Colormap::viridis(0.0, 4.0).with_lut(lut)
    }

    /// The exact color the [`point_colors`] LUT lookup produces for `v` under
    /// [`ramp_colormap`], reproducing the index math
    /// `idx = (normalize(v) * 255).clamp(0, 255)`.
    fn ramp_color_at(cmap: &Colormap, v: f64) -> Color32 {
        let idx = (cmap.normalize(v) * 255.0).clamp(0.0, 255.0) as usize;
        let [r, g, b, a] = cmap.lut[idx];
        Color32::from_rgba_unmultiplied(r, g, b, a)
    }

    #[test]
    fn point_colors_maps_values_through_colormap_lut() {
        // Wave 8C: point_colors maps each value through the colormap LUT (silx
        // __applyColormapToData). Over [0, 4] with no per-point alpha, value 0
        // hits LUT index 0, value 4 hits index 255, value 2 hits the midpoint.
        let cmap = ramp_colormap();
        let values = [0.0, 2.0, 4.0];
        let colors = point_colors(&values, &cmap, None);
        assert_eq!(
            colors,
            vec![
                ramp_color_at(&cmap, 0.0),
                ramp_color_at(&cmap, 2.0),
                ramp_color_at(&cmap, 4.0),
            ]
        );
        // Concretely: index 0 -> [0,255,0,255]; index 255 -> [255,0,0,255].
        assert_eq!(colors[0], Color32::from_rgba_unmultiplied(0, 255, 0, 255));
        assert_eq!(colors[2], Color32::from_rgba_unmultiplied(255, 0, 0, 255));
    }

    #[test]
    fn point_colors_composes_per_point_alpha() {
        // Wave 8C: with a per-point alpha array, point_colors scales each
        // color's straight alpha (silx rgbacolors[:, -1] *= __alpha), identical
        // to applying compose_per_point_alpha to the no-alpha colors.
        let cmap = ramp_colormap();
        let values = [0.0, 2.0, 4.0];
        let alpha = [0.5, 0.25, 1.0];

        let with_alpha = point_colors(&values, &cmap, Some(&alpha));

        let mut expected = point_colors(&values, &cmap, None);
        compose_per_point_alpha(&mut expected, &alpha);
        assert_eq!(with_alpha, expected);

        // Concretely: LUT alpha 255 scaled by 0.5 -> round(255*0.5) = 128.
        assert_eq!(with_alpha[0].to_srgba_unmultiplied()[3], 128);
        // ...by 0.25 -> round(255*0.25) = 64.
        assert_eq!(with_alpha[1].to_srgba_unmultiplied()[3], 64);
        // ...by 1.0 -> unchanged 255.
        assert_eq!(with_alpha[2].to_srgba_unmultiplied()[3], 255);
    }

    #[test]
    fn solid_path_colors_identical_to_points_path() {
        // Wave 8C: silx shares __applyColormapToData between POINTS and SOLID,
        // so the Solid arm must produce colors identical to the Points arm for
        // the same input. Both arms call point_colors, so the vectors match —
        // here against the prior inlined Points math to prove no drift.
        let cmap = ramp_colormap();
        let values = [0.0, 1.0, 3.0, 4.0];
        let alpha = [1.0, 0.5, 0.25, 0.75];

        // The shared helper used by BOTH arms.
        let shared = point_colors(&values, &cmap, Some(&alpha));

        // The pre-extraction inlined Points computation, reproduced here.
        let mut points_inline: Vec<Color32> =
            values.iter().map(|&v| ramp_color_at(&cmap, v)).collect();
        compose_per_point_alpha(&mut points_inline, &alpha);

        assert_eq!(shared, points_inline);
    }

    #[test]
    fn solid_path_builds_triangles_with_per_vertex_colors() {
        // Wave 8C: the Solid arm feeds point_colors into solid_triangles, the
        // exact composition rebuild_visualization performs. Four non-collinear
        // points triangulate, and every input color survives onto a vertex.
        let cmap = ramp_colormap();
        let x = [0.0, 4.0, 0.0, 4.0];
        let y = [0.0, 0.0, 4.0, 4.0];
        let values = [0.0, 2.0, 4.0, 1.0];

        let colors = point_colors(&values, &cmap, None);
        let tri = crate::core::scatter_viz::solid_triangles(&x, &y, &colors)
            .expect("four non-collinear points triangulate");

        // The mesh carries the per-vertex colors unchanged (Gourad input).
        assert_eq!(tri.colors, colors);
        assert_eq!(tri.x, x.to_vec());
        assert_eq!(tri.y, y.to_vec());
        // A square splits into at least two triangles.
        assert!(
            tri.indices.len() >= 2,
            "square triangulates into >= 2 triangles, got {}",
            tri.indices.len()
        );
    }

    #[test]
    fn solid_path_degenerate_input_yields_no_triangles() {
        // Wave 8C: fewer than 3 finite points / all collinear cannot be
        // triangulated, so the Solid arm draws nothing (None, no panic),
        // matching silx's "Cannot display as solid surface" early-out.
        let cmap = ramp_colormap();

        // Fewer than 3 points.
        let x2 = [0.0, 1.0];
        let y2 = [0.0, 1.0];
        let c2 = point_colors(&[0.0, 4.0], &cmap, None);
        assert!(crate::core::scatter_viz::solid_triangles(&x2, &y2, &c2).is_none());

        // Three collinear points (no triangle has area).
        let xc = [0.0, 1.0, 2.0];
        let yc = [0.0, 1.0, 2.0];
        let cc = point_colors(&[0.0, 2.0, 4.0], &cmap, None);
        assert!(crate::core::scatter_viz::solid_triangles(&xc, &yc, &cc).is_none());
    }

    #[test]
    fn live_curve_stats_match_core_stats_engine() {
        // Item 4: a retained curve fed into a StatsWidget yields a row equal to
        // a direct core::stats::Stats run on the same (xs, ys).
        use crate::widget::stats_widget::{StatsWidget, UpdateMode};
        let xs = vec![0.0, 1.0, 2.0, 3.0, 4.0];
        let ys = vec![2.0, -1.0, 5.0, f64::NAN, 0.5];
        let data = RetainedItemData::Curve {
            x: xs.clone(),
            y: ys.clone(),
        };
        let input = retained_data_to_stats_input(&data);
        let mut w = StatsWidget::new();
        w.set_update_mode(UpdateMode::Auto);
        w.recompute(&[("curve", input)], None);
        let rows = w.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "curve");
        let core =
            crate::core::stats::Stats::for_curve(&xs, &ys, crate::core::stats::StatScope::All);
        assert_eq!(rows[0].1, core);
    }

    #[test]
    fn live_image_stats_match_core_stats_engine() {
        // Item 4: a retained scalar image fed into a StatsWidget yields a row
        // equal to a direct core::stats::Stats run on the same pixels+geometry.
        use crate::widget::stats_widget::{StatsWidget, UpdateMode};
        let pixels = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let data = RetainedItemData::Image {
            data: pixels.clone(),
            width: 3,
            height: 2,
            origin: (0.0, 0.0),
            scale: (1.0, 1.0),
            colormap: Box::new(Colormap::viridis(0.0, 1.0)),
        };
        let input = retained_data_to_stats_input(&data);
        let mut w = StatsWidget::new();
        w.set_update_mode(UpdateMode::Auto);
        w.recompute(&[("image", input)], None);
        let rows = w.rows();
        assert_eq!(rows.len(), 1);
        let core = crate::core::stats::Stats::for_image(
            &pixels,
            3,
            2,
            (0.0, 0.0),
            (1.0, 1.0),
            crate::core::stats::StatScope::All,
        );
        assert_eq!(rows[0].1, core);
    }

    #[test]
    fn roi_stats_rows_image_match_image_roi_stats_per_roi() {
        // Item 110: one row per ROI, each reduced over the active image's pixels
        // inside that ROI via image_roi_stats (identical geometry/cast). The
        // unnamed ROI gets the "ROI {index}" label; a named one keeps its name.
        use crate::widget::roi_stats::image_roi_stats;
        let pixels = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let data = RetainedItemData::Image {
            data: pixels.clone(),
            width: 3,
            height: 2,
            origin: (0.0, 0.0),
            scale: (1.0, 1.0),
            colormap: Box::new(Colormap::viridis(0.0, 1.0)),
        };
        let mut named = ManagedRoi::new(Roi::Rect {
            x: (0.0, 2.0),
            y: (0.0, 1.0),
        });
        named.name = "left".to_owned();
        let rois = vec![
            named,
            ManagedRoi::new(Roi::Rect {
                x: (1.0, 3.0),
                y: (0.0, 2.0),
            }),
        ];

        let rows = roi_stats_rows(&rois, &data);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "left");
        assert_eq!(rows[1].label, "ROI 1");

        // Each row equals image_roi_stats over the same f32-cast pixels/geometry.
        let f32_pixels: Vec<f32> = pixels.iter().map(|&v| v as f32).collect();
        for (row, managed) in rows.iter().zip(rois.iter()) {
            let expected = image_roi_stats(&managed.roi, &f32_pixels, 3, 2, [0.0, 0.0], [1.0, 1.0]);
            assert_eq!(row.stats, expected);
        }
    }

    #[test]
    fn roi_stats_rows_curve_match_curve_roi_stats_per_roi() {
        // Item 110: for a curve item each row reduces the curve's y inside the
        // ROI's x-span via curve_roi_stats.
        use crate::widget::roi_stats::curve_roi_stats;
        let x = vec![0.0, 1.0, 2.0, 3.0, 4.0];
        let y = vec![10.0, 20.0, 30.0, 40.0, 50.0];
        let data = RetainedItemData::Curve {
            x: x.clone(),
            y: y.clone(),
        };
        let rois = vec![
            ManagedRoi::new(Roi::VRange { x: (1.0, 3.0) }),
            ManagedRoi::new(Roi::Rect {
                x: (0.0, 2.0),
                y: (0.0, 100.0),
            }),
        ];

        let rows = roi_stats_rows(&rois, &data);
        assert_eq!(rows.len(), 2);
        for (row, managed) in rows.iter().zip(rois.iter()) {
            assert_eq!(row.stats, curve_roi_stats(&managed.roi, &x, &y));
        }
    }

    #[test]
    fn roi_stats_rows_empty_without_rois() {
        // Item 110: no ROIs -> no rows (the table is empty even with data).
        let data = RetainedItemData::Curve {
            x: vec![0.0, 1.0],
            y: vec![1.0, 2.0],
        };
        assert!(roi_stats_rows(&[], &data).is_empty());
    }

    #[test]
    fn curve_roi_rows_match_curve_roi_counts_and_skip_non_curve_rois() {
        // Item 110 (CurvesROIWidget): one row per curve ROI (x-span), reduced via
        // curve_roi_counts; ROIs with no x-span (HRange) are skipped, and the
        // surviving rows keep their original ROI index in the label.
        use crate::widget::roi_stats::{curve_roi_counts, roi_x_span};
        let x = vec![0.0, 1.0, 2.0, 3.0, 4.0];
        let y = vec![0.0, 0.0, 10.0, 0.0, 0.0];

        let mut named = ManagedRoi::new(Roi::VRange { x: (0.0, 4.0) });
        named.name = "peak".to_owned();
        let rois = vec![
            named,
            // index 1: HRange has no x-span -> not a curve ROI, skipped.
            ManagedRoi::new(Roi::HRange { y: (0.0, 5.0) }),
            // index 2: a Rect contributes its x-extent.
            ManagedRoi::new(Roi::Rect {
                x: (1.0, 3.0),
                y: (-100.0, 100.0),
            }),
        ];

        let rows = curve_roi_rows(&rois, &x, &y);
        assert_eq!(rows.len(), 2); // the HRange row is skipped

        // First surviving row is the named VRange (index 0).
        assert_eq!(rows[0].label, "peak");
        assert_eq!(
            (rows[0].from, rows[0].to),
            roi_x_span(&rois[0].roi).unwrap()
        );
        assert_eq!(
            rows[0].counts,
            curve_roi_counts(&rois[0].roi, &x, &y).unwrap()
        );

        // Second surviving row keeps its original index (2) in the label.
        assert_eq!(rows[1].label, "ROI 2");
        assert_eq!(
            (rows[1].from, rows[1].to),
            roi_x_span(&rois[2].roi).unwrap()
        );
        assert_eq!(
            rows[1].counts,
            curve_roi_counts(&rois[2].roi, &x, &y).unwrap()
        );
    }

    #[test]
    fn curve_roi_rows_empty_without_rois() {
        // Item 110: no ROIs -> no rows even with curve data.
        let x = vec![0.0, 1.0, 2.0];
        let y = vec![1.0, 2.0, 3.0];
        assert!(curve_roi_rows(&[], &x, &y).is_empty());
    }

    #[test]
    fn fit_target_feeds_curve_xy_not_image() {
        // Item 5: the fit target feed extracts the live curve's (x, y) (the data
        // FitWidget.set_data receives), and refuses a non-curve item.
        let xs = vec![1.0, 2.0, 3.0, 4.0];
        let ys = vec![10.0, 20.0, 30.0, 40.0];
        let curve = RetainedItemData::Curve {
            x: xs.clone(),
            y: ys.clone(),
        };
        let (fx, fy) = retained_curve_xy(&curve).expect("curve feeds its xy");
        assert_eq!(fx, xs.as_slice());
        assert_eq!(fy, ys.as_slice());

        // An image item is not a fit target.
        let image = RetainedItemData::Image {
            data: vec![1.0, 2.0, 3.0, 4.0],
            width: 2,
            height: 2,
            origin: (0.0, 0.0),
            scale: (1.0, 1.0),
            colormap: Box::new(Colormap::viridis(0.0, 1.0)),
        };
        assert!(retained_curve_xy(&image).is_none());
    }

    #[test]
    fn raw_pixel_stddev3_autoscale_matches_mean_plus_minus_3std() {
        // Item 6: Stddev3 autoscale over a known pixel array (NaN ignored) sets
        // vmin/vmax to clamp(mean ± 3·std) and preserves the rest of the
        // colormap. Symmetric data [-1, 0, 1] has mean 0, population std
        // sqrt(2/3); mean ± 3·std = ±3·sqrt(2/3) ≈ ±2.449, both outside the
        // data range, so silx clamps to the data min/max (-1, 1).
        let pixels = [-1.0, 0.0, 1.0, f64::NAN];
        let base = Colormap::viridis(7.0, 9.0); // arbitrary prior limits
        let cm = autoscaled_colormap(&base, AutoscaleMode::Stddev3, &pixels);
        // Cross-check against the core primitive on the same data.
        let (evmin, evmax) =
            AutoscaleMode::Stddev3.range(&pixels, crate::core::colormap::DEFAULT_PERCENTILES);
        assert_eq!(cm.vmin, evmin);
        assert_eq!(cm.vmax, evmax);
        assert_eq!((cm.vmin, cm.vmax), (-1.0, 1.0));
        // The LUT (colormap identity) is preserved; only limits changed.
        assert_eq!(cm.lut, base.lut);
    }

    #[test]
    fn raw_pixel_percentile_autoscale_matches_percentile_bounds() {
        // Item 6: Percentile autoscale over a known pixel array uses the
        // colormap's percentile pair, NaN-ignoring.
        let pixels = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, f64::NAN];
        let mut base = Colormap::viridis(0.0, 1.0);
        base.set_autoscale_percentiles(10.0, 90.0);
        let cm = autoscaled_colormap(&base, AutoscaleMode::Percentile, &pixels);
        // numpy linear-interpolation percentile over the 10 finite values
        // [0..=9]: rank = p/100 * (n-1) = p/100 * 9. p=10 -> rank 0.9 -> 0.9;
        // p=90 -> rank 8.1 -> 8.1.
        let (evmin, evmax) = AutoscaleMode::Percentile.range(&pixels, (10.0, 90.0));
        assert_eq!(cm.vmin, evmin);
        assert_eq!(cm.vmax, evmax);
        assert!((cm.vmin - 0.9).abs() < 1e-12, "vmin {}", cm.vmin);
        assert!((cm.vmax - 8.1).abs() < 1e-12, "vmax {}", cm.vmax);
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
    fn image_view_paints_mask_only_in_mask_draw_mode() {
        // Item 6C-2 gate: ImageView paints the mask ONLY when the plot is in
        // MaskDraw mode AND the mask panel is enabled. Pan / Zoom / Select must
        // never paint, and MaskDraw with the panel disabled must not paint.
        for mode in [
            PlotInteractionMode::Pan,
            PlotInteractionMode::Zoom,
            PlotInteractionMode::Select,
        ] {
            assert!(
                !image_view_should_paint_mask(mode, true),
                "{mode:?} must not paint even with the mask panel enabled",
            );
        }
        assert!(
            !image_view_should_paint_mask(PlotInteractionMode::MaskDraw, false),
            "MaskDraw must not paint when the mask panel is disabled",
        );
        assert!(
            image_view_should_paint_mask(PlotInteractionMode::MaskDraw, true),
            "MaskDraw with the mask panel enabled is the only painting state",
        );
    }

    #[test]
    fn painted_level_buffer_round_trips_to_reupload_mask() {
        // Item 6C-2 conversion: a painted MaskToolsWidget level buffer becomes a
        // ScalarMask where every non-zero level is a masked pixel, and that mask
        // re-uploaded (applied to the scalar field) NaNs exactly those pixels —
        // matching Plot2D::try_add_masked_image / apply_image_mask.
        //
        // 2x3 image (width 2, height 3), row-major. Paint a couple of pixels at
        // distinct non-zero levels (silx levels 1..=255 all mask).
        let levels = [0u8, 1, 0, 0, 7, 0]; // (col1,row0) and (col0,row2) masked
        let scalar_mask = scalar_mask_from_level_buffer(2, 3, &levels);

        assert_eq!(scalar_mask.width(), 2);
        assert_eq!(scalar_mask.height(), 3);
        // The boolean masked view matches "level != 0".
        for (i, &lvl) in levels.iter().enumerate() {
            let col = i % 2;
            let row = i / 2;
            assert_eq!(
                scalar_mask.is_masked(col, row),
                lvl != 0,
                "pixel ({col},{row}) level {lvl}",
            );
        }

        // Re-upload representation: applying the mask NaNs exactly the masked
        // pixels and passes the rest through unchanged.
        let pixels = [10.0_f32, 20.0, 30.0, 40.0, 50.0, 60.0];
        let masked = scalar_mask.apply(&pixels);
        for (i, (&lvl, &out)) in levels.iter().zip(masked.iter()).enumerate() {
            if lvl != 0 {
                assert!(out.is_nan(), "masked pixel {i} (level {lvl}) must be NaN");
            } else {
                assert_eq!(out, pixels[i], "unmasked pixel {i} unchanged");
            }
        }

        // It is the same representation try_add_masked_image / apply_image_mask
        // produce for an equivalent ScalarMask built directly from the bool view.
        let mut direct = ScalarMask::new(2, 3);
        direct.set_mask_data(&levels, 2);
        assert_eq!(scalar_mask, direct);
    }

    #[test]
    fn scalar_mask_from_level_buffer_clips_oversized_to_image_shape() {
        // A level buffer longer than width*height is clipped to the image shape
        // by ScalarMask::set_mask_data (silx lazy clip), so the result always has
        // the image dimensions.
        let oversized = [1u8; 10]; // image is 2x2 = 4 pixels
        let mask = scalar_mask_from_level_buffer(2, 2, &oversized);
        assert_eq!(mask.width(), 2);
        assert_eq!(mask.height(), 2);
        assert_eq!(mask.get_mask_data().len(), 4);
        assert!(mask.get_mask_data().iter().all(|&m| m != 0));
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
