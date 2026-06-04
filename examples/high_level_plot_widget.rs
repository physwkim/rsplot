//! High-level PlotWidget example.
//!
//! Mirrors the shape of silx examples `plotWidget.py` and
//! `plotLegendsWidget.py`: a plot with a toolbar, legend, active-item stats,
//! and buttons switching between image, scatter, and histogram data.
//!
//! Run with: `cargo run --example high_level_plot_widget`

use eframe::egui;
use egui_silx::{Colormap, GraphGrid, PlotEvent, PlotWidget};

const WIDTH: u32 = 180;
const HEIGHT: u32 = 140;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DemoData {
    Image,
    Scatter,
    Histogram,
}

struct HighLevelWidgetApp {
    plot: PlotWidget,
    demo: DemoData,
    events: Vec<String>,
}

impl HighLevelWidgetApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        let mut app = Self {
            plot: PlotWidget::new(render_state, 0),
            demo: DemoData::Image,
            events: Vec::new(),
        };
        app.plot.set_graph_cursor(true);
        app.show_image();
        app.events.clear();
        app.plot.drain_events();
        app
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

    fn show_image(&mut self) {
        self.demo = DemoData::Image;
        self.plot.clear();
        self.plot.set_graph_title("Image with curve overlay");
        self.plot.set_graph_x_label("Columns");
        self.plot.set_graph_y_label("Rows", egui_silx::YAxis::Left);
        self.plot.set_keep_data_aspect_ratio(true);
        self.plot.set_graph_grid_mode(GraphGrid::None);
        self.plot
            .set_default_colormap(Colormap::viridis(-0.25, 1.25));

        let image = build_image();
        let image_handle = self
            .plot
            .try_add_image_default(WIDTH, HEIGHT, &image)
            .expect("generated image length matches dimensions");
        self.plot.set_item_legend(image_handle, "sin(x*y) image");

        let x: Vec<f64> = (0..WIDTH).map(|col| col as f64).collect();
        let y: Vec<f64> = x
            .iter()
            .map(|x| HEIGHT as f64 * (0.5 + 0.35 * (x * 0.09).sin()))
            .collect();
        self.plot.add_curve_with_legend(
            &x,
            &y,
            egui::Color32::from_rgb(255, 96, 96),
            "sine overlay",
        );
        self.plot.set_active_item(Some(image_handle));
    }

    fn show_scatter(&mut self) {
        self.demo = DemoData::Scatter;
        self.plot.clear();
        self.plot.set_graph_title("Scatter");
        self.plot.set_graph_x_label("X");
        self.plot.set_graph_y_label("Y", egui_silx::YAxis::Left);
        self.plot.set_keep_data_aspect_ratio(false);
        self.plot.set_graph_grid_mode(GraphGrid::MajorAndMinor);

        let (x, y) = scatter_points();
        let handle =
            self.plot
                .add_scatter_with_legend(&x, &y, egui::Color32::LIGHT_BLUE, "sample points");
        self.plot.set_active_item(Some(handle));
    }

    fn show_histogram(&mut self) {
        self.demo = DemoData::Histogram;
        self.plot.clear();
        self.plot.set_graph_title("Histogram");
        self.plot.set_graph_x_label("Bin");
        self.plot.set_graph_y_label("Count", egui_silx::YAxis::Left);
        self.plot.set_keep_data_aspect_ratio(false);
        self.plot.set_graph_grid_mode(GraphGrid::MajorAndMinor);

        let edges: Vec<f64> = (0..=24).map(|i| i as f64 * 0.5).collect();
        let counts: Vec<f64> = edges
            .windows(2)
            .map(|edge| {
                let c = 0.5 * (edge[0] + edge[1]);
                40.0 * (-(c - 5.0).powi(2) / 5.0).exp() + 8.0 * (c * 1.7).sin().abs()
            })
            .collect();
        let handle = self
            .plot
            .add_histogram_with_legend(&edges, &counts, egui::Color32::LIGHT_GREEN, "counts")
            .expect("histogram edges are bins + 1");
        self.plot.set_active_item(Some(handle));
    }
}

impl eframe::App for HighLevelWidgetApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::right("high_level_plot_widget_inspector")
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
            let current_demo = self.demo;
            let (_, requested_demo) = self.plot.show_toolbar_with(ui, |ui, _plot| {
                let mut requested = None;
                if ui
                    .selectable_label(current_demo == DemoData::Image, "Image")
                    .clicked()
                {
                    requested = Some(DemoData::Image);
                }
                if ui
                    .selectable_label(current_demo == DemoData::Scatter, "Scatter")
                    .clicked()
                {
                    requested = Some(DemoData::Scatter);
                }
                if ui
                    .selectable_label(current_demo == DemoData::Histogram, "Histogram")
                    .clicked()
                {
                    requested = Some(DemoData::Histogram);
                }
                requested
            });
            match requested_demo {
                Some(DemoData::Image) => self.show_image(),
                Some(DemoData::Scatter) => self.show_scatter(),
                Some(DemoData::Histogram) => self.show_histogram(),
                None => {}
            }
            self.plot.show(ui);
        });
    }
}

fn build_image() -> Vec<f32> {
    let mut data = vec![0.0; (WIDTH * HEIGHT) as usize];
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let x = -6.0 + 12.0 * col as f32 / (WIDTH - 1) as f32;
            let y = -5.0 + 10.0 * row as f32 / (HEIGHT - 1) as f32;
            let r = (x * y).abs().max(0.05);
            data[(row * WIDTH + col) as usize] = (r.sin() / r) + 0.15 * (x * 0.7).cos();
        }
    }
    data
}

fn scatter_points() -> (Vec<f64>, Vec<f64>) {
    let mut x = Vec::with_capacity(80);
    let mut y = Vec::with_capacity(80);
    for i in 0..80 {
        let t = i as f64 / 79.0;
        x.push(10.0 * t);
        y.push((t * std::f64::consts::TAU * 3.0).sin() + 0.25 * (i % 7) as f64 - 0.75);
    }
    (x, y)
}

fn format_event(event: PlotEvent) -> String {
    match event {
        PlotEvent::ItemAdded { handle, kind } => format!("added {kind:?} #{handle}"),
        PlotEvent::ItemUpdated { handle, kind } => format!("updated {kind:?} #{handle}"),
        PlotEvent::ItemRemoved { handle, kind } => format!("removed {kind:?} #{handle}"),
        PlotEvent::ActiveItemChanged { previous, current } => {
            format!("active {previous:?} -> {current:?}")
        }
        PlotEvent::LimitsChanged => "limits changed".to_owned(),
        PlotEvent::RoiChanged { index } => format!("roi changed #{index}"),
        PlotEvent::RoiCreated { index } => format!("roi created #{index}"),
        PlotEvent::RoisCleared => "rois cleared".to_owned(),
        PlotEvent::CurrentRoiChanged { previous, current } => {
            format!("current roi {previous:?} -> {current:?}")
        }
        PlotEvent::MarkerMoved { handle } => format!("marker moved #{handle}"),
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
        "egui-silx - high-level PlotWidget",
        options,
        Box::new(|cc| Ok(Box::new(HighLevelWidgetApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
