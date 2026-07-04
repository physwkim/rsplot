//! GPU-side image: the shared image pipeline and a single uploaded image.
//!
//! [`ImageData`] is the CPU spec (mirrors silx `addImage`: pixels, origin/scale,
//! alpha). Its [`ImagePixels`] is either a scalar field with a colormap or a
//! direct RGBA image (the colormap is meaningless for RGBA, so the sum type
//! makes "RGBA + colormap" unrepresentable). [`ImagePipeline`] holds the scalar
//! (colormapped) and RGBA (direct) pipelines plus the shared samplers.
//! [`GpuImage`] owns one image's textures/uniform/bind group and persists across
//! frames in `WgpuResources`.
//!
//! An image is split into a grid of tiles no larger than the device's
//! `max_texture_dimension_2d`, so images exceeding the single-texture limit
//! display correctly; a small image is a single tile. Each tile is one quad
//! with its own scalar texture and data-space rect, sharing the colormap LUT
//! and samplers; the tiles abut seamlessly because each rect maps its texture
//! onto exactly its pixel sub-grid. The initial upload happens at creation; a
//! sub-region can be re-uploaded in place via `GpuImage::update_region`
//! (dirty update), which routes the region to the overlapping tiles. Autoscale
//! is a later step (`doc/design.md` §6·§7·§13 D2).

use std::num::NonZeroU64;

use egui_wgpu::wgpu;

use crate::core::colormap::Colormap;

/// How an image's data is resampled to screen pixels (silx image
/// `interpolation`). [`Nearest`](InterpolationMode::Nearest) is the silx default
/// (its matplotlib backend hardcodes `interpolation="nearest"` and the GL data
/// texture uses `GL_NEAREST`); [`Linear`](InterpolationMode::Linear) bilinearly
/// interpolates the underlying values.
///
/// For a scalar image the interpolation happens on the SCALAR data and the
/// colormap is applied afterwards, matching silx (the GL backend filters the
/// data texture before the colormap lookup).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InterpolationMode {
    /// Sample the texel whose cell contains the pixel centre (silx default).
    #[default]
    Nearest,
    /// Bilinearly interpolate the four neighbouring texels.
    Linear,
}

impl InterpolationMode {
    /// Shader interpolation code (must match the branch in `image.wgsl`):
    /// nearest 0, linear 1.
    fn code(self) -> u32 {
        match self {
            InterpolationMode::Nearest => 0,
            InterpolationMode::Linear => 1,
        }
    }
}

/// How a scalar image is reduced when several data pixels map to one screen
/// pixel (silx `ImageDataAggregated.Aggregation`). The aggregator ignores NaNs,
/// matching silx (`numpy.nanmax` / `numpy.nanmean` / `numpy.nanmin`); a block
/// whose values are all NaN aggregates to NaN.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AggregationMode {
    /// Display the data as is — no block downsampling (silx default).
    #[default]
    None,
    /// Reduce each block to its maximum (NaNs ignored).
    Max,
    /// Reduce each block to its mean (NaNs ignored).
    Mean,
    /// Reduce each block to its minimum (NaNs ignored).
    Min,
}

/// Block-downsample a row-major `width × height` scalar field by integer factors
/// `block_x` (columns) and `block_y` (rows), reducing each `block_y × block_x`
/// block with `mode`. Mirrors silx `ImageDataAggregated._addBackendRenderer`:
///
/// ```text
/// data[:(height // lody) * lody, :(width // lodx) * lodx]
///     .reshape(height // lody, lody, width // lodx, lodx)
/// aggregator(..., axis=(1, 3))   # nanmax / nanmean / nanmin
/// ```
///
/// i.e. the remainder rows/columns that do not fill a whole block are dropped,
/// the output is `(width // block_x) × (height // block_y)`, and the aggregation
/// ignores NaNs (an all-NaN block yields NaN). Returns the new data with its new
/// `(width, height)`; for [`AggregationMode::None`], or a factor `<= 1` on both
/// axes, the data is returned unchanged.
pub fn aggregate_blocks(
    data: &[f32],
    width: u32,
    height: u32,
    block_x: u32,
    block_y: u32,
    mode: AggregationMode,
) -> (Vec<f32>, u32, u32) {
    let bx = block_x.max(1);
    let by = block_y.max(1);
    if mode == AggregationMode::None || (bx == 1 && by == 1) {
        return (data.to_vec(), width, height);
    }
    let out_w = width / bx;
    let out_h = height / by;
    let mut out = Vec::with_capacity((out_w as usize) * (out_h as usize));
    for oy in 0..out_h {
        for ox in 0..out_w {
            // Reduce the block, ignoring NaNs. `acc` is None until a finite
            // value is seen; all-NaN blocks stay None and emit NaN, matching
            // numpy.nan{max,mean,min}.
            let mut acc: Option<f32> = None;
            let mut count: u32 = 0; // finite-value count, for the mean
            for j in 0..by {
                let row = (oy * by + j) as usize;
                let base = row * (width as usize) + (ox * bx) as usize;
                for i in 0..bx {
                    let v = data[base + i as usize];
                    if v.is_nan() {
                        continue;
                    }
                    count += 1;
                    acc = Some(match (acc, mode) {
                        (None, _) => v,
                        (Some(a), AggregationMode::Max) => a.max(v),
                        (Some(a), AggregationMode::Min) => a.min(v),
                        (Some(a), AggregationMode::Mean) => a + v,
                        // None handled above; unreachable for the early-return.
                        (Some(a), AggregationMode::None) => a,
                    });
                }
            }
            let value = match acc {
                None => f32::NAN, // all-NaN block
                Some(a) if mode == AggregationMode::Mean => a / count as f32,
                Some(a) => a,
            };
            out.push(value);
        }
    }
    (out, out_w, out_h)
}

