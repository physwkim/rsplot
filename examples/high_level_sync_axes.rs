//! Synchronized axes example (high-level API).
//!
//! Mirrors silx `examples/syncaxis.py`: four 2D images share the same
//! pan/zoom via `SyncAxes`.  Dragging or zooming in any panel updates
//! all others simultaneously.
//!
//! High-level API: `PlotWidget::plot_mut()` exposes the inner `Plot`
//! needed by `SyncAxes::sync(&mut [&mut Plot, ...])`.
//!
//! Run with: `cargo run --example high_level_sync_axes`

use eframe::egui;
use rsplot::{Colormap, ColormapName, Plot2D, SyncAxes};
use std::f64::consts::PI;

const W: u32 = 80;
const H: u32 = 60;

struct SyncAxesApp {
    plots: [Plot2D; 4],
    sync: SyncAxes,
}

impl SyncAxesApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let colormap_names = [
            ColormapName::Greys,
            ColormapName::Viridis,
            ColormapName::Plasma,
            ColormapName::Inferno,
        ];
        let titles = ["greys", "viridis", "plasma", "inferno"];

        // Four plots with the same ripple pattern tinted by different colormaps.
        let plots: [Plot2D; 4] = std::array::from_fn(|i| {
            let mut p = Plot2D::new(rs, i as u64);
            p.set_graph_title(titles[i]);
            let data: Vec<f32> = (0..W * H)
                .map(|k| {
                    let x = (k % W) as f64 / W as f64 - 0.5;
                    let y = (k / W) as f64 / H as f64 - 0.5;
                    let shift = i as f64 * 0.5 * PI;
                    let r = (x * x + y * y).sqrt();
                    ((r * 10.0 + shift).sin() * 0.5 + 0.5) as f32
                })
                .collect();
            let cm = Colormap::new(colormap_names[i], 0.0, 1.0);
            p.try_add_image(W, H, &data, cm)
                .expect("fixed-size image is valid");
            p
        });

        Self {
            plots,
            sync: SyncAxes::new(), // sync both X and Y
        }
    }
}

impl eframe::App for SyncAxesApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Sync all four plots before rendering.
        {
            let [a, b, c, d] = &mut self.plots;
            self.sync
                .sync(&mut [a.plot_mut(), b.plot_mut(), c.plot_mut(), d.plot_mut()]);
        }

        let half = ui.available_size() * egui::vec2(0.5, 0.5);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.allocate_ui(half, |ui| {
                    self.plots[0].show(ui);
                });
                ui.allocate_ui(half, |ui| {
                    self.plots[1].show(ui);
                });
            });
            ui.horizontal(|ui| {
                ui.allocate_ui(half, |ui| {
                    self.plots[2].show(ui);
                });
                ui.allocate_ui(half, |ui| {
                    self.plots[3].show(ui);
                });
            });
        });
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "rsplot: synchronized axes",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(SyncAxesApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
