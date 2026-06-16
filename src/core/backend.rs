//! Backend-facing API mirroring silx `BackendBase`.
//!
//! This module defines the renderer boundary: high-level code passes item specs
//! and view state through [`Backend`], while concrete backends decide how to
//! store and draw them. The specs intentionally borrow user data so callers can
//! hand over slices; a backend that retains data clones them into its own item
//! state.

use std::path::Path;

use egui::{Color32, Pos2, Rect};

use crate::core::colormap::Colormap;
use crate::core::items::{Baseline, ErrorBars, LineStyle, Symbol};
use crate::core::marker::MarkerConstraint;
use crate::core::shape::ShapeKind;
use crate::core::transform::{Margins, YAxis};
use crate::render::gpu_image::{AggregationMode, InterpolationMode};
use crate::render::save::SaveFormat;

/// Backend item handle. Equivalent to silx's opaque backend item object.
pub type ItemHandle = u64;

/// Curve color accepted by [`CurveSpec`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurveColor<'a> {
    /// One color for the whole curve.
    Uniform(Color32),
    /// Per-vertex colors, one entry per `x`/`y` point.
    PerVertex(&'a [Color32]),
}

/// Curve item spec mirroring `BackendBase.addCurve`.
#[derive(Clone, Debug)]
pub struct CurveSpec<'a> {
    pub x: &'a [f64],
    pub y: &'a [f64],
    pub color: CurveColor<'a>,
    pub gap_color: Option<Color32>,
    pub symbol: Option<Symbol>,
    pub line_width: f32,
    pub line_style: LineStyle,
    pub y_axis: YAxis,
    pub x_error: Option<ErrorBars>,
    pub y_error: Option<ErrorBars>,
    pub fill: bool,
    pub alpha: f32,
    pub symbol_size: f32,
    pub baseline: Baseline,
    /// X-axis label shown while this curve is the active curve (silx
    /// `addCurve(xlabel=...)` / `Curve.setXLabel`). `None` keeps the graph's
    /// default X label when this curve is active.
    pub x_label: Option<&'a str>,
    /// Y-axis label shown while this curve is the active curve (silx
    /// `addCurve(ylabel=...)` / `Curve.setYLabel`). Routed to the left or right
    /// (y2) axis by [`Self::y_axis`]. `None` keeps the graph's default Y label.
    pub y_label: Option<&'a str>,
}

impl<'a> CurveSpec<'a> {
    /// Build a solid left-axis curve with a uniform color.
    pub fn new(x: &'a [f64], y: &'a [f64], color: Color32) -> Self {
        Self {
            x,
            y,
            color: CurveColor::Uniform(color),
            gap_color: None,
            symbol: None,
            line_width: 1.0,
            line_style: LineStyle::Solid,
            y_axis: YAxis::Left,
            x_error: None,
            y_error: None,
            fill: false,
            alpha: 1.0,
            symbol_size: 7.0,
            baseline: Baseline::Scalar(0.0),
            x_label: None,
            y_label: None,
        }
    }
}

/// Pixel payload accepted by [`ImageSpec`].
#[derive(Clone, Debug)]
pub enum ImagePixelsSpec<'a> {
    /// Row-major scalar field, length `width * height`, mapped through `colormap`.
    Scalar {
        width: u32,
        height: u32,
        data: &'a [f32],
        colormap: Box<Colormap>,
    },
    /// Row-major direct RGBA pixels, length `width * height`.
    Rgba {
        width: u32,
        height: u32,
        data: &'a [[u8; 4]],
    },
}

