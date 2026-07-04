//! `SidmPushButton` — write a fixed value on click.
//!
//! Ports `pydm/widgets/pushbutton.py`: on click it writes `press_value` to the
//! channel; with `relative` set it writes `current + press_value` for numeric
//! channels (PyDM `__execute_send`). An optional `release_value` is written
//! after the press (PyDM's release write), and an optional confirmation dialog
//! gates the write (PyDM `showConfirmDialog`, rendered here as an
//! [`egui::Modal`]).
//!
//! The value computation is the pure [`compute_send_value`]; the click/modal
//! handling in [`SidmPushButton::show`] is a thin shell over
//! [`SidmPushButton::send_press`]. PyDM's password protection and the separate
//! press-vs-release pointer timing are not ported: a click is one press (plus an
//! absolute release write when `release_value` is set).

use siplot::egui;

use crate::channel::{Channel, ChannelState, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::{AlarmPalette, BorderMode, ChannelBase};
use crate::widgets::display_format::{DisplayFormat, FormatSpec};
use crate::widgets::line_edit::parse_input;

/// PyDM's default confirmation prompt.
pub const DEFAULT_CONFIRM_MESSAGE: &str = "Are you sure you want to proceed?";

/// Compute the value a press would write: `press_value` parsed to the channel's
/// type, or `current + press_value` when `relative` is set on a numeric channel
/// (PyDM `__execute_send`). Returns `None` when the channel has no current value
/// or the text does not parse.
pub fn compute_send_value(
    press_value: &str,
    state: &ChannelState,
    relative: bool,
) -> Option<PvValue> {
    // PyDM requires a current value before it will send.
    state.value.as_ref()?;
    let spec = FormatSpec {
        format: DisplayFormat::Default,
        precision: None,
        show_units: false,
    };
    let parsed = parse_input(press_value, state, spec).ok()?;
    if !relative {
        return Some(parsed);
    }
    // Relative is only meaningful for numeric channels; otherwise send absolute.
    match &state.value {
        Some(PvValue::Int(cur)) => Some(PvValue::Int(cur + parsed.as_i64()?)),
        Some(PvValue::Float(cur)) => Some(PvValue::Float(cur + parsed.as_f64()?)),
        _ => Some(parsed),
    }
}

/// A momentary write button (PyDM `PyDMPushButton`).
pub struct SidmPushButton {
    base: ChannelBase,
    /// Button caption.
    pub label: String,
    /// Value text written on press (PyDM `pressValue`).
    pub press_value: String,
    /// Optional value text written after the press (PyDM `releaseValue` /
    /// `writeWhenRelease`). Written as an absolute value.
    pub release_value: Option<String>,
    /// Add `press_value` to the current value instead of replacing it (PyDM
    /// `relativeChange`).
    pub relative: bool,
    /// Require a confirmation dialog before writing (PyDM `showConfirmDialog`).
    pub show_confirm_dialog: bool,
    /// The confirmation prompt text (PyDM `confirmMessage`).
    pub confirm_message: String,
    /// Whether the confirmation modal is currently open.
    confirm_pending: bool,
}

impl SidmPushButton {
    /// Connect `address` and wrap it in a push button with the given caption and
    /// press value.
    pub fn new(
        engine: &Engine,
        address: &str,
        label: impl Into<String>,
        press_value: impl Into<String>,
    ) -> Result<Self, EngineError> {
        Ok(Self {
            // PyDMPushButton ships with alarmSensitiveBorder = False
            // (pushbutton.py:74).
            base: ChannelBase::new(engine.connect(address)?).with_border_mode(BorderMode::Off),
            label: label.into(),
            press_value: press_value.into(),
            release_value: None,
            relative: false,
            show_confirm_dialog: false,
            confirm_message: DEFAULT_CONFIRM_MESSAGE.to_owned(),
            confirm_pending: false,
        })
    }

    /// Write `current + press_value` rather than `press_value` (builder style).
    pub fn with_relative(mut self, relative: bool) -> Self {
        self.relative = relative;
        self
    }

    /// Set a release value to write after the press (builder style).
    pub fn with_release_value(mut self, release_value: impl Into<String>) -> Self {
        self.release_value = Some(release_value.into());
        self
    }

    /// Require a confirmation dialog with `message` before writing (builder
    /// style).
    pub fn with_confirm(mut self, message: impl Into<String>) -> Self {
        self.show_confirm_dialog = true;
        self.confirm_message = message.into();
        self
    }

    /// Choose which severities draw a border (builder style;
    /// `DisconnectedOnly` for converted MEDM screens — MEDM draws no severity
    /// border, the dash is the SiDM disconnect marker).
    pub fn with_border_mode(mut self, mode: BorderMode) -> Self {
        self.base.border_mode = mode;
        self
    }

    /// Colour the button caption by alarm severity when the channel is in alarm
    /// (MEDM `clrmod="alarm"` sets `XmNforeground = alarmColor`,
    /// medmMessageButton.c:348; PyDM `alarmSensitiveContent`). Off by default.
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

    /// Perform the write (press value, then the release value if any) and return
    /// the press value written, or `None` if it could not be computed.
    pub fn send_press(&self) -> Option<PvValue> {
        let state = self.base.channel().state();
        let press = compute_send_value(&self.press_value, &state, self.relative)?;
        self.base.channel().put(press.clone());
        if let Some(release) = &self.release_value
            && let Some(value) = compute_send_value(release, &state, false)
        {
            self.base.channel().put(value);
        }
        Some(press)
    }

    /// Render the button this frame. Returns the press value written this frame,
    /// or `None`. With a confirmation dialog enabled, the value is written only
    /// after the user confirms.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<PvValue> {
        let state = self.base.channel().state();
        let mut sent = None;

        let clicked = self
            .base
            .framed_alarm_content(ui, &state, true, |ui| {
                ui.button(self.label.as_str()).clicked()
            })
            .inner;
        if clicked {
            if self.show_confirm_dialog {
                self.confirm_pending = true;
            } else {
                sent = self.send_press();
            }
        }

        if self.confirm_pending {
            let id = egui::Id::new((
                "pydm_pushbutton_confirm",
                self.base.channel().address().raw(),
            ));
            let mut decision = None;
            let modal = egui::Modal::new(id).show(ui.ctx(), |ui| {
                ui.label(self.confirm_message.as_str());
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                    if ui.button("OK").clicked() {
                        decision = Some(true);
                    }
                });
            });
            // Clicking outside / pressing Esc cancels.
            if modal.should_close() {
                decision = decision.or(Some(false));
            }
            match decision {
                Some(true) => {
                    self.confirm_pending = false;
                    sent = self.send_press();
                }
                Some(false) => self.confirm_pending = false,
                None => {}
            }
        }
        sent
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
    fn push_button_defaults_alarm_border_off_like_pydm() {
        // PyDMPushButton ships alarmSensitiveBorder = False (pushbutton.py:74).
        let engine = Engine::new();
        let button =
            SidmPushButton::new(&engine, "loc://pushbtn_border", "Step", "5").expect("connect");
        assert_eq!(button.base.border_mode, BorderMode::Off);
        assert!(!button.base.alarm_sensitive_content);
        // The builder opts the caption into severity colouring (MEDM clrmod=alarm).
        let button = button.with_alarm_sensitive_content(true);
        assert!(button.base.alarm_sensitive_content);
    }

    #[test]
    fn absolute_send_parses_press_value_to_channel_type() {
        let st = state_of(Some(PvValue::Int(0)));
        assert_eq!(compute_send_value("42", &st, false), Some(PvValue::Int(42)));
        let st = state_of(Some(PvValue::Float(0.0)));
        assert_eq!(
            compute_send_value("1.5", &st, false),
            Some(PvValue::Float(1.5))
        );
    }

    #[test]
    fn relative_send_adds_to_current_numeric_value() {
        let st = state_of(Some(PvValue::Int(10)));
        assert_eq!(compute_send_value("5", &st, true), Some(PvValue::Int(15)));
        let st = state_of(Some(PvValue::Float(2.5)));
        assert_eq!(
            compute_send_value("0.5", &st, true),
            Some(PvValue::Float(3.0))
        );
    }

    #[test]
    fn relative_on_string_channel_sends_absolute() {
        // Relative is not meaningful for a string channel.
        let st = state_of(Some(PvValue::Str("x".into())));
        assert_eq!(
            compute_send_value("hello", &st, true),
            Some(PvValue::Str("hello".into()))
        );
    }

    #[test]
    fn no_current_value_means_no_send() {
        let st = state_of(None);
        assert_eq!(compute_send_value("1", &st, false), None);
    }

    #[test]
    fn unparseable_press_value_means_no_send() {
        let st = state_of(Some(PvValue::Int(0)));
        assert_eq!(compute_send_value("not-an-int", &st, false), None);
    }

    #[test]
    fn send_press_writes_to_the_channel() {
        let engine = Engine::new();
        let writer = engine.connect("loc://pushbtn_send").expect("seed handle");
        // Seed a numeric value so relative has something to add to.
        writer.put(PvValue::Int(10));
        let button = SidmPushButton::new(&engine, "loc://pushbtn_send", "Step", "5")
            .expect("connect")
            .with_relative(true);
        assert!(
            wait_for(
                || button.channel().read(|s| s.value == Some(PvValue::Int(10))),
                Duration::from_secs(2)
            ),
            "seed value not observed"
        );
        let sent = button.send_press();
        assert_eq!(sent, Some(PvValue::Int(15)));
        assert!(
            wait_for(
                || button.channel().read(|s| s.value == Some(PvValue::Int(15))),
                Duration::from_secs(2)
            ),
            "channel did not receive the relative press value (got {:?})",
            button.channel().read(|s| s.value.clone())
        );
    }
}
