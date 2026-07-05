//! Behavioural test of the pyqtgraph-style Y-axis range control on the plot
//! widgets (`widgets/plot_menu.rs`), exercised through the public
//! `set_y_range` / `enable_y_autoscale` methods the context menu also drives.
//!
//! No egui harness is needed: `inject` runs the same `update_curve_spec` ->
//! `apply_auto_limits` path a live update does, so the Y limits settle
//! synchronously and can be read straight off the plot model. A wgpu
//! `RenderState` is still required to construct the GPU-backed plot.

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use rsdm::Engine;
use rsdm::widgets::RsdmScatterPlot;
use rsplot::YAxis;
use rsplot::egui::Color32;

fn y_limits(plot: &RsdmScatterPlot) -> (f64, f64) {
    plot.plot()
        .get_graph_y_limits(YAxis::Left)
        .expect("left-Y limits")
}

#[test]
fn set_y_range_pins_y_and_survives_updates_then_autoscale_refits() {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);

    let engine = Engine::new();
    let mut plot = RsdmScatterPlot::new(&rs, 0);
    let idx = plot
        .add_xy_channel(
            &engine,
            "loc://plot_menu_x",
            "loc://plot_menu_y",
            Color32::from_rgb(0, 200, 255),
            "xy",
        )
        .expect("add channel");

    // Default: live autoscale fits the data (y in 1000..=1005).
    for i in 0..=5 {
        plot.inject(idx, f64::from(i), 1000.0 + f64::from(i));
    }
    let (lo, hi) = y_limits(&plot);
    assert!(
        lo <= 1000.0 && hi >= 1005.0,
        "default autoscale should fit the injected 1000..1005 data; got ({lo}, {hi})"
    );

    // Pin a fixed range: autoscale turns off and the exact limits apply.
    plot.set_y_range(0.0, 10.0);
    assert_eq!(
        y_limits(&plot),
        (0.0, 10.0),
        "set_y_range should apply the exact pinned range"
    );

    // A data update far outside the pinned range must NOT move it (autoscale is
    // off, so apply_auto_limits preserves the manual Y).
    plot.inject(idx, 6.0, 2000.0);
    assert_eq!(
        y_limits(&plot),
        (0.0, 10.0),
        "the pinned range must survive a streaming update; got {:?}",
        y_limits(&plot)
    );

    // Re-enabling autoscale refits to all data, now spanning 1000..=2000.
    plot.enable_y_autoscale();
    let (lo, hi) = y_limits(&plot);
    assert!(
        lo <= 1000.0 && hi >= 2000.0,
        "enable_y_autoscale should refit to bracket all data (1000..2000); got ({lo}, {hi})"
    );

    drop(engine);
}
