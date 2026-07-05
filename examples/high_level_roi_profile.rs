//! ROI Profile Window Example.
//!
//! Demonstrates the `ProfileWindow` widget alongside a `Plot2D`.
//! Use the profile toolbar (L for Line, R for Rectangle) to draw an ROI.
//! The profile window automatically updates to show the 1D profile.
//!
//! Run with: `cargo run --example high_level_roi_profile`

use eframe::egui;
use rsplot::{Colormap, Plot2D, ProfileMode, ProfileWindow, Roi};

const WIDTH: u32 = 128;
const HEIGHT: u32 = 96;

struct RoiProfileApp {
    image_plot: Plot2D,
    profile_window: ProfileWindow,
    pixels: Vec<f32>,
    active_roi_index: Option<usize>,
    last_mode: ProfileMode,
}

impl RoiProfileApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let pixels = build_image();

        let mut image_plot = Plot2D::new(rs, 0);
        image_plot.set_graph_title("Image with interactive ROI Profile");
        image_plot.set_graph_cursor(true);
        image_plot.set_default_colormap(Colormap::viridis(0.0, 1.0));
        image_plot
            .try_add_default_image(WIDTH, HEIGHT, &pixels)
            .expect("image dimensions match");

        let mut profile_window = ProfileWindow::new(rs, 1);
        profile_window.set_open(true);

        Self {
            image_plot,
            profile_window,
            pixels,
            active_roi_index: None,
            last_mode: ProfileMode::None,
        }
    }
}

impl eframe::App for RoiProfileApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Show the separate ProfileWindow
        if self.profile_window.is_open() {
            self.profile_window.show(ui.ctx());
        }

        // Show the image plot
        let (_plot_resp, mode) = ui
            .allocate_ui(ui.available_size(), |ui| {
                let (_, mode) = self.image_plot.show_toolbar_with(ui, |ui, plot| {
                    ui.separator();
                    plot.show_profile_toolbar(ui)
                });
                let resp = self.image_plot.show(ui);
                (resp, mode)
            })
            .inner;

        // Manage ROI based on mode switch
        if mode != self.last_mode {
            self.image_plot.clear_rois();
            self.active_roi_index = None;

            let cx = WIDTH as f64 / 2.0;
            let cy = HEIGHT as f64 / 2.0;
            let dx = WIDTH as f64 / 4.0;
            let dy = HEIGHT as f64 / 4.0;

            match mode {
                ProfileMode::Line => {
                    let idx = self.image_plot.add_roi(Roi::Line {
                        start: (cx - dx, cy),
                        end: (cx + dx, cy),
                    });
                    self.active_roi_index = Some(idx);
                    self.profile_window.set_open(true);
                }
                ProfileMode::Rectangle => {
                    let idx = self.image_plot.add_roi(Roi::Rect {
                        x: (cx - dx, cx + dx),
                        y: (cy - dy, cy + dy),
                    });
                    self.active_roi_index = Some(idx);
                    self.profile_window.set_open(true);
                }
                ProfileMode::Horizontal => {
                    let idx = self.image_plot.add_roi(Roi::HRange {
                        y: (cy - 1.0, cy + 1.0),
                    });
                    self.active_roi_index = Some(idx);
                    self.profile_window.set_open(true);
                }
                ProfileMode::Vertical => {
                    let idx = self.image_plot.add_roi(Roi::VRange {
                        x: (cx - 1.0, cx + 1.0),
                    });
                    self.active_roi_index = Some(idx);
                    self.profile_window.set_open(true);
                }
                ProfileMode::None => {}
            }
            self.last_mode = mode;
        }

        // Update profile based on active ROI
        if let Some(idx) = self.active_roi_index
            && let Some(managed) = self.image_plot.rois().get(idx)
        {
            // If the user modified the ROI, the plot updates automatically.
            // In a real app we'd check for `PlotEvent::RoiChanged { index: idx }`
            // but updating it every frame is fine for the example.
            self.profile_window.update_profile(
                WIDTH,
                HEIGHT,
                &self.pixels,
                &managed.roi,
                "Columns",
                "Rows",
            );
        }
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
        "rsplot: ROI Profile Window",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(RoiProfileApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
