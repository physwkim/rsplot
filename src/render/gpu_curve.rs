//! GPU-side curve: the shared curve pipeline and uploaded polylines.
//!
//! [`CurveData`] is the CPU spec (mirrors silx `addCurve`: x/y arrays + color +
//! width + Y-axis binding). [`CurvePipeline`] holds the thick-line pipeline
//! shared across curves. [`GpuCurve`] owns one curve's point storage buffer +
//! uniform + bind group and persists across frames in `WgpuResources`.
//!
//! Each segment of the polyline is expanded in the vertex shader into a
//! screen-space quad (two triangles) of the curve's pixel width, so the line is
//! a uniform thickness regardless of the data aspect ratio. The points are read
//! from a read-only storage buffer; the draw is `6 × segment count` vertices,
//! no vertex buffer. In-place re-upload ([`GpuCurve::update`]) reuses the buffer
//! for live updates. Optional markers and per-vertex color are supported; round
//! joins/caps and anti-aliasing are later steps (`doc/design.md` §7·§13 B1).

use std::num::NonZeroU64;

use egui::Color32;
use egui_wgpu::wgpu;

use crate::core::decimate::min_max_decimate;
use crate::core::transform::YAxis;

/// Identity ortho matrix; replaced every frame by the widget's transform.
const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// `log10(e) = 1 / ln(10)`, to turn a natural log into log10 — equals the
/// literal `INV_LN10` in `curve.wgsl`, so CPU-side pixel projection (for dash
/// arc length) matches the shader's `apply_scale` exactly.
const INV_LN10: f32 = std::f32::consts::LOG10_E;

/// Uniform block for the curve shader. Field order is laid out so the natural
/// `repr(C)` offsets coincide with WGSL std140 alignment (all `vec4`s first at
/// 16-aligned offsets, then `vec2`s at 8-aligned, then scalars): mat4 @0, vec4
/// @64, vec4 @80, vec4 @96, vec2 @112, vec2 @120, f32 @128/132/136/140; total
/// 144. Matches `Params` in `curve.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CurveParams {
    ortho: [[f32; 4]; 4],
    color: [f32; 4],
    /// Fill color for dashed-line gaps (linear premultiplied); used only when
    /// `use_gap_color` is set.
    gap_color: [f32; 4],
    /// Dash pattern as cumulative boundaries in physical pixels: a fragment at
    /// phase `p` is "on" when `p < .x` or `.y <= p < .z`; `.w` is the period.
    /// All zero means a solid line (no dashing).
    dash_cum: [f32; 4],
    /// 1.0 if that axis is log10, else 0.0 (x, y).
    axis_log: [f32; 2],
    /// Data-area size in physical pixels (for the pixel-space quad expansion).
    viewport_px: [f32; 2],
    /// Half the line width, in physical pixels.
    half_width_px: f32,
    /// 1.0 to take each vertex's color from the per-vertex color buffer, else
    /// 0.0 to use the uniform `color`.
    use_vertex_color: f32,
    /// Phase offset (physical pixels) added to the arc length before the dash
    /// test.
    dash_offset: f32,
    /// 1.0 to fill dash gaps with `gap_color`, else 0.0 to discard them.
    use_gap_color: f32,
}

/// Marker symbol drawn at each curve vertex (silx `symbol`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Symbol {
    Circle,
    Square,
    /// Diagonal "×".
    Cross,
    /// Upright "+".
    Plus,
    /// Upward-pointing triangle.
    Triangle,
}

impl Symbol {
    /// Shader symbol code (must match the `switch` in `markers.wgsl`).
    fn code(self) -> u32 {
        match self {
            Symbol::Circle => 0,
            Symbol::Square => 1,
            Symbol::Cross => 2,
            Symbol::Plus => 3,
            Symbol::Triangle => 4,
        }
    }
}

/// Line stroke style (silx `linestyle`). Dash lengths for the predefined styles
/// are in physical pixels and scale with the line width (`max(width, 1)`), so
/// they stay proportionate at any thickness; a [`LineStyle::Custom`] pattern is
/// taken verbatim in physical pixels.
#[derive(Clone, Debug, PartialEq)]
pub enum LineStyle {
    /// No line drawn (markers only, if any). silx `' '` / `''`.
    None,
    /// Continuous line. silx `'-'`.
    Solid,
    /// Dashed line. silx `'--'`.
    Dashed,
    /// Dash-dot line. silx `'-.'`.
    DashDot,
    /// Dotted line. silx `':'`.
    Dotted,
    /// Custom dash pattern in physical pixels: alternating on/off lengths
    /// (`on, off, on, off`), with `offset` the starting phase. silx
    /// `(offset, (dash pattern))`. Up to four entries are honored.
    Custom { offset: f32, pattern: Vec<f32> },
}

impl LineStyle {
    /// Whether this style draws a line at all (false only for [`LineStyle::None`]).
    fn draws_line(&self) -> bool {
        !matches!(self, LineStyle::None)
    }

