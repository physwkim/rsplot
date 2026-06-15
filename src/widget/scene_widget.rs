//! [`SceneWidget`] — an interactive 3D scene inside an egui `Ui`.
//!
//! The plot3d analogue of [`crate::widget::plot_widget::PlotView`]: it owns a
//! [`Camera`], the scene bounds, and the scene geometry; on each frame it
//! handles orbit/pan/zoom pointer interaction (driven by the pure helpers in
//! [`crate::core::scene3d::interaction`]) and registers the wgpu paint callback
//! ([`paint_scene3d`]) that renders the scene offscreen and blits it in.
//!
//! Port of silx `Plot3DWidget` + `SceneWidget`'s default `RotateCameraControl`:
//! left-drag orbits around the scene centre, right-drag pans, the wheel zooms.
//! The scene chrome (bounding box + RGB axes) is generated from the bounds via
//! [`Scene3dGeometry::add_bounding_box_with_axes`]; data-item geometry set with
//! [`SceneWidget::set_geometry`] is merged in beneath the chrome (every channel,
//! via [`Scene3dGeometry::extend_from`]).

use egui::{Color32, PointerButton, Pos2, Response, Sense, Ui};
use egui_wgpu::RenderState;

use crate::core::scene3d::camera::{Camera, CameraDirection, CameraFace};
use crate::core::scene3d::interaction::{OrbitDrag, PanDrag, window_to_ndc};
use crate::core::scene3d::mat4::Vec3;
use crate::render::gpu_scene3d::{
    Scene3dGeometry, Scene3dId, install_scene3d, paint_scene3d, set_scene3d, snapshot_scene3d,
};

/// Default scene background (a dark neutral grey, as in silx's 3D views).
const DEFAULT_BACKGROUND: Color32 = Color32::from_gray(30);
/// Default bounding-box / wireframe stroke colour.
const DEFAULT_BOX_COLOR: Color32 = Color32::from_gray(200);

/// An interactive 3D scene widget. Construct with [`SceneWidget::new`], optionally
/// set the data bounds and content geometry, then call [`SceneWidget::show`] each
/// frame.
pub struct SceneWidget {
    id: Scene3dId,
    camera: Camera,
    /// Axis-aligned scene bounds `(min, max)`; the chrome and camera framing
    /// derive from these.
    bounds: (Vec3, Vec3),
    box_color: Color32,
    background: Color32,
    /// Data-item geometry (excludes the box/axes chrome, which is regenerated
    /// from `bounds` on every upload). Empty until [`SceneWidget::set_geometry`].
    content: Scene3dGeometry,
    /// In-progress orbit drag (left button), if any.
    orbit: Option<OrbitDrag>,
    /// In-progress pan drag (right button), if any.
    pan: Option<PanDrag>,
}

