//! Default style constants + per-widget colour APIs of `SceneWidget` (R1-24):
//! silx clears its 3D views to grey 51 (`Plot3DWidget.py:161`
//! `setBackgroundColor((0.2, 0.2, 0.2, 1.0))`) and draws the bounding box and
//! text in white (`SceneWidget.py:373-375`). These tests build a widget on a
//! headless wgpu `RenderState` and check the defaults and the
//! foreground/text-colour setters.

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use siplot::SceneWidget;
use siplot::egui::Color32;

#[test]
fn default_style_matches_silx() {
    let rs = create_render_state(default_wgpu_setup());
    let w = SceneWidget::new(&rs, 41);
    // silx background (0.2, 0.2, 0.2, 1.0) → grey 51, not the previous grey 30.
    assert_eq!(w.background(), Color32::from_gray(51));
    // Bounding box and text default to white.
    assert_eq!(w.foreground_color(), Color32::WHITE);
    assert_eq!(w.text_color(), Color32::WHITE);
}

#[test]
fn foreground_and_text_color_setters_round_trip() {
    let rs = create_render_state(default_wgpu_setup());
    let mut w = SceneWidget::new(&rs, 42);
    w.set_foreground_color(&rs, Color32::from_rgb(10, 20, 30));
    assert_eq!(w.foreground_color(), Color32::from_rgb(10, 20, 30));
    w.set_text_color(&rs, Color32::from_rgb(200, 100, 0));
    assert_eq!(w.text_color(), Color32::from_rgb(200, 100, 0));
    // Background setter/getter pair (silx get/setBackgroundColor).
    w.set_background(Color32::BLACK);
    assert_eq!(w.background(), Color32::BLACK);
}

#[test]
fn axes_labels_default_empty_and_update_selectively() {
    // silx `setAxesLabels(xlabel=None, ...)` leaves unset axes unchanged
    // (items/core.py:702-717); Text2D labels default to empty strings.
    let rs = create_render_state(default_wgpu_setup());
    let mut w = SceneWidget::new(&rs, 43);
    assert_eq!(w.axes_labels(), ("", "", ""));
    w.set_axes_labels(Some("X (mm)"), None, Some("Z"));
    assert_eq!(w.axes_labels(), ("X (mm)", "", "Z"));
    w.set_axes_labels(None, Some("Y"), None);
    assert_eq!(w.axes_labels(), ("X (mm)", "Y", "Z"));
}
