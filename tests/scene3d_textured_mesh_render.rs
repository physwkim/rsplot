//! Headless render check for the plot3d P2.2b textured arbitrary-triangle
//! primitive (`render::gpu_scene3d` `Scene3dTexturedMesh`, the general case of
//! the image quad that the cut plane builds on). Unlike the full-quad image
//! test, this maps a 2×2 texture across a *single triangle* — proving per-vertex
//! UV interpolation over arbitrary geometry, not just an axis-aligned rect.
//!
//! The triangle is the lower-left half of the world `[0,2]×[0,2]` square in the
//! z=0 plane: `A=(0,0) uv(0,0)`, `B=(2,0) uv(1,0)`, `C=(0,2) uv(0,1)`, so the
//! interior `{x≥0, y≥0, x+y≤2}` carries `uv=(x/2, y/2)`. With the 2×2 texture
//! row0=[red, green], row1=[blue, white] and nearest sampling:
//!
//! - near A (uv≈0,0)   → red,   inside the triangle
//! - near B (uv≈1,0)   → green, inside the triangle
//! - near C (uv≈0,1)   → blue,  inside the triangle
//! - the white texel (uv 0.5..1, 0.5..1 → world x,y∈[1,2], x+y>2) lies OUTSIDE
//!   the triangle, so the top-right quadrant is the black clear — the very
//!   feature that distinguishes a triangle from a quad.
//!
//! Same camera as the image quad test, so the world→screen mapping is shared:
//! `fx = 0.313 + 0.374·(x−0.5)`, `fy = 0.687 − 0.374·(y−0.5)`.

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui::{self, Color32};
use rsplot::egui_wgpu::RenderState;
use rsplot::{
    Camera, ImageInterpolation, Scene3dGeometry, Scene3dTexturedMesh, Vec3, install_scene3d,
    paint_scene3d, set_scene3d,
};
use std::cell::RefCell;
use std::rc::Rc;

const SCENE_ID: u64 = 0;
const WIN: f32 = 300.0;

/// Premultiplied-linear RGBA8 for an opaque colour (the mesh's pixel format).
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

        // 2×2 texture: row0 = [red, green], row1 = [blue, white].
        let mut pixels = Vec::new();
        for c in [Color32::RED, Color32::GREEN, Color32::BLUE, Color32::WHITE] {
            pixels.extend_from_slice(&px(c));
        }
        let mut g = Scene3dGeometry::new();
        g.add_textured_mesh(Scene3dTexturedMesh {
            pixels,
            width: 2,
            height: 2,
            // Lower-left triangle of [0,2]×[0,2], z=0, with corner UVs.
            vertices: vec![[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 2.0, 0.0]],
            uvs: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            interpolation: ImageInterpolation::Nearest,
        });
        set_scene3d(rs, SCENE_ID, &g);

        // Camera centred on the square centre (1,1,0), looking down −z — identical
        // to the image quad test so the world→screen mapping carries over.
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
fn scene3d_textured_mesh_interpolates_uvs_over_a_triangle() {
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

    // Corners (outside the mesh): the black offscreen clear.
    for (fx, fy) in [(0.03, 0.03), (0.97, 0.97)] {
        let (r, g, b) = at(fx, fy);
        assert!(r < 40 && g < 40 && b < 40, "corner should be black clear");
    }

    let dominant = |(r, g, b): (u8, u8, u8), ch: usize| {
        let v = [r, g, b];
        v[ch] > 200 && v[(ch + 1) % 3] < 70 && v[(ch + 2) % 3] < 70
    };

    // Inside the triangle: the three corner texels sampled by their UVs.
    let red = at(0.34, 0.66); // world ≈ (0.57, 0.57), uv ≈ (0.29, 0.29) → red
    let green = at(0.65, 0.72); // world ≈ (1.40, 0.40), uv ≈ (0.70, 0.20) → green
    let blue = at(0.28, 0.35); // world ≈ (0.40, 1.40), uv ≈ (0.20, 0.70) → blue
    assert!(dominant(red, 0), "near A should be red; got {red:?}");
    assert!(dominant(green, 1), "near B should be green; got {green:?}");
    assert!(dominant(blue, 2), "near C should be blue; got {blue:?}");

    // The white-texel region (world x,y∈[1,2], x+y>2) is OUTSIDE the triangle,
    // so the top-right quadrant is the black clear — proves arbitrary-triangle
    // geometry, not a quad fill.
    let (r, g, b) = at(0.687, 0.313); // world ≈ (1.5, 1.5)
    assert!(
        r < 40 && g < 40 && b < 40,
        "top-right (white-texel quadrant, outside the triangle) should be black \
         clear, not the quad's white; got rgb({r},{g},{b})"
    );
}
