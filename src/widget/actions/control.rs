//! Plot control actions, mirroring silx `silx.gui.plot.actions.control`.
//!
//! Functions here mutate a [`PlotWidget`] (or its underlying [`Plot`]) in
//! place, performing the same state transition the corresponding silx
//! `QAction._actionTriggered` does, without the `QAction` machinery.
//!
//! [`Plot`]: crate::core::plot::Plot

use crate::widget::high_level::{ImageView, PlotWidget, ScatterView};

/// Advance the plot-wide default curve style `(lines, points)` one step,
/// porting silx `CurveStyleAction._actionTriggered` (`actions/control.py`:
/// 338-349): the state cycles line-only → line+symbol → symbol-only →
/// line-only, and the invalid `(false, false)` (neither line nor symbol)
/// recovers to line-only exactly as silx special-cases it. Pure and
/// deterministic so the cycle is unit-testable without a plot.
pub fn next_curve_style_state(current: (bool, bool)) -> (bool, bool) {
    // silx: `states = (True, False), (True, True), (False, True)`.
    const STATES: [(bool, bool); 3] = [(true, false), (true, true), (false, true)];
    if current == (false, false) {
        return (true, false);
    }
    match STATES.iter().position(|state| *state == current) {
        Some(index) => STATES[(index + 1) % STATES.len()],
        // Unreachable for the three valid states handled above; recover to the
        // line-only base rather than indexing out of range.
        None => (true, false),
    }
}

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

/// Toggle whether the arrow keys pan the data area when the plot is focused,
/// mirroring silx `PanWithArrowKeysAction` (`actions/control.py`): the checkable
/// action's `_actionTriggered(checked)` calls `plot.setPanWithArrowKeys(checked)`,
/// flipping the current state.
///
/// Returns the new `pan_with_arrow_keys` value after the toggle.
pub fn toggle_pan_with_arrow_keys(plot: &mut PlotWidget) -> bool {
    let next = !plot.plot().pan_with_arrow_keys();
    plot.plot_mut().set_pan_with_arrow_keys(next);
    next
}

/// silx zoom step factor (`ZoomInAction` calls `applyZoomToPlot(plot, 1.1)`;
/// `ZoomOutAction` calls `applyZoomToPlot(plot, 1.0 / 1.1)`,
/// `actions/control.py`). ZoomIn shrinks each axis range by this factor, ZoomOut
/// grows it by its reciprocal.
pub const ZOOM_STEP: f64 = 1.1;

/// Scale a 1D `(min, max)` range about `center` by `scale`, mirroring silx
/// `scale1DRange` (`_utils/panzoom.py`): the new range is `(max - min) / scale`,
/// keeping `center` fixed at its original fractional offset within the range. A
/// degenerate range (`min == max`, compared in the working space) is returned
/// unchanged, as in silx.
///
/// When `is_log` is true the scaling happens in `log10` space (a [`Scale::Log10`]
/// axis guarantees `min`/`max`/`center > 0` by construction): the three inputs
/// are mapped through `log10`, scaled linearly there, then mapped back via
/// `10^x` — identical to silx's log branch. silx additionally clips the result
/// to the float32 safe range; here that clip belongs to the view-limits owner
/// (`set_limits_internal`), which every commit path funnels through, so this
/// function stays pure and deterministic and the zoom math is unit-testable
/// without a GPU backend.
///
/// [`Scale::Log10`]: crate::core::transform::Scale::Log10
pub fn scale_1d_range(min: f64, max: f64, center: f64, scale: f64, is_log: bool) -> (f64, f64) {
    if is_log {
        let (lmin, lmax, lcenter) = (min.log10(), max.log10(), center.log10());
        if lmin == lmax {
            return (min, max);
        }
        let offset = (lcenter - lmin) / (lmax - lmin);
        let range = (lmax - lmin) / scale;
        let new_min = lcenter - offset * range;
        let new_max = lcenter + (1.0 - offset) * range;
        return (10f64.powf(new_min), 10f64.powf(new_max));
    }
    if min == max {
        return (min, max);
    }
    let offset = (center - min) / (max - min);
    let range = (max - min) / scale;
    let new_min = center - offset * range;
    let new_max = center + (1.0 - offset) * range;
    (new_min, new_max)
}

