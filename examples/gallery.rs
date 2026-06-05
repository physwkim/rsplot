//! Gallery capture harness.
//!
//! Renders a curated set of the high-level widgets to PNG files in
//! `doc/images/`, used by the README "Gallery" section. Capture is fully
//! headless: it uses `egui_kittest`'s wgpu test renderer to draw each scene
//! (full egui chrome plus siplot's `egui_wgpu` data-layer paint callbacks)
//! into an offscreen texture and reads it back — no window is opened.
//!
//! The scenes mirror the matching `high_level_*` examples so the gallery
//! reflects what those examples show. Run with:
//!
//! ```sh
//! cargo run --example gallery
//! ```

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use siplot::egui_wgpu::RenderState;
use siplot::{
    Colormap, CompareImages, FitModelChoice, FitWidget, GraphGrid, ImageGeometry, ImageView,
    Plot2D, PlotInteractionMode, PlotWidget, Roi, ScatterView, StackView, YAxis, egui,
};

/// A scene is a closure that draws into the harness root `Ui` each frame.
type Scene = Box<dyn FnMut(&mut egui::Ui)>;

/// Build a fresh installed `RenderState`, construct the scene with it, then
/// render the scene headlessly and write it to `doc/images/<name>.png`.
fn capture(
    name: &str,
    size: (f32, f32),
    pixels_per_point: f32,
    build: impl FnOnce(&RenderState) -> Scene,
) {
    // A fresh device + installed WgpuResources per scene keeps plot ids and GPU
    // state independent across captures.
    let render_state = create_render_state(default_wgpu_setup());
    siplot::install(&render_state);

    let mut scene = build(&render_state);

    let renderer = WgpuTestRenderer::from_render_state(render_state);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(size.0, size.1))
        .with_pixels_per_point(pixels_per_point)
        .with_max_steps(16)
        .renderer(renderer)
        .build_ui(move |ui| scene(ui));

    harness.run();
    let image = harness.render().expect("headless wgpu render");

    let path = format!("doc/images/{name}.png");
    write_png(&path, image.width(), image.height(), image.as_raw());
    println!("wrote {path} ({}x{})", image.width(), image.height());
}

/// Encode tightly packed RGBA8 pixels to a PNG file via the `png` crate.
fn write_png(path: &str, width: u32, height: u32, rgba: &[u8]) {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer.write_image_data(rgba).expect("png data");
    }
    std::fs::write(path, bytes).expect("write png");
}

