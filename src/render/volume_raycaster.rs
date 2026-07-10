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
use std::sync::Mutex;

use egui_wgpu::{RenderState, wgpu};
use half::f16;

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

/// Premultiply a straight-alpha RGBA8 buffer into `Rgba16Float` texels (`rgb *=
/// a`, both normalised to `[0, 1]`), so the linear sampler interpolates
/// premultiplied colour: a transparent voxel becomes `(0,0,0,0)` and
/// interpolating it against an opaque colour keeps the hue instead of dragging
/// it toward black. A trailing partial pixel (len not a multiple of 4) is
/// dropped.
///
/// The shader divides the sampled colour back out by the sampled coverage to
/// recover the straight RGB (`gain = sa_c / s.a`), so the *product* `rgb · a`
/// must carry the straight RGB to the input's own 8-bit precision. An 8-bit
/// product cannot: at coverage `a` it quantises the recoverable straight value
/// to steps of `1/a`, which for the low coverages a ray-march is built on
/// (`a = 3/255`) is three colour levels total. `f16` holds every `c·a/255²` to
/// ~0.05% relative, so the recovered RGB is exact to well under one 8-bit step
/// at every coverage down to 1/255.
fn premultiply_rgba(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4)
        .flat_map(|px| {
            let a = f32::from(px[3]) / 255.0;
            let texel = [
                f16::from_f32(f32::from(px[0]) / 255.0 * a),
                f16::from_f32(f32::from(px[1]) / 255.0 * a),
                f16::from_f32(f32::from(px[2]) / 255.0 * a),
                f16::from_f32(a),
            ];
            texel.map(f16::to_le_bytes)
        })
        .flatten()
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

/// Per-view GPU state: the uploaded 3D texture and the world-space box it
/// occupies. Uniforms and the bind group are NOT stored here — each paint
/// callback builds its own in `prepare` (see [`VolumeCallback`]), so two
/// callbacks sharing a [`VolumeId`] in one frame no longer clobber a shared
/// uniform buffer.
struct VolumeGpu {
    texture: Option<wgpu::Texture>,
    vol_min: [f32; 3],
    vol_max: [f32; 3],
    /// Live claims on this id — see [`acquire_volume_raycaster`]. The entry, and
    /// with it the texture, exists exactly while this is non-zero.
    claims: u32,
}

impl VolumeGpu {
    fn new() -> Self {
        Self {
            texture: None,
            vol_min: [-0.5, -0.5, -0.5],
            vol_max: [0.5, 0.5, 0.5],
            claims: 0,
        }
    }

    /// Upload a `(depth, height, width)` RGBA8 volume (row-major, straight
    /// alpha), (re)building the 3D texture. The straight alpha is premultiplied
    /// into `Rgba16Float` before upload so the linear sampler interpolates
    /// premultiplied colour (no dark fringe at colour/transparent boundaries)
    /// while the product still carries the straight RGB at low coverage — see
    /// [`premultiply_rgba`]. Costs twice the VRAM of an 8-bit texture.
    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
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
            format: wgpu::TextureFormat::Rgba16Float,
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
                bytes_per_row: Some(w * 8), // Rgba16Float
                rows_per_image: Some(h),
            },
            extent,
        );
        self.texture = Some(texture);
        (self.vol_min, self.vol_max) = volume_bounds(depth, height, width);
    }

    /// Build a bind group for one frame: a fresh uniform buffer holding this
    /// frame's camera and this view's box, plus the view's texture and the shared
    /// sampler. `None` until a volume has been uploaded. Called once per paint in
    /// `prepare`, so each callback owns its uniforms rather than sharing one
    /// per-id buffer.
    fn frame_bind_group(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &VolumePipeline,
        frame: &VolumeFrame,
    ) -> Option<wgpu::BindGroup> {
        let texture = self.texture.as_ref()?;
        let u = VolumeUniforms {
            inv_mvp: frame.inv_mvp,
            vol_min: [self.vol_min[0], self.vol_min[1], self.vol_min[2], 0.0],
            vol_max: [self.vol_max[0], self.vol_max[1], self.vol_max[2], 0.0],
            params: [frame.params[0], frame.params[1], frame.params[2], 0.0],
        };
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rsplot volume raycaster uniforms"),
            size: std::mem::size_of::<VolumeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform_buf, 0, bytemuck::bytes_of(&u));
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D3),
            ..Default::default()
        });
        // The bind group holds strong refs to the uniform buffer and view, so
        // both live as long as the callback keeps the bind group.
        Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rsplot volume raycaster bind group"),
            layout: &pipeline.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buf.as_entire_binding(),
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
        }))
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

/// Take a claim on view `id`'s GPU entry, creating it if this is the first.
///
/// The entry — and the VRAM of the texture it holds — lives exactly as long as
/// its claims: it is created here and dropped by the
/// [`release_volume_raycaster`] that takes the count back to zero. Nothing else
/// inserts or removes an entry, so a view can never free a texture another view
/// with the same [`VolumeId`] is still rendering. Requires
/// [`install_volume_raycaster`] first.
pub fn acquire_volume_raycaster(render_state: &RenderState, id: VolumeId) {
    let mut renderer = render_state.renderer.write();
    let res: &mut VolumeRaycasterResources = renderer
        .callback_resources
        .get_mut()
        .expect("VolumeRaycasterResources not installed — call install_volume_raycaster() first");
    res.scenes.entry(id).or_insert_with(VolumeGpu::new).claims += 1;
}

