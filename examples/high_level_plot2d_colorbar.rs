//! Plot2D with the unified interactive histogram colorbar.
//!
//! Demonstrates that the pyqtgraph-`HistogramLUTItem`-style interactive colorbar
//! (value histogram + gradient + two draggable `vmin`/`vmax` handles) is the
//! same widget for a bare [`Plot2D`] as for [`ImageView`] — it is painted into
//! the plot's right gutter (the "chrome" colorbar path) instead of a separate
//! standalone column. Enable it with [`Plot2D::set_interactive_colorbar`].
//!
//! Drag a handle — or right-click the colorbar and pick "Auto range" (reset to
//! the data extremes, pyqtgraph `autoLevel`): [`Plot2D::show`] returns the new
//! levels via [`rsplot::PlotResponse::colorbar_dragged_levels`]; the owner
//! applies them with [`Plot2D::set_active_image_levels`] and the image contrast
//! updates live. (Off by default; silx adjusts levels through a separate
//! `ColormapDialog`.)
//!
//! Run with: `cargo run --example high_level_plot2d_colorbar`

use eframe::egui;
use rsplot::{Colormap, Plot2D};

const WIDTH: u32 = 192;
const HEIGHT: u32 = 144;

struct App {
    plot: Plot2D,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");

        let image = build_image();
        let (dmin, dmax) = data_range(&image);

        let mut plot = Plot2D::new(render_state, 0);
        plot.set_graph_title("Plot2D — drag the colorbar handles to set vmin/vmax");
        plot.set_default_colormap(Colormap::viridis(dmin as f64, dmax as f64));
        plot.try_add_default_image(WIDTH, HEIGHT, &image)
            .expect("generated image length matches dimensions");

        // Same opt-in interactive colorbar as ImageView, here in Plot2D's gutter.
        plot.set_show_colorbar(true);
        plot.set_interactive_colorbar(true);
        // The bar axis spans the data range so both handles stay reachable, and
        // the value-distribution histogram is drawn beside the gradient.
        plot.set_colorbar_value_range(Some((dmin as f64, dmax as f64)));
        let data: Vec<f64> = image.iter().map(|&v| v as f64).collect();
        plot.set_colorbar_histogram(rsplot::core::histogram::compute_histogram(
            &data, None, false,
        ));

        Self { plot }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            let response = self.plot.show(ui);
            // Single-owner apply: the colorbar drag only reports the new levels;
            // the owner commits them to the active image's colormap.
            if let Some((vmin, vmax)) = response.colorbar_dragged_levels {
                self.plot.set_active_image_levels(vmin, vmax);
            }
        });
    }
}

fn build_image() -> Vec<f32> {
    let mut data = vec![0.0; (WIDTH * HEIGHT) as usize];
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let x = -4.0 + 8.0 * col as f32 / (WIDTH - 1) as f32;
            let y = -3.0 + 6.0 * row as f32 / (HEIGHT - 1) as f32;
            let ring = ((x * x + y * y).sqrt() * 2.4).sin();
            let spot = (-((x - 1.2).powi(2) + (y + 0.7).powi(2)) / 0.35).exp();
            data[(row * WIDTH + col) as usize] = 0.45 * ring + spot;
        }
    }
    data
}

fn data_range(image: &[f32]) -> (f32, f32) {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for &v in image {
        if v.is_finite() {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    (lo, hi)
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "rsplot - Plot2D interactive colorbar",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)) as Box<dyn eframe::App>)),
    )
}
