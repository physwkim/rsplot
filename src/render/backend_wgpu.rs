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
    /// All curves on the plot, drawn in order. Each carries its own Y-axis
    /// binding (left or y2), selected per frame in [`CurveCallback`].
    curves: Vec<GpuCurve>,
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
            curves: Vec::new(),
        }
    }

    /// Render the data layer (background clear, image, curves) for the given
    /// per-axis transforms into an offscreen `size = (w, h)` texture, read it
    /// back, and return tightly packed RGBA8 pixels (top row first). Used by
    /// [`crate::render::save::save_graph`] (`doc/design.md` §13 E1).
    ///
    /// `bg` is the linear, premultiplied clear color; the orthos/axis_log pairs
    /// are the same matrices the on-screen [`CurveCallback`] uses for each Y
    /// axis. Curves are drawn at whatever resolution they were last decimated
    /// to (the current view); no re-decimation is performed here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_to_rgba(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
        size: (u32, u32),
        bg: [f32; 4],
        ortho_left: [[f32; 4]; 4],
        axis_log_left: [f32; 2],
        ortho_right: [[f32; 4]; 4],
        axis_log_right: [f32; 2],
    ) -> Result<Vec<u8>, crate::render::save::SaveError> {
        use crate::core::transform::YAxis;
        use crate::render::save::{padded_bytes_per_row, rows_to_rgba8};

        let (w, h) = size;
        let extent = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("egui-silx offscreen target"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        // Stamp the offscreen uniforms (line width uses the target pixel size).
        let viewport_px = [w as f32, h as f32];
        if let Some(image) = &self.image {
            image.write_uniforms(queue, ortho_left, axis_log_left);
        }
        for curve in &self.curves {
            let (ortho, axis_log) = match curve.y_axis {
                YAxis::Left => (ortho_left, axis_log_left),
                YAxis::Right => (ortho_right, axis_log_right),
            };
            curve.write_uniforms(queue, ortho, axis_log, viewport_px);
        }

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui-silx offscreen pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: bg[0] as f64,
                            g: bg[1] as f64,
                            b: bg[2] as f64,
                            a: bg[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // The render pass viewport defaults to the full target. Lines first,
            // then markers, mirroring the on-screen draw order.
            if let Some(image) = &self.image {
                image.draw(&mut rp, &self.image_pipeline);
            }
            for curve in &self.curves {
                curve.draw(&mut rp, &self.curve_pipeline);
            }
            for curve in &self.curves {
                curve.draw_markers(&mut rp, &self.curve_pipeline);
            }
        }

        // Copy the target into a readback buffer with a padded row stride.
        let bpr = padded_bytes_per_row(w);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("egui-silx readback"),
            size: (bpr as u64) * (h as u64),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(h),
                },
            },
            extent,
        );
        queue.submit([encoder.finish()]);

        // Map the buffer and block until the GPU is done.
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| crate::render::save::SaveError::Readback(format!("poll: {e}")))?;
        rx.recv()
            .map_err(|e| crate::render::save::SaveError::Readback(format!("map channel: {e}")))?
            .map_err(|e| crate::render::save::SaveError::Readback(format!("buffer map: {e}")))?;

        let rgba = {
            let mapped = buffer.slice(..).get_mapped_range();
            rows_to_rgba8(&mapped, w, h, bpr, target_format)
        };
        buffer.unmap();
        Ok(rgba)
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

/// Upload `curve` to the GPU as the plot's sole curve (replacing any existing
/// curves). Requires [`install`] to have run first. The vertices are uploaded
/// once here; the per-frame transform is applied by [`CurveCallback`].
pub fn set_curve(render_state: &RenderState, curve: &CurveData) {
    set_curves(render_state, std::slice::from_ref(curve));
}

