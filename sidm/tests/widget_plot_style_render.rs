//! Headless wgpu readback of per-curve styling ([`CurveStyle`] / `set_curve_style`).
//!
//! The `CurveStyle` → `CurveSpec` mapping is unit-tested purely in
//! `widgets/plot_style.rs`; this proves a restyle actually reaches the screen. A
//! time-plot curve is added in green, then restyled to a thick red line via
//! `set_curve_style`, and the rendered frame is checked to contain red curve
//! pixels and essentially no green — proving the new style (colour + width)
//! replaced the original on the GPU. Mirrors `tests/widget_time_plot_render.rs`.
//!
//! Needs a GPU (real or software).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use sidm::Engine;
use sidm::widgets::{CurveStyle, PydmTimePlot};
use siplot::egui;

fn now_epoch_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs_f64()
}

fn count_color(raw: &[u8], want: [u8; 3]) -> u32 {
    raw.chunks_exact(4)
        .filter(|px| {
            let dominant = |c: usize| {
                if want[c] > 200 {
                    px[c] > 200
                } else {
                    px[c] < 80
                }
            };
            dominant(0) && dominant(1) && dominant(2)
        })
        .count() as u32
}

#[test]
fn set_curve_style_recolors_the_curve_on_screen() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);

    let engine = Engine::new();
    let mut plot = PydmTimePlot::new(&rs, 0).with_time_span(6.0);
    let idx = plot
        .add_channel(
            &engine,
            "loc://plot_style_render",
            egui::Color32::from_rgb(0, 255, 0),
            "v",
        )
        .expect("add channel");
    // Restyle to a thick red line, then inject a ramp inside the window.
    assert!(plot.set_curve_style(
        idx,
        CurveStyle::line(egui::Color32::from_rgb(255, 0, 0)).with_line_width(4.0)
    ));
    let now = now_epoch_secs();
    for i in 0..=4 {
        plot.inject(idx, now - f64::from(4 - i), f64::from(i));
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
    let red = count_color(image.as_raw(), [255, 0, 0]);
    let green = count_color(image.as_raw(), [0, 255, 0]);
    drop(engine);

    assert!(
        red > 100,
        "the restyled red curve should render many red pixels; got {red}"
    );
    assert!(
        green < 60,
        "the original green colour should be gone after restyle; got {green}"
    );
}

#[test]
fn set_curve_style_rejects_out_of_range_index() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);
    let engine = Engine::new();
    let mut plot = PydmTimePlot::new(&rs, 0);
    // No curves added yet.
    assert!(!plot.set_curve_style(0, CurveStyle::line(egui::Color32::WHITE)));
    drop(engine);
}
