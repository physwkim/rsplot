//! Viewpoint-preset checks for `SceneWidget` (plot3d P3.1): the seven named
//! viewpoints (silx `actions/viewpoint.py`) orient the camera along the
//! expected axis, the auto-rotate primitive (silx `RotateViewpoint`) orbits the
//! scene, and the `viewpoint_menu` drop-down (silx `ViewpointToolButton`) wires
//! a menu click through to `set_viewpoint`.
//!
//! The preset/rotate properties are asserted directly on the camera (no pixels
//! needed); the menu is exercised end-to-end through the egui/AccessKit harness:
//! clicking "View" → "Top" must reorient the camera, proving the full
//! button → menu item → `set_viewpoint` chain.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use siplot::egui;
use siplot::egui_wgpu::RenderState;
use siplot::{CameraFace, SceneWidget, Vec3, viewpoint_menu};
use std::cell::RefCell;
use std::rc::Rc;

const WIN: f32 = 320.0;
const UNIT: (Vec3, Vec3) = (Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0));

/// The expected eye-offset direction (eye − centre, normalized) for each preset.
/// silx looks *along* `-direction`, so the eye sits on the `+axis` side:
/// Front looks -Z (eye +Z), Top looks -Y (eye +Y), etc.; Side is the
/// `(1,1,1)` three-quarter view.
fn expected_offset(face: CameraFace) -> Vec3 {
    match face {
        CameraFace::Front => Vec3::new(0.0, 0.0, 1.0),
        CameraFace::Back => Vec3::new(0.0, 0.0, -1.0),
        CameraFace::Top => Vec3::new(0.0, 1.0, 0.0),
        CameraFace::Bottom => Vec3::new(0.0, -1.0, 0.0),
        CameraFace::Right => Vec3::new(1.0, 0.0, 0.0),
        CameraFace::Left => Vec3::new(-1.0, 0.0, 0.0),
        CameraFace::Side => Vec3::new(1.0, 1.0, 1.0).normalized(),
    }
}

#[test]
fn viewpoint_presets_orient_camera_along_expected_axis() {
    let rs = create_render_state(default_wgpu_setup());
    let mut scene = SceneWidget::new(&rs, 0);
    scene.set_bounds(&rs, UNIT);
    let center = scene.center();

    for face in [
        CameraFace::Front,
        CameraFace::Back,
        CameraFace::Top,
        CameraFace::Bottom,
        CameraFace::Right,
        CameraFace::Left,
        CameraFace::Side,
    ] {
        scene.set_viewpoint(face);
        let offset = (scene.camera().extrinsic.position() - center).normalized();
        let want = expected_offset(face);
        let alignment = offset.dot(want);
        assert!(
            alignment > 0.999,
            "viewpoint {face:?}: eye offset {offset:?} should align with {want:?} (dot={alignment})"
        );
    }
}

#[test]
fn fresh_scene_opens_on_the_front_view_like_silx() {
    // silx's viewport camera opens face-on down -Z (viewport.py:221-223,
    // camera.py:50): eye on the +Z side, not the (1,1,1) 'side' three-quarter.
    let rs = create_render_state(default_wgpu_setup());
    let mut scene = SceneWidget::new(&rs, 7);
    scene.set_bounds(&rs, UNIT);
    let center = scene.center();

    let offset = (scene.camera().extrinsic.position() - center).normalized();
    let alignment = offset.dot(expected_offset(CameraFace::Front));
    assert!(
        alignment > 0.999,
        "default view should be Front (eye +Z), got offset {offset:?} (dot={alignment})"
    );
}

#[test]
fn rotate_scene_orbits_around_the_center_preserving_radius() {
    let rs = create_render_state(default_wgpu_setup());
    let mut scene = SceneWidget::new(&rs, 1);
    scene.set_bounds(&rs, UNIT);
    let center = scene.center();

    scene.set_viewpoint(CameraFace::Side);
    let before = scene.camera().extrinsic.position();
    let r_before = (before - center).length();

    scene.rotate_scene(45.0);
    let after = scene.camera().extrinsic.position();
    let r_after = (after - center).length();

    assert_ne!(before, after, "rotate_scene must move the camera");
    assert!(
        (r_before - r_after).abs() < 1e-3,
        "an orbit preserves the distance to the centre: {r_before} vs {r_after}"
    );
}

/// A `SceneWidget` under a `viewpoint_menu` drop-down — the menu's click wiring
/// is exercised through the harness.
struct MenuApp {
    scene: SceneWidget,
}

impl MenuApp {
    fn new(rs: &RenderState) -> Self {
        let mut scene = SceneWidget::new(rs, 2);
        scene.set_bounds(rs, UNIT);
        Self { scene }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let _ = viewpoint_menu(ui, &mut self.scene);
        });
        self.scene.show(ui);
    }
}

#[test]
fn viewpoint_menu_click_reorients_the_camera() {
    let rs = create_render_state(default_wgpu_setup());
    let app = Rc::new(RefCell::new(MenuApp::new(&rs)));
    let renderer = WgpuTestRenderer::from_render_state(rs.clone());

    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WIN, WIN))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| app_ui.borrow_mut().ui(ui));

    // Frame 1: the default side view.
    harness.step();
    let center = app.borrow().scene.center();
    let before = app.borrow().scene.camera().extrinsic.position();

    // Open the "View" menu, then pick "Top".
    harness.get_by_label("View").click();
    harness.run();
    harness.get_by_label("Top").click();
    harness.run();

    let after = app.borrow().scene.camera().extrinsic.position();
    let offset = (after - center).normalized();
    assert_ne!(
        before, after,
        "selecting a viewpoint from the menu must move the camera"
    );
    assert!(
        offset.dot(Vec3::new(0.0, 1.0, 0.0)) > 0.999,
        "menu 'Top' must orient the camera along +Y; got offset {offset:?}"
    );
}
