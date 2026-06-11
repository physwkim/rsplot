// AUTO-GENERATED from sample.adl by adl2sidm -- do not edit by hand.

use sidm::Engine;
use sidm::Channel;
use sidm::widgets::*;
use siplot::egui::{self, Color32};

/// SiDM screen generated from `sample.adl`.
pub struct Screen {
    _engine: Engine,
    w1: SidmLabel,
    w2: SidmLineEdit,
    w3: SidmPushButton,
    w4: SidmEnumComboBox,
    w5: SidmSlider,
    w6: SidmByteIndicator,
    w7: SidmScaleIndicator,
    w8: SidmDrawing,
    gate9: Channel,
    w10: SidmDrawing,
    w11: SidmDrawing,
    w12: SidmImage,
    w13: SidmTimePlot,
    w14: SidmWaveformPlot,
    w15: SidmFrame,
    w16: SidmLabel,
}

impl Screen {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc.wgpu_render_state.as_ref().expect("adl2sidm: a wgpu render state is required");
        siplot::install(rs);
        let engine = Engine::new();
        engine.attach_repaint(cc.egui_ctx.clone());
        let w1 = SidmLabel::new(&engine, "ca://DMM1:readback")
            .expect("adl2sidm: connect ca://DMM1:readback (text update)")
            .with_precision(3);
        let w2 = SidmLineEdit::new(&engine, "ca://DMM1:setpoint")
            .expect("adl2sidm: connect ca://DMM1:setpoint");
        let w3 = SidmPushButton::new(&engine, "ca://DMM1:go", "Start", "1")
            .expect("adl2sidm: connect ca://DMM1:go (message button)")
            .with_release_value("0");
        let w4 = SidmEnumComboBox::new(&engine, "ca://DMM1:mode")
            .expect("adl2sidm: connect ca://DMM1:mode (menu)");
        let w5 = SidmSlider::new(&engine, "ca://DMM1:level")
            .expect("adl2sidm: connect ca://DMM1:level (valuator)")
            .with_limits(0.0, 100.0)
            .with_precision(2);
        let w6 = SidmByteIndicator::new(&engine, "ca://DMM1:bits")
            .expect("adl2sidm: connect ca://DMM1:bits (byte)")
            .with_num_bits(8)
            .with_orientation(Orientation::Horizontal)
            .with_big_endian(true);
        let w7 = SidmScaleIndicator::new(&engine, "ca://DMM1:fill")
            .expect("adl2sidm: connect ca://DMM1:fill (scale indicator)")
            .with_bar_indicator(true);
        let w8 = SidmDrawing::new(&engine, "ca://DMM1:show_box", DrawingShape::Rectangle)
            .expect("adl2sidm: connect ca://DMM1:show_box (drawing)")
            .with_fill(Color32::TRANSPARENT)
            .with_border(Color32::from_rgb(192, 192, 192), 2.0);
        let gate9 = engine
            .connect("calc://adl2sidm_vis_129?expr=A!=0&A=ca://DMM1:show_box&update=A")
            .expect("adl2sidm: connect visibility gate calc://adl2sidm_vis_129?expr=A!=0&A=ca://DMM1:show_box&update=A");
        let w10 = SidmDrawing::new(&engine, "loc://adl2sidm_shape_146", DrawingShape::Ellipse)
            .expect("adl2sidm: connect loc://adl2sidm_shape_146 (drawing)")
            .with_fill(Color32::from_rgb(255, 0, 0));
        let w11 = SidmDrawing::new(&engine, "loc://adl2sidm_shape_157", DrawingShape::Arc { begin_deg: 0.0, span_deg: 360.0 })
            .expect("adl2sidm: connect loc://adl2sidm_shape_157 (arc)")
            .with_fill(Color32::from_rgb(0, 255, 0));
        let w12 = SidmImage::new("logo.gif")
            .with_size(egui::Vec2::new(80.0, 24.0));
        let mut w13 = SidmTimePlot::new(rs, 0).with_time_span(60.0);
        w13.add_channel(&engine, "ca://DMM1:readback", Color32::from_rgb(0, 0, 255), "$(P)readback").expect("adl2sidm: add strip-chart curve $(P)readback");
        let mut w14 = SidmWaveformPlot::new(rs, 1);
        w14.add_xy_channel(&engine, "ca://DMM1:ywave", Some("ca://DMM1:xwave"), Color32::from_rgb(255, 0, 0), "curve 1").expect("adl2sidm: add waveform curve 1");
        let w15 = SidmFrame::new(&engine, "loc://adl2sidm_frame_206")
            .expect("adl2sidm: connect loc://adl2sidm_frame_206 (composite)");
        let w16 = SidmLabel::new(&engine, "ca://DMM1:status")
            .expect("adl2sidm: connect ca://DMM1:status (text update)");
        Self { _engine: engine, w1, w2, w3, w4, w5, w6, w7, w8, gate9, w10, w11, w12, w13, w14, w15, w16 }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        // Back-to-front: decoration (Background) -> monitor (Middle) -> control
        // (Foreground), so controls are never occluded or click-stolen.
        let Self { _engine: _, w1, w2, w3, w4, w5, w6, w7, w8, gate9, w10, w11, w12, w13, w14, w15, w16 } = self;
        let __origin = ui.max_rect().min;
        place(ui, __origin, egui::Order::Background, egui::Id::new(0u64), 10.0, 10.0, 200.0, 20.0, |ui| {
            ui.label(egui::RichText::new("Sample Panel").color(Color32::from_rgb(0, 0, 0)));
        });
        if gate9.read(|s| s.value.as_ref().and_then(|v| v.as_f64())) != Some(0.0) {
            place(ui, __origin, egui::Order::Background, egui::Id::new(8u64), 210.0, 10.0, 180.0, 120.0, |ui| {
                let _ = w8.show(ui);
            });
        }
        place(ui, __origin, egui::Order::Background, egui::Id::new(10u64), 210.0, 140.0, 60.0, 60.0, |ui| {
            let _ = w10.show(ui);
        });
        place(ui, __origin, egui::Order::Background, egui::Id::new(11u64), 290.0, 140.0, 60.0, 60.0, |ui| {
            let _ = w11.show(ui);
        });
        place(ui, __origin, egui::Order::Background, egui::Id::new(12u64), 210.0, 210.0, 80.0, 24.0, |ui| {
            let _ = w12.show(ui);
        });
        place(ui, __origin, egui::Order::Middle, egui::Id::new(1u64), 10.0, 40.0, 120.0, 20.0, |ui| {
            let _ = w1.show(ui);
        });
        place(ui, __origin, egui::Order::Middle, egui::Id::new(6u64), 10.0, 170.0, 120.0, 20.0, |ui| {
            let _ = w6.show(ui);
        });
        place(ui, __origin, egui::Order::Middle, egui::Id::new(7u64), 10.0, 200.0, 180.0, 24.0, |ui| {
            let _ = w7.show(ui);
        });
        place(ui, __origin, egui::Order::Middle, egui::Id::new(13u64), 10.0, 240.0, 380.0, 110.0, |ui| {
            let _ = w13.show(ui);
        });
        place(ui, __origin, egui::Order::Middle, egui::Id::new(14u64), 10.0, 360.0, 380.0, 110.0, |ui| {
            let _ = w14.show(ui);
        });
        place(ui, __origin, egui::Order::Middle, egui::Id::new(15u64), 210.0, 210.0, 180.0, 24.0, |ui| {
            let __frame_origin_15 = ui.max_rect().min;
            let _ = w15.show(ui, |ui| {
                place(ui, __frame_origin_15, egui::Order::Middle, egui::Id::new(16u64), 0.0, 0.0, 180.0, 24.0, |ui| {
                    let _ = w16.show(ui);
                });
            });
        });
        place(ui, __origin, egui::Order::Foreground, egui::Id::new(2u64), 10.0, 70.0, 120.0, 20.0, |ui| {
            let _ = w2.show(ui);
        });
        place(ui, __origin, egui::Order::Foreground, egui::Id::new(3u64), 10.0, 100.0, 80.0, 24.0, |ui| {
            let _ = w3.show(ui);
        });
        place(ui, __origin, egui::Order::Foreground, egui::Id::new(4u64), 100.0, 100.0, 100.0, 24.0, |ui| {
            let _ = w4.show(ui);
        });
        place(ui, __origin, egui::Order::Foreground, egui::Id::new(5u64), 10.0, 140.0, 180.0, 24.0, |ui| {
            let _ = w5.show(ui);
        });
    }
}

/// Place `add` at an absolute MEDM position inside its own `egui::Area`.
/// `origin` is the container's outer top-left (the screen origin, or a frame's
/// pre-inset origin), so a frame's `BORDER_INSET` never shifts its children. The
/// Area's `order` is the z-layer, so decoration (`Background`) renders and takes
/// input below controls (`Foreground`) regardless of call order.
#[allow(clippy::too_many_arguments)]
fn place(
    ui: &mut egui::Ui,
    origin: egui::Pos2,
    order: egui::Order,
    id: egui::Id,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    add: impl FnOnce(&mut egui::Ui),
) {
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
