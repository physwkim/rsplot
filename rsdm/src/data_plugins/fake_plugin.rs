//! `fake://` — in-process signal generators for demos and tests.
//!
//! PyDM's `fake_plugin` emits a random value on a timer; this generalizes it to
//! a few deterministic waveforms plus value-driven alarm severity, so examples
//! and tests can exercise the value/alarm/connection paths with no IOC. Read
//! only (writes are dropped).
//!
//! Address form: `fake://name?wave=sine&period=2&rate=10&min=-1&max=1`.
//! `wave` is one of `sine` (default), `ramp`, `square`, `noise`; `period` is the
//! cycle length in seconds; `rate` is updates per second; `min`/`max` bound the
//! amplitude. Warning/alarm limits default to the outer 25%/5% of the range
//! (overridable with `low`/`high`/`lolo`/`hihi`), so a sine sweep cycles
//! NoAlarm → Minor → Major.

use std::time::{Duration, SystemTime};

use crate::channel::{AlarmSeverity, PvValue};
use crate::data_plugins::{ConnectionCtx, DataPlugin};
use crate::engine::EngineError;

/// The `fake://` data plugin.
#[derive(Debug, Default, Clone, Copy)]
pub struct FakePlugin;

/// Generator waveform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wave {
    /// Sinusoid through the full `[min, max]` range.
    Sine,
    /// Rising sawtooth `[min, max)`.
    Ramp,
    /// Square wave alternating `max`/`min` each half period.
    Square,
    /// Deterministic pseudo-random samples in `[min, max)`.
    Noise,
}

impl Wave {
    fn parse(s: &str) -> Self {
        match s {
            "ramp" => Self::Ramp,
            "square" => Self::Square,
            "noise" | "random" => Self::Noise,
            _ => Self::Sine,
        }
    }
}

/// Parsed generator configuration (pure; built from query parameters).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FakeConfig {
    /// Waveform shape.
    pub wave: Wave,
    /// Cycle length in seconds.
    pub period: f64,
    /// Updates per second.
    pub rate: f64,
    /// Amplitude bounds.
    pub min: f64,
    /// Amplitude bounds.
    pub max: f64,
    /// Warning low/high limits.
    pub warn: (f64, f64),
    /// Alarm low/high limits.
    pub alarm: (f64, f64),
}

impl Default for FakeConfig {
    fn default() -> Self {
        Self::from_params(&[])
    }
}

impl FakeConfig {
    /// Build a config from query parameters, applying defaults and clamping
    /// `period`/`rate` to positive values.
    pub fn from_params(params: &[(String, String)]) -> Self {
        let mut wave = Wave::Sine;
        let mut period = 2.0_f64;
        let mut rate = 10.0_f64;
        let mut min = -1.0_f64;
        let mut max = 1.0_f64;
        let (mut low, mut high, mut lolo, mut hihi) = (None, None, None, None);
        for (key, value) in params {
            match key.as_str() {
                "wave" => wave = Wave::parse(value),
                "period" => {
                    if let Ok(v) = value.parse() {
                        period = v;
                    }
                }
                "rate" => {
                    if let Ok(v) = value.parse() {
                        rate = v;
                    }
                }
                "min" => {
                    if let Ok(v) = value.parse() {
                        min = v;
                    }
                }
                "max" => {
                    if let Ok(v) = value.parse() {
                        max = v;
                    }
                }
                "low" => low = value.parse().ok(),
                "high" => high = value.parse().ok(),
                "lolo" => lolo = value.parse().ok(),
                "hihi" => hihi = value.parse().ok(),
                _ => {}
            }
        }
        // Reject non-positive / non-finite (incl. NaN) period and rate.
        if !period.is_finite() || period <= 0.0 {
            period = 2.0;
        }
        if !rate.is_finite() || rate <= 0.0 {
            rate = 10.0;
        }
        if max < min {
            std::mem::swap(&mut min, &mut max);
        }
        let range = max - min;
        let warn = (
            low.unwrap_or(min + 0.25 * range),
            high.unwrap_or(min + 0.75 * range),
        );
        let alarm = (
            lolo.unwrap_or(min + 0.05 * range),
            hihi.unwrap_or(min + 0.95 * range),
        );
        Self {
            wave,
            period,
            rate,
            min,
            max,
            warn,
            alarm,
        }
    }
}

impl DataPlugin for FakePlugin {
    fn protocol(&self) -> &'static str {
        "fake"
    }

    fn connect(&self, ctx: ConnectionCtx) -> Result<(), EngineError> {
        let ConnectionCtx {
            writer,
            mut writes,
            // `fake://` does not reconfigure per listener (see `ConnectionCtx::listeners`).
            listeners: _,
            cancel,
            runtime,
            address,
        } = ctx;
        let cfg = FakeConfig::from_params(&address.query_params());

        runtime.spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs_f64(1.0 / cfg.rate));
            let start = tokio::time::Instant::now();
            let mut tick: u64 = 0;
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    // Read-only: a closed write queue (all Channels gone) ends
                    // the task; queued writes are ignored.
                    maybe = writes.recv() => {
                        if maybe.is_none() {
                            break;
                        }
                    }
                    _ = interval.tick() => {
                        let t = start.elapsed().as_secs_f64();
                        let value = sample(cfg.wave, t, tick, cfg.period, cfg.min, cfg.max);
                        let severity = severity_for(value, cfg.warn, cfg.alarm);
                        writer.post_value(move |s| {
                            s.connected = true;
                            s.write_access = false;
                            s.value = Some(PvValue::Float(value));
                            s.severity = severity;
                            s.warn_limits = Some(cfg.warn);
                            s.alarm_limits = Some(cfg.alarm);
                            s.timestamp = Some(SystemTime::now());
                        });
                        tick = tick.wrapping_add(1);
                    }
                }
            }
        });

        Ok(())
    }
}

