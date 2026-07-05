//! `PositionInfo` live cursor snapping (silx `PositionInfo._updateStatusBar`
//! snap, PositionInfo.py:196-292), wired into the base `PlotWidget` via
//! `snap_cursor`.
//!
//! The cores (`snapping_candidates`, `pick_polyline_indices`,
//! `pick_filled_histogram`) are unit-tested in `position_info.rs`; this
//! exercises the live wiring on a rendered widget: building `SnapItem`s from
//! the retained curve records, pick-gating each item (the silx `item.pick()`
//! engage contract), and returning the nearest picked vertex within
//! `SNAP_THRESHOLD_DIST × pixels_per_point` logical pixels — with a picked
//! filled histogram taking unconditional priority (silx `break`,
//! PositionInfo.py:246-258) — or `None` when nothing picks, the mode is
//! disabled, or no curve participates.

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui;
use rsplot::{PlotWidget, SnappingMode};

/// A `PlotWidget` populated by `build`, rendered twice through the kittest+wgpu
/// harness so the display transform is cached (snapping projects data→pixel
/// through it). Returns the shared widget and the live harness.
fn plot_rendered(
    build: impl FnOnce(&mut PlotWidget),
) -> (Rc<RefCell<PlotWidget>>, Harness<'static>) {
    plot_rendered_at(1.0, build)
}

/// Like [`plot_rendered`] with an explicit `pixels_per_point` — the egui
/// analog of the device-pixel ratio silx multiplies into the snap radius
/// (PositionInfo.py:229-237).
fn plot_rendered_at(
    pixels_per_point: f32,
    build: impl FnOnce(&mut PlotWidget),
) -> (Rc<RefCell<PlotWidget>>, Harness<'static>) {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);
    let mut plot = PlotWidget::new(&rs, 0);
    build(&mut plot);

    let plot = Rc::new(RefCell::new(plot));
    let plot_ui = plot.clone();
    let renderer = WgpuTestRenderer::from_render_state(rs.clone());
    let mut harness = Harness::builder()
        .with_size(egui::vec2(400.0, 400.0))
        .with_pixels_per_point(pixels_per_point)
        .renderer(renderer)
        .build_ui(move |ui| {
            plot_ui.borrow_mut().show(ui);
        });
    harness.step();
    harness.step();
    (plot, harness)
}

