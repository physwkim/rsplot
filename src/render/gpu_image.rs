//! GPU-side image: the shared image pipeline and a single uploaded image.
//!
//! [`ImageData`] is the CPU spec (mirrors silx `addImage`: data, origin/scale,
//! colormap, alpha). [`ImagePipeline`] holds the pipeline and samplers shared
//! across images. [`GpuImage`] owns one image's textures/uniform/bind group and
//! persists across frames in `WgpuResources`.
//!
//! An image is split into a grid of tiles no larger than the device's
//! `max_texture_dimension_2d`, so images exceeding the single-texture limit
//! display correctly; a small image is a single tile. Each tile is one quad
//! with its own scalar texture and data-space rect, sharing the colormap LUT
//! and samplers; the tiles abut seamlessly because each rect maps its texture
//! onto exactly its pixel sub-grid. The initial upload happens at creation; a
//! sub-region can be re-uploaded in place via [`GpuImage::update_region`]
//! (dirty update), which routes the region to the overlapping tiles. Autoscale
//! is a later step (`doc/design.md` §6·§7·§13 D2).

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

/// Uniform block for the image shader. Field order keeps `repr(C)` offsets
/// std140-aligned: mat4 @0, vec4 @64, vec2 @80, then scalars f32 @88/92/96/100,
/// u32 @104; padded to 112. Matches `Params` in `image.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ImageParams {
    ortho: [[f32; 4]; 4],
    rect: [f32; 4],
    /// 1.0 if that axis is log10, else 0.0 (x, y).
    axis_log: [f32; 2],
    alpha: f32,
    /// Normalization transform applied to `vmin` (the LUT-coordinate origin).
    cmap_min: f32,
    /// `1 / (norm(vmax) - norm(vmin))`, or 0 for a degenerate range.
    cmap_one_over_range: f32,
    /// Gamma exponent (used only when `norm == 3`).
    gamma: f32,
    /// Normalization code; see [`Normalization::code`](crate::core::colormap::Normalization).
    norm: u32,
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

/// Split a `width × height` image into a grid of tiles no larger than `max_dim`
/// on either side, in row-major (left→right, bottom→top) order. Each entry is a
/// tile's pixel bounds `(x0, y0, w, h)`. A small image yields a single tile.
fn tile_bounds(width: u32, height: u32, max_dim: u32) -> Vec<(u32, u32, u32, u32)> {
    let max_dim = max_dim.max(1);
    let mut tiles = Vec::new();
    let mut y0 = 0;
    while y0 < height {
        let h = (height - y0).min(max_dim);
        let mut x0 = 0;
        while x0 < width {
            let w = (width - x0).min(max_dim);
            tiles.push((x0, y0, w, h));
            x0 += w;
        }
        y0 += h;
    }
    tiles
}

/// Copy the `w × h` sub-block at `(x0, y0)` out of a row-major image of stride
/// `full_width`, producing a tightly packed row-major `w × h` buffer.
fn extract_subgrid(data: &[f32], full_width: u32, x0: u32, y0: u32, w: u32, h: u32) -> Vec<f32> {
    let mut out = Vec::with_capacity((w as usize) * (h as usize));
    for row in 0..h {
        let src = ((y0 + row) as usize) * (full_width as usize) + x0 as usize;
        out.extend_from_slice(&data[src..src + w as usize]);
    }
    out
}

/// One tile of a (possibly split) image: its own scalar texture, params, bind
/// group, data-space rect, and pixel bounds within the full image (the bounds
/// route [`GpuImage::update_region`] writes to the tiles they overlap).
struct ImageTile {
    bind_group: wgpu::BindGroup,
    params: wgpu::Buffer,
    data_texture: wgpu::Texture,
    rect: [f32; 4],
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
}

/// One uploaded image's GPU resources, persisting across frames. The image is
/// stored as one or more [`ImageTile`]s so it can exceed the single-texture
/// dimension limit.
pub struct GpuImage {
    tiles: Vec<ImageTile>,
    width: u32,
    height: u32,
    /// Resolved colormap normalization for the per-frame uniform: the bounds
    /// transformed by the normalization, plus the gamma exponent and code.
    cmap_min: f32,
    cmap_one_over_range: f32,
    gamma: f32,
    norm: u32,
    alpha: f32,
}