fn main() {
    std::fs::create_dir_all("doc/images").expect("create doc/images");

    // 1. PlotWidget — colormapped image with a curve overlay, legend + active
    //    stats panel (mirrors high_level_plot_widget).
    capture("plot_widget", (960.0, 560.0), 1.5, |rs| {
        let mut plot = PlotWidget::new(rs, 0);
        plot.set_graph_cursor(true);
        plot.set_graph_title("Image with curve overlay");
        plot.set_graph_x_label("Columns");
        plot.set_graph_y_label("Rows", YAxis::Left);
        plot.set_keep_data_aspect_ratio(true);
        plot.set_graph_grid_mode(GraphGrid::None);
        plot.set_default_colormap(Colormap::viridis(-0.25, 1.25));

        let image = build_sinc_image(180, 140);
        let handle = plot
            .try_add_image_default(180, 140, &image)
            .expect("image length matches dimensions");
        plot.set_item_legend(handle, "sin(x*y) image");

        let x: Vec<f64> = (0..180).map(|c| c as f64).collect();
        let y: Vec<f64> = x
            .iter()
            .map(|x| 140.0 * (0.5 + 0.35 * (x * 0.09).sin()))
            .collect();
        plot.add_curve_with_legend(&x, &y, egui::Color32::from_rgb(255, 96, 96), "sine overlay");
        plot.set_active_item(Some(handle));
        plot.drain_events();

        Box::new(move |ui: &mut egui::Ui| {
            egui::Panel::right("gallery_plot_widget_panel")
                .default_size(220.0)
                .show_inside(ui, |ui| {
                    ui.heading("Legends");
                    plot.show_legend(ui);
                    ui.separator();
                    ui.heading("Active stats");
                    plot.show_active_stats(ui);
                });
            egui::CentralPanel::default().show_inside(ui, |ui| {
                plot.show_toolbar(ui);
                plot.show(ui);
            });
        })
    });

    // 2. Plot2D — image with a threshold mask overlay (mirrors high_level_plot2d).
    capture("plot2d", (900.0, 560.0), 1.5, |rs| {
        let (w, h) = (192u32, 144u32);
        let mut plot = Plot2D::new(rs, 0);
        plot.set_graph_title("Plot2D image with mask overlay");
        plot.set_graph_cursor(true);
        plot.set_default_colormap(Colormap::viridis(-0.3, 1.1));

        let image = build_ring_spot_image(w, h);
        let mask: Vec<bool> = image.iter().map(|v| *v > 0.65).collect();
        let image_handle = plot
            .try_add_default_image(w, h, &image)
            .expect("image length matches dimensions");
        plot.set_item_legend(image_handle, "intensity image");
        let mask_handle = plot
            .add_mask_with_geometry(
                w,
                h,
                &mask,
                egui::Color32::from_rgba_unmultiplied(255, 80, 80, 96),
                ImageGeometry::default(),
            )
            .expect("mask length matches dimensions");
        plot.set_item_legend(mask_handle, "threshold mask");
        plot.set_active_item(Some(image_handle));
        plot.drain_events();

        Box::new(move |ui: &mut egui::Ui| {
            egui::Panel::right("gallery_plot2d_panel")
                .default_size(220.0)
                .show_inside(ui, |ui| {
                    ui.heading("Legends");
                    plot.show_legend(ui);
                    ui.separator();
                    ui.heading("Active stats");
                    plot.show_active_stats(ui);
                });
            egui::CentralPanel::default().show_inside(ui, |ui| {
                plot.show_toolbar(ui);
                plot.show(ui);
            });
        })
    });

    // 3. ImageView — central image with column/row side histograms.
    capture("image_view", (1000.0, 600.0), 1.5, |rs| {
        let (w, h) = (128u32, 96u32);
        let pixels = build_gaussian_image(w, h);
        let mut view = ImageView::new(rs, 0);
        view.set_image(w, h, &pixels, Colormap::viridis(0.0, 1.0))
            .expect("image dimensions match");
        view.image_plot_mut().set_graph_title("ImageView");
        Box::new(move |ui: &mut egui::Ui| {
            egui::CentralPanel::default().show_inside(ui, |ui| {
                // ImageView lays its three columns (image | profile+radar |
                // colorbar) out from `available_size`; reserve the two
                // horizontal item-spacing gaps so the right profile is not
                // clipped at the frame edge.
                ui.set_max_width((ui.available_width() - 20.0).max(0.0));
                view.show(ui, None, None);
            });
        })
    });

    // 4. ScatterView — value-coloured scatter with a colorbar.
    capture("scatter_view", (880.0, 560.0), 1.5, |rs| {
        let (x, y, values) = build_scatter_data();
        let mut sv = ScatterView::new(rs, 0);
        sv.set_graph_title("ScatterView — value-coloured scatter");
        sv.set_data(&x, &y, &values, Colormap::viridis(0.0, 1.0))
            .expect("x / y / values are the same length");
        Box::new(move |ui: &mut egui::Ui| {
            egui::CentralPanel::default().show_inside(ui, |ui| {
                sv.show_toolbar(ui);
                sv.show(ui);
            });
        })
    });

    // 5. StackView — a 3D sinc volume browsed as 2D frames.
    capture("stack_view", (900.0, 600.0), 1.5, |rs| {
        let (d, h, w) = (40usize, 60usize, 80usize);
        let volume = build_sinc_volume(d, h, w);
        let mut sv = StackView::new(rs, 0);
        sv.set_graph_title("StackView — 3D sinc volume");
        sv.set_volume(volume, [d, h, w], Colormap::viridis(0.0, 1.0))
            .expect("volume has the correct size");
        sv.set_dimension_labels(["Z (depth)", "Y", "X"]);
        // The central depth slice is the brightest part of the sinc volume.
        sv.set_frame(sv.frame_count() / 2);
        Box::new(move |ui: &mut egui::Ui| {
            egui::CentralPanel::default().show_inside(ui, |ui| {
                sv.perspective_ui(ui);
                sv.show_frame_controls(ui);
                sv.show(ui);
            });
        })
    });

    // 6. CompareImages — half-split comparison of two images.
    capture("compare_images", (820.0, 600.0), 1.5, |rs| {
        let (w, h) = (128u32, 128u32);
        let (a, b) = build_compare_images(w, h);
        let mut cmp = CompareImages::new(rs, 0);
        cmp.set_images(w, h, &a, &b, Colormap::viridis(0.0, 1.0))
            .expect("data matches dimensions");
        cmp.set_graph_title("CompareImages — A vs B");
        Box::new(move |ui: &mut egui::Ui| {
            egui::CentralPanel::default().show_inside(ui, |ui| {
                cmp.show_toolbar(ui);
                cmp.show(ui);
            });
        })
    });

    // 7. FitWidget — an iterative Gaussian fit over noisy data (renders as an
    //    embedded window under kittest's embed_viewports default).
    capture("fit_widget", (700.0, 540.0), 1.5, |rs| {
        let mut fit = FitWidget::new(rs, 0);
        fit.set_open(true);
        let (x, y) = build_fit_data();
        fit.set_data(&x, &y);
        // Iterative Gaussian fit (error column + reduced chi-square row), then
        // populate the result table + fitted curve so the snapshot is filled in.
        fit.set_selected_choice(FitModelChoice::IterativeGaussian);
        fit.perform_fit_choice();
        Box::new(move |ui: &mut egui::Ui| {
            fit.show(ui.ctx());
        })
    });

    // 8. ROI manager payoff — styled, named ROIs rendered on the plot.
    capture("roi_manager", (820.0, 560.0), 1.5, |rs| {
        let (w, h) = (128u32, 96u32);
        let pixels = build_gaussian_image(w, h);
        let mut plot = Plot2D::new(rs, 0);
        plot.set_graph_title("Interactive ROI Manager");
        plot.set_default_colormap(Colormap::viridis(0.0, 1.0));
        plot.try_add_default_image(w, h, &pixels)
            .expect("image dimensions match");
        plot.set_interaction_mode(PlotInteractionMode::Select);

        let rect = plot.add_roi(Roi::Rect {
            x: (18.0, 58.0),
            y: (20.0, 52.0),
        });
        plot.set_roi_name(rect, "feature A");
        plot.set_roi_color(rect, egui::Color32::from_rgb(90, 200, 255));

        let spot = plot.add_roi(Roi::Circle {
            center: (92.0, 60.0),
            radius: 16.0,
        });
        plot.set_roi_name(spot, "spot");
        plot.set_roi_color(spot, egui::Color32::from_rgb(255, 180, 80));
        plot.set_roi_fill(spot, true);
        plot.set_current_roi(Some(spot));
        plot.drain_events();

        Box::new(move |ui: &mut egui::Ui| {
            egui::CentralPanel::default().show_inside(ui, |ui| {
                plot.show_with_toolbar(ui);
            });
        })
    });
}

