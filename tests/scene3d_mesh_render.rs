//! Headless render check for the plot3d P1.2 shaded-mesh pipeline
//! (`render::gpu_scene3d` meshes + `scene3d_mesh.wgsl`): directional headlight
//! Phong shading (silx `DirectionalLight`, ambient 0.3 + diffuse 0.7).
//!
//! Two camera-facing white quads with the *same* geometry and colour but
//! different surface normals are rendered. The lighting is computed from the
//! normal, so:
//!
//! - the quad whose normal faces the headlight (camera-space `(0,0,1)`) is fully
//!   lit → ambient + diffuse ≈ 1.0 → white;
//! - the quad whose normal is perpendicular to the light gets ambient only ≈ 0.3
//!   → dark grey.
//!
//! Asserting the first is far brighter than the second proves the shader shades
//! per-normal (not flat), with the right ambient/diffuse split. Using identical
//! geometry isolates lighting from foreshortening/visibility.

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui::{self, Color32};
use rsplot::egui_wgpu::RenderState;
use rsplot::{Camera, Scene3dGeometry, Vec3, install_scene3d, paint_scene3d, set_scene3d};
use std::cell::RefCell;
use std::rc::Rc;

const SCENE_ID: u64 = 0;
const WIN: f32 = 300.0;

struct App {
    camera: Camera,
    last_rect: Option<egui::Rect>,
}

/// A camera-facing white quad of half-extent 0.3 centred at `(cx, 0, 0)`, with a
/// chosen surface `normal` (two triangles).
fn push_quad(g: &mut Scene3dGeometry, cx: f32, normal: [f32; 3]) {
    let h = 0.3;
    let p = |dx: f32, dy: f32| [cx + dx, dy, 0.0];
    let (a, b, c, d) = (p(-h, -h), p(h, -h), p(h, h), p(-h, h));
    g.add_mesh_triangle([a, b, c], Color32::WHITE, [normal; 3]);
    g.add_mesh_triangle([a, c, d], Color32::WHITE, [normal; 3]);
}

impl App {
    fn new(rs: &RenderState) -> Self {
        install_scene3d(rs);
        let mut g = Scene3dGeometry::new();
        // Left quad: normal toward the headlight → fully lit.
        push_quad(&mut g, -0.5, [0.0, 0.0, 1.0]);
        // Right quad: normal perpendicular to the light → ambient only.
        push_quad(&mut g, 0.5, [0.0, 1.0, 0.0]);
        set_scene3d(rs, SCENE_ID, &g);

        let camera = Camera::new(
            30.0,
            0.1,
            100.0,
            (1.0, 1.0),
            Vec3::new(0.0, 0.0, 5.0),
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
fn scene3d_mesh_is_shaded_by_its_normal() {
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
    let gray = |(r, g, b): (u8, u8, u8)| (r as u32 + g as u32 + b as u32) / 3;

    // Corners: the black offscreen clear.
    for (fx, fy) in [(0.03, 0.03), (0.97, 0.97)] {
        let (r, g, b) = at(fx, fy);
        assert!(r < 40 && g < 40 && b < 40, "corner should be black clear");
    }

    // Left quad centroid ≈ fx 0.31, right ≈ 0.69 (world ±0.5 at z=0, fovy 30°).
    let lit = at(0.314, 0.5);
    let ambient = at(0.686, 0.5);

    // Fully-lit quad: ambient(0.3) + diffuse(0.7)·1 = 1.0 → ~white.
    assert!(
        gray(lit) > 200,
        "normal-toward-light quad should be fully lit (~white); got rgb{lit:?}"
    );
    // Perpendicular quad: ambient only ≈ 0.3 → ~76/255 grey, clearly not black
    // (proves ambient is applied) and clearly not white (diffuse term is off).
    let a = gray(ambient);
    assert!(
        (45..120).contains(&a),
        "perpendicular quad should be ambient-only grey (~76); got rgb{ambient:?}"
    );
    assert!(
        gray(lit) > a + 100,
        "lit quad must be much brighter than the ambient-only quad; \
         lit={} ambient={a}",
        gray(lit)
    );
}