impl SceneWidget {
    /// Create a scene widget bound to `id`, installing the 3D GPU resources into
    /// `render_state` if needed. Starts with a unit-box scene framed from the
    /// silx "side" viewpoint.
    pub fn new(render_state: &RenderState, id: Scene3dId) -> Self {
        install_scene3d(render_state);

        let bounds = (Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0));
        let mut camera = Camera::new(
            30.0,
            0.1,
            100.0,
            (1.0, 1.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        // Default to the silx "side" three-quarter view, then frame the bounds.
        camera.extrinsic.reset(CameraFace::Side);
        camera.reset_camera(bounds);
        camera.adjust_depth_extent(bounds);

        let widget = SceneWidget {
            id,
            camera,
            bounds,
            box_color: DEFAULT_BOX_COLOR,
            background: DEFAULT_BACKGROUND,
            content: Scene3dGeometry::new(),
            orbit: None,
            pan: None,
        };
        widget.upload(render_state);
        widget
    }

    /// Set the scene background colour (used to clear the offscreen target).
    pub fn set_background(&mut self, color: Color32) {
        self.background = color;
    }

    /// The scene's centre of bounds (centre of rotation for orbit/pan).
    pub fn center(&self) -> Vec3 {
        (self.bounds.0 + self.bounds.1) * 0.5
    }

    /// Read-only access to the camera.
    pub fn camera(&self) -> &Camera {
        &self.camera
    }

    /// Mutable access to the camera (e.g. to apply a viewpoint preset).
    pub fn camera_mut(&mut self) -> &mut Camera {
        &mut self.camera
    }

    /// Set the axis-aligned scene bounds, re-frame the camera, and re-upload the
    /// chrome geometry.
    pub fn set_bounds(&mut self, render_state: &RenderState, bounds: (Vec3, Vec3)) {
        self.bounds = bounds;
        self.camera.reset_camera(bounds);
        self.camera.adjust_depth_extent(bounds);
        self.upload(render_state);
    }

    /// Set the scene bounds and re-upload the chrome **without** re-framing the
    /// camera, so the user's current orbit/zoom is preserved. Used when the data
    /// changes but the viewpoint should stay put — silx re-frames (`centerScene`)
    /// only on the first `setData`, not on subsequent updates. The depth frustum
    /// is still adjusted so the new bounds stay clipped correctly.
    pub fn set_bounds_keep_view(&mut self, render_state: &RenderState, bounds: (Vec3, Vec3)) {
        self.bounds = bounds;
        self.camera.adjust_depth_extent(bounds);
        self.upload(render_state);
    }

    /// Replace the data-item geometry (the box/axes chrome is kept) and re-upload.
    pub fn set_geometry(&mut self, render_state: &RenderState, geometry: Scene3dGeometry) {
        self.content = geometry;
        self.upload(render_state);
    }

    /// Re-frame the camera to the current bounds without changing orientation.
    pub fn reset_camera(&mut self) {
        self.camera.reset_camera(self.bounds);
        self.camera.adjust_depth_extent(self.bounds);
    }

    /// Set the camera to one of the predefined viewpoints (front/back/left/
    /// right/top/bottom/side) and re-frame the scene. Port of silx
    /// `_SetViewpointAction`: `camera.extrinsic.reset(face)` followed by
    /// `centerScene()`.
    pub fn set_viewpoint(&mut self, face: CameraFace) {
        self.camera.extrinsic.reset(face);
        self.camera.reset_camera(self.bounds);
        self.camera.adjust_depth_extent(self.bounds);
    }

    /// Orbit the scene about its centre around the vertical axis by
    /// `angle_degrees` (positive = the silx "left" orbit direction). Port of
    /// silx `RotateViewpoint`'s per-frame `viewport.orbitCamera("left", angle)`;
    /// the caller drives the animation (e.g. `angle = deg_per_sec * dt` each
    /// frame, requesting a repaint). The depth frustum is re-adjusted so the
    /// scene stays clipped correctly.
    pub fn rotate_scene(&mut self, angle_degrees: f32) {
        let center = self.center();
        self.camera
            .extrinsic
            .orbit(CameraDirection::Left, center, angle_degrees);
        self.camera.adjust_depth_extent(self.bounds);
    }

    /// Build the combined geometry (chrome + content) and upload it for this
    /// scene id.
    fn upload(&self, render_state: &RenderState) {
        let mut geometry = Scene3dGeometry::new();
        geometry.add_bounding_box_with_axes(self.bounds, self.box_color);
        // Append every data-item channel beneath the chrome (points, meshes,
        // images, and textured meshes too — not only lines/triangles), so the
        // P1.x/P2.x items render through the widget.
        geometry.extend_from(&self.content);
        set_scene3d(render_state, self.id, &geometry);
    }

    /// Lay out the scene over the available space, handle interaction, and paint.
    /// Returns the egui [`Response`] for the scene rect.
    pub fn show(&mut self, ui: &mut Ui) -> Response {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let ppp = ui.ctx().pixels_per_point();
        let size_px = (
            (rect.width() * ppp).max(1.0),
            (rect.height() * ppp).max(1.0),
        );
        // Keep the camera aspect in sync so interaction un-projection matches the
        // rendered frame (paint_scene3d uses the same physical pixel size).
        self.camera.set_size(size_px);
        let center = self.center();

        // Pointer position in physical pixels relative to the scene rect's origin.
        let to_local = |p: Pos2| ((p.x - rect.min.x) * ppp, (p.y - rect.min.y) * ppp);
        // Where the button went down. A drag is only *recognised* after the pointer
        // clears egui's click-vs-drag threshold, by which point
        // `interact_pointer_pos` has already moved; anchoring the orbit/pan at the
        // press origin keeps that threshold travel from being silently dropped.
        let press_origin = ui.ctx().input(|i| i.pointer.press_origin());

        // Orbit — left drag.
        if response.drag_started_by(PointerButton::Primary)
            && let Some(p) = press_origin
        {
            self.orbit = Some(OrbitDrag::begin(&self.camera, to_local(p), center));
        }
        if response.dragged_by(PointerButton::Primary)
            && let (Some(orbit), Some(p)) = (self.orbit, response.interact_pointer_pos())
        {
            orbit.update(&mut self.camera, to_local(p), size_px);
        }
        if response.drag_stopped_by(PointerButton::Primary) {
            self.orbit = None;
        }

        // Pan — right drag.
        if response.drag_started_by(PointerButton::Secondary)
            && let Some(p) = press_origin
        {
            self.pan = Some(PanDrag::begin(&self.camera, to_local(p), size_px, center));
        }
        if response.dragged_by(PointerButton::Secondary)
            && let (Some(mut pan), Some(p)) = (self.pan, response.interact_pointer_pos())
        {
            pan.update(&mut self.camera, to_local(p), size_px);
            self.pan = Some(pan);
        }
        if response.drag_stopped_by(PointerButton::Secondary) {
            self.pan = None;
        }

        // Zoom — wheel while hovering.
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0
            && let Some(p) = response.hover_pos()
        {
            let (nx, ny) = window_to_ndc(to_local(p), size_px);
            let ndc_z = self.camera.matrix().transform_point(center, true).z;
            self.camera.zoom_at((nx, ny), ndc_z, scroll > 0.0);
        }

        // Keep the scene within the depth frustum after any interaction.
        self.camera.adjust_depth_extent(self.bounds);

        paint_scene3d(ui, rect, self.id, &self.camera, self.background);
        response
    }

    /// Render the current scene at `size_px` physical pixels from the widget's
    /// camera, returning it as tightly packed RGBA8 (`width * height * 4`, top
    /// row first), or `None` if the GPU readback fails. Off-screen and
    /// synchronous — independent of the egui frame loop — so it suits saving a
    /// scene to an image file (pair with [`crate::encode_png`]).
    pub fn snapshot(&self, render_state: &RenderState, size_px: (u32, u32)) -> Option<Vec<u8>> {
        snapshot_scene3d(
            render_state,
            self.id,
            &self.camera,
            self.background,
            size_px,
        )
    }
}

/// The seven predefined viewpoints in silx's menu order, each with its silx menu
/// label and tooltip (`actions/viewpoint.py`).
const VIEWPOINT_PRESETS: [(CameraFace, &str, &str); 7] = [
    (CameraFace::Front, "Front", "View along the -Z axis"),
    (CameraFace::Back, "Back", "View along the +Z axis"),
    (CameraFace::Top, "Top", "View along the -Y axis"),
    (CameraFace::Bottom, "Bottom", "View along the +Y axis"),
    (CameraFace::Right, "Right", "View along the -X axis"),
    (CameraFace::Left, "Left", "View along the +X axis"),
    (CameraFace::Side, "Side", "Side view"),
];

/// Draw a viewpoint drop-down menu button (port of silx
/// `tools.ViewpointTools.ViewpointToolButton`): a `View` button whose menu sets
/// one of the seven predefined viewpoints on `scene`. Returns the chosen
/// [`CameraFace`] when a preset is selected this frame, otherwise `None`.
pub fn viewpoint_menu(ui: &mut Ui, scene: &mut SceneWidget) -> Option<CameraFace> {
    let mut chosen = None;
    ui.menu_button("View", |ui| {
        for (face, label, tip) in VIEWPOINT_PRESETS {
            if ui.button(label).on_hover_text(tip).clicked() {
                scene.set_viewpoint(face);
                chosen = Some(face);
                ui.close();
            }
        }
    })
    .response
    .on_hover_text("Reset the viewpoint to a defined position");
    chosen
}
