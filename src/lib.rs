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
pub use crate::core::dtime_ticks::{
    DateTime, DtUnit, best_unit, calc_ticks, calc_ticks_adaptive, format_tick, format_ticks,
};
pub use crate::core::fitting::{
    DEFAULT_DELTACHI, DEFAULT_MAX_ITER, FitError, FitFunction, FitResult, GaussianEstimateFit,
    IterativeFit, IterativeFitResult, LOG2, LeastSqResult, LinearFit, PeakModel, estimate_gaussian,
    estimate_gaussian_area, estimate_height_position_fwhm, estimate_lorentzian,
    estimate_pseudo_voigt, fit_in_range, fwhm_to_sigma_factor, gaussian_area_model, gaussian_model,
    invert_matrix, leastsq, lorentzian_model, pseudo_voigt_model,
};
pub use crate::core::items::{Baseline, ErrorBars, LineStyle, ScalarMask, Symbol};
pub use crate::core::marker::{
    DEFAULT_MARKER_SIZE, Marker, MarkerConstraint, MarkerKind, MarkerSymbol, TextAnchor,
    apply_constraint,
};
pub use crate::core::plot::{
    AxisConstraints, DataMargins, DataRange, DirtyState, GraphGrid, Plot, PlotId,
    resolved_axis_label,
};
pub use crate::core::roi::{Roi, RoiEdge};
pub use crate::core::scatter_viz::{
    BinnedStatistic, BinnedStatisticFunction, GridImage, GridMajorOrder, PointsViz, RegularGrid,
    Triangulation, binned_statistic, delaunay, detect_regular_grid, interpolate,
    irregular_grid_image, solid_triangles,
};
pub use crate::core::shape::{Line, Shape, ShapeKind};
pub use crate::core::stats::{ComCoord, StatScope, Stats};
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
pub use crate::render::save::{
    SaveError, encode_png, encode_ppm, encode_svg, encode_tiff, rgba_to_rgb, save_graph,
};
pub use crate::widget::alpha_slider::{AlphaSlider, AlphaSliderOrientation};
pub use crate::widget::colorbar::{ColorBarOrientation, ColorBarWidget};
pub use crate::widget::colormap_dialog::ColormapDialog;
pub use crate::widget::complex_image_view::{ComplexImageView, ComplexMode};
pub use crate::widget::fit_widget::{
    FitModelChoice, FitWidget, format_param_value_error, format_reduced_chisq,
};
pub use crate::widget::high_level::{
    CompareImages, CompareMode, CurveStats, ImageGeometry, ImageStats, ImageView, ItemStats,
    LegendResponse, Plot1D, Plot2D, PlotDataError, PlotEvent, PlotItemKind, PlotWidget, PlotWindow,
    PlotWithToolbarResponse, ProfileMode, ScatterView, StackView, ToolbarResponse, ValueStats,
    histogram_step_values, horizontal_profile_values, line_profile_values, rect_profile_values,
    vertical_profile_values,
};
pub use crate::widget::image_stack::{Frame, ImageStack};
pub use crate::widget::interaction::{
    CursorShape, DrawEvent, DrawInput, DrawMode, DrawParams, DrawState, FillMode, MouseButton,
    PanDirection, PlotPointerEvent, PointPick, SelectionStyle, cursor_for_edge, cursor_for_grab,
    ellipse_semi_axes, hatch_lines, image_index, nearest_point,
};
pub use crate::widget::limits_widget::LimitsWidget;
pub use crate::widget::mask_tools::{MaskTool, MaskToolsWidget, ThresholdMode};
pub use crate::widget::plot_widget::{DrawResponse, PlotInteractionMode, PlotResponse, PlotView};
pub use crate::widget::position_info::{PositionInfo, format_value};
pub use crate::widget::profile_window::ProfileWindow;
pub use crate::widget::roi_manager::{ManagedRoi, RoiManagerWidget};
pub use crate::widget::roi_stats::{RoiStats, curve_roi_stats, image_roi_stats, roi_x_span};
pub use crate::widget::scatter_mask::{ScatterMaskWidget, point_in_polygon};
pub use crate::widget::stats_widget::{
    StatsInput, StatsWidget, UpdateMode, format_significant, format_stat,
};
pub use crate::widget::sync::SyncAxes;

// Plotting-library convention: re-export so downstreams use the same
// egui/egui-wgpu without version skew.
pub use egui;
pub use egui_wgpu;
