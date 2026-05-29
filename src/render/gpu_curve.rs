//! GPU-side curve: the shared curve pipeline and a single uploaded polyline.
//!
//! [`CurveData`] is the CPU spec (mirrors silx `addCurve`: x/y arrays + color).
//! [`CurvePipeline`] holds the line-strip pipeline shared across curves.
//! [`GpuCurve`] owns one curve's vertex buffer + uniform + bind group and
//! persists across frames in `WgpuResources`.
//!
//! Scope: a single curve, uniform color, line-strip topology with wgpu's fixed
//! 1px line width. In-place vertex re-upload ([`GpuCurve::update`]) reuses the
//! buffer for live updates. Thick lines (quad expansion), per-vertex color, and
//! markers are later steps (`doc/design.md` §7·§11).

use std::num::NonZeroU64;

use egui::Color32;
use egui_wgpu::wgpu;

/// Identity ortho matrix; replaced every frame by the widget's transform.
const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// Vertex layout: one `vec2<f32>` data-space position per vertex.
const VERTEX_ATTRS: [wgpu::VertexAttribute; 1] = [wgpu::VertexAttribute {
    format: wgpu::VertexFormat::Float32x2,
    offset: 0,
    shader_location: 0,
}];

/// Uniform block for the curve shader. Layout matches `Params` in `curve.wgsl`
/// (std140-compatible: mat4 at 0, vec4 at 64, total 80).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CurveParams {
    ortho: [[f32; 4]; 4],
    color: [f32; 4],
}

/// A polyline to draw, in data coordinates. `x[i], y[i]` is vertex `i`; the
/// vertices are connected in order.
#[derive(Clone, Debug)]
pub struct CurveData {
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    pub color: Color32,
}

impl CurveData {
    /// Build a curve from equal-length x/y arrays with the given line color.
    pub fn new(x: Vec<f64>, y: Vec<f64>, color: Color32) -> Self {
        assert_eq!(x.len(), y.len(), "x and y must have equal length");
        Self { x, y, color }
    }
}

/// The render pipeline shared by all curves.
pub struct CurvePipeline {
    pub(crate) pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl CurvePipeline {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("egui-silx curve"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/curve.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("egui-silx curve bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(std::mem::size_of::<CurveParams>() as u64),
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("egui-silx curve layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("egui-silx curve pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<[f32; 2]>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &VERTEX_ATTRS,
                }],
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
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

/// One uploaded curve's GPU resources, persisting across frames.
pub struct GpuCurve {
    vertices: wgpu::Buffer,
    count: u32,
    /// Vertices the buffer can hold; an in-place [`Self::update`] up to this
    /// many vertices reuses the buffer instead of reallocating.
    capacity: u32,
    params: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    color: [f32; 4],
}

impl GpuCurve {
    /// Upload `curve`'s vertices and build its uniform + bind group.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &CurvePipeline,
        curve: &CurveData,
    ) -> Self {
        let positions: Vec<[f32; 2]> = curve
            .x
            .iter()
            .zip(&curve.y)
            .map(|(&x, &y)| [x as f32, y as f32])
            .collect();

        // max(1) keeps a zero-vertex curve from creating a zero-size buffer; the
        // draw is still skipped (count < 2) so nothing is rendered.
        let capacity = positions.len().max(1) as u32;
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("egui-silx curve vertices"),
            size: (capacity as usize * std::mem::size_of::<[f32; 2]>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if !positions.is_empty() {
            queue.write_buffer(&vertices, 0, bytemuck::cast_slice(&positions));
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
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: params.as_entire_binding(),
            }],
        });

        // sRGB Color32 -> linear, premultiplied RGBA (matches the alpha-blend target).
        let color = egui::Rgba::from(curve.color).to_array();

        let gpu = Self {
            vertices,
            count: positions.len() as u32,
            capacity,
            params,
            bind_group,
            color,
        };
        // Seed the uniform; the per-frame transform overwrites `ortho`.
        gpu.write_uniforms(queue, IDENTITY);
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
        let positions: Vec<[f32; 2]> = curve
            .x
            .iter()
            .zip(&curve.y)
            .map(|(&x, &y)| [x as f32, y as f32])
            .collect();
        if !positions.is_empty() {
            queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(&positions));
        }
        self.count = positions.len() as u32;
        self.color = egui::Rgba::from(curve.color).to_array();
        true
    }

    /// Update the per-frame data->NDC transform (and re-stamp the color).
    pub(crate) fn write_uniforms(&self, queue: &wgpu::Queue, ortho: [[f32; 4]; 4]) {
        let params = CurveParams {
            ortho,
            color: self.color,
        };
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
    }

    pub(crate) fn draw(&self, render_pass: &mut wgpu::RenderPass<'_>, pipeline: &CurvePipeline) {
        // A line strip needs at least two vertices to draw a segment.
        if self.count < 2 {
            return;
        }
        render_pass.set_pipeline(&pipeline.pipeline);
        render_pass.set_bind_group(0, &self.bind_group, &[]);
        render_pass.set_vertex_buffer(0, self.vertices.slice(..));
        render_pass.draw(0..self.count, 0..1);
    }
}
