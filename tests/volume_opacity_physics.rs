//! `VolumeRaycaster` has no silx counterpart — `plot3d/items/volume.py` ships an
//! `Isosurface` and a `CutPlane`, never a GPU direct-volume ray-caster — so its
//! opacity model has no upstream contract to diff against. These tests pin the
//! *physics* instead, which is the only contract there is.
//!
//! Beer–Lambert: a ray's transmittance through a uniform medium depends on the
//! thickness it traverses and not on how many samples the march happens to take.
//! Written as the two properties that must hold together:
//!
//! 1. **Thickness dependence** — a thinner slab of the same voxels is more
//!    transparent.
//! 2. **Step-count invariance** — the same slab renders the same at any `steps`.
//!
//! Holding only (2) is what the step-count-ratio correction did; a uniform volume
//! then read as a flat silhouette.
//!
//! The default camera looks straight down `-Z` (`VolumeRaycaster::new`), and
//! `volume_bounds` normalises the longest axis to one unit, so shrinking `depth`
//! shortens the centre ray's chord by exactly that ratio and nothing else.

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::VolumeRaycaster;
use rsplot::egui;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

const WIN: f32 = 320.0;

/// See `tests/volume_raycaster_render.rs`: concurrent wgpu device create/destroy
/// faults on the Windows CI runner's DX12 software adapter.
static GPU: Mutex<()> = Mutex::new(());

/// A `depth × 16 × 16` RGBA8 volume, every voxel `(255, 0, 0, alpha)`.
fn slab(depth: usize, alpha: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity(depth * 16 * 16 * 4);
    for _ in 0..depth * 16 * 16 {
        v.extend_from_slice(&[255, 0, 0, alpha]);
    }
    v
}

struct App {
    view: VolumeRaycaster,
}

/// Red channel of the centre pixel after rendering a `depth × 16 × 16` slab of
/// uniform-alpha red voxels at `steps` samples per ray.
///
/// The centre pixel's ray crosses the box through its middle, so its chord is
/// the slab's Z extent — `depth / 16` of a unit, by `volume_bounds`.
fn centre_red(depth: usize, alpha: u8, steps: u32) -> u8 {
    let rs = create_render_state(default_wgpu_setup());
    let mut view = VolumeRaycaster::new(&rs, 11);
    view.set_volume(&rs, &slab(depth, alpha), depth, 16, 16);
    view.set_steps(steps);
    let app = Rc::new(RefCell::new(App { view }));
    let renderer = WgpuTestRenderer::from_render_state(rs);
    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WIN, WIN))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            app_ui.borrow_mut().view.show(ui);
        });
    harness.step();
    let image = harness.render().expect("headless wgpu render");
    let (iw, ih) = (image.width() as usize, image.height() as usize);
    let centre = (ih / 2) * iw + iw / 2;
    image.as_raw()[centre * 4]
}

/// Red channel the centre pixel shows with nothing painted over it: an
/// all-transparent volume composites to the harness background exactly there.
fn background_red() -> u8 {
    centre_red(16, 0, 256)
}

/// Analytic accumulated opacity of the centre ray through a `depth × 16 × 16`
/// slab of uniform-`alpha` voxels.
///
/// `volume_bounds` normalises the longest axis to one unit, so the box extent is
/// `(1, 1, depth/16)` and the centre ray's chord is `L = depth/16`. Each sample
/// is corrected to the reference spacing `dt_ref = |extent| / 256`, so the chord
/// transmittance telescopes to `(1 - alpha)^(L / dt_ref)` exactly — no
/// discretisation error, whatever the step count.
fn analytic_opacity(depth: usize, alpha: u8) -> f64 {
    let ez = depth as f64 / 16.0;
    let diag = (1.0 + 1.0 + ez * ez).sqrt();
    let sa = f64::from(alpha) / 255.0;
    1.0 - (1.0 - sa).powf(256.0 * ez / diag)
}

/// Boundary: same voxels, four thicknesses. The rendered centre pixel must match
/// the Beer–Lambert prediction for its chord, not merely rise with it.
///
/// The render target is linear and the volume composites with premultiplied
/// alpha over the harness background, so the expected red is
/// `opacity + background · (1 - opacity)` — the volume's own colour is pure red
/// at intensity `opacity`.
///
/// The step-count-ratio correction made the exponent `REF_STEPS / steps`, which
/// carries no world distance: every chord accumulated `(1 - alpha)^steps`, so all
/// four depths rendered the same red (measured: `[225, 225, 225, 225]`).
#[test]
fn slab_opacity_matches_the_beer_lambert_prediction_for_its_chord() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    const ALPHA: u8 = 2; // faint enough that even the full cube stays unsaturated

    let bg = f64::from(background_red()) / 255.0;
    for depth in [2usize, 4, 8, 16] {
        let opacity = analytic_opacity(depth, ALPHA);
        let expect = (opacity + bg * (1.0 - opacity)) * 255.0;
        let got = f64::from(centre_red(depth, ALPHA, 256));
        assert!(
            (got - expect).abs() <= 2.0,
            "depth {depth}: chord {:.4}, opacity {opacity:.4} -> expected red {expect:.1}, got {got}",
            depth as f64 / 16.0
        );
    }
}

/// Boundary: one slab, four step counts. The march resolution must not change
/// the accumulated opacity — the property the count-ratio exponent did hold, and
/// which the path-length exponent must keep holding.
///
/// This is a preservation test: it passes against the old exponent too.
#[test]
fn opacity_is_invariant_to_the_step_count() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    const ALPHA: u8 = 2;

    let reds: Vec<u8> = [64u32, 128, 256, 512]
        .iter()
        .map(|&s| centre_red(8, ALPHA, s))
        .collect();

    let lo = *reds.iter().min().expect("four renders");
    let hi = *reds.iter().max().expect("four renders");
    assert!(
        hi - lo <= 3,
        "step count must not change the accumulated opacity, got {reds:?}"
    );
}