// --- scene data builders (mirror the matching high_level_* examples) ---

/// `sin(r)/r` sinc image over a (width, height) grid (high_level_plot_widget).
fn build_sinc_image(width: u32, height: u32) -> Vec<f32> {
    let mut data = vec![0.0; (width * height) as usize];
    for row in 0..height {
        for col in 0..width {
            let x = -6.0 + 12.0 * col as f32 / (width - 1) as f32;
            let y = -5.0 + 10.0 * row as f32 / (height - 1) as f32;
            let r = (x * y).abs().max(0.05);
            data[(row * width + col) as usize] = (r.sin() / r) + 0.15 * (x * 0.7).cos();
        }
    }
    data
}

/// Concentric-ring image with a bright spot (high_level_plot2d).
fn build_ring_spot_image(width: u32, height: u32) -> Vec<f32> {
    let mut data = vec![0.0; (width * height) as usize];
    for row in 0..height {
        for col in 0..width {
            let x = -4.0 + 8.0 * col as f32 / (width - 1) as f32;
            let y = -3.0 + 6.0 * row as f32 / (height - 1) as f32;
            let ring = ((x * x + y * y).sqrt() * 2.4).sin();
            let spot = (-((x - 1.2).powi(2) + (y + 0.7).powi(2)) / 0.35).exp();
            data[(row * width + col) as usize] = 0.45 * ring + spot;
        }
    }
    data
}

