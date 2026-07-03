//! Widget-path coverage for the silx `_forceResetZoom` cross-axis defaults
//! (`PlotWidget.py:3326-3335`) through the real `PlotWidget` verbs:
//!
//! - a plot whose only curve is bound to the right (y2) axis must refit —
//!   X from its own data, the LEFT axis adopting the right range — instead of
//!   being skipped by the old x/y_left early-return;
//! - an itemless explicit Reset Zoom lands on silx's `(1, 100)` home view.
//!
//! Needs a GPU (real or software); mirrors `tests/roi_events.rs`' harness.

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use siplot::egui::Color32;
use siplot::{Plot1D, YAxis};

#[test]
fn right_axis_only_plot_refits_on_reset() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);
    let mut plot = Plot1D::new(&rs, 0);

    let xs: Vec<f64> = (0..=10).map(|i| i as f64).collect();
    let ys: Vec<f64> = xs.iter().map(|x| 100.0 + 10.0 * x).collect();
    let h = plot.add_curve(&xs, &ys, Color32::RED);
    // Move the only curve to the right axis: the left axis now has no data.
    assert!(plot.set_curve_y_axis(h, YAxis::Right));

    // Pin the view somewhere else, then reset: the refit must still run.
    plot.plot_mut().limits = (0.0, 1.0, 0.0, 1.0);
    plot.reset_zoom_to_data();

    let limits = plot.plot().limits;
    let y2 = plot.plot().y2.expect("right axis created from right data");
    // X refits from its own data; the left axis adopts the right range
    // (silx `ranges.y is None` with yright present, PlotWidget.py:3330-3335).
    assert_eq!((limits.0, limits.1), (0.0, 10.0), "{limits:?}");
    assert_eq!((limits.2, limits.3), y2, "left adopts the right range");
    assert_eq!(y2, (100.0, 200.0));
}

#[test]
fn itemless_reset_lands_on_silx_home_view() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);
    let mut plot = Plot1D::new(&rs, 0);

    plot.plot_mut().limits = (3.0, 7.0, 2.0, 8.0);
    plot.reset_zoom_to_data();

    // silx `_forceResetZoom` with no data: (1, 100) on both axes; no right
    // axis is conjured on a y2-less plot.
    assert_eq!(plot.plot().limits, (1.0, 100.0, 1.0, 100.0));
    assert_eq!(plot.plot().y2, None);
}
