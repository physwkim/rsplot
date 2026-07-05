//! A panel of RsDM widgets driven entirely by `loc://` and `fake://`
//! channels — no IOC, no network.
//!
//! - a `fake://` sine drives a [`RsdmLabel`] readout and a scrolling
//!   [`RsdmTimePlot`] (one address ⇒ one pooled connection ⇒ one generator);
//! - a shared `loc://` float setpoint is edited from a [`RsdmLineEdit`] and a
//!   [`RsdmSlider`] and read back by a [`RsdmLabel`] — writing from either
//!   widget updates the others through the channel (single-owner value, no local
//!   echo);
//! - a `loc://` integer is entered as hex in a [`RsdmLineEdit`] and shown bit by
//!   bit in a [`RsdmByteIndicator`].
//!
//! Run with: `cargo run -p rsdm --example rsdm_local_panel`

use eframe::egui;
use rsdm::Engine;
use rsdm::widgets::{
    DisplayFormat, RsdmByteIndicator, RsdmLabel, RsdmLineEdit, RsdmSlider, RsdmTimePlot,
};

// One `fake://` sine for both the readout and the strip chart. The engine pools
// connections by full address, so the identical string yields one generator.
const TEMP: &str = "fake://temperature?wave=sine&period=8&rate=20&min=20&max=80";
// A writable float shared by the line edit, slider, and read-back label.
const SETPOINT: &str = "loc://setpoint?type=float&init=5&precision=2";
// A writable integer shown as hex and as a byte LED grid.
const FLAGS: &str = "loc://flags?type=int&init=170";

struct LocalPanel {
    // The engine owns the tokio runtime and the connections; it must outlive the
    // widgets that hold `Channel` handles.
    _engine: Engine,
    temp_label: RsdmLabel,
    temp_plot: RsdmTimePlot,
    setpoint_edit: RsdmLineEdit,
    setpoint_slider: RsdmSlider,
    setpoint_label: RsdmLabel,
    flags_edit: RsdmLineEdit,
    flags_byte: RsdmByteIndicator,
}

impl LocalPanel {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        // Required before any rsplot GPU widget (Plot1D/TimePlot/ImageView).
        rsplot::install(rs);

        let engine = Engine::new();
        // Repaint the window whenever a channel value changes.
        engine.attach_repaint(cc.egui_ctx.clone());

        let temp_label = RsdmLabel::new(&engine, TEMP)
            .expect("connect temperature label")
            .with_precision(1);
        let mut temp_plot = RsdmTimePlot::new(rs, 0).with_time_span(20.0);
        temp_plot
            .add_channel(
                &engine,
                TEMP,
                egui::Color32::from_rgb(0, 200, 255),
                "temperature",
            )
            .expect("connect temperature curve");

        let setpoint_edit = RsdmLineEdit::new(&engine, SETPOINT).expect("connect setpoint edit");
        let setpoint_slider = RsdmSlider::new(&engine, SETPOINT)
            .expect("connect setpoint slider")
            .with_limits(0.0, 10.0);
        let setpoint_label = RsdmLabel::new(&engine, SETPOINT)
            .expect("connect setpoint label")
            .with_precision(2);

        let flags_edit = RsdmLineEdit::new(&engine, FLAGS)
            .expect("connect flags edit")
            .with_format(DisplayFormat::Hex);
        let flags_byte = RsdmByteIndicator::new(&engine, FLAGS)
            .expect("connect flags byte")
            .with_num_bits(8);

        Self {
            _engine: engine,
            temp_label,
            temp_plot,
            setpoint_edit,
            setpoint_slider,
            setpoint_label,
            flags_edit,
            flags_byte,
        }
    }
}

impl eframe::App for LocalPanel {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.heading("RsDM local panel (no IOC)");
            ui.label(
                "loc:// + fake:// channels. Edit the setpoint and flags; \
                 the temperature is a fake sine.",
            );
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Temperature:");
                self.temp_label.show(ui);
            });
            ui.allocate_ui(egui::vec2(ui.available_width(), 220.0), |ui| {
                self.temp_plot.show(ui);
            });

            ui.separator();
            ui.label("Setpoint (shared loc:// float):");
            ui.horizontal(|ui| {
                self.setpoint_edit.show(ui);
                self.setpoint_label.show(ui);
            });
            self.setpoint_slider.show(ui);

            ui.separator();
            ui.label("Flags (loc:// int, hex entry):");
            ui.horizontal(|ui| {
                self.flags_edit.show(ui);
                self.flags_byte.show(ui);
            });
        });
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "RsDM local panel",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(LocalPanel::new(cc)) as Box<dyn eframe::App>)),
    )
}
