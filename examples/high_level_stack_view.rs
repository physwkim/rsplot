//! StackView example.
//!
//! Mirrors silx `examples/stackView.py`: a 3D volume browsed as a stack of 2D
//! image frames. A navigation slider (← / slider / →) steps through frames, and
//! a "Browse dimension" combo picks which volume axis to slice along — silx
//! `StackView` perspective selection with automatic transposition. The plot's X
//! and Y axis labels follow the chosen perspective (silx `setLabels`).
//!
//! Run with: `cargo run --example high_level_stack_view`

use eframe::egui;
use rsplot::{Colormap, StackView};

// Volume dimensions as [d0, d1, d2]: d0 = depth (Z), d1 = height (Y), d2 = width
// (X). With the default Axis0 perspective each frame is (d1, d2) = (Y, X).
const DEPTH: u32 = 40; // d0
const HEIGHT: u32 = 60; // d1
const WIDTH: u32 = 80; // d2

struct StackViewApp {
    sv: StackView,
}

impl StackViewApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let volume = build_volume();
        let colormap = Colormap::viridis(0.0, 1.0);

        let mut sv = StackView::new(rs, 0);
        sv.set_graph_title("StackView — 3D sinc volume");
        // Load the whole volume so the perspective combo can re-slice it along
        // any of the three dimensions.
        sv.set_volume(
            volume,
            [DEPTH as usize, HEIGHT as usize, WIDTH as usize],
            colormap,
        )
        .expect("generated volume has the correct size");
        // Per-dimension labels (silx setLabels): rotate onto the X/Y axes as the
        // perspective changes.
        sv.set_dimension_labels(["Z (depth)", "Y", "X"]);

        Self { sv }
    }
}

impl eframe::App for StackViewApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.sv.perspective_ui(ui);
        self.sv.show_frame_controls(ui);
        self.sv.show(ui);
    }
}

/// A flat row-major `[DEPTH, HEIGHT, WIDTH]` sinc volume; element `(z, y, x)`
/// sits at offset `(z * HEIGHT + y) * WIDTH + x`.
fn build_volume() -> Vec<f32> {
    let w = WIDTH as usize;
    let h = HEIGHT as usize;
    let d = DEPTH as usize;
    let mut volume = Vec::with_capacity(d * h * w);
    for z in 0..d {
        for y in 0..h {
            for x in 0..w {
                let fx = (x as f32 - w as f32 / 2.0) / (w as f32 / 4.0);
                let fy = (y as f32 - h as f32 / 2.0) / (h as f32 / 4.0);
                let fz = (z as f32 - d as f32 / 2.0) / (d as f32 / 4.0);
                let r = (fx * fx + fy * fy + fz * fz).sqrt() + 1e-6;
                volume.push((r.sin() / r).abs().min(1.0));
            }
        }
    }
    volume
}

fn main() -> eframe::Result {
    eframe::run_native(
        "rsplot: stack view",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(StackViewApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
