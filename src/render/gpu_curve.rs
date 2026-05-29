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

/// Uniform block for the curve shader. Layout matches `Params` in `curve.wgsl`
/// (std140: mat4 @0, vec4 @64, vec2 @80, vec2 @88, f32 @96, f32 @100; padded
/// to 112).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CurveParams {
    ortho: [[f32; 4]; 4],
    color: [f32; 4],
    /// 1.0 if that axis is log10, else 0.0 (x, y).
    axis_log: [f32; 2],
    /// Data-area size in physical pixels (for the pixel-space quad expansion).
    viewport_px: [f32; 2],
    /// Half the line width, in physical pixels.
    half_width_px: f32,
    /// 1.0 to take each vertex's color from the per-vertex color buffer, else
    /// 0.0 to use the uniform `color`.
    use_vertex_color: f32,
    _pad: [f32; 2],
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
    /// Marker uniform + bind group (shares the points buffer at binding 1).
    marker_params: wgpu::Buffer,
    marker_bind_group: wgpu::BindGroup,
    color: [f32; 4],
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
            marker_params,
            marker_bind_group,
            color,
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
        self.width = curve.width;
        self.symbol = curve.symbol;
        self.marker_size = curve.marker_size;
        self.y_axis = curve.y_axis;
        // The buffer now holds the new full source; force a re-decimation for
        // the current view on the next frame.
        self.src_x = curve.x.clone();
        self.src_y = curve.y.clone();
        self.full_count = positions.len() as u32;
        self.monotonic_x = is_monotonic(&curve.x);
        self.decimate_key = None;
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
        // Per-vertex color is excluded: the min/max envelope drops and reorders
        // vertices, which would unalign the colors from their points.
        let beneficial = self.symbol.is_none()
            && !self.vertex_color
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

    /// Update the per-frame data->NDC transform, axis-scale flags, and data-area
    /// pixel size (re-stamping the color and width too). `axis_log` is `[x, y]`
    /// with 1.0 for a log10 axis; `viewport_px` is the data area in physical
    /// pixels, used to keep the line width uniform in pixel space.
    pub(crate) fn write_uniforms(
        &self,
        queue: &wgpu::Queue,
        ortho: [[f32; 4]; 4],
        axis_log: [f32; 2],
        viewport_px: [f32; 2],
    ) {
        let params = CurveParams {
            ortho,
            color: self.color,
            axis_log,
            viewport_px,
            half_width_px: 0.5 * self.width,
            use_vertex_color: if self.vertex_color { 1.0 } else { 0.0 },
            _pad: [0.0; 2],
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

    /// Draw the polyline (thick-line quads). A no-op below two points.
    pub(crate) fn draw(&self, render_pass: &mut wgpu::RenderPass<'_>, pipeline: &CurvePipeline) {
        // Need at least two points (one segment) to draw anything.
        if self.count < 2 {
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
}
