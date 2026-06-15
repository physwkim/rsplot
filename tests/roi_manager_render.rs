//! Interaction check for the enriched ROI manager panel
//! (`PlotWidget::show_roi_manager`), the siplot port of silx
//! `RegionOfInterestTableWidget`. The panel renders one row per ROI — an
//! editable name, the geometry shown as a make-current selector, and a remove
//! button — plus the add / clear-all controls.
//!
//! This drives the panel through a headless egui_kittest harness and asserts,
//! via accesskit queries and a real click, that:
//!   * the table renders a row for every ROI (the per-ROI geometry labels and
//!     the column headers are present), and
//!   * clicking a row's geometry selector makes that ROI the current one
//!     (silx row selection → `sigCurrentRoiChanged`), routed through the
//!     `set_current_roi` owner API.
//!
//! Mirrors `tests/scene_window_render.rs`' accesskit interaction harness. Needs
//! a GPU (real or software) for the render backend.

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use siplot::egui;
use siplot::{PlotWidget, Roi};

const W: f32 = 360.0;
const H: f32 = 300.0;

/// The exact text the manager shows for a `Roi::Rect`, mirroring the private
/// `roi_description` Rect arm in `high_level.rs`. Used to query/click a specific
/// row's geometry selector by its accesskit label.
fn rect_desc(x: (f64, f64), y: (f64, f64)) -> String {
    format!(
        "Rect  x=[{:.3}, {:.3}]  y=[{:.3}, {:.3}]",
        x.0, x.1, y.0, y.1
    )
}

#[test]
fn roi_manager_table_renders_rows_and_click_makes_current() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);

    // Two ROIs at distinct positions → distinct geometry labels, so each row's
    // make-current selector is uniquely addressable by accesskit label.
    let alpha = ((1.0, 2.0), (1.0, 2.0));
    let beta = ((5.0, 6.0), (5.0, 6.0));

    let mut plot = PlotWidget::new(&rs, 0);
    let i_alpha = plot.add_roi(Roi::Rect {
        x: alpha.0,
        y: alpha.1,
    });
    plot.set_roi_name(i_alpha, "alpha");
    let i_beta = plot.add_roi(Roi::Rect {
        x: beta.0,
        y: beta.1,
    });
    plot.set_roi_name(i_beta, "beta");

    // No ROI is current until the user selects one.
    assert_eq!(plot.current_roi(), None, "no ROI current before selection");

    let app = Rc::new(RefCell::new(plot));
    let app_ui = app.clone();
    let renderer = WgpuTestRenderer::from_render_state(rs);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(W, H))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            app_ui.borrow_mut().show_roi_manager(ui);
        });

    harness.step();

    // The table rendered: column headers and a geometry row for each ROI.
    assert!(
        harness.query_by_label("Name").is_some(),
        "the manager table must show its Name column header"
    );
    assert!(
        harness.query_by_label("Region").is_some(),
        "the manager table must show its Region column header"
    );
    assert!(
        harness
            .query_by_label(&rect_desc(alpha.0, alpha.1))
            .is_some(),
        "the table must render a row for the first ROI"
    );
    assert!(
        harness.query_by_label(&rect_desc(beta.0, beta.1)).is_some(),
        "the table must render a row for the second ROI"
    );

    // Click the second ROI's geometry selector → it becomes current.
    harness.get_by_label(&rect_desc(beta.0, beta.1)).click();
    harness.run();

    assert_eq!(
        app.borrow().current_roi(),
        Some(i_beta),
        "clicking a row's geometry selector must make that ROI current"
    );
}
