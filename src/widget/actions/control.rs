//! Plot control actions, mirroring silx `silx.gui.plot.actions.control`.
//!
//! Functions here mutate a [`PlotWidget`] (or its underlying [`Plot`]) in
//! place, performing the same state transition the corresponding silx
//! `QAction._actionTriggered` does, without the `QAction` machinery.
//!
//! [`Plot`]: crate::core::plot::Plot

use crate::widget::high_level::{ImageView, PlotWidget, ScatterView};

/// Toggle whether the plot axes (frame/ticks/labels) are displayed, mirroring
/// silx `ShowAxisAction` (`actions/control.py`): its `_actionTriggered(checked)`
/// calls `plot.setAxesDisplayed(checked)`, flipping the current state.
///
/// Returns the new `axes_displayed` value after the toggle.
pub fn show_axis_toggle(plot: &mut PlotWidget) -> bool {
    let next = !plot.plot().axes_displayed();
    plot.plot_mut().set_axes_displayed(next);
    next
}

/// Toggle the [`ImageView`] side colorbar's visibility, mirroring silx
/// `ColorBarAction`: its `_actionTriggered(checked)` sets the `ColorBarWidget`'s
/// visibility. Returns the new `show_colorbar` value.
pub fn image_colorbar_toggle(view: &mut ImageView) -> bool {
    let next = !view.show_colorbar();
    view.set_show_colorbar(next);
    next
}

/// Toggle the [`ScatterView`] side colorbar's visibility, mirroring silx
/// `ColorBarAction`. Returns the new `show_colorbar` value.
pub fn scatter_colorbar_toggle(view: &mut ScatterView) -> bool {
    let next = !view.show_colorbar();
    view.set_show_colorbar(next);
    next
}

#[cfg(test)]
mod tests {
    #[test]
    fn show_axis_toggle_flips_axes_displayed() {
        // A bare Plot model exercises the same set_axes_displayed transition the
        // action drives through PlotWidget, without a GPU backend.
        use crate::core::plot::Plot;

        let mut plot = Plot::new(0);
        assert!(plot.axes_displayed(), "default is displayed");

        // Mirror show_axis_toggle's body on the bare model.
        let next = !plot.axes_displayed();
        plot.set_axes_displayed(next);
        assert!(!plot.axes_displayed(), "first toggle hides");

        let next = !plot.axes_displayed();
        plot.set_axes_displayed(next);
        assert!(plot.axes_displayed(), "second toggle shows again");
    }
}
