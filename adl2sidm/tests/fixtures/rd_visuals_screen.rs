// AUTO-GENERATED from rd_visuals.adl by adl2sidm -- do not edit by hand.

use sidm::Engine;
use siplot::egui::{self, Color32};

/// SiDM screen generated from `rd_visuals.adl`.
pub struct Screen {
    _engine: Engine,
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
        _render_state: Option<&siplot::egui_wgpu::RenderState>,
        _macros: Vec<(String, String)>,
    ) -> Self {
        let engine = Engine::new();
        engine.attach_repaint(ctx.clone());
        Self { _engine: engine }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        // Back-to-front: decoration (Background) -> monitor (Middle) -> control
        // (Foreground), so controls are never occluded or click-stolen.
        // Responsive layout: scale each MEDM rect by (sx, sy) to fill the
        // available area (adl2pydm grid_layout parity -- proportional reflow).
        let avail = ui.max_rect();
        let __origin = avail.min;
        let sx = avail.width() / 300.0;
        let sy = avail.height() / 160.0;
        place(ui, __origin, sx, sy, egui::Order::Background, egui::Id::new(18446744073709551615u64), 0.0, 0.0, 300.0, 160.0, |ui| {
            let __sbg = ui.max_rect();
            ui.painter().rect_filled(__sbg, egui::CornerRadius::ZERO, Color32::from_rgb(192, 192, 192));
        });
        place(ui, __origin, sx, sy, egui::Order::Foreground, egui::Id::new(0u64), 10.0, 10.0, 180.0, 24.0, |ui| {
            {
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(14.0 * sy));
                let __rect = ui.max_rect();
                let __sp = ui.spacing_mut();
                __sp.interact_size = egui::Vec2::ZERO;
                __sp.button_padding = egui::Vec2::ZERO;
                let __n = 2f32;
                {
                    let __i = 0;
                    let __cell = egui::Rect::from_min_size(__rect.min + egui::vec2(__i as f32 * __rect.width() / __n, 0.0), egui::vec2(__rect.width() / __n, __rect.height()));
                    if ui.put(__cell, egui::Button::new("A")).on_hover_text("related display: open rd_target_a.adl").clicked() {
                        eprintln!("related display: open rd_target_a.adl");
                    }
                }
                {
                    let __i = 1;
                    let __cell = egui::Rect::from_min_size(__rect.min + egui::vec2(__i as f32 * __rect.width() / __n, 0.0), egui::vec2(__rect.width() / __n, __rect.height()));
                    if ui.put(__cell, egui::Button::new("B")).on_hover_text("related display: open rd_target_b.adl").clicked() {
                        eprintln!("related display: open rd_target_b.adl");
                    }
                }
            }
        });
        place(ui, __origin, sx, sy, egui::Order::Foreground, egui::Id::new(1u64), 10.0, 44.0, 180.0, 48.0, |ui| {
            {
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(14.0 * sy));
                let __rect = ui.max_rect();
                let __sp = ui.spacing_mut();
                __sp.interact_size = egui::Vec2::ZERO;
                __sp.button_padding = egui::Vec2::ZERO;
                let __n = 2f32;
                {
                    let __i = 0;
                    let __cell = egui::Rect::from_min_size(__rect.min + egui::vec2(0.0, __i as f32 * __rect.height() / __n), egui::vec2(__rect.width(), __rect.height() / __n));
                    if ui.put(__cell, egui::Button::new("C")).on_hover_text("related display: open rd_target_c.adl").clicked() {
                        eprintln!("related display: open rd_target_c.adl");
                    }
                }
                {
                    let __i = 1;
                    let __cell = egui::Rect::from_min_size(__rect.min + egui::vec2(0.0, __i as f32 * __rect.height() / __n), egui::vec2(__rect.width(), __rect.height() / __n));
                    if ui.put(__cell, egui::Button::new("D")).on_hover_text("related display: open rd_target_d.adl").clicked() {
                        eprintln!("related display: open rd_target_d.adl");
                    }
                }
            }
        });
        place(ui, __origin, sx, sy, egui::Order::Foreground, egui::Id::new(2u64), 10.0, 100.0, 180.0, 24.0, |ui| {
            {
                let __rect = ui.max_rect();
                let __r = ui.allocate_rect(__rect, egui::Sense::click()).on_hover_text("related display: open rd_target_e.adl");
                if __r.clicked() {
                    eprintln!("related display: open rd_target_e.adl");
                }
            }
        });
    }
}

/// Place `add` at a MEDM position scaled by `(sx, sy)` -- the per-axis
/// `available / native` factors -- inside its own `egui::Area`, so the screen
/// reflows to fill the window. `origin` is the container's outer top-left (the
/// screen origin, or a frame's pre-inset origin), so a frame's `BORDER_INSET`
/// never shifts its children. The Area's `order` is the z-layer, so decoration
/// (`Background`) renders and takes input below controls (`Foreground`) regardless
/// of call order. The Area id is salted with the host `ui.id()` so two screen
/// instances sharing one viewport (related-display children on an embedded
/// fallback backend) keep distinct Area state.
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
