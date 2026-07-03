//! [`SceneWidget`] — an interactive 3D scene inside an egui `Ui`.
//!
//! The plot3d analogue of [`crate::widget::plot_widget::PlotView`]: it owns a
//! [`Camera`], the scene bounds, and the scene geometry; on each frame it
//! handles orbit/pan/zoom pointer interaction (driven by the pure helpers in
//! [`crate::core::scene3d::interaction`]) and registers the wgpu paint callback
//! ([`paint_scene3d`]) that renders the scene offscreen and blits it in.
//!
//! Port of silx `Plot3DWidget` + `SceneWidget`'s default `RotateCameraControl`
//! (`orbitAroundCenter=False`, wheel mode `"position"`): left-drag orbits around
//! the picked point under the press (scene centre on a miss), right-drag pans on
//! the picked depth plane, and the wheel zooms keeping the picked pixel
//! invariant — each gesture anchors on [`SceneWidget::pick`], the CPU stand-in
//! for silx's depth-buffer read.
//! The scene chrome (bounding box + RGB axes) is generated from the bounds via
//! [`Scene3dGeometry::add_bounding_box_with_axes`]; data-item geometry set with
//! [`SceneWidget::set_geometry`] is merged in beneath the chrome (every channel,
//! via [`Scene3dGeometry::extend_from`]).

use egui::{Color32, PointerButton, Pos2, Response, Sense, Ui};
use egui_wgpu::RenderState;

use crate::core::scene3d::camera::{Camera, CameraDirection, CameraFace};
use crate::core::scene3d::interaction::{OrbitDrag, PanDrag, window_to_ndc};
use crate::core::scene3d::mat4::Vec3;
use crate::core::scene3d::pick::{picking_segment, segment_triangles_intersection};
use crate::core::scene3d::plane::segment_plane_intersect;
use crate::render::gpu_scene3d::{
    Scene3dFog, Scene3dGeometry, Scene3dId, Scene3dShading, install_scene3d, paint_scene3d_with,
    set_scene3d, snapshot_scene3d_with,
};

/// Default scene background: silx `Plot3DWidget` clears to `(0.2, 0.2, 0.2, 1.0)`
/// (`Plot3DWidget.py:161`), i.e. grey 51.
const DEFAULT_BACKGROUND: Color32 = Color32::from_gray(51);
/// Default foreground (bounding-box / wireframe) colour: white, as silx
/// `SceneWidget` (`SceneWidget.py:374` `_foregroundColor = 1., 1., 1., 1.`,
/// matching `primitives.py` `BoxWithAxes(color=(1., 1., 1., 1.))`).
const DEFAULT_FOREGROUND: Color32 = Color32::WHITE;
/// Default text colour (axis/tick labels): white, as silx `SceneWidget`
/// (`SceneWidget.py:373` `_textColor = 1., 1., 1., 1.`).
const DEFAULT_TEXT_COLOR: Color32 = Color32::WHITE;

