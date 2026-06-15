//! Empirical headless render check for the plot3d P0.2 GPU path
//! (`render::gpu_scene3d`): the line/triangle pipelines, the offscreen
//! color+depth render, and the blit into egui's depth-less pass.
//!
//! Static review of the pipeline can confirm it *compiles* and that the WGSL is
//! valid (see `render/shaders.rs`), but not that geometry projects to the right
//! place, that the depth buffer actually resolves occlusion, or that the blit
//! lands the offscreen image in the widget rect. This test proves all three on a
//! real (or software) GPU via `egui_kittest`'s wgpu renderer:
//!
//! - A near RED triangle and a far GREEN triangle both cover screen centre. The
//!   green one is uploaded *after* red, so without depth testing it would paint
//!   over red. The test asserts centre is RED → the depth buffer wins, not draw
//!   order.
//! - A MAGENTA line band (the `LineList` pipeline) is drawn in clear sky above
//!   both triangles; the test asserts magenta pixels exist → lines render.
//! - Image corners are BLACK (the offscreen clear) and the green triangle is
//!   visible around the red one → background + far geometry both blit through.
//!
//! Needs a GPU (real or software): it builds a wgpu `RenderState` and reads back
//! the rendered texture, mirroring `tests/mask_pointer_offset.rs`.

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use siplot::egui::{self, Color32};
use siplot::egui_wgpu::RenderState;
use siplot::{Camera, Scene3dGeometry, Vec3, install_scene3d, paint_scene3d, set_scene3d};
use std::cell::RefCell;
use std::rc::Rc;

const SCENE_ID: u64 = 0;
/// Square window so the perspective aspect is ~1 (no x/y distortion of the
/// hand-placed geometry). Points == pixels at `ppp = 1`.
const WIN: f32 = 300.0;

/// A camera at `(0,0,5)` looking down `-z`, up `+y`, fovy 30° — the silx default
/// orientation. The geometry below is hand-placed in this view.
struct App {
    camera: Camera,
    /// Rect the scene was painted into on the last frame (the central panel adds
    /// a margin, so the scene is a sub-rect of the full image).
    last_rect: Option<egui::Rect>,
}

impl App {
    fn new(rs: &RenderState) -> Self {
        install_scene3d(rs);

        let mut g = Scene3dGeometry::new();

        // Near RED triangle (z = +1, closest to the camera at z = 5), apex up.
        g.add_triangle(
            [-0.3, -0.3, 1.0],
            [0.3, -0.3, 1.0],
            [0.0, 0.3, 1.0],
            Color32::from_rgb(255, 0, 0),
        );
        // Far GREEN triangle (z = -2), larger, also covering centre — uploaded
        // AFTER red on purpose: only depth testing keeps red on top at centre.
        g.add_triangle(
            [-0.6, -0.6, -2.0],
            [0.6, -0.6, -2.0],
            [0.0, 0.6, -2.0],
            Color32::from_rgb(0, 200, 0),
        );

        // MAGENTA line band in the clear sky above both triangles (y ≈ 0.8,
        // above the green apex at y = 0.6). Five stacked full-width lines so the
        // band is a few pixels tall and robust to sub-pixel line placement.
        let magenta = Color32::from_rgb(220, 0, 220);
        for i in 0..5 {
            let y = 0.78 + 0.01 * i as f32;
            g.add_line([-0.5, y, 0.0], [0.5, y, 0.0], magenta);
        }

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
fn scene3d_renders_depth_tested_geometry_and_lines() {
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

    // Sample at a rect-relative fraction (ppp = 1, so points == pixels).
    let at = |fx: f32, fy: f32| -> (u8, u8, u8) {
        let x = ((rect.min.x + fx * rect.width()).round() as usize).min(iw - 1);
        let y = ((rect.min.y + fy * rect.height()).round() as usize).min(ih - 1);
        let i = (y * iw + x) * 4;
        (raw[i], raw[i + 1], raw[i + 2])
    };

    let is_red = |(r, g, b): (u8, u8, u8)| r > 150 && g < 90 && b < 90;
    let is_green = |(r, g, b): (u8, u8, u8)| g > 120 && r < 90 && b < 90;
    let is_magenta = |(r, g, b): (u8, u8, u8)| r > 120 && b > 120 && g < 90;
    let is_black = |(r, g, b): (u8, u8, u8)| r < 50 && g < 50 && b < 50;

    // 1. Depth correctness: centre is RED even though GREEN was drawn after and
    //    covers the same area — the depth test (not draw order) decides.
    let centre = at(0.5, 0.5);
    assert!(
        is_red(centre),
        "scene centre should be the near RED triangle (depth-tested over the \
         later-drawn far green); got rgb{centre:?}"
    );

    // 2. Corners are the offscreen clear (BLACK): neither triangle reaches them,
    //    and the blit fills the rect without garbage.
    for (fx, fy) in [(0.03, 0.03), (0.97, 0.03), (0.03, 0.97), (0.97, 0.97)] {
        let c = at(fx, fy);
        assert!(
            is_black(c),
            "corner ({fx},{fy}) should be the black background clear; got rgb{c:?}"
        );
    }

    // 3 & 4. Count GREEN and MAGENTA across the rect: the far triangle must be
    //    visible (green ring around red) and the line pipeline must render.
    let mut green = 0usize;
    let mut magenta = 0usize;
    let x0 = rect.min.x.max(0.0) as usize;
    let y0 = rect.min.y.max(0.0) as usize;
    let x1 = (rect.max.x as usize).min(iw);
    let y1 = (rect.max.y as usize).min(ih);
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * iw + x) * 4;
            let px = (raw[i], raw[i + 1], raw[i + 2]);
            if is_green(px) {
                green += 1;
            }
            if is_magenta(px) {
                magenta += 1;
            }
        }
    }
    assert!(
        green > 0,
        "far GREEN triangle should be visible around the red one (got 0 green px)"
    );
    assert!(
        magenta >= 10,
        "MAGENTA line band should render (LineList pipeline); got {magenta} magenta px"
    );
}
