//! `AlphaSlider` item bindings (silx `ActiveImageAlphaSlider` /
//! `NamedItemAlphaSlider`) + the structural retention of image alpha.
//!
//! Two things are verified:
//!
//!  1. **Structural alpha retention** (the "fix the family" change): a scalar
//!     image's global opacity is retained in `RetainedItemData::Image` and is
//!     preserved across every re-upload path — explicit level edits, raw-pixel
//!     autoscale, and median filtering — instead of resetting to the
//!     `ImageSpec::scalar` default of `1.0`. These exercise the `PlotWidget`
//!     image-alpha API directly and need only a headless `RenderState` (the GPU
//!     `update_image` upload), no rendered frame.
//!
//!  2. **The binding widgets**: `ActiveImageAlphaSlider` seeds from the active
//!     image's alpha, disables when there is no scalar active image, and writes
//!     a slider change back to the image; `NamedItemAlphaSlider` does the same
//!     for an image addressed by legend. These need the egui_kittest + wgpu
//!     harness (the slider is an interactive `egui` widget).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui;
use rsplot::{
    ActiveImageAlphaSlider, AutoscaleMode, Colormap, ImageSpec, ItemHandle, NamedItemAlphaSlider,
    PlotWidget,
};

/// A 3×2 scalar ramp image, added to a fresh `PlotWidget`, made active, at the
/// given initial opacity. Returns the widget (in a render state install scope)
/// and the image handle. No frame is rendered — the image-alpha API needs only
/// the GPU upload, not the transform.
fn plot_with_image(alpha0: f32, legend: Option<&str>) -> (PlotWidget, ItemHandle) {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);
    let mut plot = PlotWidget::new(&rs, 0);
    let pixels = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
    let mut spec = ImageSpec::scalar(3, 2, &pixels, Colormap::viridis(0.0, 6.0));
    spec.alpha = alpha0;
    let handle = plot.add_image_spec(spec);
    if let Some(legend) = legend {
        plot.set_item_legend(handle, legend);
    }
    plot.set_active_image(Some(handle));
    (plot, handle)
}

// --- 1. structural retention across re-upload paths ----------------------

#[test]
fn image_alpha_round_trips_through_the_setter() {
    let (mut plot, _handle) = plot_with_image(1.0, None);
    assert_eq!(plot.active_image_alpha(), Some(1.0));

    assert!(plot.set_active_image_alpha(0.4));
    let a = plot.active_image_alpha().expect("active image has alpha");
    assert!((a - 0.4).abs() < 1e-6, "alpha setter round-trips, got {a}");

    // Out-of-range alpha is clamped (silx AlphaMixIn.setAlpha).
    assert!(plot.set_active_image_alpha(2.0));
    assert_eq!(plot.active_image_alpha(), Some(1.0));
    assert!(plot.set_active_image_alpha(-1.0));
    assert_eq!(plot.active_image_alpha(), Some(0.0));
}

#[test]
fn explicit_level_edit_preserves_alpha() {
    let (mut plot, _handle) = plot_with_image(0.3, None);
    // A colorbar-drag level edit re-uploads the image; alpha must survive.
    assert!(plot.set_active_image_levels(1.0, 5.0));
    let a = plot.active_image_alpha().expect("alpha retained");
    assert!(
        (a - 0.3).abs() < 1e-6,
        "set_active_image_levels must preserve the image alpha, got {a}"
    );
}

#[test]
fn raw_pixel_autoscale_preserves_alpha() {
    let (mut plot, _handle) = plot_with_image(0.3, None);
    assert!(plot.autoscale_active_image(AutoscaleMode::MinMax).is_some());
    let a = plot.active_image_alpha().expect("alpha retained");
    assert!(
        (a - 0.3).abs() < 1e-6,
        "autoscale_active_image must preserve the image alpha, got {a}"
    );
}

#[test]
fn median_filter_preserves_alpha() {
    let (mut plot, _handle) = plot_with_image(0.3, None);
    // A 3×3 median filter re-uploads the filtered image; alpha must survive.
    assert!(plot.apply_median_filter(3, false));
    let a = plot.active_image_alpha().expect("alpha retained");
    assert!(
        (a - 0.3).abs() < 1e-6,
        "apply_median_filter must preserve the image alpha, got {a}"
    );
}

#[test]
fn per_handle_alpha_addresses_an_image_by_legend() {
    let (mut plot, handle) = plot_with_image(1.0, Some("img"));
    let by_legend = plot.image_by_legend("img").expect("legend resolves");
    assert_eq!(by_legend, handle);
    assert_eq!(plot.image_alpha(handle), Some(1.0));

    assert!(plot.set_image_alpha(handle, 0.25));
    let a = plot.image_alpha(handle).expect("alpha retained");
    assert!(
        (a - 0.25).abs() < 1e-6,
        "set_image_alpha round-trips, got {a}"
    );

    // A non-existent legend has no addressable alpha.
    assert!(plot.image_by_legend("missing").is_none());
}

// --- 2. binding widgets through the kittest harness ----------------------

/// Shared state of an [`active_slider_harness`]: the plot, the bound slider, the
/// captured slider-track rect, and the harness itself.
type ActiveSliderBundle = (
    Rc<RefCell<PlotWidget>>,
    Rc<RefCell<ActiveImageAlphaSlider>>,
    Rc<Cell<egui::Rect>>,
    Harness<'static>,
);

