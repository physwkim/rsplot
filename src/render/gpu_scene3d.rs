//! The 3D scene renderer — wgpu line/triangle pipelines that draw depth-tested
//! geometry into an offscreen color+depth target, then blit that color into
//! egui's (depth-less) paint pass.
//!
//! This is the plot3d analogue of [`crate::render::backend_wgpu`]: persistent
//! GPU state ([`Scene3dResources`]) lives in `egui_wgpu`'s `callback_resources`
//! type map, installed once via [`install_scene3d`]; the egui side re-registers
//! a lightweight [`Scene3dCallback`] each frame via [`paint_scene3d`].
//!
//! Why offscreen-then-blit: egui's render pass has **no depth attachment**
//! (`doc/plot3d-parity-roadmap.md` Architecture), so depth-tested 3D cannot
//! draw straight into it. Each frame:
//!
//! - `prepare()` sizes an offscreen color+depth texture pair to the widget's
//!   pixel rect, writes the camera MVP uniform, and encodes one depth-tested
//!   pass (clear → triangles → lines) into the offscreen color target.
//! - `paint()` blits that color texture into egui's pass as a viewport-clipped
//!   full-screen triangle.
//!
//! Geometry is uploaded once via [`set_scene3d`] (mirroring `set_curves`); the
//! per-frame camera transform is applied in the shader from the MVP uniform.

use std::collections::HashMap;
use std::num::NonZeroU64;

use egui::Color32;
use egui_wgpu::{RenderState, wgpu};

use crate::core::scene3d::camera::Camera;

/// Scene identity key (mirrors [`crate::core::plot::PlotId`]); lets several
/// independent 3D scenes coexist in one egui app without sharing GPU state.
pub type Scene3dId = u64;

/// Offscreen depth-buffer format. 32-bit float — ample range for the camera's
/// near/far span, and universally supported as a render attachment.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// One scene vertex: world-space position + linear-premultiplied RGBA. Used by
/// both the line and triangle pipelines (shared vertex layout). `repr(C)` so the
/// 28-byte stride matches the WGSL vertex attributes exactly.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Scene3dVertex {
    /// World-space position (the model transform, if any, is folded into the MVP).
    pub pos: [f32; 3],
    /// Linear color space, premultiplied alpha — same convention as the 2D path.
    pub color: [f32; 4],
}

/// Vertex attributes for [`Scene3dVertex`]: position at location 0 (offset 0),
/// color at location 1 (offset 12). Kept as a `'static` const so the
/// [`wgpu::VertexBufferLayout`] can borrow it for pipeline creation.
const SCENE3D_VERTEX_ATTRS: [wgpu::VertexAttribute; 2] = [
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 0,
        shader_location: 0,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x4,
        offset: 12,
        shader_location: 1,
    },
];

/// Uniform block for `scene3d.wgsl`: the column-major, clip-corrected MVP.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Scene3dParams {
    /// `camera.matrix() × model`, transposed to column-major and depth-corrected
    /// for wgpu z∈[0,1] (see [`crate::core::scene3d::mat4::Mat4::to_gpu_clip_cols`]).
    mvp: [[f32; 4]; 4],
}

/// CPU-side geometry for one scene: a flat line-list and a flat triangle-list,
/// each vertex carrying its own color. Build with [`Scene3dGeometry::add_line`]
/// / [`Scene3dGeometry::add_triangle`], then upload via [`set_scene3d`].
#[derive(Clone, Debug, Default)]
pub struct Scene3dGeometry {
    /// Pairs of vertices, each pair one line segment (`LineList` topology).
    pub(crate) lines: Vec<Scene3dVertex>,
    /// Triples of vertices, each triple one triangle (`TriangleList` topology).
    pub(crate) triangles: Vec<Scene3dVertex>,
}

impl Scene3dGeometry {
    /// An empty geometry.
    pub fn new() -> Self {
        Self::default()
    }

