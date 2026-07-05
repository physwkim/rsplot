// AUTO-GENERATED from rd_parent.adl by adl2rsdm -- do not edit by hand.

use rsdm::Engine;
use rsplot::egui::{self, Color32};

/// RsDM screen generated from `rd_parent.adl`.
pub struct Screen {
    _engine: Engine,
    /// Render state handed on to child screens opened from related displays.
    __rs: Option<rsplot::egui_wgpu::RenderState>,
    /// The related displays this screen has open (MEDM's display list).
    __open: Vec<OpenDisplay>,
}

impl Screen {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc.wgpu_render_state.as_ref().expect("adl2rsdm: a wgpu render state is required");
        rsplot::install(rs);
        Self::new_in(&cc.egui_ctx, Some(rs), Vec::new())
    }

    /// Build the screen on an existing egui context (the related-display child
    /// path). `macros` is this display instance's macro table (MEDM
    /// `performMacroSubstitutions`).
    pub fn new_in(
        ctx: &egui::Context,
        render_state: Option<&rsplot::egui_wgpu::RenderState>,
        _macros: Vec<(String, String)>,
    ) -> Self {
        let engine = Engine::new();
        engine.attach_repaint(ctx.clone());
        Self { _engine: engine, __rs: render_state.cloned(), __open: Vec::new() }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        // Back-to-front: decoration (Background) -> monitor (Middle) -> control
        // (Foreground), so controls are never occluded or click-stolen.
        let Self { _engine: _, __rs, __open } = self;
        // Responsive layout: scale each MEDM rect by (sx, sy) to fill the
        // available area (adl2pydm grid_layout parity -- proportional reflow).
        let avail = ui.max_rect();
        let __origin = avail.min;
        let sx = avail.width() / 300.0;
        let sy = avail.height() / 120.0;
        place(ui, __origin, sx, sy, egui::Order::Background, egui::Id::new(18446744073709551615u64), 0.0, 0.0, 300.0, 120.0, |ui| {
            let __sbg = ui.max_rect();
            ui.painter().rect_filled(__sbg, egui::CornerRadius::ZERO, Color32::from_rgb(192, 192, 192));
        });
        place(ui, __origin, sx, sy, egui::Order::Background, egui::Id::new(0u64), 10.0, 10.0, 200.0, 20.0, |ui| {
            {
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.0 * sy));
                let __font = ui.style().override_font_id.clone().unwrap_or_else(|| egui::TextStyle::Body.resolve(ui.style()));
                let __row = ui.fonts_mut(|f| f.row_height(&__font));
                ui.add_space(((ui.available_height() - __row) / 2.0).max(0.0));
                ui.label(egui::RichText::new("RD PARENT X:").color(Color32::from_rgb(0, 0, 0)));
            }
        });
        place(ui, __origin, sx, sy, egui::Order::Foreground, egui::Id::new(1u64), 10.0, 40.0, 120.0, 24.0, |ui| {
            {
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(14.0 * sy));
                ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                    let spacing = ui.spacing_mut();
                    spacing.interact_size = egui::Vec2::ZERO;
                    spacing.button_padding = egui::Vec2::ZERO;
                    if ui.button("Open Child").on_hover_text("open rd_child.adl (macros: P=X:)").clicked() {
                        let __rd_ctx = ui.ctx().clone();
                        let __rd_args = "P=X:".to_string();
                        OpenDisplay::open_or_focus(__open, &__rd_ctx, ("__rd_rd_child", __rd_args.clone()), "rd_child.adl", egui::vec2(220.0, 90.0), || {
                            Box::new(__rd_rd_child::Screen::new_in(&__rd_ctx, __rs.as_ref(), parse_macro_args(&__rd_args)))
                        });
                    }
                });
            }
        });
        place(ui, __origin, sx, sy, egui::Order::Foreground, egui::Id::new(2u64), 10.0, 80.0, 120.0, 24.0, |ui| {
            {
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(14.0 * sy));
                ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                    let spacing = ui.spacing_mut();
                    spacing.interact_size = egui::Vec2::ZERO;
                    spacing.button_padding = egui::Vec2::ZERO;
                    if ui.button("Missing").on_hover_text("related display: open rd_missing_fixture.adl").clicked() {
                        eprintln!("related display: open rd_missing_fixture.adl");
                    }
                });
            }
        });
        // Child displays opened from related-display buttons (each in its own
        // viewport; a backend without multi-viewport support falls back to
        // embedded windows).
        OpenDisplay::show_all(__open, ui);
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

/// What a related-display child screen exposes to be hosted in a viewport: its
/// per-frame draw. Implemented by every `Screen` in this generated file.
pub trait RsdmDisplay {
    fn ui(&mut self, ui: &mut egui::Ui);
}

