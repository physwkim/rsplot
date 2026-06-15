//! Mask overlay transparency slider in `MaskToolsWidget::show_toolbar`
//! (silx `_BaseMaskToolsWidget` `transparencySlider`, :554-577), verified
//! through the egui_kittest harness.
//!
//! The value path (`set_transparency` → `mask_overlay_lut`) is unit-tested in
//! `mask_tools.rs`; this exercises the live UI wiring: the rendered
//! "Transparency" slider, when dragged, routes through `set_transparency` and
//! lowers the widget's transparency. `show_toolbar` is pure egui (the overlay
//! LUT upload is a separate GPU step), so no wgpu render state is needed.

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use siplot::MaskToolsWidget;
use siplot::egui;

#[test]
fn dragging_the_transparency_slider_lowers_the_overlay_alpha() {
    let widget = Rc::new(RefCell::new(MaskToolsWidget::new(8, 8)));
    // Default overlay transparency is silx's 8/10 = 0.8.
    assert!((widget.borrow().transparency() - 0.8).abs() < 1e-6);

    let widget_ui = widget.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 120.0))
        .with_pixels_per_point(1.0)
        .build_ui(move |ui| {
            widget_ui.borrow_mut().show_toolbar(ui);
        });
    harness.step();
    harness.step();

    // Locate the rendered "Transparency" slider and drag its handle toward the
    // left (low) end of its track. A horizontal egui slider maps the pointer x
    // proportionally across the track, so dragging to near the left edge sets a
    // low value. The label text and the slider share the label "Transparency",
    // so disambiguate by the slider's accesskit role (egui renders a Slider as
    // a SpinButton node).
    let rect = harness
        .get_by_role_and_label(egui::accesskit::Role::SpinButton, "Transparency")
        .rect();
    // Overshoot far past the left edge so the value clamps to the slider
    // minimum regardless of the track's interior insets.
    let start = egui::pos2(rect.center().x, rect.center().y);
    let end = egui::pos2(rect.left() - rect.width(), rect.center().y);
    harness.drag_at(start);
    harness.step();
    for t in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
        harness.hover_at(start + (end - start) * t);
        harness.step();
    }
    harness.drop_at(end);
    harness.step();
    harness.step();

    // The wiring identity: the widget's transparency now equals what the slider
    // displays (so the rendered slider drove `set_transparency`), and it dropped
    // far below the 0.8 default toward the 0.0 minimum.
    let node = harness.get_by_role_and_label(egui::accesskit::Role::SpinButton, "Transparency");
    let shown: f32 = node
        .value()
        .expect("slider exposes its value")
        .parse()
        .expect("slider value parses as a number");
    let alpha = widget.borrow().transparency();
    assert!(
        (alpha - shown).abs() < 1e-3,
        "the widget transparency ({alpha}) must equal the slider's value ({shown})"
    );
    assert!(
        alpha < 0.2,
        "dragging the transparency slider to the low end must drop the overlay \
         transparency near the 0.0 minimum, got {alpha}"
    );
}
