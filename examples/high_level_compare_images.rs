//! CompareImages example.
//!
//! Mirrors silx `examples/compareImages.py`: side-by-side half-split view of
//! two scalar images with A / B / ½ / A-B mode toolbar and a split slider.
//!
//! Run with: `cargo run --example high_level_compare_images`

use eframe::egui;
use rsplot::{Colormap, CompareImages};

const WIDTH: u32 = 128;
const HEIGHT: u32 = 128;

struct CompareApp {
    cmp: CompareImages,
}

impl CompareApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let (data_a, data_b) = build_images();

        let mut cmp = CompareImages::new(rs, 0);
        cmp.set_images(
            (WIDTH, HEIGHT),
            &data_a,
            (WIDTH, HEIGHT),
            &data_b,
            Colormap::viridis(0.0, 1.0),
        )
        .expect("generated data matches dimensions");
        cmp.set_graph_title("CompareImages — A vs B");

        Self { cmp }
    }
}

impl eframe::App for CompareApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.cmp.show_toolbar(ui);
        self.cmp.show(ui);
    }
}

fn build_images() -> (Vec<f32>, Vec<f32>) {
    let mut a = Vec::with_capacity((WIDTH * HEIGHT) as usize);
    let mut b = Vec::with_capacity((WIDTH * HEIGHT) as usize);
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let cx = (col as f32 - WIDTH as f32 / 2.0) / (WIDTH as f32 / 4.0);
            let cy = (row as f32 - HEIGHT as f32 / 2.0) / (HEIGHT as f32 / 4.0);
            let v = (-0.5 * (cx * cx + cy * cy)).exp();
            a.push(v);
            // B is A shifted by ~20% and scaled
            let cx2 = cx - 0.4;
            let cy2 = cy + 0.3;
            b.push(0.8 * (-0.5 * (cx2 * cx2 + cy2 * cy2)).exp());
        }
    }
    (a, b)
}

fn main() -> eframe::Result {
    eframe::run_native(
        "rsplot: compare images",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(CompareApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