/// Fog mode of the scene — port of silx `Plot3DWidget.FogMode`
/// (`Plot3DWidget.py:119-124`): either no fog or a linear fog fading the scene
/// content toward the background colour over its depth extent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FogMode {
    /// No fog (the silx default).
    #[default]
    None,
    /// Linear fog over the scene's camera-space depth extent.
    Linear,
}

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
    text_color: Color32,
    background: Color32,
    /// Fog mode (silx `Plot3DWidget.setFogMode`); default off.
    fog_mode: FogMode,
    /// Phong shininess of the viewport light (silx `viewport.light.shininess`);
    /// 0 (no specular) for plain scenes, 32 in [`crate::ScalarFieldView`].
    light_shininess: f32,
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
            box_color: DEFAULT_FOREGROUND,
            text_color: DEFAULT_TEXT_COLOR,
            background: DEFAULT_BACKGROUND,
            fog_mode: FogMode::None,
            light_shininess: 0.0,
            content: Scene3dGeometry::new(),
            orbit: None,
            pan: None,
        };
        widget.upload(render_state);
        widget
    }

    /// Set the scene background colour (used to clear the offscreen target).
    /// Port of silx `Plot3DWidget.setBackgroundColor`.
    pub fn set_background(&mut self, color: Color32) {
        self.background = color;
    }

    /// The scene background colour (silx `Plot3DWidget.getBackgroundColor`).
    pub fn background(&self) -> Color32 {
        self.background
    }

    /// Set the foreground colour — the bounding-box wireframe stroke. Port of
    /// silx `SceneWidget.setForegroundColor` (`SceneWidget.py:648`, applied to
    /// the root `BoxWithAxes`). Re-uploads the chrome geometry.
    pub fn set_foreground_color(&mut self, render_state: &RenderState, color: Color32) {
        if self.box_color != color {
            self.box_color = color;
            self.upload(render_state);
        }
    }

    /// The foreground (bounding-box) colour (silx `SceneWidget.getForegroundColor`).
    pub fn foreground_color(&self) -> Color32 {
        self.box_color
    }

    /// Set the text colour used for the scene's axis and tick labels. Port of
    /// silx `SceneWidget.setTextColor` (`SceneWidget.py:623`, forwarded to the
    /// root `LabelledAxes.tickColor`).
    pub fn set_text_color(&mut self, color: Color32) {
        self.text_color = color;
    }

    /// The text colour (silx `SceneWidget.getTextColor`).
    pub fn text_color(&self) -> Color32 {
        self.text_color
    }

    /// Set the fog mode. Port of silx `Plot3DWidget.setFogMode`
    /// (`Plot3DWidget.py:279-288`: `viewport.fog.isOn = mode is FogMode.LINEAR`).
    pub fn set_fog_mode(&mut self, mode: FogMode) {
        self.fog_mode = mode;
    }

    /// The fog mode (silx `Plot3DWidget.getFogMode`).
    pub fn fog_mode(&self) -> FogMode {
        self.fog_mode
    }

    /// Set the Phong shininess of the viewport's directional light; `0`
    /// disables the specular term (the silx `DirectionalLight` default,
    /// `function.py:296-300`). `ScalarFieldView` sets 32
    /// (`ScalarFieldView.py:928`).
    pub fn set_light_shininess(&mut self, shininess: f32) {
        self.light_shininess = shininess;
    }

    /// The viewport light's Phong shininess (`0` = no specular).
    pub fn light_shininess(&self) -> f32 {
        self.light_shininess
    }

    /// The per-frame shading options (fog + shininess) matching this widget's
    /// current state — the one owner used by both [`SceneWidget::show`] and
    /// [`SceneWidget::snapshot`], so the snapshot matches the screen.
    fn shading(&self) -> Scene3dShading {
        Scene3dShading {
            fog: (self.fog_mode == FogMode::Linear)
                .then(|| Scene3dFog::linear(&self.camera, self.bounds, self.background)),
            shininess: self.light_shininess,
        }
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

        // Orbit — left drag. Pivot on the picked object point under the press
        // (silx CameraSelectRotate.beginDrag with orbitAroundCenter=False,
        // interaction.py:150-161), falling back to the scene centre on a miss.
        if response.drag_started_by(PointerButton::Primary)
            && let Some(p) = press_origin
        {
            let win = to_local(p);
            let pivot = self
                .pick(window_to_ndc(win, size_px))
                .map_or(center, |hit| hit.position);
            self.orbit = Some(OrbitDrag::begin(&self.camera, win, pivot));
        }
        if response.dragged_by(PointerButton::Primary)
            && let (Some(orbit), Some(p)) = (self.orbit, response.interact_pointer_pos())
        {
            orbit.update(&mut self.camera, to_local(p), size_px);
        }
        if response.drag_stopped_by(PointerButton::Primary) {
            self.orbit = None;
        }

        // Pan — right drag. The pan plane sits at the picked depth under the
        // press (silx CameraSelectPan.beginDrag: `ndcZ = _pickNdcZGL(x, y)`,
        // interaction.py:226-235); a miss reads the cleared depth buffer, i.e.
        // the far plane (NDC z = 1).
        if response.drag_started_by(PointerButton::Secondary)
            && let Some(p) = press_origin
        {
            let win = to_local(p);
            let plane_z = self
                .pick(window_to_ndc(win, size_px))
                .map_or(1.0, |hit| hit.ndc_depth);
            self.pan = Some(PanDrag::begin(win, size_px, plane_z));
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

        // Zoom — wheel while hovering. Un-project the cursor at its own picked
        // depth so the pixel under the mouse stays invariant (silx CameraWheel
        // mode "position", interaction.py:329-341); a miss anchors at the far
        // plane, as silx's depth-buffer read does.
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0
            && let Some(p) = response.hover_pos()
        {
            let ndc = window_to_ndc(to_local(p), size_px);
            let ndc_z = self.pick(ndc).map_or(1.0, |hit| hit.ndc_depth);
            self.camera.zoom_at(ndc, ndc_z, scroll > 0.0);
        }

        // Keep the scene within the depth frustum after any interaction.
        self.camera.adjust_depth_extent(self.bounds);

        paint_scene3d_with(
            ui,
            rect,
            self.id,
            &self.camera,
            self.background,
            self.shading(),
        );
        response
    }

    /// Render the current scene at `size_px` physical pixels from the widget's
    /// camera, returning it as tightly packed RGBA8 (`width * height * 4`, top
    /// row first), or `None` if the GPU readback fails. Off-screen and
    /// synchronous — independent of the egui frame loop — so it suits saving a
    /// scene to an image file (pair with [`crate::encode_png`]).
    pub fn snapshot(&self, render_state: &RenderState, size_px: (u32, u32)) -> Option<Vec<u8>> {
        snapshot_scene3d_with(
            render_state,
            self.id,
            &self.camera,
            self.background,
            size_px,
            self.shading(),
        )
    }

    /// Pick the scene geometry under a click at normalized device coordinates
    /// `ndc` (`x, y ∈ [-1, 1]`; convert a widget-local pixel with
    /// [`window_to_ndc`]). Returns the nearest hit (smallest NDC depth) among the
    /// data surfaces and scatter points, or `None` if the ray misses everything
    /// or the camera is singular.
    ///
    /// Port of silx `SceneWidget.pickItems`: it builds the picking segment
    /// ([`picking_segment`]) and intersects it with the data triangles
    /// ([`segment_triangles_intersection`] over
    /// `Scene3dGeometry::pick_triangles` — flat fills, lit meshes,
    /// iso-surfaces); scatter points are hit-tested by projecting each to NDC
    /// and keeping those within [`PICK_POINT_TOLERANCE_PX`] of the click;
    /// Scatter2D-LINES data points via the [`PICK_LINE_TOLERANCE_PX`] square
    /// test (silx `_pickPoints`); image quads by plane intersection with a
    /// pixel row/column payload (silx `_Image._pickFull`). The bounding-box /
    /// axes chrome is excluded (it is not part of the data content), matching
    /// silx picking scene items rather than the frame.
    ///
    /// Uses the camera's current viewport size, so call after [`SceneWidget::show`]
    /// has run this frame (it syncs the camera aspect to the rendered rect).
    pub fn pick(&self, ndc: (f32, f32)) -> Option<ScenePick> {
        let segment = picking_segment(&self.camera, ndc)?;
        let mvp = self.camera.matrix();

        let mut best: Option<ScenePick> = None;
        let mut consider = |cand: ScenePick| {
            if best.is_none_or(|b| cand.ndc_depth < b.ndc_depth) {
                best = Some(cand);
            }
        };

        // Surfaces: the nearest triangle hit (the list is depth-sorted).
        let triangles = self.content.pick_triangles();
        if let Some(hit) = segment_triangles_intersection(segment, &triangles).first() {
            let position = hit.position(segment.0, segment.1);
            let ndc_depth = mvp.transform_point(position, true).z;
            consider(ScenePick {
                position,
                ndc_depth,
                kind: ScenePickKind::Surface,
            });
        }

        // Scatter points: nearest within the click tolerance, in front of the camera.
        let (vw, vh) = self.camera.size();
        let radius_ndc_x = 2.0 * PICK_POINT_TOLERANCE_PX / vw.max(1.0);
        let radius_ndc_y = 2.0 * PICK_POINT_TOLERANCE_PX / vh.max(1.0);
        for (index, world) in self.content.pick_points().into_iter().enumerate() {
            let p = mvp.transform_point(world, true);
            if !(-1.0..=1.0).contains(&p.z) {
                continue; // outside the depth frustum (behind camera / clipped)
            }
            let dx = (p.x - ndc.0) / radius_ndc_x;
            let dy = (p.y - ndc.1) / radius_ndc_y;
            if dx * dx + dy * dy <= 1.0 {
                consider(ScenePick {
                    position: world,
                    ndc_depth: p.z,
                    kind: ScenePickKind::Point { index },
                });
            }
        }

        // Pick-only anchors (Scatter2D LINES mode): silx `_pickPoints` with a
        // 5 px threshold (items/scatter.py:424-434, called with threshold=5.0
        // from :511) — a per-axis square test `|Δndc| < threshold/viewport`
        // plus the segment depth range `z ∈ [-1, 1]`, nearest by depth.
        let square_ndc_x = PICK_LINE_TOLERANCE_PX / vw.max(1.0);
        let square_ndc_y = PICK_LINE_TOLERANCE_PX / vh.max(1.0);
        for (index, &pos) in self.content.line_pick_anchors().iter().enumerate() {
            let world = Vec3::new(pos[0], pos[1], pos[2]);
            let p = mvp.transform_point(world, true);
            if !(-1.0..=1.0).contains(&p.z) {
                continue;
            }
            if (p.x - ndc.0).abs() < square_ndc_x && (p.y - ndc.1).abs() < square_ndc_y {
                consider(ScenePick {
                    position: world,
                    ndc_depth: p.z,
                    kind: ScenePickKind::LinePoint { index },
                });
            }
        }

        // Image quads: silx `_Image._pickFull` (items/image.py:55-84) —
        // intersect the picking segment with the image plane; exactly one
        // crossing (a coplanar segment picks nothing), reject in front of the
        // origin edge, then floor to the pixel row/column and bounds-check.
        // The layer is axis-aligned in its z = origin.z plane, pixel (col,row)
        // spanning origin + (col..col+1, row..row+1)·scale.
        for (image, layer) in self.content.images.iter().enumerate() {
            let hits = segment_plane_intersect(
                segment.0,
                segment.1,
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::new(0.0, 0.0, layer.origin[2]),
            );
            let [hit] = hits.as_slice() else {
                continue; // no crossing, or segment coplanar with the image
            };
            // World → image-frame (column, row) continuous coordinates.
            let col_f = (hit.x - layer.origin[0]) / layer.scale[0];
            let row_f = (hit.y - layer.origin[1]) / layer.scale[1];
            if col_f < 0.0 || row_f < 0.0 {
                continue; // outside image (silx rejects negatives)
            }
            let (col, row) = (col_f as usize, row_f as usize);
            if row >= layer.height as usize || col >= layer.width as usize {
                continue; // outside image
            }
            consider(ScenePick {
                position: *hit,
                ndc_depth: mvp.transform_point(*hit, true).z,
                kind: ScenePickKind::Image { image, row, col },
            });
        }

        best
    }
}

