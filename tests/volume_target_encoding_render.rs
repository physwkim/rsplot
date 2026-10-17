//! Headless wgpu readback proving `VolumeRaycaster` writes its composite in the
//! render target's encoding on an *sRGB* target too.
//!
//! The voxel texels used to be the caller's sRGB bytes whatever the target. On
//! egui-wgpu's default non-sRGB swapchain that is right (the stored byte is the
//! colormap byte; `tests/volume_opacity_physics.rs` pins that path). On an
//! `*Srgb` target the hardware encodes the shader's output on store, so the
//! already-encoded bytes were encoded a second time: an opaque `(255, 128, 0)`
//! voxel read back as `(255, 188, 0)`. Now the uploader linearises the voxels
//! for an sRGB target (`gpu_image::rgba8_for_target`), so the stored bytes are
//! the authored ones again.
//!
//! `egui_kittest` renders into a texture of the render state's `target_format`,
//! so the sRGB target is reached by rebuilding the render state with the sRGB
//! variant of its format and an egui renderer for it (`RenderState`'s fields
//! are public). Boundaries covered on that target: an opaque voxel (opacity 1,
//! no blend) and the lowest-coverage voxels (a translucent composite over the
//! background — in linear light, as the hardware blends on an sRGB target);
//! and the opaque result must equal the non-sRGB target's.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::VolumeRaycaster;
use rsplot::egui;
use rsplot::egui::mutex::RwLock;
use rsplot::egui_wgpu::{RenderState, Renderer, RendererOptions};

const WIN: f32 = 320.0;
const N: usize = 16;

/// See `tests/volume_raycaster_render.rs`: concurrent wgpu device create/destroy
/// faults on the Windows CI runner's DX12 software adapter.
static GPU: Mutex<()> = Mutex::new(());

/// A fresh render state whose colour target is the kittest default format or,
/// for `srgb`, that format's sRGB variant with an egui renderer built for it.
fn render_state(srgb: bool) -> RenderState {
    let mut rs = create_render_state(default_wgpu_setup(), RendererOptions::default());
    if srgb {
        let format = rs.target_format.add_srgb_suffix();
        assert!(
            format.is_srgb(),
            "kittest target {:?} has no sRGB variant",
            rs.target_format
        );
        rs.renderer = Arc::new(RwLock::new(Renderer::new(
            &rs.device,
            format,
            RendererOptions::default(),
        )));
        rs.target_format = format;
    }
    rs
}

/// An `N³` RGBA8 volume, every voxel `voxel`.
fn cube(voxel: [u8; 4]) -> Vec<u8> {
    voxel.iter().copied().cycle().take(N * N * N * 4).collect()
}

struct App {
    view: VolumeRaycaster,
}

/// RGB of the centre pixel after rendering a uniform `voxel` cube on `rs`'s
/// target at the default 256 steps. The centre ray crosses the cube through its
/// middle: a unit chord, by `volume_bounds`.
fn centre_rgb(rs: RenderState, voxel: [u8; 4]) -> [u8; 3] {
    let mut view = VolumeRaycaster::new(&rs, 13);
    view.set_volume(&rs, &cube(voxel), N, N, N);
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
    let centre = ((ih / 2) * iw + iw / 2) * 4;
    let raw = image.as_raw();
    [raw[centre], raw[centre + 1], raw[centre + 2]]
}

/// sRGB transfer function, byte → linear.
fn to_linear(b: u8) -> f64 {
    let s = f64::from(b) / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB transfer function, linear → byte scale (`0..=255`, unrounded).
fn to_srgb_255(l: f64) -> f64 {
    let s = if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    s * 255.0
}

/// Analytic accumulated opacity of the centre ray through the unit cube of
/// uniform-`alpha` voxels — `tests/volume_opacity_physics.rs` at depth 16: the
/// chord is 1, the reference spacing `√3 / 256`, so the transmittance is
/// `(1 - alpha)^(256 / √3)`.
fn analytic_opacity(alpha: u8) -> f64 {
    let sa = f64::from(alpha) / 255.0;
    1.0 - (1.0 - sa).powf(256.0 / 3f64.sqrt())
}

/// Boundary: opacity 1, no blend with the background. The stored bytes on an
/// sRGB target must be the voxel's own, and the same as on a non-sRGB target.
#[test]
fn opaque_voxels_store_their_own_bytes_on_an_srgb_target() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    const VOXEL: [u8; 4] = [255, 128, 0, 255];

    let srgb = centre_rgb(render_state(true), VOXEL);
    let gamma = centre_rgb(render_state(false), VOXEL);
    for c in 0..3 {
        assert!(
            srgb[c].abs_diff(VOXEL[c]) <= 1,
            "sRGB target: channel {c} stored as {} for voxel byte {} (all {srgb:?})",
            srgb[c],
            VOXEL[c]
        );
        assert!(
            srgb[c].abs_diff(gamma[c]) <= 1,
            "sRGB target {srgb:?} differs from non-sRGB target {gamma:?} at channel {c}"
        );
    }
}

/// Boundary: the lowest coverage a march is built on, composited over the
/// background. On an sRGB target the blend runs in linear light (the hardware
/// decodes the destination), so the stored byte is the encoding of
/// `linear(voxel) · opacity + linear(background) · (1 - opacity)` — the
/// authored hue at the Beer–Lambert opacity, not a double-encoded one.
#[test]
fn low_coverage_voxels_composite_their_authored_hue_on_an_srgb_target() {
    let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
    const VOXEL: [u8; 4] = [255, 128, 0, 3]; // faint orange

    // An all-transparent cube leaves the harness background at the centre.
    let bg = centre_rgb(render_state(true), [255, 128, 0, 0]);
    let got = centre_rgb(render_state(true), VOXEL);
    let opacity = analytic_opacity(VOXEL[3]);

    for c in 0..3 {
        let expect =
            to_srgb_255(to_linear(VOXEL[c]) * opacity + to_linear(bg[c]) * (1.0 - opacity));
        assert!(
            (f64::from(got[c]) - expect).abs() <= 2.0,
            "channel {c}: expected {expect:.1}, got {} (all {got:?}, background {bg:?}, \
             opacity {opacity:.4})",
            got[c]
        );
    }
}
