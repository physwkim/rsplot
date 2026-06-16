//! The wgpu backend — the `egui_wgpu::CallbackTrait` impl and persistent GPU
//! resources.
//!
//! Slice 1, step 1 scope: a "clear" pipeline that fills the data rect with a
//! solid color, plus the uniform holding that color. Image/curve pipelines and
//! per-plot/per-item GPU state maps are added to [`WgpuResources`] in later
//! steps (`doc/design.md` §3.1·§11).

use std::collections::HashMap;
use std::num::NonZeroU64;

use egui::{Color32, Pos2, Rect};
use egui_wgpu::{RenderState, wgpu};

use crate::core::backend::{
    Backend, CurveColor, CurveSpec, ImagePixelsSpec, ImageSpec, ItemHandle, MarkerSpec, PickResult,
    ShapeSpec, TriangleSpec,
};
use crate::core::marker::Marker;
use crate::core::plot::{Plot, PlotId};
use crate::core::shape::{Shape, ShapeKind};
use crate::core::transform::{Margins, Scale, Transform, YAxis};
use crate::core::triangles::Triangles;
use crate::render::gpu_curve::{CurveData, CurvePipeline, GpuCurve};
use crate::render::gpu_image::{
    AggregationMode, GpuImage, ImageData, ImagePipeline, ImagePixels, aggregate_blocks,
};

const OVERLAY_PICK_TOLERANCE_PX: f32 = 5.0;

/// Per-plot GPU data: the color uniform + bind group for the clear pass, and
/// the image/curve GPU buffers. Keyed by [`PlotId`] in [`WgpuResources`].
pub(crate) struct PlotGpuData {
    color_uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    images: Vec<GpuImage>,
    curves: Vec<GpuCurve>,
}

/// GPU resources that persist across frames. Stored as a single type in
/// `egui_wgpu`'s `callback_resources` (a type map).
///
/// Per-plot state is keyed by [`PlotId`] in `plots`, so multiple independent
/// plots can coexist in the same egui app without sharing GPU buffers.
pub struct WgpuResources {
    clear_pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    image_pipeline: ImagePipeline,
    curve_pipeline: CurvePipeline,
    /// Per-plot GPU data, keyed by plot ID.
    plots: HashMap<PlotId, PlotGpuData>,
}

impl WgpuResources {
    /// Build the clear pipeline and shared bind group layout. Per-plot state is
    /// allocated lazily by `WgpuResources::get_or_insert_plot`.
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("siplot clear"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/clear.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("siplot clear bgl"),
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
            label: Some("siplot clear layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let clear_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("siplot clear pipeline"),
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

        let image_pipeline = ImagePipeline::new(device, target_format);
        let curve_pipeline = CurvePipeline::new(device, target_format);

        Self {
            clear_pipeline,
            bind_group_layout,
            image_pipeline,
            curve_pipeline,
            plots: HashMap::new(),
        }
    }

    /// Return the [`PlotGpuData`] for `plot_id`, inserting a fresh entry if it
    /// does not yet exist.
    pub(crate) fn get_or_insert_plot(
        &mut self,
        device: &wgpu::Device,
        plot_id: PlotId,
    ) -> &mut PlotGpuData {
        if !self.plots.contains_key(&plot_id) {
            let color_uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("siplot clear color"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("siplot clear bg"),
                layout: &self.bind_group_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: color_uniform.as_entire_binding(),
                }],
            });
            self.plots.insert(
                plot_id,
                PlotGpuData {
                    color_uniform,
                    bind_group,
                    images: Vec::new(),
                    curves: Vec::new(),
                },
            );
        }
        self.plots.get_mut(&plot_id).unwrap()
    }

    /// Render the data layer (background clear, image, curves) for the given
    /// per-axis transforms into an offscreen `size = (w, h)` texture, read it
    /// back, and return tightly packed RGBA8 pixels (top row first). Used by
    /// [`crate::render::save::save_graph`] (`doc/design.md` §13 E1).
    ///
    /// `bg` is the linear, premultiplied clear color; the orthos/axis_log pairs
    /// are the same matrices the on-screen [`CurveCallback`] uses for each Y
    /// axis. Curves are drawn at whatever resolution they were last decimated
    /// to, and dashed lines reuse the dash arc length last computed for the
    /// on-screen view; neither is recomputed here, so a save target whose size
    /// differs from the on-screen data area keeps the on-screen dash metric.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_to_rgba(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
        plot_id: PlotId,
        size: (u32, u32),
        bg: [f32; 4],
        ortho_left: [[f32; 4]; 4],
        axis_log_left: [f32; 2],
        ortho_right: [[f32; 4]; 4],
        axis_log_right: [f32; 2],
        ortho_extra: &[[[f32; 4]; 4]],
        axis_log_extra: &[[f32; 2]],
    ) -> Result<Vec<u8>, crate::render::save::SaveError> {
        use crate::core::transform::YAxis;
        use crate::render::save::{padded_bytes_per_row, rows_to_rgba8};

        let plot_data = self.plots.get(&plot_id).ok_or_else(|| {
            crate::render::save::SaveError::Readback("no GPU data for plot_id".into())
        })?;

        let (w, h) = size;
        let extent = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("siplot offscreen target"),
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
        for image in &plot_data.images {
            image.write_uniforms(queue, ortho_left, axis_log_left);
        }
        for curve in &plot_data.curves {
            let (ortho, axis_log) = match curve.y_axis {
                YAxis::Left => (ortho_left, axis_log_left),
                YAxis::Right => (ortho_right, axis_log_right),
                // An extra axis with no built matrix (unknown index / no range)
                // falls back to the left axis, matching the on-screen path.
                YAxis::Extra(n) => match (ortho_extra.get(n), axis_log_extra.get(n)) {
                    (Some(&o), Some(&l)) => (o, l),
                    _ => (ortho_left, axis_log_left),
                },
            };
            curve.write_uniforms(queue, ortho, axis_log, viewport_px);
        }

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("siplot offscreen pass"),
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
            // The render pass viewport defaults to the full target. Fills, then
            // error bars, then lines, then markers, mirroring the on-screen draw
            // order.
            for image in &plot_data.images {
                image.draw(&mut rp, &self.image_pipeline);
            }
            for curve in &plot_data.curves {
                curve.draw_fill(&mut rp, &self.curve_pipeline);
            }
            for curve in &plot_data.curves {
                curve.draw_errorbars(&mut rp, &self.curve_pipeline);
            }
            for curve in &plot_data.curves {
                curve.draw(&mut rp, &self.curve_pipeline);
            }
            for curve in &plot_data.curves {
                curve.draw_caps(&mut rp, &self.curve_pipeline);
            }
            for curve in &plot_data.curves {
                curve.draw_markers(&mut rp, &self.curve_pipeline);
            }
        }

        // Copy the target into a readback buffer with a padded row stride.
        let bpr = padded_bytes_per_row(w);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("siplot readback"),
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

#[derive(Clone, Debug)]
enum BackendItem {
    Curve { handle: ItemHandle, data: CurveData },
    Image { handle: ItemHandle, data: ImageData },
    Triangles { handle: ItemHandle, data: Triangles },
    Shape { handle: ItemHandle, data: Shape },
    Marker { handle: ItemHandle, data: Marker },
}

