//! pyqtgraph-style Y-axis context menu shared by the XY plot widgets.
//!
//! siplot's plot widget already carries a built-in right-click menu (Zoom Back /
//! Reset Zoom) and exposes
//! [`show_with_context_menu`](siplot::PlotWidget::show_with_context_menu) to
//! append entries to it. This module adds a pyqtgraph `ViewBox`-style "Y axis"
//! section — an auto-scale toggle and a manual min/max range — without touching
//! siplot: each plot widget owns a [`YAxisMenu`], and [`show_with_y_axis_menu`]
//! renders it inside the plot's `show` closure and applies the captured choice.
//!
//! [`set_y_range`] and [`enable_y_autoscale`] are the single owners of the
//! "pin a fixed range (autoscale off)" / "live autoscale (on + refit)" rules;
//! the menu and the plot widgets' public `set_y_range` / `enable_y_autoscale`
//! both go through them, so the two entry points cannot drift.

use siplot::{Plot1D, PlotResponse, YAxis, egui};

/// Order a `(min, max)` pair, expanding a degenerate range to unit width so the
/// resulting Y limits are always non-empty.
fn normalize_range(min: f64, max: f64) -> (f64, f64) {
    if max > min {
        (min, max)
    } else {
        (min, min + 1.0)
    }
}

/// Pin a fixed left-Y range and disable live autoscale (pyqtgraph `setYRange`).
/// Turning autoscale off is what keeps the range from being overwritten on the
/// next data update (`apply_auto_limits` preserves a non-autoscale Y axis).
pub(crate) fn set_y_range(plot: &mut Plot1D, min: f64, max: f64) {
    let (min, max) = normalize_range(min, max);
    plot.plot_mut().set_y_autoscale(false);
    plot.set_graph_y_limits(min, max, YAxis::Left);
}

/// Re-enable live Y autoscale and refit to the data now (pyqtgraph auto-range).
pub(crate) fn enable_y_autoscale(plot: &mut Plot1D) {
    plot.plot_mut().set_y_autoscale(true);
    plot.reset_zoom_to_data();
}

/// Pin a fixed X range and disable X autoscale (pyqtgraph `setXRange`) — the X
/// counterpart of [`set_y_range`], same normalize-then-pin rule.
pub(crate) fn set_x_range(plot: &mut Plot1D, min: f64, max: f64) {
    let (min, max) = normalize_range(min, max);
    plot.plot_mut().set_x_autoscale(false);
    plot.set_graph_x_limits(min, max);
}

/// What the user committed in the Y-axis menu this frame.
enum YAxisChoice {
    /// Re-enable live autoscale (pyqtgraph "auto").
    Autoscale,
    /// Pin a fixed `[min, max]` Y range (disables autoscale).
    SetRange(f64, f64),
}

/// Per-plot state for the Y-axis context-menu section: the in-progress min/max
/// edit fields plus open-tracking, so the fields seed from the live Y range only
/// while the menu is closed (an open menu keeps the user's edits instead of
/// snapping them back to the streaming range every frame).
pub(crate) struct YAxisMenu {
    min: f64,
    max: f64,
    /// Whether the menu rendered last frame (i.e. was open).
    open: bool,
    /// Set by `render` this frame; latched into `open` by `finish`.
    rendered_this_frame: bool,
}

impl YAxisMenu {
    pub(crate) fn new() -> Self {
        Self {
            min: 0.0,
            max: 1.0,
            open: false,
            rendered_this_frame: false,
        }
    }

    /// Before `show`: while the menu is closed, track the plot's current left-Y
    /// limits so opening it shows the live range. No-op while the menu is open.
    fn sync(&mut self, current: Option<(f64, f64)>) {
        if !self.open
            && let Some((lo, hi)) = current
        {
            self.min = lo;
            self.max = hi;
        }
        self.rendered_this_frame = false;
    }

    /// Inside the `show_with_context_menu` closure: render the "Y axis" section
    /// and return the user's choice, if any. `y_autoscale` is the plot's current
    /// flag (read before `show`).
    fn render(&mut self, ui: &mut egui::Ui, y_autoscale: bool) -> Option<YAxisChoice> {
        self.rendered_this_frame = true;
        let mut choice = None;
        ui.label("Y axis");
        let mut auto = y_autoscale;
        if ui.checkbox(&mut auto, "Auto-scale").changed() && auto {
            choice = Some(YAxisChoice::Autoscale);
            ui.close();
        }
        ui.horizontal(|ui| {
            ui.label("Min");
            ui.add(egui::DragValue::new(&mut self.min).speed(0.1));
            ui.label("Max");
            ui.add(egui::DragValue::new(&mut self.max).speed(0.1));
        });
        if ui.button("Set Y range").clicked() {
            choice = Some(YAxisChoice::SetRange(self.min, self.max));
            ui.close();
        }
        choice
    }

    /// After `show`: latch whether the menu rendered (was open) this frame.
    fn finish(&mut self) {
        self.open = self.rendered_this_frame;
    }
}

/// Render `plot` with the built-in context menu plus the pyqtgraph-style Y-axis
/// section, applying any committed choice afterward. Drop-in replacement for
/// `plot.show(ui)` in a widget that owns a [`YAxisMenu`].
pub(crate) fn show_with_y_axis_menu(
    plot: &mut Plot1D,
    menu: &mut YAxisMenu,
    ui: &mut egui::Ui,
) -> PlotResponse {
    let current = plot.get_graph_y_limits(YAxis::Left);
    let y_autoscale = plot.plot().y_autoscale();
    menu.sync(current);

    let mut choice = None;
    // `plot` and `menu` are distinct borrows, so the closure can hold `menu`
    // mutably while the plot is shown.
    let response = plot.show_with_context_menu(ui, |ui| {
        choice = menu.render(ui, y_autoscale);
    });
    menu.finish();

    match choice {
        Some(YAxisChoice::Autoscale) => enable_y_autoscale(plot),
        Some(YAxisChoice::SetRange(min, max)) => set_y_range(plot, min, max),
        None => {}
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_range_orders_and_expands_degenerate() {
        assert_eq!(normalize_range(2.0, 8.0), (2.0, 8.0));
        // max <= min expands to unit width rather than an empty/inverted range.
        assert_eq!(normalize_range(5.0, 5.0), (5.0, 6.0));
        assert_eq!(normalize_range(8.0, 2.0), (8.0, 9.0));
    }

    #[test]
    fn sync_seeds_fields_only_while_menu_closed() {
        let mut menu = YAxisMenu::new();
        // Closed: the fields track the live Y range.
        menu.sync(Some((10.0, 20.0)));
        assert_eq!((menu.min, menu.max), (10.0, 20.0));

        // Simulate the menu being open this frame (render ran), then finish.
        menu.rendered_this_frame = true;
        menu.finish();
        assert!(menu.open);

        // Open: a new live range must NOT clobber the user's in-progress edits.
        menu.sync(Some((100.0, 200.0)));
        assert_eq!((menu.min, menu.max), (10.0, 20.0));

        // Menu closed again (no render this frame) → finish clears `open`, and
        // the next sync resumes tracking the live range.
        menu.finish();
        assert!(!menu.open);
        menu.sync(Some((30.0, 40.0)));
        assert_eq!((menu.min, menu.max), (30.0, 40.0));
    }
}
