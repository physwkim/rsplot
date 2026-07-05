//! Ruler measurement tool (silx `RulerToolButton` / `_RulerROI`).
//!
//! `RulerToolButton` (the checkable button + `distance_text` formatter) is
//! unit-tested in `tool_buttons`; this exercises the host integration that was
//! the remaining gap (roadmap rows 1264/1253): while the ruler is armed, a
//! primary drag draws a line ROI whose name is its measured length, recomputed
//! live. Building a `PlotWidget` and caching its transform both need a real
//! rendered frame, so this runs through the egui_kittest + wgpu harness like
//! `compare_separator`.

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui;
use rsplot::{PlotInteractionMode, PlotWidget, Roi, RoiDrawKind, RulerToolButton, YAxis};

/// Build a harness around a bare `PlotWidget` with a fixed data range
/// (x,y ∈ [0,10]) so data↔pixel mapping is deterministic, render two frames so
/// the transform is cached, and return the shared widget + harness.
fn harness() -> (Rc<RefCell<PlotWidget>>, Harness<'static>) {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);

    let mut plot = PlotWidget::new(&rs, 0);
    let x: Vec<f64> = (0..=10).map(|i| i as f64).collect();
    plot.add_curve(&x, &x, egui::Color32::WHITE);
    plot.set_limits(0.0, 10.0, 0.0, 10.0, None);

    let app = Rc::new(RefCell::new(plot));
    let app_ui = app.clone();
    let renderer = WgpuTestRenderer::from_render_state(rs.clone());
    let mut harness = Harness::builder()
        .with_size(egui::vec2(400.0, 400.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            app_ui.borrow_mut().show(ui);
        });
    harness.step();
    harness.step();
    (app, harness)
}

/// Drag from `p0` to `p1` with an incremental ramp so egui reports a genuine
/// drag (a single jump makes `drag_started` fire already at the end point).
fn drag(harness: &mut Harness<'static>, p0: egui::Pos2, p1: egui::Pos2) {
    harness.drag_at(p0);
    harness.step();
    // Start the move ramp at t=0 so the line draw captures its start point at p0
    // (the draw records its first endpoint on the first *move*, not the press).
    for t in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
        harness.hover_at(p0 + (p1 - p0) * t);
        harness.step();
    }
    harness.drop_at(p1);
    harness.step();
    harness.step();
}

/// Map data `(x, y)` to a screen pixel via the cached transform.
fn px(app: &Rc<RefCell<PlotWidget>>, x: f64, y: f64) -> egui::Pos2 {
    app.borrow()
        .data_to_pixel(x, y, YAxis::Left)
        .expect("transform cached after a frame")
}

#[test]
fn arming_the_ruler_enters_a_line_draw() {
    let (app, _harness) = harness();
    assert!(!app.borrow().ruler_active());

    app.borrow_mut().set_ruler_active(true);
    assert!(app.borrow().ruler_active());
    assert_eq!(
        app.borrow().interaction_mode(),
        PlotInteractionMode::RoiCreate(RoiDrawKind::Line),
        "arming the ruler must enter a line-ROI draw"
    );
    assert_eq!(app.borrow().ruler_roi(), None);
}

#[test]
fn dragging_draws_a_line_roi_labeled_with_its_distance() {
    let (app, mut harness) = harness();
    app.borrow_mut().set_ruler_active(true);

    let p0 = px(&app, 1.0, 1.0);
    let p1 = px(&app, 4.0, 5.0);
    drag(&mut harness, p0, p1);

    // Exactly one ruler line ROI now exists, tracked by the widget.
    assert_eq!(app.borrow().rois().len(), 1);
    let index = app.borrow().ruler_roi().expect("a ruler line was drawn");
    assert_eq!(index, 0);

    // Its name is the distance_text of its *own* geometry — the wiring under
    // test. (The drag's data endpoints differ slightly from the requested ones
    // by pixel rounding, so the label is verified against the actual line, not
    // the requested coordinates.)
    let roi = app.borrow().rois()[index].clone();
    let Roi::Line { start, end } = roi.roi else {
        panic!("the ruler ROI must be a line, got {:?}", roi.roi);
    };
    let expected = RulerToolButton::distance_text([start.0, start.1], [end.0, end.1]);
    assert_eq!(roi.name, expected, "ruler label must equal its own length");

    // Sanity: the line tracks the drag — it runs lower-left → upper-right with
    // endpoints bracketing the dragged region (the start is slightly inset from
    // the press because the draw records its first endpoint on the first move).
    assert!(
        (1.0..2.5).contains(&start.0) && (1.0..2.5).contains(&start.1),
        "ruler start should sit near the (1,1) press, got {start:?}"
    );
    assert!(
        (3.5..4.5).contains(&end.0) && (4.5..5.5).contains(&end.1),
        "ruler end should sit near the (4,5) drop, got {end:?}"
    );
}

