//! Plot markers: point / vertical-line / horizontal-line annotations drawn over
//! the data area (silx `addMarker`).
//!
//! The seven point-marker symbols (circle, point, pixel, plus, cross, diamond,
//! square) are placed across the plot, each labeled. A dashed vertical-line
//! marker and a solid horizontal-line marker show the line kinds with text. The
//! markers are egui-painter overlays, so they stay crisp and pixel-sized while
//! panning and zooming (`doc/design.md` §8).
//!
//! Run with: `cargo run --example plot_markers`

use eframe::egui;
use rsplot::{LineStyle, Marker, Plot, PlotView, Symbol, install};

struct MarkerApp {
    plot: Plot,
}

impl MarkerApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        let mut plot = Plot::new(0);
        plot.limits = (0.0, 8.0, 0.0, 6.0);
        plot.title = Some("plot markers (point / vline / hline)".to_owned());

        // One point marker per symbol, evenly spaced, each labeled.
        let symbols = [
            (Symbol::Circle, "o", egui::Color32::from_rgb(120, 180, 255)),
            (Symbol::Point, ".", egui::Color32::from_rgb(120, 255, 180)),
            (Symbol::Pixel, ",", egui::Color32::from_rgb(200, 200, 200)),
            (Symbol::Plus, "+", egui::Color32::from_rgb(255, 220, 120)),
            (Symbol::Cross, "x", egui::Color32::from_rgb(255, 150, 200)),
            (Symbol::Diamond, "d", egui::Color32::from_rgb(180, 160, 255)),
            (Symbol::Square, "s", egui::Color32::from_rgb(255, 130, 120)),
        ];
        for (i, (symbol, label, color)) in symbols.into_iter().enumerate() {
            let x = 1.0 + i as f64;
            plot.markers.push(
                Marker::point(x, 4.5)
                    .with_symbol(symbol)
                    .with_symbol_size(14.0)
                    .with_color(color)
                    .with_text(label),
            );
        }

        // A dashed vertical-line marker and a solid horizontal-line marker.
        plot.markers.push(
            Marker::vline(4.0)
                .with_color(egui::Color32::from_rgb(255, 200, 80))
                .with_line_style(LineStyle::Dashed)
                .with_text("x = 4"),
        );
        plot.markers.push(
            Marker::hline(2.0)
                .with_color(egui::Color32::from_rgb(120, 220, 255))
                .with_line_width(1.5)
                .with_text("y = 2")
                .with_bgcolor(egui::Color32::from_black_alpha(180)),
        );

        Self { plot }
    }
}

impl eframe::App for MarkerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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
        "rsplot · plot_markers",
        options,
        Box::new(|cc| Ok(Box::new(MarkerApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
