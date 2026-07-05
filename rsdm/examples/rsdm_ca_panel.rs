//! The local-panel widgets, but driven by live `ca://` PVs named on the command
//! line.
//!
//! Run with:
//! `cargo run -p rsdm --example rsdm_ca_panel -- <scalar_pv> [<flags_pv>]`
//!
//! - `<scalar_pv>` — a numeric PV, shown as a [`RsdmLabel`] readout, edited with
//!   a [`RsdmLineEdit`] and a [`RsdmSlider`], and trended on a [`RsdmTimePlot`];
//! - `<flags_pv>` (optional) — an integer PV shown bit by bit on a
//!   [`RsdmByteIndicator`].
//!
//! With no IOC reachable the channels stay disconnected and the widgets render
//! their disconnected state (the channel address in the label, a dashed border);
//! point `EPICS_CA_ADDR_LIST` at a host running the PVs to see live values.

use eframe::egui;
use rsdm::Engine;
use rsdm::widgets::{RsdmByteIndicator, RsdmLabel, RsdmLineEdit, RsdmSlider, RsdmTimePlot};

struct CaPanel {
    // The engine owns the tokio runtime and the CA connections; it must outlive
    // the widgets that hold `Channel` handles.
    _engine: Engine,
    scalar_label: RsdmLabel,
    scalar_edit: RsdmLineEdit,
    scalar_slider: RsdmSlider,
    scalar_plot: RsdmTimePlot,
    flags_byte: Option<RsdmByteIndicator>,
}

impl CaPanel {
    fn new(cc: &eframe::CreationContext<'_>, scalar: &str, flags: Option<&str>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        rsplot::install(rs);

        let engine = Engine::new();
        engine.attach_repaint(cc.egui_ctx.clone());

        let scalar_label = RsdmLabel::new(&engine, scalar).expect("connect scalar label");
        let scalar_edit = RsdmLineEdit::new(&engine, scalar).expect("connect scalar edit");
        let scalar_slider = RsdmSlider::new(&engine, scalar).expect("connect scalar slider");
        let mut scalar_plot = RsdmTimePlot::new(rs, 0).with_time_span(60.0);
        scalar_plot
            .add_channel(
                &engine,
                scalar,
                egui::Color32::from_rgb(0, 200, 255),
                scalar,
            )
            .expect("connect scalar curve");

        let flags_byte = flags.map(|addr| {
            RsdmByteIndicator::new(&engine, addr)
                .expect("connect flags byte")
                .with_num_bits(16)
        });

        Self {
            _engine: engine,
            scalar_label,
            scalar_edit,
            scalar_slider,
            scalar_plot,
            flags_byte,
        }
    }
}

impl eframe::App for CaPanel {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.heading("RsDM ca:// panel");
            ui.label("Live Channel Access PVs. Disconnected PVs show a dashed border.");
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Value:");
                self.scalar_label.show(ui);
            });
            ui.horizontal(|ui| {
                self.scalar_edit.show(ui);
            });
            self.scalar_slider.show(ui);

            ui.allocate_ui(egui::vec2(ui.available_width(), 240.0), |ui| {
                self.scalar_plot.show(ui);
            });

            if let Some(byte) = self.flags_byte.as_mut() {
                ui.separator();
                ui.label("Flags:");
                byte.show(ui);
            }
        });
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!(
            "usage: cargo run -p rsdm --example rsdm_ca_panel -- <scalar_pv> [<flags_pv>]\n\
             \n\
             \t<scalar_pv>  a numeric CA PV (label + line edit + slider + strip chart)\n\
             \t<flags_pv>   an integer CA PV (byte indicator); optional"
        );
        std::process::exit(2);
    }
    let scalar = format!("ca://{}", args[0]);
    let flags = args.get(1).map(|pv| format!("ca://{pv}"));

    eframe::run_native(
        "RsDM ca:// panel",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(move |cc| {
            Ok(Box::new(CaPanel::new(cc, &scalar, flags.as_deref())) as Box<dyn eframe::App>)
        }),
    )
}
