//! Limits-history lifecycle through the widget verbs, mirroring silx:
//! entering Zoom mode clears the history (silx `_setInteractiveMode`
//! re-instantiates the handler and `Zoom.__init__` runs
//! `getLimitsHistory().clear()`, `PlotInteraction.py:365-370`), while other
//! mode switches leave it alone.
//!
//! Needs a GPU (real or software); mirrors `tests/roi_events.rs`' harness.

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use rsplot::{Plot1D, PlotInteractionMode};

#[test]
fn entering_zoom_mode_clears_limits_history() {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);
    let mut plot = Plot1D::new(&rs, 0);

    plot.plot_mut().push_limits();
    plot.plot_mut().push_limits();
    assert_eq!(plot.plot().limits_history_len(), 2);

    // A non-Zoom switch keeps the stack.
    plot.set_interaction_mode(PlotInteractionMode::Pan);
    assert_eq!(plot.plot().limits_history_len(), 2, "Pan keeps the history");

    // Entering Zoom clears it — every time, like silx re-instantiating Zoom.
    plot.set_interaction_mode(PlotInteractionMode::Zoom);
    assert_eq!(plot.plot().limits_history_len(), 0, "Zoom entry clears");

    plot.plot_mut().push_limits();
    plot.set_interaction_mode(PlotInteractionMode::Zoom);
    assert_eq!(plot.plot().limits_history_len(), 0, "re-entry clears again");
}
