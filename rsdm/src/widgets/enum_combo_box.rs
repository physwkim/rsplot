//! `RsdmEnumComboBox` — pick an enum value from a drop-down.
//!
//! Ports `pydm/widgets/enum_combo_box.py`: the items come from the channel's
//! enum strings (`enum_strings_changed`), the current selection is derived from
//! the value (`value_changed`: an int is the index directly, a string is matched
//! against the items like Qt `findText`, anything else is ignored), and choosing
//! an item writes its **index** (`internal_combo_box_activated_int` →
//! `send_value_signal.emit(index)`).
//!
//! The item list and current index are the pure [`RsdmEnumComboBox::options`] /
//! [`RsdmEnumComboBox::current_index`]; [`RsdmEnumComboBox::show`] is a thin egui
//! shell over [`RsdmEnumComboBox::select`].

use rsplot::egui;
use rsplot::egui::NumExt as _;

use crate::channel::{Channel, ChannelState, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::{AlarmPalette, BorderMode, ChannelBase, layout_justify};
use crate::widgets::enum_choice::{enum_current_index, enum_index_value, enum_options};
use crate::widgets::label::TextAlign;

/// A drop-down bound to a PV's enum strings (PyDM `PyDMEnumComboBox`).
pub struct RsdmEnumComboBox {
    base: ChannelBase,
    /// Horizontal alignment of the face caption. `Left` is the Qt
    /// `QComboBox` default PyDM inherits; converted MEDM screens use `Center`
    /// (a Motif option menu centres its caption — `XmLabel`'s default
    /// `XmNalignment`, which medmMenu.c never overrides).
    alignment: TextAlign,
}

impl RsdmEnumComboBox {
    /// Connect `address` and wrap it in an enum combo box.
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            base: ChannelBase::new(engine.connect(address)?),
            alignment: TextAlign::Left,
        })
    }

    /// Choose which severities draw a border (builder style;
    /// `DisconnectedOnly` for converted MEDM screens — MEDM draws no severity
    /// border, the dash is the RsDM disconnect marker).
    pub fn with_border_mode(mut self, mode: BorderMode) -> Self {
        self.base.border_mode = mode;
        self
    }

    /// Align the face caption (builder style). `Center` for converted MEDM
    /// screens (Motif option-menu captions are centred); the default `Left`
    /// matches PyDM's `QComboBox`.
    pub fn with_alignment(mut self, alignment: TextAlign) -> Self {
        self.alignment = alignment;
        self
    }

    /// Colour the face caption by alarm severity when the channel is in alarm
    /// (MEDM `clrmod="alarm"` sets `XmNforeground = alarmColor`, medmMenu.c:540;
    /// PyDM `alarmSensitiveContent`). Off by default. The face text is painted by
    /// hand below (stock geometry), so this reads [`content_color`] directly
    /// rather than going through [`framed_alarm_content`] like the other
    /// controllers.
    ///
    /// [`content_color`]: ChannelBase::content_color
    /// [`framed_alarm_content`]: ChannelBase::framed_alarm_content
    pub fn with_alarm_sensitive_content(mut self, on: bool) -> Self {
        self.base.alarm_sensitive_content = on;
        self
    }

    /// Which palette the alarm colouring draws from (builder style; default PyDM,
    /// `Medm` for converted screens so `NoAlarm` paints Green3 like MEDM).
    pub fn with_alarm_palette(mut self, palette: AlarmPalette) -> Self {
        self.base.alarm_palette = palette;
        self
    }

    /// The underlying channel.
    pub fn channel(&self) -> &Channel {
        self.base.channel()
    }

    /// The drop-down items: the channel's enum strings, or empty when none are
    /// known yet (PyDM `enum_strings_changed`). Delegates to the shared
    /// [`enum_options`].
    pub fn options(state: &ChannelState) -> Vec<String> {
        enum_options(state)
    }

    /// The index currently selected for `state` (PyDM `value_changed`).
    /// Delegates to the shared [`enum_current_index`].
    pub fn current_index(state: &ChannelState) -> Option<usize> {
        enum_current_index(state)
    }

    /// Write `index` as the selected value (PyDM emits the integer index) and
    /// return the value written.
    pub fn select(&self, index: usize) -> PvValue {
        let value = enum_index_value(index);
        self.base.channel().put(value.clone());
        value
    }

    /// Render the combo box this frame. Returns the value written if the user
    /// picked a new item.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<PvValue> {
        let state = self.base.channel().state();
        let options = Self::options(&state);
        let current = Self::current_index(&state);

        // MEDM clrmod="alarm" recolours the option-menu foreground by severity;
        // the face caption is painted by hand below, so fold the override into
        // that paint (the shared `framed_alarm_content` helper only reaches the
        // egui text widgets the other controllers use).
        let alarm_fg = self.base.content_color(&state);
        let mut chosen = None;
        self.base.framed(ui, &state, true, |ui| {
            let selected_text = current
                .and_then(|i| options.get(i))
                .map(String::as_str)
                .unwrap_or("");
            let id = egui::Id::new(("pydm_enum_combo", self.base.channel().address().raw()));
            // Stock `egui::ComboBox` pins the caption to the face's left edge
            // (`Align2::LEFT_CENTER`, with no alignment hook — a pre-aligned
            // galley would centre around that left anchor instead), so the
            // face is drawn here with the stock geometry — caption + icon
            // content, the `combo_width` floor, margins from `button_padding`
            // — and the caption aligned per `self.alignment`. The popup wiring
            // below also mirrors stock `combo_box_dyn`.
            let popup_id = id.with("popup");
            let is_popup_open = egui::Popup::is_id_open(ui.ctx(), popup_id);
            let margin = ui.spacing().button_padding;
            let icon_spacing = ui.spacing().icon_spacing;
            let icon_size = egui::Vec2::splat(ui.spacing().icon_width);
            let max_popup_height = ui.spacing().combo_height;

            let wrap_width = ui.available_width() - 2.0 * margin.x - icon_spacing - icon_size.x;
            let galley = egui::WidgetText::from(selected_text).into_galley(
                ui,
                None,
                wrap_width,
                egui::TextStyle::Button,
            );

            // egui's `ComboBox` face never widens under a justified layout —
            // its width is `content.at_least(width-or-combo_width)` regardless
            // — so a justified axis must be filled explicitly, like the
            // fixed-size painters do via `justified_size`. The height fills
            // too: a Motif option menu spans its full MEDM rect, and the
            // caption stays vertically centred in the face either way.
            let justify = layout_justify(ui);
            let content_width = if justify.0 {
                ui.available_width() - 2.0 * margin.x
            } else {
                (galley.size().x + icon_spacing + icon_size.x)
                    .at_least(ui.spacing().combo_width - 2.0 * margin.x)
            };
            let content_height = if justify.1 {
                ui.available_height() - 2.0 * margin.y
            } else {
                galley.size().y.max(icon_size.y)
            };
            let outer = egui::vec2(
                content_width + 2.0 * margin.x,
                (content_height + 2.0 * margin.y).at_least(ui.spacing().interact_size.y),
            );
            let (rect, response) = ui.allocate_exact_size(outer, egui::Sense::click());

            if ui.is_rect_visible(rect) {
                let visuals = if is_popup_open {
                    &ui.visuals().widgets.open
                } else {
                    ui.style().interact(&response)
                };
                ui.painter().rect(
                    rect.expand(visuals.expansion),
                    visuals.corner_radius,
                    visuals.weak_bg_fill,
                    visuals.bg_stroke,
                    egui::StrokeKind::Inside,
                );
                let content = rect.shrink2(margin);
                let icon_rect = egui::Align2::RIGHT_CENTER
                    .align_size_within_rect(icon_size, content)
                    .expand(visuals.expansion);
                // Stock `paint_default_icon`: a downward-pointing triangle.
                let tri = egui::Rect::from_center_size(
                    icon_rect.center(),
                    egui::vec2(icon_rect.width() * 0.7, icon_rect.height() * 0.45),
                );
                ui.painter().add(egui::Shape::convex_polygon(
                    vec![tri.left_top(), tri.right_top(), tri.center_bottom()],
                    visuals.fg_stroke.color,
                    egui::Stroke::NONE,
                ));
                let text_area = egui::Rect::from_min_max(
                    content.min,
                    egui::pos2(content.max.x - icon_size.x - icon_spacing, content.max.y),
                );
                let align2 = egui::Align2([self.alignment.to_align(), egui::Align::Center]);
                let text_rect = align2.align_size_within_rect(galley.size(), text_area);
                ui.painter().galley(
                    text_rect.min,
                    galley,
                    alarm_fg.unwrap_or(visuals.text_color()),
                );
            }

            egui::Popup::menu(&response)
                .id(popup_id)
                .width(response.rect.width())
                .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
                .show(|ui| {
                    ui.set_min_width(ui.available_width());
                    egui::ScrollArea::vertical()
                        .max_height(max_popup_height)
                        .show(ui, |ui| {
                            // Stock turns wrapping off so item labels expand
                            // the (often narrow) menu instead of wrapping.
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                            for (i, opt) in options.iter().enumerate() {
                                if ui.selectable_label(Some(i) == current, opt).clicked() {
                                    chosen = Some(i);
                                }
                            }
                        });
                });
        });

        chosen
            .filter(|&i| Some(i) != current)
            .map(|i| self.select(i))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;

    fn wait_for(mut cond: impl FnMut() -> bool, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        cond()
    }

    fn state_with(value: Option<PvValue>, enums: Option<&[&str]>) -> ChannelState {
        ChannelState {
            connected: true,
            value,
            enum_strings: enums.map(|e| e.iter().map(|s| s.to_string()).collect::<Arc<[String]>>()),
            ..ChannelState::default()
        }
    }

    #[test]
    fn options_are_the_enum_strings_or_empty() {
        let st = state_with(None, Some(&["Off", "On"]));
        assert_eq!(RsdmEnumComboBox::options(&st), vec!["Off", "On"]);
        let st = state_with(None, None);
        assert!(RsdmEnumComboBox::options(&st).is_empty());
    }

    #[test]
    fn current_index_from_int_enum_and_bool() {
        let enums = Some(["Off", "On", "Trip"].as_slice());
        assert_eq!(
            RsdmEnumComboBox::current_index(&state_with(Some(PvValue::Int(2)), enums)),
            Some(2)
        );
        assert_eq!(
            RsdmEnumComboBox::current_index(&state_with(
                Some(PvValue::Enum {
                    index: 1,
                    label: None
                }),
                enums
            )),
            Some(1)
        );
        assert_eq!(
            RsdmEnumComboBox::current_index(&state_with(Some(PvValue::Bool(true)), enums)),
            Some(1)
        );
    }

    #[test]
    fn current_index_from_string_matches_enum_text() {
        let enums = Some(["Off", "On", "Trip"].as_slice());
        assert_eq!(
            RsdmEnumComboBox::current_index(&state_with(Some(PvValue::Str("Trip".into())), enums)),
            Some(2)
        );
        // A string with no matching item selects nothing (PyDM findText == -1).
        assert_eq!(
            RsdmEnumComboBox::current_index(&state_with(Some(PvValue::Str("Nope".into())), enums)),
            None
        );
    }

    #[test]
    fn current_index_none_for_unsupported_or_missing() {
        let enums = Some(["Off", "On"].as_slice());
        assert_eq!(
            RsdmEnumComboBox::current_index(&state_with(Some(PvValue::Float(1.0)), enums)),
            None
        );
        assert_eq!(
            RsdmEnumComboBox::current_index(&state_with(None, enums)),
            None
        );
        // A negative int is not a valid index.
        assert_eq!(
            RsdmEnumComboBox::current_index(&state_with(Some(PvValue::Int(-1)), enums)),
            None
        );
    }

    #[test]
    fn select_writes_the_index_to_the_channel() {
        let engine = Engine::new();
        // Config-bearing loc (type+init) so the connection connects — a bare
        // `loc://` stays disconnected until configured (R3-12).
        let combo = RsdmEnumComboBox::new(&engine, "loc://enum_combo_select?type=int&init=0")
            .expect("connect");
        assert!(
            wait_for(|| combo.channel().is_connected(), Duration::from_secs(2)),
            "combo channel never connected"
        );
        let written = combo.select(2);
        assert_eq!(written, PvValue::Int(2));
        assert!(
            wait_for(
                || combo.channel().read(|s| s.value == Some(PvValue::Int(2))),
                Duration::from_secs(2)
            ),
            "channel did not receive the selected index (got {:?})",
            combo.channel().read(|s| s.value.clone())
        );
    }

    #[test]
    fn alarm_sensitive_content_builder_flips_the_flag() {
        let engine = Engine::new();
        let combo = RsdmEnumComboBox::new(&engine, "loc://enum_combo_alarm").expect("connect");
        assert!(!combo.base.alarm_sensitive_content, "off by default (PyDM)");
        let combo = combo.with_alarm_sensitive_content(true);
        assert!(combo.base.alarm_sensitive_content);
    }
}
