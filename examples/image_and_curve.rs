//! Image + curve example: a viridis image with a polyline drawn on top, both
//! sharing one coordinate system, under axes + a colorbar.
//!
//! The image is the same diagonal-ramp + Gaussian-bump field as the `image`
//! example; the curve is a sine that sweeps across the image extent. Because the
//! image, the curve, and the axes all derive from one `Transform`, the sine
//! should sit exactly on the data grid (e.g. its midline at y = height/2).
//!
//! Run with: `cargo run --example image_and_curve`

use eframe::egui;
use rsplot::{Colormap, CurveData, PlotWidget};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 192;

fn build_image() -> Vec<f32> {
    let mut data = vec![0.0f32; (WIDTH * HEIGHT) as usize];
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let u = col as f32 / (WIDTH - 1) as f32; // 0..1 left→right
            let v = row as f32 / (HEIGHT - 1) as f32; // 0..1 bottom→top
            let ramp = u + v; // 0..2 diagonal
            let (du, dv) = (u - 0.3, v - 0.7); // bump near upper-left
            let bump = 1.5 * (-(du * du + dv * dv) / 0.01).exp();
            data[(row * WIDTH + col) as usize] = ramp + bump;
        }
    }
    data
}

fn build_curve() -> CurveData {
    // A 3-period sine spanning the image width, centered vertically.
    let n = 400usize;
    let mut x = Vec::with_capacity(n);
    let mut y = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / (n - 1) as f64; // 0..1
        let xi = t * WIDTH as f64;
        let yi =
            HEIGHT as f64 * 0.5 + HEIGHT as f64 * 0.35 * (t * std::f64::consts::TAU * 3.0).sin();
        x.push(xi);
        y.push(yi);
    }
    CurveData::new(x, y, egui::Color32::from_rgb(255, 96, 96))
}

struct ImageCurveApp {
    plot: PlotWidget,
}

impl ImageCurveApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        let mut plot = PlotWidget::new(render_state, 0);
        let image = build_image();
        plot.add_image(WIDTH, HEIGHT, &image, Colormap::viridis(0.0, 2.5));
        let curve = build_curve();
        plot.add_curve(&curve.x, &curve.y, curve.color);
        // Show a crosshair + coordinate readout following the pointer.
        plot.set_graph_cursor(true);

        Self { plot }
    }
}

impl eframe::App for ImageCurveApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.plot.show(ui);
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "rsplot · image + curve",
        options,
        Box::new(|cc| Ok(Box::new(ImageCurveApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