/// The data-space point sitting `offset_px` logical pixels away from the data
/// point `at`, displaced along `dir_px` (a pixel-space direction, y down).
fn cursor_at_pixel_offset(
    plot: &PlotWidget,
    at: [f64; 2],
    dir_px: [f32; 2],
    offset_px: f32,
) -> [f64; 2] {
    let v = plot
        .data_to_pixel(at[0], at[1], rsplot::YAxis::Left)
        .expect("cached transform");
    let norm = (dir_px[0] * dir_px[0] + dir_px[1] * dir_px[1]).sqrt();
    let p = egui::pos2(
        v.x + dir_px[0] / norm * offset_px,
        v.y + dir_px[1] / norm * offset_px,
    );
    let (x, y) = plot
        .pixel_to_data(p, rsplot::YAxis::Left)
        .expect("cached transform");
    [x, y]
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
fn histogram_snaps_to_bin_centre_and_count() {
    // silx snaps a histogram pick to the bin centre `0.5 * (edges[i] +
    // edges[i+1])` and the bin's count value (PositionInfo.py:246-258) — not
    // to a rendered step-polyline vertex.
    let (plot, _harness) = plot_rendered(|plot| {
        plot.add_histogram(
            &[0.0, 2.0, 4.0, 6.0, 8.0, 10.0],
            &[1.0, 3.0, 5.0, 7.0, 9.0],
            egui::Color32::from_rgb(0, 180, 90),
        )
        .expect("N + 1 edges for N counts");
    });
    let plot = plot.borrow();

    // The bin [4, 6) has centre x = 5 and count 5: a cursor there is inside
    // the filled bar (0 ≤ 5 ≤ value 5), so the area pick engages and snaps to
    // the bin centre + count (5, 5) — not to a rendered step-polyline vertex.
    let snap = plot
        .snap_cursor([5.0, 5.0], SnappingMode::CURVE)
        .expect("a cursor on a bin centre must snap to it");
    assert!(
        (snap.data[0] - 5.0).abs() < 1e-9 && (snap.data[1] - 5.0).abs() < 1e-9,
        "snapped to the wrong point: {:?}",
        snap.data
    );
}

#[test]
fn filled_bar_interior_snaps_far_from_the_apex() {
    // THE divergence R2-9 cites: hovering the middle of a tall filled bar
    // snaps in silx (area pick, items/histogram.py:245-291) even though the
    // bin apex is hundreds of pixels away.
    let (plot, _harness) = plot_rendered(|plot| {
        plot.add_histogram(
            &[0.0, 2.0, 4.0, 6.0, 8.0, 10.0],
            &[2.0, 6.0, 4.0, 8.0, 9.0],
            egui::Color32::from_rgb(0, 180, 90),
        )
        .expect("N + 1 edges for N counts");
    });
    let plot = plot.borrow();

    // (9, 2) sits mid-bar in [8, 10) (count 9): ~250 px below the apex (9, 9).
    let snap = plot
        .snap_cursor([9.0, 2.0], SnappingMode::CURVE)
        .expect("a cursor inside a filled bar must area-pick it");
    assert!(
        (snap.data[0] - 9.0).abs() < 1e-9 && (snap.data[1] - 9.0).abs() < 1e-9,
        "must snap to the bin centre + count: {:?}",
        snap.data
    );
    assert_eq!(snap.index, 4, "picked the wrong bin");
}

#[test]
fn picked_histogram_outranks_a_nearer_curve_vertex() {
    // silx `break`s on a histogram pick (PositionInfo.py:246-258): the bin
    // centre + count win even over a curve vertex at distance zero, and even
    // when the curve comes first in item order (the assignment overwrites the
    // curve's earlier snap).
    let (plot, _harness) = plot_rendered(|plot| {
        let xs: Vec<f64> = (0..=10).map(|i| i as f64).collect();
        let ys = xs.clone();
        plot.add_curve(&xs, &ys, egui::Color32::from_rgb(0, 120, 255));
        plot.add_histogram(
            &[0.0, 2.0, 4.0, 6.0, 8.0, 10.0],
            &[2.0, 6.0, 4.0, 8.0, 9.0],
            egui::Color32::from_rgb(0, 180, 90),
        )
        .expect("N + 1 edges for N counts");
    });
    let plot = plot.borrow();

    // (3, 3) is exactly a curve vertex AND inside the bar [2, 4) (count 6).
    let snap = plot
        .snap_cursor([3.0, 3.0], SnappingMode::CURVE)
        .expect("the filled bar must pick");
    assert!(
        (snap.data[0] - 3.0).abs() < 1e-9 && (snap.data[1] - 6.0).abs() < 1e-9,
        "the picked histogram must outrank the curve vertex: {:?}",
        snap.data
    );
}

#[test]
fn vertex_within_radius_but_outside_the_pick_box_does_not_snap() {
    // silx engages a curve through its ±3 px pick box (BackendOpenGL
    // _PICK_OFFSET); a vertex within the 5 px snap radius whose polyline
    // stays outside the box never becomes a candidate. The pre-fix code
    // snapped here (global-nearest apex within 5 px, no pick gate).
    let (plot, _harness) = plot_with_line();
    let plot = plot.borrow();

    // Pixel-space direction of the line at (5, 5), and its perpendicular.
    let p5 = plot.data_to_pixel(5.0, 5.0, rsplot::YAxis::Left).unwrap();
    let p6 = plot.data_to_pixel(6.0, 6.0, rsplot::YAxis::Left).unwrap();
    let dir = [p6.x - p5.x, p6.y - p5.y];
    let perp = [-dir[1], dir[0]];

    // 4.6 px perpendicular from the vertex: within the 5 px snap radius, but
    // the ±3 px axis-aligned box reaches at most 3·(|cos θ| + |sin θ|) ≤ 4.25
    // px toward the line — no pick, no snap.
    let far = cursor_at_pixel_offset(&plot, [5.0, 5.0], perp, 4.6);
    assert!(
        plot.snap_cursor(far, SnappingMode::CURVE).is_none(),
        "an unpicked vertex must not snap even inside the snap radius"
    );

    // 2 px perpendicular: the vertex sits inside the pick box → snaps.
    let near = cursor_at_pixel_offset(&plot, [5.0, 5.0], perp, 2.0);
    let snap = plot
        .snap_cursor(near, SnappingMode::CURVE)
        .expect("a picked vertex within the radius must snap");
    assert!(
        (snap.data[0] - 5.0).abs() < 1e-9 && (snap.data[1] - 5.0).abs() < 1e-9,
        "snapped to the wrong vertex: {:?}",
        snap.data
    );
}

#[test]
fn snap_radius_scales_with_pixels_per_point() {
    // silx: sqDistInPixels = (SNAP_THRESHOLD_DIST * devicePixelRatio)²
    // (PositionInfo.py:229-237). A vertex ~7 logical px away along the line
    // (so the polyline picks) snaps at pixels_per_point 2 (radius 10) but not
    // at 1 (radius 5).
    let build = |plot: &mut PlotWidget| {
        let xs: Vec<f64> = (0..=10).map(|i| i as f64).collect();
        let ys = xs.clone();
        plot.add_curve(&xs, &ys, egui::Color32::from_rgb(0, 120, 255));
    };

    let (plot, _harness) = plot_rendered_at(2.0, build);
    let plot = plot.borrow();
    let p5 = plot.data_to_pixel(5.0, 5.0, rsplot::YAxis::Left).unwrap();
    let p6 = plot.data_to_pixel(6.0, 6.0, rsplot::YAxis::Left).unwrap();
    let dir = [p6.x - p5.x, p6.y - p5.y];
    let cursor = cursor_at_pixel_offset(&plot, [5.0, 5.0], dir, 7.0);
    let snap = plot
        .snap_cursor(cursor, SnappingMode::CURVE)
        .expect("7 px is inside the DPR-2 radius of 10 px");
    assert!(
        (snap.data[0] - 5.0).abs() < 1e-9 && (snap.data[1] - 5.0).abs() < 1e-9,
        "snapped to the wrong vertex: {:?}",
        snap.data
    );
    drop(plot);

    let (plot, _harness) = plot_rendered_at(1.0, build);
    let plot = plot.borrow();
    let p5 = plot.data_to_pixel(5.0, 5.0, rsplot::YAxis::Left).unwrap();
    let p6 = plot.data_to_pixel(6.0, 6.0, rsplot::YAxis::Left).unwrap();
    let dir = [p6.x - p5.x, p6.y - p5.y];
    let cursor = cursor_at_pixel_offset(&plot, [5.0, 5.0], dir, 7.0);
    assert!(
        plot.snap_cursor(cursor, SnappingMode::CURVE).is_none(),
        "7 px is outside the DPR-1 radius of 5 px"
    );
}

#[test]
fn uncached_transform_yields_no_snap() {
    // A widget that has never rendered has no cached transform, so data→pixel
    // projection fails and snapping returns None rather than panicking.
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);
    let mut plot = PlotWidget::new(&rs, 0);
    let xs: Vec<f64> = (0..=10).map(|i| i as f64).collect();
    let ys = xs.clone();
    plot.add_curve(&xs, &ys, egui::Color32::from_rgb(0, 120, 255));
    assert!(
        plot.snap_cursor([5.0, 5.0], SnappingMode::CURVE).is_none(),
        "snapping before any frame is rendered must return None"
    );
}