/// One open related display: a child screen shown in its own immediate egui
/// viewport, keyed by (module, macro args) so a second click focuses the
/// existing window instead of duplicating it (MEDM `popupExistingDisplay`;
/// MEDM dedups across *all* displays, this list is per parent instance).
pub struct OpenDisplay {
    key: (&'static str, String),
    viewport: egui::ViewportId,
    title: String,
    size: egui::Vec2,
    screen: Box<dyn RsdmDisplay>,
}

impl OpenDisplay {
    /// Focus the already-open display for `key`, or build one with `make` and
    /// open it (MEDM `relatedDisplayCreateNewDisplay`).
    pub fn open_or_focus(
        open: &mut Vec<OpenDisplay>,
        ctx: &egui::Context,
        key: (&'static str, String),
        title: &str,
        size: egui::Vec2,
        make: impl FnOnce() -> Box<dyn RsdmDisplay>,
    ) {
        if let Some(d) = open.iter().find(|d| d.key == key) {
            if ctx.embed_viewports() {
                // Embedded fallback: there is no native window to focus --
                // the child renders as an `egui::Window` whose area id is its
                // viewport id (egui `Window::from_viewport`), so raise that
                // window instead (MEDM `popupExistingDisplay` raises too).
                ctx.move_to_top(egui::LayerId::new(
                    egui::Order::Middle,
                    egui::Id::new(d.viewport),
                ));
            } else {
                ctx.send_viewport_cmd_to(d.viewport, egui::ViewportCommand::Focus);
            }
            return;
        }
        // A process-wide monotonic id keeps every viewport distinct, even
        // across close-and-reopen and across parent instances.
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        open.push(OpenDisplay {
            key,
            viewport: egui::ViewportId::from_hash_of(("adl2rsdm related display", n)),
            title: title.to_owned(),
            size,
            screen: make(),
        });
    }

    /// Show every open display as an immediate viewport (a native OS window;
    /// egui falls back to an embedded `egui::Window` when the backend has no
    /// multi-viewport support), dropping each one whose window was closed.
    pub fn show_all(open: &mut Vec<OpenDisplay>, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        open.retain_mut(|d| {
            let mut keep = true;
            ctx.show_viewport_immediate(
                d.viewport,
                egui::ViewportBuilder::default()
                    .with_title(d.title.clone())
                    .with_inner_size(d.size),
                |ui, _class| {
                    d.screen.ui(ui);
                    if ui.ctx().input(|i| i.viewport().close_requested()) {
                        keep = false;
                    }
                },
            );
            keep
        });
    }
}

/// Parse MEDM's related-display `args` ("A=1,B=2") into a macro table: names
/// delimited by `=`, values by `,`, every whitespace character stripped from
/// both (medm/utils.c `generateNameValueTable`).
pub fn parse_macro_args(args: &str) -> Vec<(String, String)> {
    args.split(',')
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            let name: String = name.chars().filter(|c| !c.is_whitespace()).collect();
            if name.is_empty() {
                return None;
            }
            let value: String = value.chars().filter(|c| !c.is_whitespace()).collect();
            Some((name, value))
        })
        .collect()
}

impl RsdmDisplay for Screen {
    fn ui(&mut self, ui: &mut egui::Ui) {
        Screen::ui(self, ui)
    }
}

/// Related-display target `rd_child.adl`, converted alongside the root screen.
pub mod __rd_rd_child {
    // AUTO-GENERATED from rd_child.adl by adl2rsdm -- do not edit by hand.

    use rsdm::Engine;
    use rsplot::egui::{self, Color32};

    /// RsDM screen generated from `rd_child.adl`.
    pub struct Screen {
        _engine: Engine,
        __m: MacroTable,
        /// Render state handed on to child screens opened from related displays.
        __rs: Option<rsplot::egui_wgpu::RenderState>,
        /// The related displays this screen has open (MEDM's display list).
        __open: Vec<super::OpenDisplay>,
    }

    impl Screen {
        /// Build the screen on an existing egui context (the related-display child
        /// path). `macros` is this display instance's macro table (MEDM
        /// `performMacroSubstitutions`).
        pub fn new_in(
            ctx: &egui::Context,
            render_state: Option<&rsplot::egui_wgpu::RenderState>,
            macros: Vec<(String, String)>,
        ) -> Self {
            let __m = MacroTable(macros);
            let engine = Engine::new();
            engine.attach_repaint(ctx.clone());
            Self { _engine: engine, __m, __rs: render_state.cloned(), __open: Vec::new() }
        }

