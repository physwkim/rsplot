//! Widget-path coverage for the axis-state toggles' immediate refits:
//!
//! - silx `Axis._internalSetScale` (`items/axis.py:398-421` X, `:463-484` Y):
//!   switching an axis to log with a current lower limit `<= 0` snaps the
//!   limits to the strictly positive data range ((1, 100) when none) at
//!   toggle time, instead of leaving a log axis with `min <= 0`;
//! - silx `setKeepDataAspectRatio` (`PlotWidget.py:2958-2969`): a *changed*
//!   flag forces a reset zoom; re-applying the same value is a no-op.
//!
//! Needs a GPU (real or software); mirrors `tests/roi_events.rs`' harness.

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use siplot::Plot1D;
use siplot::egui::Color32;

#[test]
fn y_log_toggle_snaps_to_positive_data_range() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);
    let mut plot = Plot1D::new(&rs, 0);

    // ys spans [-5, 5]; the strictly positive ys are {1..5}.
    let xs: Vec<f64> = (0..=10).map(|i| i as f64).collect();
    let ys: Vec<f64> = xs.iter().map(|x| x - 5.0).collect();
    let _ = plot.add_curve(&xs, &ys, Color32::RED);
    let (_, _, y0, y1) = plot.plot().limits;
    assert_eq!((y0, y1), (-5.0, 5.0), "auto refit before the toggle");

    plot.set_y_log(true);
    // vmin <= 0 with vmax (5) > 0 inside positive data: silx keeps the
    // current vmax and adopts the positive data min
    // (setLimits(dataRange[0], vmax), items/axis.py:474-480).
    let (_, _, y0, y1) = plot.plot().limits;
    assert_eq!((y0, y1), (1.0, 5.0), "positive-data refit at toggle time");
}

#[test]
fn x_log_toggle_with_no_positive_data_lands_on_1_100() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);
    let mut plot = Plot1D::new(&rs, 0);

    // All xs non-positive: no positive X data exists.
    let xs: Vec<f64> = (0..=10).map(|i| -(i as f64)).collect();
    let ys: Vec<f64> = (0..=10).map(|i| i as f64 + 1.0).collect();
    let _ = plot.add_curve(&xs, &ys, Color32::RED);

    plot.set_x_log(true);
    // silx: dataRange is None under the log filter -> setLimits(1, 100).
    let (x0, x1, _, _) = plot.plot().limits;
    assert_eq!((x0, x1), (1.0, 100.0));
}

#[test]
fn keep_aspect_toggle_forces_reset_zoom_once() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);
    let mut plot = Plot1D::new(&rs, 0);

    let xs: Vec<f64> = (0..=10).map(|i| i as f64).collect();
    let ys: Vec<f64> = xs.clone();
    let _ = plot.add_curve(&xs, &ys, Color32::RED);
    let home = plot.plot().limits;

    // Zoom somewhere else, then toggle: the changed flag refits to data.
    plot.set_graph_x_limits(2.0, 3.0);
    assert_ne!(plot.plot().limits, home);
    plot.set_keep_data_aspect_ratio(true);
    assert_eq!(plot.plot().limits, home, "changed flag forces a reset zoom");

    // Re-applying the same value is silx's early return: no refit.
    plot.set_graph_x_limits(2.0, 3.0);
    let zoomed = plot.plot().limits;
    plot.set_keep_data_aspect_ratio(true);
    assert_eq!(plot.plot().limits, zoomed, "unchanged flag is a no-op");
}
