//! ROI collection-change events on `PlotWidget` (silx `RegionOfInterestManager`
//! signals). `PlotWidget` owns a `WgpuBackend`, so it needs a real `RenderState`
//! — built here through egui_kittest's wgpu test setup (headless), the same way
//! the render tests do — but no rendering is exercised; only the ROI mutation
//! API and the events it pushes are asserted.

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use rsplot::{PlotEvent, PlotWidget, Roi, RoiInteractionMode};

fn rect(x: f64) -> Roi {
    Roi::Rect {
        x: (x, x + 1.0),
        y: (0.0, 1.0),
    }
}

fn arc() -> Roi {
    Roi::Arc {
        center: (0.0, 0.0),
        radius: 1.5,
        weight: 1.0,
        start_angle: 0.0,
        end_angle: std::f64::consts::FRAC_PI_2,
    }
}

#[test]
fn remove_roi_emits_about_to_be_removed_before_removal() {
    let rs = create_render_state(default_wgpu_setup());
    let mut plot = PlotWidget::new(&rs, 0);

    assert_eq!(plot.add_roi(rect(0.0)), 0);
    assert_eq!(plot.add_roi(rect(2.0)), 1);
    plot.drain_events(); // discard the two RoiAdded events

    plot.remove_roi(0);
    let events = plot.drain_events();

    // silx `sigRoiAboutToBeRemoved` fires for the removed index...
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PlotEvent::RoiAboutToBeRemoved { index: 0 })),
        "remove_roi must emit RoiAboutToBeRemoved {{ index: 0 }}, got {events:?}"
    );
    // ...and a single removal must NOT masquerade as a clear-all.
    assert!(
        !events.iter().any(|e| matches!(e, PlotEvent::RoisCleared)),
        "a single-ROI removal must not emit RoisCleared, got {events:?}"
    );
    assert_eq!(plot.rois().len(), 1, "one ROI remains after removing one");
}

#[test]
fn clear_rois_emits_rois_cleared() {
    let rs = create_render_state(default_wgpu_setup());
    let mut plot = PlotWidget::new(&rs, 0);
    plot.add_roi(rect(0.0));
    plot.add_roi(rect(2.0));
    plot.drain_events();

    plot.clear_rois();
    let events = plot.drain_events();
    assert!(
        events.iter().any(|e| matches!(e, PlotEvent::RoisCleared)),
        "clear_rois emits RoisCleared, got {events:?}"
    );
    assert_eq!(plot.rois().len(), 0);
}

#[test]
fn save_then_load_round_trips_rois_through_the_widget() {
    let rs = create_render_state(default_wgpu_setup());
    let mut plot = PlotWidget::new(&rs, 0);
    plot.add_roi(rect(0.0));
    plot.add_roi(rect(2.0));
    let before = plot.rois().to_vec();
    plot.drain_events();

    // Unique per-test path (nextest runs each test in its own process).
    let path = std::env::temp_dir().join("rsplot_roi_events_round_trip.rois");
    plot.save_rois_to_path(&path)
        .expect("save_rois_to_path writes");

    // Mutate the live set, then load the file back: the loaded set replaces it.
    plot.add_roi(rect(9.0));
    plot.drain_events();
    plot.load_rois_from_path(&path)
        .expect("load_rois_from_path reads");

    assert_eq!(
        plot.rois(),
        before.as_slice(),
        "the loaded ROIs replace the live set and match what was saved"
    );

    // load = clear-all (RoisCleared) then one RoiAdded per loaded ROI.
    let events = plot.drain_events();
    assert!(
        events.iter().any(|e| matches!(e, PlotEvent::RoisCleared)),
        "load_rois_from_path clears the previous set first, got {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, PlotEvent::RoiAdded { .. }))
            .count(),
        before.len(),
        "load_rois_from_path emits one RoiAdded per loaded ROI, got {events:?}"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn set_roi_interaction_mode_switches_mode_and_emits_event() {
    let rs = create_render_state(default_wgpu_setup());
    let mut plot = PlotWidget::new(&rs, 0);
    plot.add_roi(arc());
    plot.drain_events();
    // An Arc seeds the silx default (ThreePoint).
    assert_eq!(
        plot.roi_interaction_mode(0),
        Some(RoiInteractionMode::ArcThreePoint)
    );

    // An available mode switches and emits RoiInteractionModeChanged.
    assert!(plot.set_roi_interaction_mode(0, RoiInteractionMode::ArcPolar));
    assert_eq!(
        plot.roi_interaction_mode(0),
        Some(RoiInteractionMode::ArcPolar)
    );
    let events = plot.drain_events();
    assert!(
        events.iter().any(|e| matches!(
            e,
            PlotEvent::RoiInteractionModeChanged {
                index: 0,
                mode: RoiInteractionMode::ArcPolar
            }
        )),
        "set_roi_interaction_mode must emit RoiInteractionModeChanged, got {events:?}"
    );

    // A mode foreign to the kind is rejected — mode unchanged, no event.
    assert!(!plot.set_roi_interaction_mode(0, RoiInteractionMode::BandBounded));
    assert_eq!(
        plot.roi_interaction_mode(0),
        Some(RoiInteractionMode::ArcPolar)
    );
    assert!(
        plot.drain_events().is_empty(),
        "a rejected mode change emits no event"
    );

    // An out-of-range index changes nothing and emits no event.
    assert!(!plot.set_roi_interaction_mode(9, RoiInteractionMode::ArcThreePoint));
    assert!(plot.drain_events().is_empty());
}

#[test]
fn remove_roi_out_of_range_emits_nothing() {
    let rs = create_render_state(default_wgpu_setup());
    let mut plot = PlotWidget::new(&rs, 0);
    plot.add_roi(rect(0.0));
    plot.drain_events();

    plot.remove_roi(7); // out of range
    assert!(
        plot.drain_events().is_empty(),
        "an out-of-range remove_roi must emit no event"
    );
    assert_eq!(plot.rois().len(), 1, "nothing was removed");
}
