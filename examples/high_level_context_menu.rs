//! High-level context-menu example.
//!
//! Mirrors silx `plotContextMenu.py`: right-click the plot area for custom
//! plot actions wired to a retained high-level widget. Custom entries are
//! appended to the plot's built-in menu (Zoom Back / Reset Zoom) through
//! `PlotWidget::show_with_context_menu` — the plot owns the single context
//! menu on its response, exactly like silx adds actions to the plot's default
//! menu instead of installing a second one.
//!
//! Run with: `cargo run --example high_level_context_menu`

use std::path::PathBuf;

use eframe::egui;
use rsplot::{GraphGrid, PlotWidget};

struct ContextMenuApp {
    plot: PlotWidget,
    save_path: PathBuf,
    status: String,
}

impl ContextMenuApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        let mut plot = PlotWidget::new(render_state, 0);
        plot.set_graph_title("Right-click the plot area");
        plot.set_graph_grid_mode(GraphGrid::MajorAndMinor);
        plot.set_graph_cursor(true);

        let x: Vec<f64> = (0..1000)
            .map(|i| i as f64 / 999.0 * std::f64::consts::TAU)
            .collect();
        let y: Vec<f64> = x.iter().map(|x| x.sin()).collect();
        plot.add_curve_with_legend(&x, &y, egui::Color32::LIGHT_BLUE, "sin");
        plot.drain_events();

        Self {
            plot,
            save_path: PathBuf::from("rsplot-context-menu.png"),
            status: "right-click the plot".to_owned(),
        }
    }

    fn save_graph(&mut self) {
        match self.plot.save_graph(&self.save_path, (900, 600)) {
            Ok(()) => {
                self.status = format!("saved {}", self.save_path.display());
            }
            Err(error) => {
                self.status = format!("save failed: {error}");
            }
        }
    }
}

impl eframe::App for ContextMenuApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            let status = self.status.clone();
            self.plot.show_toolbar_with(ui, |ui, _plot| {
                ui.label(status);
            });
            // The closure only renders entries and signals the choices; the
            // owner applies them after `show` returns (it cannot borrow
            // `self.plot` while the plot is shown).
            let mut cursor = self.plot.graph_cursor();
            let mut grid = self.plot.graph_grid();
            let (mut cursor_changed, mut grid_changed, mut save_clicked) = (false, false, false);
            self.plot.show_with_context_menu(ui, |ui| {
                cursor_changed |= ui.checkbox(&mut cursor, "Cursor").changed();
                grid_changed |= ui.checkbox(&mut grid, "Grid").changed();
                if ui.button("Save PNG").clicked() {
                    save_clicked = true;
                    ui.close();
                }
            });
            if cursor_changed {
                self.plot.set_graph_cursor(cursor);
            }
            if grid_changed {
                self.plot.set_graph_grid(grid);
            }
            if save_clicked {
                self.save_graph();
            }
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "rsplot - high-level context menu",
        options,
        Box::new(|cc| Ok(Box::new(ContextMenuApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