    /// True when there is nothing to draw (no lines and no triangles).
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.triangles.is_empty()
    }

    /// Drop all geometry, keeping allocated capacity for reuse.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.triangles.clear();
    }

    /// Append a line segment `a→b` in one solid [`Color32`].
    pub fn add_line(&mut self, a: [f32; 3], b: [f32; 3], color: Color32) {
        let rgba = egui::Rgba::from(color).to_array();
        self.add_line_rgba(a, b, rgba);
    }

    /// Append a line segment `a→b` with explicit linear-premultiplied RGBA.
    pub fn add_line_rgba(&mut self, a: [f32; 3], b: [f32; 3], rgba: [f32; 4]) {
        self.lines.push(Scene3dVertex {
            pos: a,
            color: rgba,
        });
        self.lines.push(Scene3dVertex {
            pos: b,
            color: rgba,
        });
    }

    /// Append a triangle `a, b, c` in one solid [`Color32`].
    pub fn add_triangle(&mut self, a: [f32; 3], b: [f32; 3], c: [f32; 3], color: Color32) {
        let rgba = egui::Rgba::from(color).to_array();
        self.add_triangle_rgba(a, b, c, rgba);
    }

    /// Append a triangle `a, b, c` with explicit linear-premultiplied RGBA.
    pub fn add_triangle_rgba(&mut self, a: [f32; 3], b: [f32; 3], c: [f32; 3], rgba: [f32; 4]) {
        for pos in [a, b, c] {
            self.triangles.push(Scene3dVertex { pos, color: rgba });
        }
    }
}

/// The shared pipelines + layouts for 3D rendering. Built once in
/// [`Scene3dResources::new`].
struct Scene3dPipeline {
    /// egui's surface format; the offscreen color target uses it too so colors
    /// round-trip through the blit without an extra color-space conversion.
    target_format: wgpu::TextureFormat,
    /// `group(0)` layout for the MVP uniform (vertex stage).
    scene_bgl: wgpu::BindGroupLayout,
    /// Depth-tested `LineList` pipeline.
    line_pipeline: wgpu::RenderPipeline,
    /// Depth-tested `TriangleList` pipeline (no face culling).
    tri_pipeline: wgpu::RenderPipeline,
    /// `group(0)` layout for the blit (sampled texture + sampler, fragment stage).
    blit_bgl: wgpu::BindGroupLayout,
    /// Depth-less full-screen blit pipeline (offscreen color → egui pass).
    blit_pipeline: wgpu::RenderPipeline,
    /// Linear-filtering, clamp-to-edge sampler for the blit.
    sampler: wgpu::Sampler,
}

