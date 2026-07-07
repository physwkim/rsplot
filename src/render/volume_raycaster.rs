//! GPU resources for the [`crate::widget::volume_raycaster::VolumeRaycaster`].
//!
//! Self-contained and parallel to [`crate::render::gpu_scene3d`], but far
//! simpler: there is no geometry, no offscreen target and no depth buffer. Each
//! frame draws a single full-screen triangle whose fragment shader ray-marches a
//! per-view 3D RGBA texture and composites premultiplied colour straight into
//! egui's render pass (`shaders/volume_raycaster.wgsl`).
//!
//! Lifecycle mirrors the scene path: [`install_volume_raycaster`] once at
//! startup inserts the shared pipeline into `callback_resources`;
//! [`set_volume_raycaster`] uploads a view's volume texture;
//! [`paint_volume_raycaster`] registers the per-frame paint callback.

use std::collections::HashMap;

use egui_wgpu::{RenderState, wgpu};

/// Identifies one raycaster view within the shared resources (caller-assigned,
/// like `Scene3dId`).
pub type VolumeId = u64;

/// Per-frame camera + rendering uniforms. Column-major matrix for WGSL.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VolumeUniforms {
    inv_mvp: [[f32; 4]; 4],
    vol_min: [f32; 4],
    vol_max: [f32; 4],
    params: [f32; 4],
}

/// The camera-derived half of a frame's uniforms (the volume box comes from the
/// per-view texture). Built by the widget from its [`crate::Camera`].
#[derive(Clone, Copy, Debug)]
pub struct VolumeFrame {
    pub id: VolumeId,
    /// Inverse of the camera clip matrix, column-major (`Mat4::to_gpu_cols`).
    pub inv_mvp: [[f32; 4]; 4],
    /// `[step_count, alpha_scale, cull_floor]`.
    pub params: [f32; 3],
}

/// World-space axis-aligned bounds for a `(depth, height, width)` volume: the
/// box is centred at the origin with its longest axis spanning one unit, so the
/// grid keeps its aspect ratio. Shared by the renderer (uniforms) and the widget
/// (camera framing) so the two never disagree.
pub fn volume_bounds(depth: usize, height: usize, width: usize) -> ([f32; 3], [f32; 3]) {
    let (dz, dy, dx) = (
        depth.max(1) as f32,
        height.max(1) as f32,
        width.max(1) as f32,
    );
    let longest = dz.max(dy).max(dx);
    // x ↔ width, y ↔ height, z ↔ depth.
    let half = [dx / longest * 0.5, dy / longest * 0.5, dz / longest * 0.5];
    ([-half[0], -half[1], -half[2]], half)
}

/// Premultiply a straight-alpha RGBA8 buffer (`rgb *= a/255`, rounded), so the
/// linear sampler interpolates premultiplied colour: a transparent voxel becomes
/// `(0,0,0,0)` and interpolating it against an opaque colour keeps the hue
/// instead of dragging it toward black. A trailing partial pixel (len not a
/// multiple of 4) is dropped.
fn premultiply_rgba(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4)
        .flat_map(|px| {
            let a = px[3] as u16;
            [
                ((px[0] as u16 * a + 127) / 255) as u8,
                ((px[1] as u16 * a + 127) / 255) as u8,
                ((px[2] as u16 * a + 127) / 255) as u8,
                px[3],
            ]
        })
        .collect()
}

struct VolumePipeline {
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl VolumePipeline {
    fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rsplot volume raycaster"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/volume_raycaster.wgsl").into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rsplot volume raycaster bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<VolumeUniforms>() as u64,
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rsplot volume raycaster layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rsplot volume raycaster pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rsplot volume raycaster sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        Self {
            pipeline,
            bgl,
            sampler,
        }
    }
}

/// Per-view GPU state: the uniform buffer, the uploaded 3D texture, its bind
/// group and the world-space box the texture occupies.
struct VolumeGpu {
    uniform_buf: wgpu::Buffer,
    bind_group: Option<wgpu::BindGroup>,
    vol_min: [f32; 3],
    vol_max: [f32; 3],
}

impl VolumeGpu {
    fn new(device: &wgpu::Device) -> Self {
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rsplot volume raycaster uniforms"),
            size: std::mem::size_of::<VolumeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            uniform_buf,
            bind_group: None,
            vol_min: [-0.5, -0.5, -0.5],
            vol_max: [0.5, 0.5, 0.5],
        }
    }

    /// Upload a `(depth, height, width)` RGBA8 volume (row-major, straight
    /// alpha), (re)building the 3D texture and bind group. The straight alpha is
    /// premultiplied before upload so the linear sampler interpolates
    /// premultiplied colour (no dark fringe at colour/transparent boundaries).
    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &VolumePipeline,
        rgba: &[u8],
        dims: (usize, usize, usize),
    ) {
        let (depth, height, width) = dims;
        let (w, h, d) = (width as u32, height as u32, depth as u32);
        let premul = premultiply_rgba(rgba);
        let extent = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: d,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rsplot volume raycaster texture"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &premul,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            extent,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D3),
            ..Default::default()
        });
        self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rsplot volume raycaster bind group"),
            layout: &pipeline.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
                },
            ],
        }));
        (self.vol_min, self.vol_max) = volume_bounds(depth, height, width);
    }

    fn write_uniforms(&self, queue: &wgpu::Queue, frame: &VolumeFrame) {
        let u = VolumeUniforms {
            inv_mvp: frame.inv_mvp,
            vol_min: [self.vol_min[0], self.vol_min[1], self.vol_min[2], 0.0],
            vol_max: [self.vol_max[0], self.vol_max[1], self.vol_max[2], 0.0],
            params: [frame.params[0], frame.params[1], frame.params[2], 0.0],
        };
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));
    }
}

