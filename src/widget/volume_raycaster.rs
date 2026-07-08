//! `VolumeRaycaster` — an interactive direct-volume-rendering widget.
//!
//! Renders a 3-D RGBA field by ray-marching it on the GPU (front-to-back alpha
//! compositing), with orbit / pan / zoom driven by the same camera and
//! interaction state machines as [`crate::SceneWidget`]. Unlike
//! [`crate::ScalarFieldView`] (which extracts iso-surface *geometry*), this shows
//! the volume itself: every voxel's colour and opacity contribute along the ray,
//! so a caller supplies a straight-alpha RGBA volume (e.g. hue = chemistry,
//! alpha = density) and gets a VTK-style ray-cast.
//!
//! Lifecycle: [`VolumeRaycaster::new`] installs the shared GPU pipeline;
//! [`VolumeRaycaster::set_volume`] uploads a `(depth, height, width)` RGBA8
//! volume; [`VolumeRaycaster::show`] handles interaction and paints.

use egui::{PointerButton, Pos2, Response, Sense, Ui};
use egui_wgpu::RenderState;

use crate::core::scene3d::camera::Camera;
use crate::core::scene3d::interaction::{OrbitDrag, PanDrag, window_to_ndc};
use crate::core::scene3d::mat4::{Mat4, Vec3};
use crate::render::volume_raycaster::{
    VolumeFrame, VolumeId, install_volume_raycaster, paint_volume_raycaster,
    remove_volume_raycaster, set_volume_raycaster, volume_bounds,
};

/// Upper bound on samples per ray; beyond this a single frame can outrun the
/// OS GPU-timeout watchdog on modest hardware.
const MAX_STEPS: u32 = 4096;

/// Interactive GPU direct-volume-rendering widget. See the module docs.
pub struct VolumeRaycaster {
    id: VolumeId,
    camera: Camera,
    /// World-space axis-aligned box the volume occupies (centred, aspect-kept).
    bounds: (Vec3, Vec3),
    has_volume: bool,

    // Ray-march quality / transfer knobs.
    steps: u32,
    alpha_scale: f32,
    cull_floor: f32,

    // In-flight interaction.
    orbit: Option<OrbitDrag>,
    pan: Option<PanDrag>,
}