/// Scale a 1D `(min, max)` range by `scale` about its own midpoint — the center
/// the toolbar zoom buttons use when the whole view is visible (silx
/// `applyZoomToPlot` defaults the center to the plot-bounds center, which for a
/// fully-visible view is the range midpoint). For a `is_log` axis the visual
/// midpoint is the geometric mean `sqrt(min * max)` (the data value at the middle
/// pixel, matching silx mapping the pixel center through `pixelToData`).
/// Convenience over [`scale_1d_range`].
pub fn scale_1d_range_about_midpoint(min: f64, max: f64, scale: f64, is_log: bool) -> (f64, f64) {
    let center = if is_log {
        (min * max).sqrt()
    } else {
        0.5 * (min + max)
    };
    scale_1d_range(min, max, center, scale, is_log)
}

/// Apply a zoom `scale` to the plot's X and Y limits about their midpoints,
/// mirroring silx `applyZoomToPlot` (`_utils/panzoom.py`). `scale > 1` zooms in
/// (shrinks the range); `scale < 1` zooms out.
///
/// Does NOT push the limits history: silx's `ZoomInAction`/`ZoomOutAction` call
/// `applyZoomToPlot` → `plot.setLimits(...)`, which never touches
/// `LimitsHistory`; only the drag-zoom *interaction* pushes (`getLimitsHistory()
/// .push()` in `PlotInteraction`, mirrored by `plot_widget.rs`). So [`zoom_back`]
/// restores the last interactive zoom, not a toolbar zoom — matching silx.
///
/// [`zoom_back`]: zoom_back
fn apply_zoom(plot: &mut PlotWidget, scale: f64) {
    use crate::core::transform::Scale;
    // Per-axis log state drives whether each range scales in log space, matching
    // silx applyZoomToPlot passing each axis' _isLogarithmic() to scale1DRange.
    let x_log = plot.plot().x_scale == Scale::Log10;
    let y_log = plot.plot().y_scale == Scale::Log10;
    let (xmin, xmax) = plot.x_limits();
    let (nxmin, nxmax) = scale_1d_range_about_midpoint(xmin, xmax, scale, x_log);
    let y = plot.y_limits(crate::core::transform::YAxis::Left);
    let (nymin, nymax) = match y {
        Some((ymin, ymax)) => scale_1d_range_about_midpoint(ymin, ymax, scale, y_log),
        None => {
            let (_, _, ymin, ymax) = plot.plot().limits;
            scale_1d_range_about_midpoint(ymin, ymax, scale, y_log)
        }
    };
    // silx scales y2 with the left Y axis' log state (applyZoomToPlot passes
    // plot.getYAxis()._isLogarithmic() for the right axis too); mirror that.
    let y2 = plot
        .plot()
        .y2
        .map(|(y2min, y2max)| scale_1d_range_about_midpoint(y2min, y2max, scale, y_log));
    plot.set_limits(nxmin, nxmax, nymin, nymax, y2);
}

/// Zoom in, shrinking the view by [`ZOOM_STEP`] about its center (silx
/// `ZoomInAction`).
pub fn zoom_in(plot: &mut PlotWidget) {
    apply_zoom(plot, ZOOM_STEP);
}

/// Zoom out, growing the view by `1 / `[`ZOOM_STEP`] about its center (silx
/// `ZoomOutAction`).
pub fn zoom_out(plot: &mut PlotWidget) {
    apply_zoom(plot, 1.0 / ZOOM_STEP);
}

/// Restore the most recently pushed view from the limits history, falling back
/// to a reset-zoom when the history is empty, mirroring silx `ZoomBackAction`
/// (whose `LimitsHistory.pop` calls `plot.resetZoom()` on an empty stack).
/// Returns `true` if a stored view was restored, `false` if it fell back to
/// reset-zoom.
pub fn zoom_back(plot: &mut PlotWidget) -> bool {
    if plot.zoom_back() {
        true
    } else {
        plot.reset_zoom();
        false
    }
}

/// Cycle the plot-wide default curve style, mirroring silx `CurveStyleAction`:
/// advances the `(plot lines, plot points)` state via [`next_curve_style_state`]
/// and applies the new defaults to every curve. Returns the new
/// `(lines, points)` state.
pub fn curve_style_cycle(plot: &mut PlotWidget) -> (bool, bool) {
    plot.cycle_curve_style()
}