/// Build a kittest harness around a shared `PlotWidget` whose `show` is driven
/// each frame, plus an `ActiveImageAlphaSlider` shown above it; capture the
/// slider's response rect each frame so a drag can target the track. Renders two
/// frames so the plot transform is cached.
fn active_slider_harness(alpha0: f32, with_image: bool) -> ActiveSliderBundle {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);
    let mut plot = PlotWidget::new(&rs, 0);
    if with_image {
        let pixels = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let mut spec = ImageSpec::scalar(3, 2, &pixels, Colormap::viridis(0.0, 6.0));
        spec.alpha = alpha0;
        let handle = plot.add_image_spec(spec);
        plot.set_active_image(Some(handle));
    }

    let plot = Rc::new(RefCell::new(plot));
    let slider = Rc::new(RefCell::new(
        ActiveImageAlphaSlider::new().with_orientation(rsplot::AlphaSliderOrientation::Vertical),
    ));
    let track = Rc::new(Cell::new(egui::Rect::NOTHING));

    let plot_ui = plot.clone();
    let slider_ui = slider.clone();
    let track_ui = track.clone();
    let renderer = WgpuTestRenderer::from_render_state(rs.clone());
    let harness = Harness::builder()
        .with_size(egui::vec2(400.0, 400.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            let resp = slider_ui.borrow_mut().show(ui, &mut plot_ui.borrow_mut());
            track_ui.set(resp.rect);
            plot_ui.borrow_mut().show(ui);
        });
    (plot, slider, track, harness)
}

#[test]
fn active_image_slider_seeds_from_the_image_alpha() {
    let (plot, slider, _track, mut harness) = active_slider_harness(0.5, true);
    harness.step();
    harness.step();
    // The slider bound to the active image and seeded from its 0.5 opacity.
    let a = slider.borrow().alpha();
    assert!(
        (a - 0.5).abs() < 0.01,
        "active-image slider must seed from the image's alpha (0.5), got {a}"
    );
    assert_eq!(plot.borrow().active_image_alpha(), Some(0.5));
}

#[test]
fn active_image_slider_disables_without_an_active_image() {
    // No image: the slider has nothing to bind to (silx getItem() -> None).
    let (plot, _slider, _track, mut harness) = active_slider_harness(1.0, false);
    harness.step();
    harness.step();
    assert_eq!(
        plot.borrow().active_image_alpha(),
        None,
        "no scalar active image means no addressable alpha"
    );
    assert_eq!(plot.borrow().active_image_handle(), None);
}

#[test]
fn dragging_the_active_image_slider_writes_alpha_back_to_the_image() {
    let (plot, slider, track, mut harness) = active_slider_harness(1.0, true);
    harness.step();
    harness.step();
    assert_eq!(plot.borrow().active_image_alpha(), Some(1.0));

    // Drag down the vertical track: a vertical egui slider's value increases
    // upward, so dragging from near the top toward the bottom lowers it.
    let rect = track.get();
    let top = egui::pos2(rect.center().x, rect.top() + rect.height() * 0.1);
    let bottom = egui::pos2(rect.center().x, rect.top() + rect.height() * 0.9);
    harness.drag_at(top);
    harness.step();
    for t in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
        harness.hover_at(top + (bottom - top) * t);
        harness.step();
    }
    harness.drop_at(bottom);
    harness.step();
    harness.step();

    // The image's opacity dropped from 1.0, and the wiring identity holds: the
    // applied image alpha equals the slider's reported alpha.
    let img_alpha = plot.borrow().active_image_alpha().expect("active image");
    let slider_alpha = slider.borrow().alpha();
    assert!(
        img_alpha < 0.95,
        "dragging the slider down must lower the image alpha below 1.0, got {img_alpha}"
    );
    assert!(
        (img_alpha - slider_alpha).abs() < 1e-3,
        "the image alpha ({img_alpha}) must equal the slider's value ({slider_alpha})"
    );
}

#[test]
fn named_item_slider_binds_by_legend_and_disables_for_unknown() {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);
    let mut plot = PlotWidget::new(&rs, 0);
    let pixels = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
    let mut spec = ImageSpec::scalar(3, 2, &pixels, Colormap::viridis(0.0, 6.0));
    spec.alpha = 0.7;
    let handle = plot.add_image_spec(spec);
    plot.set_item_legend(handle, "img");
    plot.set_active_image(Some(handle));

    let plot = Rc::new(RefCell::new(plot));
    // Two named sliders: one targets the real legend, one an unknown legend.
    let bound = Rc::new(RefCell::new(NamedItemAlphaSlider::new("img")));
    let unbound = Rc::new(RefCell::new(NamedItemAlphaSlider::new("nope")));

    let plot_ui = plot.clone();
    let bound_ui = bound.clone();
    let unbound_ui = unbound.clone();
    let renderer = WgpuTestRenderer::from_render_state(rs.clone());
    let mut harness = Harness::builder()
        .with_size(egui::vec2(400.0, 200.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            bound_ui.borrow_mut().show(ui, &mut plot_ui.borrow_mut());
            unbound_ui.borrow_mut().show(ui, &mut plot_ui.borrow_mut());
            plot_ui.borrow_mut().show(ui);
        });
    harness.step();
    harness.step();

    // The bound slider seeded from the image's 0.7 opacity; the unbound one
    // stayed at the fully-opaque default (no item to seed from).
    assert_eq!(bound.borrow().legend(), "img");
    let a = bound.borrow().alpha();
    assert!(
        (a - 0.7).abs() < 0.01,
        "named slider must seed from its image's alpha (0.7), got {a}"
    );
    assert_eq!(
        unbound.borrow().alpha(),
        1.0,
        "a slider naming an absent image stays at the opaque default"
    );
}
