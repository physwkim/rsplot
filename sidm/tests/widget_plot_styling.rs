//! Builder styling on the plot widgets (R1-36): title / axis labels / pinned
//! ranges / colours forwarded to the underlying `Plot1D`, asserted through its
//! getters. No frame is rendered, but constructing a plot needs a wgpu
//! `RenderState` (real or software), like `widget_time_plot_render.rs`.

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use sidm::widgets::{SidmScatterPlot, SidmTimePlot, SidmWaveformPlot};
use siplot::YAxis;
use siplot::egui::Color32;

#[test]
fn builders_style_the_underlying_plot() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);

    // Waveform: the full surface, including both pinned ranges (MEDM cartesian
    // plot rangeStyle="user-specified" on x_axis / y1_axis).
    let wf = SidmWaveformPlot::new(&rs, 0)
        .with_title("Waveform")
        .with_x_label("Index")
        .with_y_label("Counts")
        .with_x_range(0.0, 100.0)
        .with_y_range(-5.0, 5.0)
        .with_axis_color(Color32::RED)
        .with_background_color(Color32::BLACK);
    let plot = wf.plot();
    assert_eq!(plot.graph_title(), Some("Waveform"));
    assert_eq!(plot.graph_x_label(), Some("Index"));
    assert_eq!(plot.graph_y_label(YAxis::Left), Some("Counts"));
    assert_eq!(plot.get_graph_x_limits(), (0.0, 100.0));
    assert_eq!(plot.get_graph_y_limits(YAxis::Left), Some((-5.0, 5.0)));
    assert!(
        !plot.plot().x_autoscale(),
        "with_x_range must pin the X axis (autoscale off)"
    );
    assert!(
        !plot.plot().y_autoscale(),
        "with_y_range must pin the Y axis (autoscale off)"
    );
    assert_eq!(plot.data_background_color(), Color32::BLACK);

    // Time plot (strip chart): plotcom title/ylabel + a pinned Y range; the X
    // axis stays the scrolling time window (no with_x_range on purpose).
    let tp = SidmTimePlot::new(&rs, 1)
        .with_title("Trend")
        .with_x_label("Time (s)")
        .with_y_label("Volts")
        .with_y_range(0.0, 10.0);
    let plot = tp.plot();
    assert_eq!(plot.graph_title(), Some("Trend"));
    assert_eq!(plot.graph_x_label(), Some("Time (s)"));
    assert_eq!(plot.graph_y_label(YAxis::Left), Some("Volts"));
    assert_eq!(plot.get_graph_y_limits(YAxis::Left), Some((0.0, 10.0)));
    assert!(!plot.plot().y_autoscale());

    // Scatter: shares the same builder set; spot-check X.
    let sc = SidmScatterPlot::new(&rs, 2)
        .with_x_label("X")
        .with_x_range(1.0, 2.0);
    let plot = sc.plot();
    assert_eq!(plot.graph_x_label(), Some("X"));
    assert_eq!(plot.get_graph_x_limits(), (1.0, 2.0));
    assert!(!plot.plot().x_autoscale());
}
