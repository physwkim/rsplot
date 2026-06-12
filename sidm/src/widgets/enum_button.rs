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
use crate::widgets::base::{BorderMode, ChannelBase, layout_justify};
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
        let mut draw = |ui: &mut egui::Ui, size: Option<egui::Vec2>| {
            for &idx in &order {
                let label = options[idx].as_str();
                let selected = Some(idx) == current;
                let clicked = match (widget_type, size) {
                    (EnumButtonType::Push, Some(size)) => ui
                        .add_sized(size, egui::Button::selectable(selected, label))
                        .clicked(),
                    (EnumButtonType::Push, None) => ui.selectable_label(selected, label).clicked(),
                    (EnumButtonType::Radio, Some(size)) => ui
                        .add_sized(size, egui::RadioButton::new(selected, label))
                        .clicked(),
                    (EnumButtonType::Radio, None) => ui.radio(selected, label).clicked(),
                };
                if clicked {
                    chosen = Some(idx);
                }
            }
        };

        self.base.framed(ui, &state, true, |ui| {
            // The `vertical`/`horizontal` stack replaces an inherited justified
            // layout, so the buttons would hug their captions inside the bclr
            // backing. MEDM divides the choice-button rect equally among the
            // buttons (medmChoiceButtons.c XmNfractionBase), so under a
            // justified layout size each button to its equal share; a
            // non-justified axis keeps egui's default interact size.
            let justify = layout_justify(ui);
            let size = (justify.0 || justify.1).then(|| {
                let n = order.len().max(1) as f32;
                let d = ui.spacing().interact_size;
                let gap = ui.spacing().item_spacing;
                let (w, h) = (
                    if justify.0 { ui.available_width() } else { d.x },
                    if justify.1 {
                        ui.available_height()
                    } else {
                        d.y
                    },
                );
                match orientation {
                    Orientation::Vertical => egui::vec2(w, (h - gap.y * (n - 1.0)) / n),
                    Orientation::Horizontal => egui::vec2((w - gap.x * (n - 1.0)) / n, h),
                }
            });
            match orientation {
                Orientation::Vertical => {
                    ui.vertical(|ui| draw(ui, size));
                }
                Orientation::Horizontal => {
                    ui.horizontal(|ui| draw(ui, size));
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
        let button = SidmEnumButton::new(&engine, "loc://enum_button_select").expect("connect");
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
}