    /// The dash pattern for the given line width as `(cumulative boundaries,
    /// phase offset)`, or `None` for a solid (un-dashed) line. The boundaries
    /// encode up to two on/off pairs: a fragment at phase `p` is "on" when
    /// `p < cum[0]` or `cum[1] <= p < cum[2]`; `cum[3]` is the period.
    fn dash_spec(&self, width: f32) -> Option<([f32; 4], f32)> {
        // Predefined patterns scale with the line width so they look right at
        // any thickness; a degenerate width still gives a 1px unit.
        let u = width.max(1.0);
        let from_intervals = |intervals: [f32; 4], offset: f32| -> Option<([f32; 4], f32)> {
            let cum = [
                intervals[0],
                intervals[0] + intervals[1],
                intervals[0] + intervals[1] + intervals[2],
                intervals[0] + intervals[1] + intervals[2] + intervals[3],
            ];
            // A zero (or negative) period would make the modulo ill-defined;
            // treat it as solid.
            if cum[3] > 0.0 {
                Some((cum, offset))
            } else {
                None
            }
        };
        match self {
            LineStyle::None | LineStyle::Solid => None,
            // on, off
            LineStyle::Dashed => from_intervals([5.0 * u, 4.0 * u, 0.0, 0.0], 0.0),
            // dot, gap, dot, gap (single dot per period via the empty 2nd pair)
            LineStyle::Dotted => from_intervals([1.5 * u, 2.5 * u, 0.0, 0.0], 0.0),
            // dash, gap, dot, gap
            LineStyle::DashDot => from_intervals([6.0 * u, 3.0 * u, 1.5 * u, 3.0 * u], 0.0),
            LineStyle::Custom { offset, pattern } => {
                let g = |i: usize| pattern.get(i).copied().unwrap_or(0.0);
                from_intervals([g(0), g(1), g(2), g(3)], *offset)
            }
        }
    }
}

/// Uniform block for the marker shader. Layout matches `Params` in
/// `markers.wgsl` (std140: mat4 @0, vec4 @64, vec2 @80, vec2 @88, f32 @96,
/// u32 @100; padded to 112).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MarkerParams {
    ortho: [[f32; 4]; 4],
    color: [f32; 4],
    axis_log: [f32; 2],
    viewport_px: [f32; 2],
    /// Half the marker size, in physical pixels.
    half_size_px: f32,
    /// Symbol code; see [`Symbol::code`].
    symbol: u32,
    _pad: [f32; 2],
}

/// A polyline to draw, in data coordinates. `x[i], y[i]` is vertex `i`; the
/// vertices are connected in order.
#[derive(Clone, Debug)]
pub struct CurveData {
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    pub color: Color32,
    /// Per-vertex line color (silx per-point color), one entry per `x`/`y`
    /// vertex. `None` draws the whole line in the single [`Self::color`]; when
    /// set, the segment between two vertices is a gradient between their colors.
    /// Mutually exclusive with decimation (the envelope would unalign colors),
    /// so a per-vertex-colored curve always draws at full resolution.
    pub colors: Option<Vec<Color32>>,
    /// Line stroke style (silx `linestyle`). [`LineStyle::Solid`] by default.
    /// Any dashed style also disables decimation so the dash phase stays
    /// continuous over the full-resolution geometry.
    pub line_style: LineStyle,
    /// Fill color for dashed-line gaps (silx `gapcolor`). `None` leaves the
    /// gaps transparent; only meaningful with a dashed [`Self::line_style`].
    pub gap_color: Option<Color32>,
    /// Line width in physical pixels (`doc/design.md` §12·§13 B1).
    pub width: f32,
    /// Marker symbol drawn at each vertex, or `None` for a line only.
    pub symbol: Option<Symbol>,
    /// Marker size (full extent) in physical pixels.
    pub marker_size: f32,
    /// Which Y axis this curve is plotted against (left by default).
    pub y_axis: YAxis,
}

impl CurveData {
    /// Build a curve from equal-length x/y arrays with the given line color, a
    /// 1px width, no markers, plotted against the main left Y axis.
    pub fn new(x: Vec<f64>, y: Vec<f64>, color: Color32) -> Self {
        assert_eq!(x.len(), y.len(), "x and y must have equal length");
        Self {
            x,
            y,
            color,
            colors: None,
            line_style: LineStyle::Solid,
            gap_color: None,
            width: 1.0,
            symbol: None,
            marker_size: 7.0,
            y_axis: YAxis::Left,
        }
    }

    /// Color each vertex individually; each segment is a gradient between its
    /// two endpoint colors (silx per-point color). `colors` must have one entry
    /// per vertex (same length as `x`/`y`). Setting this disables decimation
    /// for the curve so colors stay aligned with their vertices.
    pub fn with_colors(mut self, colors: Vec<Color32>) -> Self {
        assert_eq!(
            colors.len(),
            self.x.len(),
            "colors must have one entry per vertex"
        );
        self.colors = Some(colors);
        self
    }

