//! `SidmEnumButton` — pick an enum value from a row/column of buttons.
//!
//! Ports `pydm/widgets/enum_button.py` (`PyDMEnumButton`): one (exclusive)
//! button per enum string, the checked one being the current value
//! (`value_changed`), and clicking one writes its **index**
//! (`handle_button_clicked` → `send_value_signal.emit(button_id)`). Buttons are
//! laid out vertically (default) or horizontally (`orientation`), in natural
//! order, a `customOrder` of indices, or reversed (`invertOrder`).
//!
//! The choices / current index / written value are the shared
//! [`enum_choice`](crate::widgets::enum_choice) owner (also used by
//! [`SidmEnumComboBox`]); the display order is the pure [`order_indices`];
//! [`SidmEnumButton::show`] is a thin egui shell.
//!
//! [`SidmEnumComboBox`]: crate::widgets::SidmEnumComboBox

use siplot::egui;

use crate::channel::{Channel, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::{AlarmPalette, BorderMode, ChannelBase, layout_justify};
use crate::widgets::byte::Orientation;
use crate::widgets::enum_choice::{enum_current_index, enum_index_value, enum_options};

/// Which button widget to draw per choice (PyDM `WidgetType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EnumButtonType {
    /// A push button per choice, highlighted when selected (PyDM default).
    #[default]
    Push,
    /// A radio button per choice.
    Radio,
}

/// The order the choices are displayed in (PyDM `rebuild_layout`): a
/// `custom_order` of indices (out-of-range indices dropped) or the natural
/// `0..num_items`, optionally reversed by `invert`.
pub fn order_indices(num_items: usize, custom_order: Option<&[usize]>, invert: bool) -> Vec<usize> {
    let mut order: Vec<usize> = match custom_order {
        Some(custom) => custom.iter().copied().filter(|&i| i < num_items).collect(),
        None => (0..num_items).collect(),
    };
    if invert {
        order.reverse();
    }
    order
}

/// A group of exclusive buttons bound to a PV's enum strings (PyDM
/// `PyDMEnumButton`).
pub struct SidmEnumButton {
    base: ChannelBase,
    widget_type: EnumButtonType,
    orientation: Orientation,
    custom_order: Option<Vec<usize>>,
    invert_order: bool,
}

