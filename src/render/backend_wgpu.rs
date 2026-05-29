//! The wgpu backend — the `egui_wgpu::CallbackTrait` impl and persistent GPU
//! resources.
//!
//! Slice 1, step 1 scope: a "clear" pipeline that fills the data rect with a
//! solid color, plus the uniform holding that color. Image/curve pipelines and
//! per-plot/per-item GPU state maps are added to [`WgpuResources`] in later
//! steps (`doc/design.md` §3.1·§11).

use std::num::NonZeroU64;

use egui_wgpu::{RenderState, wgpu};

use crate::render::gpu_curve::{CurveData, CurvePipeline, GpuCurve};
use crate::render::gpu_image::{GpuImage, ImageData, ImagePipeline};

/// GPU resources that persist across frames. Stored as a single type in
/// `egui_wgpu`'s `callback_resources` (a type map).
///
/// Note: this step assumes a single plot with a single image. Multi-plot /
/// multi-image extends this with a `HashMap<PlotId, _>` of per-plot state and a
/// map of images per plot (`doc/design.md` §3.1·§12).
pub struct WgpuResources {
    clear_pipeline: wgpu::RenderPipeline,
    color_uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    image_pipeline: ImagePipeline,
    image: Option<GpuImage>,
    curve_pipeline: CurvePipeline,
    curve: Option<GpuCurve>,
}

impl WgpuResources {
    /// Build the clear pipeline and the color uniform.
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("egui-silx clear"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/clear.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("egui-silx clear bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(16),
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("egui-silx clear layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let clear_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("egui-silx clear pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                // blend: None (from target_format.into()) → replace write = a true clear.
                targets: &[Some(target_format.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let color_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("egui-silx clear color"),
            size: 16, // vec4<f32>
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("egui-silx clear bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: color_uniform.as_entire_binding(),
            }],
        });

        let image_pipeline = ImagePipeline::new(device, target_format);
        let curve_pipeline = CurvePipeline::new(device, target_format);

        Self {
            clear_pipeline,
            color_uniform,
            bind_group,
            image_pipeline,
            image: None,
            curve_pipeline,
            curve: None,
        }
    }
}

/// Install [`WgpuResources`] into eframe's `RenderState` once. Call this at app
/// creation with the `RenderState` obtained from the `CreationContext`.
pub fn install(render_state: &RenderState) {
    let resources = WgpuResources::new(&render_state.device, render_state.target_format);
    render_state
        .renderer
        .write()
        .callback_resources
        .insert(resources);
}

/// Upload `image` to the GPU and make it the plot's current image. Requires
/// [`install`] to have run first. The image is uploaded once here; the per-frame
/// transform is applied by [`ImageCallback`].
pub fn set_image(render_state: &RenderState, image: &ImageData) {
    let mut renderer = render_state.renderer.write();
    let res: &mut WgpuResources = renderer
        .callback_resources
        .get_mut()
        .expect("WgpuResources not installed — call egui_silx::install() first");
    let gpu = GpuImage::new(
        &render_state.device,
        &render_state.queue,
        &res.image_pipeline,
        image,
    );
    res.image = Some(gpu);
}

/// Upload `curve` to the GPU and make it the plot's current curve. Requires
/// [`install`] to have run first. The vertices are uploaded once here; the
/// per-frame transform is applied by [`CurveCallback`].
pub fn set_curve(render_state: &RenderState, curve: &CurveData) {
    let mut renderer = render_state.renderer.write();
    let res: &mut WgpuResources = renderer
        .callback_resources
        .get_mut()
        .expect("WgpuResources not installed — call egui_silx::install() first");
    let gpu = GpuCurve::new(
        &render_state.device,
        &render_state.queue,
        &res.curve_pipeline,
        curve,
    );
    res.curve = Some(gpu);
}

/// Re-upload a `w × h` sub-region of the current image at `(x0, y0)` in place
/// (dirty update), reusing the existing texture. `data` is row-major, length
/// `w * h`. A no-op if no image has been set. This is the partial-write path
/// for live updates (`doc/design.md` §11.7).
pub fn update_image_region(
    render_state: &RenderState,
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    data: &[f32],
) {
    let renderer = render_state.renderer.read();
    let res: &WgpuResources = renderer
        .callback_resources
        .get()
        .expect("WgpuResources not installed — call egui_silx::install() first");
    if let Some(image) = &res.image {
        image.update_region(&render_state.queue, x0, y0, w, h, data);
    }
}

/// Re-upload `curve`'s vertices in place (dirty update), reusing the existing
/// GPU buffer when the vertex count fits; reallocates only if it grew beyond
/// the allocated capacity. Creates the curve if none has been set yet
/// (`doc/design.md` §11.7).
pub fn update_curve(render_state: &RenderState, curve: &CurveData) {
    let mut renderer = render_state.renderer.write();
    let res: &mut WgpuResources = renderer
        .callback_resources
        .get_mut()
        .expect("WgpuResources not installed — call egui_silx::install() first");
    let fits = match &mut res.curve {
        Some(existing) => existing.update(&render_state.queue, curve),
        None => false,
    };
    if !fits {
        let gpu = GpuCurve::new(
            &render_state.device,
            &render_state.queue,
            &res.curve_pipeline,
            curve,
        );
        res.curve = Some(gpu);
    }
}

/// Paint callback that fills the data rect with a solid color. This is a
/// lightweight value re-registered by the egui side every frame; the actual GPU
/// resources are looked up from [`WgpuResources`].
pub(crate) struct ClearCallback {
    /// Linear color space, premultiplied RGBA.
    pub color: [f32; 4],
}

impl egui_wgpu::CallbackTrait for ClearCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let res: &WgpuResources = resources
            .get()
            .expect("WgpuResources not installed — call egui_silx::install() at startup");
        queue.write_buffer(&res.color_uniform, 0, bytemuck::bytes_of(&self.color));
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let res: &WgpuResources = resources
            .get()
            .expect("WgpuResources not installed — call egui_silx::install() at startup");
        render_pass.set_pipeline(&res.clear_pipeline);
        render_pass.set_bind_group(0, &res.bind_group, &[]);
        render_pass.draw(0..3, 0..1);
    }
}

