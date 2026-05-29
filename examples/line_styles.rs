//! Line styles: solid, dashed, dash-dot, dotted, a dashed line with a gap-fill
//! color, and a "no line" curve that shows only its markers (silx `linestyle`
//! and `gapcolor`).
//!
//! Each curve is a sine offset vertically so the stroke patterns are easy to
//! compare. Dash lengths are in physical pixels and scale with the line width,
//! and stay continuous across the curve as you pan and zoom.
//!
//! Run with: `cargo run --example line_styles`

use eframe::egui;
use egui::Color32;
use egui_silx::{CurveData, LineStyle, Plot, PlotWidget, Symbol, install, set_curves};

const T_MAX: f64 = std::f64::consts::TAU;

fn sine(offset: f64) -> (Vec<f64>, Vec<f64>) {
    let n = 400;
    let x: Vec<f64> = (0..n).map(|i| i as f64 / (n - 1) as f64 * T_MAX).collect();
    let y: Vec<f64> = x.iter().map(|&t| t.sin() + offset).collect();
    (x, y)
}

fn build() -> Vec<CurveData> {
    let rows: [(f64, LineStyle, Color32); 4] = [
        (5.0, LineStyle::Solid, Color32::from_rgb(120, 200, 255)),
        (3.0, LineStyle::Dashed, Color32::from_rgb(255, 180, 90)),
        (1.0, LineStyle::DashDot, Color32::from_rgb(160, 255, 140)),
        (-1.0, LineStyle::Dotted, Color32::from_rgb(255, 130, 200)),
    ];
    let mut curves: Vec<CurveData> = rows
        .into_iter()
        .map(|(off, style, color)| {
            let (x, y) = sine(off);
            CurveData::new(x, y, color)
                .with_width(2.5)
                .with_line_style(style)
        })
        .collect();

    // A dashed line whose gaps are filled with a dim color (silx gapcolor),
    // giving a two-tone dash that stays visible on any background.
    let (x, y) = sine(-3.0);
    curves.push(
        CurveData::new(x, y, Color32::from_rgb(255, 90, 90))
            .with_width(3.0)
            .with_line_style(LineStyle::Dashed)
            .with_gap_color(Color32::from_rgb(60, 60, 70)),
    );

    // No line, markers only (linestyle ' ').
    let (x, y) = sine(-5.0);
    curves.push(
        CurveData::new(x, y, Color32::from_rgb(220, 220, 120))
            .with_line_style(LineStyle::None)
            .with_symbol(Symbol::Circle)
            .with_marker_size(5.0),
    );

    curves
}

struct LineStylesApp {
    plot: Plot,
}

impl LineStylesApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);
        set_curves(render_state, &build());

        let mut plot = Plot::new(0);
        plot.limits = (0.0, T_MAX, -6.5, 6.5);
        plot.title = Some("line styles".to_owned());
        plot.x_label = Some("t".to_owned());

        Self { plot }
    }
}

impl eframe::App for LineStylesApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            PlotWidget::new().show(ui, &mut self.plot);
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "egui-silx · line_styles",
        options,
        Box::new(|cc| Ok(Box::new(LineStylesApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
