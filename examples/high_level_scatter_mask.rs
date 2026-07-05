//! Scatter point masking and alpha-slider example.
//!
//! Mirrors silx `examples/scatterMask.py`: scatter points colored by a
//! scalar value with two runtime controls:
//!
//! - **Alpha slider** — global point opacity (same as `NamedScatterAlphaSlider`).
//! - **Threshold range** — points with values outside `[lo, hi]` are drawn
//!   at low opacity, mirroring the scatter mask selection.
//!
//! Demonstrates building `CurveColor::PerVertex` with per-point alpha for
//! masking without a dedicated mask widget.
//!
//! Run with: `cargo run --example high_level_scatter_mask`

use eframe::egui;
use rsplot::{Colormap, CurveColor, CurveSpec, ItemHandle, LineStyle, PlotWidget, Symbol};

const N: usize = 500;
const MASKED_ALPHA: u8 = 30;

fn halton(index: usize, base: usize) -> f64 {
    let mut f = 1.0f64;
    let mut r = 0.0f64;
    let mut i = index;
    while i > 0 {
        f /= base as f64;
        r += f * (i % base) as f64;
        i /= base;
    }
    r
}

struct ScatterMaskApp {
    plot: PlotWidget,
    scatter_handle: ItemHandle,
    xs: Vec<f64>,
    ys: Vec<f64>,
    values: Vec<f64>,
    colormap: Colormap,
    alpha: u8,
    mask_lo: f32,
    mask_hi: f32,
}

impl ScatterMaskApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let xs: Vec<f64> = (0..N).map(|i| halton(i + 1, 2) * 2.0 - 1.0).collect();
        let ys: Vec<f64> = (0..N).map(|i| halton(i + 1, 3) * 2.0 - 1.0).collect();
        let values: Vec<f64> = xs
            .iter()
            .zip(ys.iter())
            .map(|(&x, &y)| (-(x * x + y * y) / 0.5).exp())
            .collect();

        let mut plot = PlotWidget::new(rs, 0);
        plot.set_graph_title("Scatter mask & alpha slider");
        plot.set_graph_cursor(true);

        let colormap = Colormap::viridis(0.0, 1.0);
        let colors = build_colors(&values, &colormap, 255, 0.0, 1.0);
        let mut spec = CurveSpec::new(&xs, &ys, egui::Color32::WHITE);
        spec.color = CurveColor::PerVertex(&colors);
        spec.line_style = LineStyle::None;
        spec.symbol = Some(Symbol::Circle);
        spec.symbol_size = 6.0;
        let scatter_handle = plot.add_curve_spec(spec);
        plot.set_item_legend(scatter_handle, "scatter (Gaussian intensity)");

        Self {
            plot,
            scatter_handle,
            xs,
            ys,
            values,
            colormap,
            alpha: 255,
            mask_lo: 0.0,
            mask_hi: 1.0,
        }
    }

    fn rebuild_scatter(&mut self) {
        let colors = build_colors(
            &self.values,
            &self.colormap,
            self.alpha,
            self.mask_lo as f64,
            self.mask_hi as f64,
        );
        let mut spec = CurveSpec::new(&self.xs, &self.ys, egui::Color32::WHITE);
        spec.color = CurveColor::PerVertex(&colors);
        spec.line_style = LineStyle::None;
        spec.symbol = Some(Symbol::Circle);
        spec.symbol_size = 6.0;
        self.plot.update_curve_spec(self.scatter_handle, spec);
    }
}

fn build_colors(
    values: &[f64],
    colormap: &Colormap,
    alpha: u8,
    lo: f64,
    hi: f64,
) -> Vec<egui::Color32> {
    values
        .iter()
        .map(|&v| {
            let t = colormap.normalize(v);
            let idx = (t * 255.0).clamp(0.0, 255.0) as usize;
            let [r, g, b, _] = colormap.lut[idx];
            let a = if v >= lo && v <= hi {
                alpha
            } else {
                MASKED_ALPHA
            };
            egui::Color32::from_rgba_unmultiplied(r, g, b, a)
        })
        .collect()
}

impl eframe::App for ScatterMaskApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let mut dirty = false;

        // Controls panel — a `SidePanel` bounds its own width so the full-width
        // `ui.separator()`s can't expand the column to the whole window (which
        // would collapse the plot to zero width).
        egui::Panel::left("scatter_mask_controls")
            .resizable(false)
            .default_size(180.0)
            .show_inside(ui, |ui| {
                ui.label("Point opacity (alpha)");
                let mut alpha_f = self.alpha as f32 / 255.0;
                if ui
                    .add(egui::Slider::new(&mut alpha_f, 0.0..=1.0).text("α"))
                    .changed()
                {
                    self.alpha = (alpha_f * 255.0) as u8;
                    dirty = true;
                }

                ui.separator();
                ui.label("Value threshold (mask)");
                ui.label("Unmasked range [lo, hi]:");
                if ui
                    .add(egui::Slider::new(&mut self.mask_lo, 0.0..=1.0).text("lo"))
                    .changed()
                {
                    if self.mask_lo > self.mask_hi {
                        self.mask_hi = self.mask_lo;
                    }
                    dirty = true;
                }
                if ui
                    .add(egui::Slider::new(&mut self.mask_hi, 0.0..=1.0).text("hi"))
                    .changed()
                {
                    if self.mask_hi < self.mask_lo {
                        self.mask_lo = self.mask_hi;
                    }
                    dirty = true;
                }

                ui.separator();
                ui.label(format!(
                    "Selected: {}/{N}",
                    self.values
                        .iter()
                        .filter(|&&v| v >= self.mask_lo as f64 && v <= self.mask_hi as f64)
                        .count()
                ));
            });

        // Plot fills the remaining central area.
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.plot.show_toolbar(ui);
            self.plot.show(ui);
        });

        if dirty {
            self.rebuild_scatter();
        }
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "rsplot: scatter mask",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(ScatterMaskApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
