//! GPU-side image: the shared image pipeline and a single uploaded image.
//!
//! [`ImageData`] is the CPU spec (mirrors silx `addImage`: data, origin/scale,
//! colormap, alpha). [`ImagePipeline`] holds the pipeline and samplers shared
//! across images. [`GpuImage`] owns one image's textures/uniform/bind group and
//! persists across frames in `WgpuResources`.
//!
//! Scope: a single non-tiled image, linear colormap. The initial upload happens
//! at creation; a sub-region can be re-uploaded in place via
//! [`GpuImage::update_region`] (dirty update). Tiling for textures beyond
//! `max_texture_dimension_2d` and autoscale are later steps (`doc/design.md`
//! §6·§7).

use std::num::NonZeroU64;

use egui_wgpu::wgpu;

use crate::core::colormap::Colormap;

/// Identity ortho matrix; replaced every frame by the widget's transform.
const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// Uniform block for the image shader. Layout matches `Params` in `image.wgsl`
/// (std140-compatible: mat4 at 0, vec4 at 64, vec2 at 80, f32 at 88, total 96).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ImageParams {
    ortho: [[f32; 4]; 4],
    rect: [f32; 4],
    clim: [f32; 2],
    alpha: f32,
    _pad: f32,
}

/// A 2D scalar (or single-channel) image to display, in data coordinates.
///
/// `origin` is the data coordinate of the lower-left corner of pixel `(0, 0)`;
/// `scale` is data units per pixel. Row 0 of `data` is drawn at the bottom.
#[derive(Clone, Debug)]
pub struct ImageData {
    /// Row-major scalar values, length `width * height`.
    pub data: Vec<f32>,
    pub width: u32,
    pub height: u32,
    pub origin: (f64, f64),
    pub scale: (f64, f64),
    pub colormap: Colormap,
    pub alpha: f32,
}

impl ImageData {
    /// Build an image at origin `(0, 0)` with unit scale and full opacity.
    pub fn new(width: u32, height: u32, data: Vec<f32>, colormap: Colormap) -> Self {
        assert_eq!(
            data.len(),
            (width as usize) * (height as usize),
            "data length must equal width * height"
        );
        Self {
            data,
            width,
            height,
            origin: (0.0, 0.0),
            scale: (1.0, 1.0),
            colormap,
            alpha: 1.0,
        }
    }
}

/// The render pipeline and samplers shared by all images.
pub struct ImagePipeline {
    pub(crate) pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    data_sampler: wgpu::Sampler,
    lut_sampler: wgpu::Sampler,
}

impl ImagePipeline {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("egui-silx image"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/image.wgsl").into()),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("egui-silx image bgl"),
                entries: &[
                    // 0: params uniform (used by both stages)
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: NonZeroU64::new(
                                std::mem::size_of::<ImageParams>() as u64
                            ),
                        },
                        count: None,
                    },
                    // 1: scalar data texture (unfilterable float)
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    // 2: data sampler (non-filtering / nearest)
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                        count: None,
                    },
                    // 3: LUT texture (filterable float)
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    // 4: LUT sampler (filtering / linear)
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("egui-silx image layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("egui-silx image pipeline"),
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

        let data_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("egui-silx image data sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let lut_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("egui-silx image lut sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self {
            pipeline,
            bind_group_layout,
            data_sampler,
            lut_sampler,
        }
    }
}

/// One uploaded image's GPU resources, persisting across frames.
pub struct GpuImage {
    bind_group: wgpu::BindGroup,
    params: wgpu::Buffer,
    /// Retained so a sub-region can be re-uploaded in place (dirty update).
    data_texture: wgpu::Texture,
    width: u32,
    height: u32,
    rect: [f32; 4],
    clim: [f32; 2],
    alpha: f32,
}

impl GpuImage {
    /// Upload `image` (data + LUT textures) and build its bind group. The
    /// scalar texture is `R32Float`; the LUT is a 256x1 sRGB texture.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &ImagePipeline,
        image: &ImageData,
    ) -> Self {
        let data_size = wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        };
        let data_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("egui-silx image data"),
            size: data_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &data_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&image.data),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * image.width),
                rows_per_image: Some(image.height),
            },
            data_size,
        );

        let lut_size = wgpu::Extent3d {
            width: 256,
            height: 1,
            depth_or_array_layers: 1,
        };
        let lut_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("egui-silx image lut"),
            size: lut_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &lut_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&image.colormap.lut),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * 256),
                rows_per_image: Some(1),
            },
            lut_size,
        );

        let data_view = data_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let lut_view = lut_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("egui-silx image params"),
            size: std::mem::size_of::<ImageParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("egui-silx image bg"),
            layout: &pipeline.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&data_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&pipeline.data_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&lut_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&pipeline.lut_sampler),
                },
            ],
        });

        let (ox, oy) = image.origin;
        let (sx, sy) = image.scale;
        let rect = [
            ox as f32,
            oy as f32,
            (ox + sx * image.width as f64) as f32,
            (oy + sy * image.height as f64) as f32,
        ];
        let clim = [image.colormap.vmin as f32, image.colormap.vmax as f32];

        let gpu = Self {
            bind_group,
            params,
            data_texture,
            width: image.width,
            height: image.height,
            rect,
            clim,
            alpha: image.alpha,
        };
        // Seed the uniform; the per-frame transform overwrites `ortho`.
        gpu.write_uniforms(queue, IDENTITY);
        gpu
    }

    /// Re-upload a `w × h` sub-region at `(x0, y0)` of the scalar texture in
    /// place (dirty update), without recreating any GPU resources. `data` is
    /// row-major, length `w * h`. Row `y0` is the same row the shader samples,
    /// so increasing `y0` moves the region upward in the displayed image
    /// (origin lower-left). Panics if the region exceeds the texture bounds.
    pub(crate) fn update_region(
        &self,
        queue: &wgpu::Queue,
        x0: u32,
        y0: u32,
        w: u32,
        h: u32,
        data: &[f32],
    ) {
        assert_eq!(
            data.len(),
            (w as usize) * (h as usize),
            "data length must equal w * h"
        );
        assert!(
            x0 + w <= self.width && y0 + h <= self.height,
            "region out of bounds"
        );
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.data_texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: x0, y: y0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(data),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Update the per-frame data->NDC transform (and re-stamp the static fields).
    pub(crate) fn write_uniforms(&self, queue: &wgpu::Queue, ortho: [[f32; 4]; 4]) {
        let params = ImageParams {
            ortho,
            rect: self.rect,
            clim: self.clim,
            alpha: self.alpha,
            _pad: 0.0,
        };
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
    }

    pub(crate) fn draw(&self, render_pass: &mut wgpu::RenderPass<'_>, pipeline: &ImagePipeline) {
        render_pass.set_pipeline(&pipeline.pipeline);
        render_pass.set_bind_group(0, &self.bind_group, &[]);
        render_pass.draw(0..6, 0..1);
    }
}