impl BackendItem {
    fn handle(&self) -> ItemHandle {
        match self {
            BackendItem::Curve { handle, .. }
            | BackendItem::Image { handle, .. }
            | BackendItem::Triangles { handle, .. }
            | BackendItem::Shape { handle, .. }
            | BackendItem::Marker { handle, .. } => *handle,
        }
    }
}

/// Retained wgpu implementation of the backend-facing API.
///
/// The struct owns the plot model plus the backend item registry. GPU data
/// items (`add_curve`/`add_image`) are synchronized into [`WgpuResources`];
/// overlay items (`add_triangles`/`add_shape`/`add_marker`) are mirrored onto
/// [`Plot`] so [`crate::PlotView`] draws them each frame.
pub struct WgpuBackend {
    render_state: RenderState,
    plot: Plot,
    next_handle: ItemHandle,
    items: Vec<BackendItem>,
    last_data_area: Option<Rect>,
    /// Visibility state per item. Default `true`. Invisible items are excluded
    /// from every draw pass and from overlay lists, but their handles remain live.
    item_visible: HashMap<ItemHandle, bool>,
    /// Draw-order z-value per item. Default `0.0`. Within each GPU item type
    /// (images, curves), items are sorted ascending by z before drawing.
    item_z: HashMap<ItemHandle, f32>,
}

impl WgpuBackend {
    /// Install wgpu resources and create an empty backend for `plot_id`.
    pub fn new(render_state: &RenderState, plot_id: PlotId) -> Self {
        Self::from_plot(render_state, Plot::new(plot_id))
    }

    /// Install wgpu resources and attach an existing plot model.
    pub fn from_plot(render_state: &RenderState, plot: Plot) -> Self {
        install(render_state);
        Self {
            render_state: render_state.clone(),
            plot,
            next_handle: 1,
            items: Vec::new(),
            last_data_area: None,
            item_visible: HashMap::new(),
            item_z: HashMap::new(),
        }
    }

    /// The plot model to pass to [`crate::PlotView`].
    pub fn plot(&self) -> &Plot {
        &self.plot
    }

    /// Mutable plot model to pass to [`crate::PlotView`].
    pub fn plot_mut(&mut self) -> &mut Plot {
        &mut self.plot
    }

    /// Record the data-area rect returned by the widget's transform. This
    /// enables [`Backend::data_to_pixel`], [`Backend::pixel_to_data`], and
    /// [`Backend::plot_bounds_in_pixels`] between frames.
    pub fn set_plot_bounds_in_pixels(&mut self, data_area: Rect) {
        self.last_data_area = Some(data_area);
    }

    fn alloc_handle(&mut self) -> ItemHandle {
        let handle = self.next_handle;
        self.next_handle = self
            .next_handle
            .checked_add(1)
            .expect("backend item handle overflow");
        self.item_visible.insert(handle, true);
        self.item_z.insert(handle, 0.0);
        handle
    }

    /// Show or hide an item. Hidden items are excluded from all draw passes.
    /// Returns `false` if the handle is unknown.
    pub fn set_item_visible(&mut self, handle: ItemHandle, visible: bool) -> bool {
        let Some(v) = self.item_visible.get_mut(&handle) else {
            return false;
        };
        *v = visible;
        self.sync_plot_items();
        self.sync_gpu_items();
        true
    }

    /// Whether an item is currently visible. Returns `true` for unknown handles
    /// (conservative default so callers can always draw safely).
    pub fn is_item_visible(&self, handle: ItemHandle) -> bool {
        self.item_visible.get(&handle).copied().unwrap_or(true)
    }

    /// Set the draw-order z-value for an item. Items with a higher z are drawn
    /// on top within their GPU layer (images above images, curves above curves).
    /// Returns `false` if the handle is unknown.
    pub fn set_item_z(&mut self, handle: ItemHandle, z: f32) -> bool {
        let Some(v) = self.item_z.get_mut(&handle) else {
            return false;
        };
        *v = z;
        self.sync_gpu_items();
        true
    }

    /// Current z-value for an item. Returns `0.0` for unknown handles.
    pub fn item_z(&self, handle: ItemHandle) -> f32 {
        self.item_z.get(&handle).copied().unwrap_or(0.0)
    }

    fn visible_items_sorted_by_z(&self) -> Vec<&BackendItem> {
        let mut items: Vec<(f32, &BackendItem)> = self
            .items
            .iter()
            .filter(|item| {
                self.item_visible
                    .get(&item.handle())
                    .copied()
                    .unwrap_or(true)
            })
            .map(|item| {
                let z = self.item_z.get(&item.handle()).copied().unwrap_or(0.0);
                (z, item)
            })
            .collect();
        items.sort_by(|a, b| a.0.total_cmp(&b.0));
        items.into_iter().map(|(_, item)| item).collect()
    }

    fn sync_gpu_items(&self) {
        let visible = self.visible_items_sorted_by_z();
        let images: Vec<ImageData> = visible
            .iter()
            .filter_map(|item| match item {
                BackendItem::Image { data, .. } => Some(data.clone()),
                _ => None,
            })
            .collect();
        let curves: Vec<CurveData> = visible
            .iter()
            .filter_map(|item| match item {
                BackendItem::Curve { data, .. } => Some(data.clone()),
                _ => None,
            })
            .collect();
        set_images(&self.render_state, self.plot.id, &images);
        set_curves(&self.render_state, self.plot.id, &curves);
    }

    fn sync_plot_items(&mut self) {
        // Collect cloned overlay data first so there is no outstanding borrow on
        // `self` when `self.plot.*` fields are assigned below.
        let mut triangles: Vec<Triangles> = Vec::new();
        let mut shapes: Vec<Shape> = Vec::new();
        let mut markers: Vec<Marker> = Vec::new();
        // Parallel to `markers`: marker_handles[i] is the handle of markers[i],
        // pushed in lockstep below so the mirror index maps back to the backend
        // item for drag persistence.
        let mut marker_handles: Vec<ItemHandle> = Vec::new();
        let mut colormap = None;

        let mut items_with_z: Vec<(f32, &BackendItem)> = self
            .items
            .iter()
            .filter(|item| {
                self.item_visible
                    .get(&item.handle())
                    .copied()
                    .unwrap_or(true)
            })
            .map(|item| {
                let z = self.item_z.get(&item.handle()).copied().unwrap_or(0.0);
                (z, item)
            })
            .collect();
        items_with_z.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, item) in &items_with_z {
            match item {
                BackendItem::Triangles { data, .. } => triangles.push(data.clone()),
                BackendItem::Shape { data, .. } => shapes.push(data.clone()),
                BackendItem::Marker { handle, data } => {
                    markers.push(data.clone());
                    marker_handles.push(*handle);
                }
                _ => {}
            }
        }
        for (_, item) in items_with_z.iter().rev() {
            if let BackendItem::Image { data, .. } = item
                && let Some(cm) = data.colormap()
            {
                colormap = Some(cm.clone());
                break;
            }
        }

