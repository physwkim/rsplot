//! `ScatterView` line-profile side-plot (silx `ScatterProfileToolBar`): sampling
//! a profile across the scatter and pushing it into the profile side window.
//!
//! The profile *display* lives in its own egui viewport (a separate OS window),
//! so its pixels are not headlessly render-verifiable here; this exercises the
//! data path — `show_line_profile` samples the retained scatter, converts the
//! interpolated profile to a value-vs-distance curve, and opens the side window
//! only when a profile was actually produced. Building a `ScatterView` needs a
//! wgpu render state (real or software).

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use siplot::{Colormap, ScatterView};

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
