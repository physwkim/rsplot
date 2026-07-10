//! Headless end-to-end checks for `VolumeRaycaster`: an opaque RGBA volume must
//! ray-march to visible colour, an all-transparent volume must not, and the 3D
//! texture a `VolumeId` names must live exactly as long as the views holding
//! that id. Uses the same `egui_kittest` wgpu harness as the scene tests (no
//! window).

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::VolumeRaycaster;
use rsplot::egui;
use rsplot::egui_wgpu::RenderState;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

const WIN: f32 = 320.0;

/// Serializes the two GPU tests in this binary. `cargo test` runs them on
/// parallel threads, so each would create and tear down its own wgpu device at
/// the same time; on the Windows CI runner's DX12 software adapter (WARP) that
/// concurrent device create/destroy faults with STATUS_ACCESS_VIOLATION (a
/// native crash in the adapter, not in rsplot). Holding this lock across each
/// test's whole render keeps exactly one device alive at a time. Poison is
/// recovered (a failed assertion in one test must not wedge the other).
static GPU: Mutex<()> = Mutex::new(());

/// An `n³` RGBA8 volume, every voxel `(r, g, b, a)`.
fn solid(n: usize, rgba: [u8; 4]) -> Vec<u8> {
    let mut v = Vec::with_capacity(n * n * n * 4);
    for _ in 0..n * n * n {
        v.extend_from_slice(&rgba);
    }
    v
}

/// Count pixels that are clearly red (the volume colour): red high, green/blue
/// low. The harness background is dark grey, so any such pixel is ray-marched
/// volume colour.
fn count_red(raw: &[u8], iw: usize, ih: usize) -> usize {
    (0..iw * ih)
        .filter(|&px| {
            let i = px * 4;
            raw[i] > 120 && raw[i + 1] < 80 && raw[i + 2] < 80
        })
        .count()
}

struct App {
    view: VolumeRaycaster,
}

impl App {
    fn new(rs: &RenderState, rgba: [u8; 4]) -> Self {
        let mut view = VolumeRaycaster::new(rs, 7);
        view.set_volume(rs, &solid(16, rgba), 16, 16, 16);
        Self { view }
    }
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.view.show(ui);
    }
}

fn red_pixels_for(rgba: [u8; 4]) -> usize {
    let rs = create_render_state(default_wgpu_setup());
    let app = Rc::new(RefCell::new(App::new(&rs, rgba)));
    let renderer = WgpuTestRenderer::from_render_state(rs);
    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WIN, WIN))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| app_ui.borrow_mut().ui(ui));
    harness.step();
    let image = harness.render().expect("headless wgpu render");
    let (iw, ih) = (image.width() as usize, image.height() as usize);
    count_red(image.as_raw(), iw, ih)
}

/// Render one view (already built and uploaded) and count its red pixels.
fn red_pixels_of(rs: &RenderState, view: VolumeRaycaster) -> usize {
    let view = Rc::new(RefCell::new(view));
    let renderer = WgpuTestRenderer::from_render_state(rs.clone());
    let ui_view = view.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WIN, WIN))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            ui_view.borrow_mut().show(ui);
        });
    harness.step();
    let image = harness.render().expect("headless wgpu render");
    let (iw, ih) = (image.width() as usize, image.height() as usize);
    count_red(image.as_raw(), iw, ih)
}

/// Boundary: two live claims on one id, one given back. `remove` used to drop the
/// shared entry outright, so the surviving view's ray-march found no texture and
/// painted nothing.
#[test]
fn remove_keeps_the_texture_while_another_same_id_view_lives() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let rs = create_render_state(default_wgpu_setup());

    let mut uploader = VolumeRaycaster::new(&rs, 21);
    uploader.set_volume(&rs, &solid(16, [255, 0, 0, 255]), 16, 16, 16);
    let survivor = VolumeRaycaster::new(&rs, 21); // second claim on the same volume
    uploader.remove(&rs);

    let red = red_pixels_of(&rs, survivor);
    assert!(
        red > 500,
        "one view's remove() must not free a texture another view still renders; \
         only {red} red px"
    );
}

/// Boundary: the last claim goes. Dropping the view must free the entry, so a
/// fresh view taking the same id starts empty — it used to inherit the dead
/// view's texture, which is the same leak that pinned the VRAM for the app's
/// lifetime.
#[test]
fn dropping_the_last_view_frees_the_texture() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let rs = create_render_state(default_wgpu_setup());

    let mut uploader = VolumeRaycaster::new(&rs, 22);
    uploader.set_volume(&rs, &solid(16, [255, 0, 0, 255]), 16, 16, 16);
    drop(uploader);

    let reused = VolumeRaycaster::new(&rs, 22); // same id, nothing uploaded
    let red = red_pixels_of(&rs, reused);
    assert!(
        red < 50,
        "the last view's drop must free id 22's texture; {red} red px leaked into \
         a fresh view that uploaded nothing"
    );
}

#[test]
fn opaque_volume_ray_marches_to_colour() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let red = red_pixels_for([255, 0, 0, 255]);
    assert!(
        red > 500,
        "an opaque red volume must ray-march to a red blob; only {red} red px"
    );
}

#[test]
fn transparent_volume_renders_nothing() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let red = red_pixels_for([255, 0, 0, 0]); // alpha 0 everywhere
    assert!(
        red < 50,
        "a fully transparent volume must not paint colour; {red} red px leaked"
    );
}