impl Scene3dPipeline {
    fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let scene_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("siplot scene3d"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/scene3d.wgsl").into()),
        });
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("siplot scene3d blit"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/scene3d_blit.wgsl").into()),
        });

        let scene_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("siplot scene3d scene bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(64),
                },
                count: None,
            }],
        });

        let scene_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("siplot scene3d scene layout"),
            bind_group_layouts: &[Some(&scene_bgl)],
            immediate_size: 0,
        });

        let vertex_buffers = [wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Scene3dVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &SCENE3D_VERTEX_ATTRS,
        }];

        // Lines and triangles differ only in primitive topology; everything else
        // (shader, vertex layout, depth state, target) is shared.
        let make_scene_pipeline = |topology: wgpu::PrimitiveTopology, label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&scene_layout),
                vertex: wgpu::VertexState {
                    module: &scene_shader,
                    entry_point: Some("vs_main"),
                    buffers: &vertex_buffers,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &scene_shader,
                    entry_point: Some("fs_main"),
                    // blend: None (from target_format.into()) → opaque write;
                    // depth testing resolves occlusion.
                    targets: &[Some(target_format.into())],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology,
                    // No culling: wireframes/axes and double-sided meshes must
                    // show both faces (silx does not cull these).
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        let line_pipeline =
            make_scene_pipeline(wgpu::PrimitiveTopology::LineList, "siplot scene3d lines");
        let tri_pipeline = make_scene_pipeline(
            wgpu::PrimitiveTopology::TriangleList,
            "siplot scene3d triangles",
        );

        let blit_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("siplot scene3d blit bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let blit_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("siplot scene3d blit layout"),
            bind_group_layouts: &[Some(&blit_bgl)],
            immediate_size: 0,
        });

        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("siplot scene3d blit pipeline"),
            layout: Some(&blit_layout),
            vertex: wgpu::VertexState {
                module: &blit_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
                entry_point: Some("fs_main"),
                // blend: None → replace; the scene (opaque background) occludes
                // whatever egui drew behind the widget rect.
                targets: &[Some(target_format.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            // egui's pass has no depth attachment, so the blit must not test depth.
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("siplot scene3d blit sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        Self {
            target_format,
            scene_bgl,
            line_pipeline,
            tri_pipeline,
            blit_bgl,
            blit_pipeline,
            sampler,
        }
    }
}

/// Per-scene GPU data: vertex buffers, the MVP uniform, and the offscreen
/// color+depth render target (recreated on size change).
struct Scene3dGpu {
    /// MVP uniform, written each frame in [`Scene3dResources::prepare_scene`].
    params_buf: wgpu::Buffer,
    /// `group(0)` bind group over `params_buf` for the scene pipelines.
    scene_bind_group: wgpu::BindGroup,
    /// Line vertices; `None` while empty (skip the draw).
    line_vbuf: Option<wgpu::Buffer>,
    line_count: u32,
    /// Triangle vertices; `None` while empty (skip the draw).
    tri_vbuf: Option<wgpu::Buffer>,
    tri_count: u32,
    /// Pixel size of the current offscreen target (`[0, 0]` until first sized).
    size: [u32; 2],
    /// Offscreen color view (target format); the blit samples this.
    color_view: Option<wgpu::TextureView>,
    /// Offscreen depth view (`Depth32Float`), for depth testing.
    depth_view: Option<wgpu::TextureView>,
    /// `group(0)` bind group over the color view + sampler for the blit pipeline.
    blit_bind_group: Option<wgpu::BindGroup>,
}

impl Scene3dGpu {
    fn new(device: &wgpu::Device, pipeline: &Scene3dPipeline) -> Self {
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("siplot scene3d params"),
            size: std::mem::size_of::<Scene3dParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let scene_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("siplot scene3d scene bind group"),
            layout: &pipeline.scene_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: params_buf.as_entire_binding(),
            }],
        });
        Self {
            params_buf,
            scene_bind_group,
            line_vbuf: None,
            line_count: 0,
            tri_vbuf: None,
            tri_count: 0,
            size: [0, 0],
            color_view: None,
            depth_view: None,
            blit_bind_group: None,
        }
    }

    /// Replace the line + triangle vertex buffers from `geometry`.
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, geometry: &Scene3dGeometry) {
        self.line_vbuf = make_vertex_buffer(device, queue, &geometry.lines, "siplot scene3d lines");
        self.line_count = geometry.lines.len() as u32;
        self.tri_vbuf =
            make_vertex_buffer(device, queue, &geometry.triangles, "siplot scene3d tris");
        self.tri_count = geometry.triangles.len() as u32;
    }

    /// Ensure the offscreen color+depth target matches `size` (in physical
    /// pixels), recreating the textures and blit bind group on a size change.
    fn ensure_offscreen(
        &mut self,
        device: &wgpu::Device,
        pipeline: &Scene3dPipeline,
        size: [u32; 2],
    ) {
        let size = [size[0].max(1), size[1].max(1)];
        if self.size == size && self.color_view.is_some() {
            return;
        }
        let extent = wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        };
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("siplot scene3d color"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: pipeline.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("siplot scene3d depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        let blit_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("siplot scene3d blit bind group"),
            layout: &pipeline.blit_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
                },
            ],
        });
        self.size = size;
        self.color_view = Some(color_view);
        self.depth_view = Some(depth_view);
        self.blit_bind_group = Some(blit_bind_group);
    }

    /// Encode the offscreen depth-tested pass (clear → triangles → lines) into
    /// `encoder`. Runs in `prepare()`, before the blit samples the result.
    fn encode_offscreen(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &Scene3dPipeline,
        background: [f32; 4],
    ) {
        let (Some(color_view), Some(depth_view)) = (&self.color_view, &self.depth_view) else {
            return;
        };
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("siplot scene3d offscreen pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: background[0] as f64,
                        g: background[1] as f64,
                        b: background[2] as f64,
                        a: background[3] as f64,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        rp.set_bind_group(0, &self.scene_bind_group, &[]);
        if let (Some(buf), true) = (&self.tri_vbuf, self.tri_count > 0) {
            rp.set_pipeline(&pipeline.tri_pipeline);
            rp.set_vertex_buffer(0, buf.slice(..));
            rp.draw(0..self.tri_count, 0..1);
        }
        if let (Some(buf), true) = (&self.line_vbuf, self.line_count > 0) {
            rp.set_pipeline(&pipeline.line_pipeline);
            rp.set_vertex_buffer(0, buf.slice(..));
            rp.draw(0..self.line_count, 0..1);
        }
    }
}

/// Create a `VERTEX | COPY_DST` buffer holding `verts`, or `None` when empty.
fn make_vertex_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    verts: &[Scene3dVertex],
    label: &str,
) -> Option<wgpu::Buffer> {
    if verts.is_empty() {
        return None;
    }
    let bytes = bytemuck::cast_slice(verts);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, bytes);
    Some(buffer)
}

/// Persistent 3D GPU resources, stored in `egui_wgpu`'s `callback_resources`.
/// Per-scene state is keyed by [`Scene3dId`].
pub struct Scene3dResources {
    pipeline: Scene3dPipeline,
    scenes: HashMap<Scene3dId, Scene3dGpu>,
}

