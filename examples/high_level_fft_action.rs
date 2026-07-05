//! Custom toolbar FFT action example.
//!
//! Mirrors silx `examples/fftPlotAction.py`: a custom toggle button in the
//! toolbar that computes the DFT amplitude spectrum of all curves when active
//! and restores the originals when toggled off.
//!
//! Demonstrates `show_toolbar_with` for injecting application-specific actions
//! into the standard rsplot toolbar.
//!
//! Run with: `cargo run --example high_level_fft_action`

use eframe::egui;
use rsplot::{CurveData, ItemHandle, Plot1D, YAxis};
use std::f64::consts::PI;

const N: usize = 256;

struct FftApp {
    plot: Plot1D,
    /// True = currently showing FFT; false = showing original.
    fft_active: bool,
    /// Handle to the signal curve (updated in place).
    curve_handle: ItemHandle,
    /// Original time-domain signal.
    signal_x: Vec<f64>,
    signal_y: Vec<f64>,
    /// Precomputed FFT amplitude spectrum.
    fft_x: Vec<f64>,
    fft_y: Vec<f64>,
}

impl FftApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let mut plot = Plot1D::new(rs, 0);
        plot.set_graph_title("FFT action — custom toolbar button");
        plot.set_graph_x_label("time");
        plot.set_graph_y_label("amplitude", YAxis::Left);

        let signal_x: Vec<f64> = (0..N).map(|i| i as f64 / N as f64).collect();
        let signal_y: Vec<f64> = signal_x
            .iter()
            .map(|&t| (2.0 * PI * 5.0 * t).sin() + 0.5 * (2.0 * PI * 17.0 * t).sin())
            .collect();

        let curve_handle =
            plot.add_curve_with_legend(&signal_x, &signal_y, egui::Color32::YELLOW, "signal");

        let (fft_x, fft_y) = dft_amplitude(&signal_y, N as f64);

        Self {
            plot,
            fft_active: false,
            curve_handle,
            signal_x,
            signal_y,
            fft_x,
            fft_y,
        }
    }
}

impl eframe::App for FftApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let (_, toggled) = self.plot.show_toolbar_with(ui, |ui, _plot| {
            ui.separator();
            ui.selectable_label(self.fft_active, "FFT")
                .on_hover_text("Toggle FFT amplitude spectrum")
                .clicked()
        });

        if toggled {
            self.fft_active = !self.fft_active;

            if self.fft_active {
                // Switch to frequency domain.
                self.plot.set_graph_x_label("frequency (Hz)");
                self.plot.set_graph_y_label("|X(f)|", YAxis::Left);
                self.plot
                    .set_item_legend(self.curve_handle, "DFT amplitude");
                let curve = CurveData::new(
                    self.fft_x.clone(),
                    self.fft_y.clone(),
                    egui::Color32::LIGHT_BLUE,
                );
                self.plot.update_curve_data(self.curve_handle, &curve);
            } else {
                // Restore time domain.
                self.plot.set_graph_x_label("time");
                self.plot.set_graph_y_label("amplitude", YAxis::Left);
                self.plot.set_item_legend(self.curve_handle, "signal");
                let curve = CurveData::new(
                    self.signal_x.clone(),
                    self.signal_y.clone(),
                    egui::Color32::YELLOW,
                );
                self.plot.update_curve_data(self.curve_handle, &curve);
            }
        }

        self.plot.show(ui);
    }
}

/// Compute the one-sided DFT amplitude spectrum of `signal`.
/// Returns `(freqs, amplitudes)` for bins 0..N/2.
fn dft_amplitude(signal: &[f64], sample_rate: f64) -> (Vec<f64>, Vec<f64>) {
    let n = signal.len();
    let half = n / 2;
    let mut freqs = Vec::with_capacity(half);
    let mut amps = Vec::with_capacity(half);

    for k in 0..half {
        let mut re = 0.0f64;
        let mut im = 0.0f64;
        for (j, &s) in signal.iter().enumerate() {
            let angle = -2.0 * PI * k as f64 * j as f64 / n as f64;
            re += s * angle.cos();
            im += s * angle.sin();
        }
        freqs.push(k as f64 * sample_rate / n as f64);
        amps.push((re * re + im * im).sqrt() / n as f64);
    }
    (freqs, amps)
}

fn main() -> eframe::Result {
    eframe::run_native(
        "rsplot: FFT action",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(FftApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
