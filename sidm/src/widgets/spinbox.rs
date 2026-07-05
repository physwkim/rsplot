//! `SidmSpinbox` — a numeric entry that writes a float.
//!
//! Ports `pydm/widgets/spinbox.py`: a `QDoubleSpinBox` whose decimals follow the
//! PV precision (`precision_changed` → `setDecimals`), whose min/max follow the
//! control limits unless the user overrides them (`reset_limits` /
//! `userDefinedLimits`), and which writes the entered value as a float on change
//! (`send_value`). PyDM's `step_exponent` single-step is reproduced faithfully:
//! the single step is `10^step_exponent` and `step_exponent` defaults to `0`
//! (step = 1.0), independent of precision (`spinbox.py:35`); Ctrl+Left/Right
//! adjust it (floored at `-decimals`, `spinbox.py:84-88`) and the `Step: 1E{n}`
//! suffix shows it (`spinbox.py:143-145`).
//!
//! The range resolution is the pure
//! [`control_range`], the step-exponent clamp is the pure
//! [`stepped_exponent`]; the write is
//! [`SidmSpinbox::set_value`]; [`SidmSpinbox::show`] is a thin egui shell.

use siplot::egui;

use crate::channel::{Channel, ChannelState, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::{AlarmPalette, BorderMode, ChannelBase, UserLimits, control_range};

/// A writable numeric spin box (PyDM `PyDMSpinbox`).
pub struct SidmSpinbox {
    base: ChannelBase,
    /// Override the displayed decimals (PyDM `precision`); `None` uses the PV's
    /// precision (or `0`).
    pub precision_override: Option<i32>,
    /// Override the min/max instead of using the PV's control limits (PyDM
    /// `userDefinedLimits`); per-bound so a single end can stay channel-driven
    /// (MEDM single-sided `limits`, R2-66).
    pub user_limits: UserLimits,
    /// The single-step exponent (PyDM `step_exponent`): the single step is
    /// `10^step_exponent`. Defaults to `0` (step = 1.0), independent of
    /// precision (`spinbox.py:35`); Ctrl+Left/Right adjust it in [`Self::show`].
    pub step_exponent: i32,
    /// Show the `Step: 1E{n}` suffix (PyDM `showStepExponent`, default `true`).
    pub show_step_exponent: bool,
    /// Send on every step/change rather than only on Enter (PyDM `writeOnPress`,
    /// default `false`, `spinbox.py:31`). When `false` the entry composes
    /// locally and commits once on Enter (`keyPressEvent` Return/Enter →
    /// `send_value`, `spinbox.py:90-91`).
    pub write_on_press: bool,
}

/// Clamp a step exponent to PyDM's floor of `-decimals` after a `delta` change
/// (`spinbox.py:88`, `step_exponent = max(-decimals, step_exponent ± 1)`).
pub fn stepped_exponent(step_exponent: i32, decimals: i32, delta: i32) -> i32 {
    (step_exponent + delta).max(-decimals)
}

/// Whether a spinbox interaction should write to the channel this frame. PyDM
/// sends on Enter unconditionally (`keyPressEvent`, `spinbox.py:90-91`) and on
/// any step/change only when `write_on_press` is set (`stepBy`,
/// `spinbox.py:55-66`, default `false`).
pub fn should_write(enter_pressed: bool, changed: bool, write_on_press: bool) -> bool {
    enter_pressed || (changed && write_on_press)
}

impl SidmSpinbox {
    /// Connect `address` and wrap it in a spin box.
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            // PyDMSpinbox ships with alarmSensitiveBorder = False (spinbox.py:29).
            base: ChannelBase::new(engine.connect(address)?).with_border_mode(BorderMode::Off),
            precision_override: None,
            user_limits: UserLimits::default(),
            step_exponent: 0,
            show_step_exponent: true,
            write_on_press: false,
        })
    }

    /// Override the displayed decimals (builder style).
    pub fn with_precision(mut self, precision: i32) -> Self {
        self.precision_override = Some(precision);
        self
    }

    /// Override both min/max range bounds (builder style; PyDM
    /// `userDefinedLimits`).
    pub fn with_limits(mut self, min: f64, max: f64) -> Self {
        self.user_limits = UserLimits::both(min, max);
        self
    }

    /// Pin only the lower bound, leaving the upper channel-driven (builder style;
    /// MEDM single-sided `loprSrc="default"`, R2-66).
    pub fn with_lower_limit(mut self, min: f64) -> Self {
        self.user_limits.lower = Some(min);
        self
    }

    /// Pin only the upper bound, leaving the lower channel-driven (builder style;
    /// MEDM single-sided `hoprSrc="default"`, R2-66).
    pub fn with_upper_limit(mut self, max: f64) -> Self {
        self.user_limits.upper = Some(max);
        self
    }

    /// Set the single-step exponent (builder style): the single step becomes
    /// `10^step_exponent` (PyDM `step_exponent`).
    pub fn with_step_exponent(mut self, step_exponent: i32) -> Self {
        self.step_exponent = step_exponent;
        self
    }

    /// The single step: `10^step_exponent` (PyDM `setSingleStep(10**exp)`).
    pub fn single_step(&self) -> f64 {
        10f64.powi(self.step_exponent)
    }

    /// Send on every step/change instead of only on Enter (builder style; PyDM
    /// `writeOnPress`).
    pub fn with_write_on_press(mut self, write_on_press: bool) -> Self {
        self.write_on_press = write_on_press;
        self
    }

    /// Choose which severities draw a border (builder style;
    /// `DisconnectedOnly` for converted MEDM screens — MEDM draws no severity
    /// border, the dash is the SiDM disconnect marker).
    pub fn with_border_mode(mut self, mode: BorderMode) -> Self {
        self.base.border_mode = mode;
        self
    }

    /// Colour the digits by alarm severity when the channel is in alarm (MEDM
    /// `clrmod="alarm"` sets `XmNforeground = alarmColor`, medmWheelSwitch.c:390;
    /// PyDM `alarmSensitiveContent`). Off by default.
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

    /// Render the spin box this frame. Returns the value written this frame:
    /// PyDM sends on Enter, and on every step only when `write_on_press` is set
    /// (default off — the entry composes locally and commits on Enter).
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<PvValue> {
        let state = self.base.channel().state();
        let decimals = self.decimals(&state);
        let step = self.single_step();
        let show_step = self.show_step_exponent;
        let step_exponent = self.step_exponent;
        let range = control_range(&state, self.user_limits);
        let mut value = state
            .value
            .as_ref()
            .and_then(PvValue::as_f64)
            .unwrap_or(0.0);

        let response = self
            .base
            .framed_alarm_content(ui, &state, true, |ui| {
                let mut drag = egui::DragValue::new(&mut value)
                    .speed(step)
                    .max_decimals(decimals.max(0) as usize);
                if show_step {
                    // PyDM's showStepExponent suffix (spinbox.py:143-145); with
                    // units off it is " Step: 1E{n}".
                    drag = drag.suffix(format!(" Step: 1E{step_exponent}"));
                }
                if let Some((lo, hi)) = range {
                    drag = drag.range(lo..=hi);
                }
                ui.add(drag)
            })
            .inner;

        // Ctrl+Left/Right adjust the step exponent while the entry is focused
        // (spinbox.py:84-88); each change is floored at -decimals.
        if response.has_focus() {
            let (left, right) = ui.input(|i| {
                (
                    i.modifiers.ctrl && i.key_pressed(egui::Key::ArrowLeft),
                    i.modifiers.ctrl && i.key_pressed(egui::Key::ArrowRight),
                )
            });
            if left {
                self.step_exponent = stepped_exponent(self.step_exponent, decimals, 1);
            } else if right {
                self.step_exponent = stepped_exponent(self.step_exponent, decimals, -1);
            }
        }

        // PyDM sends only on Enter (the entry buffers edits until commit), and on
        // each step only under write_on_press — never on every drag/step tick.
        let enter_pressed = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        should_write(enter_pressed, response.changed(), self.write_on_press)
            .then(|| self.set_value(value))
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

    fn spinbox(address: &str) -> (Engine, SidmSpinbox) {
        let engine = Engine::new();
        let spin = SidmSpinbox::new(&engine, address).expect("connect");
        (engine, spin)
    }

    #[test]
    fn step_defaults_to_one_independent_of_precision() {
        // PyDM's step_exponent defaults to 0 → single step 1.0, whatever the
        // precision (spinbox.py:35); sidm previously used 10^-precision.
        let (_e, spin) = spinbox("loc://spin_step_default");
        assert_eq!(spin.step_exponent, 0);
        assert_eq!(spin.single_step(), 1.0);
        // decimals(state) has no bearing on the step: a PREC=3 PV still steps 1.0.
        assert_eq!(spin.decimals(&state_with(Some(3), None)), 3);
        assert_eq!(spin.single_step(), 1.0);
    }

    #[test]
    fn with_step_exponent_sets_the_power_of_ten() {
        let (_e, spin) = spinbox("loc://spin_step_exp");
        assert_eq!(spin.with_step_exponent(-2).single_step(), 0.01);
        let (_e, spin) = spinbox("loc://spin_step_exp2");
        assert_eq!(spin.with_step_exponent(3).single_step(), 1000.0);
    }

    #[test]
    fn write_gated_on_enter_unless_write_on_press() {
        // The core R2-53 fix: a change without Enter and without write_on_press
        // must NOT write (PyDM sends only on Enter by default).
        assert!(!should_write(false, true, false));
        // Enter always writes, changed or not (keyPressEvent → send_value).
        assert!(should_write(true, false, false));
        assert!(should_write(true, true, false));
        // write_on_press writes on any change (stepBy → send_value).
        assert!(should_write(false, true, true));
        // Idle: no interaction, no write.
        assert!(!should_write(false, false, false));
        assert!(!should_write(false, false, true));
    }

    #[test]
    fn write_on_press_defaults_off_like_pydm() {
        let (_e, spin) = spinbox("loc://spin_wop_default");
        assert!(!spin.write_on_press);
        assert!(spin.with_write_on_press(true).write_on_press);
    }

    #[test]
    fn stepped_exponent_floors_at_negative_decimals() {
        // Ctrl+Left (+1) / Ctrl+Right (-1), floored at -decimals (spinbox.py:88).
        assert_eq!(stepped_exponent(0, 3, 1), 1);
        assert_eq!(stepped_exponent(0, 3, -1), -1);
        // Already at the floor: Ctrl+Right cannot go below -decimals.
        assert_eq!(stepped_exponent(-3, 3, -1), -3);
        // Ctrl+Left from the floor moves up normally.
        assert_eq!(stepped_exponent(-3, 3, 1), -2);
    }

    #[test]
    fn spinbox_defaults_alarm_border_off_like_pydm() {
        // PyDMSpinbox ships alarmSensitiveBorder = False (spinbox.py:29).
        let (_e, spin) = spinbox("loc://spin_border");
        assert_eq!(spin.base.border_mode, BorderMode::Off);
        assert!(!spin.base.alarm_sensitive_content);
        // The builder opts the digits into severity colouring (MEDM clrmod=alarm).
        let spin = spin.with_alarm_sensitive_content(true);
        assert!(spin.base.alarm_sensitive_content);
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
        assert_eq!(control_range(&st, UserLimits::default()), Some((0.0, 10.0)));
        assert_eq!(
            control_range(&st, UserLimits::both(-1.0, 1.0)),
            Some((-1.0, 1.0))
        );
        let st = state_with(Some(1), None);
        assert_eq!(control_range(&st, UserLimits::default()), None);
    }

    #[test]
    fn single_sided_limit_keeps_the_other_end_channel_driven() {
        // R2-66: one pinned bound, the other from the channel (DRVL/DRVH).
        let st = state_with(Some(1), Some((0.0, 10.0)));
        let (_e, spin) = spinbox("loc://spin_upper_only");
        let spin = spin.with_upper_limit(1.0);
        assert_eq!(
            spin.user_limits,
            UserLimits {
                lower: None,
                upper: Some(1.0)
            }
        );
        assert_eq!(control_range(&st, spin.user_limits), Some((0.0, 1.0)));
        let (_e, spin) = spinbox("loc://spin_lower_only");
        let spin = spin.with_lower_limit(-3.0);
        assert_eq!(control_range(&st, spin.user_limits), Some((-3.0, 10.0)));
        // No channel limit → a single-sided override can't form a full range.
        assert_eq!(
            control_range(&state_with(Some(1), None), spin.user_limits),
            None
        );
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
