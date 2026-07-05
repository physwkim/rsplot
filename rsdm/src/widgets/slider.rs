//! `RsdmSlider` — a slider that writes a float.
//!
//! Ports `pydm/widgets/slider.py`: the track spans `num_steps` discrete
//! positions (PyDM default `101`) linearly mapped across the range
//! (`np.linspace(minimum, maximum, num=num_steps)`); the range comes from the
//! control limits unless the user overrides them (`reset_slider_limits` /
//! `userDefinedLimits`); moving the handle writes the mapped value
//! (`internal_slider_moved` → `send_value`); and the slider is interactive only
//! when connected, writable, and the limits are known
//! (`should_enable = write_access and connected and not needs_limit_info`).
//!
//! The range resolution is the pure
//! [`control_range`]; the write is
//! [`RsdmSlider::set_value`]; [`RsdmSlider::show`] is a thin egui shell.

use rsplot::egui;

use crate::channel::{Channel, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::{AlarmPalette, BorderMode, ChannelBase, UserLimits, control_range};
use crate::widgets::byte::Orientation;

/// PyDM's default number of slider positions (`PyDMSlider._num_steps`).
pub const DEFAULT_NUM_STEPS: u32 = 101;

/// A writable slider, horizontal by default (PyDM `PyDMSlider`).
pub struct RsdmSlider {
    base: ChannelBase,
    /// Override the min/max instead of using the PV's control limits (PyDM
    /// `userDefinedLimits`); per-bound so a single end can stay channel-driven
    /// (MEDM single-sided `limits`, R2-66).
    pub user_limits: UserLimits,
    /// Number of discrete positions along the track (PyDM `num_steps`).
    pub num_steps: u32,
    /// Override the displayed decimals; `None` uses the PV's precision (or `0`).
    pub precision_override: Option<i32>,
    /// Track direction (PyDM `PyDMSlider(orientation=…)`, Qt orientation).
    orientation: Orientation,
}

impl RsdmSlider {
    /// Connect `address` and wrap it in a slider.
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            // PyDMSlider ships with alarmSensitiveBorder = False and
            // alarmSensitiveContent = True (slider.py:264-265).
            base: ChannelBase::new(engine.connect(address)?)
                .with_border_mode(BorderMode::Off)
                .with_alarm_sensitive_content(true),
            user_limits: UserLimits::default(),
            num_steps: DEFAULT_NUM_STEPS,
            precision_override: None,
            orientation: Orientation::Horizontal,
        })
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

    /// Lay the track out horizontally (default) or vertically (builder style;
    /// PyDM passes the Qt orientation to `PyDMSlider`, `slider.py:35-36`; a
    /// vertical track grows upward, like Qt's un-inverted vertical slider and
    /// MEDM's `direction="up"` valuator).
    pub fn with_orientation(mut self, orientation: Orientation) -> Self {
        self.orientation = orientation;
        self
    }

    /// Set the number of discrete positions (builder style).
    pub fn with_num_steps(mut self, num_steps: u32) -> Self {
        self.num_steps = num_steps;
        self
    }

    /// Override the displayed decimals (builder style).
    pub fn with_precision(mut self, precision: i32) -> Self {
        self.precision_override = Some(precision);
        self
    }

    /// Choose which severities draw a border (builder style;
    /// `DisconnectedOnly` for converted MEDM screens — MEDM draws no severity
    /// border, the dash is the RsDM disconnect marker).
    pub fn with_border_mode(mut self, mode: BorderMode) -> Self {
        self.base.border_mode = mode;
        self
    }

    /// Colour the value readout by alarm severity when the channel is in alarm
    /// (MEDM `clrmod="alarm"` sets `XmNforeground = alarmColor`, medmValuator.c:
    /// 892-895; PyDM `alarmSensitiveContent`, which `RsdmSlider::new` defaults on
    /// — the converter turns it off for a MEDM valuator without `clrmod=alarm`).
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

    /// The value step between adjacent positions for `range`: `(hi - lo) /
    /// (num_steps - 1)`. At least two positions are assumed so a single-step
    /// slider does not divide by zero.
    pub fn step_size(&self, range: (f64, f64)) -> f64 {
        let intervals = self.num_steps.max(2) - 1;
        (range.1 - range.0) / f64::from(intervals)
    }

    /// Write `value` to the channel as a float (PyDM `send_value`) and return it.
    pub fn set_value(&self, value: f64) -> PvValue {
        let written = PvValue::Float(value);
        self.base.channel().put(written.clone());
        written
    }

    /// Render the slider this frame. Returns the value written if the user moved
    /// the handle. With no known range the slider is shown disabled (PyDM
    /// `needs_limit_info`).
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<PvValue> {
        let state = self.base.channel().state();
        let range = control_range(&state, self.user_limits);
        let decimals = self
            .precision_override
            .or(state.precision)
            .unwrap_or(0)
            .max(0);
        let mut value = state
            .value
            .as_ref()
            .and_then(PvValue::as_f64)
            .unwrap_or(0.0);

        let changed = self
            .base
            .framed_alarm_content(ui, &state, true, |ui| match range {
                Some((lo, hi)) => {
                    let step = self.step_size((lo, hi));
                    // Clamp/step-normalize EDITS only. The default
                    // `SliderClamping::Always` re-normalizes the incoming
                    // value every frame and marks the response changed when
                    // that alters it, which turned every off-grid monitor
                    // update into a write-back put (an external 13.6 on a
                    // 5..20 range came back as 13.55 one frame later). PyDM
                    // writes only from user interaction
                    // (`internal_slider_moved` → `send_value`).
                    let mut slider = egui::Slider::new(&mut value, lo..=hi)
                        .clamping(egui::SliderClamping::Edits)
                        .max_decimals(decimals.max(0) as usize);
                    if self.orientation == Orientation::Vertical {
                        slider = slider.vertical();
                    }
                    if step > 0.0 {
                        slider = slider.step_by(step);
                    }
                    ui.add(slider).changed()
                }
                None => {
                    // No limits yet: a disabled placeholder track (PyDM disables
                    // the slider until it has range information).
                    let mut slider = egui::Slider::new(&mut value, 0.0..=1.0);
                    if self.orientation == Orientation::Vertical {
                        slider = slider.vertical();
                    }
                    ui.add_enabled(false, slider);
                    false
                }
            })
            .inner;

        changed.then(|| self.set_value(value))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::channel::ChannelState;

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

    fn slider(address: &str) -> (Engine, RsdmSlider) {
        let engine = Engine::new();
        let slider = RsdmSlider::new(&engine, address).expect("connect");
        (engine, slider)
    }

    #[test]
    fn slider_defaults_alarm_border_off_content_on_like_pydm() {
        // PyDMSlider ships alarmSensitiveBorder = False and
        // alarmSensitiveContent = True (slider.py:264-265).
        let (_e, slider) = slider("loc://slider_alarm_defaults");
        assert_eq!(slider.base.border_mode, BorderMode::Off);
        assert!(slider.base.alarm_sensitive_content);
        // The converter turns it off for a MEDM valuator without clrmod=alarm.
        let slider = slider.with_alarm_sensitive_content(false);
        assert!(!slider.base.alarm_sensitive_content);
    }

    #[test]
    fn step_size_spans_the_range_over_num_steps_minus_one() {
        let (_e, slider) = slider("loc://slider_step_a");
        // Default 101 positions → 100 intervals.
        assert_eq!(slider.step_size((0.0, 100.0)), 1.0);
        let slider = slider.with_num_steps(11);
        assert_eq!(slider.step_size((0.0, 10.0)), 1.0);
    }

    #[test]
    fn step_size_never_divides_by_zero() {
        let (_e, slider) = slider("loc://slider_step_b");
        // A degenerate single-step request is clamped to at least one interval.
        let slider = slider.with_num_steps(1);
        assert_eq!(slider.step_size((0.0, 5.0)), 5.0);
    }

    #[test]
    fn range_uses_user_limits_over_ctrl_limits() {
        let st = ChannelState {
            connected: true,
            ctrl_limits: Some((0.0, 10.0)),
            ..ChannelState::default()
        };
        assert_eq!(control_range(&st, UserLimits::default()), Some((0.0, 10.0)));
        assert_eq!(
            control_range(&st, UserLimits::both(-5.0, 5.0)),
            Some((-5.0, 5.0))
        );
        let st = ChannelState {
            connected: true,
            ..ChannelState::default()
        };
        assert_eq!(control_range(&st, UserLimits::default()), None);
    }

    #[test]
    fn single_sided_limit_keeps_the_other_end_channel_driven() {
        // R2-66: MEDM hoprSrc="default" (upper pinned) keeps LOPR from the
        // channel; loprSrc="default" (lower pinned) keeps HOPR from the channel.
        let st = ChannelState {
            connected: true,
            ctrl_limits: Some((0.0, 10.0)),
            ..ChannelState::default()
        };
        // Upper pinned to 1.0, lower channel-driven (DRVL=0.0).
        let (_e, upper_only) = slider("loc://slider_upper_only");
        let upper_only = upper_only.with_upper_limit(1.0);
        assert_eq!(
            upper_only.user_limits,
            UserLimits {
                lower: None,
                upper: Some(1.0)
            }
        );
        assert_eq!(control_range(&st, upper_only.user_limits), Some((0.0, 1.0)));
        // Lower pinned to -3.0, upper channel-driven (DRVH=10.0).
        let (_e, lower_only) = slider("loc://slider_lower_only");
        let lower_only = lower_only.with_lower_limit(-3.0);
        assert_eq!(
            control_range(&st, lower_only.user_limits),
            Some((-3.0, 10.0))
        );
        // Single-sided with NO channel limit → no full range.
        let st_no_ctrl = ChannelState {
            connected: true,
            ..ChannelState::default()
        };
        assert_eq!(control_range(&st_no_ctrl, lower_only.user_limits), None);
    }

    #[test]
    fn external_off_grid_update_does_not_echo_a_put() {
        // PyDM parity: `send_value` fires only from user interaction
        // (`internal_slider_moved`); a monitor update must never write
        // back. With egui's default `SliderClamping::Always` the slider
        // re-normalized the incoming value every frame (clamp + step
        // rounding + max_decimals) and reported the result as changed, so
        // an external write landing off the step grid (13.6 on a 5..20
        // range = 0.15 steps) was echoed back to the channel as 13.55 one
        // frame later — retargeting the IOC that wrote it.
        // Config-bearing loc (type+init) so the connection connects — a bare
        // `loc://` stays disconnected until configured (R3-12).
        let (engine, slider) = slider("loc://slider_echo?type=float&init=0");
        let slider = slider.with_limits(5.0, 20.0).with_precision(3);
        let seed = engine.connect("loc://slider_echo").expect("seed handle");
        assert!(
            wait_for(|| slider.channel().is_connected(), Duration::from_secs(2)),
            "slider channel never connected"
        );
        seed.put(PvValue::Float(13.6));
        assert!(
            wait_for(
                || slider
                    .channel()
                    .read(|s| s.value == Some(PvValue::Float(13.6))),
                Duration::from_secs(2)
            ),
            "channel never saw the external 13.6"
        );

        let mut slider = slider;
        let mut harness = egui_kittest::Harness::new_ui(move |ui| {
            slider.show(ui);
        });
        harness.step();
        // Give an in-flight echo put time to land before asserting.
        std::thread::sleep(Duration::from_millis(100));
        harness.step();
        std::thread::sleep(Duration::from_millis(100));

        // Rendering without interaction must not write anything: the
        // channel still holds the un-snapped external value.
        assert_eq!(
            seed.read(|s| s.value.clone()),
            Some(PvValue::Float(13.6)),
            "slider echoed a write-back put on a pure monitor update"
        );
    }

    #[test]
    fn vertical_orientation_renders_taller_than_wide() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let (_engine, slider) = slider("loc://slider_vertical");
        let mut slider = slider
            .with_limits(0.0, 10.0)
            .with_orientation(Orientation::Vertical);
        assert_eq!(slider.orientation, Orientation::Vertical);

        let size = Rc::new(RefCell::new(egui::Vec2::ZERO));
        let out = Rc::clone(&size);
        let mut harness = egui_kittest::Harness::new_ui(move |ui| {
            slider.show(ui);
            *out.borrow_mut() = ui.min_size();
        });
        harness.step();
        let size = *size.borrow();
        assert!(
            size.y > size.x,
            "vertical slider must be taller than wide (got {size:?})"
        );
    }

    #[test]
    fn set_value_writes_a_float_to_the_channel() {
        // Config-bearing loc (type+init) so the connection connects — a bare
        // `loc://` stays disconnected until configured (R3-12).
        let (engine, slider) = slider("loc://slider_set?type=float&init=0");
        let _seed = engine.connect("loc://slider_set").expect("seed handle");
        assert!(
            wait_for(|| slider.channel().is_connected(), Duration::from_secs(2)),
            "slider channel never connected"
        );
        let written = slider.set_value(7.0);
        assert_eq!(written, PvValue::Float(7.0));
        assert!(
            wait_for(
                || slider
                    .channel()
                    .read(|s| s.value == Some(PvValue::Float(7.0))),
                Duration::from_secs(2)
            ),
            "channel did not receive the slider value (got {:?})",
            slider.channel().read(|s| s.value.clone())
        );
    }
}
