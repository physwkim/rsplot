use egui::Color32;
use egui_wgpu::RenderState;

use crate::core::backend::ItemHandle;
use crate::core::fitting::{
    FitFunction, FitResult, GaussianEstimateFit, IterativeFit, IterativeFitResult, LinearFit,
    PeakModel,
};
use crate::core::plot::PlotId;
use crate::render::gpu_curve::CurveData;
use crate::widget::high_level::Plot1D;

/// Format a fitted parameter value together with its estimated error as
/// `value ± error`, mirroring the silx `FitWidget` results table which shows a
/// value and its sigma (the square root of the covariance diagonal).
///
/// A non-finite error is rendered without the `±` term (silx leaves the
/// uncertainty blank when it cannot be computed).
pub fn format_param_value_error(value: f64, error: f64) -> String {
    if error.is_finite() {
        format!("{value:.6} ± {error:.6}")
    } else {
        format!("{value:.6}")
    }
}

/// Format the reduced chi-square goodness-of-fit metric for the results table
/// (silx `FitWidget` shows `chisq` / reduced chi-square). `None` (non-positive
/// degrees of freedom) renders as `N/A`.
pub fn format_reduced_chisq(reduced_chisq: Option<f64>) -> String {
    match reduced_chisq {
        Some(rc) if rc.is_finite() => format!("{rc:.6}"),
        _ => "N/A".to_string(),
    }
}

/// The selectable fit model in [`FitWidget`].
///
/// The first two variants preserve the original analytical fits (Linear and
/// the analytical Gaussian estimate); the remaining variants drive the
/// iterative Levenberg-Marquardt path with a results table that includes
/// per-parameter errors and reduced chi-square.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitModelChoice {
    /// Analytical linear fit (`LinearFit`).
    Linear,
    /// Analytical Gaussian estimate (`GaussianEstimateFit`).
    GaussianEstimate,
    /// Iterative Gaussian (height parameterisation).
    IterativeGaussian,
    /// Iterative Gaussian (area parameterisation).
    IterativeGaussianArea,
    /// Iterative Lorentzian.
    IterativeLorentzian,
    /// Iterative pseudo-Voigt.
    IterativePseudoVoigt,
}

impl FitModelChoice {
    /// All choices, in display order.
    pub const ALL: [FitModelChoice; 6] = [
        FitModelChoice::Linear,
        FitModelChoice::GaussianEstimate,
        FitModelChoice::IterativeGaussian,
        FitModelChoice::IterativeGaussianArea,
        FitModelChoice::IterativeLorentzian,
        FitModelChoice::IterativePseudoVoigt,
    ];

    /// Display name for the combo box.
    pub fn label(self) -> &'static str {
        match self {
            FitModelChoice::Linear => "Linear",
            FitModelChoice::GaussianEstimate => "Gaussian (Estimate)",
            FitModelChoice::IterativeGaussian => "Gaussian (Iterative)",
            FitModelChoice::IterativeGaussianArea => "Gaussian Area (Iterative)",
            FitModelChoice::IterativeLorentzian => "Lorentzian (Iterative)",
            FitModelChoice::IterativePseudoVoigt => "Pseudo-Voigt (Iterative)",
        }
    }

    /// The [`PeakModel`] this choice maps to, if it is one of the iterative
    /// peak models.
    pub fn peak_model(self) -> Option<PeakModel> {
        match self {
            FitModelChoice::IterativeGaussian => Some(PeakModel::Gaussian),
            FitModelChoice::IterativeGaussianArea => Some(PeakModel::GaussianArea),
            FitModelChoice::IterativeLorentzian => Some(PeakModel::Lorentzian),
            FitModelChoice::IterativePseudoVoigt => Some(PeakModel::PseudoVoigt),
            FitModelChoice::Linear | FitModelChoice::GaussianEstimate => None,
        }
    }
}

/// A window widget to perform curve fitting on 1D data and display the result.
pub struct FitWidget {
    plot: Plot1D,
    data_handle: Option<ItemHandle>,
    fit_handle: Option<ItemHandle>,
    win: crate::widget::detached::DetachedWindow,
    open: bool,

    // Data
    x_data: Vec<f64>,
    y_data: Vec<f64>,

    // Fit state
    selected_function_idx: usize,
    fit_result: Option<FitResult>,