        self.plot.triangles = triangles;
        self.plot.shapes = shapes;
        self.plot.markers = markers;
        self.plot.marker_handles = marker_handles;
        self.plot.colormap = colormap;
    }

    fn transform_for(&self, axis: YAxis) -> Option<Transform> {
        let area = self.last_data_area?;
        match axis {
            YAxis::Left => Some(self.plot.transform(area)),
            YAxis::Right => self.plot.transform_y2(area),
            YAxis::Extra(n) => self.plot.transform_extra(n, area),
        }
    }

    fn find_item(&self, handle: ItemHandle) -> Option<&BackendItem> {
        self.items.iter().find(|item| item.handle() == handle)
    }

    /// Replace the data/style for an existing curve handle.
    pub fn update_curve(&mut self, handle: ItemHandle, curve: CurveSpec<'_>) -> bool {
        let Some(item) = self
            .items
            .iter_mut()
            .find(|item| matches!(item, BackendItem::Curve { handle: h, .. } if *h == handle))
        else {
            return false;
        };
        *item = BackendItem::Curve {
            handle,
            data: curve_data_from_spec(curve),
        };
        self.sync_gpu_items();
        true
    }

    /// Replace the data/style for an existing image handle.
    pub fn update_image(&mut self, handle: ItemHandle, image: ImageSpec<'_>) -> bool {
        let Some(item) = self
            .items
            .iter_mut()
            .find(|item| matches!(item, BackendItem::Image { handle: h, .. } if *h == handle))
        else {
            return false;
        };
        *item = BackendItem::Image {
            handle,
            data: image_data_from_spec(image),
        };
        self.sync_plot_items();
        self.sync_gpu_items();
        true
    }

    /// Replace the [`Marker`] data for an existing marker handle, returning
    /// `false` if no marker with that handle exists.
    ///
    /// Markers are the per-frame mirror `plot.markers` (rebuilt from the backend
    /// items on every sync), so a live drag that mutates the mirror does not
    /// persist on its own — the next sync clobbers it. This writes the new data
    /// back to the owning `BackendItem::Marker` so subsequent syncs stay
    /// consistent. Markers are painter-drawn from `plot.markers`, not part of the
    /// GPU item set, so only `sync_plot_items` is needed (no `sync_gpu_items`).
    pub fn update_marker(&mut self, handle: ItemHandle, marker: Marker) -> bool {
        let Some(item) = self
            .items
            .iter_mut()
            .find(|item| matches!(item, BackendItem::Marker { handle: h, .. } if *h == handle))
        else {
            return false;
        };
        *item = BackendItem::Marker {
            handle,
            data: marker,
        };
        self.sync_plot_items();
        true
    }

    /// The [`Marker`] data of an existing marker handle (the persisted backend
    /// item, the source of truth), or `None` if no marker with that handle
    /// exists. Reads the backend item rather than the per-frame `plot.markers`
    /// mirror so it is correct even between syncs.
    pub fn marker(&self, handle: ItemHandle) -> Option<&Marker> {
        match self.find_item(handle) {
            Some(BackendItem::Marker { data, .. }) => Some(data),
            _ => None,
        }
    }

    /// Remove every backend item and synchronize GPU/plot state.
    pub fn clear_items(&mut self) {
        self.items.clear();
        self.item_visible.clear();
        self.item_z.clear();
        self.sync_plot_items();
        self.sync_gpu_items();
    }
}

impl Backend for WgpuBackend {
    type SaveError = crate::render::save::SaveError;

    fn add_curve(&mut self, curve: CurveSpec<'_>) -> ItemHandle {
        let handle = self.alloc_handle();
        self.items.push(BackendItem::Curve {
            handle,
            data: curve_data_from_spec(curve),
        });
        self.sync_gpu_items();
        handle
    }

    fn add_image(&mut self, image: ImageSpec<'_>) -> ItemHandle {
        let handle = self.alloc_handle();
        self.items.push(BackendItem::Image {
            handle,
            data: image_data_from_spec(image),
        });
        self.sync_plot_items();
        self.sync_gpu_items();
        handle
    }

    fn add_triangles(&mut self, tris: TriangleSpec<'_>) -> ItemHandle {
        let handle = self.alloc_handle();
        self.items.push(BackendItem::Triangles {
            handle,
            data: triangles_from_spec(tris),
        });
        self.sync_plot_items();
        handle
    }

    fn add_shape(&mut self, shape: ShapeSpec<'_>) -> ItemHandle {
        let handle = self.alloc_handle();
        self.items.push(BackendItem::Shape {
            handle,
            data: shape_from_spec(shape),
        });
        self.sync_plot_items();
        handle
    }

    fn add_marker(&mut self, marker: MarkerSpec<'_>) -> ItemHandle {
        let handle = self.alloc_handle();
        self.items.push(BackendItem::Marker {
            handle,
            data: marker_from_spec(marker),
        });
        self.sync_plot_items();
        handle
    }

    fn remove(&mut self, item: ItemHandle) -> bool {
        let before = self.items.len();
        self.items.retain(|existing| existing.handle() != item);
        let removed = self.items.len() != before;
        if removed {
            self.item_visible.remove(&item);
            self.item_z.remove(&item);
            self.sync_plot_items();
            self.sync_gpu_items();
        }
        removed
    }

    fn set_limits(&mut self, xmin: f64, xmax: f64, ymin: f64, ymax: f64, y2: Option<(f64, f64)>) {
        self.plot.limits = (xmin, xmax, ymin, ymax);
        self.plot.y2 = y2;
    }

    fn x_limits(&self) -> (f64, f64) {
        (self.plot.limits.0, self.plot.limits.1)
    }

    fn y_limits(&self, axis: YAxis) -> Option<(f64, f64)> {
        match axis {
            YAxis::Left => Some((self.plot.limits.2, self.plot.limits.3)),
            YAxis::Right => self.plot.y2,
            YAxis::Extra(n) => self.plot.extra.get(n).and_then(|a| a.range),
        }
    }

    fn set_x_log(&mut self, on: bool) {
        self.plot.x_scale = if on { Scale::Log10 } else { Scale::Linear };
    }

    fn set_y_log(&mut self, on: bool) {
        self.plot.y_scale = if on { Scale::Log10 } else { Scale::Linear };
    }

    fn set_x_inverted(&mut self, on: bool) {
        self.plot.x_inverted = on;
    }

    fn set_y_inverted(&mut self, on: bool) {
        self.plot.y_inverted = on;
    }

    fn set_keep_data_aspect_ratio(&mut self, on: bool) {
        self.plot.keep_aspect = on;
    }

    fn data_to_pixel(&self, x: f64, y: f64, axis: YAxis) -> Option<Pos2> {
        self.transform_for(axis).map(|t| t.data_to_pixel(x, y))
    }

    fn pixel_to_data(&self, p: Pos2, axis: YAxis) -> Option<(f64, f64)> {
        self.transform_for(axis).map(|t| t.pixel_to_data(p))
    }

    fn plot_bounds_in_pixels(&self) -> Option<Rect> {
        self.last_data_area
    }

    fn set_axes_margins(&mut self, margins: Margins) {
        self.plot.margins = margins;
    }

