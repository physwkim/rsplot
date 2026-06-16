//! `CompareImages` AUTO alignment (silx `AlignmentMode.AUTO` →
//! `__createSiftData` → `LinearAlign`).
//!
//! The registration algorithm itself is unit-tested in `core::sift_align`; these
//! tests drive it through the widget the way a host would — `set_images` two
//! images that differ by a known shift, `set_alignment(Auto)`, render — and
//! assert the wiring: a successful registration keeps the AUTO mode and lines B
//! up with A under the status-bar read-out, while a featureless pair falls back
//! to ORIGIN (silx `__setDefaultAlignmentMode`). Building a `CompareImages` needs
//! a wgpu render state and a real frame, so this runs through egui_kittest.

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use siplot::egui;
use siplot::{Colormap, CompareAlignment, CompareImages};

/// A sum of Gaussian blobs at distinct positions/scales — a SIFT-friendly
/// (blob-detector) target, deterministic for reproducibility.
fn blob_image(w: usize, h: usize) -> Vec<f32> {
    let blobs = [
        (22.0f32, 26.0, 4.0, 1.0f32),
        (54.0, 20.0, 6.0, 0.9),
        (36.0, 54.0, 5.0, 1.0),
        (62.0, 58.0, 3.0, 0.8),
        (44.0, 40.0, 7.0, 0.7),
        (26.0, 62.0, 4.5, 0.85),
    ];
    let mut img = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut v = 0.0f32;
            for &(cx, cy, sigma, amp) in &blobs {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                v += amp * (-(dx * dx + dy * dy) / (2.0 * sigma * sigma)).exp();
            }
            img[y * w + x] = v;
        }
    }
    img
}

/// Shift content by `(+dx, +dy)`: `b(x, y) = a(x-dx, y-dy)`.
fn shift_image(a: &[f32], w: usize, h: usize, dx: isize, dy: isize) -> Vec<f32> {
    let mut b = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let sx = x as isize - dx;
            let sy = y as isize - dy;
            if sx >= 0 && sy >= 0 && (sx as usize) < w && (sy as usize) < h {
                b[y * w + x] = a[sy as usize * w + sx as usize];
            }
        }
    }
    b
}

/// Build a harness around a `CompareImages` showing `a`/`b` (both `w×h`) in AUTO
/// alignment, render two frames so the SIFT registration + composite are cached.
fn harness_auto(
    w: usize,
    h: usize,
    a: Vec<f32>,
    b: Vec<f32>,
) -> (Rc<RefCell<CompareImages>>, Harness<'static>) {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);

    let mut view = CompareImages::new(&rs, 0);
    view.set_images(
        (w as u32, h as u32),
        &a,
        (w as u32, h as u32),
        &b,
        Colormap::viridis(0.0, 1.0),
    )
    .expect("equal-length image data");
    view.set_alignment(CompareAlignment::Auto);

    let app = Rc::new(RefCell::new(view));
    let app_ui = app.clone();
    let renderer = WgpuTestRenderer::from_render_state(rs.clone());
    let mut harness = Harness::builder()
        .with_size(egui::vec2(400.0, 400.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            app_ui.borrow_mut().show(ui);
        });
    harness.step();
    harness.step();
    (app, harness)
}

#[test]
fn auto_alignment_registers_a_shifted_image() {
    let (w, h) = (80usize, 80usize);
    let a = blob_image(w, h);
    let b = shift_image(&a, w, h, 4, 3);

    let (app, _harness) = harness_auto(w, h, a.clone(), b);
    let cmp = app.borrow();

    // Registration succeeded → the widget did NOT fall back to ORIGIN.
    assert_eq!(
        cmp.alignment(),
        CompareAlignment::Auto,
        "AUTO should hold after a successful SIFT registration"
    );

    // Under an interior display coordinate, the raw B value (looked up through the
    // estimated affine) matches the raw A value there, because B is A shifted and
    // the affine inverts that shift.
    let (va, vb) = cmp.raw_pixel_data(44.0, 40.0);
    let va = va.expect("A value in range");
    let vb = vb.expect("B value in range");
    assert!(
        (va - vb).abs() < 0.1,
        "aligned B value {vb} should track A value {va}"
    );
}

#[test]
fn auto_alignment_falls_back_to_origin_without_features() {
    let (w, h) = (32usize, 32usize);
    let flat = vec![0.5f32; w * h];

    let (app, _harness) = harness_auto(w, h, flat.clone(), flat);
    let cmp = app.borrow();

    // A featureless pair yields too few SIFT matches → silx `__setDefaultAlignmentMode`.
    assert_eq!(
        cmp.alignment(),
        CompareAlignment::Origin,
        "a featureless pair should fall back to ORIGIN"
    );
}