#[test]
fn a_second_drag_replaces_the_ruler_line() {
    let (app, mut harness) = harness();
    app.borrow_mut().set_ruler_active(true);

    drag(&mut harness, px(&app, 1.0, 1.0), px(&app, 4.0, 5.0));
    assert_eq!(app.borrow().rois().len(), 1);
    let first_name = app.borrow().rois()[0].name.clone();

    // A second measurement replaces the first: still exactly one ROI, relabeled.
    drag(&mut harness, px(&app, 0.0, 0.0), px(&app, 8.0, 0.0));
    assert_eq!(
        app.borrow().rois().len(),
        1,
        "a new measurement must replace the previous ruler line, not accumulate"
    );
    let index = app.borrow().ruler_roi().expect("ruler line present");
    let roi = app.borrow().rois()[index].clone();
    let Roi::Line { start, end } = roi.roi else {
        panic!("ruler ROI must be a line");
    };
    let expected = RulerToolButton::distance_text([start.0, start.1], [end.0, end.1]);
    assert_eq!(roi.name, expected);
    // The horizontal 8px segment is longer than the first ≈5px one.
    assert_ne!(roi.name, first_name);
}

#[test]
fn disarming_hides_the_ruler_line_and_rearming_reshows_it() {
    let (app, mut harness) = harness();
    // The default mode is Zoom; arming saves it and must restore it on disarm.
    assert_eq!(app.borrow().interaction_mode(), PlotInteractionMode::Zoom);
    app.borrow_mut().set_ruler_active(true);

    drag(&mut harness, px(&app, 1.0, 1.0), px(&app, 4.0, 5.0));
    assert_eq!(app.borrow().rois().len(), 1);
    let name = app.borrow().rois()[0].name.clone();

    // silx RulerToolButton._callback: unchecking hides the last measurement
    // (`_lastRoiCreated.setVisible(isChecked())`, RulerToolButton.py:118-122);
    // it is NOT removed.
    app.borrow_mut().set_ruler_active(false);
    assert!(!app.borrow().ruler_active());
    assert_eq!(
        app.borrow().rois().len(),
        1,
        "disarming must keep the measurement (hidden), not destroy it"
    );
    assert!(
        !app.borrow().rois()[0].visible,
        "the kept measurement must be hidden while disarmed"
    );
    assert_eq!(
        app.borrow().ruler_roi(),
        Some(0),
        "the widget must keep tracking the hidden ruler line"
    );
    assert_eq!(
        app.borrow().interaction_mode(),
        PlotInteractionMode::Zoom,
        "disarming must restore the pre-ruler interaction mode"
    );

    // Re-checking reshows the previous measurement, unchanged.
    app.borrow_mut().set_ruler_active(true);
    assert!(app.borrow().rois()[0].visible, "re-arming must reshow it");
    assert_eq!(app.borrow().rois()[0].name, name);
}

#[test]
fn ruler_tracking_survives_other_roi_removals() {
    let (app, mut harness) = harness();
    // An unrelated ROI sits at index 0, so the ruler line lands at index 1.
    app.borrow_mut().add_roi(Roi::Rect {
        x: (6.0, 8.0),
        y: (6.0, 8.0),
    });
    app.borrow_mut().set_ruler_active(true);
    drag(&mut harness, px(&app, 1.0, 1.0), px(&app, 4.0, 5.0));
    assert_eq!(app.borrow().ruler_roi(), Some(1));

    // Removing the ROI below shifts the ruler line down; the tracker follows
    // (silx tracks _lastRoiCreated by reference, immune to list order).
    app.borrow_mut().remove_roi(0);
    assert_eq!(app.borrow().ruler_roi(), Some(0));
    let roi = app.borrow().rois()[0].clone();
    assert!(matches!(roi.roi, Roi::Line { .. }));

    // Removing the ruler line itself clears the tracker; clearing all ROIs
    // does too.
    app.borrow_mut().remove_roi(0);
    assert_eq!(app.borrow().ruler_roi(), None);

    drag(&mut harness, px(&app, 0.0, 0.0), px(&app, 3.0, 0.0));
    assert!(app.borrow().ruler_roi().is_some());
    app.borrow_mut().clear_rois();
    assert_eq!(app.borrow().ruler_roi(), None);
}
