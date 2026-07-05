//! Active-curve axis-label swap + right-click zoom menu.
//!
//! Demonstrates two recently-added high-level `PlotWidget` behaviors:
//!
//! 1. **Active-curve axis-label swap** (silx `Plot._setActiveItem`). Each curve
//!    carries its own X/Y labels (silx `addCurve(xlabel=, ylabel=)`). When a
//!    curve becomes the *active* curve, its labels OVERRIDE the graph's default
//!    axis labels; when no curve is active, the graph defaults show. A curve
//!    bound to the right (Y2) axis routes its Y label to the right axis, leaving
//!    the left axis on its graph default.
//!
//!    Click a curve in the plot (active-curve handling is on) — or click a row
//!    in the side-panel legend — and watch the axis labels swap. "Clear active"
//!    restores the `(graph default ...)` labels so you can see the fallback.
//!
//! 2. **Right-click zoom context menu** (silx `PlotWidget.contextMenuEvent`).
//!    Right-click anywhere on the plot for a `Zoom Back` / `Reset Zoom` menu.
//!    Wheel-zoom or drag-zoom in first, then right-click to step back or reset.
//!
//! Run with: `cargo run --example high_level_active_curve_labels`

use eframe::egui;
use egui::Color32;
use rsplot::{CurveSpec, GraphGrid, ItemHandle, PlotInteractionMode, PlotWidget, YAxis};

const N: usize = 400;
const T_MAX: f64 = 10.0;

/// One curve's display metadata, kept so the side panel can report which axis
/// the active curve's Y label routes to and echo its labels.
struct CurveInfo {
    handle: ItemHandle,
    x_label: &'static str,
    y_label: &'static str,
    axis: YAxis,
}

struct ActiveLabelApp {
    plot: PlotWidget,
    curves: Vec<CurveInfo>,
}

impl ActiveLabelApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");

        let mut plot = PlotWidget::new(render_state, 0);
        plot.set_graph_title("Active-curve axis labels");
        // Graph defaults: shown whenever no curve is active (silx _defaultLabel).
        plot.set_graph_x_label("(graph default) sample");
        plot.set_graph_y_label("(graph default) left signal", YAxis::Left);
        plot.set_graph_y_label("(graph default) right signal", YAxis::Right);
        plot.set_graph_cursor(true);
        plot.set_graph_grid_mode(GraphGrid::MajorAndMinor);
        plot.set_interaction_mode(PlotInteractionMode::Zoom);
        // Clicking a curve makes it the active curve, swapping in its labels.
        plot.set_active_curve_handling(true);

        let xs: Vec<f64> = (0..N).map(|i| T_MAX * i as f64 / (N - 1) as f64).collect();

        // Left-axis curve A: voltage.
        let voltage: Vec<f64> = xs.iter().map(|&t| (t * 1.4).sin()).collect();
        let voltage_h = add_labeled_curve(
            &mut plot,
            &xs,
            &voltage,
            Color32::from_rgb(120, 180, 255),
            "Time [s]",
            "Voltage [V]",
            YAxis::Left,
        );

        // Left-axis curve B: current.
        let current: Vec<f64> = xs.iter().map(|&t| 0.6 * (t * 1.4 + 0.9).cos()).collect();
        let current_h = add_labeled_curve(
            &mut plot,
            &xs,
            &current,
            Color32::from_rgb(120, 220, 140),
            "Time [s]",
            "Current [A]",
            YAxis::Left,
        );

        // Right (Y2) axis curve C: temperature. Its Y label routes to the
        // right axis when active; the left axis stays on its graph default.
        let temperature: Vec<f64> = xs
            .iter()
            .map(|&t| 20.0 + 60.0 * (1.0 - (-t / 3.0).exp()))
            .collect();
        let temperature_h = add_labeled_curve(
            &mut plot,
            &xs,
            &temperature,
            Color32::from_rgb(255, 160, 80),
            "Time [s]",
            "Temperature [\u{b0}C]",
            YAxis::Right,
        );

        // Fixed ranges so both axes read naturally side by side.
        plot.set_graph_y_limits(-1.5, 1.5, YAxis::Left);
        plot.set_graph_y_limits(0.0, 100.0, YAxis::Right);

        plot.set_item_legend(voltage_h, "Voltage (left)");
        plot.set_item_legend(current_h, "Current (left)");
        plot.set_item_legend(temperature_h, "Temperature (right / Y2)");

        // Start with no active curve so the graph-default labels show first.
        plot.set_active_curve(None);
        plot.drain_events();

        let curves = vec![
            CurveInfo {
                handle: voltage_h,
                x_label: "Time [s]",
                y_label: "Voltage [V]",
                axis: YAxis::Left,
            },
            CurveInfo {
                handle: current_h,
                x_label: "Time [s]",
                y_label: "Current [A]",
                axis: YAxis::Left,
            },
            CurveInfo {
                handle: temperature_h,
                x_label: "Time [s]",
                y_label: "Temperature [\u{b0}C]",
                axis: YAxis::Right,
            },
        ];

        Self { plot, curves }
    }
}

/// Add a curve carrying its own per-curve X/Y labels on the chosen axis.
fn add_labeled_curve(
    plot: &mut PlotWidget,
    x: &[f64],
    y: &[f64],
    color: Color32,
    x_label: &str,
    y_label: &str,
    axis: YAxis,
) -> ItemHandle {
    let mut spec = CurveSpec::new(x, y, color);
    spec.line_width = 2.0;
    spec.y_axis = axis;
    spec.x_label = Some(x_label);
    spec.y_label = Some(y_label);
    plot.add_curve_spec(spec)
}

impl eframe::App for ActiveLabelApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::right("active_label_panel")
            .resizable(true)
            .default_size(260.0)
            .show_inside(ui, |ui| {
                ui.heading("Curve legend");
                // The graph legend shows each curve's icon (color, line style,
                // marker) and makes the clicked curve active (silx
                // CurveLegendsWidget). Activating a curve swaps in its axis labels,
                // echoed below.
                self.plot.show_legend(ui);

                // The legend has no "no active curve" row, so offer an explicit
                // reset to the graph-default labels (silx setActiveCurve(None)).
                if ui.button("Clear active (graph defaults)").clicked() {
                    self.plot.set_active_curve(None);
                }

                ui.separator();
                ui.heading("Displayed axis labels");
                // Read after the legend so a click this frame is reflected now.
                let active = self.plot.active_curve();
                match active.and_then(|h| self.curves.iter().find(|c| c.handle == h)) {
                    Some(info) => {
                        ui.label(format!("X: {}", info.x_label));
                        match info.axis {
                            YAxis::Left => {
                                ui.label(format!("Y (left): {}", info.y_label));
                                ui.label("Y2 (right): (graph default) right signal");
                            }
                            YAxis::Right => {
                                ui.label("Y (left): (graph default) left signal");
                                ui.label(format!("Y2 (right): {}", info.y_label));
                            }
                            YAxis::Extra(n) => {
                                ui.label(format!("Y (extra {n}): {}", info.y_label));
                            }
                        }
                    }
                    None => {
                        ui.label("X: (graph default) sample");
                        ui.label("Y (left): (graph default) left signal");
                        ui.label("Y2 (right): (graph default) right signal");
                    }
                }

                ui.separator();
                ui.label(
                    "Click a legend row to activate that curve; its labels override the graph defaults.",
                );
                ui.label("Right-click the plot for Zoom Back / Reset Zoom.");
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.plot.show_toolbar(ui);
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
        "rsplot - active-curve axis labels",
        options,
        Box::new(|cc| Ok(Box::new(ActiveLabelApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
