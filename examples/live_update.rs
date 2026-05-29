//! Live-update example: per-frame dirty uploads, reusing GPU resources.
//!
//! Each frame two partial updates run, neither recreating any GPU resource:
//! - the curve's vertices are rewritten in place (`update_curve`) so the sine
//!   scrolls horizontally;
//! - a few rows of the image texture are rewritten (`update_image_region`) so a
//!   bright scan line sweeps upward; the previous rows are restored from the
//!   base image, demonstrating a true sub-region `write_texture`.
//!
//! Pan/zoom/reset still work (image, curve, axes move together). Run with:
//! `cargo run --example live_update`

use std::f64::consts::TAU;

use eframe::egui;
use egui_silx::{
    Colormap, CurveData, ImageData, Plot, PlotWidget, install, set_curve, set_image, update_curve,
    update_image_region,
};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 192;
const BAR_H: u32 = 3;
const CURVE_N: usize = 400;
const CLIM_MAX: f32 = 2.5;

fn build_image_data() -> Vec<f32> {
    let mut data = vec![0.0f32; (WIDTH * HEIGHT) as usize];
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let u = col as f32 / (WIDTH - 1) as f32;
            let v = row as f32 / (HEIGHT - 1) as f32;
            let ramp = u + v;
            let (du, dv) = (u - 0.3, v - 0.7);
            let bump = 1.5 * (-(du * du + dv * dv) / 0.01).exp();
            data[(row * WIDTH + col) as usize] = ramp + bump;
        }
    }
    data
}

/// Curve x positions (fixed); only y is animated each frame.
fn curve_x() -> Vec<f64> {
    (0..CURVE_N)
        .map(|i| i as f64 / (CURVE_N - 1) as f64 * WIDTH as f64)
        .collect()
}

fn curve_y(x: &[f64], phase: f64) -> Vec<f64> {
    let center = HEIGHT as f64 * 0.5;
    let amp = HEIGHT as f64 * 0.35;
    x.iter()
        .map(|&xi| center + amp * ((xi / WIDTH as f64) * TAU * 3.0 + phase).sin())
        .collect()
}

struct LiveApp {
    plot: Plot,
    base: Vec<f32>,
    x: Vec<f64>,
    last_row: Option<u32>,
}

impl LiveApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        let base = build_image_data();
        let image = ImageData::new(
            WIDTH,
            HEIGHT,
            base.clone(),
            Colormap::viridis(0.0, CLIM_MAX as f64),
        );
        set_image(render_state, &image);

        let x = curve_x();
        let red = egui::Color32::from_rgb(255, 96, 96);
        set_curve(
            render_state,
            &CurveData::new(x.clone(), curve_y(&x, 0.0), red),
        );

        let mut plot = Plot::new(0);
        plot.limits = (0.0, WIDTH as f64, 0.0, HEIGHT as f64);
        plot.colormap = image.colormap().cloned();

        Self {
            plot,
            base,
            x,
            last_row: None,
        }
    }

    /// Rows `[r0, r0 + BAR_H)` of the base image, row-major.
    fn base_rows(&self, r0: u32) -> &[f32] {
        let start = (r0 * WIDTH) as usize;
        let end = ((r0 + BAR_H) * WIDTH) as usize;
        &self.base[start..end]
    }
}

impl eframe::App for LiveApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let render_state = frame
            .wgpu_render_state()
            .expect("wgpu render state")
            .clone();
        let time = ui.input(|i| i.time);

        // Curve: rewrite vertices in place (scrolls with time).
        let red = egui::Color32::from_rgb(255, 96, 96);
        update_curve(
            &render_state,
            &CurveData::new(self.x.clone(), curve_y(&self.x, time * 2.0), red),
        );

        // Image: sweep a bright scan line upward via sub-region writes.
        let span = HEIGHT - BAR_H;
        let row = ((time * 40.0) as i64).rem_euclid(span as i64) as u32;
        if let Some(prev) = self.last_row {
            let restore = self.base_rows(prev).to_vec();
            update_image_region(&render_state, 0, prev, WIDTH, BAR_H, &restore);
        }
        let bright = vec![CLIM_MAX; (WIDTH * BAR_H) as usize];
        update_image_region(&render_state, 0, row, WIDTH, BAR_H, &bright);
        self.last_row = Some(row);

        egui::CentralPanel::default().show_inside(ui, |ui| {
            PlotWidget::new().show(ui, &mut self.plot);
        });

        // Keep animating.
        ui.ctx().request_repaint();
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "egui-silx · live update",
        options,
        Box::new(|cc| Ok(Box::new(LiveApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
