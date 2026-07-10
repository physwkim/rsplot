//! Picking example: highlight the curve vertex nearest the pointer.
//!
//! The app keeps a CPU copy of the curve points (the GPU upload is a render
//! mirror). `PlotView::show` returns the display `Transform`; on hover the app
//! calls `nearest_point` to find the closest vertex within a pixel threshold and
//! draws a highlight ring + an index/coordinate readout (`doc/design.md`
//! §13 C2).
//!
//! Run with: `cargo run --example picking`

use eframe::egui;
use egui::{Align2, Color32, FontId, Stroke, vec2};
use rsplot::{CurveData, Plot, PlotView, Symbol, install, nearest_point, set_curve};

const N: usize = 24;

fn build_points() -> Vec<(f64, f64)> {
    (0..N)
        .map(|i| {
            let t = i as f64 / (N - 1) as f64;
            (t * 10.0, 5.0 + 3.0 * (t * std::f64::consts::TAU).sin())
        })
        .collect()
}

struct PickingApp {
    plot: Plot,
    points: Vec<(f64, f64)>,
}

impl PickingApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        let points = build_points();
        let x = points.iter().map(|p| p.0).collect();
        let y = points.iter().map(|p| p.1).collect();
        let curve = CurveData::new(x, y, egui::Color32::from_rgb(120, 180, 255))
            .with_width(1.5)
            .with_symbol(Symbol::Circle)
            .with_marker_size(9.0);
        set_curve(render_state, 0, &curve);

        let mut plot = Plot::new(0);
        plot.limits = (-0.5, 10.5, 0.0, 9.0);

        Self { plot, points }
    }
}

impl eframe::App for PickingApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            let out = PlotView::new().show(ui, &mut self.plot);
            // Highlight the nearest vertex within 14 px of the pointer.
            if let Some(cursor) = out.response.hover_pos()
                && let Some(pick) = nearest_point(&self.points, &out.transform, cursor, 14.0)
            {
                let painter = ui.painter();
                let c = out.transform.data_to_pixel(pick.x, pick.y);
                painter.circle_stroke(c, 9.0, Stroke::new(2.0_f32, Color32::WHITE));
                painter.text(
                    c + vec2(12.0, -12.0),
                    Align2::LEFT_BOTTOM,
                    format!("#{}  ({:.2}, {:.2})", pick.index, pick.x, pick.y),
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
        "rsplot · picking",
        options,
        Box::new(|cc| Ok(Box::new(PickingApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
