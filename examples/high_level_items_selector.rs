//! High-level item-selector example.
//!
//! Mirrors silx `plotItemsSelector.py`: a plot contains multiple item kinds,
//! and a side panel selects retained items by legend.
//!
//! Run with: `cargo run --example high_level_items_selector`

use eframe::egui;
use egui_silx::{Colormap, GraphGrid, ItemHandle, PlotItemKind, PlotWidget};

struct ItemRow {
    handle: ItemHandle,
    legend: &'static str,
    kind: PlotItemKind,
}

struct ItemsSelectorApp {
    plot: PlotWidget,
    rows: Vec<ItemRow>,
    selected: Vec<ItemHandle>,
}

impl ItemsSelectorApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        let mut plot = PlotWidget::new(render_state, 0);
        plot.set_graph_title("Item selector");
        plot.set_graph_grid_mode(GraphGrid::Major);
        plot.set_default_colormap(Colormap::viridis(0.0, 8.0));

        let mut rows = Vec::new();

        let x = [0.0, 1.0, 2.0];
        let y = [3.0, 2.0, 1.0];
        let handle = plot.add_curve_with_legend(&x, &y, egui::Color32::YELLOW, "A curve");
        rows.push(ItemRow {
            handle,
            legend: "A curve",
            kind: PlotItemKind::Curve,
        });

        let sx = [0.0, 1.0, 2.5];
        let sy = [3.0, 2.5, 0.9];
        let handle = plot.add_scatter_with_legend(&sx, &sy, egui::Color32::LIGHT_BLUE, "A scatter");
        rows.push(ItemRow {
            handle,
            legend: "A scatter",
            kind: PlotItemKind::Scatter,
        });

        let edges = [0.0, 1.0, 2.5, 3.0];
        let counts = [0.0, 1.0, 2.0];
        let handle = plot
            .add_histogram_with_legend(&edges, &counts, egui::Color32::LIGHT_GREEN, "A histogram")
            .expect("histogram edges are bins + 1");
        rows.push(ItemRow {
            handle,
            legend: "A histogram",
            kind: PlotItemKind::Histogram,
        });

        let image = [0.0, 1.0, 2.0, 3.0, 2.0, 1.0];
        let handle = plot.add_image_with_legend(3, 2, &image, "An image");
        rows.push(ItemRow {
            handle,
            legend: "An image",
            kind: PlotItemKind::Image,
        });

        plot.drain_events();
        Self {
            plot,
            selected: vec![rows[0].handle],
            rows,
        }
    }

    fn set_selected(&mut self, handle: ItemHandle, selected: bool) {
        if selected {
            if !self.selected.contains(&handle) {
                self.selected.push(handle);
            }
            self.plot.set_active_item(Some(handle));
        } else {
            self.selected.retain(|item| *item != handle);
            if self.plot.active_item() == Some(handle) {
                self.plot.set_active_item(self.selected.last().copied());
            }
        }
    }
}

impl eframe::App for ItemsSelectorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::right("items_selector")
            .resizable(true)
            .default_size(260.0)
            .show_inside(ui, |ui| {
                ui.heading("Items");
                for index in 0..self.rows.len() {
                    let handle = self.rows[index].handle;
                    let legend = self.rows[index].legend;
                    let kind = self.rows[index].kind;
                    let mut selected = self.selected.contains(&handle);
                    if ui
                        .checkbox(&mut selected, format!("{legend} ({})", kind.as_str()))
                        .changed()
                    {
                        self.set_selected(handle, selected);
                    }
                }

                ui.separator();
                ui.label(format!("selected: {}", self.selected.len()));
                for handle in &self.selected {
                    if let Some(legend) = self.plot.item_legend(*handle) {
                        ui.label(legend);
                    }
                }
                ui.separator();
                self.plot.show_active_stats(ui);
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            let selected_count = self.selected.len();
            self.plot.show_toolbar_with(ui, |ui, _plot| {
                ui.label(format!("selected: {selected_count}"));
            });
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
        "egui-silx - high-level item selector",
        options,
        Box::new(|cc| Ok(Box::new(ItemsSelectorApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