    fn set_title(&mut self, title: Option<&str>) {
        self.plot.title = title.map(ToOwned::to_owned);
    }

    fn set_x_label(&mut self, label: Option<&str>) {
        self.plot.x_label = label.map(ToOwned::to_owned);
    }

    fn set_y_label(&mut self, label: Option<&str>, axis: YAxis) {
        match axis {
            YAxis::Left => self.plot.y_label = label.map(ToOwned::to_owned),
            YAxis::Right => self.plot.y2_label = label.map(ToOwned::to_owned),
            YAxis::Extra(n) => {
                if let Some(a) = self.plot.extra.get_mut(n) {
                    a.label = label.map(ToOwned::to_owned);
                }
            }
        }
    }

    fn set_foreground_colors(&mut self, foreground: Color32, grid: Color32) {
        self.plot.foreground = Some(foreground);
        self.plot.grid_color = Some(grid);
    }

    fn set_background_colors(&mut self, _background: Color32, data_background: Color32) {
        self.plot.data_background = data_background;
    }

    fn pick_item(&self, p: Pos2, item: ItemHandle) -> Option<PickResult> {
        match self.find_item(item)? {
            BackendItem::Curve { data, .. } => {
                let transform = self
                    .transform_for(data.y_axis)
                    .or_else(|| self.transform_for(YAxis::Left))?;
                nearest_curve_point(data, &transform, p, 3.0)
            }
            BackendItem::Image { data, .. } => {
                let transform = self.transform_for(YAxis::Left)?;
                pick_image_pixel(data, &transform, p)
                    .map(|(col, row)| PickResult::ImagePixel { col, row })
            }
            BackendItem::Triangles { handle, data } => {
                let transform = self.transform_for(YAxis::Left)?;
                pick_triangles(data, &transform, p).then_some(PickResult::Item { handle: *handle })
            }
            BackendItem::Shape { handle, data } => {
                let transform = self.transform_for(YAxis::Left)?;
                pick_shape(data, &transform, p).then_some(PickResult::Item { handle: *handle })
            }
            BackendItem::Marker { handle, data } => {
                let transform = self
                    .transform_for(data.y_axis)
                    .or_else(|| self.transform_for(YAxis::Left))?;
                pick_marker(data, &transform, p).then_some(PickResult::Item { handle: *handle })
            }
        }
    }

    fn items_back_to_front(&self) -> Vec<ItemHandle> {
        self.items.iter().map(BackendItem::handle).collect()
    }

    fn replot(&mut self) {
        self.sync_plot_items();
        self.sync_gpu_items();
    }

    fn save_graph(&self, path: &std::path::Path, size: (u32, u32)) -> Result<(), Self::SaveError> {
        crate::render::save::save_graph(&self.render_state, &self.plot, size, path)
    }

    fn save_graph_with_format(
        &self,
        path: &std::path::Path,
        size: (u32, u32),
        format: crate::render::save::SaveFormat,
        dpi: u32,
    ) -> Result<(), Self::SaveError> {
        crate::render::save::save_graph_with_format(
            &self.render_state,
            &self.plot,
            size,
            path,
            format,
            dpi,
        )
    }
}

fn apply_alpha(color: Color32, alpha: f32) -> Color32 {
    // Scale the straight alpha by `alpha`, preserving the straight RGB. Reading
    // the premultiplied `color.r()/g()/b()` accessors and re-wrapping via
    // `from_rgba_unmultiplied` would double-premultiply the RGB for translucent
    // colors; `scale_alpha` reads straight RGB.
    crate::core::color::scale_alpha(color, alpha)
}

fn curve_data_from_spec(spec: CurveSpec<'_>) -> CurveData {
    let color = match spec.color {
        CurveColor::Uniform(color) => apply_alpha(color, spec.alpha),
        CurveColor::PerVertex(colors) => colors
            .first()
            .copied()
            .map(|color| apply_alpha(color, spec.alpha))
            .unwrap_or(Color32::WHITE),
    };
    let mut curve = CurveData::new(spec.x.to_vec(), spec.y.to_vec(), color)
        .with_width(spec.line_width)
        .with_line_style(spec.line_style)
        .with_marker_size(spec.symbol_size)
        .with_y_axis(spec.y_axis);
    if let CurveColor::PerVertex(colors) = spec.color {
        curve = curve.with_colors(
            colors
                .iter()
                .copied()
                .map(|color| apply_alpha(color, spec.alpha))
                .collect(),
        );
    }
    if let Some(gap_color) = spec.gap_color {
        curve = curve.with_gap_color(apply_alpha(gap_color, spec.alpha));
    }
    if let Some(symbol) = spec.symbol {
        curve = curve.with_symbol(symbol);
    }
    if let Some(error) = spec.x_error {
        curve = curve.with_x_error(error);
    }
    if let Some(error) = spec.y_error {
        curve = curve.with_y_error(error);
    }
    if spec.fill {
        curve = curve.with_fill(spec.baseline);
    }
    curve
}

fn image_data_from_spec(spec: ImageSpec<'_>) -> ImageData {
    // Scale grows with the aggregation block factor so the (downsampled) image
    // covers the same data-space extent, mirroring silx
    // ImageDataAggregated._addBackendRenderer: `scale = sx * lodx, sy * lody`.
    let mut scale = spec.scale;
    let mut image = match spec.pixels {
        ImagePixelsSpec::Scalar {
            width,
            height,
            data,
            colormap,
        } => {
            let (bx, by) = spec.aggregation_block;
            let (agg_data, agg_w, agg_h) =
                aggregate_blocks(data, width, height, bx, by, spec.aggregation);
            if spec.aggregation != AggregationMode::None {
                scale = (scale.0 * bx.max(1) as f64, scale.1 * by.max(1) as f64);
            }
            // A per-pixel alpha map is at the original resolution; reduce it by
            // the same blocks (mean coverage) when the data is aggregated so it
            // matches the downsampled image dimensions.
            let alpha_map = spec.alpha_map.map(|am| {
                assert_eq!(
                    am.len(),
                    (width as usize) * (height as usize),
                    "alpha_map length must equal width * height"
                );
                if spec.aggregation != AggregationMode::None {
                    aggregate_blocks(am, width, height, bx, by, AggregationMode::Mean).0
                } else {
                    am.to_vec()
                }
            });
            let mut img = ImageData::new(agg_w, agg_h, agg_data, *colormap);
            img.alpha_map = alpha_map;
            img
        }
        // Aggregation is a scalar-data reduction; RGBA images are passed through
        // unaggregated (silx ImageDataAggregated is a scalar density map).
        ImagePixelsSpec::Rgba {
            width,
            height,
            data,
        } => ImageData::rgba(width, height, data.to_vec()),
    };
    image.origin = spec.origin;
    image.scale = scale;
    image.alpha = spec.alpha.clamp(0.0, 1.0);
    image.interpolation = spec.interpolation;
    image
}

fn triangles_from_spec(spec: TriangleSpec<'_>) -> Triangles {
    Triangles::new(
        spec.x.to_vec(),
        spec.y.to_vec(),
        spec.triangles.to_vec(),
        spec.colors.to_vec(),
    )
    .with_alpha(spec.alpha)
}

