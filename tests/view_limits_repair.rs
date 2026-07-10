//! Every view-limits commit passes through one owner (`set_limits_internal`)
//! that runs the silx `checkAxisLimits` repair, so no public entry point can
//! install an inverted, degenerate, or float32-unsafe range.
//!
//! silx enforces this in `PlotWidget.setLimits` (`PlotWidget.py:2723-2730`),
//! which runs `Axis._checkLimits` on X, Y and — when present — Y2
//! (`items/axis.py:145-154` → `_utils/panzoom.py:49-75`).
//!
//! Cases are one per invariant boundary, not one per user story.
//!
//! Needs a GPU (real or software); mirrors `tests/limits_history_lifecycle.rs`.

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use rsplot::{AxisSide, Plot1D, YAxis};

const F32_SAFE_MAX: f64 = 1e37;
const F32_SAFE_MIN: f64 = -1e37;

fn widget() -> Plot1D {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);
    Plot1D::new(&rs, 0)
}

/// Boundary: `vmax < vmin` on both axes — silx swaps rather than committing an
/// inverted range (`panzoom.py:57-59`).
#[test]
fn inverted_limits_are_swapped() {
    let mut plot = widget();
    plot.set_limits(10.0, 0.0, 20.0, 5.0, None);
    assert_eq!(plot.x_limits(), (0.0, 10.0));
    assert_eq!(plot.y_limits(YAxis::Left), Some((5.0, 20.0)));
}

/// Boundary: `vmax == vmin == 0` — silx expands to `(-0.1, 0.1)`
/// (`panzoom.py:62-63`). Entry point: the X-only setter.
#[test]
fn degenerate_zero_range_expands_to_pm_point_one() {
    let mut plot = widget();
    plot.set_graph_x_limits(0.0, 0.0);
    assert_eq!(plot.x_limits(), (-0.1, 0.1));
}

/// Boundary: `vmax == vmin > 0` — silx expands to `(0.9v, 1.1v)`
/// (`panzoom.py:67-69`). Entry point: the Y-left setter.
#[test]
fn degenerate_positive_range_expands_by_ten_percent() {
    let mut plot = widget();
    plot.set_graph_y_limits(5.0, 5.0, YAxis::Left);
    let (lo, hi) = plot.y_limits(YAxis::Left).expect("left axis has limits");
    assert!((lo - 4.5).abs() < 1e-12, "lo {lo}");
    assert!((hi - 5.5).abs() < 1e-12, "hi {hi}");
}

/// Boundary: bounds outside the float32-safe window are clipped to it
/// (`panzoom.py:54-55`), on both axes, through the all-axis setter.
#[test]
fn out_of_float32_range_limits_are_clipped() {
    let mut plot = widget();
    plot.set_limits(-1e40, 1e40, -1e40, 1e40, None);
    assert_eq!(plot.x_limits(), (F32_SAFE_MIN, F32_SAFE_MAX));
    assert_eq!(
        plot.y_limits(YAxis::Left),
        Some((F32_SAFE_MIN, F32_SAFE_MAX))
    );
}

/// Regression: the right axis used to be written raw, bypassing the owner.
/// silx runs `_checkLimits` on y2 too (`PlotWidget.py:2729-2730`).
#[test]
fn y2_limits_are_repaired_not_written_raw() {
    let mut plot = widget();
    plot.set_graph_y_limits(5.0, 5.0, YAxis::Right);
    let (lo, hi) = plot.y_limits(YAxis::Right).expect("right axis has limits");
    assert!((lo - 4.5).abs() < 1e-12, "lo {lo}");
    assert!((hi - 5.5).abs() < 1e-12, "hi {hi}");

    plot.set_graph_y_limits(20.0, 5.0, YAxis::Right);
    assert_eq!(
        plot.y_limits(YAxis::Right),
        Some((5.0, 20.0)),
        "inverted y2"
    );
}

/// Regression: an extra (stacked) axis used to be written raw. It repairs
/// against its own scale, which is what makes it a separate clamp call.
#[test]
fn extra_axis_limits_are_repaired_not_written_raw() {
    let mut plot = widget();
    let ax = plot.add_extra_y_axis(AxisSide::Left);
    plot.set_graph_y_limits(5.0, 5.0, YAxis::Extra(ax));
    let (lo, hi) = plot.y_limits(YAxis::Extra(ax)).expect("extra axis range");
    assert!((lo - 4.5).abs() < 1e-12, "lo {lo}");
    assert!((hi - 5.5).abs() < 1e-12, "hi {hi}");

    plot.set_graph_y_limits(20.0, 5.0, YAxis::Extra(ax));
    assert_eq!(plot.y_limits(YAxis::Extra(ax)), Some((5.0, 20.0)));
}

/// Boundary: the toolbar zoom path commits through the same owner, so
/// unbounded Zoom-Out saturates at the float32-safe window instead of growing
/// without limit. silx clamps this path twice (`scale1DRange` plus
/// `setLimits`); one owner is enough to hold the invariant.
#[test]
fn repeated_toolbar_zoom_out_stays_in_the_float32_window() {
    let mut plot = widget();
    plot.set_limits(0.0, 1.0, 0.0, 1.0, None);
    for _ in 0..1000 {
        rsplot::actions::control::zoom_out(&mut plot);
    }
    let (x0, x1) = plot.x_limits();
    let (y0, y1) = plot.y_limits(YAxis::Left).expect("left axis has limits");
    for v in [x0, x1, y0, y1] {
        assert!(v.is_finite(), "limit escaped to {v}");
        assert!(
            (F32_SAFE_MIN..=F32_SAFE_MAX).contains(&v),
            "limit {v} outside"
        );
    }
    assert!(x0 < x1, "x collapsed: {x0} {x1}");
    assert!(y0 < y1, "y collapsed: {y0} {y1}");
}