    /// Set the line stroke style (silx `linestyle`).
    pub fn with_line_style(mut self, style: LineStyle) -> Self {
        self.line_style = style;
        self
    }

    /// Fill dashed-line gaps with `color` instead of leaving them transparent
    /// (silx `gapcolor`). Only visible with a dashed [`Self::line_style`].
    pub fn with_gap_color(mut self, color: Color32) -> Self {
        self.gap_color = Some(color);
        self
    }

    /// Set the line width in physical pixels (clamped to ≥ 0).
    pub fn with_width(mut self, width: f32) -> Self {
        self.width = width.max(0.0);
        self
    }

    /// Draw `symbol` markers at each vertex (size via [`Self::with_marker_size`]).
    pub fn with_symbol(mut self, symbol: Symbol) -> Self {
        self.symbol = Some(symbol);
        self
    }

    /// Set the marker size (full extent) in physical pixels (clamped to ≥ 0).
    pub fn with_marker_size(mut self, size: f32) -> Self {
        self.marker_size = size.max(0.0);
        self
    }

    /// Bind this curve to the given Y axis (left or right/y2).
    pub fn with_y_axis(mut self, y_axis: YAxis) -> Self {
        self.y_axis = y_axis;
        self
    }
}

/// The render pipelines shared by all curves: the thick-line pipeline and the
/// marker pipeline. The line layout has the curve uniform at binding 0, the
/// points storage buffer at binding 1, and a per-vertex color storage buffer at
/// binding 2. The marker pipeline has its own minimal layout (marker uniform at
/// binding 0, the shared points at binding 1), so the line uniform/layout can
/// grow without affecting markers.
pub struct CurvePipeline {
    pub(crate) pipeline: wgpu::RenderPipeline,
    pub(crate) marker_pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    marker_bind_group_layout: wgpu::BindGroupLayout,
}

impl CurvePipeline {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("egui-silx curve"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/curve.wgsl").into()),
        });
        let marker_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("egui-silx markers"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/markers.wgsl").into()),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("egui-silx curve bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: NonZeroU64::new(
                                std::mem::size_of::<CurveParams>() as u64
                            ),
                        },
                        count: None,
                    },
                    // The polyline points, read in the vertex shader for quad expansion.
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: NonZeroU64::new(
                                std::mem::size_of::<[f32; 2]>() as u64
                            ),
                        },
                        count: None,
                    },
                    // Per-vertex line colors (linear premultiplied RGBA), read in
                    // the vertex shader; a 1-element placeholder when unused.
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: NonZeroU64::new(
                                std::mem::size_of::<[f32; 4]>() as u64
                            ),
                        },
                        count: None,
                    },
                    // Per-vertex cumulative pixel arc length for dashing, read in
                    // the vertex shader; a 1-element placeholder when not dashed.
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: NonZeroU64::new(std::mem::size_of::<f32>() as u64),
                        },
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("egui-silx curve layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        // Markers have their own minimal layout (uniform at 0 + points at 1) so
        // the curve uniform/layout can grow per-vertex color, dashes, etc.
        // without forcing the marker uniform to match the curve uniform's size
        // or carry dead curve-only bindings.
        let marker_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("egui-silx marker bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: NonZeroU64::new(
                                std::mem::size_of::<MarkerParams>() as u64
                            ),
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: NonZeroU64::new(
                                std::mem::size_of::<[f32; 2]>() as u64
                            ),
                        },
                        count: None,
                    },
                ],
            });
        let marker_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("egui-silx marker layout"),
                bind_group_layouts: &[Some(&marker_bind_group_layout)],
                immediate_size: 0,
            });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("egui-silx curve pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                // No vertex buffers: positions come from the storage buffer and
                // each vertex is derived from its index.
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            // Triangle list: two triangles (6 vertices) per polyline segment.
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // Marker pipeline: its own minimal layout, one quad (6 vertices) per point.
        let marker_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("egui-silx marker pipeline"),
            layout: Some(&marker_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &marker_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &marker_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            marker_pipeline,
            bind_group_layout,
            marker_bind_group_layout,
        }
    }
}

