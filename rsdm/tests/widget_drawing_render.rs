//! Headless wgpu readback of [`RsdmDrawing`].
//!
//! The colour decision and rotation maths are unit-tested purely in the module;
//! this proves a filled shape actually reaches the screen. A red-filled
//! rectangle is rendered and its red pixels counted, with a transparent-fill
//! drawing as the control — the same empirical pattern as
//! `tests/widget_base_render.rs` (plain egui painting, no `rsplot::install`).
//!
//! Needs a GPU (real or software).

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsdm::Engine;
use rsdm::widgets::{DrawingShape, RsdmDrawing};
use rsplot::egui;

fn count_red(raw: &[u8]) -> u32 {
    raw.chunks_exact(4)
        .filter(|px| px[0] > 200 && px[1] < 80 && px[2] < 80)
        .count() as u32
}

fn render_drawing(fill: egui::Color32) -> u32 {
    let rs = create_render_state(default_wgpu_setup());
    let engine = Engine::new();
    let drawing = RsdmDrawing::new(&engine, "loc://drawing_demo", DrawingShape::Rectangle)
        .expect("connect")
        .with_fill(fill)
        .with_size(egui::vec2(120.0, 80.0));

    let app = Rc::new(RefCell::new(drawing));
    let renderer = WgpuTestRenderer::from_render_state(rs);
    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(300.0, 200.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            app_ui.borrow_mut().show(ui);
        });
    harness.step();
    harness.step();
    let image = harness.render().expect("headless wgpu render");
    let red = count_red(image.as_raw());
    drop(engine);
    red
}

#[test]
fn filled_rectangle_renders_its_fill_color() {
    let red = render_drawing(egui::Color32::from_rgb(255, 0, 0));
    // A 120×80 filled rectangle should cover thousands of red pixels.
    assert!(
        red > 3000,
        "the filled rectangle should render many fill-coloured pixels; got {red}"
    );
}

#[test]
fn transparent_fill_renders_no_color() {
    let red = render_drawing(egui::Color32::TRANSPARENT);
    assert!(
        red < 50,
        "a transparent drawing should render almost no fill-coloured pixels; got {red}"
    );
}
