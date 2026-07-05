//! `CompareImages` on-plot draggable split separator (silx `__vline`/`__hline`
//! markers + `__separatorConstraint`/`__separatorMoved`).
//!
//! silx exposes the split position as a draggable line marker over the plot, not
//! a slider; rsplot keeps the slider as a convenience but adds the faithful
//! separator. These tests drive the separator the way a user would — pressing on
//! the line and dragging it — and assert the drag folds back into `split()`
//! (silx `__separatorMoved`). Building a `CompareImages` needs a wgpu render
//! state (real or software), and the drag needs a real frame so the plot caches
//! its transform, so this runs through the egui_kittest harness.

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui;
use rsplot::{Colormap, CompareImages, CompareMode, YAxis};

/// Build a harness around a `CompareImages` in the given mode, render two frames
/// so the transform/composite are cached, and return the shared widget + harness.
///
/// Both images are 8×8 with uniform but distinct intensities, so any split
/// position is valid and the composite is well-defined; the exact pixel values
/// do not matter — only that the separator maps to `split`.
fn harness_for(mode: CompareMode) -> (Rc<RefCell<CompareImages>>, Harness<'static>) {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);

    let mut view = CompareImages::new(&rs, 0);
    let data_a = vec![0.2f32; 8 * 8];
    let data_b = vec![0.8f32; 8 * 8];
    view.set_images(
        (8, 8),
        &data_a,
        (8, 8),
        &data_b,
        Colormap::viridis(0.0, 1.0),
    )
    .expect("equal-length image data");
    view.set_mode(mode);

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

/// Drag from `p0` to `p1` with an incremental ramp so egui reports a genuine
/// drag (a single jump makes `drag_started` fire already at the end point).
fn drag(harness: &mut Harness<'static>, p0: egui::Pos2, p1: egui::Pos2) {
    harness.drag_at(p0);
    harness.step();
    for t in [0.2f32, 0.5, 0.8, 1.0] {
        harness.hover_at(p0 + (p1 - p0) * t);
        harness.step();
    }
    harness.drop_at(p1);
    harness.step();
    harness.step();
}

#[test]
fn dragging_the_vertical_separator_sets_the_split() {
    let (app, mut harness) = harness_for(CompareMode::HalfHalf);

    // The separator starts centered: data x = 0.5 * 8 = 4.0.
    assert!((app.borrow().split() - 0.5).abs() < 1e-6);

    // Press on the vertical line (data x = 4.0, any y inside) and drag it to
    // data x = 6.0 → split should become 6/8 = 0.75.
    let p0 = app
        .borrow()
        .data_to_pixel(4.0, 4.0, YAxis::Left)
        .expect("transform cached after a frame");
    let p1 = app
        .borrow()
        .data_to_pixel(6.0, 4.0, YAxis::Left)
        .expect("transform cached after a frame");
    drag(&mut harness, p0, p1);

    let split = app.borrow().split();
    assert!(
        (split - 0.75).abs() < 0.06,
        "dragging the vertical separator to x=6 of 8 must set split≈0.75, got {split}"
    );
}

#[test]
fn dragging_the_horizontal_separator_sets_the_split() {
    let (app, mut harness) = harness_for(CompareMode::SplitHorizontal);

    assert!((app.borrow().split() - 0.5).abs() < 1e-6);

    // Press on the horizontal line (data y = 4.0, any x inside) and drag it to
    // data y = 2.0 → split should become 2/8 = 0.25.
    let p0 = app
        .borrow()
        .data_to_pixel(4.0, 4.0, YAxis::Left)
        .expect("transform cached after a frame");
    let p1 = app
        .borrow()
        .data_to_pixel(4.0, 2.0, YAxis::Left)
        .expect("transform cached after a frame");
    drag(&mut harness, p0, p1);

    let split = app.borrow().split();
    assert!(
        (split - 0.25).abs() < 0.06,
        "dragging the horizontal separator to y=2 of 8 must set split≈0.25, got {split}"
    );
}

#[test]
fn programmatic_split_repositions_the_separator() {
    // A `set_split` between frames must move the separator with it: after the
    // move, grabbing the line at its *new* data position (x = 0.25 * 8 = 2.0)
    // and nudging it must read back near the dragged position — proving the
    // marker actually sits at the programmatic split, not the old center.
    let (app, mut harness) = harness_for(CompareMode::HalfHalf);
    app.borrow_mut().set_split(0.25);
    harness.step();
    harness.step();

    let p0 = app
        .borrow()
        .data_to_pixel(2.0, 4.0, YAxis::Left)
        .expect("transform cached");
    let p1 = app
        .borrow()
        .data_to_pixel(3.0, 4.0, YAxis::Left)
        .expect("transform cached");
    drag(&mut harness, p0, p1);

    let split = app.borrow().split();
    assert!(
        (split - 0.375).abs() < 0.06,
        "grabbing the separator at the programmatic split (x=2) and dragging to x=3 \
         must read back split≈0.375, got {split}"
    );
}
