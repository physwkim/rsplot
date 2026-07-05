//! Curve-legend icons: line style, color, and marker per curve.
//!
//! Ports silx's `plotCurveLegendWidget.py` demo (`CurveLegendsWidget`): a side
//! panel listing each curve's legend next to an icon that shows the curve's
//! **color**, **line style** (dashed / dotted / solid), and **marker symbol** —
//! exactly as the curve is drawn, so the legend reads like matplotlib's.
//!
//! The three curves mirror the silx example:
//!
//! - `random` — red, dashed line (`--`), square markers (`s`)
//! - `sin`    — blue, dotted line (`:`), circle markers (`o`)
//! - `cos`    — blue, solid line (`-`), no marker
//!
//! Click a legend row to make that curve active; click the eye to hide it;
//! right-click for Set Active / Map to Y axis / Points / Lines (silx
//! `LegendListContextMenu`).
//!
//! Run with: `cargo run --example high_level_curve_legend`

use eframe::egui;
use egui::Color32;
use rsplot::{
    CurveSpec, GraphGrid, ItemHandle, LineStyle, PlotInteractionMode, PlotWidget, Symbol,
};

const N: usize = 100;
const PI: f64 = std::f64::consts::PI;

struct CurveLegendApp {
    plot: PlotWidget,
}

impl CurveLegendApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");

        let mut plot = PlotWidget::new(render_state, 0);
        plot.set_graph_title("CurveLegendWidgets demo");
        plot.set_graph_x_label("X");
        plot.set_graph_y_label("Y", rsplot::YAxis::Left);
        plot.set_graph_cursor(true);
        plot.set_graph_grid_mode(GraphGrid::MajorAndMinor);
        plot.set_interaction_mode(PlotInteractionMode::Zoom);
        // Clicking a legend row activates that curve (silx _switchCurveActive).
        plot.set_active_curve_handling(true);

        let x: Vec<f64> = (0..N)
            .map(|i| -PI + 2.0 * PI * i as f64 / (N - 1) as f64)
            .collect();

        // random: red, dashed, square markers.
        let random: Vec<f64> = (0..N).map(|i| 2.0 * pseudo_random(i) - 1.0).collect();
        add_styled_curve(
            &mut plot,
            &x,
            &random,
            "random",
            Color32::RED,
            LineStyle::Dashed,
            Some(Symbol::Square),
        );

        // sin: blue, dotted, circle markers.
        let sin: Vec<f64> = x.iter().map(|&t| t.sin()).collect();
        add_styled_curve(
            &mut plot,
            &x,
            &sin,
            "sin",
            Color32::BLUE,
            LineStyle::Dotted,
            Some(Symbol::Circle),
        );

        // cos: blue, solid, no marker.
        let cos: Vec<f64> = x.iter().map(|&t| t.cos()).collect();
        add_styled_curve(
            &mut plot,
            &x,
            &cos,
            "cos",
            Color32::BLUE,
            LineStyle::Solid,
            None,
        );

        plot.drain_events();
        Self { plot }
    }
}

/// Add a curve carrying its own color, line style, and marker symbol, then label
/// it for the legend.
fn add_styled_curve(
    plot: &mut PlotWidget,
    x: &[f64],
    y: &[f64],
    legend: &str,
    color: Color32,
    line_style: LineStyle,
    symbol: Option<Symbol>,
) -> ItemHandle {
    let mut spec = CurveSpec::new(x, y, color);
    spec.line_width = 1.5;
    spec.line_style = line_style;
    spec.symbol = symbol;
    spec.symbol_size = 7.0;
    let handle = plot.add_curve_spec(spec);
    plot.set_item_legend(handle, legend);
    handle
}

/// Deterministic pseudo-random value in `[0, 1)` (a splitmix64 step), so the
/// "random" curve looks noisy without pulling in an RNG dependency.
fn pseudo_random(i: usize) -> f64 {
    let mut z = (i as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0x1234_5678_9ABC_DEF0);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

impl eframe::App for CurveLegendApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::right("curve_legend_panel")
            .resizable(true)
            .default_size(220.0)
            .show_inside(ui, |ui| {
                ui.heading("Curve legends");
                self.plot.show_legend(ui);

                ui.separator();
                ui.label("Each icon shows the curve's color, line style, and marker.");
                ui.label("Click a row to activate it; the eye toggles visibility.");
                ui.label("Right-click a row for Set Active / Map to Y axis / Points / Lines.");
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
        "rsplot - curve legend icons",
        options,
        Box::new(|cc| Ok(Box::new(CurveLegendApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
