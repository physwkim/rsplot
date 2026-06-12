// AUTO-GENERATED from sample.adl by adl2sidm -- do not edit by hand.

use sidm::Engine;
use sidm::Channel;
use sidm::widgets::*;
use siplot::egui::{self, Color32};

/// SiDM screen generated from `sample.adl`.
pub struct Screen {
    _engine: Engine,
    alarm2: Channel,
    w3: SidmLabel,
    w4: SidmLineEdit,
    w5: SidmPushButton,
    w6: SidmEnumComboBox,
    w7: SidmSlider,
    w8: SidmByteIndicator,
    w9: SidmScaleIndicator,
    w10: SidmDrawing,
    gate11: Channel,
    w12: SidmDrawing,
    w13: SidmDrawing,
    w14: SidmImage,
    w15: SidmTimePlot,
    w16: SidmWaveformPlot,
    w17: SidmFrame,
    w18: SidmLabel,
}

impl Screen {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc.wgpu_render_state.as_ref().expect("adl2sidm: a wgpu render state is required");
        siplot::install(rs);
        Self::new_in(&cc.egui_ctx, Some(rs), Vec::new())
    }

    /// Build the screen on an existing egui context (the related-display child
    /// path). `macros` is this display instance's macro table (MEDM
    /// `performMacroSubstitutions`).
    pub fn new_in(
        ctx: &egui::Context,
        render_state: Option<&siplot::egui_wgpu::RenderState>,
        _macros: Vec<(String, String)>,
    ) -> Self {
        let rs = render_state.expect("adl2sidm: this screen needs a wgpu render state for its plots");
        // This instance's block of siplot PlotIds (unique per instance, so
        // related-display children never collide on GPU plot resources).
        let __plot_base = next_plot_ids(2);
        let engine = Engine::new();
        engine.attach_repaint(ctx.clone());
        let alarm2 = engine
            .connect("ca://DMM1:status")
            .expect("adl2sidm: connect alarm-colour source ca://DMM1:status");
        let w3 = SidmLabel::new(&engine, "ca://DMM1:readback")
            .expect("adl2sidm: connect ca://DMM1:readback (text update)")
            .with_precision(3);
        let w4 = SidmLineEdit::new(&engine, "ca://DMM1:setpoint")
            .expect("adl2sidm: connect ca://DMM1:setpoint");
        let w5 = SidmPushButton::new(&engine, "ca://DMM1:go", "Start", "1")
            .expect("adl2sidm: connect ca://DMM1:go (message button)")
            .with_release_value("0");
        let w6 = SidmEnumComboBox::new(&engine, "ca://DMM1:mode")
            .expect("adl2sidm: connect ca://DMM1:mode (menu)");
        let w7 = SidmSlider::new(&engine, "ca://DMM1:level")
            .expect("adl2sidm: connect ca://DMM1:level (valuator)")
            .with_limits(0.0, 100.0)
            .with_precision(2);
        let w8 = SidmByteIndicator::new(&engine, "ca://DMM1:bits")
            .expect("adl2sidm: connect ca://DMM1:bits (byte)")
            .with_num_bits(8)
            .with_orientation(Orientation::Horizontal)
            .with_big_endian(true);
        let w9 = SidmScaleIndicator::new(&engine, "ca://DMM1:fill")
            .expect("adl2sidm: connect ca://DMM1:fill (scale indicator)")
            .with_bar_indicator(true);
        let w10 = SidmDrawing::new(&engine, "ca://DMM1:show_box", DrawingShape::Rectangle)
            .expect("adl2sidm: connect ca://DMM1:show_box (drawing)")
            .with_fill(Color32::TRANSPARENT)
            .with_border(Color32::from_rgb(192, 192, 192), 2.0)
            .with_alarm_sensitive_border(true)
            .with_alarm_palette(AlarmPalette::Medm)
            .with_size(egui::Vec2::new(180.0, 120.0));
        let gate11 = engine
            .connect("calc://adl2sidm_vis_148?expr=A!=0&A=ca://DMM1:show_box&update=A")
            .expect("adl2sidm: connect visibility gate calc://adl2sidm_vis_148?expr=A!=0&A=ca://DMM1:show_box&update=A");
        let w12 = SidmDrawing::new(&engine, "loc://adl2sidm_shape_0", DrawingShape::Ellipse)
            .expect("adl2sidm: connect loc://adl2sidm_shape_0 (drawing)")
            .with_fill(Color32::from_rgb(255, 0, 0))
            .with_size(egui::Vec2::new(60.0, 60.0));
        let w13 = SidmDrawing::new(&engine, "loc://adl2sidm_shape_1", DrawingShape::Arc { begin_deg: 0.0, span_deg: 360.0 })
            .expect("adl2sidm: connect loc://adl2sidm_shape_1 (arc)")
            .with_fill(Color32::from_rgb(0, 255, 0))
            .with_size(egui::Vec2::new(60.0, 60.0));
        let w14 = SidmImage::new("logo.gif")
            .with_size(egui::Vec2::new(80.0, 24.0));
        let mut w15 = SidmTimePlot::new(rs, __plot_base).with_time_span(60.0);
        w15.add_channel(&engine, "ca://DMM1:readback", Color32::from_rgb(0, 0, 255), "DMM1:readback").expect("adl2sidm: add strip-chart curve DMM1:readback");
        let mut w16 = SidmWaveformPlot::new(rs, __plot_base + 1);
        w16.add_xy_channel(&engine, "ca://DMM1:ywave", Some("ca://DMM1:xwave"), Color32::from_rgb(255, 0, 0), "curve 1").expect("adl2sidm: add waveform curve 1");
        let w17 = SidmFrame::new(&engine, "loc://adl2sidm_frame_2")
            .expect("adl2sidm: connect loc://adl2sidm_frame_2 (composite)");
        let w18 = SidmLabel::new(&engine, "ca://DMM1:status")
            .expect("adl2sidm: connect ca://DMM1:status (text update)");
        Self { _engine: engine, alarm2, w3, w4, w5, w6, w7, w8, w9, w10, gate11, w12, w13, w14, w15, w16, w17, w18 }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        // Back-to-front: decoration (Background) -> monitor (Middle) -> control
        // (Foreground), so controls are never occluded or click-stolen.
        let Self { _engine: _, alarm2, w3, w4, w5, w6, w7, w8, w9, w10, gate11, w12, w13, w14, w15, w16, w17, w18 } = self;
        let __origin = ui.max_rect().min;
        place(ui, __origin, egui::Order::Background, egui::Id::new(18446744073709551615u64), 0.0, 0.0, 400.0, 500.0, |ui| {
            let __sbg = ui.max_rect();
            ui.painter().rect_filled(__sbg, egui::CornerRadius::ZERO, Color32::from_rgb(192, 192, 192));
        });
        place(ui, __origin, egui::Order::Background, egui::Id::new(0u64), 10.0, 10.0, 200.0, 20.0, |ui| {
            {
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.0));
                ui.label(egui::RichText::new("Sample Panel DMM1:").color(Color32::from_rgb(0, 0, 0)));
            }
        });
        place(ui, __origin, egui::Order::Background, egui::Id::new(1u64), 10.0, 475.0, 180.0, 18.0, |ui| {
            {
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(11.0));
                let __c = alarm2.read(|s| severity_color_medm(s.effective_severity()));
                ui.label(egui::RichText::new("STATUS").color(__c));
            }
        });
        if gate11.read(|s| s.value.as_ref().and_then(|v| v.as_f64())).is_some_and(|v| v != 0.0) {
            place(ui, __origin, egui::Order::Background, egui::Id::new(10u64), 210.0, 10.0, 180.0, 120.0, |ui| {
                ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                    let _ = w10.show(ui);
                });
            });
        }
        place(ui, __origin, egui::Order::Background, egui::Id::new(12u64), 210.0, 140.0, 60.0, 60.0, |ui| {
            ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                let _ = w12.show(ui);
            });
        });
        place(ui, __origin, egui::Order::Background, egui::Id::new(13u64), 290.0, 140.0, 60.0, 60.0, |ui| {
            ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                let _ = w13.show(ui);
            });
        });
        place(ui, __origin, egui::Order::Background, egui::Id::new(14u64), 210.0, 210.0, 80.0, 24.0, |ui| {
            ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                let _ = w14.show(ui);
            });
        });
        place(ui, __origin, egui::Order::Middle, egui::Id::new(3u64), 10.0, 40.0, 120.0, 20.0, |ui| {
            {
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.0));
                ui.style_mut().visuals.override_text_color = Some(Color32::from_rgb(255, 255, 255));
                ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                    let _ = w3.show(ui);
                });
            }
        });
        place(ui, __origin, egui::Order::Middle, egui::Id::new(8u64), 10.0, 170.0, 120.0, 20.0, |ui| {
            ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                let _ = w8.show(ui);
            });
        });
        place(ui, __origin, egui::Order::Middle, egui::Id::new(9u64), 10.0, 200.0, 180.0, 24.0, |ui| {
            ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                let _ = w9.show(ui);
            });
        });
        place(ui, __origin, egui::Order::Middle, egui::Id::new(15u64), 10.0, 240.0, 380.0, 110.0, |ui| {
            ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                let _ = w15.show(ui);
            });
        });
        place(ui, __origin, egui::Order::Middle, egui::Id::new(16u64), 10.0, 360.0, 380.0, 110.0, |ui| {
            ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                let _ = w16.show(ui);
            });
        });
        place(ui, __origin, egui::Order::Middle, egui::Id::new(17u64), 210.0, 210.0, 180.0, 24.0, |ui| {
            let __frame_origin_17 = ui.max_rect().min;
            let _ = w17.show(ui, |ui| {
                place(ui, __frame_origin_17, egui::Order::Middle, egui::Id::new(18u64), 0.0, 0.0, 180.0, 24.0, |ui| {
                    {
                        ui.style_mut().override_font_id = Some(egui::FontId::proportional(14.0));
                        ui.style_mut().visuals.override_text_color = Some(Color32::from_rgb(255, 255, 255));
                        ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                            let _ = w18.show(ui);
                        });
                    }
                });
            });
        });
        place(ui, __origin, egui::Order::Foreground, egui::Id::new(4u64), 10.0, 70.0, 120.0, 20.0, |ui| {
            {
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.0));
                ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                    let _ = w4.show(ui);
                });
            }
        });
        place(ui, __origin, egui::Order::Foreground, egui::Id::new(5u64), 10.0, 100.0, 80.0, 24.0, |ui| {
            {
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(14.0));
                let __bg = ui.max_rect();
                ui.painter().rect_filled(__bg, egui::CornerRadius::ZERO, Color32::from_rgb(192, 192, 192));
                let __v = &mut ui.style_mut().visuals;
                __v.widgets.inactive.weak_bg_fill = Color32::from_rgb(192, 192, 192);
                __v.widgets.hovered.weak_bg_fill = Color32::from_rgb(192, 192, 192);
                __v.widgets.active.weak_bg_fill = Color32::from_rgb(192, 192, 192);
                __v.widgets.open.weak_bg_fill = Color32::from_rgb(192, 192, 192);
                __v.text_edit_bg_color = Some(Color32::from_rgb(192, 192, 192));
                __v.override_text_color = Some(Color32::from_rgb(0, 0, 0));
                ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                    let _ = w5.show(ui);
                });
            }
        });
        place(ui, __origin, egui::Order::Foreground, egui::Id::new(6u64), 100.0, 100.0, 100.0, 24.0, |ui| {
            {
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(14.0));
                ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                    let _ = w6.show(ui);
                });
            }
        });
        place(ui, __origin, egui::Order::Foreground, egui::Id::new(7u64), 10.0, 140.0, 180.0, 24.0, |ui| {
            ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                let _ = w7.show(ui);
            });
        });
    }
}