/// Identity ortho matrix; replaced every frame by the widget's transform.
const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// Convert an sRGB-encoded `[u8; 4]` color (the colormap's `nan_color`) to
/// linear-space `[f32; 4]`. The scalar-image LUT is an `Rgba8UnormSrgb` texture,
/// so sampling it returns linear RGB; converting the nan_color the same way lets
/// the shader composite it identically to the colormapped path. The RGB channels
/// use the standard sRGB transfer function; alpha is linear (not gamma-encoded).
fn srgb_u8_to_linear_rgba(c: [u8; 4]) -> [f32; 4] {
    let to_linear = |b: u8| {
        let s = b as f32 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    [
        to_linear(c[0]),
        to_linear(c[1]),
        to_linear(c[2]),
        c[3] as f32 / 255.0,
    ]
}

/// Uniform block for the image shader. Field order keeps `repr(C)` offsets
/// std140-aligned: mat4 @0, vec4 @64, vec2 @80, then scalars f32 @88/92/96/100,
/// u32 @104/108/112, explicit pad @116, then vec4 @128 (16-aligned); total 144.
/// Matches `Params` in `image.wgsl` (where WGSL auto-pads the vec4 to 128).
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
    /// Interpolation code; see [`InterpolationMode::code`]: nearest 0, linear 1.
    interp: u32,
    /// 1 if a per-pixel alpha map is bound at binding 5 (silx
    /// `ImageData.getAlphaData`), else 0 — when 0 the shader skips the alpha
    /// texture sample and a shared 1×1 dummy is bound for layout validity.
    has_alpha_map: u32,
    /// Padding so `nan_color` (a vec4) lands at the 16-aligned offset 128, where
    /// WGSL auto-places it after the three trailing u32s.
    _pad: [u32; 3],
    /// Linear RGBA used for non-finite (NaN / +/-inf) samples (silx
    /// `Colormap.nan_color`). vec4 at offset 128, 16-byte aligned.
    nan_color: [f32; 4],
}

/// Uniform block for the RGBA (direct) image shader: just the transform, rect,
/// axis-scale flags, and global alpha — no colormap fields. `repr(C)` offsets
/// stay std140-aligned: mat4 @0, vec4 @64, vec2 @80, f32 @88; padded to 96.
/// Matches `Params` in `image_rgba.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ImageRgbaParams {
    ortho: [[f32; 4]; 4],
    rect: [f32; 4],
    /// 1.0 if that axis is log10, else 0.0 (x, y).
    axis_log: [f32; 2],
    alpha: f32,
    _pad: f32,
}

/// The pixel payload of an [`ImageData`]: either a scalar field that the shader
/// colormaps, or a direct RGBA image displayed without a colormap (silx accepts
/// both in `addImage`). Making this a sum type keeps "RGBA + colormap" — an
/// illegal combination — unrepresentable.
#[derive(Clone, Debug)]
pub enum ImagePixels {
    /// Row-major scalar values (length `width * height`) mapped through
    /// `colormap` under its [`Normalization`](crate::core::colormap::Normalization).
    /// The colormap is boxed because its 256-entry LUT dwarfs the RGBA variant.
    Scalar {
        data: Vec<f32>,
        colormap: Box<Colormap>,
    },
    /// Row-major sRGB RGBA pixels (length `width * height`), displayed directly.
    Rgba { data: Vec<[u8; 4]> },
}

