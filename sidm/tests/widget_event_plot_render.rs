//! Headless wgpu readback of [`PydmEventPlot`].
//!
//! `event_sample` (the `(x_idx, y_idx)` selection) is unit-tested purely in the
//! module; this drives real `loc://` event arrays through the widget — exercising
//! the poll → extract → buffer → redraw pipeline inside `show` — and checks that
//! the accumulated markers reach the screen. An empty plot is the control so the
//! threshold reflects the markers, not the chrome. Mirrors
//! `tests/widget_time_plot_render.rs`.
//!
//! Needs a GPU (real or software).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use sidm::widgets::PydmEventPlot;
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

fn count_red(raw: &[u8]) -> u32 {
    raw.chunks_exact(4)
        .filter(|px| px[0] > 200 && px[1] < 80 && px[2] < 80)
        .count() as u32
}

/// Drive each `(x, y)` sample as a two-element event array over a `loc://`
/// channel (selected by `x_idx=0`, `y_idx=1`), stepping the harness between
/// updates so each is accumulated. Returns `(red_pixels, points_accumulated)`.
fn render_events(samples: &[(f64, f64)]) -> (u32, usize) {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);
    let engine = Engine::new();
    let mut plot = PydmEventPlot::new(&rs, 0);
    let idx = plot
        .add_channel(
            &engine,
            "loc://event_render",
            0,
            1,
            egui::Color32::from_rgb(255, 0, 0),
            "ev",
        )
        .expect("add channel");

    let writer = engine.connect("loc://event_render").expect("writer handle");
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

    for &(x, y) in samples {
        let prev = writer.read(|s| s.stamp);
        writer.put(PvValue::FloatArray(Arc::from(vec![x, y])));
        assert!(
            wait_for(|| writer.read(|s| s.stamp) != prev, Duration::from_secs(2)),
            "event array put never observed"
        );
        // One frame per update so each (x, y) is polled and accumulated.
        harness.step();
    }
    harness.step();
    let image = harness.render().expect("headless wgpu render");
    let red = count_red(image.as_raw());
    let points = app.borrow().point_count(idx).expect("curve exists");
    drop(engine);
    (red, points)
}

#[test]
fn event_arrays_accumulate_and_render_markers() {
    let samples = [
        (1.0, 1.0),
        (2.0, 3.0),
        (3.0, 2.0),
        (4.0, 5.0),
        (5.0, 1.0),
        (6.0, 4.0),
    ];
    let (red, points) = render_events(&samples);
    assert_eq!(
        points,
        samples.len(),
        "every in-range event array should add one point"
    );
    assert!(
        red > 100,
        "the accumulated markers should render many red pixels; got {red}"
    );
}

#[test]
fn empty_event_plot_renders_no_markers() {
    let (red, points) = render_events(&[]);
    assert_eq!(points, 0, "no events → no points");
    assert!(
        red < 50,
        "an empty event plot should render almost no marker pixels; got {red}"
    );
}
