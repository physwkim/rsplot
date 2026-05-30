//! StackView example.
//!
//! Mirrors silx `examples/stackView.py`: a 3D volume rendered as a stack of
//! 2D image frames with a navigation slider (← / slider / →).
//!
//! Run with: `cargo run --example high_level_stack_view`

use eframe::egui;
use egui_silx::{Colormap, StackView};

const WIDTH: u32 = 80;
const HEIGHT: u32 = 60;
const DEPTH: u32 = 40;

struct StackViewApp {
    sv: StackView,
}

impl StackViewApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let frames = build_stack();
        let colormap = Colormap::viridis(0.0, 1.0);

        let mut sv = StackView::new(rs, 0);
        sv.set_graph_title("StackView — 3D sinc volume");
        sv.set_stack(WIDTH, HEIGHT, frames, colormap)
            .expect("generated frames have the correct size");

        Self { sv }
    }
}

impl eframe::App for StackViewApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.sv.show_frame_controls(ui);
        self.sv.show(ui);
    }
}

fn build_stack() -> Vec<Vec<f32>> {
    let w = WIDTH as usize;
    let h = HEIGHT as usize;
    let d = DEPTH as usize;
    let mut stack = Vec::with_capacity(d);
    for z in 0..d {
        let mut frame = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let fx = (x as f32 - w as f32 / 2.0) / (w as f32 / 4.0);
                let fy = (y as f32 - h as f32 / 2.0) / (h as f32 / 4.0);
                let fz = (z as f32 - d as f32 / 2.0) / (d as f32 / 4.0);
                let r = (fx * fx + fy * fy + fz * fz).sqrt() + 1e-6;
                frame.push((r.sin() / r).abs().min(1.0));
            }
        }
        stack.push(frame);
    }
    stack
}

fn main() -> eframe::Result {
    eframe::run_native(
        "egui-silx: stack view",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(StackViewApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
