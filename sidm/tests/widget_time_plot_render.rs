//! Headless wgpu readback of [`PydmTimePlot`].
//!
//! The sample-feeding logic and fixed-rate timing are unit-tested purely in
//! `widgets/time_plot.rs`; this proves the curve actually reaches the screen.
//! It injects a spread of synthetic `(time, value)` samples (PyDM's "inject data
//! into the curve" path) that fall inside the scroll window, renders the plot in
//! `egui_kittest`'s headless wgpu renderer, and counts the curve-coloured (pure
//! green) pixels — the same empirical pattern as `tests/widget_byte_render.rs`.
//! An empty plot is rendered as a control so the threshold reflects the curve,
//! not the chrome.
//!
//! Needs a GPU (real or software): it constructs a wgpu `RenderState`, installs
//! siplot's pipelines, and reads back the rendered texture (mirrors
//! `tests/mask_pointer_offset.rs`).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use sidm::Engine;
use sidm::widgets::PydmTimePlot;
use siplot::egui;

struct App {
    plot: PydmTimePlot,
}

impl App {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.plot.show(ui);
    }
}

fn now_epoch_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs_f64()
}

/// Pure-green (curve colour) pixel count in an RGBA frame.
fn count_green(raw: &[u8]) -> u32 {
    raw.chunks_exact(4)
        .filter(|px| px[0] < 80 && px[1] > 200 && px[2] < 80)
        .count() as u32
}

/// Render a time plot after running `setup` on it (e.g. injecting samples) and
/// return the green pixel count.
fn render_with(setup: impl FnOnce(&mut PydmTimePlot, usize)) -> u32 {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);

    let engine = Engine::new();
    let mut plot = PydmTimePlot::new(&rs, 0).with_time_span(6.0);
    let idx = plot
        .add_channel(
            &engine,
            "loc://time_plot_render",
            egui::Color32::from_rgb(0, 255, 0),
            "v",
        )
        .expect("add channel");
    setup(&mut plot, idx);

    let app = Rc::new(RefCell::new(App { plot }));
    let renderer = WgpuTestRenderer::from_render_state(rs);
    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(400.0, 300.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| app_ui.borrow_mut().ui(ui));

    // Two frames: the first lays out and scrolls the window, the second settles.
    harness.step();
    harness.step();
    let image = harness.render().expect("headless wgpu render");
    let green = count_green(image.as_raw());
    // Keep the engine (and thus the channel) alive through rendering.
    drop(engine);
    green
}

#[test]
fn injected_samples_render_a_curve() {
    // A rising ramp across the last ~4 seconds, inside the 6-second window.
    let green = render_with(|plot, idx| {
        let now = now_epoch_secs();
        for i in 0..=4 {
            plot.inject(idx, now - f64::from(4 - i), f64::from(i));
        }
    });
    assert!(
        green > 100,
        "the injected curve should render many green pixels; got {green}"
    );
}

#[test]
fn empty_plot_renders_no_curve_color() {
    // No samples injected: the curve colour must be essentially absent (only the
    // tiny legend swatch could contribute), proving the curve drives the count
    // in the test above.
    let green = render_with(|_plot, _idx| {});
    assert!(
        green < 60,
        "an empty plot should render almost no curve-coloured pixels; got {green}"
    );
}
