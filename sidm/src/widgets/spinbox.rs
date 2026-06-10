//! `PydmSpinbox` — a numeric entry that writes a float.
//!
//! Ports `pydm/widgets/spinbox.py`: a `QDoubleSpinBox` whose decimals follow the
//! PV precision (`precision_changed` → `setDecimals`), whose min/max follow the
//! control limits unless the user overrides them (`reset_limits` /
//! `userDefinedLimits`), and which writes the entered value as a float on change
//! (`send_value`). PyDM's `step_exponent` single-step is reproduced as a builder
//! `step`, defaulting to `10^-precision` (one unit in the last displayed digit).
//!
//! The range resolution is the pure
//! [`control_range`]; the write is
//! [`PydmSpinbox::set_value`]; [`PydmSpinbox::show`] is a thin egui shell.

use siplot::egui;

use crate::channel::{Channel, ChannelState, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::{ChannelBase, control_range};

/// A writable numeric spin box (PyDM `PyDMSpinbox`).
pub struct PydmSpinbox {
    base: ChannelBase,
    /// Override the displayed decimals (PyDM `precision`); `None` uses the PV's
    /// precision (or `0`).
    pub precision_override: Option<i32>,
    /// Override the min/max instead of using the PV's control limits (PyDM
    /// `userDefinedLimits`).
    pub user_limits: Option<(f64, f64)>,
    /// The single-step increment (PyDM `step_exponent` → `10^exp`); `None`
    /// derives `10^-precision`.
    pub step: Option<f64>,
}

impl PydmSpinbox {
    /// Connect `address` and wrap it in a spin box.
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            base: ChannelBase::new(engine.connect(address)?),
            precision_override: None,
            user_limits: None,
            step: None,
        })
    }

    /// Override the displayed decimals (builder style).
    pub fn with_precision(mut self, precision: i32) -> Self {
        self.precision_override = Some(precision);
        self
    }

    /// Override the min/max range (builder style; PyDM `userDefinedLimits`).
    pub fn with_limits(mut self, min: f64, max: f64) -> Self {
        self.user_limits = Some((min, max));
        self
    }

    /// Set the single-step increment (builder style).
    pub fn with_step(mut self, step: f64) -> Self {
        self.step = Some(step);
        self
    }

    /// The underlying channel.
    pub fn channel(&self) -> &Channel {
        self.base.channel()
    }

    /// The decimals to display: the override, else the PV precision, else `0`
    /// (never negative).
    fn decimals(&self, state: &ChannelState) -> i32 {
        self.precision_override
            .or(state.precision)
            .unwrap_or(0)
            .max(0)
    }

    /// Write `value` to the channel as a float (PyDM `send_value`) and return it.
    pub fn set_value(&self, value: f64) -> PvValue {
        let written = PvValue::Float(value);
        self.base.channel().put(written.clone());
        written
    }

    /// Render the spin box this frame. Returns the value written if the user
    /// changed it.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<PvValue> {
        let state = self.base.channel().state();
        let decimals = self.decimals(&state);
        let step = self.step.unwrap_or_else(|| 10f64.powi(-decimals));
        let range = control_range(&state, self.user_limits);
        let mut value = state
            .value
            .as_ref()
            .and_then(PvValue::as_f64)
            .unwrap_or(0.0);

        let changed = self
            .base
            .framed(ui, &state, true, |ui| {
                let mut drag = egui::DragValue::new(&mut value)
                    .speed(step)
                    .max_decimals(decimals.max(0) as usize);
                if let Some((lo, hi)) = range {
                    drag = drag.range(lo..=hi);
                }
                ui.add(drag).changed()
            })
            .inner;

        changed.then(|| self.set_value(value))
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

    fn state_with(precision: Option<i32>, ctrl: Option<(f64, f64)>) -> ChannelState {
        ChannelState {
            connected: true,
            write_access: true,
            value: Some(PvValue::Float(0.0)),
            precision,
            ctrl_limits: ctrl,
            ..ChannelState::default()
        }
    }

    fn spinbox(address: &str) -> (Engine, PydmSpinbox) {
        let engine = Engine::new();
        let spin = PydmSpinbox::new(&engine, address).expect("connect");
        (engine, spin)
    }

    #[test]
    fn decimals_prefers_override_then_precision_then_zero() {
        let (_e, spin) = spinbox("loc://spin_decimals_a");
        assert_eq!(spin.decimals(&state_with(Some(2), None)), 2);
        let spin = spin.with_precision(4);
        assert_eq!(spin.decimals(&state_with(Some(2), None)), 4);
        let (_e, spin) = spinbox("loc://spin_decimals_b");
        assert_eq!(spin.decimals(&state_with(None, None)), 0);
        // A negative PV precision is clamped to zero.
        assert_eq!(spin.decimals(&state_with(Some(-3), None)), 0);
    }

    #[test]
    fn range_uses_user_limits_over_ctrl_limits() {
        let st = state_with(Some(1), Some((0.0, 10.0)));
        assert_eq!(control_range(&st, None), Some((0.0, 10.0)));
        assert_eq!(control_range(&st, Some((-1.0, 1.0))), Some((-1.0, 1.0)));
        let st = state_with(Some(1), None);
        assert_eq!(control_range(&st, None), None);
    }

    #[test]
    fn set_value_writes_a_float_to_the_channel() {
        let (engine, spin) = spinbox("loc://spin_set");
        let _seed = engine.connect("loc://spin_set").expect("seed handle");
        assert!(
            wait_for(|| spin.channel().is_connected(), Duration::from_secs(2)),
            "spinbox channel never connected"
        );
        let written = spin.set_value(3.5);
        assert_eq!(written, PvValue::Float(3.5));
        assert!(
            wait_for(
                || spin
                    .channel()
                    .read(|s| s.value == Some(PvValue::Float(3.5))),
                Duration::from_secs(2)
            ),
            "channel did not receive the spin value (got {:?})",
            spin.channel().read(|s| s.value.clone())
        );
    }
}