/// One uploaded curve's GPU resources, persisting across frames.
pub struct GpuCurve {
    points: wgpu::Buffer,
    /// Points currently in the buffer prefix `[0, count)` and drawn this frame
    /// (the full set, or a decimated envelope when [`Self::ensure_decimated`]
    /// has reduced it).
    count: u32,
    /// Points the buffer can hold; an in-place [`Self::update`] up to this many
    /// points reuses the buffer instead of reallocating. Sized to the full
    /// source, so any decimated envelope (≤ source length) always fits.
    capacity: u32,
    params: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// Per-vertex color storage buffer (binding 2). Holds `count` colors when
    /// [`Self::vertex_color`] is set; otherwise a 1-element placeholder.
    vcolors: wgpu::Buffer,
    /// Colors the `vcolors` buffer can hold; `0` when the curve was created with
    /// no per-vertex colors (the placeholder), forcing a realloc on first use.
    colors_capacity: u32,
    /// Whether to draw with per-vertex color (the `use_vertex_color` flag).
    vertex_color: bool,
    /// Per-vertex cumulative pixel arc length (binding 3), recomputed on a view
    /// change by [`Self::ensure_arclen`] for dashed curves; a 1-element
    /// placeholder otherwise.
    arclen: wgpu::Buffer,
    /// Arc-length entries the `arclen` buffer can hold; `0` for the placeholder
    /// (non-dashed at creation), forcing a realloc when the curve becomes dashed.
    arclen_capacity: u32,
    /// The `(ortho, axis_log, viewport_px)` the arc length was last computed
    /// for, or `None` when never computed; lets [`Self::ensure_arclen`] skip
    /// recomputation when the view is unchanged.
    arclen_key: Option<ArclenKey>,
    /// Marker uniform + bind group (shares the points buffer at binding 1).
    marker_params: wgpu::Buffer,
    marker_bind_group: wgpu::BindGroup,
    color: [f32; 4],
    /// Line stroke style (selects the dash pattern; [`LineStyle::None`] skips
    /// the line entirely).
    line_style: LineStyle,
    /// Dashed-gap fill color (linear premultiplied), or `None` to leave gaps
    /// transparent.
    gap_color: Option<[f32; 4]>,
    /// Line width in physical pixels.
    width: f32,
    /// Marker symbol, or `None` for a line only.
    symbol: Option<Symbol>,
    /// Marker size (full extent) in physical pixels.
    marker_size: f32,
    /// Which Y axis this curve is bound to; selects the per-frame ortho matrix.
    pub(crate) y_axis: YAxis,
    /// Full-resolution source kept on the CPU so the curve can be re-decimated
    /// for the current view without a fresh upload from the caller.
    src_x: Vec<f64>,
    src_y: Vec<f64>,
    /// Source vertex count (the un-decimated length).
    full_count: u32,
    /// Whether `src_x` is monotonically non-decreasing. Decimation reorders by
    /// x within each column, so it is only valid (lossless-looking) for sorted
    /// x; otherwise the curve is always drawn at full resolution.
    monotonic_x: bool,
    /// The `(x_min bits, x_max bits, columns)` the buffer was last decimated
    /// for, or `None` when the buffer currently holds the full source. Lets
    /// [`Self::ensure_decimated`] skip work when the view is unchanged.
    decimate_key: Option<(u64, u64, u32)>,
}

/// The `(ortho, axis_log, viewport_px)` a curve's dash arc length was last
/// computed for; an unchanged value means the arc length can be reused.
type ArclenKey = ([[f32; 4]; 4], [f32; 2], [f32; 2]);

/// Whether `xs` is monotonically non-decreasing.
fn is_monotonic(xs: &[f64]) -> bool {
    xs.windows(2).all(|w| w[0] <= w[1])
}

/// Pack equal-length f64 x/y into the `[f32; 2]` positions the shader reads.
fn pack(x: &[f64], y: &[f64]) -> Vec<[f32; 2]> {
    x.iter()
        .zip(y)
        .map(|(&x, &y)| [x as f32, y as f32])
        .collect()
}

/// Pack sRGB colors into the linear premultiplied RGBA the shader blends with
/// (matches the single-color conversion `egui::Rgba::from`).
fn pack_colors(colors: &[Color32]) -> Vec<[f32; 4]> {
    colors
        .iter()
        .map(|&c| egui::Rgba::from(c).to_array())
        .collect()
}

/// Project one data point to the shader's pixel space — NDC (`ortho * point`,
/// perspective divide) scaled by half the viewport. Mirrors `to_ndc` in
/// `curve.wgsl` (column-major matrix, log10 via `ln * INV_LN10`) so CPU dash arc
/// length matches the geometry the GPU rasterizes. The origin is the data-area
/// center, but only distances between points are used, so the offset cancels.
fn project_px(
    x: f64,
    y: f64,
    ortho: &[[f32; 4]; 4],
    axis_log: [f32; 2],
    half_vp: [f32; 2],
) -> [f32; 2] {
    let sx = if axis_log[0] > 0.5 {
        (x as f32).ln() * INV_LN10
    } else {
        x as f32
    };
    let sy = if axis_log[1] > 0.5 {
        (y as f32).ln() * INV_LN10
    } else {
        y as f32
    };
    let cx = ortho[0][0] * sx + ortho[1][0] * sy + ortho[3][0];
    let cy = ortho[0][1] * sx + ortho[1][1] * sy + ortho[3][1];
    let cw = ortho[0][3] * sx + ortho[1][3] * sy + ortho[3][3];
    [cx / cw * half_vp[0], cy / cw * half_vp[1]]
}