/// Drop one claim on view `id`, freeing its 3D texture once the last claim goes.
/// A no-op if `id` holds no claim or the pipeline is not installed.
pub fn release_volume_raycaster(render_state: &RenderState, id: VolumeId) {
    let mut renderer = render_state.renderer.write();
    let Some(res) = renderer
        .callback_resources
        .get_mut::<VolumeRaycasterResources>()
    else {
        return;
    };
    let Some(scene) = res.scenes.get_mut(&id) else {
        return;
    };
    scene.claims = scene.claims.saturating_sub(1);
    if scene.claims == 0 {
        res.scenes.remove(&id);
    }
}

/// Upload view `id`'s volume as a `(depth, height, width)` RGBA8 texture
/// (row-major, straight alpha). Requires a claim from
/// [`acquire_volume_raycaster`]; without one the call is a no-op, so an upload
/// can never resurrect an entry whose last holder released it.
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
    let Some(scene) = res.scenes.get_mut(&id) else {
        debug_assert!(false, "set_volume_raycaster: id {id} holds no claim");
        return;
    };
    scene.upload(
        &render_state.device,
        &render_state.queue,
        rgba,
        (depth, height, width),
    );
}

/// egui paint callback. `prepare` builds this frame's own bind group (fresh
/// uniform buffer for the camera + the view's texture) and stashes it in `ready`;
/// `paint` ray-marches with it. Because the bind group is per-callback rather
/// than a shared per-id buffer, two callbacks with the same [`VolumeId`] in one
/// frame each render with their own camera.
struct VolumeCallback {
    frame: VolumeFrame,
    /// Built in `prepare`, consumed in `paint`. `Mutex` only to satisfy the
    /// `CallbackTrait: Send + Sync` bound — prepare and paint run in sequence on
    /// the render thread, never concurrently.
    ready: Mutex<Option<wgpu::BindGroup>>,
}

impl egui_wgpu::CallbackTrait for VolumeCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(res) = resources.get::<VolumeRaycasterResources>()
            && let Some(scene) = res.scenes.get(&self.frame.id)
        {
            let bg = scene.frame_bind_group(device, queue, &res.pipeline, &self.frame);
            *self.ready.lock().unwrap() = bg;
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        // Hold the guard through the draw so the bind group (and the uniform
        // buffer it owns) stay alive while the pass records.
        let ready = self.ready.lock().unwrap();
        if let Some(res) = resources.get::<VolumeRaycasterResources>()
            && let Some(bind_group) = ready.as_ref()
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
/// Each callback builds its own uniform buffer + bind group in `prepare` (keyed
/// off `frame`, not shared per id), so painting the same [`VolumeId`] twice in
/// one frame — two panels of the same volume from different cameras — renders
/// each with its own viewpoint.
pub fn paint_volume_raycaster(ui: &mut egui::Ui, rect: egui::Rect, frame: VolumeFrame) {
    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
        rect,
        VolumeCallback {
            frame,
            ready: Mutex::new(None),
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::premultiply_rgba;
    use half::f16;

    /// Decode one `Rgba16Float` texel back to `[f32; 4]`, as the sampler does.
    fn texel(out: &[u8], i: usize) -> [f32; 4] {
        std::array::from_fn(|c| {
            let o = i * 8 + c * 2;
            f16::from_le_bytes([out[o], out[o + 1]]).to_f32()
        })
    }

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
        assert_eq!(texel(&out, 0), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(texel(&out, 1), [0.0; 4]);
        let half = texel(&out, 2);
        assert!((half[3] - 128.0 / 255.0).abs() < 1e-3, "alpha preserved");
        assert!((half[0] - half[3]).abs() < 1e-3, "white × a == a");
    }

    /// The shader recovers the straight RGB as `s.rgb / s.a`. Boundary: the
    /// lowest coverages, where an 8-bit premultiplied product would quantise
    /// that quotient to steps of `1/a` — at `a = 3` the recovered channel could
    /// only be 0, 1/3, 2/3 or 1, so `200/255 = 0.784` would read as `0.667`.
    #[test]
    fn premultiply_recovers_the_straight_rgb_at_the_lowest_coverages() {
        for a in [1u8, 2, 3, 4, 8, 128, 255] {
            for c in [1u8, 37, 128, 200, 254, 255] {
                let out = premultiply_rgba(&[c, 0, 0, a]);
                let t = texel(&out, 0);
                let recovered = t[0] / t[3];
                let want = f32::from(c) / 255.0;
                assert!(
                    (recovered - want).abs() < 0.5 / 255.0,
                    "a={a} c={c}: recovered {recovered}, want {want}"
                );
            }
        }
    }

    #[test]
    fn premultiply_drops_trailing_partial_pixel() {
        let src = [255, 0, 0, 255, 1, 2]; // 6 bytes: one pixel + 2 stragglers
        assert_eq!(premultiply_rgba(&src).len(), 8); // one Rgba16Float texel
    }
}
