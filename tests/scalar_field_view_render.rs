//! Headless end-to-end checks for `ScalarFieldView` (plot3d P2.3c): the view
//! owns a `ScalarField3D`, frames the camera to the volume on the first
//! `set_data`, builds and uploads the iso-surface / cut-plane geometry through
//! its inner `SceneWidget`, and paints it via the offscreen-render-then-blit
//! path.
//!
//! Two properties are covered:
//! - an iso-surface set through the view renders (magenta — a colour the chrome
//!   cannot make), and `clear_isosurfaces` rebuilds the scene without it;
//! - the camera is framed only on the **first** `set_data` (silx
//!   `centerScene`-once), so a later `set_data` with differently-sized data
//!   keeps the user's viewpoint.

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::ScalarFieldView;
use rsplot::egui;
use rsplot::egui::Color32;
use rsplot::egui_wgpu::RenderState;
use std::cell::RefCell;
use std::rc::Rc;

const WIN: f32 = 320.0;

/// An `n×n×n` field whose interior `(n-2)³` block is `1.0` and the rest `0.0`:
/// an iso-surface at level `0.5` wraps the central block.
fn blob(n: usize) -> Vec<f32> {
    let mut data = vec![0.0f32; n * n * n];
    for z in 1..n - 1 {
        for y in 1..n - 1 {
            for x in 1..n - 1 {
                data[(z * n + y) * n + x] = 1.0;
            }
        }
    }
    data
}

/// Count magenta pixels: red and blue both high, green low. The chrome (grey
/// wireframe + pure R/G/B axes + dark background) cannot produce magenta, so any
/// magenta pixel comes from the lit iso-surface mesh.
fn count_magenta(raw: &[u8], iw: usize, ih: usize) -> usize {
    (0..iw * ih)
        .filter(|&px| {
            let i = px * 4;
            raw[i] > 120 && raw[i + 2] > 120 && raw[i + 1] < 80
        })
        .count()
}

struct IsoApp {
    view: ScalarFieldView,
}

impl IsoApp {
    fn new(rs: &RenderState) -> Self {
        let mut view = ScalarFieldView::new(rs, 5);
        assert!(
            view.set_data(rs, &blob(5), 5, 5, 5),
            "5³ blob is valid data"
        );
        // A magenta iso-surface around the central block.
        view.add_isosurface(rs, 0.5, Color32::from_rgb(255, 0, 255));
        Self { view }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        self.view.show(ui);
    }
}

#[test]
fn scalar_field_view_renders_isosurface_and_clear_removes_it() {
    let rs = create_render_state(default_wgpu_setup());
    let app = Rc::new(RefCell::new(IsoApp::new(&rs)));
    // Clone the render state so the test body can still rebuild geometry after
    // the renderer takes ownership (both handles share the same GPU + renderer).
    let renderer = WgpuTestRenderer::from_render_state(rs.clone());

    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WIN, WIN))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| app_ui.borrow_mut().ui(ui));

    // Frame 1: the iso-surface must be visible.
    harness.step();
    let image = harness.render().expect("headless wgpu render");
    let (iw, ih) = (image.width() as usize, image.height() as usize);
    let magenta_iso = count_magenta(image.as_raw(), iw, ih);
    assert!(
        magenta_iso > 100,
        "the iso-surface (magenta) must render through ScalarFieldView; only {magenta_iso} px"
    );

    // Frame 2: clearing the iso-surfaces rebuilds the scene without the mesh.
    app.borrow_mut().view.clear_isosurfaces(&rs);
    harness.step();
    let image2 = harness.render().expect("headless wgpu render");
    let magenta_clear = count_magenta(image2.as_raw(), iw, ih);
    assert!(
        magenta_clear < magenta_iso / 10,
        "clear_isosurfaces must rebuild without the mesh; {magenta_clear} magenta px remain (was {magenta_iso})"
    );
}

#[test]
fn scalar_field_view_frames_camera_only_on_first_set_data() {
    let rs = create_render_state(default_wgpu_setup());

    let mut view = ScalarFieldView::new(&rs, 6);
    assert!(
        view.set_data(&rs, &blob(5), 5, 5, 5),
        "5³ blob is valid data"
    );
    // First data frames the camera to the 5³ volume box.
    let p1 = view.scene().camera().extrinsic.position();

    // A second, larger field updates the bounds but must NOT re-frame the camera.
    assert!(
        view.set_data(&rs, &blob(6), 6, 6, 6),
        "6³ blob is valid data"
    );
    let p2 = view.scene().camera().extrinsic.position();
    assert_eq!(
        p1, p2,
        "the camera must not re-frame on the second set_data (silx centerScene-once)"
    );

    // Sanity: framing a fresh view to a 6³ box lands at a different eye position
    // than the 5³ framing, so p1 == p2 is genuine preservation, not coincidence.
    let mut fresh = ScalarFieldView::new(&rs, 9);
    assert!(
        fresh.set_data(&rs, &blob(6), 6, 6, 6),
        "6³ blob is valid data"
    );
    let p_fresh6 = fresh.scene().camera().extrinsic.position();
    assert_ne!(
        p1, p_fresh6,
        "framing to a 6³ box should differ from a 5³ box"
    );
}
