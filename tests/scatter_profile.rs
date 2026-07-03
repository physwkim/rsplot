//! `ScatterView` line-profile side-plot (silx `ScatterProfileToolBar`): sampling
//! a profile across the scatter and pushing it into the profile side window.
//!
//! The profile *display* lives in its own egui viewport (a separate OS window),
//! so its pixels are not headlessly render-verifiable here; this exercises the
//! data path — `show_line_profile` samples the retained scatter, converts the
//! interpolated profile to a value-vs-distance curve, and opens the side window
//! only when a profile was actually produced. Building a `ScatterView` needs a
//! wgpu render state (real or software).

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use siplot::egui;
use siplot::{Colormap, ScatterView, YAxis};

/// A triangle of scattered points carrying the affine field `v = x + 2y`, plus a
/// 4th point so the convex hull is a quad. Linear interpolation reproduces the
/// field exactly inside the hull.
fn affine_scatter() -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let x = vec![0.0, 4.0, 0.0, 4.0];
    let y = vec![0.0, 0.0, 4.0, 4.0];
    let values = x.iter().zip(&y).map(|(x, y)| x + 2.0 * y).collect();
    (x, y, values)
}

#[test]
fn show_line_profile_opens_window_for_an_in_hull_segment() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);

    let mut view = ScatterView::new(&rs, 0);
    let (x, y, values) = affine_scatter();
    view.set_data(&x, &y, &values, Colormap::viridis(0.0, 12.0))
        .expect("equal-length scatter data");

    // The window starts closed.
    assert!(
        !view.profile_window().is_open(),
        "profile window is closed until a profile is shown"
    );

    // A segment crossing the hull yields a profile → the side window opens.
    let shown = view.show_line_profile((0.5, 0.5), (3.5, 3.5), 9);
    assert!(shown, "an in-hull segment must produce a profile");
    assert!(
        view.profile_window().is_open(),
        "showing a profile must open the side window"
    );
}

#[test]
fn show_line_profile_is_a_noop_for_an_out_of_hull_segment() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);

    let mut view = ScatterView::new(&rs, 0);
    let (x, y, values) = affine_scatter();
    view.set_data(&x, &y, &values, Colormap::viridis(0.0, 12.0))
        .expect("equal-length scatter data");

    // A segment entirely outside the convex hull interpolates to all-None, so no
    // profile is produced and the window stays closed.
    let shown = view.show_line_profile((10.0, 10.0), (20.0, 20.0), 9);
    assert!(
        !shown,
        "a fully out-of-hull segment must produce no profile"
    );
    assert!(
        !view.profile_window().is_open(),
        "an empty profile must leave the side window closed"
    );
}

#[test]
fn show_line_profile_without_data_is_a_noop() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);

    let mut view = ScatterView::new(&rs, 0);
    let shown = view.show_line_profile((0.0, 0.0), (1.0, 1.0), 5);
    assert!(!shown, "no data → no profile");
    assert!(!view.profile_window().is_open());
}

#[test]
fn profile_mode_drag_samples_a_profile_and_opens_the_window() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);

    let mut view = ScatterView::new(&rs, 0);
    let (x, y, values) = affine_scatter();
    view.set_data(&x, &y, &values, Colormap::viridis(0.0, 12.0))
        .expect("equal-length scatter data");
    // Arm the interactive line-profile tool (silx `ScatterProfileToolBar`).
    view.set_profile_mode(true);
    assert!(view.profile_mode());

    let app = Rc::new(RefCell::new(view));
    let app_ui = app.clone();
    let renderer = WgpuTestRenderer::from_render_state(rs.clone());
    let mut harness = Harness::builder()
        .with_size(egui::vec2(420.0, 360.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            app_ui.borrow_mut().show(ui);
        });

    // Two frames so the plot caches its data area / transform.
    harness.step();
    harness.step();
    assert!(
        !app.borrow().profile_window().is_open(),
        "window stays closed until a drag samples a profile"
    );

    // Pixel positions of two hull-interior data points (the quad hull is
    // [0,4]×[0,4]); the segment (1,1)→(3,3) is fully inside.
    let p0 = app
        .borrow()
        .data_to_pixel(1.0, 1.0, YAxis::Left)
        .expect("data area cached after a frame");
    let p1 = app
        .borrow()
        .data_to_pixel(3.0, 3.0, YAxis::Left)
        .expect("data area cached after a frame");

    // Simulate a continuous drag: press near p0, move incrementally to p1, then
    // release (egui reports `drag_started` only on the first move clearing its
    // click-vs-drag threshold, so the start stays near p0).
    harness.drag_at(p0);
    harness.step();
    for t in [0.2f32, 0.5, 0.8, 1.0] {
        harness.hover_at(p0 + (p1 - p0) * t);
        harness.step();
    }
    harness.drop_at(p1);
    harness.step();
    harness.step();

    assert!(
        app.borrow().profile_window().is_open(),
        "dragging the armed profile tool across the hull must sample a profile and open the window"
    );
}

#[test]
fn scatter_view_stats_run_over_the_value_array() {
    // silx _ScatterContext (stats.py:425-498): the stats table reduces the
    // scatter's *value* array with (x, y) as position axes — not the marker
    // curve's y positions. Exercises the ScatterView Points-arm retained-data
    // wiring end-to-end through PlotWidget::feed_all_stats.
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);

    let mut view = ScatterView::new(&rs, 0);
    let (x, y, values) = affine_scatter(); // v = x + 2y on a 4x4 quad
    view.set_data(&x, &y, &values, Colormap::viridis(0.0, 12.0))
        .expect("equal-length scatter data");

    let mut stats = siplot::StatsWidget::new();
    stats.set_update_mode(siplot::UpdateMode::Auto);
    let fed = view.feed_all_stats(&mut stats, None);
    assert_eq!(fed, 1, "the scatter item feeds one stats row");
    let s = &stats.rows()[0].1;
    // Values are [0, 4, 8, 12]: max 12 at (4, 4), min 0 at (0, 0) — value
    // extrema at their (x, y), not y extrema.
    assert_eq!(s.max, Some(12.0));
    assert_eq!(s.coord_max.x, Some(4.0));
    assert_eq!(s.coord_max.y, Some(4.0));
    assert_eq!(s.min, Some(0.0));
    assert_eq!(s.sum, Some(24.0));
}

#[test]
fn disarming_profile_mode_closes_the_window() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);

    let mut view = ScatterView::new(&rs, 0);
    let (x, y, values) = affine_scatter();
    view.set_data(&x, &y, &values, Colormap::viridis(0.0, 12.0))
        .expect("equal-length scatter data");

    view.set_profile_mode(true);
    assert!(view.show_line_profile((0.5, 0.5), (3.5, 3.5), 9));
    assert!(view.profile_window().is_open());

    // Disarming the tool clears the profile (silx deselect behavior).
    view.set_profile_mode(false);
    assert!(!view.profile_mode());
    assert!(
        !view.profile_window().is_open(),
        "disarming the profile tool closes the side window"
    );
}