/// Sample the waveform at elapsed time `t` (seconds) / counter `tick`. Result is
/// in `[min, max]` (`[min, max)` for `Ramp`/`Noise`).
pub fn sample(wave: Wave, t: f64, tick: u64, period: f64, min: f64, max: f64) -> f64 {
    let mid = 0.5 * (min + max);
    let amp = 0.5 * (max - min);
    let phase = (t / period).rem_euclid(1.0);
    match wave {
        Wave::Sine => mid + amp * (std::f64::consts::TAU * phase).sin(),
        Wave::Ramp => min + (max - min) * phase,
        Wave::Square => {
            if phase < 0.5 {
                max
            } else {
                min
            }
        }
        Wave::Noise => min + (max - min) * pseudo_random(tick),
    }
}

/// Deterministic pseudo-random in `[0, 1)` from a counter (splitmix64 finalizer).
fn pseudo_random(tick: u64) -> f64 {
    let mut z = tick
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0x9E37_79B9);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    // Top 53 bits → [0, 1).
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// Alarm severity from a value against warning `(low, high)` and alarm
/// `(lolo, hihi)` limits (EPICS HIGH/HIHI/LOW/LOLO semantics).
pub fn severity_for(value: f64, warn: (f64, f64), alarm: (f64, f64)) -> AlarmSeverity {
    let (low, high) = warn;
    let (lolo, hihi) = alarm;
    if value <= lolo || value >= hihi {
        AlarmSeverity::Major
    } else if value <= low || value >= high {
        AlarmSeverity::Minor
    } else {
        AlarmSeverity::NoAlarm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn config_defaults_and_derived_limits() {
        let c = FakeConfig::default();
        assert_eq!(c.wave, Wave::Sine);
        assert_eq!(c.period, 2.0);
        assert_eq!(c.rate, 10.0);
        assert_eq!((c.min, c.max), (-1.0, 1.0));
        // range 2.0: warn outer 25%, alarm outer 5% (float tolerance — the
        // 0.05/0.95 fractions are not exact in f64).
        assert!((c.warn.0 - (-0.5)).abs() < 1e-9 && (c.warn.1 - 0.5).abs() < 1e-9);
        assert!((c.alarm.0 - (-0.9)).abs() < 1e-9 && (c.alarm.1 - 0.9).abs() < 1e-9);
    }

    #[test]
    fn config_clamps_nonpositive_period_and_rate_and_orders_bounds() {
        let c = FakeConfig::from_params(&params(&[
            ("period", "0"),
            ("rate", "-5"),
            ("min", "3"),
            ("max", "1"),
            ("wave", "ramp"),
        ]));
        assert_eq!(c.period, 2.0);
        assert_eq!(c.rate, 10.0);
        assert_eq!((c.min, c.max), (1.0, 3.0));
        assert_eq!(c.wave, Wave::Ramp);
    }

    #[test]
    fn sine_passes_through_midpoint_and_peak() {
        // t=0 -> midpoint; t=period/4 -> max.
        assert!((sample(Wave::Sine, 0.0, 0, 2.0, -1.0, 1.0) - 0.0).abs() < 1e-9);
        assert!((sample(Wave::Sine, 0.5, 0, 2.0, -1.0, 1.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ramp_is_monotonic_within_a_period() {
        let a = sample(Wave::Ramp, 0.0, 0, 2.0, 0.0, 10.0);
        let b = sample(Wave::Ramp, 1.0, 0, 2.0, 0.0, 10.0);
        assert!((a - 0.0).abs() < 1e-9);
        assert!((b - 5.0).abs() < 1e-9);
    }

    #[test]
    fn square_alternates_each_half_period() {
        assert_eq!(sample(Wave::Square, 0.1, 0, 2.0, -1.0, 1.0), 1.0);
        assert_eq!(sample(Wave::Square, 1.1, 0, 2.0, -1.0, 1.0), -1.0);
    }

    #[test]
    fn all_waves_stay_in_bounds() {
        for wave in [Wave::Sine, Wave::Ramp, Wave::Square, Wave::Noise] {
            for k in 0..200u64 {
                let t = k as f64 * 0.037;
                let v = sample(wave, t, k, 1.7, -2.0, 5.0);
                assert!(
                    (-2.0..=5.0).contains(&v),
                    "{wave:?} produced {v} out of range"
                );
            }
        }
    }

    #[test]
    fn noise_is_deterministic() {
        assert_eq!(
            sample(Wave::Noise, 0.0, 42, 1.0, 0.0, 1.0),
            sample(Wave::Noise, 9.9, 42, 1.0, 0.0, 1.0)
        );
    }

    #[test]
    fn severity_thresholds_cycle() {
        let warn = (-0.5, 0.5);
        let alarm = (-0.9, 0.9);
        assert_eq!(severity_for(0.0, warn, alarm), AlarmSeverity::NoAlarm);
        assert_eq!(severity_for(0.6, warn, alarm), AlarmSeverity::Minor);
        assert_eq!(severity_for(-0.6, warn, alarm), AlarmSeverity::Minor);
        assert_eq!(severity_for(0.95, warn, alarm), AlarmSeverity::Major);
        assert_eq!(severity_for(-0.95, warn, alarm), AlarmSeverity::Major);
    }
}
