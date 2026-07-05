//! [`ScalarFieldView`] example — the plot3d flagship.
//!
//! Mirrors silx `examples/viewer3DVolume.py`: a 3D scalar field displayed with an
//! iso-surface (and an interactive cutting plane). The data is the same dummy
//! volume silx generates when no file is given — the `sinc` field
//! `sin(x·y·z) / (x·y·z)` sampled over `[-10, 10]³` — and the iso-surface uses
//! silx's default auto level `mean + std`.
//!
//! Left-drag orbits, right-drag pans, wheel zooms. (The cut plane and per-iso
//! levels are exposed interactively by the composed window in the
//! `scene_window` example, via the `ScalarFieldProperties` panel.)
//!
//! Run with: `cargo run --example scalar_field_view`

use eframe::egui;
use rsplot::egui::Color32;
use rsplot::{ScalarFieldView, mean_plus_std};

/// Volume grid size per axis (silx uses 64).
const N: usize = 64;

struct ScalarFieldApp {
    view: ScalarFieldView,
}

impl ScalarFieldApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let data = sinc_volume();
        let mut view = ScalarFieldView::new(rs, 0);
        assert!(view.set_data(rs, &data, N, N, N), "cubic volume is valid");
        // silx: window.addIsosurface(default_isolevel, "#FF0000FF") — auto level
        // mean + std, opaque red.
        view.add_auto_isosurface(rs, mean_plus_std, Color32::from_rgb(255, 0, 0));

        Self { view }
    }
}

impl eframe::App for ScalarFieldApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.view.show(ui);
        });
    }
}

/// The silx dummy volume: `sinc(x·y·z)` over `[-10, 10]³`, row-major
/// `(depth, height, width)`. At `t = x·y·z → 0` the limit `sin(t)/t = 1`
/// is used (no NaN), which is the true `sinc` value rather than silx's `0/0`.
fn sinc_volume() -> Vec<f32> {
    let coord = |i: usize| -10.0 + 20.0 * i as f32 / (N - 1) as f32;
    let mut data = vec![0.0f32; N * N * N];
    for z in 0..N {
        for y in 0..N {
            for x in 0..N {
                let t = coord(x) * coord(y) * coord(z);
                let v = if t.abs() < 1e-9 { 1.0 } else { t.sin() / t };
                data[(z * N + y) * N + x] = v;
            }
        }
    }
    data
}

fn main() -> eframe::Result {
    eframe::run_native(
        "rsplot: scalar field view",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(ScalarFieldApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
