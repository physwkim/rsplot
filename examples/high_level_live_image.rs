//! High-level live-image example.
//!
//! Mirrors silx `plotUpdateImageFromThread.py` and
//! `plotUpdateImageFromGevent.py` at the egui level: image data is updated in
//! place on the UI thread while the item handle is retained.
//!
//! Run with: `cargo run --example high_level_live_image`

use eframe::egui;
use egui_silx::{Colormap, ItemHandle, Plot2D};

const WIDTH: u32 = 128;
const HEIGHT: u32 = 96;

struct LiveImageApp {
    plot: Plot2D,
    image: ItemHandle,
    colormap: Colormap,
    paused: bool,
    phase: f32,
}

impl LiveImageApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        let mut plot = Plot2D::new(render_state, 0);
        plot.set_graph_title("Live image update");
        plot.set_graph_cursor(true);
        plot.set_auto_reset_zoom(false);

        let colormap = Colormap::viridis(0.0, 1.5);
        plot.set_default_colormap(colormap.clone());
        let data = image_values(0.0);
        let image = plot
            .try_add_default_image(WIDTH, HEIGHT, &data)
            .expect("generated image length matches dimensions");
        plot.set_item_legend(image, "signal");
        plot.set_limits(0.0, WIDTH as f64, 0.0, HEIGHT as f64, None);
        plot.drain_events();

        Self {
            plot,
            image,
            colormap,
            paused: false,
            phase: 0.0,
        }
    }

    fn update_image(&mut self, dt: f32) {
        self.phase += dt;
        let data = image_values(self.phase);
        self.plot
            .try_update_image(self.image, WIDTH, HEIGHT, &data, self.colormap.clone())
            .expect("generated image length matches dimensions");
    }
}

impl eframe::App for LiveImageApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::right("live_image_stats")
            .resizable(true)
            .default_size(180.0)
            .show_inside(ui, |ui| {
                ui.heading("Stats");
                self.plot.show_active_stats(ui);
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            let mut paused = self.paused;
            let phase = self.phase;
            self.plot.show_toolbar_with(ui, |ui, _plot| {
                ui.checkbox(&mut paused, "Paused");
                ui.label(format!("phase: {phase:.2}"));
            });
            self.paused = paused;
            if !self.paused {
                let dt = ui
                    .input(|input| input.stable_dt)
                    .clamp(1.0 / 240.0, 1.0 / 15.0);
                self.update_image(dt);
                ui.ctx().request_repaint();
            }
            self.plot.show(ui);
        });
    }
}

fn image_values(phase: f32) -> Vec<f32> {
    let mut data = vec![0.0; (WIDTH * HEIGHT) as usize];
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let x = -3.0 + 6.0 * col as f32 / (WIDTH - 1) as f32;
            let y = -2.0 + 4.0 * row as f32 / (HEIGHT - 1) as f32;
            let cx = 1.3 * phase.cos();
            let cy = 0.8 * phase.sin();
            let gaussian = (-((x - cx).powi(2) + (y - cy).powi(2)) / 0.55).exp();
            let ripple = 0.25 * ((x * 4.0 + phase * 3.0).sin() + (y * 3.0).cos());
            data[(row * WIDTH + col) as usize] = 0.35 + gaussian + ripple;
        }
    }
    data
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "egui-silx - high-level live image",
        options,
        Box::new(|cc| Ok(Box::new(LiveImageApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