/// A 2D image to display, in data coordinates.
///
/// `origin` is the data coordinate of the lower-left corner of pixel `(0, 0)`;
/// `scale` is data units per pixel. Row 0 of the pixels is drawn at the bottom.
#[derive(Clone, Debug)]
pub struct ImageData {
    pub pixels: ImagePixels,
    pub width: u32,
    pub height: u32,
    pub origin: (f64, f64),
    pub scale: (f64, f64),
    pub alpha: f32,
    /// How the data is resampled to screen pixels (silx `interpolation`),
    /// defaulting to [`Nearest`](InterpolationMode::Nearest).
    pub interpolation: InterpolationMode,
    /// Optional per-pixel alpha map (silx `ImageData.setAlphaData`), row-major
    /// and length `width * height`, each in `[0, 1]`. Multiplied into the global
    /// [`alpha`](Self::alpha) per pixel, so a value of `0` makes that pixel fully
    /// transparent (the layers below show through) and `1` leaves it at the
    /// global opacity. `None` (the default) means a uniform global alpha — every
    /// pixel renders identically to before, with the per-pixel multiply gated off
    /// in the shader. Only meaningful for a [`ImagePixels::Scalar`] image (silx
    /// exposes alpha data only on the colormapped `ImageData`); ignored for RGBA.
    pub alpha_map: Option<Vec<f32>>,
}

impl ImageData {
    /// Build a colormapped scalar image at origin `(0, 0)` with unit scale and
    /// full opacity. `data` is row-major, length `width * height`.
    pub fn new(width: u32, height: u32, data: Vec<f32>, colormap: Colormap) -> Self {
        assert_eq!(
            data.len(),
            (width as usize) * (height as usize),
            "data length must equal width * height"
        );
        Self {
            pixels: ImagePixels::Scalar {
                data,
                colormap: Box::new(colormap),
            },
            width,
            height,
            origin: (0.0, 0.0),
            scale: (1.0, 1.0),
            alpha: 1.0,
            interpolation: InterpolationMode::default(),
            alpha_map: None,
        }
    }

    /// Build a direct RGBA image (no colormap) at origin `(0, 0)` with unit
    /// scale and full opacity. `data` is row-major sRGB RGBA, length
    /// `width * height` (silx `addImage` with an RGBA array).
    pub fn rgba(width: u32, height: u32, data: Vec<[u8; 4]>) -> Self {
        assert_eq!(
            data.len(),
            (width as usize) * (height as usize),
            "data length must equal width * height"
        );
        Self {
            pixels: ImagePixels::Rgba { data },
            width,
            height,
            origin: (0.0, 0.0),
            scale: (1.0, 1.0),
            alpha: 1.0,
            interpolation: InterpolationMode::default(),
            alpha_map: None,
        }
    }

    /// The colormap for a scalar image, or `None` for an RGBA image — used to
    /// mirror the image's colormap onto the plot's colorbar.
    pub fn colormap(&self) -> Option<&Colormap> {
        match &self.pixels {
            ImagePixels::Scalar { colormap, .. } => Some(colormap.as_ref()),
            ImagePixels::Rgba { .. } => None,
        }
    }

    /// Set the data-to-screen interpolation (silx `interpolation`).
    pub fn with_interpolation(mut self, interpolation: InterpolationMode) -> Self {
        self.interpolation = interpolation;
        self
    }

    /// Set the per-pixel alpha map (silx `ImageData.setAlphaData`); see
    /// [`alpha_map`](Self::alpha_map). Panics if `alpha_map.len()` does not equal
    /// `width * height`. Has no effect on an RGBA image.
    pub fn with_alpha_map(mut self, alpha_map: Vec<f32>) -> Self {
        assert_eq!(
            alpha_map.len(),
            (self.width as usize) * (self.height as usize),
            "alpha_map length must equal width * height"
        );
        self.alpha_map = Some(alpha_map);
        self
    }
}

/// The render pipelines and samplers shared by all images: the scalar
/// (colormapped) pipeline and the RGBA (direct) pipeline. The scalar layout is
/// uniform + data texture + data sampler + LUT texture + LUT sampler; the RGBA
/// layout is its own minimal uniform + RGBA texture + sampler (no LUT).
pub struct ImagePipeline {
    pub(crate) pipeline: wgpu::RenderPipeline,
    pub(crate) rgba_pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    rgba_bind_group_layout: wgpu::BindGroupLayout,
    data_sampler: wgpu::Sampler,
    /// NEAREST sampler for the 256-entry colormap LUT lookup (silx GL_NEAREST,
    /// `GLPlotImage.py:338-347`): the LUT holds discrete registered colors, so
    /// sampling must snap to the nearest texel, not interpolate between entries.
    lut_sampler: wgpu::Sampler,
    /// LINEAR sampler for the *direct RGBA* image path's bilinear interpolation
    /// (`Rgba8UnormSrgb` is hardware-filterable). Kept separate from
    /// [`Self::lut_sampler`] so making the LUT lookup NEAREST does not turn RGBA
    /// linear interpolation into nearest.
    linear_sampler: wgpu::Sampler,
    /// A 1×1 `R32Float` texture of value `1.0`, bound at scalar binding 5 for
    /// images without a per-pixel alpha map so the bind group stays valid; the
    /// shader never samples it (its `has_alpha_map` flag is 0).
    dummy_alpha_view: wgpu::TextureView,
}

