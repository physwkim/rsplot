//! [`SceneWindow`] example — the composed plot3d window.
//!
//! Mirrors the scalar-field part of silx `examples/plot3dSceneWindow.py`: the
//! same `sinc` volume carrying two iso-surfaces (levels `0.2` translucent red and
//! `0.5` opaque blue, as in silx) and a visible cutting plane with the `jet`
//! colormap, hosted in the composed window:
//!
//! - a **View** toolbar (viewpoint presets) + a Properties toggle,
//! - the [`ScalarFieldView`](rsplot::ScalarFieldView) scene,
//! - a `ScalarFieldProperties` side panel (cut-plane visibility / colormap /
//!   value range / autoscale, per-iso level/colour/add/remove), and
//! - a **position/value readout** along the bottom (silx `PositionInfoWidget`):
//!   hover the scene and the X / Y / Z / Data fields track the picked point.
//!
//! Run with: `cargo run --example scene_window`

use eframe::egui;
use rsplot::egui::Color32;
use rsplot::egui_wgpu::RenderState;
use rsplot::{Colormap, ColormapName, SceneWindow, Vec3};

/// Volume grid size per axis (silx uses 64).
const N: usize = 64;

struct SceneWindowApp {
    window: SceneWindow,
    rs: RenderState,
}

impl SceneWindowApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let data = sinc_volume();
        let mut window = SceneWindow::new(rs, 0);
        assert!(
            window.view_mut().set_data(rs, &data, N, N, N),
            "cubic volume is valid"
        );

        // Two iso-surfaces, matching silx: 0.2 → #FF000080, 0.5 → #0000FFFF.
        window
            .view_mut()
            .add_isosurface(rs, 0.2, Color32::from_rgba_unmultiplied(255, 0, 0, 128));
        window
            .view_mut()
            .add_isosurface(rs, 0.5, Color32::from_rgb(0, 0, 255));

        // A visible cutting plane through the volume centre, jet colormap.
        {
            let field = window.view_mut().field_mut();
            let plane = field.cut_plane_mut();
            plane.set_point(Vec3::new(N as f32 / 2.0, N as f32 / 2.0, N as f32 / 2.0));
            plane.set_colormap(Colormap::new(ColormapName::Jet, -0.25, 1.0));
            plane.set_visible(true);
        }
        window.view_mut().rebuild(rs);

        Self {
            window,
            rs: rs.clone(),
        }
    }
}

impl eframe::App for SceneWindowApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.window.show(ui, &self.rs);
    }
}

/// The silx dummy volume: `sinc(x·y·z)` over `[-10, 10]³`, row-major
/// `(depth, height, width)`; `sin(t)/t → 1` at `t → 0`.
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
        "rsplot: scene window",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(SceneWindowApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
