//! Live profile toolbar example.
//!
//! Mirrors `silx/examples/plotProfile.py`: a 2D image plot with the compact
//! profile-mode toolbar (None / Horizontal / Vertical / Line / Rectangle).
//! While Horizontal or Vertical is active and the cursor hovers over the image,
//! the corresponding row or column profile is extracted from the pixel data and
//! drawn live in a companion Plot1D below.  (The shared toolbar also shows Line
//! and Rectangle, but only the row/column modes are wired in this example.)
//!
//! A yellow crosshair marker stays on the image at the last profiled pixel —
//! unlike the hover crosshair, it survives the pointer leaving the image (silx
//! keeps the profile ROI drawn on the image the same way). The profile plot has
//! its own toolbar; its auto-scale X/Y toggles control which axes follow the
//! live profile data (reset-zoom-on-data is per-axis-flag aware).
//!
//! Run with: `cargo run --example high_level_live_profile`

use eframe::egui;
use rsplot::{Colormap, CurveData, ItemHandle, Plot1D, Plot2D, ProfileMode, YAxis};

const WIDTH: u32 = 128;
const HEIGHT: u32 = 96;

struct LiveProfileApp {
    image_plot: Plot2D,
    profile_plot: Plot1D,
    pixels: Vec<f32>,
    profile_handle: ItemHandle,
    /// Persistent crosshair on the image at the last profiled pixel
    /// `(vertical x-marker, horizontal y-marker)`, created on the first
    /// profile extraction.
    cross_markers: Option<(ItemHandle, ItemHandle)>,
}

impl LiveProfileApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let pixels = build_image();

        let mut image_plot = Plot2D::new(rs, 0);
        image_plot.set_graph_title("Image (hover for live profile)");
        image_plot.set_graph_cursor(true);
        image_plot.set_default_colormap(Colormap::viridis(0.0, 1.0));
        image_plot
            .try_add_default_image(WIDTH, HEIGHT, &pixels)
            .expect("image dimensions match");

        let mut profile_plot = Plot1D::new(rs, 1);
        profile_plot.set_graph_title("Profile");

        // Pre-insert an empty curve that will be updated in the frame loop.
        let init_y: Vec<f64> = vec![0.0; WIDTH as usize];
        let init_x: Vec<f64> = (0..WIDTH as usize).map(|i| i as f64).collect();
        let profile_handle =
            profile_plot.add_curve_with_legend(&init_x, &init_y, egui::Color32::YELLOW, "profile");

        Self {
            image_plot,
            profile_plot,
            pixels,
            profile_handle,
            cross_markers: None,
        }
    }

    /// Place (or move) the persistent crosshair markers at the profiled
    /// pixel's center.
    fn update_cross_markers(&mut self, cx: f64, cy: f64) {
        match self.cross_markers {
            Some((vx, hy)) => {
                self.image_plot.set_marker_position(vx, cx, cy);
                self.image_plot.set_marker_position(hy, cx, cy);
            }
            None => {
                let vx = self.image_plot.add_x_marker(cx, egui::Color32::YELLOW);
                let hy = self
                    .image_plot
                    .add_y_marker(cy, egui::Color32::YELLOW, YAxis::Left);
                self.cross_markers = Some((vx, hy));
            }
        }
    }
}

impl eframe::App for LiveProfileApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let half_h = ui.available_size() * egui::vec2(1.0, 0.5);

        // Top half: image + profile toolbar.
        let (plot_resp, mode) = ui
            .allocate_ui(half_h, |ui| {
                let (_, mode) = self.image_plot.show_toolbar_with(ui, |ui, plot| {
                    ui.separator();
                    plot.show_profile_toolbar(ui)
                });
                let resp = self.image_plot.show(ui);
                (resp, mode)
            })
            .inner;

        // Update profile curve from hover position when a mode is active.
        if let Some((x, y)) =
            self.image_plot
                .profile_at_cursor(&plot_resp, &self.pixels, WIDTH, HEIGHT, mode)
        {
            let curve = CurveData::new(x, y, egui::Color32::YELLOW);
            // profile_plot is a Plot1D, which DerefMuts to PlotWidget.
            self.profile_plot
                .update_curve_data(self.profile_handle, &curve);

            // Mark the profiled pixel on the image with a persistent crosshair
            // (the hover crosshair vanishes when the pointer leaves; silx keeps
            // the profile ROI drawn on the image). Same cursor→pixel mapping as
            // `profile_at_cursor` (floor to the pixel, marker at its center).
            if let Some(p) = plot_resp.response.hover_pos() {
                let (dx, dy) = plot_resp.transform.pixel_to_data(p);
                self.update_cross_markers(dx.floor() + 0.5, dy.floor() + 0.5);
            }

            // Relabel X axis to reflect current mode.
            let label = match mode {
                ProfileMode::Horizontal => "column",
                ProfileMode::Vertical => "row",
                ProfileMode::None | ProfileMode::Line | ProfileMode::Rectangle => "index",
            };
            self.profile_plot.set_graph_x_label(label);
        }

        // Bottom half: profile plot with its own toolbar. The auto-scale X/Y
        // toggle buttons gate which axes refit when the profile data updates.
        ui.allocate_ui(half_h, |ui| {
            self.profile_plot.show_toolbar(ui);
            self.profile_plot.show(ui);
        });

        // Neither plot's event queue is consumed by this example; drain so the
        // per-frame marker moves and curve updates do not accumulate.
        self.image_plot.drain_events();
        self.profile_plot.drain_events();
    }
}

fn build_image() -> Vec<f32> {
    let mut pixels = Vec::with_capacity((WIDTH * HEIGHT) as usize);
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let cx = (col as f32 - WIDTH as f32 / 2.0) / (WIDTH as f32 / 4.0);
            let cy = (row as f32 - HEIGHT as f32 / 2.0) / (HEIGHT as f32 / 4.0);
            pixels.push((-0.5 * (cx * cx + cy * cy)).exp());
        }
    }
    pixels
}

fn main() -> eframe::Result {
    eframe::run_native(
        "rsplot: live profile toolbar",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(LiveProfileApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