impl Scene3dResources {
    fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        Self {
            pipeline: Scene3dPipeline::new(device, target_format),
            scenes: HashMap::new(),
        }
    }

    /// Size the offscreen target, write the MVP uniform, and encode the
    /// depth-tested offscreen pass for `frame.id` (creating per-scene state if
    /// needed).
    fn prepare_scene(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &Scene3dFrame,
    ) {
        let Self { pipeline, scenes } = self;
        let scene = scenes
            .entry(frame.id)
            .or_insert_with(|| Scene3dGpu::new(device, pipeline));
        scene.ensure_offscreen(device, pipeline, frame.size_px);
        let params = Scene3dParams { mvp: frame.mvp };
        queue.write_buffer(&scene.params_buf, 0, bytemuck::bytes_of(&params));
        scene.encode_offscreen(encoder, pipeline, frame.background);
    }
}

/// Install the 3D scene GPU resources into `render_state` if not already present.
/// Idempotent — safe to call once per app startup (independent of the 2D
/// [`crate::render::backend_wgpu::install`]).
pub fn install_scene3d(render_state: &RenderState) {
    let mut renderer = render_state.renderer.write();
    if renderer
        .callback_resources
        .get::<Scene3dResources>()
        .is_some()
    {
        return;
    }
    let resources = Scene3dResources::new(&render_state.device, render_state.target_format);
    renderer.callback_resources.insert(resources);
}

/// Upload `geometry` as scene `id`'s current geometry (replacing any existing).
/// Requires [`install_scene3d`] to have run first.
pub fn set_scene3d(render_state: &RenderState, id: Scene3dId, geometry: &Scene3dGeometry) {
    let mut renderer = render_state.renderer.write();
    let res: &mut Scene3dResources = renderer
        .callback_resources
        .get_mut()
        .expect("Scene3dResources not installed — call siplot::install_scene3d() first");
    let Scene3dResources { pipeline, scenes } = res;
    let scene = scenes
        .entry(id)
        .or_insert_with(|| Scene3dGpu::new(&render_state.device, pipeline));
    scene.upload(&render_state.device, &render_state.queue, geometry);
}

/// Register the paint callback that renders scene `id` into `rect` from
/// `camera`'s viewpoint, on `background`. The camera's aspect is taken from
/// `rect`'s pixel size for this frame (the passed `camera` is not mutated).
/// Requires [`install_scene3d`] + [`set_scene3d`].
pub fn paint_scene3d(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: Scene3dId,
    camera: &Camera,
    background: Color32,
) {
    let ppp = ui.ctx().pixels_per_point();
    let w = (rect.width() * ppp).round().max(1.0) as u32;
    let h = (rect.height() * ppp).round().max(1.0) as u32;
    let mut cam = *camera;
    cam.set_size((w as f32, h as f32));
    let mvp = cam.matrix().to_gpu_clip_cols();
    let background = egui::Rgba::from(background).to_array();
    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
        rect,
        Scene3dCallback {
            frame: Scene3dFrame {
                id,
                mvp,
                size_px: [w, h],
                background,
            },
        },
    ));
}

/// The per-frame render request for one scene: which scene, the camera MVP, the
/// target pixel size, and the clear color. Grouping these keeps the prepare API
/// to a single owner rather than a long positional argument list.
#[derive(Clone, Copy)]
struct Scene3dFrame {
    id: Scene3dId,
    /// Column-major, clip-corrected MVP for this frame.
    mvp: [[f32; 4]; 4],
    /// Offscreen target size in physical pixels.
    size_px: [u32; 2],
    /// Clear color, linear premultiplied.
    background: [f32; 4],
}

/// Lightweight per-frame paint callback (the heavy GPU state lives in
/// [`Scene3dResources`]). Renders offscreen in `prepare`, blits in `paint`.
struct Scene3dCallback {
    frame: Scene3dFrame,
}

impl egui_wgpu::CallbackTrait for Scene3dCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let res: &mut Scene3dResources = resources
            .get_mut()
            .expect("Scene3dResources not installed — call siplot::install_scene3d() at startup");
        res.prepare_scene(device, queue, egui_encoder, &self.frame);
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let res: &Scene3dResources = resources
            .get()
            .expect("Scene3dResources not installed — call siplot::install_scene3d() at startup");
        if let Some(scene) = res.scenes.get(&self.frame.id)
            && let Some(blit_bind_group) = &scene.blit_bind_group
        {
            render_pass.set_pipeline(&res.pipeline.blit_pipeline);
            render_pass.set_bind_group(0, blit_bind_group, &[]);
            render_pass.draw(0..3, 0..1);
        }
    }
}