impl SidmEnumButton {
    /// Connect `address` and wrap it in an enum button group.
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            base: ChannelBase::new(engine.connect(address)?),
            widget_type: EnumButtonType::default(),
            orientation: Orientation::default(),
            custom_order: None,
            invert_order: false,
        })
    }

    /// Choose push buttons or radio buttons (builder style; PyDM `widgetType`).
    pub fn with_widget_type(mut self, widget_type: EnumButtonType) -> Self {
        self.widget_type = widget_type;
        self
    }

    /// Lay the buttons out vertically (default) or horizontally (builder style;
    /// PyDM `orientation`).
    pub fn with_orientation(mut self, orientation: Orientation) -> Self {
        self.orientation = orientation;
        self
    }

    /// Display the choices in this order of indices (builder style; PyDM
    /// `useCustomOrder` + `customOrder`). Out-of-range indices are dropped.
    pub fn with_custom_order(mut self, order: Vec<usize>) -> Self {
        self.custom_order = Some(order);
        self
    }

    /// Reverse the display order (builder style; PyDM `invertOrder`).
    pub fn with_invert_order(mut self, invert: bool) -> Self {
        self.invert_order = invert;
        self
    }

    /// Choose which severities draw a border (builder style;
    /// `DisconnectedOnly` for converted MEDM screens — MEDM draws no severity
    /// border, the dash is the SiDM disconnect marker).
    pub fn with_border_mode(mut self, mode: BorderMode) -> Self {
        self.base.border_mode = mode;
        self
    }

    /// Colour the choice captions by alarm severity when the channel is in alarm
    /// (MEDM `clrmod="alarm"` sets `XmNforeground = alarmColor`,
    /// medmChoiceButtons.c:375; PyDM `alarmSensitiveContent`). Off by default.
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

    /// Write `index` as the selected value (PyDM emits the integer index) and
    /// return the value written.
    pub fn select(&self, index: usize) -> PvValue {
        let value = enum_index_value(index);
        self.base.channel().put(value.clone());
        value
    }

    /// Render the button group this frame. Returns the value written if the user
    /// clicked a new choice.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<PvValue> {
        let state = self.base.channel().state();
        let options = enum_options(&state);
        let current = enum_current_index(&state);
        let order = order_indices(
            options.len(),
            self.custom_order.as_deref(),
            self.invert_order,
        );
        let widget_type = self.widget_type;
        let orientation = self.orientation;

        let mut chosen = None;
        self.base.framed_alarm_content(ui, &state, true, |ui| {
            let justify = layout_justify(ui);
            if justify.0 || justify.1 {
                // MEDM divides the choice-button rect EXACTLY among the
                // buttons, with zero spacing and zero margins
                // (medmChoiceButtons.c createToggleButtons: XmNspacing=0,
                // XmNmarginWidth=0, usedWidth = width/numButtons). Flow
                // layouts cannot honour a fixed rect: `ui.horizontal` floors
                // its row at `interact_size.y` and the justified parent
                // re-centres the overflow (both measured), which displaced
                // the captions out of narrow MEDM rects (the asynRecord
                // 55×18 Off/On toggles lost their glyph bottoms). Place each
                // button at its exact share of the content rect instead;
                // `put` centres the caption inside that share. Truncate
                // rather than letting a long caption outgrow its share
                // (Motif clips at the button bounds).
                let avail = ui.available_rect_before_wrap();
                let d = ui.spacing().interact_size;
                {
                    let spacing = ui.spacing_mut();
                    spacing.button_padding = egui::Vec2::ZERO;
                    // egui buttons floor their height at `interact_size.y`,
                    // which would inflate a share smaller than it right back
                    // past the rect — MEDM buttons have no minimum size.
                    spacing.interact_size = egui::Vec2::ZERO;
                }
                let n = order.len().max(1) as f32;
                let (w, h) = (
                    if justify.0 { avail.width() } else { d.x },
                    if justify.1 { avail.height() } else { d.y },
                );
                let size = match orientation {
                    Orientation::Vertical => egui::vec2(w, h / n),
                    Orientation::Horizontal => egui::vec2(w / n, h),
                };
                let step = match orientation {
                    Orientation::Vertical => egui::vec2(0.0, size.y),
                    Orientation::Horizontal => egui::vec2(size.x, 0.0),
                };
                for (k, &idx) in order.iter().enumerate() {
                    let rect = egui::Rect::from_min_size(avail.min + step * k as f32, size);
                    let label = options[idx].as_str();
                    let selected = Some(idx) == current;
                    let clicked = match widget_type {
                        EnumButtonType::Push => ui
                            .put(
                                rect,
                                egui::Button::selectable(selected, label)
                                    .wrap_mode(egui::TextWrapMode::Truncate),
                            )
                            .clicked(),
                        EnumButtonType::Radio => ui
                            .put(rect, egui::RadioButton::new(selected, label))
                            .clicked(),
                    };
                    if clicked {
                        chosen = Some(idx);
                    }
                }
            } else {
                // Plain layout: the PyDM shape — buttons hug their captions
                // in a flow stack.
                let mut draw = |ui: &mut egui::Ui| {
                    for &idx in &order {
                        let label = options[idx].as_str();
                        let selected = Some(idx) == current;
                        let clicked = match widget_type {
                            EnumButtonType::Push => ui.selectable_label(selected, label).clicked(),
                            EnumButtonType::Radio => ui.radio(selected, label).clicked(),
                        };
                        if clicked {
                            chosen = Some(idx);
                        }
                    }
                };
                match orientation {
                    Orientation::Vertical => {
                        ui.vertical(|ui| draw(ui));
                    }
                    Orientation::Horizontal => {
                        ui.horizontal(|ui| draw(ui));
                    }
                }
            }
        });

        chosen
            .filter(|&i| Some(i) != current)
            .map(|i| self.select(i))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::channel::PvValue;

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

    #[test]
    fn natural_order_is_zero_to_n() {
        assert_eq!(order_indices(3, None, false), vec![0, 1, 2]);
    }

    #[test]
    fn invert_reverses_the_order() {
        assert_eq!(order_indices(3, None, true), vec![2, 1, 0]);
    }

    #[test]
    fn custom_order_is_used_and_out_of_range_dropped() {
        assert_eq!(order_indices(3, Some(&[2, 0, 1]), false), vec![2, 0, 1]);
        // index 5 is past the end → dropped.
        assert_eq!(order_indices(3, Some(&[2, 5, 0]), false), vec![2, 0]);
        // custom order then inverted.
        assert_eq!(order_indices(3, Some(&[2, 0, 1]), true), vec![1, 0, 2]);
    }

    #[test]
    fn select_writes_the_index_to_the_channel() {
        let engine = Engine::new();
        // Config-bearing loc (type+init) so the connection connects — a bare
        // `loc://` stays disconnected until configured (R3-12).
        let button = SidmEnumButton::new(&engine, "loc://enum_button_select?type=int&init=0")
            .expect("connect");
        assert!(
            wait_for(|| button.channel().is_connected(), Duration::from_secs(2)),
            "button channel never connected"
        );
        let written = button.select(1);
        assert_eq!(written, PvValue::Int(1));
        assert!(
            wait_for(
                || button.channel().read(|s| s.value == Some(PvValue::Int(1))),
                Duration::from_secs(2)
            ),
            "channel did not receive the selected index (got {:?})",
            button.channel().read(|s| s.value.clone())
        );
    }

    #[test]
    fn alarm_sensitive_content_builder_flips_the_flag() {
        let engine = Engine::new();
        let button = SidmEnumButton::new(&engine, "loc://enum_button_alarm").expect("connect");
        assert!(
            !button.base.alarm_sensitive_content,
            "off by default (PyDM)"
        );
        let button = button.with_alarm_sensitive_content(true);
        assert!(button.base.alarm_sensitive_content);
    }
}
