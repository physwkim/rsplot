//! Interactive colormap picker example.
//!
//! Mirrors silx `examples/colormapDialog.py`: an image whose colormap
//! (name, vmin, vmax, normalization) can be changed at runtime through a
//! sidebar panel.
//!
//! When the colormap settings change the image is re-uploaded with the new
//! `ImageSpec::scalar(..., new_colormap)`.  This demonstrates the pattern for
//! live colormap editing without a dedicated dialog widget.
//!
//! Run with: `cargo run --example high_level_colormap`

use eframe::egui;
use rsplot::{Colormap, ColormapName, ImageSpec, ItemHandle, Normalization, Plot2D};

const W: u32 = 128;
const H: u32 = 128;

const COLORMAPS: &[(ColormapName, &str)] = &[
    (ColormapName::Viridis, "Viridis"),
    (ColormapName::Inferno, "Inferno"),
    (ColormapName::Magma, "Magma"),
    (ColormapName::Plasma, "Plasma"),
    (ColormapName::Greys, "Greys"),
    (ColormapName::Turbo, "Turbo"),
    (ColormapName::Cividis, "Cividis"),
    (ColormapName::Spectral, "Spectral"),
];

struct ColormapApp {
    plot: Plot2D,
    image_handle: ItemHandle,
    /// Raw pixel data (never changes — only the colormap is updated).
    pixels: Vec<f32>,
    selected_cm: usize,
    vmin: f32,
    vmax: f32,
    log_norm: bool,
    dirty: bool,
}

impl ColormapApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let pixels = build_image();
        let colormap = Colormap::viridis(0.0, 1.0);
        let mut plot = Plot2D::new(rs, 0);
        plot.set_graph_title("Interactive colormap");
        let image_handle = plot.add_image(W, H, &pixels, colormap);

        Self {
            plot,
            image_handle,
            pixels,
            selected_cm: 0,
            vmin: 0.0,
            vmax: 1.0,
            log_norm: false,
            dirty: false,
        }
    }

    fn rebuild_colormap(&self) -> Colormap {
        let name = COLORMAPS[self.selected_cm].0;
        let mut cm = Colormap::new(name, self.vmin as f64, self.vmax as f64);
        if self.log_norm {
            cm = cm.with_normalization(Normalization::Log);
        }
        cm
    }
}

impl eframe::App for ColormapApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.dirty {
            let cm = self.rebuild_colormap();
            self.plot
                .update_image_spec(self.image_handle, ImageSpec::scalar(W, H, &self.pixels, cm));
            self.dirty = false;
        }

        // Left: fixed-width settings panel. A `SidePanel` bounds its own width,
        // so the full-width `ui.separator()`s inside can't expand the column to
        // the whole window — the failure mode of nesting it in a `ui.horizontal`
        // next to the plot, which collapsed the image to zero width.
        egui::Panel::left("colormap_controls")
            .resizable(false)
            .default_size(160.0)
            .show_inside(ui, |ui| {
                ui.label("Colormap");
                for (i, &(_, name)) in COLORMAPS.iter().enumerate() {
                    if ui.radio_value(&mut self.selected_cm, i, name).changed() {
                        self.dirty = true;
                    }
                }

                ui.separator();
                ui.label("Normalization");
                if ui.checkbox(&mut self.log_norm, "Log scale").changed() {
                    self.dirty = true;
                }

                ui.separator();
                ui.label("vmin / vmax");
                if ui
                    .add(
                        egui::DragValue::new(&mut self.vmin)
                            .speed(0.01)
                            .range(0.0..=0.99),
                    )
                    .changed()
                {
                    if self.vmin >= self.vmax {
                        self.vmax = (self.vmin + 0.01).min(1.0);
                    }
                    self.dirty = true;
                }
                if ui
                    .add(
                        egui::DragValue::new(&mut self.vmax)
                            .speed(0.01)
                            .range(0.01..=1.0),
                    )
                    .changed()
                {
                    if self.vmax <= self.vmin {
                        self.vmin = (self.vmax - 0.01).max(0.0);
                    }
                    self.dirty = true;
                }
            });

        // Right: image fills the remaining central area.
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.plot.show(ui);
        });
    }
}

/// Gaussian-blend test image with values in [0, 1].
fn build_image() -> Vec<f32> {
    (0..W * H)
        .map(|k| {
            let x = (k % W) as f64 / W as f64 - 0.5;
            let y = (k / W) as f64 / H as f64 - 0.5;
            let r2 = x * x + y * y;
            // Two Gaussian blobs at different positions.
            let g1 = (-r2 / 0.02).exp() as f32;
            let x2 = (k % W) as f64 / W as f64 - 0.3;
            let y2 = (k / W) as f64 / H as f64 - 0.7;
            let g2 = (-(x2 * x2 + y2 * y2) / 0.03).exp() as f32 * 0.8;
            (g1 + g2).min(1.0)
        })
        .collect()
}

fn main() -> eframe::Result {
    eframe::run_native(
        "rsplot: interactive colormap",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(ColormapApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
