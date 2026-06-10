//! `PydmEnumComboBox` — pick an enum value from a drop-down.
//!
//! Ports `pydm/widgets/enum_combo_box.py`: the items come from the channel's
//! enum strings (`enum_strings_changed`), the current selection is derived from
//! the value (`value_changed`: an int is the index directly, a string is matched
//! against the items like Qt `findText`, anything else is ignored), and choosing
//! an item writes its **index** (`internal_combo_box_activated_int` →
//! `send_value_signal.emit(index)`).
//!
//! The item list and current index are the pure [`PydmEnumComboBox::options`] /
//! [`PydmEnumComboBox::current_index`]; [`PydmEnumComboBox::show`] is a thin egui
//! shell over [`PydmEnumComboBox::select`].

use siplot::egui;

use crate::channel::{Channel, ChannelState, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::ChannelBase;

/// A drop-down bound to a PV's enum strings (PyDM `PyDMEnumComboBox`).
pub struct PydmEnumComboBox {
    base: ChannelBase,
}

impl PydmEnumComboBox {
    /// Connect `address` and wrap it in an enum combo box.
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            base: ChannelBase::new(engine.connect(address)?),
        })
    }

    /// The underlying channel.
    pub fn channel(&self) -> &Channel {
        self.base.channel()
    }

    /// The drop-down items: the channel's enum strings, or empty when none are
    /// known yet (PyDM `enum_strings_changed`).
    pub fn options(state: &ChannelState) -> Vec<String> {
        state
            .enum_strings
            .as_deref()
            .map(<[String]>::to_vec)
            .unwrap_or_default()
    }

    /// The index currently selected for `state` (PyDM `value_changed`): an
    /// integer/enum/bool value is the index directly; a string is matched against
    /// the enum strings (`findText`). Any other value (or no value) selects
    /// nothing.
    pub fn current_index(state: &ChannelState) -> Option<usize> {
        match &state.value {
            Some(PvValue::Int(n)) => usize::try_from(*n).ok(),
            Some(PvValue::Enum { index, .. }) => Some(*index as usize),
            Some(PvValue::Bool(b)) => Some(usize::from(*b)),
            Some(PvValue::Str(s)) => state
                .enum_strings
                .as_deref()
                .and_then(|items| items.iter().position(|item| item == s.as_ref())),
            _ => None,
        }
    }

    /// Write `index` as the selected value (PyDM emits the integer index) and
    /// return the value written.
    pub fn select(&self, index: usize) -> PvValue {
        let value = PvValue::Int(index as i64);
        self.base.channel().put(value.clone());
        value
    }

    /// Render the combo box this frame. Returns the value written if the user
    /// picked a new item.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<PvValue> {
        let state = self.base.channel().state();
        let options = Self::options(&state);
        let current = Self::current_index(&state);

        let mut chosen = None;
        self.base.framed(ui, &state, true, |ui| {
            let selected_text = current
                .and_then(|i| options.get(i))
                .map(String::as_str)
                .unwrap_or("");
            let id = egui::Id::new(("pydm_enum_combo", self.base.channel().address().raw()));
            egui::ComboBox::from_id_salt(id)
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    for (i, opt) in options.iter().enumerate() {
                        if ui.selectable_label(Some(i) == current, opt).clicked() {
                            chosen = Some(i);
                        }
                    }
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
        assert_eq!(PydmEnumComboBox::options(&st), vec!["Off", "On"]);
        let st = state_with(None, None);
        assert!(PydmEnumComboBox::options(&st).is_empty());
    }

    #[test]
    fn current_index_from_int_enum_and_bool() {
        let enums = Some(["Off", "On", "Trip"].as_slice());
        assert_eq!(
            PydmEnumComboBox::current_index(&state_with(Some(PvValue::Int(2)), enums)),
            Some(2)
        );
        assert_eq!(
            PydmEnumComboBox::current_index(&state_with(
                Some(PvValue::Enum {
                    index: 1,
                    label: None
                }),
                enums
            )),
            Some(1)
        );
        assert_eq!(
            PydmEnumComboBox::current_index(&state_with(Some(PvValue::Bool(true)), enums)),
            Some(1)
        );
    }

    #[test]
    fn current_index_from_string_matches_enum_text() {
        let enums = Some(["Off", "On", "Trip"].as_slice());
        assert_eq!(
            PydmEnumComboBox::current_index(&state_with(Some(PvValue::Str("Trip".into())), enums)),
            Some(2)
        );
        // A string with no matching item selects nothing (PyDM findText == -1).
        assert_eq!(
            PydmEnumComboBox::current_index(&state_with(Some(PvValue::Str("Nope".into())), enums)),
            None
        );
    }

    #[test]
    fn current_index_none_for_unsupported_or_missing() {
        let enums = Some(["Off", "On"].as_slice());
        assert_eq!(
            PydmEnumComboBox::current_index(&state_with(Some(PvValue::Float(1.0)), enums)),
            None
        );
        assert_eq!(
            PydmEnumComboBox::current_index(&state_with(None, enums)),
            None
        );
        // A negative int is not a valid index.
        assert_eq!(
            PydmEnumComboBox::current_index(&state_with(Some(PvValue::Int(-1)), enums)),
            None
        );
    }

    #[test]
    fn select_writes_the_index_to_the_channel() {
        let engine = Engine::new();
        let combo = PydmEnumComboBox::new(&engine, "loc://enum_combo_select").expect("connect");
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
}
