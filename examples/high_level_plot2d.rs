//! High-level Plot2D example.
//!
//! Mirrors the image, mask, and profile-tool shape of silx examples
//! `plotProfile.py` and `scatterMask.py`: a 2D image with a mask overlay,
//! row/column profile extraction, legend selection, and active-item stats.
//!
//! Run with: `cargo run --example high_level_plot2d`

use eframe::egui;
use rsplot::{Colormap, ImageGeometry, Plot2D, ValueStats};

const WIDTH: u32 = 192;
const HEIGHT: u32 = 144;

struct Plot2dApp {
    plot: Plot2D,
    image: Vec<f32>,
    mask: Vec<bool>,
    row: u32,
    column: u32,
    mask_visible: bool,
}

impl Plot2dApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        let mut plot = Plot2D::new(render_state, 0);
        plot.set_graph_title("Plot2D image, mask, and profiles");
        plot.set_graph_cursor(true);
        plot.set_default_colormap(Colormap::viridis(-0.3, 1.1));

        let image = build_image();
        let mask = build_mask(&image);
        let image_handle = plot
            .try_add_default_image(WIDTH, HEIGHT, &image)
            .expect("generated image length matches dimensions");
        plot.set_item_legend(image_handle, "intensity image");
        let mask_handle = plot
            .add_mask_with_geometry(
                WIDTH,
                HEIGHT,
                &mask,
                egui::Color32::from_rgba_unmultiplied(255, 80, 80, 96),
                ImageGeometry::default(),
            )
            .expect("generated mask length matches dimensions");
        plot.set_item_legend(mask_handle, "threshold mask");
        plot.set_active_item(Some(image_handle));
        plot.drain_events();

        Self {
            plot,
            image,
            mask,
            row: HEIGHT / 2,
            column: WIDTH / 2,
            mask_visible: true,
        }
    }

    fn ensure_mask_state(&mut self) {
        let existing = self.plot.mask_by_legend("threshold mask");
        match (self.mask_visible, existing) {
            (true, None) => {
                let handle = self
                    .plot
                    .add_mask_with_geometry(
                        WIDTH,
                        HEIGHT,
                        &self.mask,
                        egui::Color32::from_rgba_unmultiplied(255, 80, 80, 96),
                        ImageGeometry::default(),
                    )
                    .expect("generated mask length matches dimensions");
                self.plot.set_item_legend(handle, "threshold mask");
            }
            (false, Some(handle)) => {
                self.plot.remove_mask(handle);
            }
            _ => {}
        }
    }

    fn row_stats(&self) -> ValueStats {
        let values = self
            .plot
            .horizontal_profile(WIDTH, HEIGHT, &self.image, self.row)
            .expect("row slider is clamped to image height");
        ValueStats::from_f64(&values)
    }

    fn column_stats(&self) -> ValueStats {
        let values = self
            .plot
            .vertical_profile(WIDTH, HEIGHT, &self.image, self.column)
            .expect("column slider is clamped to image width");
        ValueStats::from_f64(&values)
    }
}

impl eframe::App for Plot2dApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::right("plot2d_inspector")
            .resizable(true)
            .default_size(230.0)
            .show_inside(ui, |ui| {
                ui.heading("Legends");
                self.plot.show_legend(ui);
                ui.separator();
                ui.heading("Active stats");
                self.plot.show_active_stats(ui);
                ui.separator();
                ui.heading("Horizontal profile");
                show_value_stats(ui, self.row_stats());
                ui.separator();
                ui.heading("Vertical profile");
                show_value_stats(ui, self.column_stats());
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            let mut mask_visible = self.mask_visible;
            let mut row = self.row;
            let mut column = self.column;
            self.plot.show_toolbar_with(ui, |ui, _plot| {
                ui.checkbox(&mut mask_visible, "Mask");
                ui.add(egui::Slider::new(&mut row, 0..=HEIGHT - 1).text("row"));
                ui.add(egui::Slider::new(&mut column, 0..=WIDTH - 1).text("column"));
            });
            self.mask_visible = mask_visible;
            self.row = row;
            self.column = column;
            self.ensure_mask_state();
            self.plot.show(ui);
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

fn build_mask(image: &[f32]) -> Vec<bool> {
    image.iter().map(|value| *value > 0.65).collect()
}

fn show_value_stats(ui: &mut egui::Ui, stats: ValueStats) {
    ui.label(format!("n: {}", stats.count));
    ui.label(format!("finite: {}", stats.finite_count));
    ui.label(format!("min: {}", fmt_value(stats.min)));
    ui.label(format!("max: {}", fmt_value(stats.max)));
    ui.label(format!("mean: {}", fmt_value(stats.mean)));
}

fn fmt_value(value: Option<f64>) -> String {
    value.map_or_else(|| "n/a".to_owned(), |value| format!("{value:.4}"))
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "rsplot - high-level Plot2D",
        options,
        Box::new(|cc| Ok(Box::new(Plot2dApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
