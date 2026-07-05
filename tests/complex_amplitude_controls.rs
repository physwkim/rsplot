//! `ComplexImageView::show_amplitude_range_controls` UI (silx
//! `ComplexImageView._AmplitudeRangeDialog`, ComplexImageView.py:50-155),
//! verified through the egui_kittest + wgpu harness.
//!
//! The composite math (`amplitude_phase_log_rgba`) and the
//! `set/get_amplitude_range_info` accessors are unit-tested in
//! `complex_image_view.rs`; this exercises the live UI: the rendered
//! autoscale checkbox and the delta drag-value, when actuated, route through
//! `set_amplitude_range_info` and the displayed range updates. The view holds a
//! `Plot2D`, and changing the range marks the image dirty so the next `show`
//! rebuilds it on the GPU, so a wgpu render state is required.

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui;
use rsplot::{ComplexImageView, ComplexMode};

/// A `ComplexImageView` over a 2×2 complex field whose maximum modulus is
/// exactly 5.0 (the `(3, 4)` point), in the log-amplitude/phase mode, rendered
/// through the harness so the controls and the image both draw. `build` runs
/// once before the first frame. Returns the shared view, the captured controls
/// rect is not needed (widgets are queried by accesskit).
fn view_harness() -> (Rc<RefCell<ComplexImageView>>, Harness<'static>) {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);
    let mut view = ComplexImageView::new(&rs, 0);
    // max |z| = |(3,4)| = 5.0; the rest are smaller.
    let data = [(3.0f32, 4.0f32), (1.0, 0.0), (0.0, 1.0), (2.0, 0.0)];
    view.set_data(2, 2, &data).expect("2x2 data");
    view.set_mode(ComplexMode::Log10AmplitudePhase);

    let view = Rc::new(RefCell::new(view));
    let view_ui = view.clone();
    let renderer = WgpuTestRenderer::from_render_state(rs.clone());
    let harness = Harness::builder()
        .with_size(egui::vec2(700.0, 300.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            view_ui.borrow_mut().show_amplitude_range_controls(ui);
            view_ui.borrow_mut().show(ui);
        });
    (view, harness)
}

#[test]
fn autoscale_checkbox_toggles_the_displayed_max() {
    let (view, mut harness) = view_harness();
    harness.step();
    harness.step();
    // Default: autoscale on (max = None), 2 log10 decades.
    assert_eq!(view.borrow().amplitude_range_info(), (None, 2.0));

    // Unchecking autoscale seeds the displayed max from the data's max amplitude
    // (silx `_autoscaleCheckBoxToggled`), which is exactly 5.0 here.
    harness.get_by_label("autoscale").click();
    harness.step();
    harness.step();
    let (max, delta) = view.borrow().amplitude_range_info();
    let max = max.expect("leaving autoscale seeds an explicit displayed max");
    assert!(
        (max - 5.0).abs() < 1e-4,
        "displayed max must seed from the data max amplitude (5.0), got {max}"
    );
    assert_eq!(delta, 2.0, "delta is unchanged by the autoscale toggle");

    // Re-checking autoscale clears the max back to None (autoscale to data).
    harness.get_by_label("autoscale").click();
    harness.step();
    harness.step();
    assert_eq!(view.borrow().amplitude_range_info().0, None);
}

#[test]
fn dragging_the_displayed_max_when_enabled_changes_the_range() {
    let (view, mut harness) = view_harness();
    harness.step();
    harness.step();

    // Turn autoscale off so the "Displayed Max." drag-value (the first
    // SpinButton-role node) becomes enabled and seeds to the data max of 5.0.
    harness.get_by_label("autoscale").click();
    harness.step();
    harness.step();
    assert!((view.borrow().amplitude_range_info().0.unwrap() - 5.0).abs() < 1e-4);

    // Drag the (now enabled) max drag-value to the right to raise it above 5.0.
    // It is the first SpinButton-role node (the delta drag-value follows it).
    let max_rect = harness
        .get_all_by_role(egui::accesskit::Role::SpinButton)
        .next()
        .expect("the max drag-value is present")
        .rect();
    let start = max_rect.center();
    let end = egui::pos2(start.x + 60.0, start.y);
    harness.drag_at(start);
    harness.step();
    for t in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
        harness.hover_at(start + (end - start) * t);
        harness.step();
    }
    harness.drop_at(end);
    harness.step();
    harness.step();

    let max = view
        .borrow()
        .amplitude_range_info()
        .0
        .expect("max stays explicit");
    assert!(
        max > 5.0,
        "dragging the enabled displayed-max drag-value right must raise it above \
         the seeded 5.0, got {max}"
    );
}

#[test]
fn dragging_the_delta_changes_the_log_decades() {
    let (view, mut harness) = view_harness();
    harness.step();
    harness.step();
    assert_eq!(view.borrow().amplitude_range_info().1, 2.0);

    // The delta drag-value is the last SpinButton-role node in the controls row
    // (after the autoscale checkbox and the "Displayed Max." drag-value). Drag
    // it to the right to raise the log-decade count.
    let delta_rect = {
        let nodes = harness.get_all_by_role(egui::accesskit::Role::SpinButton);
        nodes.last().expect("a delta drag-value is present").rect()
    };
    let start = delta_rect.center();
    let end = egui::pos2(start.x + 80.0, start.y);
    harness.drag_at(start);
    harness.step();
    for t in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
        harness.hover_at(start + (end - start) * t);
        harness.step();
    }
    harness.drop_at(end);
    harness.step();
    harness.step();

    let delta = view.borrow().amplitude_range_info().1;
    assert!(
        delta > 2.0,
        "dragging the delta drag-value right must raise the log-decade count \
         above the 2.0 default, got {delta}"
    );
}
