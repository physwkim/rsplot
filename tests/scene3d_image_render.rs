//! Headless render check for the plot3d P1.3 textured-image pipeline
//! (`render::gpu_scene3d` image layers + `scene3d_image.wgsl`): a 2×2 colour
//! checkerboard placed as a quad in the z=0 plane should project, quadrant by
//! quadrant, to the expected screen colours.
//!
//! The image data is row-major (row 0 first); UV `v=0` is at the origin corner
//! (world y=0), so row 0 lands at the *bottom* of the screen and row 1 at the
//! top (screen y is flipped). With the camera centred on the image:
//!
//! - bottom-left  = (row0, col0) = red
//! - bottom-right = (row0, col1) = green
//! - top-left     = (row1, col0) = blue
//! - top-right    = (row1, col1) = white
//!
//! Asserting the four quadrants proves texture upload, UV mapping, the world-rect
//! quad, and the MVP projection are all correct.

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui::{self, Color32};
use rsplot::egui_wgpu::RenderState;
use rsplot::{
    Camera, ImageInterpolation, Scene3dGeometry, Scene3dImageLayer, Vec3, install_scene3d,
    paint_scene3d, set_scene3d,
};
use std::cell::RefCell;
use std::rc::Rc;

const SCENE_ID: u64 = 0;
const WIN: f32 = 300.0;

/// Premultiplied-linear RGBA8 for an opaque colour (the layer's pixel format).
fn px(c: Color32) -> [u8; 4] {
    let [r, g, b, a] = egui::Rgba::from(c).to_array();
    [
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
        (a * 255.0).round() as u8,
    ]
}

struct App {
    camera: Camera,
    last_rect: Option<egui::Rect>,
}

impl App {
    fn new(rs: &RenderState) -> Self {
        install_scene3d(rs);

        // 2×2 image: row0 = [red, green], row1 = [blue, white].
        let mut pixels = Vec::new();
        for c in [Color32::RED, Color32::GREEN, Color32::BLUE, Color32::WHITE] {
            pixels.extend_from_slice(&px(c));
        }
        let mut g = Scene3dGeometry::new();
        g.add_image_layer(Scene3dImageLayer {
            pixels,
            width: 2,
            height: 2,
            origin: [0.0, 0.0, 0.0],
            scale: [1.0, 1.0], // spans world [0,2]×[0,2]
            interpolation: ImageInterpolation::Nearest,
        });
        set_scene3d(rs, SCENE_ID, &g);

        // Camera centred on the image centre (1,1,0), looking down −z.
        let camera = Camera::new(
            30.0,
            0.1,
            100.0,
            (1.0, 1.0),
            Vec3::new(1.0, 1.0, 5.0),
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
fn scene3d_image_quad_projects_its_texels() {
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

    // Corners (outside the image quad): the black offscreen clear.
    for (fx, fy) in [(0.03, 0.03), (0.97, 0.97)] {
        let (r, g, b) = at(fx, fy);
        assert!(r < 40 && g < 40 && b < 40, "corner should be black clear");
    }

    // Quadrant centres (image spans the central ~75% of the square rect).
    let red = at(0.313, 0.687);
    let green = at(0.687, 0.687);
    let blue = at(0.313, 0.313);
    let white = at(0.687, 0.313);

    let dominant = |(r, g, b): (u8, u8, u8), ch: usize| {
        let v = [r, g, b];
        v[ch] > 200 && v[(ch + 1) % 3] < 70 && v[(ch + 2) % 3] < 70
    };
    assert!(dominant(red, 0), "bottom-left should be red; got {red:?}");
    assert!(
        dominant(green, 1),
        "bottom-right should be green; got {green:?}"
    );
    assert!(dominant(blue, 2), "top-left should be blue; got {blue:?}");
    assert!(
        white.0 > 200 && white.1 > 200 && white.2 > 200,
        "top-right should be white; got {white:?}"
    );
}

/// A front image with one fully transparent texel must let the 3D geometry
/// behind the hole show through — silx `_Image` discards `alpha == 0` texels
/// (`primitives.py:2120-2123`), so they write neither colour nor depth. Before
/// the fix the transparent stand-in for GLSL `discard` still wrote depth and
/// occluded whatever sat behind the hole.
struct DepthHoleApp {
    camera: Camera,
    last_rect: Option<egui::Rect>,
}

impl DepthHoleApp {
    fn new(rs: &RenderState) -> Self {
        install_scene3d(rs);

        let mut g = Scene3dGeometry::new();

        // FRONT quad at z=0 (nearer the camera): 2×2 with the bottom-left texel
        // (row0,col0) fully transparent (a "hole") and the rest opaque red.
        // Inserted FIRST, so it is drawn first — without the discard it stamps
        // depth across the hole and hides the back quad there.
        let transparent = [0u8, 0, 0, 0];
        let mut front = Vec::new();
        front.extend_from_slice(&transparent); // row0,col0 — the hole
        front.extend_from_slice(&px(Color32::RED)); // row0,col1
        front.extend_from_slice(&px(Color32::RED)); // row1,col0
        front.extend_from_slice(&px(Color32::RED)); // row1,col1
        g.add_image_layer(Scene3dImageLayer {
            pixels: front,
            width: 2,
            height: 2,
            origin: [0.0, 0.0, 0.0],
            scale: [1.0, 1.0], // world [0,2]×[0,2] at z=0
            interpolation: ImageInterpolation::Nearest,
        });

        // BACK quad at z=-1 (farther): opaque green, same world rect. Visible at
        // the hole only if the front's transparent texel was discarded.
        g.add_image_layer(Scene3dImageLayer {
            pixels: px(Color32::GREEN).repeat(4),
            width: 2,
            height: 2,
            origin: [0.0, 0.0, -1.0],
            scale: [1.0, 1.0],
            interpolation: ImageInterpolation::Nearest,
        });
        set_scene3d(rs, SCENE_ID, &g);

        let camera = Camera::new(
            30.0,
            0.1,
            100.0,
            (1.0, 1.0),
            Vec3::new(1.0, 1.0, 5.0),
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
fn transparent_front_texel_does_not_occlude_geometry_behind() {
    let rs = create_render_state(default_wgpu_setup());
    let app = Rc::new(RefCell::new(DepthHoleApp::new(&rs)));
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

    // Bottom-left quadrant = the front hole → the back green must show through.
    let hole = at(0.313, 0.687);
    assert!(
        hole.1 > 150 && hole.0 < 90 && hole.2 < 90,
        "hole should reveal the green quad behind it; got {hole:?}"
    );
    // Bottom-right quadrant = front's opaque red texel, drawn over the back.
    let opaque = at(0.687, 0.687);
    assert!(
        opaque.0 > 150 && opaque.1 < 90 && opaque.2 < 90,
        "opaque front texel should stay red over the back quad; got {opaque:?}"
    );
}
