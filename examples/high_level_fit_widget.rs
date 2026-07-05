//! Fit Widget Example.
//!
//! Demonstrates the `FitWidget` together with `ItemsSelectionDialog` reuse: the
//! main window plots the candidate curves (the selected one drawn as the active
//! curve), and the dialog (configured single-select, curve and histogram only)
//! picks which one to fit. This mirrors silx `actions/fit.py`'s `_initFit`,
//! which fits a curve FROM a plot: it builds an `ItemsSelectionDialog`, calls
//! `setItemsSelectionMode(SingleSelection)` and
//! `setAvailableKinds(["curve", "histogram"])`, then runs `setData` on the
//! chosen item. The fit result (data + fitted curve) shows in the detached
//! Fit Widget window.
//!
//! Run with: `cargo run --example high_level_fit_widget`

use eframe::egui;
use rsplot::{
    FitWidget, ItemHandle, ItemsSelectionDialog, Plot1D, PlotItemKind, SelectableItem,
    SelectionMode,
};

/// One fittable dataset shown in the picker.
struct Dataset {
    label: &'static str,
    x: Vec<f64>,
    y: Vec<f64>,
}

fn gaussian(mu: f64, sigma: f64, a: f64, bg: f64) -> Dataset {
    let mut x = Vec::with_capacity(100);
    let mut y = Vec::with_capacity(100);
    for i in 0..100 {
        let xi = i as f64 * 0.1;
        // Simple deterministic pseudo-noise so the example needs no rng.
        let noise = ((i * 12345) % 100) as f64 / 100.0 - 0.5;
        let z = (xi - mu) / sigma;
        let yi = a * (-0.5 * z * z).exp() + bg + noise * 1.5;
        x.push(xi);
        y.push(yi);
    }
    Dataset { label: "", x, y }
}

struct FitApp {
    fit_widget: FitWidget,
    datasets: Vec<Dataset>,
    /// Main-window plot of the candidate curves (silx fits a curve FROM a
    /// plot); the selected dataset is drawn as the active curve.
    plot: Plot1D,
    /// Plot handle of each dataset's curve, indexed like `datasets`.
    curve_handles: Vec<ItemHandle>,
    /// Curve picker (silx `ItemsSelectionDialog`), single-select over curves.
    picker: ItemsSelectionDialog,
    /// Index of the dataset currently feeding the fit widget.
    fitted: Option<usize>,
}

impl FitApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let mut fit_widget = FitWidget::new(rs, 0);
        fit_widget.set_open(true);

        let datasets = vec![
            Dataset {
                label: "narrow peak",
                ..gaussian(5.0, 1.0, 10.0, 2.0)
            },
            Dataset {
                label: "wide peak",
                ..gaussian(6.0, 2.0, 6.0, 1.0)
            },
            Dataset {
                label: "tall peak",
                ..gaussian(4.0, 0.7, 14.0, 3.0)
            },
        ];

        let mut plot = Plot1D::new(rs, 1);
        plot.set_graph_title("Curves to fit (pick one on the left)");
        let colors = [
            egui::Color32::LIGHT_BLUE,
            egui::Color32::LIGHT_GREEN,
            egui::Color32::ORANGE,
        ];
        let curve_handles: Vec<ItemHandle> = datasets
            .iter()
            .zip(colors.iter().cycle())
            .map(|(d, &color)| plot.add_curve_with_legend(&d.x, &d.y, color, d.label))
            .collect();

        // Configure the dialog exactly as silx's fit tool does: only fittable
        // kinds offered, single selection, the first curve picked initially.
        let mut picker = ItemsSelectionDialog::new(
            datasets
                .iter()
                .enumerate()
                .map(|(i, d)| SelectableItem::new(d.label, PlotItemKind::Curve, i == 0))
                .collect(),
        );
        picker.set_available_kinds(&[PlotItemKind::Curve, PlotItemKind::Histogram]);
        picker.set_selection_mode(SelectionMode::Single);

        let mut app = Self {
            fit_widget,
            datasets,
            plot,
            curve_handles,
            picker,
            fitted: None,
        };
        app.sync_fitted_item();
        app
    }

    /// Feed the picker's currently selected dataset to the fit widget, if it
    /// changed (silx `_setFittedItem` → `FitWidget.setData`), and highlight it
    /// as the main plot's active curve.
    fn sync_fitted_item(&mut self) {
        // The picker is single-select, so at most one label comes back.
        let chosen = self
            .picker
            .selected_items()
            .next()
            .map(|it| it.label.to_string());
        let chosen_idx =
            chosen.and_then(|label| self.datasets.iter().position(|d| d.label == label));
        if chosen_idx != self.fitted {
            self.fitted = chosen_idx;
            self.plot
                .set_active_curve(chosen_idx.map(|i| self.curve_handles[i]));
            if let Some(i) = chosen_idx {
                let d = &self.datasets[i];
                self.fit_widget.set_data(&d.x, &d.y);
            }
        }
    }
}

impl eframe::App for FitApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::left("fit_curve_picker")
            .resizable(true)
            .default_size(220.0)
            .show_inside(ui, |ui| {
                ui.heading("Curve to fit");
                self.picker.ui(ui);
            });
        // Apply any selection change to the fit widget's data.
        self.sync_fitted_item();

        egui::CentralPanel::default().show_inside(ui, |ui| {
            if !self.fit_widget.is_open() && ui.button("Open Fit Widget").clicked() {
                self.fit_widget.set_open(true);
            }
            // Main plot of the candidate curves; the fit widget renders into
            // its own detached window beside this one.
            self.plot.show(ui);
            self.fit_widget.show(ui.ctx());
        });
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "rsplot: Fit Widget",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(FitApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
