//! `PositionInfo` live cursor snapping (silx `PositionInfo._updateStatusBar`
//! snap, PositionInfo.py:196-292), wired into the base `PlotWidget` via
//! `snap_cursor`.
//!
//! The cores (`snapping_candidates` → `snap_to_nearest`) are unit-tested in
//! `position_info.rs`; this exercises the live wiring on a rendered widget:
//! building `SnapItem`s from the retained curve records, projecting each
//! vertex through the cached display transform, and returning the nearest
//! vertex within `SNAP_THRESHOLD_DIST` logical pixels — or `None` when the
//! cursor is too far, the mode is disabled, or no curve participates.

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use siplot::egui;
use siplot::{PlotWidget, SnappingMode};

/// A `PlotWidget` populated by `build`, rendered twice through the kittest+wgpu
/// harness so the display transform is cached (snapping projects data→pixel
/// through it). Returns the shared widget and the live harness.
fn plot_rendered(
    build: impl FnOnce(&mut PlotWidget),
) -> (Rc<RefCell<PlotWidget>>, Harness<'static>) {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);
    let mut plot = PlotWidget::new(&rs, 0);
    build(&mut plot);

    let plot = Rc::new(RefCell::new(plot));
    let plot_ui = plot.clone();
    let renderer = WgpuTestRenderer::from_render_state(rs.clone());
    let mut harness = Harness::builder()
        .with_size(egui::vec2(400.0, 400.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            plot_ui.borrow_mut().show(ui);
        });
    harness.step();
    harness.step();
    (plot, harness)
}

/// A `PlotWidget` carrying a `y = x` curve over the integers `0..=10`.
fn plot_with_line() -> (Rc<RefCell<PlotWidget>>, Harness<'static>) {
    plot_rendered(|plot| {
        let xs: Vec<f64> = (0..=10).map(|i| i as f64).collect();
        let ys = xs.clone();
        plot.add_curve(&xs, &ys, egui::Color32::from_rgb(0, 120, 255));
    })
}

#[test]
fn snap_lands_on_the_nearest_curve_vertex() {
    let (plot, _harness) = plot_with_line();
    let plot = plot.borrow();

    // A cursor sitting essentially on the (5, 5) vertex snaps to it.
    let snap = plot
        .snap_cursor([5.0, 5.0], SnappingMode::CURVE)
        .expect("a cursor on the (5,5) vertex must snap to it");
    assert!(
        (snap.data[0] - 5.0).abs() < 1e-9 && (snap.data[1] - 5.0).abs() < 1e-9,
        "snapped to the wrong vertex: {:?}",
        snap.data
    );
}

#[test]
fn snap_returns_none_when_no_vertex_is_within_the_threshold() {
    let (plot, _harness) = plot_with_line();
    let plot = plot.borrow();

    // (5.5, 4.5) is off the line and far from any integer vertex: with a 400px
    // window over a 0..10 range (~36 px/unit), the nearest vertex (5,5) or
    // (4,4) is well beyond the 5-logical-pixel snap radius.
    assert!(
        plot.snap_cursor([5.5, 4.5], SnappingMode::CURVE).is_none(),
        "a cursor far from every vertex must not snap"
    );
}

#[test]
fn disabled_mode_never_snaps() {
    let (plot, _harness) = plot_with_line();
    let plot = plot.borrow();

    // Even directly on a vertex, DISABLED yields no candidates → no snap.
    assert!(
        plot.snap_cursor([5.0, 5.0], SnappingMode::DISABLED)
            .is_none(),
        "SnappingMode::DISABLED must never snap"
    );
    // SCATTER-only mode finds no scatter (the only item is a curve) → no snap.
    assert!(
        plot.snap_cursor([5.0, 5.0], SnappingMode::SCATTER)
            .is_none(),
        "a scatter-only mode must not snap a curve vertex"
    );
}

#[test]
fn scatter_points_snap_under_scatter_mode_only() {
    // A base-widget scatter is a symbol-only curve-kind item that retains its
    // points, so SCATTER mode snaps to a scatter point — and CURVE mode does
    // not (kind filtering), proving the mode→kind mapping is honored.
    let (plot, _harness) = plot_rendered(|plot| {
        let xs: Vec<f64> = (0..=10).map(|i| i as f64).collect();
        let ys = xs.clone();
        plot.add_scatter(&xs, &ys, egui::Color32::from_rgb(255, 120, 0));
    });
    let plot = plot.borrow();

    let snap = plot
        .snap_cursor([5.0, 5.0], SnappingMode::SCATTER)
        .expect("SCATTER mode must snap to a scatter point");
    assert!(
        (snap.data[0] - 5.0).abs() < 1e-9 && (snap.data[1] - 5.0).abs() < 1e-9,
        "snapped to the wrong scatter point: {:?}",
        snap.data
    );
    assert!(
        plot.snap_cursor([5.0, 5.0], SnappingMode::CURVE).is_none(),
        "CURVE mode must not snap a scatter point (kind filtering)"
    );
}

#[test]
fn uncached_transform_yields_no_snap() {
    // A widget that has never rendered has no cached transform, so data→pixel
    // projection fails and snapping returns None rather than panicking.
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);
    let mut plot = PlotWidget::new(&rs, 0);
    let xs: Vec<f64> = (0..=10).map(|i| i as f64).collect();
    let ys = xs.clone();
    plot.add_curve(&xs, &ys, egui::Color32::from_rgb(0, 120, 255));
    assert!(
        plot.snap_cursor([5.0, 5.0], SnappingMode::CURVE).is_none(),
        "snapping before any frame is rendered must return None"
    );
}
