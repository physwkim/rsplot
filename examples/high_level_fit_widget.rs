//! Fit Widget Example.
//!
//! Demonstrates the `FitWidget` which allows performing simple curve fits.
//!
//! Run with: `cargo run --example high_level_fit_widget`

use eframe::egui;
use egui_silx::FitWidget;

struct FitApp {
    fit_widget: FitWidget,
}

impl FitApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let mut fit_widget = FitWidget::new(rs, 0);
        fit_widget.set_open(true);

        // Generate some noisy Gaussian data
        let mut x = Vec::with_capacity(100);
        let mut y = Vec::with_capacity(100);

        for i in 0..100 {
            let xi = i as f64 * 0.1;
            let mu = 5.0;
            let sigma = 1.0;
            let a = 10.0;
            let bg = 2.0;

            // Simple pseudo-random noise
            let noise = ((i * 12345) % 100) as f64 / 100.0 - 0.5;

            let z = (xi - mu) / sigma;
            let yi = a * (-0.5 * z * z).exp() + bg + noise * 1.5;

            x.push(xi);
            y.push(yi);
        }

        fit_widget.set_data(&x, &y);

        Self { fit_widget }
    }
}

impl eframe::App for FitApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.fit_widget.is_open() {
            self.fit_widget.show(ui.ctx());
        } else {
            if ui.button("Open Fit Widget").clicked() {
                self.fit_widget.set_open(true);
            }
        }
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "egui-silx: Fit Widget",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(FitApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
