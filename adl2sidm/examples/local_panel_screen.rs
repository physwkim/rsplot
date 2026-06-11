// AUTO-GENERATED from local_panel.adl by adl2sidm -- do not edit by hand.

use sidm::Engine;
use sidm::widgets::*;
use siplot::egui::{self, Color32};

/// SiDM screen generated from `local_panel.adl`.
pub struct Screen {
    _engine: Engine,
    w1: SidmLabel,
    w2: SidmTimePlot,
    w3: SidmDrawing,
    w4: SidmLineEdit,
    w5: SidmLabel,
    w6: SidmSlider,
    w7: SidmByteIndicator,
    w8: SidmLineEdit,
}

impl Screen {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc.wgpu_render_state.as_ref().expect("adl2sidm: a wgpu render state is required");
        siplot::install(rs);
        let engine = Engine::new();
        engine.attach_repaint(cc.egui_ctx.clone());
        let w1 = SidmLabel::new(&engine, "fake://temperature?wave=sine&period=8&rate=20&min=20&max=80")
            .expect("adl2sidm: connect fake://temperature?wave=sine&period=8&rate=20&min=20&max=80 (text update)")
            .with_precision(1);
        let mut w2 = SidmTimePlot::new(rs, 0).with_time_span(20.0);
        w2.add_channel(&engine, "fake://temperature?wave=sine&period=8&rate=20&min=20&max=80", Color32::from_rgb(0, 0, 255), "fake://temperature?wave=sine&period=8&rate=20&min=20&max=80").expect("adl2sidm: add strip-chart curve fake://temperature?wave=sine&period=8&rate=20&min=20&max=80");
        let w3 = SidmDrawing::new(&engine, "loc://adl2sidm_shape_66", DrawingShape::Rectangle)
            .expect("adl2sidm: connect loc://adl2sidm_shape_66 (drawing)")
            .with_fill(Color32::TRANSPARENT)
            .with_border(Color32::from_rgb(192, 192, 192), 2.0);
        let w4 = SidmLineEdit::new(&engine, "loc://setpoint?type=float&init=5&precision=2")
            .expect("adl2sidm: connect loc://setpoint?type=float&init=5&precision=2");
        let w5 = SidmLabel::new(&engine, "loc://setpoint?type=float&init=5&precision=2")
            .expect("adl2sidm: connect loc://setpoint?type=float&init=5&precision=2 (text update)")
            .with_precision(2);
        let w6 = SidmSlider::new(&engine, "loc://setpoint?type=float&init=5&precision=2")
            .expect("adl2sidm: connect loc://setpoint?type=float&init=5&precision=2 (valuator)")
            .with_limits(0.0, 10.0)
            .with_precision(2);
        let w7 = SidmByteIndicator::new(&engine, "loc://flags?type=int&init=170")
            .expect("adl2sidm: connect loc://flags?type=int&init=170 (byte)")
            .with_num_bits(8)
            .with_orientation(Orientation::Horizontal)
            .with_big_endian(true);
        let w8 = SidmLineEdit::new(&engine, "loc://flags?type=int&init=170")
            .expect("adl2sidm: connect loc://flags?type=int&init=170");
        Self { _engine: engine, w1, w2, w3, w4, w5, w6, w7, w8 }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        // Back-to-front: decoration (Background) -> monitor (Middle) -> control
        // (Foreground), so controls are never occluded or click-stolen.
        let Self { _engine: _, w1, w2, w3, w4, w5, w6, w7, w8 } = self;
        place(ui, egui::Order::Background, egui::Id::new(0u64), 10.0, 10.0, 320.0, 22.0, |ui| {
            ui.label(egui::RichText::new("SiDM panel from .adl (no IOC)").color(Color32::from_rgb(0, 0, 0)));
        });
        place(ui, egui::Order::Background, egui::Id::new(3u64), 6.0, 200.0, 348.0, 160.0, |ui| {
            let _ = w3.show(ui);
        });
        place(ui, egui::Order::Middle, egui::Id::new(1u64), 10.0, 42.0, 150.0, 20.0, |ui| {
            let _ = w1.show(ui);
        });
        place(ui, egui::Order::Middle, egui::Id::new(2u64), 10.0, 70.0, 340.0, 120.0, |ui| {
            let _ = w2.show(ui);
        });
        place(ui, egui::Order::Middle, egui::Id::new(5u64), 170.0, 214.0, 120.0, 20.0, |ui| {
            let _ = w5.show(ui);
        });
        place(ui, egui::Order::Middle, egui::Id::new(7u64), 20.0, 300.0, 140.0, 20.0, |ui| {
            let _ = w7.show(ui);
        });
        place(ui, egui::Order::Foreground, egui::Id::new(4u64), 20.0, 214.0, 140.0, 22.0, |ui| {
            let _ = w4.show(ui);
        });
        place(ui, egui::Order::Foreground, egui::Id::new(6u64), 20.0, 246.0, 320.0, 24.0, |ui| {
            let _ = w6.show(ui);
        });
        place(ui, egui::Order::Foreground, egui::Id::new(8u64), 170.0, 300.0, 120.0, 22.0, |ui| {
            let _ = w8.show(ui);
        });
        place(ui, egui::Order::Foreground, egui::Id::new(9u64), 20.0, 332.0, 130.0, 22.0, |ui| {
            if ui.button("Echo").clicked() {
                let _ = std::process::Command::new("sh").arg("-c").arg("echo hello from adl2sidm").spawn();
            }
        });
        place(ui, egui::Order::Foreground, egui::Id::new(10u64), 170.0, 332.0, 170.0, 22.0, |ui| {
            ui.menu_button("Shell Command", |ui| {
                if ui.button("Date").clicked() {
                    let _ = std::process::Command::new("sh").arg("-c").arg("date").spawn();
                    ui.close();
                }
                if ui.button("Uptime").clicked() {
                    let _ = std::process::Command::new("sh").arg("-c").arg("uptime").spawn();
                    ui.close();
                }
            });
        });
    }
}

/// Place `add` at an absolute MEDM position inside its own `egui::Area`. The
/// Area's `order` is the z-layer, so decoration (`Background`) renders and takes
/// input below controls (`Foreground`) regardless of call order.
#[allow(clippy::too_many_arguments)]
fn place(
    ui: &mut egui::Ui,
    order: egui::Order,
    id: egui::Id,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    add: impl FnOnce(&mut egui::Ui),
) {
    let origin = ui.max_rect().min;
    let rect = egui::Rect::from_min_size(origin + egui::vec2(x, y), egui::vec2(w, h));
    egui::Area::new(id)
        .order(order)
        .fixed_pos(rect.min)
        .constrain(false)
        .show(ui.ctx(), |ui| {
            ui.set_clip_rect(rect);
            ui.set_max_size(rect.size());
            add(ui);
        });
}
