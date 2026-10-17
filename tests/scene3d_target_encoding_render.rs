//! Headless wgpu readback proving the 3D scene reaches a non-sRGB render target
//! in that target's encoding: an opaque triangle and the background clear must
//! come back as their own `Color32` bytes, both through the on-screen blit and
//! through `snapshot_scene3d`.
//!
//! `egui_kittest`'s renderer targets a non-sRGB `Rgba8Unorm` texture — the same
//! kind of target egui-wgpu's swapchain is — and reads the stored bytes back
//! verbatim. The scene is shaded in linear light and used to be blitted into
//! that target unconverted, so every 3D colour came out darker than its
//! `Color32` (teal `(33, 145, 140)` read back as `(4, 71, 66)`). Now the scene
//! renders into the sRGB variant of the surface format and the blit encodes the
//! linear sample for a non-sRGB surface (`gpu_scene3d::blit_entry_point`), so
//! the bytes round-trip to within one 8-bit step.
//!
//! Boundaries covered: a shaded primitive (triangle) and the clear colour; the
//! blit path and the snapshot readback path. Needs a GPU (real or software).

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui::{self, Color32};
use rsplot::egui_wgpu::RenderState;
use rsplot::egui_wgpu::RendererOptions;
use rsplot::{
    Camera, Scene3dGeometry, Vec3, install_scene3d, paint_scene3d, set_scene3d, snapshot_scene3d,
};

const SCENE_ID: u64 = 0;
const WIN: f32 = 300.0;
/// Half-side of the sampled block.
const BLOCK: usize = 3;

const TRIANGLE: Color32 = Color32::from_rgb(33, 145, 140);
const BACKGROUND: Color32 = Color32::from_rgb(128, 64, 32);

/// A camera at `(0,0,5)` looking down `-z`, up `+y`, fovy 30°.
fn camera() -> Camera {
    Camera::new(
        30.0,
        0.1,
        100.0,
        (1.0, 1.0),
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::new(0.0, 0.0, -1.0),
        Vec3::new(0.0, 1.0, 0.0),
    )
}

/// Upload one opaque triangle covering the view centre (its corners stay well
/// inside the frame, so the frame corners show the clear colour).
fn upload_scene(rs: &RenderState) {
    install_scene3d(rs);
    let mut g = Scene3dGeometry::new();
    g.add_triangle(
        [-0.5, -0.5, 0.0],
        [0.5, -0.5, 0.0],
        [0.0, 0.5, 0.0],
        TRIANGLE,
    );
    set_scene3d(rs, SCENE_ID, &g);
}

/// Every pixel of the `(2·BLOCK)²` block around `(cx, cy)` must be `expected`
/// to within one 8-bit step per channel.
fn assert_block_is(
    raw: &[u8],
    width: usize,
    (cx, cy): (usize, usize),
    expected: Color32,
    what: &str,
) {
    let expected = [expected.r(), expected.g(), expected.b()];
    for y in cy - BLOCK..cy + BLOCK {
        for x in cx - BLOCK..cx + BLOCK {
            let i = (y * width + x) * 4;
            let got = [raw[i], raw[i + 1], raw[i + 2]];
            let close = got.iter().zip(expected).all(|(&g, e)| g.abs_diff(e) <= 1);
            assert!(
                close,
                "{what}: pixel ({x},{y}) is rgb{got:?}, expected rgb{expected:?} ± 1 \
                 (the Color32's own sRGB bytes on the non-sRGB target)"
            );
        }
    }
}

struct App {
    camera: Camera,
    last_rect: Option<egui::Rect>,
}

impl App {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let (rect, _resp) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
        paint_scene3d(ui, rect, SCENE_ID, &self.camera, BACKGROUND);
        self.last_rect = Some(rect);
    }
}

#[test]
fn blitted_scene_reads_back_its_color32_bytes() {
    let rs = create_render_state(default_wgpu_setup(), RendererOptions::default());
    upload_scene(&rs);
    let app = Rc::new(RefCell::new(App {
        camera: camera(),
        last_rect: None,
    }));
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
    let iw = image.width() as usize;
    let raw = image.as_raw();

    let at = |fx: f32, fy: f32| {
        (
            (rect.min.x + fx * rect.width()).round() as usize,
            (rect.min.y + fy * rect.height()).round() as usize,
        )
    };
    assert_block_is(raw, iw, at(0.5, 0.5), TRIANGLE, "blit: triangle at centre");
    assert_block_is(raw, iw, at(0.08, 0.08), BACKGROUND, "blit: clear at corner");
}

#[test]
fn snapshot_reads_back_its_color32_bytes() {
    let rs = create_render_state(default_wgpu_setup(), RendererOptions::default());
    upload_scene(&rs);

    let (w, h) = (WIN as u32, WIN as u32);
    let raw = snapshot_scene3d(&rs, SCENE_ID, &camera(), BACKGROUND, (w, h))
        .expect("snapshot of an uploaded scene");
    assert_eq!(raw.len(), (w * h * 4) as usize);

    let iw = w as usize;
    let at = |fx: f32, fy: f32| {
        (
            (fx * w as f32).round() as usize,
            (fy * h as f32).round() as usize,
        )
    };
    assert_block_is(
        &raw,
        iw,
        at(0.5, 0.5),
        TRIANGLE,
        "snapshot: triangle at centre",
    );
    assert_block_is(
        &raw,
        iw,
        at(0.08, 0.08),
        BACKGROUND,
        "snapshot: clear at corner",
    );
}
