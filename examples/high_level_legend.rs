//! High-level legend example.
//!
//! Shows the retained legend list with multiple named curves, representative
//! 2D items, overlay items, and annotation item kinds.
//!
//! Run with: `cargo run --example high_level_legend`

use eframe::egui;
use rsplot::{
    Colormap, GraphGrid, ImageGeometry, LineStyle, PlotEvent, PlotInteractionMode, PlotItemKind,
    PlotWidget, ShapeSpec, Symbol, YAxis,
};

const WIDTH: u32 = 120;
const HEIGHT: u32 = 90;

struct LegendDemoApp {
    plot: PlotWidget,
    events: Vec<String>,
}

impl LegendDemoApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        let mut plot = PlotWidget::new(render_state, 0);
        plot.set_graph_title("Legend item styles");
        plot.set_graph_x_label("X");
        plot.set_graph_y_label("Y", YAxis::Left);
        plot.set_graph_cursor(true);
        plot.set_graph_grid_mode(GraphGrid::MajorAndMinor);
        plot.set_interaction_mode(PlotInteractionMode::Select);
        plot.set_default_colormap(Colormap::viridis(-0.6, 1.2));

        let image = build_image();
        let image_handle = plot
            .add_image_with_geometry(
                WIDTH,
                HEIGHT,
                &image,
                Colormap::viridis(-0.6, 1.2),
                ImageGeometry {
                    origin: (0.0, -1.0),
                    scale: (0.1, 0.08),
                    alpha: 0.72,
                },
            )
            .expect("generated image length matches dimensions");
        plot.set_item_legend(image_handle, "intensity image");

        let mask = build_mask(&image);
        let mask_handle = plot
            .add_mask_with_geometry(
                WIDTH,
                HEIGHT,
                &mask,
                egui::Color32::from_rgba_unmultiplied(255, 80, 120, 90),
                ImageGeometry {
                    origin: (0.0, -1.0),
                    scale: (0.1, 0.08),
                    alpha: 1.0,
                },
            )
            .expect("generated mask length matches dimensions");
        plot.set_item_legend(mask_handle, "threshold mask");

        let x = x_values();
        let temperature: Vec<f64> = x.iter().map(|x| 1.8 + (x * 1.8).sin()).collect();
        plot.add_curve_with_legend(
            &x,
            &temperature,
            egui::Color32::from_rgb(245, 220, 72),
            "temperature",
        );

        let pressure: Vec<f64> = x
            .iter()
            .map(|x| 2.25 + (x * 1.15 + 0.8).cos() * 0.75)
            .collect();
        plot.add_curve_with_legend(
            &x,
            &pressure,
            egui::Color32::from_rgb(80, 170, 255),
            "pressure",
        );

        let reference: Vec<f64> = x.iter().map(|x| 0.65 + 0.22 * x).collect();
        plot.add_curve_with_legend(
            &x,
            &reference,
            egui::Color32::from_rgb(255, 120, 95),
            "reference",
        );

        let scatter_x: Vec<f64> = (0..18).map(|i| i as f64 * 0.65).collect();
        let scatter_y: Vec<f64> = scatter_x
            .iter()
            .enumerate()
            .map(|(i, x)| -0.2 + (x * 1.4).cos() * 0.7 + (i % 3) as f64 * 0.18)
            .collect();
        plot.add_scatter_with_legend(
            &scatter_x,
            &scatter_y,
            egui::Color32::from_rgb(80, 190, 255),
            "scatter",
        );

        let edges: Vec<f64> = (0..=13).map(|i| i as f64 * 0.9).collect();
        let counts: Vec<f64> = edges
            .windows(2)
            .map(|edge| {
                let c = 0.5 * (edge[0] + edge[1]);
                -1.0 + 1.4 * (-(c - 5.0).powi(2) / 5.0).exp()
            })
            .collect();
        plot.add_histogram_with_legend(
            &edges,
            &counts,
            egui::Color32::from_rgb(90, 220, 130),
            "histogram",
        )
        .expect("histogram edges are bins + 1");

        let shape = plot.add_shape(ShapeSpec {
            x: &[2.4, 4.2],
            y: &[2.8, 4.1],
            kind: rsplot::ShapeKind::Rectangle,
            color: egui::Color32::from_rgb(255, 140, 70),
            fill: false,
            overlay: false,
            line_style: LineStyle::Dashed,
            line_width: 2.0,
            gap_color: Some(egui::Color32::BLACK),
        });
        plot.set_item_legend(shape, "ROI rectangle");

        let marker = plot.add_point_marker(
            7.3,
            2.5,
            egui::Color32::from_rgb(230, 120, 255),
            Symbol::Diamond,
        );
        plot.set_item_legend(marker, "peak marker");

        plot.set_active_item(Some(image_handle));
        plot.drain_events();
        Self {
            plot,
            events: Vec::new(),
        }
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

impl eframe::App for LegendDemoApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::right("legend_demo_panel")
            .resizable(true)
            .default_size(230.0)
            .show_inside(ui, |ui| {
                ui.heading("Legends");
                let response = self.plot.show_legend(ui);
                if let Some(handle) = response.selected {
                    self.events.push(format!("selected #{handle}"));
                }

                ui.separator();
                ui.heading("Active item");
                if let Some(handle) = self.plot.active_item() {
                    let legend = self.plot.item_legend(handle).unwrap_or("unnamed");
                    let kind = self
                        .plot
                        .item_kind(handle)
                        .map(PlotItemKind::as_str)
                        .unwrap_or("item");
                    ui.label(format!("{legend} ({kind})"));
                } else {
                    ui.label("none");
                }

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
            self.plot.show_toolbar(ui);
            self.plot.show(ui);
        });
    }
}

fn x_values() -> Vec<f64> {
    (0..160).map(|i| i as f64 / 159.0 * 11.5).collect()
}

fn build_image() -> Vec<f32> {
    let mut data = vec![0.0; (WIDTH * HEIGHT) as usize];
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let x = -3.0 + 6.0 * col as f32 / (WIDTH - 1) as f32;
            let y = -2.5 + 5.0 * row as f32 / (HEIGHT - 1) as f32;
            let ring = ((x * x + y * y).sqrt() * 4.0).sin();
            let spot = (-((x - 1.0).powi(2) + (y + 0.8).powi(2)) / 0.45).exp();
            data[(row * WIDTH + col) as usize] = 0.45 * ring + spot;
        }
    }
    data
}

fn build_mask(image: &[f32]) -> Vec<bool> {
    image.iter().map(|value| *value > 0.65).collect()
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
        PlotEvent::RoiAdded { index } => format!("roi added #{index}"),
        PlotEvent::RoiChanged { index } => format!("roi changed #{index}"),
        PlotEvent::RoiCreated { index } => format!("roi created #{index}"),
        PlotEvent::DrawingProgress { mode, points } => {
            format!("drawing {mode:?} ({} pts)", points.len())
        }
        PlotEvent::DrawingFinished { mode, .. } => format!("drawing finished {mode:?}"),
        PlotEvent::RoiAboutToBeRemoved { index } => format!("roi #{index} about to be removed"),
        PlotEvent::RoisCleared => "rois cleared".to_owned(),
        PlotEvent::CurrentRoiChanged { previous, current } => {
            format!("current roi {previous:?} -> {current:?}")
        }
        PlotEvent::RoiInteractionModeChanged { index, mode } => {
            format!("roi #{index} mode {}", mode.label())
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
        "rsplot - high-level legend",
        options,
        Box::new(|cc| Ok(Box::new(LegendDemoApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
