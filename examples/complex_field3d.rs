//! [`ComplexField3D`] example — a 3D complex field with selectable projection.
//!
//! Ports the use of silx `items.volume.ComplexField3D`: a complex volume
//! `(re, im)` projected to a real scalar through a [`ComplexMode`], then rendered
//! as an iso-surface (the scalar-field path). The data is a Gaussian-enveloped
//! plane wave `amp·e^{iφ}`, so the projection mode visibly changes the surface:
//! `Absolute` wraps the Gaussian core, `Phase`/`Real`/`Imaginary` cut the wave.
//!
//! Pick a mode from the combo box: silx `setComplexMode` re-projects the field
//! and clears the iso-surfaces, so the example re-adds an auto (`mean + std`)
//! iso-surface for the new projection and re-uploads.
//!
//! Run with: `cargo run --example complex_field3d`

use eframe::egui;
use rsplot::egui::Color32;
use rsplot::egui_wgpu::RenderState;
use rsplot::{ComplexField3D, ComplexMode, Scene3dGeometry, SceneWidget, mean_plus_std};

const N: usize = 40;
const ISO_COLOR: Color32 = Color32::from_rgb(255, 0, 255);

/// The scalar projection modes meaningful for an iso-surface (the two HSV
/// amplitude-phase composites are 2D-image modes with no 3D scalar).
const MODES: [(ComplexMode, &str); 6] = [
    (ComplexMode::Absolute, "Absolute |z|"),
    (ComplexMode::Phase, "Phase ∠z"),
    (ComplexMode::Real, "Real"),
    (ComplexMode::Imaginary, "Imaginary"),
    (ComplexMode::SquareAmplitude, "Square amplitude |z|²"),
    (ComplexMode::Log10Amplitude, "log₁₀|z|"),
];

struct ComplexApp {
    field: ComplexField3D,
    scene: SceneWidget,
    rs: RenderState,
    mode: ComplexMode,
}

impl ComplexApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let (re, im) = plane_wave();
        let field = ComplexField3D::new().with_data(&re, &im, N, N, N);

        let mut scene = SceneWidget::new(rs, 0);
        if let Some(bounds) = field.bounds() {
            scene.set_bounds(rs, bounds);
        }

        let mut app = Self {
            field,
            scene,
            rs: rs.clone(),
            mode: ComplexMode::Absolute,
        };
        app.apply_mode(ComplexMode::Absolute);
        app
    }

    /// Switch the complex projection, re-add the auto iso-surface (silx clears
    /// iso-surfaces on `setComplexMode`), and re-upload.
    fn apply_mode(&mut self, mode: ComplexMode) {
        self.mode = mode;
        self.field.set_complex_mode(mode);
        self.field
            .field_mut()
            .add_auto_isosurface(mean_plus_std, ISO_COLOR);
        let mut geometry = Scene3dGeometry::new();
        self.field.append_to(&mut geometry);
        self.scene.set_geometry(&self.rs, geometry);
    }
}

impl eframe::App for ComplexApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top(ui.id().with("complex_toolbar")).show_inside(ui, |ui| {
            let current = MODES
                .iter()
                .find(|(m, _)| *m == self.mode)
                .map(|(_, name)| *name)
                .unwrap_or("");
            let mut chosen = self.mode;
            egui::ComboBox::from_label("Complex mode")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (mode, name) in MODES {
                        ui.selectable_value(&mut chosen, mode, name);
                    }
                });
            if chosen != self.mode {
                self.apply_mode(chosen);
            }
        });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.scene.show(ui);
        });
    }
}

/// A Gaussian-enveloped plane wave `amp·e^{iφ}` over `[-1, 1]³`, returned as
/// `(re, im)` row-major `(depth, height, width)`.
fn plane_wave() -> (Vec<f32>, Vec<f32>) {
    let coord = |i: usize| -1.0 + 2.0 * i as f32 / (N - 1) as f32;
    let n = N * N * N;
    let (mut re, mut im) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for z in 0..N {
        for y in 0..N {
            for x in 0..N {
                let (cx, cy, cz) = (coord(x), coord(y), coord(z));
                let amp = (-3.0 * (cx * cx + cy * cy + cz * cz)).exp();
                let phase = 6.0 * (cx + cy + cz);
                re.push(amp * phase.cos());
                im.push(amp * phase.sin());
            }
        }
    }
    (re, im)
}

fn main() -> eframe::Result {
    eframe::run_native(
        "rsplot: complex field 3D",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(ComplexApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