impl GpuImage {
    /// Upload `image` (data + LUT textures) and build per-tile bind groups. The
    /// scalar textures are `R32Float`; the LUT is a 256x1 sRGB texture shared by
    /// every tile. The image is split into tiles no larger than the device's
    /// `max_texture_dimension_2d`, so it can exceed the single-texture limit.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &ImagePipeline,
        image: &ImageData,
    ) -> Self {
        // LUT texture is shared across tiles (built once).
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
        let lut_view = lut_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let (ox, oy) = image.origin;
        let (sx, sy) = image.scale;
        let max_dim = device.limits().max_texture_dimension_2d;

        let tiles = tile_bounds(image.width, image.height, max_dim)
            .into_iter()
            .map(|(x0, y0, w, h)| {
                let tile_size = wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                };
                let data_texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("egui-silx image tile"),
                    size: tile_size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R32Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let sub = extract_subgrid(&image.data, image.width, x0, y0, w, h);
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &data_texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytemuck::cast_slice(&sub),
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4 * w),
                        rows_per_image: Some(h),
                    },
                    tile_size,
                );
                let data_view = data_texture.create_view(&wgpu::TextureViewDescriptor::default());

                let params = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("egui-silx image tile params"),
                    size: std::mem::size_of::<ImageParams>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("egui-silx image tile bg"),
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

                // The tile's data-space rect: its pixel sub-grid mapped through
                // origin + scale, so adjacent tiles abut exactly.
                let rect = [
                    (ox + sx * x0 as f64) as f32,
                    (oy + sy * y0 as f64) as f32,
                    (ox + sx * (x0 + w) as f64) as f32,
                    (oy + sy * (y0 + h) as f64) as f32,
                ];
                ImageTile {
                    bind_group,
                    params,
                    data_texture,
                    rect,
                    x0,
                    y0,
                    w,
                    h,
                }
            })
            .collect();

        let (cmap_min, cmap_one_over_range) = image.colormap.norm_bounds();
        let gpu = Self {
            tiles,
            width: image.width,
            height: image.height,
            cmap_min,
            cmap_one_over_range,
            gamma: image.colormap.gamma,
            norm: image.colormap.normalization.code(),
            alpha: image.alpha,
        };
        // Seed each tile's uniform; the per-frame transform overwrites `ortho`.
        gpu.write_uniforms(queue, IDENTITY, [0.0, 0.0]);
        gpu
    }

    /// Re-upload a `w × h` sub-region at `(x0, y0)` of the image in place (dirty
    /// update), routing it to the tiles it overlaps without recreating any GPU
    /// resources. `data` is row-major, length `w * h`. Row `y0` is the same row
    /// the shader samples, so increasing `y0` moves the region upward in the
    /// displayed image (origin lower-left). Panics if the region exceeds the
    /// image bounds.
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
        for tile in &self.tiles {
            // Intersect the region with this tile, in full-image pixel coords.
            let ix0 = x0.max(tile.x0);
            let iy0 = y0.max(tile.y0);
            let ix1 = (x0 + w).min(tile.x0 + tile.w);
            let iy1 = (y0 + h).min(tile.y0 + tile.h);
            if ix1 <= ix0 || iy1 <= iy0 {
                continue; // no overlap with this tile
            }
            let ow = ix1 - ix0;
            let oh = iy1 - iy0;
            // Sub-block of the incoming region buffer (stride = region width w).
            let sub = extract_subgrid(data, w, ix0 - x0, iy0 - y0, ow, oh);
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tile.data_texture,
                    mip_level: 0,
                    // Destination is tile-local.
                    origin: wgpu::Origin3d {
                        x: ix0 - tile.x0,
                        y: iy0 - tile.y0,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&sub),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * ow),
                    rows_per_image: Some(oh),
                },
                wgpu::Extent3d {
                    width: ow,
                    height: oh,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    /// Update the per-frame data->NDC transform and axis-scale flags on every
    /// tile (and re-stamp the static fields). `axis_log` is `[x, y]` with 1.0
    /// for a log10 axis, matching the plot's transform.
    pub(crate) fn write_uniforms(
        &self,
        queue: &wgpu::Queue,
        ortho: [[f32; 4]; 4],
        axis_log: [f32; 2],
    ) {
        for tile in &self.tiles {
            let params = ImageParams {
                ortho,
                rect: tile.rect,
                axis_log,
                alpha: self.alpha,
                cmap_min: self.cmap_min,
                cmap_one_over_range: self.cmap_one_over_range,
                gamma: self.gamma,
                norm: self.norm,
                _pad: 0.0,
            };
            queue.write_buffer(&tile.params, 0, bytemuck::bytes_of(&params));
        }
    }

    pub(crate) fn draw(&self, render_pass: &mut wgpu::RenderPass<'_>, pipeline: &ImagePipeline) {
        render_pass.set_pipeline(&pipeline.pipeline);
        for tile in &self.tiles {
            render_pass.set_bind_group(0, &tile.bind_group, &[]);
            render_pass.draw(0..6, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_image_is_a_single_tile() {
        let tiles = tile_bounds(100, 60, 8192);
        assert_eq!(tiles, vec![(0, 0, 100, 60)]);
    }

    #[test]
    fn oversize_image_splits_into_grid_covering_every_pixel() {
        // 5 × 3 image with max_dim 2: columns {0..2, 2..4, 4..5}, rows
        // {0..2, 2..3} → 3 × 2 = 6 tiles, with remainder tiles on the edges.
        let w = 5;
        let h = 3;
        let tiles = tile_bounds(w, h, 2);
        assert_eq!(tiles.len(), 6);
        // Bounds stay inside the image and the areas sum to the whole image.
        let mut area = 0u32;
        for (x0, y0, tw, th) in &tiles {
            assert!(x0 + tw <= w && y0 + th <= h, "tile out of bounds");
            assert!(*tw <= 2 && *th <= 2, "tile exceeds max_dim");
            area += tw * th;
        }
        assert_eq!(area, w * h, "tiles must cover every pixel exactly once");
    }

    #[test]
    fn extract_subgrid_copies_the_right_block() {
        // 4-wide image; pull the 2×2 block at (1, 1).
        #[rustfmt::skip]
        let data = vec![
            0.0, 1.0, 2.0, 3.0,
            4.0, 5.0, 6.0, 7.0,
            8.0, 9.0, 10.0, 11.0,
        ];
        let sub = extract_subgrid(&data, 4, 1, 1, 2, 2);
        assert_eq!(sub, vec![5.0, 6.0, 9.0, 10.0]);
    }
}