/// Centred Gaussian image (high_level_image_view / high_level_roi_manager).
fn build_gaussian_image(width: u32, height: u32) -> Vec<f32> {
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for row in 0..height {
        for col in 0..width {
            let cx = (col as f32 - width as f32 / 2.0) / (width as f32 / 4.0);
            let cy = (row as f32 - height as f32 / 2.0) / (height as f32 / 4.0);
            pixels.push((-0.5 * (cx * cx + cy * cy)).exp());
        }
    }
    pixels
}

/// Halton-sampled scatter with a distance-from-centre value (high_level_scatter_view).
fn build_scatter_data() -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let n = 300usize;
    let mut x = Vec::with_capacity(n);
    let mut y = Vec::with_capacity(n);
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let xi = halton(i + 1, 2) * 100.0;
        let yi = halton(i + 1, 3) * 80.0;
        let cx = xi - 50.0;
        let cy = yi - 40.0;
        v.push((-(cx * cx + cy * cy) / 1200.0).exp());
        x.push(xi);
        y.push(yi);
    }
    (x, y, v)
}

fn halton(mut index: usize, base: usize) -> f64 {
    let mut result = 0.0;
    let mut f = 1.0;
    while index > 0 {
        f /= base as f64;
        result += f * (index % base) as f64;
        index /= base;
    }
    result
}

/// Flat row-major `[d, h, w]` sinc volume (high_level_stack_view).
fn build_sinc_volume(d: usize, h: usize, w: usize) -> Vec<f32> {
    let mut volume = Vec::with_capacity(d * h * w);
    for z in 0..d {
        for y in 0..h {
            for x in 0..w {
                let fx = (x as f32 - w as f32 / 2.0) / (w as f32 / 4.0);
                let fy = (y as f32 - h as f32 / 2.0) / (h as f32 / 4.0);
                let fz = (z as f32 - d as f32 / 2.0) / (d as f32 / 4.0);
                let r = (fx * fx + fy * fy + fz * fz).sqrt() + 1e-6;
                volume.push((r.sin() / r).abs().min(1.0));
            }
        }
    }
    volume
}

/// Two offset Gaussians for the A/B comparison (high_level_compare_images).
fn build_compare_images(width: u32, height: u32) -> (Vec<f32>, Vec<f32>) {
    let mut a = Vec::with_capacity((width * height) as usize);
    let mut b = Vec::with_capacity((width * height) as usize);
    for row in 0..height {
        for col in 0..width {
            let cx = (col as f32 - width as f32 / 2.0) / (width as f32 / 4.0);
            let cy = (row as f32 - height as f32 / 2.0) / (height as f32 / 4.0);
            a.push((-0.5 * (cx * cx + cy * cy)).exp());
            let cx2 = cx - 0.4;
            let cy2 = cy + 0.3;
            b.push(0.8 * (-0.5 * (cx2 * cx2 + cy2 * cy2)).exp());
        }
    }
    (a, b)
}

/// Noisy Gaussian-on-background samples for the fit (high_level_fit_widget).
fn build_fit_data() -> (Vec<f64>, Vec<f64>) {
    let mut x = Vec::with_capacity(100);
    let mut y = Vec::with_capacity(100);
    for i in 0..100 {
        let xi = i as f64 * 0.1;
        let (mu, sigma, a, bg) = (5.0, 1.0, 10.0, 2.0);
        let noise = ((i * 12345) % 100) as f64 / 100.0 - 0.5;
        let z = (xi - mu) / sigma;
        y.push(a * (-0.5 * z * z).exp() + bg + noise * 1.5);
        x.push(xi);
    }
    (x, y)
}
