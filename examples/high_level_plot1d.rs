//! High-level Plot1D example.
//!
//! Mirrors the 1D parts of silx examples `plotStats.py` and
//! `plotLegendsWidget.py`: curves, scatter, histogram, legend selection, stats,
//! and item lookup/removal by legend.
//!
//! Run with: `cargo run --example high_level_plot1d`

use eframe::egui;
use egui_silx::{CurveSpec, GraphGrid, Plot1D, PlotEvent};

const N: usize = 360;

struct Plot1dApp {
    plot: Plot1D,
    curve: egui_silx::ItemHandle,
    phase: f64,
    events: Vec<String>,
}

impl Plot1dApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        let mut plot = Plot1D::new(render_state, 0);
        plot.set_graph_title("Plot1D high-level items");
        plot.set_graph_grid_mode(GraphGrid::MajorAndMinor);
        plot.set_graph_cursor(true);

        let x = x_values();
        let y = sine_values(&x, 0.0);
        let curve = plot.add_curve_with_legend(&x, &y, egui::Color32::YELLOW, "phase sine");

        let (sx, sy) = scatter_values();
        plot.add_scatter_with_legend(&sx, &sy, egui::Color32::LIGHT_BLUE, "sample points");

        let (edges, counts) = histogram_values();
        plot.add_histogram_with_legend(&edges, &counts, egui::Color32::LIGHT_GREEN, "counts")
            .expect("histogram edges are bins + 1");

        plot.set_active_item(Some(curve));
        plot.drain_events();

        Self {
            plot,
            curve,
            phase: 0.0,
            events: Vec::new(),
        }
    }

    fn update_curve(&mut self) {
        let x = x_values();
        let y = sine_values(&x, self.phase);
        self.plot
            .update_curve_spec(self.curve, CurveSpec::new(&x, &y, egui::Color32::YELLOW));
    }

    fn remember_events(&mut self) {
        for event in self.plot.drain_events() {
            self.events.push(format_event(event));
        }
        let keep = 8;
        if self.events.len() > keep {
            self.events.drain(0..self.events.len() - keep);
        }
    }
}

impl eframe::App for Plot1dApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::right("plot1d_inspector")
            .resizable(true)
            .default_size(230.0)
            .show_inside(ui, |ui| {
                ui.heading("Legends");
                self.plot.show_legend(ui);
                ui.separator();
                ui.heading("Active stats");
                self.plot.show_active_stats(ui);
                ui.separator();
                ui.heading("Events");
                self.remember_events();
                for event in self.events.iter().rev() {
                    ui.label(event);
                }
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            let scatter_present = self.plot.scatter_by_legend("sample points").is_some();
            let mut phase = self.phase;
            let mut phase_changed = false;
            let (_, (activate_scatter, toggle_scatter)) =
                self.plot.show_toolbar_with(ui, |ui, _plot| {
                    phase_changed = ui
                        .add(
                            egui::Slider::new(&mut phase, 0.0..=std::f64::consts::TAU)
                                .text("phase")
                                .max_decimals(2),
                        )
                        .changed();
                    let activate_scatter = ui.button("Activate scatter").clicked();
                    let label = if scatter_present {
                        "Remove scatter"
                    } else {
                        "Re-add scatter"
                    };
                    let toggle_scatter = ui.button(label).clicked();
                    (activate_scatter, toggle_scatter)
                });

            if phase_changed {
                self.phase = phase;
                self.update_curve();
            }

            if activate_scatter && let Some(handle) = self.plot.scatter_by_legend("sample points") {
                self.plot.set_active_item(Some(handle));
            }

            if toggle_scatter {
                if scatter_present {
                    if let Some(handle) = self.plot.scatter_by_legend("sample points") {
                        self.plot.remove_scatter(handle);
                    }
                } else {
                    let (sx, sy) = scatter_values();
                    self.plot.add_scatter_with_legend(
                        &sx,
                        &sy,
                        egui::Color32::LIGHT_BLUE,
                        "sample points",
                    );
                }
            }
            self.plot.show(ui);
        });
    }
}

fn x_values() -> Vec<f64> {
    (0..N).map(|i| i as f64 / (N - 1) as f64 * 12.0).collect()
}

fn sine_values(x: &[f64], phase: f64) -> Vec<f64> {
    x.iter()
        .map(|x| (x * 1.7 + phase).sin() + 0.25 * (x * 0.4).cos())
        .collect()
}

fn scatter_values() -> (Vec<f64>, Vec<f64>) {
    let mut x = Vec::with_capacity(32);
    let mut y = Vec::with_capacity(32);
    for i in 0..32 {
        let t = i as f64 / 31.0;
        x.push(12.0 * t);
        y.push(0.8 * (t * std::f64::consts::TAU * 2.0).cos() + 0.2 * (i % 5) as f64);
    }
    (x, y)
}

fn histogram_values() -> (Vec<f64>, Vec<f64>) {
    let edges: Vec<f64> = (0..=18).map(|i| i as f64 * 12.0 / 18.0).collect();
    let counts = edges
        .windows(2)
        .map(|edge| {
            let c = 0.5 * (edge[0] + edge[1]);
            1.5 + 3.0 * (-(c - 7.0).powi(2) / 7.0).exp()
        })
        .collect();
    (edges, counts)
}

fn format_event(event: PlotEvent) -> String {
    match event {
        PlotEvent::ItemAdded { handle, kind } => format!("added {kind:?} #{handle}"),
        PlotEvent::ItemUpdated { handle, kind } => format!("updated {kind:?} #{handle}"),
        PlotEvent::ItemRemoved { handle, kind } => format!("removed {kind:?} #{handle}"),
        PlotEvent::ActiveItemChanged { previous, current } => {
            format!("active {previous:?} -> {current:?}")
        }
        PlotEvent::LimitsChanged { x, y, y2 } => {
            format!("limits x{x:?} y{y:?} y2{y2:?}")
        }
        PlotEvent::RoiChanged { index } => format!("roi changed #{index}"),
        PlotEvent::RoiCreated { index } => format!("roi created #{index}"),
        PlotEvent::DrawingProgress { mode, points } => {
            format!("drawing {mode:?} ({} pts)", points.len())
        }
        PlotEvent::DrawingFinished { mode, .. } => format!("drawing finished {mode:?}"),
        PlotEvent::RoisCleared => "rois cleared".to_owned(),
        PlotEvent::CurrentRoiChanged { previous, current } => {
            format!("current roi {previous:?} -> {current:?}")
        }
        PlotEvent::MarkerMoved { handle } => format!("marker moved #{handle}"),
        PlotEvent::MarkerDragStarted { handle } => format!("marker drag start #{handle}"),
        PlotEvent::MarkerDragFinished { handle } => format!("marker drag finish #{handle}"),
        PlotEvent::CurveClicked {
            handle,
            index,
            x,
            y,
            button,
        } => format!("curve #{handle} clicked @ pt {index} ({x:.3},{y:.3}) [{button:?}]"),
        PlotEvent::ImageClicked {
            handle,
            col,
            row,
            button,
        } => format!("image #{handle} clicked @ ({col},{row}) [{button:?}]"),
        PlotEvent::ItemClicked { handle, button } => format!("item #{handle} clicked [{button:?}]"),
        PlotEvent::ItemHovered {
            handle,
            kind,
            draggable,
            ..
        } => format!(
            "hover {kind:?} #{handle}{}",
            if draggable { " (draggable)" } else { "" }
        ),
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "egui-silx - high-level Plot1D",
        options,
        Box::new(|cc| Ok(Box::new(Plot1dApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
