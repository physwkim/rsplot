//! egui-silx — silx-style scientific plotting on top of egui + wgpu.
//!
//! See `doc/design.md` for the design. The layering mirrors silx's
//! `BackendBase ↔ BackendPygfx`:
//!
//! - [`core`]   — the `Plot` model + `Backend` trait + shared types (Transform/Colormap …)
//! - [`render`] — the wgpu renderer (`egui_wgpu::CallbackTrait` impl), owns GPU resources
//! - [`widget`] — high-level plotting widgets plus [`PlotView`] for chrome + interaction + paint-callback registration
//!
//! Current scope: vertical slice 1, steps 1–5 — a wgpu data layer (clear,
//! colormapped image, polyline curve) under egui chrome (frame, grid, ticks,
//! tick labels, colorbar), all sharing one coordinate transform.

pub mod core;
pub mod render;
pub mod widget;

pub use crate::core::backend::{
    Backend, CurveColor, CurveSpec, ImagePixelsSpec, ImageSpec, ItemHandle, MarkerSpec, PickResult,
    ShapeSpec, TriangleSpec,
};
pub use crate::core::colormap::{
    AutoscaleMode, Colormap, ColormapName, DEFAULT_PERCENTILES, Normalization,
};
pub use crate::core::decimate::min_max_decimate;
pub use crate::core::items::{Baseline, ErrorBars, LineStyle, Symbol};
pub use crate::core::marker::{DEFAULT_MARKER_SIZE, Marker, MarkerKind, MarkerSymbol};
pub use crate::core::plot::{AxisConstraints, GraphGrid, Plot, PlotId};
pub use crate::core::roi::{Roi, RoiEdge};
pub use crate::core::shape::{Shape, ShapeKind};
pub use crate::core::transform::{Axis, Margins, Scale, Transform, YAxis};
pub use crate::core::triangles::Triangles;
pub use crate::render::backend_wgpu::{
    WgpuBackend, install, set_curve, set_curves, set_image, set_images, update_curve,
    update_curve_at, update_image_region,
};
pub use crate::render::gpu_curve::CurveData;
pub use crate::render::gpu_image::{
    AggregationMode, ImageData, ImagePixels, InterpolationMode, aggregate_blocks,
};
pub use crate::render::save::{SaveError, encode_png, save_graph};
pub use crate::widget::colormap_dialog::ColormapDialog;
pub use crate::widget::complex_image_view::{ComplexImageView, ComplexMode};
pub use crate::widget::fit_widget::FitWidget;
pub use crate::widget::high_level::{
    CompareImages, CompareMode, CurveStats, ImageGeometry, ImageStats, ImageView, ItemStats,
    LegendResponse, Plot1D, Plot2D, PlotDataError, PlotEvent, PlotItemKind, PlotWidget, PlotWindow,
    PlotWithToolbarResponse, ProfileMode, ScatterView, StackView, ToolbarResponse, ValueStats,
    histogram_step_values, horizontal_profile_values, line_profile_values, rect_profile_values,
    vertical_profile_values,
};
pub use crate::widget::interaction::{PanDirection, PointPick, image_index, nearest_point};
pub use crate::widget::limits_widget::LimitsWidget;
pub use crate::widget::mask_tools::{MaskTool, MaskToolsWidget, ThresholdMode};
pub use crate::widget::plot_widget::{PlotInteractionMode, PlotResponse, PlotView};
pub use crate::widget::profile_window::ProfileWindow;
pub use crate::widget::roi_manager::{ManagedRoi, RoiManagerWidget};
pub use crate::widget::sync::SyncAxes;

// Plotting-library convention: re-export so downstreams use the same
// egui/egui-wgpu without version skew.
pub use egui;
pub use egui_wgpu;
