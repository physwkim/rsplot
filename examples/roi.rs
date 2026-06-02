//! ROI example: a draggable rectangular region and a horizontal band over a
//! curve.
//!
//! The plot owns a `Vec<Roi>`; `PlotView` draws each region with translucent
//! fill, a border, and square edge handles, and lets the pointer drag an edge.
//! When an edge moves, `PlotView::show` reports the changed index via
//! `PlotResponse::roi_changed`; this app prints the new bounds as a readout
//! (`doc/design.md` §13 C3).
//!
//! Run with: `cargo run --example roi`

use eframe::egui;
use egui::{Align2, Color32, FontId, pos2};
use egui_silx::{CurveData, Plot, PlotView, Roi, install, set_curve};

const N: usize = 200;

fn build_points() -> (Vec<f64>, Vec<f64>) {
    let x: Vec<f64> = (0..N).map(|i| i as f64 / (N - 1) as f64 * 10.0).collect();
    let y: Vec<f64> = x
        .iter()
        .map(|&t| 5.0 + 3.0 * (t * std::f64::consts::TAU * 0.3).sin())
        .collect();
    (x, y)
}

struct RoiApp {
    plot: Plot,
}

impl RoiApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        let (x, y) = build_points();
        let curve = CurveData::new(x, y, Color32::from_rgb(120, 180, 255)).with_width(1.5);
        set_curve(render_state, 0, &curve);

        let mut plot = Plot::new(0);
        plot.limits = (-0.5, 10.5, 0.0, 9.0);
        plot.rois = vec![
            Roi::Rect {
                x: (2.0, 5.0),
                y: (3.0, 7.0),
            },
            Roi::HRange { y: (1.0, 2.0) },
        ];

        Self { plot }
    }
}

impl eframe::App for RoiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            let out = PlotView::new().show(ui, &mut self.plot);
            // Report the bounds of whichever region's edge is being dragged.
            if let Some(i) = out.roi_changed {
                let text = match self.plot.rois[i] {
                    Roi::Rect { x, y } => {
                        format!(
                            "rect #{i}  x=[{:.2}, {:.2}]  y=[{:.2}, {:.2}]",
                            x.0, x.1, y.0, y.1
                        )
                    }
                    Roi::HRange { y } => format!("hrange #{i}  y=[{:.2}, {:.2}]", y.0, y.1),
                    Roi::VRange { x } => format!("vrange #{i}  x=[{:.2}, {:.2}]", x.0, x.1),
                    Roi::Point { x, y } => format!("point #{i}  ({x:.2}, {y:.2})"),
                    Roi::Line { start, end } => format!(
                        "line #{i}  ({:.2}, {:.2}) → ({:.2}, {:.2})",
                        start.0, start.1, end.0, end.1
                    ),
                    Roi::Polygon { ref vertices } => {
                        format!("polygon #{i}  {} vertices", vertices.len())
                    }
                    Roi::Cross { center } => {
                        format!("cross #{i}  ({:.2}, {:.2})", center.0, center.1)
                    }
                    Roi::Circle { center, radius } => {
                        format!(
                            "circle #{i}  c=({:.2}, {:.2})  r={radius:.2}",
                            center.0, center.1
                        )
                    }
                    Roi::Ellipse { center, radii } => format!(
                        "ellipse #{i}  c=({:.2}, {:.2})  r=({:.2}, {:.2})",
                        center.0, center.1, radii.0, radii.1
                    ),
                };
                ui.painter().text(
                    pos2(ui.max_rect().left() + 12.0, ui.max_rect().top() + 12.0),
                    Align2::LEFT_TOP,
                    text,
                    FontId::proportional(13.0),
                    Color32::WHITE,
                );
            }
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "egui-silx · roi",
        options,
        Box::new(|cc| Ok(Box::new(RoiApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