        pub fn ui(&mut self, ui: &mut egui::Ui) {
            // Back-to-front: decoration (Background) -> monitor (Middle) -> control
            // (Foreground), so controls are never occluded or click-stolen.
            let Self { _engine: _, __m, __rs, __open } = self;
            // Responsive layout: scale each MEDM rect by (sx, sy) to fill the
            // available area (adl2pydm grid_layout parity -- proportional reflow).
            let avail = ui.max_rect();
            let __origin = avail.min;
            let sx = avail.width() / 220.0;
            let sy = avail.height() / 90.0;
            place(ui, __origin, sx, sy, egui::Order::Background, egui::Id::new(18446744073709551615u64), 0.0, 0.0, 220.0, 90.0, |ui| {
                let __sbg = ui.max_rect();
                ui.painter().rect_filled(__sbg, egui::CornerRadius::ZERO, Color32::from_rgb(192, 192, 192));
            });
            place(ui, __origin, sx, sy, egui::Order::Background, egui::Id::new(0u64), 10.0, 10.0, 180.0, 20.0, |ui| {
                {
                    ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.0 * sy));
                    let __font = ui.style().override_font_id.clone().unwrap_or_else(|| egui::TextStyle::Body.resolve(ui.style()));
                    let __row = ui.fonts_mut(|f| f.row_height(&__font));
                    ui.add_space(((ui.available_height() - __row) / 2.0).max(0.0));
                    ui.label(egui::RichText::new(__m.expand("CHILD $(P)").as_str()).color(Color32::from_rgb(0, 0, 0)));
                }
            });
            place(ui, __origin, sx, sy, egui::Order::Foreground, egui::Id::new(1u64), 10.0, 50.0, 120.0, 24.0, |ui| {
                {
                    ui.style_mut().override_font_id = Some(egui::FontId::proportional(14.0 * sy));
                    ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                        let spacing = ui.spacing_mut();
                        spacing.interact_size = egui::Vec2::ZERO;
                        spacing.button_padding = egui::Vec2::ZERO;
                        if ui.button("Back").on_hover_text(__m.expand("open rd_parent.adl (macros: P=$(P))").as_str()).clicked() {
                            let __rd_ctx = ui.ctx().clone();
                            let __rd_args = __m.expand_args("P=$(P)");
                            super::OpenDisplay::open_or_focus(__open, &__rd_ctx, ("", __rd_args.clone()), "rd_parent.adl", egui::vec2(300.0, 120.0), || {
                                Box::new(super::Screen::new_in(&__rd_ctx, __rs.as_ref(), super::parse_macro_args(&__rd_args)))
                            });
                        }
                    });
                }
            });
            // Child displays opened from related-display buttons (each in its own
            // viewport; a backend without multi-viewport support falls back to
            // embedded windows).
            super::OpenDisplay::show_all(__open, ui);
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

    /// A display instance's macro table. `expand` follows MEDM's lexer `getToken`
    /// (child screens re-parsing their own file — an unknown `$(name)` stays
    /// literal); `expand_args` follows MEDM `performMacroSubstitutions` (the
    /// related-display `args` path — an unknown `$(name)` is dropped).
    pub struct MacroTable(pub Vec<(String, String)>);

    impl MacroTable {
        /// Substitute `$(name)`/`${name}` for a defined macro, leaving an unknown
        /// reference in place exactly as MEDM's lexer does (medm/medmCommon.c
        /// `getToken`).
        fn expand(&self, s: &str) -> String {
            let mut out = s.to_string();
            for (name, value) in &self.0 {
                out = out.replace(&format!("$({name})"), value);
                out = out.replace(&format!("${{{name}}}"), value);
            }
            out
        }

        /// Substitute `$(name)` for a defined macro and *drop* an undefined one
        /// (`$(X)` with X unbound becomes empty, not literal), MEDM
        /// `performMacroSubstitutions` (medm/utils.c:3444-3459). Only the `$(...)`
        /// form is a macro here — a `$` not opening `(` is copied verbatim, exactly as
        /// MEDM's byte scanner does.
        fn expand_args(&self, s: &str) -> String {
            let mut out = String::with_capacity(s.len());
            let mut rest = s;
            while let Some(d) = rest.find('$') {
                out.push_str(&rest[..d]);
                let after = &rest[d + 1..];
                if let Some(tail) = after.strip_prefix('(') {
                    // MEDM reads to the ')' or, if none, to end-of-string, then
                    // substitutes the defined value or drops the reference.
                    let (name, next) = match tail.find(')') {
                        Some(end) => (&tail[..end], &tail[end + 1..]),
                        None => (tail, ""),
                    };
                    if let Some((_, value)) = self.0.iter().find(|(n, _)| n == name) {
                        out.push_str(value);
                    }
                    rest = next;
                } else {
                    // `$` not opening `(` -> verbatim (includes `${...}`).
                    out.push('$');
                    rest = after;
                }
            }
            out.push_str(rest);
            out
        }
    }

    impl super::RsdmDisplay for Screen {
        fn ui(&mut self, ui: &mut egui::Ui) {
            Screen::ui(self, ui)
        }
    }
}
