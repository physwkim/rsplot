use egui::Color32;
use egui_wgpu::RenderState;

use crate::core::backend::ItemHandle;
use crate::core::fitting::{FitFunction, FitResult, GaussianEstimateFit, LinearFit};
use crate::core::plot::PlotId;
use crate::render::gpu_curve::CurveData;
use crate::widget::high_level::Plot1D;

/// A window widget to perform curve fitting on 1D data and display the result.
pub struct FitWidget {
    plot: Plot1D,
    data_handle: Option<ItemHandle>,
    fit_handle: Option<ItemHandle>,
    window_id: egui::Id,
    open: bool,

    // Data
    x_data: Vec<f64>,
    y_data: Vec<f64>,

    // Fit state
    selected_function_idx: usize,
    fit_result: Option<FitResult>,
}

impl FitWidget {
    /// Create a new FitWidget with a backing Plot1D.
    pub fn new(render_state: &RenderState, plot_id: PlotId) -> Self {
        let mut plot = Plot1D::new(render_state, plot_id);
        plot.set_graph_title("Fit Result");

        Self {
            plot,
            data_handle: None,
            fit_handle: None,
            window_id: egui::Id::new(plot_id).with("fit_widget"),
            open: false,
            x_data: Vec::new(),
            y_data: Vec::new(),
            selected_function_idx: 0,
            fit_result: None,
        }
    }

    /// Is the window currently open?
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open or close the window.
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    /// Set the data to fit.
    pub fn set_data(&mut self, x: &[f64], y: &[f64]) {
        self.x_data = x.to_vec();
        self.y_data = y.to_vec();

        let curve = CurveData::new(self.x_data.clone(), self.y_data.clone(), Color32::BLUE);
        if let Some(handle) = self.data_handle {
            self.plot.update_curve_data(handle, &curve);
        } else {
            self.data_handle = Some(self.plot.add_curve_with_legend(
                &self.x_data,
                &self.y_data,
                Color32::BLUE,
                "Data",
            ));
        }

        // Clear previous fit
        if let Some(handle) = self.fit_handle {
            self.plot.remove(handle);
            self.fit_handle = None;
        }
        self.fit_result = None;
        self.plot.reset_zoom_to_data();
    }

    /// Perform the fit using the currently selected function.
    pub fn perform_fit(&mut self) {
        if self.x_data.is_empty() || self.y_data.is_empty() {
            return;
        }

        let functions: [&dyn FitFunction; 2] = [&LinearFit, &GaussianEstimateFit];
        let func = functions[self.selected_function_idx];

        if let Some(result) = func.fit(&self.x_data, &self.y_data) {
            let curve = CurveData::new(self.x_data.clone(), result.y_fit.clone(), Color32::RED);
            if let Some(handle) = self.fit_handle {
                self.plot.update_curve_data(handle, &curve);
            } else {
                self.fit_handle = Some(self.plot.add_curve_with_legend(
                    &self.x_data,
                    &result.y_fit,
                    Color32::RED,
                    "Fit",
                ));
            }
            self.fit_result = Some(result);
        } else {
            // Fit failed
            self.fit_result = None;
            if let Some(handle) = self.fit_handle {
                self.plot.remove(handle);
                self.fit_handle = None;
            }
        }
    }

    /// Show the fit widget using the given egui context.
    pub fn show(&mut self, ctx: &egui::Context) {
        let mut open = self.open;
        egui::Window::new("Fit Widget")
            .id(self.window_id)
            .open(&mut open)
            .default_size([600.0, 400.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Fit Function:");
                    egui::ComboBox::from_id_salt("fit_function_combo")
                        .selected_text(match self.selected_function_idx {
                            0 => "Linear",
                            1 => "Gaussian (Estimate)",
                            _ => "Unknown",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.selected_function_idx, 0, "Linear");
                            ui.selectable_value(
                                &mut self.selected_function_idx,
                                1,
                                "Gaussian (Estimate)",
                            );
                        });

                    if ui.button("Fit").clicked() {
                        self.perform_fit();
                    }
                });

                ui.separator();

                // Show fit parameters if available
                if let Some(result) = &self.fit_result {
                    ui.group(|ui| {
                        ui.heading("Fit Parameters");
                        egui::Grid::new("fit_params_grid")
                            .num_columns(2)
                            .show(ui, |ui| {
                                for (name, val) in
                                    result.param_names.iter().zip(result.parameters.iter())
                                {
                                    ui.label(name);
                                    ui.label(format!("{:.6}", val));
                                    ui.end_row();
                                }
                            });
                    });
                    ui.separator();
                }

                // Show the plot
                self.plot.show(ui);
            });
        self.open = open;
    }
}