/// Image item spec mirroring `BackendBase.addImage`.
#[derive(Clone, Debug)]
pub struct ImageSpec<'a> {
    pub pixels: ImagePixelsSpec<'a>,
    pub origin: (f64, f64),
    pub scale: (f64, f64),
    pub alpha: f32,
    /// Data-to-screen interpolation (silx image `interpolation`, default
    /// [`Nearest`](InterpolationMode::Nearest)).
    pub interpolation: InterpolationMode,
    /// Block aggregation applied to a scalar field before upload (silx
    /// `ImageDataAggregated`, default [`None`](AggregationMode::None)). Ignored
    /// for an RGBA image.
    pub aggregation: AggregationMode,
    /// Per-axis block factors `(block_x, block_y)` for [`aggregation`], mirroring
    /// silx's level-of-detail `(lodx, lody)`. Each must be `>= 1`; `(1, 1)` is a
    /// no-op even with an aggregation mode set.
    ///
    /// [`aggregation`]: ImageSpec::aggregation
    pub aggregation_block: (u32, u32),
    /// Optional per-pixel alpha map (silx `ImageData.setAlphaData`), row-major
    /// and length `width * height`, each in `[0, 1]`, multiplied into the global
    /// [`alpha`](Self::alpha) per pixel. `None` (the default) is a uniform global
    /// alpha. Only meaningful for a scalar image; ignored for RGBA.
    pub alpha_map: Option<&'a [f32]>,
}

impl<'a> ImageSpec<'a> {
    /// Build a scalar colormapped image at origin `(0, 0)` and unit scale.
    pub fn scalar(width: u32, height: u32, data: &'a [f32], colormap: Colormap) -> Self {
        Self {
            pixels: ImagePixelsSpec::Scalar {
                width,
                height,
                data,
                colormap: Box::new(colormap),
            },
            origin: (0.0, 0.0),
            scale: (1.0, 1.0),
            alpha: 1.0,
            interpolation: InterpolationMode::default(),
            aggregation: AggregationMode::default(),
            aggregation_block: (1, 1),
            alpha_map: None,
        }
    }

    /// Build a direct RGBA image at origin `(0, 0)` and unit scale.
    pub fn rgba(width: u32, height: u32, data: &'a [[u8; 4]]) -> Self {
        Self {
            pixels: ImagePixelsSpec::Rgba {
                width,
                height,
                data,
            },
            origin: (0.0, 0.0),
            scale: (1.0, 1.0),
            alpha: 1.0,
            interpolation: InterpolationMode::default(),
            aggregation: AggregationMode::default(),
            aggregation_block: (1, 1),
            alpha_map: None,
        }
    }

    /// Set the data-to-screen interpolation (silx image `interpolation`).
    pub fn with_interpolation(mut self, interpolation: InterpolationMode) -> Self {
        self.interpolation = interpolation;
        self
    }

    /// Set the block aggregation and per-axis block factors `(block_x, block_y)`
    /// applied to a scalar field before upload (silx `ImageDataAggregated`).
    pub fn with_aggregation(mut self, mode: AggregationMode, block: (u32, u32)) -> Self {
        self.aggregation = mode;
        self.aggregation_block = block;
        self
    }

    /// Set the per-pixel alpha map (silx `ImageData.setAlphaData`); see
    /// [`alpha_map`](Self::alpha_map). The slice must outlive the spec and have
    /// length `width * height` (validated downstream at upload). No effect on RGBA.
    pub fn with_alpha_map(mut self, alpha_map: &'a [f32]) -> Self {
        self.alpha_map = Some(alpha_map);
        self
    }
}

/// Triangle mesh spec mirroring `BackendBase.addTriangles`.
#[derive(Clone, Debug)]
pub struct TriangleSpec<'a> {
    pub x: &'a [f64],
    pub y: &'a [f64],
    pub triangles: &'a [[u32; 3]],
    pub colors: &'a [Color32],
    pub alpha: f32,
}

/// Shape spec mirroring `BackendBase.addShape`.
#[derive(Clone, Debug)]
pub struct ShapeSpec<'a> {
    pub x: &'a [f64],
    pub y: &'a [f64],
    pub kind: ShapeKind,
    pub color: Color32,
    pub fill: bool,
    /// Preserved from silx. siplot currently draws shapes in one overlay pass.
    pub overlay: bool,
    pub line_style: LineStyle,
    pub line_width: f32,
    pub gap_color: Option<Color32>,
}

