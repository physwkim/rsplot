//! 3D image & height-map example.
//!
//! Mirrors the image / height-map items of silx plot3d (pygfx
//! `17_3d_image_heightmap`): a colormapped [`ImageData3D`] and an [`ImageRgba3D`]
//! laid flat as textured quads, beside a [`HeightMapData`] surface that lifts the
//! same kind of field into Z and colours it by height. The three are spread along
//! X so each is clearly visible.
//!
//! Left-drag orbits, right-drag pans, wheel zooms.
//!
//! Run with: `cargo run --example scene3d_image`

use eframe::egui;
use rsplot::egui::Color32;
use rsplot::{
    Colormap, ColormapName, HeightMapData, ImageData3D, ImageRgba3D, Scene3dGeometry, SceneWidget,
    Vec3,
};

const W: usize = 20;

struct ImageApp {
    scene: SceneWidget,
}

impl ImageApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let mut geometry = Scene3dGeometry::new();

        // A colormapped data image (Gaussian), flat at z = 0, to the left.
        ImageData3D::new()
            .with_data(&gaussian(), W, W)
            .with_colormap(Colormap::new(ColormapName::Viridis, 0.0, 1.0))
            .with_origin([-24.0, 0.0, 0.0])
            .append_to(&mut geometry);

        // An RGBA image (colour gradient), flat at z = 0, to the right.
        ImageRgba3D::new()
            .with_data(&gradient(), W, W)
            .with_origin([24.0, 0.0, 0.0])
            .append_to(&mut geometry);

        // The same Gaussian as a height-map surface, coloured by height (magma),
        // in the centre at its natural grid coordinates.
        let bump: Vec<f32> = gaussian().iter().map(|&v| 6.0 * v as f32).collect();
        let bump64: Vec<f64> = bump.iter().map(|&h| h as f64).collect();
        HeightMapData::new()
            .with_data(&bump, W, W)
            .with_colormapped_data(&bump64, W, W)
            .with_colormap(Colormap::new(ColormapName::Magma, 0.0, 6.0))
            .append_to(&mut geometry);

        let mut scene = SceneWidget::new(rs, 0);
        scene.set_bounds(
            rs,
            (Vec3::new(-24.0, 0.0, 0.0), Vec3::new(44.0, W as f32, 6.0)),
        );
        scene.set_geometry(rs, geometry);
        Self { scene }
    }
}

impl eframe::App for ImageApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.scene.show(ui);
        });
    }
}

/// A centred Gaussian bump over the `W×W` grid, values in `[0, 1]` (row-major).
fn gaussian() -> Vec<f64> {
    let mut data = Vec::with_capacity(W * W);
    for row in 0..W {
        for col in 0..W {
            let cx = (col as f64 - W as f64 / 2.0) / (W as f64 / 4.0);
            let cy = (row as f64 - W as f64 / 2.0) / (W as f64 / 4.0);
            data.push((-0.5 * (cx * cx + cy * cy)).exp());
        }
    }
    data
}

/// A red→green (X) × blue (Y) colour gradient over the `W×W` grid.
fn gradient() -> Vec<Color32> {
    let mut pixels = Vec::with_capacity(W * W);
    for row in 0..W {
        for col in 0..W {
            let r = (255 * col / (W - 1)) as u8;
            let g = (255 * (W - 1 - col) / (W - 1)) as u8;
            let b = (255 * row / (W - 1)) as u8;
            pixels.push(Color32::from_rgb(r, g, b));
        }
    }
    pixels
}

fn main() -> eframe::Result {
    eframe::run_native(
        "rsplot: 3D image & height map",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(ImageApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
