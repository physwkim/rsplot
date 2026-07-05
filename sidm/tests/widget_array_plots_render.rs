//! Headless wgpu readback of [`SidmWaveformPlot`] and [`SidmScatterPlot`].
//!
//! The redraw gating and array extraction are unit-tested purely in their
//! modules; this proves the array curve / scatter markers actually reach the
//! screen. The waveform curve is fed a Y array over a `loc://` channel; the
//! scatter markers are injected directly (PyDM's "inject data into the curve"
//! path). Both render in `egui_kittest`'s headless wgpu renderer and the
//! curve-coloured (pure green) pixels are counted, with an empty plot as the
//! control — the same empirical pattern as `tests/widget_time_plot_render.rs`.
//!
//! Needs a GPU (real or software).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use sidm::widgets::{SidmScatterPlot, SidmWaveformPlot};
use sidm::{Engine, PvValue};
use siplot::egui;

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
        .filter(|px| px[0] < 80 && px[1] > 200 && px[2] < 80)
        .count() as u32
}

#[test]
fn waveform_curve_renders_from_a_y_array() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);
    let engine = Engine::new();
    let mut plot = SidmWaveformPlot::new(&rs, 0);
    // Config-bearing loc (type+init) so the connection connects — a bare
    // `loc://` stays disconnected until configured (R3-12); the array write
    // below overrides the scalar init.
    plot.add_channel(
        &engine,
        "loc://wave_y?type=float&init=0",
        egui::Color32::from_rgb(0, 255, 0),
        "y",
    )
    .expect("add channel");

    // Push a rising Y array; the waveform plots it against the sample index.
    let writer = engine.connect("loc://wave_y").expect("writer handle");
    writer.put(PvValue::FloatArray(Arc::from([
        0.0, 1.0, 2.0, 3.0, 4.0, 5.0,
    ])));
    assert!(
        wait_for(
            || writer.read(|s| matches!(s.value, Some(PvValue::FloatArray(_)))),
            Duration::from_secs(2)
        ),
        "waveform Y channel never observed the array"
    );

    let app = Rc::new(RefCell::new(plot));
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
    let green = count_green(image.as_raw());
    drop(engine);
    assert!(
        green > 100,
        "the waveform curve should render many green pixels; got {green}"
    );
}

#[test]
fn scatter_markers_render_from_injected_pairs() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);
    let engine = Engine::new();
    let mut plot = SidmScatterPlot::new(&rs, 0);
    let idx = plot
        .add_xy_channel(
            &engine,
            "loc://scatter_x",
            "loc://scatter_y",
            egui::Color32::from_rgb(0, 255, 0),
            "xy",
        )
        .expect("add channel");
    // Inject a spread of pairs so multiple markers are visible.
    for i in 0..8 {
        let t = f64::from(i);
        plot.inject(idx, t, t * t);
    }

    let app = Rc::new(RefCell::new(plot));
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
    let green = count_green(image.as_raw());
    drop(engine);
    assert!(
        green > 100,
        "the scatter markers should render many green pixels; got {green}"
    );
}

#[test]
fn empty_array_plot_renders_no_curve_color() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);
    let engine = Engine::new();
    let mut plot = SidmWaveformPlot::new(&rs, 0);
    plot.add_channel(
        &engine,
        "loc://wave_empty",
        egui::Color32::from_rgb(0, 255, 0),
        "y",
    )
    .expect("add channel");

    let app = Rc::new(RefCell::new(plot));
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
    let green = count_green(image.as_raw());
    drop(engine);
    assert!(
        green < 60,
        "an empty plot should render almost no curve-coloured pixels; got {green}"
    );
}