/// Paint callback that draws the plot's image (if any) with the given per-frame
/// data→NDC transform. A no-op when no image has been set.
pub(crate) struct ImageCallback {
    /// data→NDC orthographic matrix from the plot's `Transform`.
    pub ortho: [[f32; 4]; 4],
}

impl egui_wgpu::CallbackTrait for ImageCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let res: &WgpuResources = resources
            .get()
            .expect("WgpuResources not installed — call egui_silx::install() at startup");
        if let Some(image) = &res.image {
            image.write_uniforms(queue, self.ortho);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let res: &WgpuResources = resources
            .get()
            .expect("WgpuResources not installed — call egui_silx::install() at startup");
        if let Some(image) = &res.image {
            image.draw(render_pass, &res.image_pipeline);
        }
    }
}

/// Paint callback that draws the plot's curve (if any) with the given per-frame
/// data→NDC transform. A no-op when no curve has been set.
pub(crate) struct CurveCallback {
    /// data→NDC orthographic matrix from the plot's `Transform`.
    pub ortho: [[f32; 4]; 4],
}

impl egui_wgpu::CallbackTrait for CurveCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let res: &WgpuResources = resources
            .get()
            .expect("WgpuResources not installed — call egui_silx::install() at startup");
        if let Some(curve) = &res.curve {
            curve.write_uniforms(queue, self.ortho);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let res: &WgpuResources = resources
            .get()
            .expect("WgpuResources not installed — call egui_silx::install() at startup");
        if let Some(curve) = &res.curve {
            curve.draw(render_pass, &res.curve_pipeline);
        }
    }
}
