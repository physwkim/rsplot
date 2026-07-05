//! Headless render check for the plot3d P2.1 marching-cubes isosurface
//! (`ScalarField3D` + `Isosurface` → `render::gpu_scene3d` meshes). A 5×5×5
//! field with a solid 3×3×3 high block at its centre, iso-level 0.5, yields a
//! gold cube whose `+z` face (world z = 4) spans world x,y ∈ [1, 4].
//!
//! Looking straight down `−z` at the volume centre, that face fills the middle of
//! the view; its outward normal (gradient descent, `invert_normals = true`) points
//! at the camera, so the headlight fully lights it. Asserting the centre is lit
//! gold (red-dominant, blue ≈ 0) and the corners are the black clear proves the
//! whole chain: marching cubes runs, the `zyx → xyz` swap + 0.5 offset places the
//! surface inside the volume box, and the lit mesh path renders it.

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui::{self, Color32};
use rsplot::egui_wgpu::RenderState;
use rsplot::{
    Camera, DEFAULT_ISOSURFACE_COLOR, ScalarField3D, Scene3dGeometry, Vec3, install_scene3d,
    paint_scene3d, set_scene3d,
};
use std::cell::RefCell;
use std::rc::Rc;

const SCENE_ID: u64 = 0;
const WIN: f32 = 300.0;

struct App {
    camera: Camera,
    last_rect: Option<egui::Rect>,
}

impl App {
    fn new(rs: &RenderState) -> Self {
        install_scene3d(rs);

        // 5×5×5 field, central 3×3×3 block = 1.0, rest 0.0.
        let (d, h, w) = (5usize, 5usize, 5usize);
        let mut data = vec![0.0f32; d * h * w];
        for z in 1..4 {
            for y in 1..4 {
                for x in 1..4 {
                    data[(z * h + y) * w + x] = 1.0;
                }
            }
        }
        let mut sf = ScalarField3D::new().with_data(&data, d, h, w);
        sf.add_isosurface(0.5, DEFAULT_ISOSURFACE_COLOR);

        let mut g = Scene3dGeometry::new();
        sf.append_to(&mut g);
        set_scene3d(rs, SCENE_ID, &g);

        // Look down −z at the volume centre (2.5, 2.5, 2.5); the +z cube face is
        // at z=4, so it faces the camera.
        let camera = Camera::new(
            30.0,
            0.1,
            100.0,
            (1.0, 1.0),
            Vec3::new(2.5, 2.5, 12.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        Self {
            camera,
            last_rect: None,
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let (rect, _resp) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
        paint_scene3d(ui, rect, SCENE_ID, &self.camera, Color32::BLACK);
        self.last_rect = Some(rect);
    }
}

#[test]
fn scene3d_isosurface_renders_lit_gold_cube() {
    let rs = create_render_state(default_wgpu_setup());
    let app = Rc::new(RefCell::new(App::new(&rs)));
    let renderer = WgpuTestRenderer::from_render_state(rs);

    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WIN, WIN))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| app_ui.borrow_mut().ui(ui));

    harness.step();
    let rect = app.borrow().last_rect.expect("scene rect captured");

    let image = harness.render().expect("headless wgpu render");
    let (iw, ih) = (image.width() as usize, image.height() as usize);
    let raw = image.as_raw();

    let at = |fx: f32, fy: f32| -> (u8, u8, u8) {
        let x = ((rect.min.x + fx * rect.width()).round() as usize).min(iw - 1);
        let y = ((rect.min.y + fy * rect.height()).round() as usize).min(ih - 1);
        let i = (y * iw + x) * 4;
        (raw[i], raw[i + 1], raw[i + 2])
    };

    // Corners: outside the cube projection → the black offscreen clear.
    for (fx, fy) in [(0.03, 0.03), (0.97, 0.97), (0.03, 0.97), (0.97, 0.03)] {
        let (r, g, b) = at(fx, fy);
        assert!(
            r < 40 && g < 40 && b < 40,
            "corner ({fx},{fy}) should be black clear; got rgb({r},{g},{b})"
        );
    }

    // Centre: the fully-lit +z gold face. Gold #FFD700 → lit linear bytes
    // ≈ (255, 173, 0): red-dominant, green medium, blue ≈ 0.
    let (r, g, b) = at(0.5, 0.5);
    assert!(r > 150, "centre red should be high (lit gold); got r={r}");
    assert!(
        b < 50,
        "centre blue should be ≈0 (gold has no blue); got b={b}"
    );
    assert!(r > g, "gold is red-dominant; got r={r} g={g}");
    assert!(
        (60..240).contains(&g),
        "gold has a medium green component (not red, not white); got g={g}"
    );
}