fn shape_from_spec(spec: ShapeSpec<'_>) -> Shape {
    let _overlay = spec.overlay;
    let shape = match spec.kind {
        ShapeKind::Polygon => Shape::polygon(spec.x.to_vec(), spec.y.to_vec()),
        ShapeKind::Rectangle => {
            assert!(
                spec.x.len() >= 2 && spec.y.len() >= 2,
                "rectangle shape requires two x and two y coordinates"
            );
            Shape::rectangle(spec.x[0], spec.y[0], spec.x[1], spec.y[1])
        }
        ShapeKind::Polyline => Shape::polyline(spec.x.to_vec(), spec.y.to_vec()),
        ShapeKind::HLine => Shape::hlines(spec.y.to_vec()),
        ShapeKind::VLine => Shape::vlines(spec.x.to_vec()),
    };
    let mut shape = shape
        .with_color(spec.color)
        .with_fill(spec.fill)
        .with_line_style(spec.line_style)
        .with_line_width(spec.line_width);
    if let Some(gap_color) = spec.gap_color {
        shape = shape.with_gap_color(gap_color);
    }
    shape
}

fn marker_from_spec(spec: MarkerSpec<'_>) -> Marker {
    let mut marker = match (spec.x, spec.y) {
        (Some(x), Some(y)) => {
            let mut marker = Marker::point(x, y).with_symbol_size(spec.symbol_size);
            if let Some(symbol) = spec.symbol {
                marker = marker.with_symbol(symbol);
            }
            marker
        }
        (Some(x), None) => Marker::vline(x),
        (None, Some(y)) => Marker::hline(y),
        (None, None) => panic!("marker requires at least one coordinate"),
    }
    .with_color(spec.color)
    .with_line_style(spec.line_style)
    .with_line_width(spec.line_width)
    .with_y_axis(spec.y_axis)
    .with_draggable(spec.is_draggable)
    .with_constraint(spec.constraint);
    if let Some(text) = spec.text {
        marker = marker.with_text(text);
    }
    if let Some(bg_color) = spec.bg_color {
        marker = marker.with_bgcolor(bg_color);
    }
    marker
}

fn nearest_curve_point(
    data: &CurveData,
    transform: &Transform,
    cursor: Pos2,
    threshold_px: f32,
) -> Option<PickResult> {
    let mut best: Option<(usize, f64, f64, f32)> = None;
    for (index, (&x, &y)) in data.x.iter().zip(&data.y).enumerate() {
        let dist_px = transform.data_to_pixel(x, y).distance(cursor);
        if dist_px <= threshold_px && best.is_none_or(|(_, _, _, best_dist)| dist_px < best_dist) {
            best = Some((index, x, y, dist_px));
        }
    }
    best.map(|(index, x, y, distance_px)| PickResult::CurvePoint {
        index,
        x,
        y,
        distance_px,
    })
}

fn pick_image_pixel(data: &ImageData, transform: &Transform, cursor: Pos2) -> Option<(u32, u32)> {
    let (col, row) = image_index(data, transform, cursor)?;
    image_pixel_pickable(data, col, row).then_some((col, row))
}

fn image_pixel_pickable(data: &ImageData, col: u32, row: u32) -> bool {
    if data.alpha <= 0.0 {
        return false;
    }
    match &data.pixels {
        ImagePixels::Scalar { .. } => true,
        ImagePixels::Rgba { data: pixels } => {
            let index = (row as usize)
                .saturating_mul(data.width as usize)
                .saturating_add(col as usize);
            pixels.get(index).is_some_and(|pixel| pixel[3] > 0)
        }
    }
}

fn pick_triangles(data: &Triangles, transform: &Transform, cursor: Pos2) -> bool {
    data.indices.iter().any(|tri| {
        let [a, b, c] = *tri;
        let a = a as usize;
        let b = b as usize;
        let c = c as usize;
        let Some((&ax, &ay)) = data.x.get(a).zip(data.y.get(a)) else {
            return false;
        };
        let Some((&bx, &by)) = data.x.get(b).zip(data.y.get(b)) else {
            return false;
        };
        let Some((&cx, &cy)) = data.x.get(c).zip(data.y.get(c)) else {
            return false;
        };
        point_in_triangle(
            cursor,
            transform.data_to_pixel(ax, ay),
            transform.data_to_pixel(bx, by),
            transform.data_to_pixel(cx, cy),
        )
    })
}

fn pick_shape(shape: &Shape, transform: &Transform, cursor: Pos2) -> bool {
    let tolerance = OVERLAY_PICK_TOLERANCE_PX + shape.line_width.max(1.0) * 0.5;
    match shape.kind {
        ShapeKind::HLine => shape.y.iter().any(|&y| {
            let py = transform.data_to_pixel(transform.x.min, y).y;
            (cursor.y - py).abs() <= tolerance
                && cursor.x >= transform.area.left() - tolerance
                && cursor.x <= transform.area.right() + tolerance
        }),
        ShapeKind::VLine => shape.x.iter().any(|&x| {
            let px = transform.data_to_pixel(x, transform.y.min).x;
            (cursor.x - px).abs() <= tolerance
                && cursor.y >= transform.area.top() - tolerance
                && cursor.y <= transform.area.bottom() + tolerance
        }),
        ShapeKind::Rectangle | ShapeKind::Polygon | ShapeKind::Polyline => {
            let points = shape.screen_points(transform);
            if points.len() < 2 {
                return false;
            }
            if shape.fill && shape.kind != ShapeKind::Polyline && point_in_polygon(cursor, &points)
            {
                return true;
            }

            let close_path = matches!(shape.kind, ShapeKind::Rectangle | ShapeKind::Polygon);
            let open_hit = points
                .windows(2)
                .any(|segment| distance_to_segment(cursor, segment[0], segment[1]) <= tolerance);
            let close_hit = close_path
                && distance_to_segment(cursor, points[points.len() - 1], points[0]) <= tolerance;
            open_hit || close_hit
        }
    }
}

fn pick_marker(marker: &Marker, transform: &Transform, cursor: Pos2) -> bool {
    // Delegates to the pure per-kind hit-test on the marker model so the
    // geometry lives in one place (shared with the interaction-layer drag pick).
    marker.pick(transform, cursor)
}

fn point_in_triangle(p: Pos2, a: Pos2, b: Pos2, c: Pos2) -> bool {
    let d1 = signed_area(p, a, b);
    let d2 = signed_area(p, b, c);
    let d3 = signed_area(p, c, a);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

fn point_in_polygon(point: Pos2, points: &[Pos2]) -> bool {
    let mut inside = false;
    let mut previous = points[points.len() - 1];
    for &current in points {
        let crosses = (current.y > point.y) != (previous.y > point.y);
        if crosses {
            let x = (previous.x - current.x) * (point.y - current.y) / (previous.y - current.y)
                + current.x;
            if point.x < x {
                inside = !inside;
            }
        }
        previous = current;
    }
    inside
}

fn signed_area(a: Pos2, b: Pos2, c: Pos2) -> f32 {
    (a.x - c.x) * (b.y - c.y) - (b.x - c.x) * (a.y - c.y)
}

fn distance_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let ab_len_sq = ab.length_sq();
    if ab_len_sq <= f32::EPSILON {
        return p.distance(a);
    }
    let ap = p - a;
    let t = (ap.dot(ab) / ab_len_sq).clamp(0.0, 1.0);
    p.distance(a + t * ab)
}