impl ImagePipeline {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("siplot image"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/image.wgsl").into()),
        });
        let rgba_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("siplot image rgba"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/image_rgba.wgsl").into()),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("siplot image bgl"),
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
                    // 5: per-pixel alpha texture (unfilterable float, like the
                    // data texture); a 1×1 dummy when the image has no alpha map.
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("siplot image layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("siplot image pipeline"),
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

        // RGBA pipeline: its own minimal layout (uniform + RGBA texture +
        // sampler, no LUT). The texture is declared filterable and the sampler
        // slot is filtering so the bind group can choose the nearest or linear
        // sampler per image (silx `interpolation`). `Rgba8UnormSrgb` is a
        // filterable format, so this needs no extra wgpu feature.
        let rgba_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("siplot image rgba bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: NonZeroU64::new(
                                std::mem::size_of::<ImageRgbaParams>() as u64,
                            ),
                        },
                        count: None,
                    },
                    // 1: RGBA texture (filterable; nearest or linear per image)
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    // 2: sampler (filtering: the nearest or linear sampler is
                    // bound per image; a Filtering slot accepts either)
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
        let rgba_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("siplot image rgba layout"),
            bind_group_layouts: &[Some(&rgba_bind_group_layout)],
            immediate_size: 0,
        });
        let rgba_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("siplot image rgba pipeline"),
            layout: Some(&rgba_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &rgba_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &rgba_shader,
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
            label: Some("siplot image data sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        // NEAREST for the LUT lookup (silx GL_NEAREST, GLPlotImage.py:338-347):
        // the 256-entry LUT is a discrete palette, so the shader snaps to the
        // nearest texel — `min(floor(coord·256), 255)` with ClampToEdge — instead
        // of blending between entries. Matches the CPU `Colormap::lut_index`.
        let lut_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("siplot image lut sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        // LINEAR sampler for the direct-RGBA image path's bilinear interpolation.
        let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("siplot image linear sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // 1×1 R32Float texture of 1.0, bound at scalar binding 5 for images
        // without a per-pixel alpha map (the shader gates it off via
        // `has_alpha_map == 0`, so its value is never actually read).
        let dummy_alpha = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("siplot image dummy alpha"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // (no queue write needed: the dummy is never sampled, only bound)
        let dummy_alpha_view = dummy_alpha.create_view(&wgpu::TextureViewDescriptor::default());

        Self {
            pipeline,
            rgba_pipeline,
            bind_group_layout,
            rgba_bind_group_layout,
            data_sampler,
            lut_sampler,
            linear_sampler,
            dummy_alpha_view,
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
/// `full_width`, producing a tightly packed row-major `w × h` buffer. Generic
/// over the texel type so it serves both scalar (`f32`) and RGBA (`[u8; 4]`).
fn extract_subgrid<T: Copy>(
    data: &[T],
    full_width: u32,
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
) -> Vec<T> {
    let mut out = Vec::with_capacity((w as usize) * (h as usize));
    for row in 0..h {
        let src = ((y0 + row) as usize) * (full_width as usize) + x0 as usize;
        out.extend_from_slice(&data[src..src + w as usize]);
    }
    out
}

/// One tile of a (possibly split) image: its own texture (`R32Float` for a
/// scalar image, `Rgba8UnormSrgb` for an RGBA one), params, bind group,
/// data-space rect, and pixel bounds within the full image (the bounds route
/// [`GpuImage::update_region`] writes to the tiles they overlap).
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

/// What a [`GpuImage`] renders: a colormapped scalar field (carrying the
/// resolved normalization for the per-frame uniform) or a direct RGBA image
/// (no colormap fields — they would have no meaning).
#[derive(Clone, Copy)]
enum GpuImageKind {
    Scalar {
        /// Bounds transformed by the normalization (`norm(vmin)`).
        cmap_min: f32,
        /// `1 / (norm(vmax) - norm(vmin))`, or 0 for a degenerate range.
        cmap_one_over_range: f32,
        gamma: f32,
        /// Normalization code; see `Normalization::code`.
        norm: u32,
        /// Interpolation code; see [`InterpolationMode::code`].
        interp: u32,
        /// 1 if a per-pixel alpha map is bound at binding 5, else 0.
        has_alpha_map: u32,
        /// Linear RGBA for non-finite samples (silx `Colormap.nan_color`),
        /// pre-converted from the colormap's sRGB bytes.
        nan_color: [f32; 4],
    },
    Rgba,
}

/// One uploaded image's GPU resources, persisting across frames. The image is
/// stored as one or more `ImageTile`s so it can exceed the single-texture
/// dimension limit.
pub struct GpuImage {
    tiles: Vec<ImageTile>,
    width: u32,
    height: u32,
    kind: GpuImageKind,
    alpha: f32,
}

impl GpuImage {
    /// Upload `image` and build per-tile bind groups. A scalar image uploads
    /// `R32Float` tiles plus a shared 256x1 sRGB LUT and uses the colormap
    /// pipeline; an RGBA image uploads `Rgba8UnormSrgb` tiles and uses the
    /// direct pipeline (no LUT). Either way the image is split into tiles no
    /// larger than the device's `max_texture_dimension_2d`, so it can exceed the
    /// single-texture limit.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &ImagePipeline,
        image: &ImageData,
    ) -> Self {
        let (ox, oy) = image.origin;
        let (sx, sy) = image.scale;
        let max_dim = device.limits().max_texture_dimension_2d;
        // A tile's data-space rect: its pixel sub-grid mapped through origin +
        // scale, so adjacent tiles abut exactly.
        let rect_of = |x0: u32, y0: u32, w: u32, h: u32| {
            [
                (ox + sx * x0 as f64) as f32,
                (oy + sy * y0 as f64) as f32,
                (ox + sx * (x0 + w) as f64) as f32,
                (oy + sy * (y0 + h) as f64) as f32,
            ]
        };

        let (tiles, kind) = match &image.pixels {
            ImagePixels::Scalar { data, colormap } => {
                // LUT texture is shared across tiles (built once).
                let lut_size = wgpu::Extent3d {
                    width: 256,
                    height: 1,
                    depth_or_array_layers: 1,
                };
                let lut_texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("siplot image lut"),
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
                    bytemuck::cast_slice(&colormap.lut),
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4 * 256),
                        rows_per_image: Some(1),
                    },
                    lut_size,
                );
                let lut_view = lut_texture.create_view(&wgpu::TextureViewDescriptor::default());

                let tiles = tile_bounds(image.width, image.height, max_dim)
                    .into_iter()
                    .map(|(x0, y0, w, h)| {
                        let tile_size = wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        };
                        let data_texture = device.create_texture(&wgpu::TextureDescriptor {
                            label: Some("siplot image tile"),
                            size: tile_size,
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format: wgpu::TextureFormat::R32Float,
                            usage: wgpu::TextureUsages::TEXTURE_BINDING
                                | wgpu::TextureUsages::COPY_DST,
                            view_formats: &[],
                        });
                        let sub = extract_subgrid(data, image.width, x0, y0, w, h);
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
                        let data_view =
                            data_texture.create_view(&wgpu::TextureViewDescriptor::default());

                        // Per-pixel alpha: when the image carries an alpha map,
                        // upload this tile's slice of it to its own R32Float
                        // texture; otherwise bind the pipeline's 1×1 dummy. The
                        // bind group retains the texture (like the LUT), so the
                        // local view need not outlive this closure.
                        let alpha_view = image.alpha_map.as_ref().map(|alpha| {
                            let alpha_texture = device.create_texture(&wgpu::TextureDescriptor {
                                label: Some("siplot image tile alpha"),
                                size: tile_size,
                                mip_level_count: 1,
                                sample_count: 1,
                                dimension: wgpu::TextureDimension::D2,
                                format: wgpu::TextureFormat::R32Float,
                                usage: wgpu::TextureUsages::TEXTURE_BINDING
                                    | wgpu::TextureUsages::COPY_DST,
                                view_formats: &[],
                            });
                            let sub = extract_subgrid(alpha, image.width, x0, y0, w, h);
                            queue.write_texture(
                                wgpu::TexelCopyTextureInfo {
                                    texture: &alpha_texture,
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
                            alpha_texture.create_view(&wgpu::TextureViewDescriptor::default())
                        });
                        let alpha_binding =
                            alpha_view.as_ref().unwrap_or(&pipeline.dummy_alpha_view);

                        let params = device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("siplot image tile params"),
                            size: std::mem::size_of::<ImageParams>() as u64,
                            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        });
                        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("siplot image tile bg"),
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
                                    resource: wgpu::BindingResource::Sampler(
                                        &pipeline.data_sampler,
                                    ),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 3,
                                    resource: wgpu::BindingResource::TextureView(&lut_view),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 4,
                                    resource: wgpu::BindingResource::Sampler(&pipeline.lut_sampler),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 5,
                                    resource: wgpu::BindingResource::TextureView(alpha_binding),
                                },
                            ],
                        });
                        ImageTile {
                            bind_group,
                            params,
                            data_texture,
                            rect: rect_of(x0, y0, w, h),
                            x0,
                            y0,
                            w,
                            h,
                        }
                    })
                    .collect();

                let (cmap_min, cmap_one_over_range) = colormap.norm_bounds();
                (
                    tiles,
                    GpuImageKind::Scalar {
                        cmap_min,
                        cmap_one_over_range,
                        gamma: colormap.gamma,
                        norm: colormap.normalization.code(),
                        interp: image.interpolation.code(),
                        has_alpha_map: u32::from(image.alpha_map.is_some()),
                        nan_color: srgb_u8_to_linear_rgba(colormap.nan_color),
                    },
                )
            }
            ImagePixels::Rgba { data } => {
                // silx interpolation on a direct RGBA image: `Rgba8UnormSrgb` is
                // filterable, so the hardware sampler does it (no manual bilinear
                // and no extra wgpu feature). Nearest reuses the data sampler.
                let rgba_sampler = match image.interpolation {
                    InterpolationMode::Nearest => &pipeline.data_sampler,
                    InterpolationMode::Linear => &pipeline.linear_sampler,
                };
                let tiles = tile_bounds(image.width, image.height, max_dim)
                    .into_iter()
                    .map(|(x0, y0, w, h)| {
                        let tile_size = wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        };
                        let data_texture = device.create_texture(&wgpu::TextureDescriptor {
                            label: Some("siplot image rgba tile"),
                            size: tile_size,
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format: wgpu::TextureFormat::Rgba8UnormSrgb,
                            usage: wgpu::TextureUsages::TEXTURE_BINDING
                                | wgpu::TextureUsages::COPY_DST,
                            view_formats: &[],
                        });
                        let sub = extract_subgrid(data, image.width, x0, y0, w, h);
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
                        let data_view =
                            data_texture.create_view(&wgpu::TextureViewDescriptor::default());

                        let params = device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("siplot image rgba tile params"),
                            size: std::mem::size_of::<ImageRgbaParams>() as u64,
                            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        });
                        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("siplot image rgba tile bg"),
                            layout: &pipeline.rgba_bind_group_layout,
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
                                    resource: wgpu::BindingResource::Sampler(rgba_sampler),
                                },
                            ],
                        });
                        ImageTile {
                            bind_group,
                            params,
                            data_texture,
                            rect: rect_of(x0, y0, w, h),
                            x0,
                            y0,
                            w,
                            h,
                        }
                    })
                    .collect();
                (tiles, GpuImageKind::Rgba)
            }
        };

        let gpu = Self {
            tiles,
            width: image.width,
            height: image.height,
            kind,
            alpha: image.alpha,
        };
        // Seed each tile's uniform; the per-frame transform overwrites `ortho`.
        gpu.write_uniforms(queue, IDENTITY, [0.0, 0.0]);
        gpu
    }

    /// Re-upload a `w × h` sub-region at `(x0, y0)` of the image in place (dirty
    /// update), routing it to the tiles it overlaps without recreating any GPU
    /// resources. `data` is row-major scalar values, length `w * h`. Row `y0` is
    /// the same row the shader samples, so increasing `y0` moves the region
    /// upward in the displayed image (origin lower-left). Panics if the region
    /// exceeds the image bounds, or if the image is RGBA (this is the scalar
    /// live-update path; the `f32` bytes would corrupt an `Rgba8` texture).
    pub(crate) fn update_region(
        &self,
        queue: &wgpu::Queue,
        x0: u32,
        y0: u32,
        w: u32,
        h: u32,
        data: &[f32],
    ) {
        assert!(
            matches!(self.kind, GpuImageKind::Scalar { .. }),
            "update_region is only valid for scalar images"
        );
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
            match self.kind {
                GpuImageKind::Scalar {
                    cmap_min,
                    cmap_one_over_range,
                    gamma,
                    norm,
                    interp,
                    has_alpha_map,
                    nan_color,
                } => {
                    let params = ImageParams {
                        ortho,
                        rect: tile.rect,
                        axis_log,
                        alpha: self.alpha,
                        cmap_min,
                        cmap_one_over_range,
                        gamma,
                        norm,
                        interp,
                        has_alpha_map,
                        _pad: [0; 3],
                        nan_color,
                    };
                    queue.write_buffer(&tile.params, 0, bytemuck::bytes_of(&params));
                }
                GpuImageKind::Rgba => {
                    let params = ImageRgbaParams {
                        ortho,
                        rect: tile.rect,
                        axis_log,
                        alpha: self.alpha,
                        _pad: 0.0,
                    };
                    queue.write_buffer(&tile.params, 0, bytemuck::bytes_of(&params));
                }
            }
        }
    }

    pub(crate) fn draw(&self, render_pass: &mut wgpu::RenderPass<'_>, pipeline: &ImagePipeline) {
        let pipe = match self.kind {
            GpuImageKind::Scalar { .. } => &pipeline.pipeline,
            GpuImageKind::Rgba => &pipeline.rgba_pipeline,
        };
        render_pass.set_pipeline(pipe);
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
    fn image_params_is_std140_sized() {
        // The std140 uniform layout (see the struct comment) must stay 144 bytes
        // with the vec4 nan_color 16-byte aligned at offset 128; drift here means
        // the Rust struct and the WGSL `Params` no longer agree.
        assert_eq!(std::mem::size_of::<ImageParams>(), 144);
        assert_eq!(std::mem::align_of::<ImageParams>(), 4);
    }

    #[test]
    fn srgb_u8_to_linear_rgba_matches_transfer_function() {
        // Endpoints are exact: 0 -> 0, 255 -> 1.0 in every RGB channel; alpha is
        // a plain linear 0..1 (not gamma-encoded).
        let black = srgb_u8_to_linear_rgba([0, 0, 0, 255]);
        assert_eq!(black, [0.0, 0.0, 0.0, 1.0]);
        let white = srgb_u8_to_linear_rgba([255, 255, 255, 255]);
        for ch in white {
            assert!((ch - 1.0).abs() < 1e-6, "white channel {ch} != 1.0");
        }
        // silx default nan_color (255, 255, 255, 0): white but fully transparent.
        let nan = srgb_u8_to_linear_rgba([255, 255, 255, 0]);
        assert_eq!(nan[3], 0.0, "default nan_color alpha must be transparent");
        // A mid sRGB value sits below its naive (s) value after linearization
        // (the curve is convex), and alpha tracks the byte linearly.
        let mid = srgb_u8_to_linear_rgba([188, 188, 188, 128]);
        let expected = ((188.0_f32 / 255.0 + 0.055) / 1.055).powf(2.4);
        assert!((mid[0] - expected).abs() < 1e-6, "mid linearization");
        assert!((mid[3] - 128.0 / 255.0).abs() < 1e-6, "alpha is linear");
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

    #[test]
    fn extract_subgrid_works_on_rgba_texels() {
        // Same block, but the texels are [u8; 4] (the generic over Copy texel).
        #[rustfmt::skip]
        let data = vec![
            [0, 0, 0, 0], [1, 1, 1, 1], [2, 2, 2, 2], [3, 3, 3, 3],
            [4, 4, 4, 4], [5, 5, 5, 5], [6, 6, 6, 6], [7, 7, 7, 7],
        ];
        let sub = extract_subgrid(&data, 4, 1, 0, 2, 2);
        assert_eq!(
            sub,
            vec![[1, 1, 1, 1], [2, 2, 2, 2], [5, 5, 5, 5], [6, 6, 6, 6]]
        );
    }

    #[test]
    fn scalar_constructor_carries_a_colormap() {
        let img = ImageData::new(2, 2, vec![0.0, 1.0, 2.0, 3.0], Colormap::viridis(0.0, 3.0));
        assert!(matches!(img.pixels, ImagePixels::Scalar { .. }));
        assert!(img.colormap().is_some());
        assert_eq!((img.width, img.height), (2, 2));
    }

    #[test]
    fn rgba_constructor_has_no_colormap() {
        let px = vec![
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [0, 0, 0, 0],
        ];
        let img = ImageData::rgba(2, 2, px);
        assert!(matches!(img.pixels, ImagePixels::Rgba { .. }));
        assert!(img.colormap().is_none());
        assert_eq!((img.width, img.height), (2, 2));
    }

    #[test]
    #[should_panic(expected = "data length must equal width * height")]
    fn rgba_constructor_rejects_length_mismatch() {
        ImageData::rgba(2, 2, vec![[0, 0, 0, 0]; 3]);
    }

    #[test]
    fn interpolation_default_matches_silx() {
        // silx default image interpolation is "nearest".
        assert_eq!(InterpolationMode::default(), InterpolationMode::Nearest);
    }

    /// A 4×4 row-major field aggregated by 2×2 blocks: each 2×2 block reduces to
    /// one output value, row-major. Blocks (max / min / mean):
    /// {0,1,4,5} {2,3,6,7} / {8,9,12,13} {10,11,14,15}.
    #[rustfmt::skip]
    fn field_4x4() -> Vec<f32> {
        vec![
            0.0,  1.0,  2.0,  3.0,
            4.0,  5.0,  6.0,  7.0,
            8.0,  9.0,  10.0, 11.0,
            12.0, 13.0, 14.0, 15.0,
        ]
    }

    #[test]
    fn aggregate_max_over_4x4_into_2x2() {
        let (out, w, h) = aggregate_blocks(&field_4x4(), 4, 4, 2, 2, AggregationMode::Max);
        assert_eq!((w, h), (2, 2));
        assert_eq!(out, vec![5.0, 7.0, 13.0, 15.0]);
    }

    #[test]
    fn aggregate_min_over_4x4_into_2x2() {
        let (out, w, h) = aggregate_blocks(&field_4x4(), 4, 4, 2, 2, AggregationMode::Min);
        assert_eq!((w, h), (2, 2));
        assert_eq!(out, vec![0.0, 2.0, 8.0, 10.0]);
    }

    #[test]
    fn aggregate_mean_over_4x4_into_2x2() {
        let (out, w, h) = aggregate_blocks(&field_4x4(), 4, 4, 2, 2, AggregationMode::Mean);
        assert_eq!((w, h), (2, 2));
        assert_eq!(out, vec![2.5, 4.5, 10.5, 12.5]);
    }

    #[test]
    fn aggregate_none_returns_data_unchanged() {
        let (out, w, h) = aggregate_blocks(&field_4x4(), 4, 4, 2, 2, AggregationMode::None);
        // NONE ignores the block factor entirely (silx Aggregation.NONE).
        assert_eq!((w, h), (4, 4));
        assert_eq!(out, field_4x4());
    }

    #[test]
    fn aggregate_block_factor_one_is_a_no_op() {
        // (1, 1) blocks reduce nothing even with an aggregation mode set.
        let (out, w, h) = aggregate_blocks(&field_4x4(), 4, 4, 1, 1, AggregationMode::Max);
        assert_eq!((w, h), (4, 4));
        assert_eq!(out, field_4x4());
    }

    #[test]
    fn aggregate_drops_remainder_rows_and_cols() {
        // 3×3 with 2×2 blocks: only the top-left 2×2 fills a block; the
        // remainder row/col is dropped (silx truncates to (w//b)*b, (h//b)*b).
        #[rustfmt::skip]
        let data = vec![
            0.0, 1.0, 2.0,
            3.0, 4.0, 5.0,
            6.0, 7.0, 8.0,
        ];
        let (out, w, h) = aggregate_blocks(&data, 3, 3, 2, 2, AggregationMode::Max);
        assert_eq!((w, h), (1, 1));
        assert_eq!(out, vec![4.0]); // max{0,1,3,4}
    }

    #[test]
    fn aggregate_ignores_nans_and_all_nan_block_is_nan() {
        // Block {NaN, 1, NaN, 5}: max ignores NaNs -> 5; the second block is all
        // NaN -> NaN (numpy.nanmax over an all-NaN slice).
        let nan = f32::NAN;
        #[rustfmt::skip]
        let data = vec![
            nan, 1.0, nan, nan,
            nan, 5.0, nan, nan,
            0.0, 0.0, 0.0, 0.0,
            0.0, 0.0, 0.0, 0.0,
        ];
        let (out, _, _) = aggregate_blocks(&data, 4, 4, 2, 2, AggregationMode::Max);
        assert_eq!(out[0], 5.0); // NaNs ignored
        assert!(out[1].is_nan()); // all-NaN block
        assert_eq!(out[2], 0.0);
        assert_eq!(out[3], 0.0);
    }

    #[test]
    fn aggregate_mean_ignores_nans_in_count() {
        // Block {NaN, 2, 4, NaN}: nanmean = (2 + 4) / 2 = 3 (NaNs not counted).
        let nan = f32::NAN;
        #[rustfmt::skip]
        let data = vec![
            nan, 2.0,
            4.0, nan,
        ];
        let (out, w, h) = aggregate_blocks(&data, 2, 2, 2, 2, AggregationMode::Mean);
        assert_eq!((w, h), (1, 1));
        assert_eq!(out, vec![3.0]);
    }

    #[test]
    fn aggregation_default_matches_silx() {
        // silx default aggregation is Aggregation.NONE.
        assert_eq!(AggregationMode::default(), AggregationMode::None);
    }

    #[test]
    fn interpolation_codes_match_shader() {
        // Must stay in sync with the branch in image.wgsl.
        assert_eq!(InterpolationMode::Nearest.code(), 0);
        assert_eq!(InterpolationMode::Linear.code(), 1);
    }

    #[test]
    fn new_image_defaults_to_nearest_interpolation() {
        let img = ImageData::new(2, 2, vec![0.0, 1.0, 2.0, 3.0], Colormap::viridis(0.0, 3.0));
        assert_eq!(img.interpolation, InterpolationMode::Nearest);
        let img = img.with_interpolation(InterpolationMode::Linear);
        assert_eq!(img.interpolation, InterpolationMode::Linear);
    }
}
