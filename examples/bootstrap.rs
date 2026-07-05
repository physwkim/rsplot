//! Bootstrap example: an eframe window whose central panel hosts an empty
//! `PlotView`. Verifies that the wgpu render state is available, that
//! `WgpuResources` is installed, and that the data rect is cleared on the GPU.
//!
//! Run with: `cargo run --example bootstrap`

use eframe::egui;
use rsplot::{Plot, PlotView, install};

struct BootstrapApp {
    plot: Plot,
}

impl BootstrapApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // The wgpu render state is only present when eframe uses the wgpu renderer.
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        Self { plot: Plot::new(0) }
    }
}

impl eframe::App for BootstrapApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // The root `ui` has no margin/background; wrap it in a CentralPanel.
        egui::CentralPanel::default().show_inside(ui, |ui| {
            PlotView::new().show(ui, &mut self.plot);
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "rsplot · bootstrap",
        options,
        Box::new(|cc| Ok(Box::new(BootstrapApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
