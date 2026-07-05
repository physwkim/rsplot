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
use rsdm::Engine;
use rsdm::widgets::{CurveStyle, RsdmScatterPlot, RsdmTimePlot};
use rsplot::{AxisSide, YAxis, egui};

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
    rsplot::install(&rs);

    let engine = Engine::new();
    let mut plot = RsdmTimePlot::new(&rs, 0).with_time_span(6.0);
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
fn set_curve_style_binds_curve_to_extra_axis_and_autoscales_it() {
    // End-to-end proof of the multi-axis wiring through the rsdm widget API:
    // a curve restyled onto an extra (stacked) Y axis via `CurveStyle::with_y_axis`
    // + `set_curve_style` must (a) make that axis autoscale to fit *its* data
    // (through `ensure_axis_autoscale` -> `set_extra_y_autoscale`, applied on the
    // reset-zoom-to-data refit), and (b) render against that scale. The injected
    // Y data (1000..1005) sits far outside the left-axis default, so the line
    // only lands on-screen if the curve is truly bound to the extra axis that
    // fitted it.
    //
    // A scatter plot (not a time plot) is used deliberately: it has no scrolling
    // X window, so each data update refits the autoscale-on axes (including extra
    // axes via `reset_extra_axes_to`) into the default view and the binding is
    // observed directly. `RsdmTimePlot` now also keeps `auto_reset_zoom` on (its
    // Y / y2 / extra axes refit live), but its per-frame X scroll would only add
    // noise to this end-to-end axis-binding check.
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);

    let engine = Engine::new();
    let mut plot = RsdmScatterPlot::new(&rs, 0);
    let extra = plot.plot_mut().add_extra_y_axis(AxisSide::Right);
    let idx = plot
        .add_xy_channel(
            &engine,
            "loc://plot_style_extra_x",
            "loc://plot_style_extra_y",
            egui::Color32::from_rgb(255, 0, 0),
            "xy",
        )
        .expect("add xy channel");
    // A thick connecting line (a scatter curve item may carry one) gives a robust
    // red-pixel signal across the plot; bound to the extra axis it traces the
    // 1000..1005 span diagonally.
    assert!(
        plot.set_curve_style(
            idx,
            CurveStyle::line(egui::Color32::from_rgb(255, 0, 0))
                .with_line_width(4.0)
                .with_y_axis(YAxis::Extra(extra))
        )
    );
    for i in 0..=5 {
        plot.inject(idx, f64::from(i), 1000.0 + f64::from(i));
    }

    // (a) The extra axis autoscaled to fit its curve's 1000..1005 span.
    let (lo, hi) = plot
        .plot()
        .get_graph_y_limits(YAxis::Extra(extra))
        .expect("extra axis range autoscaled from its bound curve");
    assert!(
        lo <= 1000.0 && hi >= 1005.0,
        "extra axis should fit its curve's 1000..1005 span; got {lo}..{hi}"
    );

    // (b) The line reaches the screen against that extra scale.
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
    drop(engine);

    assert!(
        red > 100,
        "the extra-axis line at y≈1000 (axis range ~1000..1005) should render \
         mid-plot; got {red} red px"
    );
}

#[test]
fn set_curve_style_rejects_out_of_range_index() {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);
    let engine = Engine::new();
    let mut plot = RsdmTimePlot::new(&rs, 0);
    // No curves added yet.
    assert!(!plot.set_curve_style(0, CurveStyle::line(egui::Color32::WHITE)));
    drop(engine);
}
