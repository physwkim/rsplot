//! FitAction plot flow (R2-8): the fit target seeds its range from the
//! plot's visible X window (silx `FitAction` triggered:
//! `self._setXRange(*plot.getXAxis().getLimits())`, actions/fit.py:249) and
//! the fit result overlays the SOURCE plot as a `Fit <legend>` curve, hidden
//! while no result exists (`handle_signal`, actions/fit.py:429-451).
//!
//! Needs a GPU (real or software); mirrors `tests/limits_history_lifecycle.rs`.

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use siplot::egui::Color32;
use siplot::{FitWidget, Plot1D};

/// A plot showing a straight line over x = 0..100, zoomed to [20, 40].
fn zoomed_line_plot(rs: &egui_wgpu::RenderState) -> (Plot1D, siplot::ItemHandle) {
    let mut plot = Plot1D::new(rs, 0);
    let x: Vec<f64> = (0..=100).map(f64::from).collect();
    let y: Vec<f64> = x.iter().map(|&x| 2.0 * x + 1.0).collect();
    let handle = plot.add_curve_with_legend(&x, &y, Color32::BLUE, "line");
    plot.set_graph_x_limits(20.0, 40.0);
    (plot, handle)
}

#[test]
fn fit_target_seeds_range_from_visible_x_window() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);
    let (plot, handle) = zoomed_line_plot(&rs);
    let mut fit = FitWidget::new(&rs, 1);

    assert!(plot.set_fit_target(&mut fit, handle));
    let (lo, hi) = fit.fit_range().expect("range seeded from the view window");
    assert_eq!((lo, hi), plot.x_limits(), "range = current X limits");

    // The fit then runs on the visible window only (silx fitmanager fits the
    // xmin/xmax-restricted data): default Linear model, in-range xs only.
    fit.perform_fit_choice();
    let (fx, fy) = fit.fit_curve().expect("linear fit succeeds");
    assert!(!fx.is_empty());
    assert!(fx.iter().all(|&v| (lo..=hi).contains(&v)), "xs ⊂ window");
    // Perfect line: the fitted model reproduces y = 2x + 1 on the window.
    for (&x, &y) in fx.iter().zip(fy) {
        assert!((y - (2.0 * x + 1.0)).abs() < 1e-9);
    }
}

#[test]
fn fit_overlay_appears_updates_in_place_and_hides_without_result() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);
    let (mut plot, handle) = zoomed_line_plot(&rs);
    let mut fit = FitWidget::new(&rs, 1);

    // No fit yet → no overlay is created (silx adds the curve only on
    // FitFinished).
    assert!(plot.set_fit_target(&mut fit, handle));
    assert_eq!(plot.sync_fit_overlay(&fit, handle), None);

    // A successful fit adds `Fit <legend>` on the SOURCE plot, visible.
    fit.perform_fit_choice();
    let overlay = plot
        .sync_fit_overlay(&fit, handle)
        .expect("overlay after FitFinished");
    assert_eq!(plot.item_legend(overlay), Some("Fit <line>"));
    assert!(plot.is_item_visible(overlay));
    assert_ne!(overlay, handle);

    // A re-fit updates the same overlay handle in place (silx
    // `fit_curve.setData`), no duplicate curve.
    fit.perform_fit_choice();
    assert_eq!(plot.sync_fit_overlay(&fit, handle), Some(overlay));

    // New data clears the fit result → the overlay is HIDDEN, not removed
    // (silx FitStarted/FitFailed → setVisible(False)).
    assert!(plot.set_fit_target(&mut fit, handle));
    assert_eq!(plot.sync_fit_overlay(&fit, handle), Some(overlay));
    assert!(!plot.is_item_visible(overlay), "hidden while no result");

    // And a following successful fit re-shows the same curve.
    fit.perform_fit_choice();
    assert_eq!(plot.sync_fit_overlay(&fit, handle), Some(overlay));
    assert!(plot.is_item_visible(overlay));
}