/// Shared raycaster resources living in `callback_resources`.
pub struct VolumeRaycasterResources {
    pipeline: VolumePipeline,
    scenes: HashMap<VolumeId, VolumeGpu>,
}

impl VolumeRaycasterResources {
    fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        Self {
            pipeline: VolumePipeline::new(device, target_format),
            scenes: HashMap::new(),
        }
    }
}

/// Install the raycaster pipeline into `render_state` if absent. Idempotent.
pub fn install_volume_raycaster(render_state: &RenderState) {
    let mut renderer = render_state.renderer.write();
    if renderer
        .callback_resources
        .get::<VolumeRaycasterResources>()
        .is_some()
    {
        return;
    }
    let resources = VolumeRaycasterResources::new(&render_state.device, render_state.target_format);
    renderer.callback_resources.insert(resources);
}

/// Upload view `id`'s volume as a `(depth, height, width)` RGBA8 texture
/// (row-major, straight alpha). Requires [`install_volume_raycaster`] first.
pub fn set_volume_raycaster(
    render_state: &RenderState,
    id: VolumeId,
    rgba: &[u8],
    depth: usize,
    height: usize,
    width: usize,
) {
    // Guard the wgpu texture invariants: non-empty extent and exactly one RGBA8
    // texel per voxel. An invalid call is a no-op (keeping any prior upload)
    // rather than a wgpu validation panic; debug builds trip an assert so the
    // caller bug is caught.
    let expected = depth.saturating_mul(height).saturating_mul(width) * 4;
    if depth == 0 || height == 0 || width == 0 || rgba.len() != expected {
        debug_assert!(
            false,
            "set_volume_raycaster: bad volume ({depth}x{height}x{width}, {} bytes, expected {expected})",
            rgba.len()
        );
        return;
    }
    let mut renderer = render_state.renderer.write();
    let res: &mut VolumeRaycasterResources = renderer
        .callback_resources
        .get_mut()
        .expect("VolumeRaycasterResources not installed — call install_volume_raycaster() first");
    let VolumeRaycasterResources { pipeline, scenes } = res;
    let scene = scenes
        .entry(id)
        .or_insert_with(|| VolumeGpu::new(&render_state.device));
    scene.upload(
        &render_state.device,
        &render_state.queue,
        pipeline,
        rgba,
        (depth, height, width),
    );
}

/// Drop view `id`'s GPU resources (texture, bind group, uniform buffer),
/// freeing their VRAM. A no-op if `id` was never uploaded or the pipeline is not
/// installed. Call when a raycaster view goes away so its texture does not linger
/// in `callback_resources` for the app's lifetime.
pub fn remove_volume_raycaster(render_state: &RenderState, id: VolumeId) {
    let mut renderer = render_state.renderer.write();
    if let Some(res) = renderer
        .callback_resources
        .get_mut::<VolumeRaycasterResources>()
    {
        res.scenes.remove(&id);
    }
}

/// egui paint callback: write this frame's uniforms in `prepare`, ray-march in
/// `paint`.
struct VolumeCallback {
    frame: VolumeFrame,
}

impl egui_wgpu::CallbackTrait for VolumeCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(res) = resources.get::<VolumeRaycasterResources>()
            && let Some(scene) = res.scenes.get(&self.frame.id)
        {
            scene.write_uniforms(queue, &self.frame);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(res) = resources.get::<VolumeRaycasterResources>()
            && let Some(scene) = res.scenes.get(&self.frame.id)
            && let Some(bind_group) = &scene.bind_group
        {
            render_pass.set_pipeline(&res.pipeline.pipeline);
            render_pass.set_bind_group(0, bind_group, &[]);
            render_pass.draw(0..3, 0..1);
        }
    }
}

/// Register the raycaster paint callback for `frame` over `rect`. A no-op on
/// screen until [`set_volume_raycaster`] has uploaded a volume for `frame.id`.
///
/// **Invariant: paint each [`VolumeId`] at most once per frame.** The per-id
/// uniform buffer is shared between `prepare` and `paint`, so if two callbacks
/// with the same id run in one frame, `prepare` writes that buffer twice and
/// both `paint`s read the value from whichever prepared last — both views render
/// with the last one's camera. Give every on-screen view its own id (the
/// [`crate::VolumeRaycaster`] widget already binds one id per instance).
pub fn paint_volume_raycaster(ui: &mut egui::Ui, rect: egui::Rect, frame: VolumeFrame) {
    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
        rect,
        VolumeCallback { frame },
    ));
}

#[cfg(test)]
mod tests {
    use super::premultiply_rgba;

    #[test]
    fn premultiply_zeroes_transparent_and_keeps_opaque() {
        // Opaque red is unchanged; fully transparent becomes (0,0,0,0) so the
        // linear filter cannot bleed its colour; half-alpha halves rgb.
        let src = [
            255, 0, 0, 255, // opaque red
            200, 100, 50, 0, // transparent → all zero
            255, 255, 255, 128, // half coverage → ~half
        ];
        let out = premultiply_rgba(&src);
        assert_eq!(&out[0..4], &[255, 0, 0, 255]);
        assert_eq!(&out[4..8], &[0, 0, 0, 0]);
        assert_eq!(out[11], 128); // alpha preserved
        assert_eq!(out[8], 128); // 255*128/255 rounded
    }

    #[test]
    fn premultiply_drops_trailing_partial_pixel() {
        let src = [255, 0, 0, 255, 1, 2]; // 6 bytes: one pixel + 2 stragglers
        assert_eq!(premultiply_rgba(&src).len(), 4);
    }
}