/// Toggle the X-axis autoscale flag, mirroring silx `XAxisAutoScaleAction`
/// (`actions/control.py`): its `_actionTriggered(checked)` calls
/// `plot.getXAxis().setAutoScale(checked)` and, when enabling, `plot.resetZoom()`.
///
/// This flips the current `x_autoscale` flag; when the new value is `true` it
/// also reset-zooms the widget so the X axis immediately refits to data (silx
/// only reset-zooms on enable, never on disable — disabling just pins the
/// current view). The reset routes through [`PlotWidget::reset_zoom`], which
/// honors the per-axis autoscale flags. Returns the new `x_autoscale` value.
pub fn toggle_x_autoscale(plot: &mut PlotWidget) -> bool {
    let next = !plot.plot().x_autoscale();
    plot.plot_mut().set_x_autoscale(next);
    if next {
        plot.reset_zoom();
    }
    next
}

/// Toggle the Y-axis autoscale flag, mirroring silx `YAxisAutoScaleAction`
/// (`actions/control.py`). Like [`toggle_x_autoscale`], reset-zooms only when
/// enabling (silx `if checked: plot.resetZoom()`). Returns the new
/// `y_autoscale` value.
pub fn toggle_y_autoscale(plot: &mut PlotWidget) -> bool {
    let next = !plot.plot().y_autoscale();
    plot.plot_mut().set_y_autoscale(next);
    if next {
        plot.reset_zoom();
    }
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
    use super::*;

    #[test]
    fn curve_style_state_cycles_like_silx_and_recovers_invalid() {
        // silx cycle: line-only → line+symbol → symbol-only → line-only.
        assert_eq!(next_curve_style_state((true, false)), (true, true));
        assert_eq!(next_curve_style_state((true, true)), (false, true));
        assert_eq!(next_curve_style_state((false, true)), (true, false));
        // The invalid (no line, no symbol) state recovers to line-only.
        assert_eq!(next_curve_style_state((false, false)), (true, false));
    }

    #[test]
    fn scale_1d_range_zoom_in_and_out_about_midpoint() {
        // Range [0, 10], midpoint 5.
        // Zoom in by 1.1: range 10/1.1 = 9.0909..., centered on 5 →
        // [0.4545..., 9.5454...].
        let (zin_min, zin_max) = scale_1d_range_about_midpoint(0.0, 10.0, ZOOM_STEP, false);
        assert!((zin_min - (5.0 - 10.0 / 1.1 / 2.0)).abs() < 1e-12);
        assert!((zin_max - (5.0 + 10.0 / 1.1 / 2.0)).abs() < 1e-12);
        assert!(zin_max - zin_min < 10.0, "zoom in shrinks the range");

        // Zoom out by 1/1.1: range 10*1.1 = 11, centered on 5 → [-0.5, 10.5].
        let (zout_min, zout_max) = scale_1d_range_about_midpoint(0.0, 10.0, 1.0 / ZOOM_STEP, false);
        assert!((zout_min - (-0.5)).abs() < 1e-12);
        assert!((zout_max - 10.5).abs() < 1e-12);
        assert!(zout_max - zout_min > 10.0, "zoom out grows the range");
    }

    #[test]
    fn scale_1d_range_keeps_off_center_invariant_point() {
        // center at min keeps min fixed; new max = min + (max-min)/scale.
        let (min, max) = scale_1d_range(2.0, 12.0, 2.0, 2.0, false);
        assert!((min - 2.0).abs() < 1e-12, "center stays put");
        assert!((max - 7.0).abs() < 1e-12, "range halved from the center");
    }

    #[test]
    fn scale_1d_range_degenerate_is_unchanged() {
        assert_eq!(scale_1d_range(3.0, 3.0, 3.0, 1.5, false), (3.0, 3.0));
    }

    #[test]
    fn scale_1d_range_log_scales_in_log10_space() {
        // Log axis [1, 1000]: silx scale1DRange scales in log10 space. Zoom about
        // the geometric midpoint keeps the geometric center fixed and divides the
        // log10 span (= 3 decades) by the scale.
        let (lo, hi) = scale_1d_range_about_midpoint(1.0, 1000.0, ZOOM_STEP, true);
        // Geometric center preserved (the log-space midpoint is the invariant).
        assert!(
            ((lo * hi).sqrt() - (1.0_f64 * 1000.0).sqrt()).abs() < 1e-9,
            "geometric center fixed"
        );
        // log10 span divided by the zoom step (3 decades / 1.1).
        let new_span = hi.log10() - lo.log10();
        assert!(
            (new_span - 3.0 / ZOOM_STEP).abs() < 1e-9,
            "log span / scale"
        );
        assert!(new_span < 3.0, "zoom in shrinks the log span");
        // Both bounds stay positive (a valid log axis).
        assert!(lo > 0.0 && hi > lo);
    }

    #[test]
    fn zoom_back_restores_pushed_view_then_falls_back_when_empty() {
        // Exercise the zoom_back action's decision (restore-or-fallback) on the
        // bare Plot model the action ultimately drives, without a GPU backend.
        use crate::core::plot::Plot;

        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        // Push the home view, then change limits (a zoom).
        plot.push_limits();
        plot.limits = (2.0, 4.0, 2.0, 4.0);
        assert_eq!(plot.limits_history_len(), 1);

        // First zoom_back restores the pushed view and pops the entry.
        assert!(plot.zoom_back(), "restored a stored view");
        assert_eq!(plot.limits, (0.0, 10.0, 0.0, 10.0));
        assert_eq!(plot.limits_history_len(), 0);

        // Empty history: zoom_back returns false (the action then reset-zooms;
        // here we just assert it does not panic and signals empty).
        assert!(!plot.zoom_back(), "empty history signals fallback");
    }

    #[test]
    fn autoscale_toggle_reset_on_enable_refits_only_that_axis() {
        // Mirror toggle_x_autoscale / toggle_y_autoscale on the bare Plot model
        // the actions drive through PlotWidget (no GPU backend): flip the flag,
        // and on enable reset-zoom through the flag-aware owner.
        use crate::core::plot::{DataRange, Plot};

        // Start with X autoscale OFF and a pinned X view; Y autoscale ON.
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 1.0, 0.0, 1.0);
        plot.set_x_autoscale(false);
        plot.set_y_autoscale(true);
        let data = DataRange {
            x: Some((10.0, 20.0)),
            y: Some((-5.0, 5.0)),
            y2: None,
        };

        // Enable X autoscale: flag flips true, and the reset-on-enable refits X
        // (now autoscale-on) while Y also refits (it was already on).
        let next = !plot.x_autoscale();
        plot.set_x_autoscale(next);
        assert!(next, "X autoscale enabled");
        plot.reset_zoom_to_data_range(data);
        assert_eq!(plot.limits, (10.0, 20.0, -5.0, 5.0));
    }

    #[test]
    fn autoscale_toggle_disable_does_not_reset() {
        // silx reset-zooms only on enable; disabling pins the current view.
        // Mirror the disable branch: flag flips false, no reset-zoom is called,
        // so limits are untouched.
        use crate::core::plot::Plot;

        let mut plot = Plot::new(0);
        plot.limits = (3.0, 7.0, 2.0, 8.0);
        assert!(plot.x_autoscale(), "default on");

        let next = !plot.x_autoscale();
        plot.set_x_autoscale(next);
        assert!(!next, "X autoscale disabled");
        // No reset-zoom on disable -> limits unchanged.
        assert_eq!(plot.limits, (3.0, 7.0, 2.0, 8.0));
    }

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

    #[test]
    fn pan_with_arrow_keys_toggle_flips_flag() {
        // A bare Plot model exercises the same set_pan_with_arrow_keys transition
        // the action drives through PlotWidget, without a GPU backend.
        use crate::core::plot::Plot;

        let mut plot = Plot::new(0);
        assert!(plot.pan_with_arrow_keys(), "default is enabled (silx True)");

        // Mirror toggle_pan_with_arrow_keys's body on the bare model.
        let next = !plot.pan_with_arrow_keys();
        plot.set_pan_with_arrow_keys(next);
        assert!(!plot.pan_with_arrow_keys(), "first toggle disables");

        let next = !plot.pan_with_arrow_keys();
        plot.set_pan_with_arrow_keys(next);
        assert!(plot.pan_with_arrow_keys(), "second toggle re-enables");
    }
}