/// Upload `curves` to the GPU as the plot's full curve set (replacing any
/// existing curves), preserving order. Each curve keeps its own Y-axis binding
/// ([`CurveData::y_axis`]). Requires [`install`] to have run first.
pub fn set_curves(render_state: &RenderState, curves: &[CurveData]) {
    let mut renderer = render_state.renderer.write();
    let res: &mut WgpuResources = renderer
        .callback_resources
        .get_mut()
        .expect("WgpuResources not installed — call egui_silx::install() first");
    res.curves = curves
        .iter()
        .map(|curve| {
            GpuCurve::new(
                &render_state.device,
                &render_state.queue,
                &res.curve_pipeline,
                curve,
            )
        })
        .collect();
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

/// Re-upload the first curve's vertices in place (dirty update). Convenience
/// for single-curve plots; see [`update_curve_at`] for a specific index.
pub fn update_curve(render_state: &RenderState, curve: &CurveData) {
    update_curve_at(render_state, 0, curve);
}

/// Re-upload curve `index`'s vertices in place (dirty update), reusing the
/// existing GPU buffer when the vertex count fits; reallocates only if it grew
/// beyond the allocated capacity. If `index` is past the end (or no such curve
/// exists yet), the curve set is extended so `index` becomes the last curve
/// (`doc/design.md` §11.7).
pub fn update_curve_at(render_state: &RenderState, index: usize, curve: &CurveData) {
    let mut renderer = render_state.renderer.write();
    let res: &mut WgpuResources = renderer
        .callback_resources
        .get_mut()
        .expect("WgpuResources not installed — call egui_silx::install() first");
    let fits = match res.curves.get_mut(index) {
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
        match res.curves.get_mut(index) {
            Some(slot) => *slot = gpu,
            None => res.curves.push(gpu),
        }
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
    /// Per-axis log flag `[x, y]` (1.0 = log10), matching the transform.
    pub axis_log: [f32; 2],
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
            image.write_uniforms(queue, self.ortho, self.axis_log);
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

/// Paint callback that draws the plot's curves with the per-frame data→NDC
/// transform of each curve's bound Y axis. A no-op when no curve has been set.
pub(crate) struct CurveCallback {
    /// Left (main) axis data→NDC matrix; used by `YAxis::Left` curves.
    pub ortho_left: [[f32; 4]; 4],
    /// Per-axis log flag `[x, y]` for the left axis.
    pub axis_log_left: [f32; 2],
    /// Right (y2) axis data→NDC matrix; used by `YAxis::Right` curves. Equals
    /// `ortho_left` when the plot has no y2 axis.
    pub ortho_right: [[f32; 4]; 4],
    /// Per-axis log flag `[x, y]` for the right axis.
    pub axis_log_right: [f32; 2],
    /// Data-area size in physical pixels, for the pixel-space line expansion.
    pub viewport_px: [f32; 2],
    /// Visible data-x window `(x_min, x_max)`, shared by both axes, used to
    /// re-decimate large curves for the current view (`doc/design.md` §13 D1).
    pub x_window: (f64, f64),
    /// Number of pixel columns to decimate into (the data-area pixel width), or
    /// `0` to disable decimation (e.g. a log x-axis, where equal data-x bins are
    /// not equal pixel columns).
    pub decimate_columns: u32,
}

impl CurveCallback {
    /// Pick the (ortho, axis_log) pair matching a curve's bound axis.
    fn matrices_for(&self, y_axis: crate::core::transform::YAxis) -> ([[f32; 4]; 4], [f32; 2]) {
        match y_axis {
            crate::core::transform::YAxis::Left => (self.ortho_left, self.axis_log_left),
            crate::core::transform::YAxis::Right => (self.ortho_right, self.axis_log_right),
        }
    }
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
        let res: &mut WgpuResources = resources
            .get_mut()
            .expect("WgpuResources not installed — call egui_silx::install() at startup");
        let (x_min, x_max) = self.x_window;
        for curve in &mut res.curves {
            // Re-decimate to the current view first (a no-op once the view is
            // steady), then stamp the per-frame uniforms.
            curve.ensure_decimated(queue, x_min, x_max, self.decimate_columns);
            let (ortho, axis_log) = self.matrices_for(curve.y_axis);
            curve.write_uniforms(queue, ortho, axis_log, self.viewport_px);
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
        // Lines first, then markers, so markers sit on top of every line.
        for curve in &res.curves {
            curve.draw(render_pass, &res.curve_pipeline);
        }
        for curve in &res.curves {
            curve.draw_markers(render_pass, &res.curve_pipeline);
        }
    }
}