/// Pixel tolerance for scatter-point picking: a point is pickable when it
/// projects within this many pixels of the click. silx tests against the
/// marker footprint; a fixed tolerance is a documented simplification (the
/// per-point marker size is not threaded into the pick path).
pub const PICK_POINT_TOLERANCE_PX: f32 = 7.0;

/// Pixel threshold of the pick-anchor square test — silx picks `Scatter2D`
/// LINES mode at its data points with `threshold=5.0`
/// (`items/scatter.py:511`), tested as `|Δndc| < threshold/viewport` per axis
/// (`items/scatter.py:424-428`).
pub const PICK_LINE_TOLERANCE_PX: f32 = 5.0;

/// What [`SceneWidget::pick`] hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScenePickKind {
    /// A data surface (a triangle of a fill, lit mesh, or iso-surface).
    Surface,
    /// A scatter point, with its index in the points channel.
    Point { index: usize },
    /// A pick-only data-point anchor (Scatter2D LINES mode — silx picks the
    /// data points, not the segments; `items/scatter.py:509-511`), with its
    /// index in the anchors channel.
    LinePoint { index: usize },
    /// An image-quad pixel (silx `_Image._pickFull`, `items/image.py:55-84`),
    /// with the layer's index in the images channel and the picked pixel's
    /// row/column in the image data.
    Image {
        image: usize,
        row: usize,
        col: usize,
    },
}

/// The nearest scene hit from [`SceneWidget::pick`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScenePick {
    /// World-space position of the hit.
    pub position: Vec3,
    /// NDC depth `z ∈ [-1, 1]` of the hit (smaller is nearer the camera); the
    /// key used to choose the nearest across surfaces and points.
    pub ndc_depth: f32,
    /// Which channel was hit.
    pub kind: ScenePickKind,
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
