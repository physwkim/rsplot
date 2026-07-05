//! Headless wgpu readback of [`RsdmScaleIndicator`].
//!
//! The proportion maths is unit-tested purely in the module; this proves the bar
//! actually reaches the screen and tracks the value. A red bar over `[0, 100]` is
//! rendered at a high value and a low value (more red at the high one), and an
//! off-scale value renders no bar at all — the same empirical pattern as
//! `tests/widget_drawing_render.rs` (plain egui painting, no `rsplot::install`).
//!
//! Needs a GPU (real or software).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsdm::Engine;
use rsdm::widgets::RsdmScaleIndicator;
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

/// Render a bar-style scale over `[0, 100]` showing `init` and count red pixels.
fn render_scale(init: f64) -> u32 {
    let rs = create_render_state(default_wgpu_setup());
    let engine = Engine::new();
    let address = format!("loc://scale_demo?type=float&init={init}");
    let scale = RsdmScaleIndicator::new(&engine, &address)
        .expect("connect")
        .with_limits(0.0, 100.0)
        .with_bar_indicator(true)
        .with_value_label(false)
        .with_bar_color(egui::Color32::from_rgb(255, 0, 0))
        .with_size(egui::vec2(240.0, 40.0));

    // The loc:// init value arrives asynchronously; wait for it before rendering.
    assert!(
        wait_for(
            || scale.channel().read(|s| s.value.is_some()),
            Duration::from_secs(2)
        ),
        "scale channel never observed its init value"
    );

    let app = Rc::new(RefCell::new(scale));
    let renderer = WgpuTestRenderer::from_render_state(rs);
    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(320.0, 120.0))
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
fn bar_grows_with_the_value() {
    let high = render_scale(75.0);
    let low = render_scale(25.0);
    // The bar fills from the origin to the value proportion, so 75% covers
    // markedly more red than 25%.
    assert!(
        high > low + 1000,
        "the bar at 75% should cover more pixels than at 25%; high={high} low={low}"
    );
    assert!(low > 500, "the 25% bar should still render; got {low}");
}

#[test]
fn off_scale_value_renders_no_bar() {
    // 150 is above the [0, 100] upper limit → off-scale, no bar drawn.
    let red = render_scale(150.0);
    assert!(
        red < 50,
        "an off-scale value should render no bar-coloured pixels; got {red}"
    );
}