/// Cumulative pixel arc length along the polyline: entry `i` is the summed
/// pixel distance from vertex 0 to vertex `i` (so entry 0 is 0). Used as the
/// dash parameter so dashes have a uniform on-screen period regardless of how
/// the data is sampled or zoomed.
fn cumulative_arclen(
    x: &[f64],
    y: &[f64],
    ortho: &[[f32; 4]; 4],
    axis_log: [f32; 2],
    viewport_px: [f32; 2],
) -> Vec<f32> {
    let half_vp = [viewport_px[0] * 0.5, viewport_px[1] * 0.5];
    let mut out = Vec::with_capacity(x.len());
    let mut acc = 0.0f32;
    let mut prev: Option<[f32; 2]> = None;
    for (&px, &py) in x.iter().zip(y) {
        let p = project_px(px, py, ortho, axis_log, half_vp);
        if let Some(q) = prev {
            let (dx, dy) = (p[0] - q[0], p[1] - q[1]);
            acc += (dx * dx + dy * dy).sqrt();
        }
        out.push(acc);
        prev = Some(p);
    }
    out
}

impl GpuCurve {
    /// Upload `curve`'s vertices and build its uniform + bind group.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &CurvePipeline,
        curve: &CurveData,
    ) -> Self {
        let positions = pack(&curve.x, &curve.y);

        // max(1) keeps a zero-point curve from creating a zero-size buffer (also
        // satisfies the storage binding's nonzero min size); the draw is still
        // skipped (count < 2) so nothing is rendered.
        let capacity = positions.len().max(1) as u32;
        let points = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("egui-silx curve points"),
            size: (capacity as usize * std::mem::size_of::<[f32; 2]>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if !positions.is_empty() {
            queue.write_buffer(&points, 0, bytemuck::cast_slice(&positions));
        }

        // Per-vertex color buffer. Sized to `capacity` (so in-place updates fit)
        // when the curve carries colors, else a 1-element placeholder that the
        // shader never samples (`use_vertex_color` stays 0).
        let vertex_color = curve.colors.is_some();
        let colors_capacity = if vertex_color { capacity } else { 0 };
        let vcolors = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("egui-silx curve colors"),
            size: (colors_capacity.max(1) as usize * std::mem::size_of::<[f32; 4]>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if let Some(colors) = &curve.colors {
            let packed = pack_colors(colors);
            if !packed.is_empty() {
                queue.write_buffer(&vcolors, 0, bytemuck::cast_slice(&packed));
            }
        }

        // Arc-length buffer for dashing. Sized to `capacity` (dashed curves
        // never decimate, so `count == capacity`) when the curve is dashed at
        // creation, else a 1-element placeholder. Filled per-frame by
        // `ensure_arclen` once the transform is known.
        let dashed = curve.line_style.dash_spec(curve.width).is_some();
        let arclen_capacity = if dashed { capacity } else { 0 };
        let arclen = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("egui-silx curve arclen"),
            size: (arclen_capacity.max(1) as usize * std::mem::size_of::<f32>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("egui-silx curve params"),
            size: std::mem::size_of::<CurveParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("egui-silx curve bg"),
            layout: &pipeline.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: points.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: vcolors.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: arclen.as_entire_binding(),
                },
            ],
        });

        // Marker uniform + bind group: same layout, sharing the points buffer.
        let marker_params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("egui-silx marker params"),
            size: std::mem::size_of::<MarkerParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let marker_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("egui-silx marker bg"),
            layout: &pipeline.marker_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: marker_params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: points.as_entire_binding(),
                },
            ],
        });

        // sRGB Color32 -> linear, premultiplied RGBA (matches the alpha-blend target).
        let color = egui::Rgba::from(curve.color).to_array();

        let gpu = Self {
            points,
            count: positions.len() as u32,
            capacity,
            params,
            bind_group,
            vcolors,
            colors_capacity,
            vertex_color,
            arclen,
            arclen_capacity,
            arclen_key: None,
            marker_params,
            marker_bind_group,
            color,
            line_style: curve.line_style.clone(),
            gap_color: curve.gap_color.map(|c| egui::Rgba::from(c).to_array()),
            width: curve.width,
            symbol: curve.symbol,
            marker_size: curve.marker_size,
            y_axis: curve.y_axis,
            monotonic_x: is_monotonic(&curve.x),
            full_count: positions.len() as u32,
            src_x: curve.x.clone(),
            src_y: curve.y.clone(),
            decimate_key: None,
        };
        // Seed the uniforms; the per-frame transform/viewport overwrite them.
        gpu.write_uniforms(queue, IDENTITY, [0.0, 0.0], [1.0, 1.0]);
        gpu
    }

    /// Re-upload `curve`'s vertices into the existing buffer in place (dirty
    /// update), reusing all GPU resources. Returns `false` if the new vertex
    /// count exceeds the allocated [`Self::capacity`], in which case the caller
    /// must reallocate (build a fresh [`GpuCurve`]).
    pub(crate) fn update(&mut self, queue: &wgpu::Queue, curve: &CurveData) -> bool {
        assert_eq!(
            curve.x.len(),
            curve.y.len(),
            "x and y must have equal length"
        );
        if curve.x.len() as u32 > self.capacity {
            return false;
        }
        // Per-vertex colors must fit the existing color buffer; if the curve
        // gained colors (or grew past the buffer), force a fresh allocation.
        if let Some(colors) = &curve.colors {
            assert_eq!(
                colors.len(),
                curve.x.len(),
                "colors must have one entry per vertex"
            );
            if colors.len() as u32 > self.colors_capacity {
                return false;
            }
        }
        // A dashed curve needs an arc-length entry per vertex; if it became
        // dashed (or grew past the buffer), force a fresh allocation.
        if curve.line_style.dash_spec(curve.width).is_some()
            && curve.x.len() as u32 > self.arclen_capacity
        {
            return false;
        }
        let positions = pack(&curve.x, &curve.y);
        if !positions.is_empty() {
            queue.write_buffer(&self.points, 0, bytemuck::cast_slice(&positions));
        }
        self.count = positions.len() as u32;
        self.vertex_color = curve.colors.is_some();
        if let Some(colors) = &curve.colors {
            let packed = pack_colors(colors);
            if !packed.is_empty() {
                queue.write_buffer(&self.vcolors, 0, bytemuck::cast_slice(&packed));
            }
        }
        self.color = egui::Rgba::from(curve.color).to_array();
        self.line_style = curve.line_style.clone();
        self.gap_color = curve.gap_color.map(|c| egui::Rgba::from(c).to_array());
        self.width = curve.width;
        self.symbol = curve.symbol;
        self.marker_size = curve.marker_size;
        self.y_axis = curve.y_axis;
        // The buffer now holds the new full source; force a re-decimation and a
        // re-computation of the dash arc length for the current view next frame.
        self.src_x = curve.x.clone();
        self.src_y = curve.y.clone();
        self.full_count = positions.len() as u32;
        self.monotonic_x = is_monotonic(&curve.x);
        self.decimate_key = None;
        self.arclen_key = None;
        true
    }

    /// Ensure the buffer holds the right vertices for the current view: a
    /// per-pixel-column min/max envelope when the curve has many more points
    /// than `columns`, or the full source otherwise.
    ///
    /// `x_min`/`x_max` is the visible data-x window and `columns` is the data
    /// area width in pixels (or `0` to disable, e.g. on a log x-axis where
    /// equal data-x bins are not equal pixel columns). Decimation is skipped
    /// for markered curves (every vertex must keep its marker) and for
    /// non-monotonic x. The result is cached by `(x_min, x_max, columns)`, so a
    /// steady view does no work after the first frame (`doc/design.md` §13 D1).
    pub(crate) fn ensure_decimated(
        &mut self,
        queue: &wgpu::Queue,
        x_min: f64,
        x_max: f64,
        columns: u32,
    ) {
        // Decimate only when it strictly reduces the count and the envelope is
        // valid; `2 * columns + 2` is the most points a decimation can emit, so
        // requiring more sources than that guarantees a reduction that fits.
        // Per-vertex color and dashing are excluded: the min/max envelope drops
        // and reorders vertices, which would unalign per-vertex colors and break
        // the cross-segment dash phase.
        let beneficial = self.symbol.is_none()
            && !self.vertex_color
            && self.line_style.dash_spec(self.width).is_none()
            && self.monotonic_x
            && columns > 0
            && self.full_count as u64 > 2 * columns as u64 + 2;

        if !beneficial {
            // Restore the full source if a previous view had decimated it.
            if self.decimate_key.is_some() {
                let positions = pack(&self.src_x, &self.src_y);
                if !positions.is_empty() {
                    queue.write_buffer(&self.points, 0, bytemuck::cast_slice(&positions));
                }
                self.count = self.full_count;
                self.decimate_key = None;
            }
            return;
        }

        let key = (x_min.to_bits(), x_max.to_bits(), columns);
        if self.decimate_key == Some(key) {
            return; // view unchanged since the last decimation
        }

        let (dx, dy) = min_max_decimate(&self.src_x, &self.src_y, x_min, x_max, columns);
        let positions = pack(&dx, &dy);
        if !positions.is_empty() {
            queue.write_buffer(&self.points, 0, bytemuck::cast_slice(&positions));
        }
        self.count = positions.len() as u32;
        self.decimate_key = Some(key);
    }

    /// Recompute the per-vertex cumulative pixel arc length for the current
    /// transform and upload it, when the curve is dashed and the view changed.
    /// A no-op for solid lines (arc length is unused) and for an unchanged view.
    /// Must run before [`Self::write_uniforms`] each frame, like
    /// [`Self::ensure_decimated`] (`doc/design.md` §13 D1).
    pub(crate) fn ensure_arclen(
        &mut self,
        queue: &wgpu::Queue,
        ortho: [[f32; 4]; 4],
        axis_log: [f32; 2],
        viewport_px: [f32; 2],
    ) {
        if self.line_style.dash_spec(self.width).is_none() {
            return; // solid line: arc length is never sampled
        }
        let key = (ortho, axis_log, viewport_px);
        if self.arclen_key == Some(key) {
            return; // view unchanged since the last computation
        }
        // Dashed curves never decimate, so the drawn points are the full source.
        let lens = cumulative_arclen(&self.src_x, &self.src_y, &ortho, axis_log, viewport_px);
        if !lens.is_empty() {
            queue.write_buffer(&self.arclen, 0, bytemuck::cast_slice(&lens));
        }
        self.arclen_key = Some(key);
    }

    /// Update the per-frame data->NDC transform, axis-scale flags, and data-area
    /// pixel size (re-stamping the color, width, and dash pattern too).
    /// `axis_log` is `[x, y]` with 1.0 for a log10 axis; `viewport_px` is the
    /// data area in physical pixels, used to keep the line width uniform in
    /// pixel space.
    pub(crate) fn write_uniforms(
        &self,
        queue: &wgpu::Queue,
        ortho: [[f32; 4]; 4],
        axis_log: [f32; 2],
        viewport_px: [f32; 2],
    ) {
        let (dash_cum, dash_offset) = self
            .line_style
            .dash_spec(self.width)
            .unwrap_or(([0.0; 4], 0.0));
        let params = CurveParams {
            ortho,
            color: self.color,
            gap_color: self.gap_color.unwrap_or([0.0; 4]),
            dash_cum,
            axis_log,
            viewport_px,
            half_width_px: 0.5 * self.width,
            use_vertex_color: if self.vertex_color { 1.0 } else { 0.0 },
            dash_offset,
            use_gap_color: if self.gap_color.is_some() { 1.0 } else { 0.0 },
        };
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));

        // Marker uniform shares the same transform/viewport; symbol code is the
        // sentinel `0` (unused) when no marker is set.
        let marker = MarkerParams {
            ortho,
            color: self.color,
            axis_log,
            viewport_px,
            half_size_px: 0.5 * self.marker_size,
            symbol: self.symbol.map_or(0, Symbol::code),
            _pad: [0.0; 2],
        };
        queue.write_buffer(&self.marker_params, 0, bytemuck::bytes_of(&marker));
    }

    /// Draw the polyline (thick-line quads). A no-op below two points or when
    /// the line style draws no line ([`LineStyle::None`] — markers only).
    pub(crate) fn draw(&self, render_pass: &mut wgpu::RenderPass<'_>, pipeline: &CurvePipeline) {
        // Need at least two points (one segment) to draw anything.
        if self.count < 2 || !self.line_style.draws_line() {
            return;
        }
        // 6 vertices (two triangles) per segment; segment count = points - 1.
        let vertices = 6 * (self.count - 1);
        render_pass.set_pipeline(&pipeline.pipeline);
        render_pass.set_bind_group(0, &self.bind_group, &[]);
        render_pass.draw(0..vertices, 0..1);
    }

    /// Draw a marker at each point, if this curve has a symbol. A no-op when no
    /// symbol is set or there are no points.
    pub(crate) fn draw_markers(
        &self,
        render_pass: &mut wgpu::RenderPass<'_>,
        pipeline: &CurvePipeline,
    ) {
        if self.symbol.is_none() || self.count == 0 {
            return;
        }
        // One quad (6 vertices) per point.
        let vertices = 6 * self.count;
        render_pass.set_pipeline(&pipeline.marker_pipeline);
        render_pass.set_bind_group(0, &self.marker_bind_group, &[]);
        render_pass.draw(0..vertices, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_codes_match_shader_switch() {
        // These must stay in sync with the `switch` cases in markers.wgsl.
        assert_eq!(Symbol::Circle.code(), 0);
        assert_eq!(Symbol::Square.code(), 1);
        assert_eq!(Symbol::Cross.code(), 2);
        assert_eq!(Symbol::Plus.code(), 3);
        assert_eq!(Symbol::Triangle.code(), 4);
    }

    #[test]
    fn curve_data_defaults_and_builders() {
        let c = CurveData::new(vec![0.0, 1.0], vec![0.0, 1.0], Color32::WHITE);
        assert_eq!(c.width, 1.0);
        assert_eq!(c.symbol, None);
        assert_eq!(c.marker_size, 7.0);
        assert_eq!(c.y_axis, YAxis::Left);
        assert_eq!(c.colors, None);

        let c = c
            .with_width(-3.0) // clamped to 0
            .with_symbol(Symbol::Plus)
            .with_marker_size(-1.0) // clamped to 0
            .with_y_axis(YAxis::Right);
        assert_eq!(c.width, 0.0);
        assert_eq!(c.symbol, Some(Symbol::Plus));
        assert_eq!(c.marker_size, 0.0);
        assert_eq!(c.y_axis, YAxis::Right);
    }

    #[test]
    fn with_colors_sets_per_vertex_colors() {
        let c = CurveData::new(vec![0.0, 1.0, 2.0], vec![0.0, 1.0, 0.0], Color32::WHITE)
            .with_colors(vec![Color32::RED, Color32::GREEN, Color32::BLUE]);
        assert_eq!(
            c.colors,
            Some(vec![Color32::RED, Color32::GREEN, Color32::BLUE])
        );
    }

    #[test]
    #[should_panic(expected = "colors must have one entry per vertex")]
    fn with_colors_rejects_length_mismatch() {
        CurveData::new(vec![0.0, 1.0, 2.0], vec![0.0, 1.0, 0.0], Color32::WHITE)
            .with_colors(vec![Color32::RED, Color32::GREEN]);
    }

    #[test]
    fn pack_colors_matches_single_color_conversion() {
        // Per-vertex packing must match the single-color path so a uniform
        // per-vertex list renders identically to a single `color`.
        let c = Color32::from_rgba_unmultiplied(200, 100, 50, 180);
        assert_eq!(pack_colors(&[c])[0], egui::Rgba::from(c).to_array());
    }

    #[test]
    fn line_style_default_and_builders() {
        let c = CurveData::new(vec![0.0, 1.0], vec![0.0, 1.0], Color32::WHITE);
        assert_eq!(c.line_style, LineStyle::Solid);
        assert_eq!(c.gap_color, None);

        let c = c
            .with_line_style(LineStyle::Dashed)
            .with_gap_color(Color32::BLACK);
        assert_eq!(c.line_style, LineStyle::Dashed);
        assert_eq!(c.gap_color, Some(Color32::BLACK));
    }

    #[test]
    fn dash_spec_solid_and_none_are_undashed() {
        assert_eq!(LineStyle::Solid.dash_spec(1.0), None);
        assert_eq!(LineStyle::None.dash_spec(1.0), None);
    }

    #[test]
    fn draws_line_false_only_for_none() {
        assert!(!LineStyle::None.draws_line());
        assert!(LineStyle::Solid.draws_line());
        assert!(LineStyle::Dashed.draws_line());
        assert!(LineStyle::DashDot.draws_line());
        assert!(LineStyle::Dotted.draws_line());
    }

    #[test]
    fn dash_spec_boundaries_and_period() {
        // Dashed at width 1: on=5, off=4 -> cumulative [5, 9, 9, 9], period 9.
        let (cum, off) = LineStyle::Dashed.dash_spec(1.0).expect("dashed");
        assert_eq!(cum, [5.0, 9.0, 9.0, 9.0]);
        assert_eq!(off, 0.0);

        // Dash-dot at width 1: dash 6, gap 3, dot 1.5, gap 3.
        let (cum, _) = LineStyle::DashDot.dash_spec(1.0).expect("dashdot");
        assert_eq!(cum, [6.0, 9.0, 10.5, 13.5]);
    }

    #[test]
    fn dash_spec_scales_with_width() {
        // Predefined patterns scale with max(width, 1): width 2 doubles them.
        let (cum, _) = LineStyle::Dashed.dash_spec(2.0).expect("dashed");
        assert_eq!(cum, [10.0, 18.0, 18.0, 18.0]);
    }

    #[test]
    fn dash_spec_custom_pattern_and_offset() {
        let style = LineStyle::Custom {
            offset: 2.0,
            pattern: vec![3.0, 1.0],
        };
        let (cum, off) = style.dash_spec(1.0).expect("custom");
        assert_eq!(cum, [3.0, 4.0, 4.0, 4.0]);
        assert_eq!(off, 2.0);

        // A degenerate (empty / zero-period) custom pattern is treated as solid.
        let empty = LineStyle::Custom {
            offset: 0.0,
            pattern: vec![],
        };
        assert_eq!(empty.dash_spec(1.0), None);
    }

    #[test]
    fn cumulative_arclen_matches_pixel_distance() {
        use crate::core::transform::{Axis, Transform};
        use egui::{Rect, pos2};

        // A linear transform; the cumulative pixel arc length must equal the sum
        // of `data_to_pixel` distances (the on-screen length the dashes span).
        let area = Rect::from_min_max(pos2(50.0, 30.0), pos2(450.0, 230.0));
        let t = Transform::with_axes(Axis::linear(-2.0, 6.0), Axis::linear(1.0, 9.0), area);
        let ortho = t.ortho_matrix();
        let viewport = [area.width(), area.height()];

        let x = vec![-2.0, 0.0, 3.0, 6.0];
        let y = vec![1.0, 4.0, 2.0, 9.0];
        let lens = cumulative_arclen(&x, &y, &ortho, [0.0, 0.0], viewport);
        assert_eq!(lens.len(), 4);
        assert_eq!(lens[0], 0.0);
        assert!(lens.windows(2).all(|w| w[1] >= w[0]), "non-decreasing");

        let mut expected = 0.0f32;
        for i in 1..x.len() {
            let a = t.data_to_pixel(x[i - 1], y[i - 1]);
            let b = t.data_to_pixel(x[i], y[i]);
            expected += (b - a).length();
        }
        let total = *lens.last().unwrap();
        assert!(
            (total - expected).abs() <= 1e-2,
            "arc length {total} vs pixel distance {expected}"
        );
    }
}