/// Place `add` at an absolute MEDM position inside its own `egui::Area`.
/// `origin` is the container's outer top-left (the screen origin, or a frame's
/// pre-inset origin), so a frame's `BORDER_INSET` never shifts its children. The
/// Area's `order` is the z-layer, so decoration (`Background`) renders and takes
/// input below controls (`Foreground`) regardless of call order. The Area id is
/// salted with the host `ui.id()` so two screen instances sharing one viewport
/// (related-display children on an embedded fallback backend) keep distinct
/// Area state.
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
    egui::Area::new(ui.id().with(id))
        .order(order)
        .fixed_pos(rect.min)
        .constrain(false)
        .show(ui.ctx(), |ui| {
            ui.set_clip_rect(rect);
            ui.set_max_size(rect.size());
            add(ui);
        });
}

/// Allocate a contiguous block of `count` siplot `PlotId`s, unique across every
/// screen instance built from this generated file (related-display children
/// included) -- siplot keys per-plot GPU resources by `PlotId`, so two
/// instances must never share one. (Two *separately generated* files compiled
/// into one app each start at 0 and can still collide; convert such screens
/// together through one root instead.)
fn next_plot_ids(count: u64) -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    SEQ.fetch_add(count, std::sync::atomic::Ordering::Relaxed)
}
