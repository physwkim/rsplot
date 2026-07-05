//! `RsdmCheckbox` — a writable boolean toggle.
//!
//! Ports `pydm/widgets/checkbox.py`: checked when the channel value is positive
//! (`new_val > 0`), and a toggle writes `1`/`0` (PyDM `send_value`). The check
//! state and the value written are pure (`is_checked`, `value_for`); the egui
//! toggle is a thin shell over [`RsdmCheckbox::set_checked`].

use rsplot::egui;

use crate::channel::{Channel, ChannelState, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::ChannelBase;

/// A writable boolean checkbox (PyDM `PyDMCheckbox`).
pub struct RsdmCheckbox {
    base: ChannelBase,
    /// The text shown beside the checkbox.
    pub label: String,
}

impl RsdmCheckbox {
    /// Connect `address` and wrap it in a checkbox with the given label.
    pub fn new(
        engine: &Engine,
        address: &str,
        label: impl Into<String>,
    ) -> Result<Self, EngineError> {
        Ok(Self {
            base: ChannelBase::new(engine.connect(address)?),
            label: label.into(),
        })
    }

    /// The underlying channel.
    pub fn channel(&self) -> &Channel {
        self.base.channel()
    }

    /// Whether the box is checked for `state`: the value is present and positive
    /// (PyDM `value_changed`: `new_val > 0`). A missing value is unchecked.
    pub fn is_checked(state: &ChannelState) -> bool {
        state
            .value
            .as_ref()
            .and_then(PvValue::as_f64)
            .is_some_and(|v| v > 0.0)
    }

    /// The value to write for a check state (PyDM emits `1`/`0`). A boolean
    /// channel keeps its `Bool` type; everything else is written as an integer.
    pub fn value_for(checked: bool, state: &ChannelState) -> PvValue {
        match state.value {
            Some(PvValue::Bool(_)) => PvValue::Bool(checked),
            _ => PvValue::Int(i64::from(checked)),
        }
    }

    /// Write the check state to the channel and return the value written.
    pub fn set_checked(&self, checked: bool) -> PvValue {
        let state = self.base.channel().state();
        let value = Self::value_for(checked, &state);
        self.base.channel().put(value.clone());
        value
    }

    /// Render the checkbox this frame. Returns the value written if the user
    /// toggled it.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<PvValue> {
        let state = self.base.channel().state();
        let mut checked = Self::is_checked(&state);
        let before = checked;
        self.base.framed(ui, &state, true, |ui| {
            ui.checkbox(&mut checked, self.label.as_str());
        });
        (checked != before).then(|| self.set_checked(checked))
    }
}

#[cfg(test)]
mod tests {
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

    fn state_of(value: Option<PvValue>) -> ChannelState {
        ChannelState {
            connected: true,
            value,
            ..ChannelState::default()
        }
    }

    #[test]
    fn checked_iff_value_is_positive() {
        assert!(RsdmCheckbox::is_checked(&state_of(Some(PvValue::Int(1)))));
        assert!(RsdmCheckbox::is_checked(&state_of(Some(PvValue::Float(
            0.5
        )))));
        assert!(RsdmCheckbox::is_checked(&state_of(Some(PvValue::Bool(
            true
        )))));
        assert!(!RsdmCheckbox::is_checked(&state_of(Some(PvValue::Int(0)))));
        assert!(!RsdmCheckbox::is_checked(&state_of(Some(PvValue::Bool(
            false
        )))));
        assert!(!RsdmCheckbox::is_checked(&state_of(None)));
    }

    #[test]
    fn value_for_keeps_bool_type_else_int() {
        assert_eq!(
            RsdmCheckbox::value_for(true, &state_of(Some(PvValue::Bool(false)))),
            PvValue::Bool(true)
        );
        assert_eq!(
            RsdmCheckbox::value_for(true, &state_of(Some(PvValue::Int(0)))),
            PvValue::Int(1)
        );
        assert_eq!(
            RsdmCheckbox::value_for(false, &state_of(Some(PvValue::Float(2.0)))),
            PvValue::Int(0)
        );
    }

    #[test]
    fn set_checked_writes_to_the_channel() {
        let engine = Engine::new();
        // Config-bearing loc (type+init) so the connection is connected — a
        // bare `loc://` is disconnected until configured (R3-12), and the
        // widget must be connected to accept the write.
        let checkbox = RsdmCheckbox::new(&engine, "loc://checkbox_set?type=int&init=0", "enable")
            .expect("connect");
        assert!(
            wait_for(|| checkbox.channel().is_connected(), Duration::from_secs(2)),
            "checkbox channel never connected"
        );
        checkbox.set_checked(true);
        assert!(
            wait_for(
                || checkbox
                    .channel()
                    .read(|s| s.value == Some(PvValue::Int(1))),
                Duration::from_secs(2)
            ),
            "channel did not receive the checked value (got {:?})",
            checkbox.channel().read(|s| s.value.clone())
        );
    }
}
