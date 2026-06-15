//! ROI collection-change events on `PlotWidget` (silx `RegionOfInterestManager`
//! signals). `PlotWidget` owns a `WgpuBackend`, so it needs a real `RenderState`
//! — built here through egui_kittest's wgpu test setup (headless), the same way
//! the render tests do — but no rendering is exercised; only the ROI mutation
//! API and the events it pushes are asserted.

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use siplot::{PlotEvent, PlotWidget, Roi};

fn rect(x: f64) -> Roi {
    Roi::Rect {
        x: (x, x + 1.0),
        y: (0.0, 1.0),
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