fn image_index(data: &ImageData, transform: &Transform, cursor: Pos2) -> Option<(u32, u32)> {
    if data.scale.0 <= 0.0 || data.scale.1 <= 0.0 {
        return None;
    }
    let (x, y) = transform.pixel_to_data(cursor);
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let col = ((x - data.origin.0) / data.scale.0).floor();
    let row = ((y - data.origin.1) / data.scale.1).floor();
    if col < 0.0 || row < 0.0 {
        return None;
    }
    let (col, row) = (col as u32, row as u32);
    (col < data.width && row < data.height).then_some((col, row))
}

/// Install [`WgpuResources`] into eframe's `RenderState`. A no-op if already
/// installed (idempotent). Call this at app creation with the `RenderState`
/// obtained from the `CreationContext`.
pub fn install(render_state: &RenderState) {
    let mut renderer = render_state.renderer.write();
    if renderer.callback_resources.get::<WgpuResources>().is_some() {
        return;
    }
    let resources = WgpuResources::new(&render_state.device, render_state.target_format);
    renderer.callback_resources.insert(resources);
}

/// Upload `image` to the GPU and make it the plot's current image. Requires
/// [`install`] to have run first. The image is uploaded once here; the per-frame
/// transform is applied by `ImageCallback`.
pub fn set_image(render_state: &RenderState, plot_id: PlotId, image: &ImageData) {
    set_images(render_state, plot_id, std::slice::from_ref(image));
}

/// Upload `images` to the GPU as the plot's full image set (replacing any
/// existing images), preserving order. Requires [`install`] to have run first.
pub fn set_images(render_state: &RenderState, plot_id: PlotId, images: &[ImageData]) {
    let mut renderer = render_state.renderer.write();
    let res: &mut WgpuResources = renderer
        .callback_resources
        .get_mut()
        .expect("WgpuResources not installed — call siplot::install() first");
    let gpu_images: Vec<GpuImage> = images
        .iter()
        .map(|image| {
            GpuImage::new(
                &render_state.device,
                &render_state.queue,
                &res.image_pipeline,
                image,
            )
        })
        .collect();
    let plot_data = res.get_or_insert_plot(&render_state.device, plot_id);
    plot_data.images = gpu_images;
}

/// Upload `curve` to the GPU as the plot's sole curve (replacing any existing
/// curves). Requires [`install`] to have run first. The vertices are uploaded
/// once here; the per-frame transform is applied by `CurveCallback`.
pub fn set_curve(render_state: &RenderState, plot_id: PlotId, curve: &CurveData) {
    set_curves(render_state, plot_id, std::slice::from_ref(curve));
}

/// Upload `curves` to the GPU as the plot's full curve set (replacing any
/// existing curves), preserving order. Each curve keeps its own Y-axis binding
/// ([`CurveData::y_axis`]). Requires [`install`] to have run first.
pub fn set_curves(render_state: &RenderState, plot_id: PlotId, curves: &[CurveData]) {
    let mut renderer = render_state.renderer.write();
    let res: &mut WgpuResources = renderer
        .callback_resources
        .get_mut()
        .expect("WgpuResources not installed — call siplot::install() first");
    let gpu_curves: Vec<GpuCurve> = curves
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
    let plot_data = res.get_or_insert_plot(&render_state.device, plot_id);
    plot_data.curves = gpu_curves;
}

/// Re-upload a `w × h` sub-region of the current image at `(x0, y0)` in place
/// (dirty update), reusing the existing texture. `data` is row-major, length
/// `w * h`. A no-op if no image has been set for `plot_id`. This is the
/// partial-write path for live updates (`doc/design.md` §11.7).
pub fn update_image_region(
    render_state: &RenderState,
    plot_id: PlotId,
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
        .expect("WgpuResources not installed — call siplot::install() first");
    if let Some(plot_data) = res.plots.get(&plot_id)
        && let Some(image) = plot_data.images.first()
    {
        image.update_region(&render_state.queue, x0, y0, w, h, data);
    }
}

/// Re-upload the first curve's vertices in place (dirty update). Convenience
/// for single-curve plots; see [`update_curve_at`] for a specific index.
pub fn update_curve(render_state: &RenderState, plot_id: PlotId, curve: &CurveData) {
    update_curve_at(render_state, plot_id, 0, curve);
}

/// Re-upload curve `index`'s vertices in place (dirty update), reusing the
/// existing GPU buffer when the vertex count fits; reallocates only if it grew
/// beyond the allocated capacity. If `index` is past the end (or no such curve
/// exists yet), the curve set is extended so `index` becomes the last curve
/// (`doc/design.md` §11.7).
pub fn update_curve_at(
    render_state: &RenderState,
    plot_id: PlotId,
    index: usize,
    curve: &CurveData,
) {
    let mut renderer = render_state.renderer.write();
    let res: &mut WgpuResources = renderer
        .callback_resources
        .get_mut()
        .expect("WgpuResources not installed — call siplot::install() first");
    // Try in-place update first (needs mutable access to the existing curve).
    let fits = res
        .plots
        .get_mut(&plot_id)
        .and_then(|d| d.curves.get_mut(index))
        .map(|existing| existing.update(&render_state.queue, curve))
        .unwrap_or(false);
    if !fits {
        let gpu = GpuCurve::new(
            &render_state.device,
            &render_state.queue,
            &res.curve_pipeline,
            curve,
        );
        let plot_data = res.get_or_insert_plot(&render_state.device, plot_id);
        match plot_data.curves.get_mut(index) {
            Some(slot) => *slot = gpu,
            None => plot_data.curves.push(gpu),
        }
    }
}

/// Paint callback that fills the data rect with a solid color. This is a
/// lightweight value re-registered by the egui side every frame; the actual GPU
/// resources are looked up from [`WgpuResources`].
pub(crate) struct ClearCallback {
    /// Linear color space, premultiplied RGBA.
    pub color: [f32; 4],
    /// Which plot's GPU data to use.
    pub plot_id: PlotId,
}

impl egui_wgpu::CallbackTrait for ClearCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let res: &mut WgpuResources = resources
            .get_mut()
            .expect("WgpuResources not installed — call siplot::install() at startup");
        let plot_data = res.get_or_insert_plot(device, self.plot_id);
        queue.write_buffer(&plot_data.color_uniform, 0, bytemuck::bytes_of(&self.color));
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
            .expect("WgpuResources not installed — call siplot::install() at startup");
        if let Some(plot_data) = res.plots.get(&self.plot_id) {
            render_pass.set_pipeline(&res.clear_pipeline);
            render_pass.set_bind_group(0, &plot_data.bind_group, &[]);
            render_pass.draw(0..3, 0..1);
        }
    }
}

