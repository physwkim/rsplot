//! Shapes: polygon / rectangle / polyline / hline / vline overlays drawn over
//! the data area (silx `addShape`).
//!
//! A filled translucent triangle (polygon), a dashed-outline rectangle, an open
//! polyline, and full-span horizontal/vertical lines demonstrate the shape kinds,
//! fill, line styles, and the gap-color fill on a dashed outline. Shapes are
//! egui-painter overlays clipped to the data area (`doc/design.md` §8).
//!
//! Run with: `cargo run --example shapes`

use eframe::egui;
use rsplot::{LineStyle, Plot, PlotView, Shape, install};

struct ShapeApp {
    plot: Plot,
}

impl ShapeApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        plot.title = Some("shapes (polygon / rectangle / polyline / lines)".to_owned());

        // A filled translucent triangle with a solid outline.
        plot.shapes.push(
            Shape::polygon(vec![1.0, 3.0, 2.0], vec![1.0, 1.0, 4.0])
                .with_fill(true)
                .with_color(egui::Color32::from_rgba_unmultiplied(120, 180, 255, 80))
                .with_line_width(1.5),
        );

        // A rectangle with a dashed outline whose gaps are filled with a second
        // color (silx gapcolor), unfilled interior.
        plot.shapes.push(
            Shape::rectangle(5.0, 5.0, 9.0, 8.0)
                .with_color(egui::Color32::from_rgb(255, 200, 80))
                .with_line_style(LineStyle::Dashed)
                .with_line_width(2.0)
                .with_gap_color(egui::Color32::from_rgb(70, 50, 10)),
        );

        // An open polyline (no fill, no closing segment).
        plot.shapes.push(
            Shape::polyline(vec![1.0, 2.5, 4.0, 5.5], vec![6.0, 9.0, 6.0, 9.0])
                .with_color(egui::Color32::from_rgb(120, 255, 180))
                .with_line_width(1.5),
        );

        // A horizontal and a vertical full-span line.
        plot.shapes.push(
            Shape::hlines(vec![2.5])
                .with_color(egui::Color32::from_rgb(255, 130, 200))
                .with_line_style(LineStyle::Dotted),
        );
        plot.shapes.push(
            Shape::vlines(vec![7.0])
                .with_color(egui::Color32::from_rgb(180, 160, 255))
                .with_line_style(LineStyle::DashDot),
        );

        Self { plot }
    }
}

impl eframe::App for ShapeApp {
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
        "rsplot · shapes",
        options,
        Box::new(|cc| Ok(Box::new(ShapeApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