    // Iterative-fit state (Wave 5, additive).
    selected_choice: FitModelChoice,
    iterative_result: Option<IterativeFitResult>,
    /// Optional fit range `[xmin, xmax]`; `None` fits the whole curve
    /// (silx `FitWidget` xmin/xmax).
    fit_range: Option<(f64, f64)>,
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
            win: crate::widget::detached::DetachedWindow::new(
                egui::Id::new(plot_id).with("fit_widget"),
                egui::vec2(600.0, 400.0),
            ),
            open: false,
            x_data: Vec::new(),
            y_data: Vec::new(),
            selected_function_idx: 0,
            fit_result: None,
            selected_choice: FitModelChoice::Linear,
            iterative_result: None,
            fit_range: None,
        }
    }

    /// Set the fit range `[xmin, xmax]`; only points inside it are fitted
    /// (silx `FitWidget` xmin/xmax). Pass `None` to fit the whole curve.
    pub fn set_fit_range(&mut self, range: Option<(f64, f64)>) {
        self.fit_range = range;
    }

    /// The currently selected fit model choice.
    pub fn selected_choice(&self) -> FitModelChoice {
        self.selected_choice
    }

    /// Set the selected fit model choice.
    pub fn set_selected_choice(&mut self, choice: FitModelChoice) {
        self.selected_choice = choice;
    }

    /// The most recent iterative-fit result (covariance / chi-square), if the
    /// last successful fit used an iterative peak model.
    pub fn iterative_result(&self) -> Option<&IterativeFitResult> {
        self.iterative_result.as_ref()
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
        self.iterative_result = None;
        self.plot.reset_zoom_to_data();
    }

    /// Restrict the data to the configured fit range, if any. Returns owned
    /// `(xs, ys)` of the in-range points (silx `FitWidget` xmin/xmax).
    fn ranged_data(&self) -> (Vec<f64>, Vec<f64>) {
        match self.fit_range {
            Some((xmin, xmax)) => {
                let (lo, hi) = if xmin <= xmax {
                    (xmin, xmax)
                } else {
                    (xmax, xmin)
                };
                let mut xs = Vec::new();
                let mut ys = Vec::new();
                for (&xi, &yi) in self.x_data.iter().zip(self.y_data.iter()) {
                    if xi >= lo && xi <= hi {
                        xs.push(xi);
                        ys.push(yi);
                    }
                }
                (xs, ys)
            }
            None => (self.x_data.clone(), self.y_data.clone()),
        }
    }

    /// Perform the fit using the currently selected [`FitModelChoice`].
    ///
    /// Iterative peak models are refined with Levenberg-Marquardt and populate
    /// the results table (per-parameter error + reduced chi-square); the
    /// analytical Linear / Gaussian-estimate choices keep their original
    /// behaviour. Honors the configured fit range.
    pub fn perform_fit_choice(&mut self) {
        if self.x_data.is_empty() || self.y_data.is_empty() {
            return;
        }
        let (xs, ys) = self.ranged_data();
        // The fit curve is drawn over the in-range points so the displayed fit
        // matches what was fitted.
        let result: Option<FitResult> = match self.selected_choice {
            FitModelChoice::Linear => {
                self.iterative_result = None;
                LinearFit.fit(&xs, &ys)
            }
            FitModelChoice::GaussianEstimate => {
                self.iterative_result = None;
                GaussianEstimateFit.fit(&xs, &ys)
            }
            choice => {
                // One of the iterative peak models.
                let model = IterativeFit::new(
                    choice
                        .peak_model()
                        .expect("non-analytical choice has a peak model"),
                );
                match model.fit_full(&xs, &ys) {
                    Some(ir) => {
                        let fit = ir.fit.clone();
                        self.iterative_result = Some(ir);
                        Some(fit)
                    }
                    None => {
                        self.iterative_result = None;
                        None
                    }
                }
            }
        };

        match result {
            Some(result) => {
                let curve = CurveData::new(xs.clone(), result.y_fit.clone(), Color32::RED);
                if let Some(handle) = self.fit_handle {
                    self.plot.update_curve_data(handle, &curve);
                } else {
                    self.fit_handle = Some(self.plot.add_curve_with_legend(
                        &xs,
                        &result.y_fit,
                        Color32::RED,
                        "Fit",
                    ));
                }
                self.fit_result = Some(result);
            }
            None => {
                self.fit_result = None;
                self.iterative_result = None;
                if let Some(handle) = self.fit_handle {
                    self.plot.remove(handle);
                    self.fit_handle = None;
                }
            }
        }
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
        if !self.open {
            return;
        }
        let pos = self.win.position(ctx);
        let id = self.win.id();
        let size = self.win.size();
        let signals =
            crate::widget::detached::show_detached(ctx, id, "Fit Widget", size, pos, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Fit Function:");
                    egui::ComboBox::from_id_salt("fit_function_combo")
                        .selected_text(self.selected_choice.label())
                        .show_ui(ui, |ui| {
                            for choice in FitModelChoice::ALL {
                                ui.selectable_value(
                                    &mut self.selected_choice,
                                    choice,
                                    choice.label(),
                                );
                            }
                        });

                    if ui.button("Fit").clicked() {
                        self.perform_fit_choice();
                    }
                });

                ui.separator();

                // Show fit parameters if available. Iterative fits add a per
                // parameter estimated error column and a reduced chi-square row
                // (silx FitWidget results table).
                if let Some(result) = &self.fit_result {
                    let errors: Option<Vec<f64>> =
                        self.iterative_result.as_ref().map(|ir| ir.std_errors());
                    ui.group(|ui| {
                        ui.heading("Fit Parameters");
                        egui::Grid::new("fit_params_grid")
                            .num_columns(3)
                            .show(ui, |ui| {
                                ui.label("Parameter");
                                ui.label("Value");
                                ui.label("Error");
                                ui.end_row();
                                for (i, (name, val)) in result
                                    .param_names
                                    .iter()
                                    .zip(result.parameters.iter())
                                    .enumerate()
                                {
                                    ui.label(name);
                                    ui.label(format!("{val:.6}"));
                                    match errors.as_ref().and_then(|e| e.get(i)) {
                                        Some(&err) if err.is_finite() => {
                                            ui.label(format!("{err:.6}"));
                                        }
                                        _ => {
                                            ui.label("");
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                        if let Some(ir) = &self.iterative_result {
                            ui.separator();
                            ui.horizontal(|ui| {
                                ui.label("Reduced chi-square:");
                                ui.label(format_reduced_chisq(ir.reduced_chisq()));
                            });
                        }
                    });
                    ui.separator();
                }

                // Show the plot
                self.plot.show(ui);
            });
        self.win.apply_signals(&signals, &mut self.open);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::fitting::{IterativeFit, LeastSqResult, PeakModel};

    #[test]
    fn format_value_error_with_finite_error() {
        assert_eq!(
            format_param_value_error(1.234_567_8, 0.012_345_6),
            "1.234568 ± 0.012346"
        );
    }

    #[test]
    fn format_value_error_with_nonfinite_error_drops_pm() {
        let s = format_param_value_error(2.5, f64::NAN);
        assert_eq!(s, "2.500000");
        assert!(!s.contains('±'));
    }

    #[test]
    fn format_reduced_chisq_some_and_none() {
        assert_eq!(format_reduced_chisq(Some(0.5)), "0.500000");
        assert_eq!(format_reduced_chisq(None), "N/A");
        assert_eq!(format_reduced_chisq(Some(f64::INFINITY)), "N/A");
    }

    #[test]
    fn error_extraction_from_covariance_diagonal() {
        // The results table errors come from sqrt(diag(covariance)).
        let res = LeastSqResult {
            parameters: vec![1.0, 2.0],
            covariance: vec![vec![9.0, 0.0], vec![0.0, 25.0]],
            chisq: 0.0,
            reduced_chisq: Some(0.0),
            niter: 1,
            nfev: 1,
        };
        let errs = res.std_errors();
        assert!((errs[0] - 3.0).abs() < 1e-12);
        assert!((errs[1] - 5.0).abs() < 1e-12);
        // And formatting them.
        assert_eq!(
            format_param_value_error(res.parameters[0], errs[0]),
            "1.000000 ± 3.000000"
        );
    }

    #[test]
    fn peak_model_mapping_for_iterative_choices() {
        assert_eq!(
            FitModelChoice::IterativeGaussian.peak_model(),
            Some(PeakModel::Gaussian)
        );
        assert_eq!(
            FitModelChoice::IterativeGaussianArea.peak_model(),
            Some(PeakModel::GaussianArea)
        );
        assert_eq!(
            FitModelChoice::IterativeLorentzian.peak_model(),
            Some(PeakModel::Lorentzian)
        );
        assert_eq!(
            FitModelChoice::IterativePseudoVoigt.peak_model(),
            Some(PeakModel::PseudoVoigt)
        );
        assert_eq!(FitModelChoice::Linear.peak_model(), None);
        assert_eq!(FitModelChoice::GaussianEstimate.peak_model(), None);
    }

    #[test]
    fn all_choices_listed_once_in_order() {
        assert_eq!(FitModelChoice::ALL.len(), 6);
        assert_eq!(FitModelChoice::ALL[0], FitModelChoice::Linear);
        assert_eq!(FitModelChoice::ALL[5], FitModelChoice::IterativePseudoVoigt);
    }

    #[test]
    fn iterative_fit_result_table_has_one_error_per_param() {
        // A clean gaussian; the per-parameter error vector must line up with
        // the parameter vector so the results table renders one error per row.
        let xs: Vec<f64> = (0..201).map(|i| i as f64 * 0.1).collect();
        let ys = crate::core::fitting::gaussian_model(&xs, &[5.0, 10.0, 2.0, 0.5]);
        let ir = IterativeFit::new(PeakModel::Gaussian)
            .fit_full(&xs, &ys)
            .unwrap();
        assert_eq!(ir.fit.parameters.len(), ir.std_errors().len());
        assert_eq!(ir.fit.param_names.len(), ir.fit.parameters.len());
    }
}