/// Paint callback that draws the plot's image (if any) with the given per-frame
/// data→NDC transform. A no-op when no image has been set.
pub(crate) struct ImageCallback {
    /// data→NDC orthographic matrix from the plot's `Transform`.
    pub ortho: [[f32; 4]; 4],
    /// Per-axis log flag `[x, y]` (1.0 = log10), matching the transform.
    pub axis_log: [f32; 2],
    /// Which plot's GPU data to use.
    pub plot_id: PlotId,
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
            .expect("WgpuResources not installed — call siplot::install() at startup");
        if let Some(plot_data) = res.plots.get(&self.plot_id) {
            for image in &plot_data.images {
                image.write_uniforms(queue, self.ortho, self.axis_log);
            }
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
            .expect("WgpuResources not installed — call siplot::install() at startup");
        if let Some(plot_data) = res.plots.get(&self.plot_id) {
            for image in &plot_data.images {
                image.draw(render_pass, &res.image_pipeline);
            }
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
    /// Per-extra-axis data→NDC matrices, indexed by `YAxis::Extra(n)`. A curve
    /// bound to an index past the end (or to an axis with no range) falls back
    /// to the left axis.
    pub ortho_extra: Vec<[[f32; 4]; 4]>,
    /// Per-extra-axis log flag `[x, y]`, parallel to [`Self::ortho_extra`].
    pub axis_log_extra: Vec<[f32; 2]>,
    /// Data-area size in physical pixels, for the pixel-space line expansion.
    pub viewport_px: [f32; 2],
    /// Visible data-x window `(x_min, x_max)`, shared by both axes, used to
    /// re-decimate large curves for the current view (`doc/design.md` §13 D1).
    pub x_window: (f64, f64),
    /// Number of pixel columns to decimate into (the data-area pixel width), or
    /// `0` to disable decimation (e.g. a log x-axis, where equal data-x bins are
    /// not equal pixel columns).
    pub decimate_columns: u32,
    /// Which plot's GPU data to use.
    pub plot_id: PlotId,
}

impl CurveCallback {
    /// Pick the (ortho, axis_log) pair matching a curve's bound axis.
    fn matrices_for(&self, y_axis: crate::core::transform::YAxis) -> ([[f32; 4]; 4], [f32; 2]) {
        use crate::core::transform::YAxis;
        match y_axis {
            YAxis::Left => (self.ortho_left, self.axis_log_left),
            YAxis::Right => (self.ortho_right, self.axis_log_right),
            // Past-the-end / no-range extra axes fall back to the left matrix,
            // mirroring the right axis' y2-absent fallback.
            YAxis::Extra(n) => match (self.ortho_extra.get(n), self.axis_log_extra.get(n)) {
                (Some(&o), Some(&l)) => (o, l),
                _ => (self.ortho_left, self.axis_log_left),
            },
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
            .expect("WgpuResources not installed — call siplot::install() at startup");
        let (x_min, x_max) = self.x_window;
        if let Some(plot_data) = res.plots.get_mut(&self.plot_id) {
            for curve in &mut plot_data.curves {
                // Re-decimate to the current view first (a no-op once the view is
                // steady), recompute the dash arc length for the view (a no-op for
                // solid lines / a steady view), then stamp the per-frame uniforms.
                curve.ensure_decimated(queue, x_min, x_max, self.decimate_columns);
                let (ortho, axis_log) = self.matrices_for(curve.y_axis);
                curve.ensure_arclen(queue, ortho, axis_log, self.viewport_px);
                curve.write_uniforms(queue, ortho, axis_log, self.viewport_px);
            }
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
            .expect("WgpuResources not installed — call siplot::install() at startup");
        // Fills first (behind), then error bars, then lines, then markers, so
        // each stroke sits on top of its own fill, the line sits on top of its
        // error bars, and markers sit on top of every line.
        if let Some(plot_data) = res.plots.get(&self.plot_id) {
            for curve in &plot_data.curves {
                curve.draw_fill(render_pass, &res.curve_pipeline);
            }
            for curve in &plot_data.curves {
                curve.draw_errorbars(render_pass, &res.curve_pipeline);
            }
            for curve in &plot_data.curves {
                curve.draw(render_pass, &res.curve_pipeline);
            }
            for curve in &plot_data.curves {
                curve.draw_caps(render_pass, &res.curve_pipeline);
            }
            for curve in &plot_data.curves {
                curve.draw_markers(render_pass, &res.curve_pipeline);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Color32, Rect, pos2};

    use crate::core::colormap::Colormap;
    use crate::core::items::{Baseline, ErrorBars, LineStyle, Symbol};
    use crate::core::marker::{MarkerConstraint, MarkerKind};
    use crate::render::gpu_image::{ImagePixels, InterpolationMode};

    #[test]
    fn curve_spec_conversion_preserves_backend_fields() {
        let x = [0.0, 1.0, 2.0];
        let y = [3.0, 4.0, 5.0];
        let colors = [
            Color32::from_rgba_unmultiplied(10, 20, 30, 200),
            Color32::from_rgba_unmultiplied(40, 50, 60, 200),
            Color32::from_rgba_unmultiplied(70, 80, 90, 200),
        ];
        let spec = CurveSpec {
            x: &x,
            y: &y,
            color: CurveColor::PerVertex(&colors),
            gap_color: Some(Color32::from_rgba_unmultiplied(1, 2, 3, 200)),
            symbol: Some(Symbol::Square),
            line_width: 4.0,
            line_style: LineStyle::Dashed,
            y_axis: YAxis::Right,
            x_error: Some(ErrorBars::Symmetric(0.5)),
            y_error: Some(ErrorBars::PerPoint(vec![0.1, 0.2, 0.3])),
            fill: true,
            alpha: 0.5,
            symbol_size: 9.0,
            baseline: Baseline::PerPoint(vec![1.0, 1.5, 2.0]),
            x_label: None,
            y_label: None,
        };

        let curve = curve_data_from_spec(spec);
        assert_eq!(curve.x, x);
        assert_eq!(curve.y, y);
        assert_eq!(curve.color, apply_alpha(colors[0], 0.5));
        assert_eq!(
            curve.colors,
            Some(vec![
                apply_alpha(colors[0], 0.5),
                apply_alpha(colors[1], 0.5),
                apply_alpha(colors[2], 0.5),
            ])
        );
        assert_eq!(
            curve.gap_color,
            Some(apply_alpha(
                Color32::from_rgba_unmultiplied(1, 2, 3, 200),
                0.5
            ))
        );
        assert_eq!(curve.symbol, Some(Symbol::Square));
        assert_eq!(curve.width, 4.0);
        assert_eq!(curve.line_style, LineStyle::Dashed);
        assert_eq!(curve.y_axis, YAxis::Right);
        assert_eq!(curve.x_error, Some(ErrorBars::Symmetric(0.5)));
        assert_eq!(
            curve.y_error,
            Some(ErrorBars::PerPoint(vec![0.1, 0.2, 0.3]))
        );
        assert!(curve.fill);
        assert_eq!(curve.baseline, Baseline::PerPoint(vec![1.0, 1.5, 2.0]));
        assert_eq!(curve.marker_size, 9.0);
    }

    #[test]
    fn image_spec_conversion_sets_geometry_and_alpha() {
        let pixels = [0.0, 1.0, 2.0, 3.0];
        let mut spec = ImageSpec::scalar(2, 2, &pixels, Colormap::viridis(0.0, 3.0));
        spec.origin = (10.0, 20.0);
        spec.scale = (0.5, 2.0);
        spec.alpha = 1.5;
        let image = image_data_from_spec(spec);

        assert_eq!(image.width, 2);
        assert_eq!(image.height, 2);
        assert_eq!(image.origin, (10.0, 20.0));
        assert_eq!(image.scale, (0.5, 2.0));
        assert_eq!(image.alpha, 1.0);
        assert_eq!(image.interpolation, InterpolationMode::Nearest);
        match image.pixels {
            ImagePixels::Scalar { data, .. } => assert_eq!(data, pixels),
            ImagePixels::Rgba { .. } => panic!("expected scalar image"),
        }
    }

    #[test]
    fn image_spec_conversion_aggregates_and_scales() {
        // A 4×4 field aggregated by 2×2 MAX -> 2×2, with the scale multiplied by
        // the block factor so the image keeps the same data-space extent (silx
        // ImageDataAggregated: scale = sx*lodx, sy*lody).
        #[rustfmt::skip]
        let pixels = [
            0.0,  1.0,  2.0,  3.0,
            4.0,  5.0,  6.0,  7.0,
            8.0,  9.0,  10.0, 11.0,
            12.0, 13.0, 14.0, 15.0,
        ];
        let mut spec = ImageSpec::scalar(4, 4, &pixels, Colormap::viridis(0.0, 15.0))
            .with_aggregation(AggregationMode::Max, (2, 2));
        spec.scale = (1.0, 3.0);
        let image = image_data_from_spec(spec);

        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(image.scale, (2.0, 6.0)); // (1*2, 3*2)
        match image.pixels {
            ImagePixels::Scalar { data, .. } => assert_eq!(data, vec![5.0, 7.0, 13.0, 15.0]),
            ImagePixels::Rgba { .. } => panic!("expected scalar image"),
        }
    }

    #[test]
    fn marker_spec_conversion_selects_kind_and_style() {
        let marker = marker_from_spec(MarkerSpec {
            x: Some(1.0),
            y: Some(2.0),
            text: Some("peak"),
            color: Color32::RED,
            symbol: Some(Symbol::Diamond),
            symbol_size: 11.0,
            line_style: LineStyle::Dotted,
            line_width: 3.0,
            y_axis: YAxis::Right,
            bg_color: Some(Color32::BLACK),
            is_draggable: true,
            constraint: MarkerConstraint::Vertical,
        });

        assert_eq!(
            marker.kind,
            MarkerKind::Point {
                x: 1.0,
                y: 2.0,
                symbol: Symbol::Diamond,
                size: 11.0,
            }
        );
        assert_eq!(marker.text.as_deref(), Some("peak"));
        assert_eq!(marker.color, Color32::RED);
        assert_eq!(marker.bgcolor, Some(Color32::BLACK));
        assert_eq!(marker.line_style, LineStyle::Dotted);
        assert_eq!(marker.line_width, 3.0);
        assert_eq!(marker.y_axis, YAxis::Right);
        assert!(marker.is_draggable);
        assert_eq!(marker.constraint, MarkerConstraint::Vertical);
    }

    #[test]
    fn marker_accepts_full_symbol_catalog_not_just_the_old_seven() {
        // Markers share the curve `Symbol` catalog (silx `Marker` is a
        // `SymbolMixIn`). `CaretUp` was unreachable under the deleted 7-variant
        // `MarkerSymbol`; it must now round-trip through the spec conversion.
        let marker = marker_from_spec(MarkerSpec {
            x: Some(0.0),
            y: Some(0.0),
            text: None,
            color: Color32::WHITE,
            symbol: Some(Symbol::CaretUp),
            symbol_size: 8.0,
            line_style: LineStyle::Solid,
            line_width: 1.0,
            y_axis: YAxis::Left,
            bg_color: None,
            is_draggable: false,
            constraint: MarkerConstraint::None,
        });
        assert_eq!(
            marker.kind,
            MarkerKind::Point {
                x: 0.0,
                y: 0.0,
                symbol: Symbol::CaretUp,
                size: 8.0,
            }
        );
    }

    #[test]
    fn image_pick_uses_origin_scale_and_transform() {
        let image = ImageData::new(4, 3, vec![0.0; 12], Colormap::viridis(0.0, 1.0));
        let transform = Transform::new(
            0.0,
            4.0,
            0.0,
            3.0,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 300.0)),
        );

        assert_eq!(
            image_index(&image, &transform, pos2(150.0, 150.0)),
            Some((1, 1))
        );
        assert_eq!(image_index(&image, &transform, pos2(450.0, 150.0)), None);
        assert_eq!(
            pick_image_pixel(&image, &transform, pos2(150.0, 150.0)),
            Some((1, 1))
        );

        let rgba = ImageData::rgba(
            2,
            1,
            vec![
                Color32::from_rgba_unmultiplied(255, 0, 0, 0).to_srgba_unmultiplied(),
                Color32::from_rgba_unmultiplied(255, 0, 0, 128).to_srgba_unmultiplied(),
            ],
        );
        let rgba_transform = Transform::new(
            0.0,
            2.0,
            0.0,
            1.0,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(200.0, 100.0)),
        );
        assert_eq!(
            pick_image_pixel(&rgba, &rgba_transform, pos2(50.0, 50.0)),
            None
        );
        assert_eq!(
            pick_image_pixel(&rgba, &rgba_transform, pos2(150.0, 50.0)),
            Some((1, 0))
        );
    }

    #[test]
    fn overlay_pick_helpers_cover_shapes_markers_and_triangles() {
        let transform = Transform::new(
            0.0,
            10.0,
            0.0,
            10.0,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0)),
        );

        let shape = Shape::rectangle(2.0, 2.0, 4.0, 4.0).with_line_width(2.0);
        assert!(pick_shape(&shape, &transform, pos2(20.0, 70.0)));
        assert!(!pick_shape(&shape, &transform, pos2(80.0, 20.0)));

        let filled_shape = Shape::rectangle(2.0, 2.0, 4.0, 4.0).with_fill(true);
        assert!(pick_shape(&filled_shape, &transform, pos2(30.0, 70.0)));

        let marker = Marker::point(5.0, 5.0).with_symbol_size(10.0);
        assert!(pick_marker(&marker, &transform, pos2(51.0, 49.0)));
        assert!(!pick_marker(&marker, &transform, pos2(80.0, 20.0)));

        let tris = Triangles::new(
            vec![1.0, 6.0, 1.0],
            vec![1.0, 1.0, 6.0],
            vec![[0, 1, 2]],
            vec![Color32::RED, Color32::GREEN, Color32::BLUE],
        );
        assert!(pick_triangles(&tris, &transform, pos2(25.0, 75.0)));
        assert!(!pick_triangles(&tris, &transform, pos2(90.0, 10.0)));
    }
}