impl VolumeRaycaster {
    /// Create a raycaster bound to scene key `id`, installing the shared pipeline
    /// into `render_state` (idempotent). The camera looks down `-z`; the first
    /// [`set_volume`](Self::set_volume) frames it to the volume.
    ///
    /// The `id` keys the uploaded 3D texture, so distinct volumes need distinct
    /// ids (two views sharing an id share the last-uploaded texture). Cameras are
    /// not shared: each paint builds its own uniforms, so same-id views still
    /// render with their own viewpoint.
    pub fn new(render_state: &RenderState, id: VolumeId) -> Self {
        install_volume_raycaster(render_state);
        let camera = Camera::new(
            30.0,
            0.1,
            100.0,
            (1.0, 1.0),
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        Self {
            id,
            camera,
            bounds: (Vec3::new(-0.5, -0.5, -0.5), Vec3::new(0.5, 0.5, 0.5)),
            has_volume: false,
            steps: 256,
            alpha_scale: 1.0,
            cull_floor: 0.0,
            orbit: None,
            pan: None,
        }
    }

    /// Upload a `(depth, height, width)` RGBA8 volume (row-major, straight
    /// alpha). The first upload frames the camera to the volume; later uploads
    /// keep the viewpoint so a changing field animates in place.
    pub fn set_volume(
        &mut self,
        render_state: &RenderState,
        rgba: &[u8],
        depth: usize,
        height: usize,
        width: usize,
    ) {
        set_volume_raycaster(render_state, self.id, rgba, depth, height, width);
        let (mn, mx) = volume_bounds(depth, height, width);
        self.bounds = (Vec3::from_array(mn), Vec3::from_array(mx));
        if !self.has_volume {
            self.camera.reset_camera(self.bounds);
            self.has_volume = true;
        }
    }

    /// Number of samples per ray (higher = smoother, slower). Default 256,
    /// clamped to `[1, MAX_STEPS]` — an unbounded count can stall the GPU long
    /// enough to trip the OS device-timeout reset.
    pub fn set_steps(&mut self, steps: u32) {
        self.steps = steps.clamp(1, MAX_STEPS);
    }

    /// Global opacity multiplier applied to each sample's alpha. Default 1.0.
    pub fn set_alpha_scale(&mut self, alpha_scale: f32) {
        self.alpha_scale = alpha_scale.max(0.0);
    }

    /// Skip samples whose (straight) alpha is at or below this. Default 0.0.
    pub fn set_cull_floor(&mut self, cull_floor: f32) {
        self.cull_floor = cull_floor.clamp(0.0, 1.0);
    }

    /// Re-frame the camera to the current volume bounds.
    pub fn reset_view(&mut self) {
        self.camera.reset_camera(self.bounds);
    }

    /// Free this view's uploaded GPU volume (the 3D texture). Its VRAM is
    /// otherwise held for the app's lifetime, since the shared resources keep one
    /// entry per id. After this the view paints nothing until the next
    /// [`set_volume`](Self::set_volume).
    pub fn remove(&mut self, render_state: &RenderState) {
        remove_volume_raycaster(render_state, self.id);
        self.has_volume = false;
    }

    /// Lay the view over the available space, handle orbit/pan/zoom, and paint.
    /// Left drag orbits the volume centre; Ctrl/Cmd-drag pans; the wheel zooms.
    pub fn show(&mut self, ui: &mut Ui) -> Response {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let ppp = ui.ctx().pixels_per_point();
        let size_px = (
            (rect.width() * ppp).max(1.0),
            (rect.height() * ppp).max(1.0),
        );
        self.camera.set_size(size_px);
        let center = (self.bounds.0 + self.bounds.1) * 0.5;

        let to_local = |p: Pos2| ((p.x - rect.min.x) * ppp, (p.y - rect.min.y) * ppp);
        let press_origin = ui.ctx().input(|i| i.pointer.press_origin());

        // Begin a gesture at the press origin (before egui's drag threshold moves
        // the pointer), mirroring `SceneWidget`. No geometry to pick against, so
        // orbit pivots on the box centre and pan sits on the box-centre depth
        // plane (the same NDC-z the wheel zoom anchors on) — anchoring on the far
        // plane would make the pan drift faster than the cursor.
        if response.drag_started_by(PointerButton::Primary)
            && let Some(p) = press_origin
        {
            let ctrl = ui.ctx().input(|i| i.modifiers.command);
            let win = to_local(p);
            if ctrl {
                let ndc_z = self.camera.matrix().transform_point(center, true).z;
                self.pan = Some(PanDrag::begin(win, size_px, ndc_z));
            } else {
                self.orbit = Some(OrbitDrag::begin(&self.camera, win, center));
            }
        }
        if response.dragged_by(PointerButton::Primary)
            && let Some(p) = response.interact_pointer_pos()
        {
            let local = to_local(p);
            if let Some(orbit) = self.orbit {
                orbit.update(&mut self.camera, local, size_px);
            }
            if let Some(mut pan) = self.pan {
                pan.update(&mut self.camera, local, size_px);
                self.pan = Some(pan);
            }
        }
        if response.drag_stopped_by(PointerButton::Primary) {
            self.orbit = None;
            self.pan = None;
        }

        // Wheel zoom, anchored at the box-centre depth (no depth buffer to pick).
        if let Some(p) = response.hover_pos() {
            let steps = ui.input(|i| wheel_zoom_steps(&i.events));
            if !steps.is_empty() {
                let ndc = window_to_ndc(to_local(p), size_px);
                let ndc_z = self.camera.matrix().transform_point(center, true).z;
                for zoom_in in steps {
                    self.camera.zoom_at(ndc, ndc_z, zoom_in);
                }
            }
        }

        self.camera.adjust_depth_extent(self.bounds);

        let inv_mvp = self
            .camera
            .matrix()
            .inverse()
            .map_or_else(|| Mat4::IDENTITY.to_gpu_cols(), |m| m.to_gpu_cols());
        let frame = VolumeFrame {
            id: self.id,
            inv_mvp,
            params: [self.steps as f32, self.alpha_scale, self.cull_floor],
        };
        paint_volume_raycaster(ui, rect, frame);
        response
    }
}

/// One `±0.2` zoom step per raw `MouseWheel` event (true = zoom in). Mirrors
/// `SceneWidget`'s wheel handling: a fixed step per event, magnitude-independent,
/// read from the raw events rather than the smoothed per-frame delta.
fn wheel_zoom_steps(events: &[egui::Event]) -> Vec<bool> {
    events
        .iter()
        .filter_map(|e| match e {
            egui::Event::MouseWheel { delta, .. } if delta.y != 0.0 => Some(delta.y > 0.0),
            _ => None,
        })
        .collect()
}