/// Marker spec mirroring `BackendBase.addMarker`, excluding Qt-only font
/// details.
#[derive(Clone, Debug)]
pub struct MarkerSpec<'a> {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub text: Option<&'a str>,
    pub color: Color32,
    pub symbol: Option<Symbol>,
    pub symbol_size: f32,
    pub line_style: LineStyle,
    pub line_width: f32,
    pub y_axis: YAxis,
    pub bg_color: Option<Color32>,
    /// Whether the user may drag the marker (silx `addMarker` `draggable`).
    pub is_draggable: bool,
    /// Drag-time position filter (silx `addMarker` `constraint`, the
    /// `'horizontal'` / `'vertical'` presets).
    pub constraint: MarkerConstraint,
}

/// Result of backend item picking.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PickResult {
    /// A curve vertex was picked.
    CurvePoint {
        index: usize,
        x: f64,
        y: f64,
        distance_px: f32,
    },
    /// An image pixel was picked: `(column, row)`.
    ImagePixel { col: u32, row: u32 },
    /// A non-indexed overlay item was picked.
    Item { handle: ItemHandle },
}

/// Renderer boundary modeled after silx `BackendBase`.
pub trait Backend {
    type SaveError;

    fn add_curve(&mut self, curve: CurveSpec<'_>) -> ItemHandle;
    fn add_image(&mut self, image: ImageSpec<'_>) -> ItemHandle;
    fn add_triangles(&mut self, tris: TriangleSpec<'_>) -> ItemHandle;
    fn add_shape(&mut self, shape: ShapeSpec<'_>) -> ItemHandle;
    fn add_marker(&mut self, marker: MarkerSpec<'_>) -> ItemHandle;
    fn remove(&mut self, item: ItemHandle) -> bool;

    fn set_limits(&mut self, xmin: f64, xmax: f64, ymin: f64, ymax: f64, y2: Option<(f64, f64)>);
    fn x_limits(&self) -> (f64, f64);
    fn y_limits(&self, axis: YAxis) -> Option<(f64, f64)>;
    fn set_x_log(&mut self, on: bool);
    fn set_y_log(&mut self, on: bool);
    fn set_x_inverted(&mut self, on: bool);
    fn set_y_inverted(&mut self, on: bool);
    fn set_keep_data_aspect_ratio(&mut self, on: bool);

    fn data_to_pixel(&self, x: f64, y: f64, axis: YAxis) -> Option<Pos2>;
    fn pixel_to_data(&self, p: Pos2, axis: YAxis) -> Option<(f64, f64)>;
    fn plot_bounds_in_pixels(&self) -> Option<Rect>;
    fn set_axes_margins(&mut self, margins: Margins);

    fn set_title(&mut self, title: Option<&str>);
    fn set_x_label(&mut self, label: Option<&str>);
    fn set_y_label(&mut self, label: Option<&str>, axis: YAxis);
    fn set_foreground_colors(&mut self, foreground: Color32, grid: Color32);
    fn set_background_colors(&mut self, background: Color32, data_background: Color32);

    fn pick_item(&self, p: Pos2, item: ItemHandle) -> Option<PickResult>;
    fn items_back_to_front(&self) -> Vec<ItemHandle>;

    fn replot(&mut self);
    fn save_graph(&self, path: &Path, size: (u32, u32)) -> Result<(), Self::SaveError>;
    /// Render the figure to `path` in the given [`SaveFormat`] at `dpi`.
    ///
    /// Generalizes [`Self::save_graph`] (PNG-only) over silx's raster save
    /// formats (PNG/PPM/SVG/TIFF) plus the raster-embedding EPS/PDF, faithful to
    /// silx `BackendBase.saveGraph(fileName, fileFormat, dpi)`.
    fn save_graph_with_format(
        &self,
        path: &Path,
        size: (u32, u32),
        format: SaveFormat,
        dpi: u32,
    ) -> Result<(), Self::SaveError>;
}
