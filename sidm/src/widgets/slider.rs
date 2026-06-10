//! `PydmSlider` — a horizontal slider that writes a float.
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
//! [`control_range`](crate::widgets::base::control_range); the write is
//! [`PydmSlider::set_value`]; [`PydmSlider::show`] is a thin egui shell.

use siplot::egui;

use crate::channel::{Channel, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::{ChannelBase, control_range};

/// PyDM's default number of slider positions (`PyDMSlider._num_steps`).
pub const DEFAULT_NUM_STEPS: u32 = 101;

/// A writable horizontal slider (PyDM `PyDMSlider`).
pub struct PydmSlider {
    base: ChannelBase,
    /// Override the min/max instead of using the PV's control limits (PyDM
    /// `userDefinedLimits`).
    pub user_limits: Option<(f64, f64)>,
    /// Number of discrete positions along the track (PyDM `num_steps`).
    pub num_steps: u32,
    /// Override the displayed decimals; `None` uses the PV's precision (or `0`).
    pub precision_override: Option<i32>,
}

impl PydmSlider {
    /// Connect `address` and wrap it in a slider.
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            base: ChannelBase::new(engine.connect(address)?),
            user_limits: None,
            num_steps: DEFAULT_NUM_STEPS,
            precision_override: None,
        })
    }

    /// Override the min/max range (builder style; PyDM `userDefinedLimits`).
    pub fn with_limits(mut self, min: f64, max: f64) -> Self {
        self.user_limits = Some((min, max));
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
            .framed(ui, &state, true, |ui| match range {
                Some((lo, hi)) => {
                    let step = self.step_size((lo, hi));
                    let mut slider = egui::Slider::new(&mut value, lo..=hi)
                        .max_decimals(decimals.max(0) as usize);
                    if step > 0.0 {
                        slider = slider.step_by(step);
                    }
                    ui.add(slider).changed()
                }
                None => {
                    // No limits yet: a disabled placeholder track (PyDM disables
                    // the slider until it has range information).
                    ui.add_enabled(false, egui::Slider::new(&mut value, 0.0..=1.0));
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

    fn slider(address: &str) -> (Engine, PydmSlider) {
        let engine = Engine::new();
        let slider = PydmSlider::new(&engine, address).expect("connect");
        (engine, slider)
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
        assert_eq!(control_range(&st, None), Some((0.0, 10.0)));
        assert_eq!(control_range(&st, Some((-5.0, 5.0))), Some((-5.0, 5.0)));
        let st = ChannelState {
            connected: true,
            ..ChannelState::default()
        };
        assert_eq!(control_range(&st, None), None);
    }

    #[test]
    fn set_value_writes_a_float_to_the_channel() {
        let (engine, slider) = slider("loc://slider_set");
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
