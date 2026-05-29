//! Secondary-Y (y2) axis example: two curves on different scales, each aligned
//! to its own axis.
//!
//! Both curves share the X axis (time, 0..10). The blue curve reads on the
//! main left axis (0..100); the orange curve is bound to the right y2 axis
//! (0..1) via `CurveData::with_y_axis(YAxis::Right)`. The left axis ticks sit
//! in the left gutter and the y2 ticks in the right gutter, each curve tracking
//! its own scale (`doc/design.md` §13 A5).
//!
//! Run with: `cargo run --example y2_axis`

use eframe::egui;
use egui_silx::{CurveData, Plot, PlotWidget, YAxis, install, set_curves};

const N: usize = 400;

fn sample<F: Fn(f64) -> f64>(f: F, color: egui::Color32) -> CurveData {
    let mut x = Vec::with_capacity(N);
    let mut y = Vec::with_capacity(N);
    for i in 0..N {
        let t = 10.0 * i as f64 / (N - 1) as f64; // 0..10
        x.push(t);
        y.push(f(t));
    }
    CurveData::new(x, y, color)
}

struct Y2App {
    plot: Plot,
}

impl Y2App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        // Left axis (0..100): a damped oscillation. Right y2 axis (0..1): a
        // slow ramp toward saturation. Different scales, shared X.
        let left = sample(
            |t| 50.0 + 40.0 * (t * 1.5).sin() * (-t / 8.0).exp(),
            egui::Color32::from_rgb(120, 180, 255),
        );
        let right = sample(
            |t| 1.0 - (-t / 3.0).exp(),
            egui::Color32::from_rgb(255, 160, 80),
        )
        .with_y_axis(YAxis::Right);
        set_curves(render_state, &[left, right]);

        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 100.0);
        plot.y2 = Some((0.0, 1.0));

        Self { plot }
    }
}

impl eframe::App for Y2App {
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
        "egui-silx · y2 axis",
        options,
        Box::new(|cc| Ok(Box::new(Y2App::new(cc)) as Box<dyn eframe::App>)),
    )
}
