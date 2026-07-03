//! siplot — silx-style scientific plotting on top of egui + wgpu.
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
pub use crate::core::background::{
    Background, DEFAULT_SNIP_WIDTH, DEFAULT_STRIP_ITERATIONS, DEFAULT_STRIP_THRESHOLD_FACTOR,
    DEFAULT_STRIP_WIDTH, poly_eval, polyfit, snip_background, strip_background,
};
pub use crate::core::calibration::Calibration;
pub use crate::core::colormap::{
    AutoscaleMode, Colormap, ColormapName, DEFAULT_PERCENTILES, Normalization, register_colormap,
    registered_colormap_cursor_color, registered_colormaps,
};
pub use crate::core::colormap_io::{
    ColormapIoError, decode_colormap, encode_colormap, load_colormap, save_colormap,
};
pub use crate::core::decimate::min_max_decimate;
pub use crate::core::dtime_ticks::{
    DateTime, DtUnit, TimeZone, best_unit, calc_ticks, calc_ticks_adaptive, calc_ticks_adaptive_tz,
    calc_ticks_tz, format_tick, format_tick_tz, format_ticks, format_ticks_tz,
};
pub use crate::core::fitting::{
    BackgroundPeakFit, Constraint, DEFAULT_DELTACHI, DEFAULT_MAX_ITER, FitError, FitFunction,
    FitResult, GaussianEstimateFit, IterativeFit, IterativeFitResult, LOG2, LeastSqResult,
    LinearFit, PeakModel, atan_stepup_model, estimate_atan_stepup, estimate_gaussian,
    estimate_gaussian_area, estimate_height_position_fwhm, estimate_lorentzian,
    estimate_multi_gaussian, estimate_pseudo_voigt, estimate_slit, estimate_stepdown,
    estimate_stepup, fit_in_range, fit_multi_gaussian, fit_multi_gaussian_full,
    fit_peak_constrained, fit_peak_from, fit_peak_with_background, fwhm_to_sigma_factor,
    gaussian_area_model, gaussian_model, invert_matrix, leastsq, leastsq_constrained,
    lorentzian_model, multi_gaussian_model, pseudo_voigt_model, slit_model, stepdown_model,
    stepup_model,
};
pub use crate::core::items::{Baseline, ErrorBars, LineStyle, ScalarMask, Symbol};
pub use crate::core::marker::{
    DEFAULT_MARKER_SIZE, Marker, MarkerConstraint, MarkerKind, TextAnchor, apply_constraint,
};
pub use crate::core::peaks::{
    DEFAULT_PEAK_SENSITIVITY, Peak, guess_fwhm, peak_search, peak_search_range,
};
pub use crate::core::plot::{
    AxisConstraints, DataMargins, DataRange, DirtyState, ExtraAxis, GraphGrid, Plot, PlotId,
    TickMode, resolved_axis_label,
};
pub use crate::core::roi::{BandLines, ManagedRoi, Roi, RoiEdge, RoiInteractionMode, RoiLineStyle};
pub use crate::core::roi_io::{RoiIoError, decode_rois, encode_rois, load_rois, save_rois};
pub use crate::core::scatter_viz::{
    BinnedStatistic, BinnedStatisticFunction, GridImage, GridMajorOrder, PointsViz, RegularGrid,
    ScatterLineProfile, Triangulation, binned_statistic, delaunay, detect_regular_grid,
    interpolate, irregular_grid_image, irregular_grid_pick, irregular_grid_triangles,
    regular_grid_pick, scatter_line_profile, solid_triangles,
};
pub use crate::core::scene3d::camera::{
    Camera, CameraDirection, CameraExtrinsic, CameraFace, CameraMove, Orthographic, Perspective,
    Projection,
};
pub use crate::core::scene3d::interaction::{OrbitDrag, PanDrag, window_to_ndc};
pub use crate::core::scene3d::marching_cubes::{MarchingCubes, MarchingCubesError, isosurface};
pub use crate::core::scene3d::mat4::{
    Mat4, Vec3, mat4_look_at_dir, mat4_orthographic, mat4_perspective, mat4_rotate, mat4_scale,
    mat4_translate,
};
pub use crate::core::scene3d::pick::{
    TriangleHit, picking_segment, segment_triangles_intersection,
};
pub use crate::core::scene3d::plane::{
    Plane, angle_between_vectors, box_plane_intersect, segment_plane_intersect,
};
pub use crate::core::scene3d::transform::{Item3DTransform, RotationCenter, transform_aabb};
pub use crate::core::shape::{Line, Shape, ShapeKind, triangulate_simple_polygon};
pub use crate::core::sift_align::{
    AffineTransformation, MATCH_RATIO_THRESHOLD, MatchedKeypoint, SiftAlignment, decompose_affine,
    sift_auto_align,
};
pub use crate::core::stats::{ComCoord, StatScope, Stats};
pub use crate::core::transform::{Axis, AxisSide, Margins, Scale, Transform, YAxis};
pub use crate::core::triangles::Triangles;
pub use crate::render::backend_wgpu::{
    WgpuBackend, install, set_curve, set_curves, set_image, set_images, update_curve,
    update_curve_at, update_image_region,
};
pub use crate::render::gpu_curve::CurveData;
pub use crate::render::gpu_image::{
    AggregationMode, ImageData, ImagePixels, InterpolationMode, aggregate_blocks,
};
pub use crate::render::gpu_scene3d::{
    ImageInterpolation, OVERVIEW_SIZE_PX, PointMarker, Scene3dFog, Scene3dGeometry, Scene3dId,
    Scene3dImageLayer, Scene3dMeshVertex, Scene3dPoint, Scene3dResources, Scene3dShading,
    Scene3dTexturedMesh, Scene3dVertex, install_scene3d, paint_scene3d, paint_scene3d_with,
    set_scene3d, snapshot_scene3d, snapshot_scene3d_with,
};
pub use crate::render::save::{
    SaveError, SaveFormat, encode_eps, encode_pdf, encode_png, encode_ppm, encode_svg, encode_tiff,
    rgba_to_rgb, save_graph, save_graph_with_format,
};
pub use crate::render::scene3d_items::{
    Box3D, ColormapMesh3D, ComplexField3D, CutPlane, Cylinder3D, DEFAULT_CUT_PLANE_RESOLUTION,
    DEFAULT_ISOSURFACE_COLOR, DEFAULT_SCATTER2D_LINE_WIDTH, DEFAULT_SCATTER3D_SIZE, HeightMapData,
    HeightMapRGBA, Hexagon3D, ImageData3D, ImageRgba3D, Isosurface, Mesh3D, MeshColor,
    MeshDrawMode, ScalarField3D, Scatter2D, Scatter2DVisualization, Scatter3D, mean_plus_std,
};
pub use crate::widget::actions;
pub use crate::widget::actions::io::{SaveTarget, curve_to_csv};
pub use crate::widget::actions::mode::{mask_draw_mode, pan_mode, select_mode, zoom_mode};
pub use crate::widget::alpha_slider::{
    ActiveImageAlphaSlider, AlphaSlider, AlphaSliderOrientation, NamedItemAlphaSlider,
};
pub use crate::widget::colorbar::{ColorBarOrientation, ColorBarWidget};
pub use crate::widget::colormap_dialog::ColormapDialog;
pub use crate::widget::complex_image_view::{ComplexImageView, ComplexMode};
pub use crate::widget::curves_roi_widget::{CurveRoiRow, CurvesRoiWidget};
pub use crate::widget::fit_widget::{
    FitModelChoice, FitWidget, format_param_value_error, format_reduced_chisq,
};
pub use crate::widget::high_level::{
    CompareAlignment, CompareImages, CompareMode, CurveStats, CurveStyle, HistogramAlign,
    ImageGeometry, ImageHistogramAxis, ImageProfileHistogram, ImageStats, ImageView, ItemStats,
    LegendAction, LegendResponse, Plot1D, Plot2D, PlotDataError, PlotEvent, PlotItemKind,
    PlotWidget, PlotWindow, PlotWithToolbarResponse, ProfileMethod, ProfileMode, ScatterPick,
    ScatterView, ScatterVisualization, StackPerspective, StackProfile, StackProfileDimension,
    StackView, ToolbarResponse, ValueStats, aligned_profile_values, compare_pixel_at,
    default_dimension_label, dimension_axis_labels, histogram_edges, histogram_step_values,
    horizontal_profile_values, line_profile_band, line_profile_values, pick_histogram,
    rect_profile_values, scatter_pick_pixels, scatter_position_info, stack_aligned_profile,
    stack_frame, stack_frame_count, stack_line_profile, vertical_profile_values,
};
pub use crate::widget::histogram_colorbar::{
    Handle as HistogramColorBarHandle, HistogramColorBar, HistogramColorBarResponse,
};
pub use crate::widget::image_stack::{Frame, FrameLoader, Hdf5FrameLoader, ImageStack};
pub use crate::widget::interaction::{
    ArcControlPoint, ArcControlPoints, CursorShape, DrawEvent, DrawInput, DrawMode, DrawParams,
    DrawState, FillMode, MouseButton, PanDirection, PlotPointerEvent, PointPick, RoiDrawKind,
    RoiGrab, RoiKeyAction, SelectionStyle, arc_control_points, arc_from_three_points,
    arc_from_two_points, arc_three_point_drag, constrain_zoom_axes, cursor_for_edge,
    cursor_for_grab, ellipse_semi_axes, hatch_lines, image_index, nearest_point, roi_draw_mode,
    roi_from_draw, roi_grab_at, roi_key_action,
};
pub use crate::widget::items_selection_dialog::{
    ItemsSelectionDialog, SelectableItem, SelectionMode,
};
pub use crate::widget::limits_widget::LimitsWidget;
pub use crate::widget::mask_tools::{MaskTool, MaskToolsWidget, ThresholdMode};
pub use crate::widget::plot_widget::{DrawResponse, PlotInteractionMode, PlotResponse, PlotView};
pub use crate::widget::position_info::{
    PositionInfo, SNAP_THRESHOLD_DIST, Snap, SnapItem, SnapItemKind, SnappingMode, format_value,
    snap_to_nearest, snapping_candidates,
};
pub use crate::widget::print_preview::{
    CommentPosition, PrintPreview, compose_page, resize_item_rect,
};
pub use crate::widget::profile_window::ProfileWindow;
pub use crate::widget::radar_view::{
    DataRect, RadarMapping, RadarResponse, RadarView, clamp_viewport, point_in_rect,
};
pub use crate::widget::roi_manager::RoiManagerWidget;
pub use crate::widget::roi_stats::{
    CurveRoiCounts, RoiStats, curve_roi_counts, curve_roi_stats, image_roi_stats, roi_x_span,
};
pub use crate::widget::roi_stats_widget::{RoiStatsRow, RoiStatsWidget};
pub use crate::widget::scalar_field_properties::ScalarFieldProperties;
pub use crate::widget::scalar_field_view::{FieldPick, ScalarFieldView};
pub use crate::widget::scatter_mask::{ScatterMaskWidget, point_in_polygon};
pub use crate::widget::scene_position_info::ScenePositionInfo;
pub use crate::widget::scene_widget::{
    FogMode, PICK_LINE_TOLERANCE_PX, PICK_POINT_TOLERANCE_PX, ScenePick, ScenePickKind,
    SceneWidget, viewpoint_menu,
};
pub use crate::widget::scene_window::SceneWindow;
pub use crate::widget::stack_profile_window::StackProfileWindow;
pub use crate::widget::stats_widget::{
    StatsInput, StatsWidget, UpdateMode, format_significant, format_stat,
};
pub use crate::widget::sync::SyncAxes;
pub use crate::widget::tool_buttons::{
    ProfileToolButton, RulerToolButton, SymbolToolButton, SymbolToolChange,
};

// Plotting-library convention: re-export so downstreams use the same
// egui/egui-wgpu without version skew.
pub use egui;
pub use egui_wgpu;
