//! Headless wgpu readback of [`RsdmImageView`].
//!
//! The reshape, array extraction, and colour-range logic are unit-tested purely
//! in the module; this proves a flat array channel actually reaches the screen
//! as a colour-mapped image. A 16×16 gradient is pushed over a `loc://` channel
//! and the colour-mapped (saturated, non-grey) pixels are counted, with an
//! image-less view as the control — the same empirical pattern as
//! `tests/widget_array_plots_render.rs`.
//!
//! The rsplot colorbar and side histograms render a viridis gradient regardless
//! of whether *our* array reached `set_image`, so they are turned off here to
//! isolate the array→reshape→image pipeline this widget owns; an image-less view
//! then has essentially no colour-mapped pixels.
//!
//! Needs a GPU (real or software).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsdm::widgets::RsdmImageView;
use rsdm::{Engine, PvValue};
use rsplot::egui;

fn wait_for(mut cond: impl FnMut() -> bool, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    cond()
}

/// Count pixels whose channels are clearly separated — i.e. colour-mapped image
/// pixels (viridis purple/green/yellow), not the grey plot chrome or background.
fn count_colorful(raw: &[u8]) -> u32 {
    raw.chunks_exact(4)
        .filter(|px| {
            let mx = i32::from(px[0].max(px[1]).max(px[2]));
            let mn = i32::from(px[0].min(px[1]).min(px[2]));
            mx - mn > 40
        })
        .count() as u32
}

/// Hide the colorbar and side histograms so only the image contributes
/// colour-mapped pixels.
fn bare(view: &mut RsdmImageView) {
    view.view_mut().set_show_colorbar(false);
    view.view_mut().set_side_histogram_displayed(false);
}

#[test]
fn gradient_array_renders_a_color_mapped_image() {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);
    let engine = Engine::new();
    // Config-bearing loc (type+init) so the connection connects — a bare
    // `loc://` stays disconnected until configured (R3-12); the array write
    // below overrides the scalar init.
    let mut view = RsdmImageView::new(&engine, &rs, 0, "loc://cam?type=float&init=0", None)
        .expect("connect image channel")
        .with_width(16)
        .with_color_map_range(0.0, 255.0);
    bare(&mut view);

    // A 16×16 gradient spanning the full colormap range.
    let pixels: Vec<f64> = (0..256).map(|i| i as f64).collect();
    let writer = engine.connect("loc://cam").expect("writer handle");
    writer.put(PvValue::FloatArray(Arc::from(pixels)));
    assert!(
        wait_for(
            || writer.read(|s| matches!(s.value, Some(PvValue::FloatArray(_)))),
            Duration::from_secs(2)
        ),
        "image channel never observed the array"
    );

    let app = Rc::new(RefCell::new(view));
    let renderer = WgpuTestRenderer::from_render_state(rs);
    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(400.0, 300.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            app_ui.borrow_mut().show(ui);
        });
    harness.step();
    harness.step();
    let image = harness.render().expect("headless wgpu render");
    let colorful = count_colorful(image.as_raw());
    assert!(
        app.borrow().has_image(),
        "the view should have uploaded an image"
    );
    drop(engine);
    assert!(
        colorful > 2000,
        "the colour-mapped image should render many saturated pixels; got {colorful}"
    );
}

#[test]
fn middle_click_copies_image_and_width_channels() {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);
    let engine = Engine::new();
    let mut view = RsdmImageView::new(
        &engine,
        &rs,
        0,
        "loc://cam_copy",
        Some("loc://cam_copy_w?type=int&init=16"),
    )
    .expect("connect image channel")
    .with_width(16);
    bare(&mut view);

    let app = Rc::new(RefCell::new(view));
    let renderer = WgpuTestRenderer::from_render_state(rs);
    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(400.0, 300.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            app_ui.borrow_mut().show(ui);
        });
    let pos = egui::pos2(200.0, 150.0);
    harness.step();
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos));
    harness.step();
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Middle,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    let copies: Vec<String> = harness
        .output()
        .platform_output
        .commands
        .iter()
        .filter_map(|c| match c {
            egui::OutputCommand::CopyText(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    drop(engine);
    // PyDM channels() order: image first, then width; protocols stripped.
    assert_eq!(
        copies,
        vec!["cam_copy cam_copy_w?type=int&init=16".to_owned()],
        "middle press over the image view must copy both channel names"
    );
}

#[test]
fn view_without_data_renders_no_color_mapped_image() {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);
    let engine = Engine::new();
    let mut view = RsdmImageView::new(&engine, &rs, 0, "loc://cam_empty", None)
        .expect("connect image channel")
        .with_width(16);
    bare(&mut view);

    let app = Rc::new(RefCell::new(view));
    let renderer = WgpuTestRenderer::from_render_state(rs);
    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(400.0, 300.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            app_ui.borrow_mut().show(ui);
        });
    harness.step();
    harness.step();
    let image = harness.render().expect("headless wgpu render");
    let colorful = count_colorful(image.as_raw());
    assert!(
        !app.borrow().has_image(),
        "an image-less view should not have uploaded an image"
    );
    drop(engine);
    assert!(
        colorful < 200,
        "an image-less view should render almost no colour-mapped pixels; got {colorful}"
    );
}
