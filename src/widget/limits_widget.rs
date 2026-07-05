use crate::core::transform::YAxis;
use crate::widget::high_level::Plot2D;

/// A widget for interactively setting the plot limits, scaling, and grid options.
pub struct LimitsWidget {
    win: crate::widget::detached::DetachedWindow,
    pub open: bool,

    // Staged limits. When the user types/drags values, they update these,
    // and then apply them to the plot (or auto-apply if configured).
    x_min: f64,
    x_max: f64,
    y_min: f64,
    y_max: f64,

    // Options
    x_log: bool,
    y_log: bool,
    grid: bool,

    initialized: bool,
}

impl Default for LimitsWidget {
    fn default() -> Self {
        Self {
            win: crate::widget::detached::DetachedWindow::new(
                egui::Id::new("limits_widget"),
                egui::vec2(300.0, 360.0),
            ),
            open: false,
            x_min: 0.0,
            x_max: 1.0,
            y_min: 0.0,
            y_max: 1.0,
            x_log: false,
            y_log: false,
            grid: true,
            initialized: false,
        }
    }
}

impl LimitsWidget {
    /// Create a new LimitsWidget.
    pub fn new() -> Self {
        Self::default()
    }

    /// Synchronize the widget state with the current plot state.
    fn sync_from_plot(&mut self, plot: &Plot2D) {
        let (x_min, x_max) = plot.get_graph_x_limits();
        self.x_min = x_min;
        self.x_max = x_max;

        if let Some((y_min, y_max)) = plot.get_graph_y_limits(YAxis::Left) {
            self.y_min = y_min;
            self.y_max = y_max;
        }

        // Note: PlotWidget doesn't easily expose is_x_log getter directly in high_level yet,
        // but it could. For now, we assume this widget drives the settings, or we
        // just let the user toggle it. If they toggle it here, it pushes to plot.
    }

    /// Show the Limits window.
    pub fn show(&mut self, ctx: &egui::Context, plot: &mut Plot2D) {
        if !self.initialized {
            self.sync_from_plot(plot);
            self.initialized = true;
        }

        if !self.open {
            return;
        }
        let mut apply = false;
        let pos = self.win.position(ctx);
        let id = self.win.id();
        let size = self.win.size();

        let signals = crate::widget::detached::show_detached(
            ctx,
            id,
            "Axis & Limits Settings",
            size,
            pos,
            |ui| {
                ui.group(|ui| {
                    ui.heading("X Axis");
                    ui.horizontal(|ui| {
                        ui.label("Min:");
                        if ui
                            .add(egui::DragValue::new(&mut self.x_min).speed(0.1))
                            .changed()
                        {
                            apply = true;
                        }
                        ui.label("Max:");
                        if ui
                            .add(egui::DragValue::new(&mut self.x_max).speed(0.1))
                            .changed()
                        {
                            apply = true;
                        }
                    });
                    if ui.checkbox(&mut self.x_log, "Log Scale").changed() {
                        plot.set_graph_x_log(self.x_log);
                    }
                });

                ui.group(|ui| {
                    ui.heading("Y Axis");
                    ui.horizontal(|ui| {
                        ui.label("Min:");
                        if ui
                            .add(egui::DragValue::new(&mut self.y_min).speed(0.1))
                            .changed()
                        {
                            apply = true;
                        }
                        ui.label("Max:");
                        if ui
                            .add(egui::DragValue::new(&mut self.y_max).speed(0.1))
                            .changed()
                        {
                            apply = true;
                        }
                    });
                    if ui.checkbox(&mut self.y_log, "Log Scale").changed() {
                        plot.set_graph_y_log(self.y_log);
                    }
                });

                ui.separator();

                if ui.checkbox(&mut self.grid, "Show Grid").changed() {
                    // Match silx's GridAction gridMode="both": grid on = major +
                    // minor (see high_level.rs toolbar grid button).
                    plot.set_graph_grid_mode(if self.grid {
                        crate::core::plot::GraphGrid::MajorAndMinor
                    } else {
                        crate::core::plot::GraphGrid::None
                    });
                }

                ui.horizontal(|ui| {
                    if ui.button("Sync from Plot").clicked() {
                        self.sync_from_plot(plot);
                    }
                });
            },
        );

        self.win.apply_signals(&signals, &mut self.open);

        if apply {
            // Apply limits to plot
            plot.set_graph_x_limits(self.x_min, self.x_max);
            plot.set_graph_y_limits(self.y_min, self.y_max, YAxis::Left);
        }
    }
}
