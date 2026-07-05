//! Headless wgpu readback of [`RsdmSymbol`].
//!
//! The value→state lookup is unit-tested purely in the module; this proves the
//! selected symbol actually reaches the screen and that a value with no
//! configured state draws nothing. A red circle is configured for state `1`; the
//! channel is driven to `1` (red appears) and to `2` (no state → nothing) — the
//! same empirical pattern as `tests/widget_drawing_render.rs` (plain egui
//! painting, no `rsplot::install`).
//!
//! Needs a GPU (real or software).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsdm::Engine;
use rsdm::widgets::{DrawingShape, RsdmSymbol};
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

fn count_red(raw: &[u8]) -> u32 {
    raw.chunks_exact(4)
        .filter(|px| px[0] > 200 && px[1] < 80 && px[2] < 80)
        .count() as u32
}

/// Render a symbol whose state `1` is a red circle, with the channel at `value`.
fn render_symbol(value: i64) -> u32 {
    let rs = create_render_state(default_wgpu_setup());
    let engine = Engine::new();
    let address = format!("loc://symbol_demo?type=int&init={value}");
    let symbol = RsdmSymbol::new(&engine, &address)
        .expect("connect")
        .with_state(0, DrawingShape::Circle, egui::Color32::from_gray(90))
        .with_state(1, DrawingShape::Circle, egui::Color32::from_rgb(255, 0, 0))
        .with_size(egui::vec2(80.0, 80.0));

    assert!(
        wait_for(
            || symbol.channel().read(|s| s.value.is_some()),
            Duration::from_secs(2)
        ),
        "symbol channel never observed its init value"
    );

    let app = Rc::new(RefCell::new(symbol));
    let renderer = WgpuTestRenderer::from_render_state(rs);
    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(160.0, 160.0))
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
fn matching_state_renders_its_symbol() {
    let red = render_symbol(1);
    // The red circle for state 1 covers many pixels.
    assert!(
        red > 1500,
        "the state-1 red circle should render many pixels; got {red}"
    );
}

#[test]
fn value_without_a_state_renders_nothing() {
    // State 2 is not configured → PyDM paints nothing.
    let red = render_symbol(2);
    assert!(
        red < 50,
        "an unconfigured value should render no symbol pixels; got {red}"
    );
}
