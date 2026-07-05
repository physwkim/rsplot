//! Headless wgpu readback of [`RsdmMultiStateIndicator`].
//!
//! The value→state selection is unit-tested purely in the module; this proves
//! the selected state colour actually reaches the screen and that an out-of-range
//! value shows black (the pre-value `_curr_color`) rather than a state colour. A
//! green fill is configured for state `2`; the channel is driven to `2` (green
//! appears) and to `99` (out of range → no state set → black fill). The fill is
//! probed in green so PyDM's intrinsic red border (always painted) cannot be
//! mistaken for a state colour — the same empirical pattern as
//! `tests/widget_symbol_render.rs`.
//!
//! Needs a GPU (real or software).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsdm::Engine;
use rsdm::widgets::RsdmMultiStateIndicator;
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

fn count_green(raw: &[u8]) -> u32 {
    raw.chunks_exact(4)
        .filter(|px| px[1] > 200 && px[0] < 80 && px[2] < 80)
        .count() as u32
}

/// Render a multi-state indicator whose state `2` is green, with the channel at
/// `value`, and return the count of green pixels.
fn render_multi_state(value: i64) -> u32 {
    let rs = create_render_state(default_wgpu_setup());
    let engine = Engine::new();
    let address = format!("loc://multi_state_demo?type=int&init={value}");
    let indicator = RsdmMultiStateIndicator::new(&engine, &address)
        .expect("connect")
        .with_render_as_rectangle(true)
        .with_state_color(2, egui::Color32::from_rgb(0, 220, 0))
        .with_size(egui::vec2(80.0, 80.0));

    assert!(
        wait_for(
            || indicator.channel().read(|s| s.value.is_some()),
            Duration::from_secs(2)
        ),
        "multi-state channel never observed its init value"
    );

    let app = Rc::new(RefCell::new(indicator));
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
    let green = count_green(image.as_raw());
    drop(engine);
    green
}

#[test]
fn in_range_value_renders_its_state_colour() {
    let green = render_multi_state(2);
    // The green rectangle for state 2 covers many pixels.
    assert!(
        green > 1500,
        "the state-2 green fill should render many pixels; got {green}"
    );
}

#[test]
fn out_of_range_value_renders_black_not_a_state_colour() {
    // 99 is outside 0..=15 → PyDM leaves `_curr_state` unset → black fill (only
    // the intrinsic red border is drawn, which is not green).
    let green = render_multi_state(99);
    assert!(
        green < 50,
        "an out-of-range value should leave the indicator black; got {green}"
    );
}
