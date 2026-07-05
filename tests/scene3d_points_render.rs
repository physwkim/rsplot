//! Headless render check for the plot3d P1.1 point-sprite pipeline
//! (`render::gpu_scene3d` points + `scene3d_points.wgsl`): billboarded, pixel-
//! sized scatter markers with per-marker coverage shapes.
//!
//! Static checks confirm the WGSL is valid and the code compiles, but not that a
//! point projects to the right pixel, is sized in pixels (rather than wgpu's
//! 1×1 `PointList`), or that the marker `alpha_symbol`/discard actually carves
//! the shape. This proves all three on a real (or software) GPU:
//!
//! - A RED **square** and a BLUE **circle** of equal pixel size sit symmetric
//!   about screen centre (square left, circle right). The test asserts:
//!   - both render and the square is far larger than 1px (sizing works),
//!   - the square covers visibly more pixels than the circle (the circle's
//!     corners are discarded → marker shapes differ),
//!   - the red centroid is left of centre and the blue centroid right of it
//!     (billboard projection places points correctly).
//! - Image corners are the BLACK offscreen clear.

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui::{self, Color32};
use rsplot::egui_wgpu::RenderState;
use rsplot::{
    Camera, PointMarker, Scene3dGeometry, Vec3, install_scene3d, paint_scene3d, set_scene3d,
};
use std::cell::RefCell;
use std::rc::Rc;

const SCENE_ID: u64 = 0;
const WIN: f32 = 300.0;
/// Marker sprite diameter in pixels — large enough that a 1×1 `PointList`
/// fallback would be unmistakably distinguishable from a real sized sprite.
const SIZE: f32 = 40.0;

struct App {
    camera: Camera,
    last_rect: Option<egui::Rect>,
}

impl App {
    fn new(rs: &RenderState) -> Self {
        install_scene3d(rs);

        let mut g = Scene3dGeometry::new();
        // Equal-size square (red) and circle (blue) at z = 0, symmetric about the
        // optical axis → identical magnification, so a pixel-count difference can
        // only come from the marker shape.
        g.add_point(
            [-0.5, 0.0, 0.0],
            Color32::from_rgb(255, 0, 0),
            SIZE,
            PointMarker::Square,
        );
        g.add_point(
            [0.5, 0.0, 0.0],
            Color32::from_rgb(0, 0, 255),
            SIZE,
            PointMarker::Circle,
        );
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
fn scene3d_renders_sized_billboard_point_markers() {
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
    let is_red = |(r, g, b): (u8, u8, u8)| r > 150 && g < 90 && b < 90;
    let is_blue = |(r, g, b): (u8, u8, u8)| b > 150 && r < 90 && g < 90;
    let is_black = |(r, g, b): (u8, u8, u8)| r < 50 && g < 50 && b < 50;

    // Corners: the black offscreen clear.
    for (fx, fy) in [(0.03, 0.03), (0.97, 0.03), (0.03, 0.97), (0.97, 0.97)] {
        let c = at(fx, fy);
        assert!(
            is_black(c),
            "corner ({fx},{fy}) should be the black clear; got rgb{c:?}"
        );
    }

    // Count each marker's pixels and accumulate its centroid (rect-relative).
    let (x0, y0) = (rect.min.x.max(0.0) as usize, rect.min.y.max(0.0) as usize);
    let (x1, y1) = ((rect.max.x as usize).min(iw), (rect.max.y as usize).min(ih));
    let (mut red, mut blue) = (0usize, 0usize);
    let (mut red_cx, mut blue_cx) = (0.0f64, 0.0f64);
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * iw + x) * 4;
            let px = (raw[i], raw[i + 1], raw[i + 2]);
            let fx = (x as f32 - rect.min.x) as f64 / rect.width() as f64;
            if is_red(px) {
                red += 1;
                red_cx += fx;
            } else if is_blue(px) {
                blue += 1;
                blue_cx += fx;
            }
        }
    }

    // Sizing: a 40px square is ~1600px; a 1×1 PointList fallback would be ~1.
    assert!(
        red > 800,
        "RED square should be a sized sprite (~{}px²), not a 1px point; got {red} px",
        SIZE as usize
    );
    assert!(blue > 0, "BLUE circle should render; got 0 px");

    // Marker shapes: the square fills its whole sprite; the circle discards its
    // corners (area ≈ π/4 of the square), so the square must cover clearly more.
    assert!(
        red as f64 > blue as f64 * 1.15,
        "square should cover more than the circle (corners discarded); \
         square={red} circle={blue}"
    );

    // Billboard projection: red is the left point, blue the right one.
    let red_cx = red_cx / red as f64;
    let blue_cx = blue_cx / blue as f64;
    assert!(
        red_cx < 0.45,
        "RED square centroid should be left of centre; got fx={red_cx:.3}"
    );
    assert!(
        blue_cx > 0.55,
        "BLUE circle centroid should be right of centre; got fx={blue_cx:.3}"
    );
}
