//! ScatterView example.
//!
//! Mirrors silx `examples/scatterview.py`: scatter points whose colour is
//! driven by a per-point value array mapped through a colormap (value-coloured
//! scatter).
//!
//! Run with: `cargo run --example high_level_scatter_view`

use eframe::egui;
use rsplot::{Colormap, ScatterView};

struct ScatterViewApp {
    sv: ScatterView,
}

impl ScatterViewApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let (x, y, values) = build_data();
        let colormap = Colormap::viridis(0.0, 1.0);

        let mut sv = ScatterView::new(rs, 0);
        sv.set_graph_title("ScatterView — value-coloured scatter");
        sv.set_data(&x, &y, &values, colormap)
            .expect("x / y / values are the same length");

        Self { sv }
    }
}

impl eframe::App for ScatterViewApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.sv.show_toolbar(ui);
            // `ScatterView::show` fills all remaining height, so reserve one row
            // at the bottom for the position-info readout before showing the
            // plot — otherwise the readout is pushed below the window edge and
            // is never visible.
            let info_h = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
            let plot_h = (ui.available_height() - info_h).max(0.0);
            let response = ui
                .allocate_ui(egui::vec2(ui.available_width(), plot_h), |ui| {
                    self.sv.show(ui)
                })
                .inner;
            // Position-info readout (silx ScatterView X/Y/Data/Index): hover a
            // point and X/Y/value/index snap to it; off a point, Data/Index
            // show "-".
            self.sv.show_position_info(ui, &response);
        });
    }
}

fn build_data() -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let n = 300usize;
    let mut x = Vec::with_capacity(n);
    let mut y = Vec::with_capacity(n);
    let mut v = Vec::with_capacity(n);

    // Pseudo-random grid: Halton-like sampling avoids importing rand.
    for i in 0..n {
        let xi = halton(i + 1, 2) * 100.0;
        let yi = halton(i + 1, 3) * 80.0;
        // value = distance from centre, normalised to [0,1]
        let cx = xi - 50.0;
        let cy = yi - 40.0;
        let val = (-(cx * cx + cy * cy) / 1200.0).exp();
        x.push(xi);
        y.push(yi);
        v.push(val);
    }
    (x, y, v)
}

fn halton(mut index: usize, base: usize) -> f64 {
    let mut result = 0.0;
    let mut f = 1.0;
    while index > 0 {
        f /= base as f64;
        result += f * (index % base) as f64;
        index /= base;
    }
    result
}

fn main() -> eframe::Result {
    eframe::run_native(
        "rsplot: scatter view",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(ScatterViewApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
