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
    w13: SidmFrame,
    w15: SidmLabel,
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
        let w3 = SidmDrawing::new(&engine, "loc://adl2sidm_shape_0", DrawingShape::Rectangle)
            .expect("adl2sidm: connect loc://adl2sidm_shape_0 (drawing)")
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
        let w13 = SidmFrame::new(&engine, "loc://adl2sidm_embed_1")
            .expect("adl2sidm: connect loc://adl2sidm_embed_1 (embedded embed_child.adl)");
        let w15 = SidmLabel::new(&engine, "loc://embcount?type=int&init=7")
            .expect("adl2sidm: connect loc://embcount?type=int&init=7 (text update)")
            .with_precision(0);
        Self { _engine: engine, w1, w2, w3, w4, w5, w6, w7, w8, w13, w15 }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        // Back-to-front: decoration (Background) -> monitor (Middle) -> control
        // (Foreground), so controls are never occluded or click-stolen.
        let Self { _engine: _, w1, w2, w3, w4, w5, w6, w7, w8, w13, w15 } = self;
        // Responsive layout: scale each MEDM rect by (sx, sy) to fill the
        // available area (adl2pydm grid_layout parity -- proportional reflow).
        let avail = ui.max_rect();
        let __origin = avail.min;
        let sx = avail.width() / 360.0;
        let sy = avail.height() / 460.0;
        place(ui, __origin, sx, sy, egui::Order::Background, egui::Id::new(18446744073709551615u64), 0.0, 0.0, 360.0, 460.0, |ui| {
            let __sbg = ui.max_rect();
            ui.painter().rect_filled(__sbg, egui::CornerRadius::ZERO, Color32::from_rgb(255, 255, 255));
        });
        place(ui, __origin, sx, sy, egui::Order::Background, egui::Id::new(0u64), 10.0, 10.0, 320.0, 22.0, |ui| {
            ui.label(egui::RichText::new("SiDM panel from .adl (no IOC)").color(Color32::from_rgb(0, 0, 0)));
        });
        place(ui, __origin, sx, sy, egui::Order::Background, egui::Id::new(3u64), 6.0, 200.0, 348.0, 160.0, |ui| {
            let _ = w3.show(ui);
        });
        place(ui, __origin, sx, sy, egui::Order::Middle, egui::Id::new(1u64), 10.0, 42.0, 150.0, 20.0, |ui| {
            {
                ui.style_mut().visuals.override_text_color = Some(Color32::from_rgb(0, 0, 0));
                let _ = w1.show(ui);
            }
        });
        place(ui, __origin, sx, sy, egui::Order::Middle, egui::Id::new(2u64), 10.0, 70.0, 340.0, 120.0, |ui| {
            let _ = w2.show(ui);
        });
        place(ui, __origin, sx, sy, egui::Order::Middle, egui::Id::new(5u64), 170.0, 214.0, 120.0, 20.0, |ui| {
            {
                ui.style_mut().visuals.override_text_color = Some(Color32::from_rgb(0, 0, 0));
                let _ = w5.show(ui);
            }
        });
        place(ui, __origin, sx, sy, egui::Order::Middle, egui::Id::new(7u64), 20.0, 300.0, 140.0, 20.0, |ui| {
            let _ = w7.show(ui);
        });
        place(ui, __origin, sx, sy, egui::Order::Middle, egui::Id::new(13u64), 20.0, 400.0, 160.0, 40.0, |ui| {
            let __frame_origin_13 = ui.max_rect().min;
            let _ = w13.show(ui, |ui| {
                place(ui, __frame_origin_13, sx, sy, egui::Order::Background, egui::Id::new(14u64), 4.0, 2.0, 152.0, 14.0, |ui| {
                    ui.label(egui::RichText::new("embedded child").color(Color32::from_rgb(0, 0, 0)));
                });
                place(ui, __frame_origin_13, sx, sy, egui::Order::Middle, egui::Id::new(15u64), 4.0, 20.0, 152.0, 16.0, |ui| {
                    let _ = w15.show(ui);
                });
            });
        });
        place(ui, __origin, sx, sy, egui::Order::Foreground, egui::Id::new(4u64), 20.0, 214.0, 140.0, 22.0, |ui| {
            let _ = w4.show(ui);
        });
        place(ui, __origin, sx, sy, egui::Order::Foreground, egui::Id::new(6u64), 20.0, 246.0, 320.0, 24.0, |ui| {
            let _ = w6.show(ui);
        });
        place(ui, __origin, sx, sy, egui::Order::Foreground, egui::Id::new(8u64), 170.0, 300.0, 120.0, 22.0, |ui| {
            let _ = w8.show(ui);
        });
        place(ui, __origin, sx, sy, egui::Order::Foreground, egui::Id::new(9u64), 20.0, 332.0, 130.0, 22.0, |ui| {
            if ui.button("Echo").clicked() {
                let _ = std::process::Command::new("sh").arg("-c").arg("echo hello from adl2sidm").spawn();
            }
        });
        place(ui, __origin, sx, sy, egui::Order::Foreground, egui::Id::new(10u64), 170.0, 332.0, 170.0, 22.0, |ui| {
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
        place(ui, __origin, sx, sy, egui::Order::Foreground, egui::Id::new(11u64), 20.0, 366.0, 130.0, 22.0, |ui| {
            if ui.button("Detail").on_hover_text("related display: open detail.adl").clicked() {
                eprintln!("related display: open detail.adl");
            }
        });
        place(ui, __origin, sx, sy, egui::Order::Foreground, egui::Id::new(12u64), 170.0, 366.0, 170.0, 22.0, |ui| {
            ui.menu_button("Screens", |ui| {
                if ui.button("Overview").on_hover_text("related display: open overview.adl").clicked() {
                    eprintln!("related display: open overview.adl");
                    ui.close();
                }
                if ui.button("Tuning").on_hover_text("related display: open tuning.adl (macros: P=DMM1:)").clicked() {
                    eprintln!("related display: open tuning.adl (macros: P=DMM1:)");
                    ui.close();
                }
            });
        });
    }
}

/// Place `add` at a MEDM position scaled by `(sx, sy)` -- the per-axis
/// `available / native` factors -- inside its own `egui::Area`, so the screen
/// reflows to fill the window. `origin` is the container's outer top-left (the
/// screen origin, or a frame's pre-inset origin), so a frame's `BORDER_INSET`
/// never shifts its children. The Area's `order` is the z-layer, so decoration
/// (`Background`) renders and takes input below controls (`Foreground`) regardless
/// of call order.
#[allow(clippy::too_many_arguments)]
fn place(
    ui: &mut egui::Ui,
    origin: egui::Pos2,
    sx: f32,
    sy: f32,
    order: egui::Order,
    id: egui::Id,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    add: impl FnOnce(&mut egui::Ui),
) {
    let rect =
        egui::Rect::from_min_size(origin + egui::vec2(x * sx, y * sy), egui::vec2(w * sx, h * sy));
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
