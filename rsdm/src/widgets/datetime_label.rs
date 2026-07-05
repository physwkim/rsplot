//! `RsdmDateTimeLabel` — render a numeric time channel as a date/time string.
//!
//! Ports `pydm/widgets/datetime.py` (`PyDMDateTimeLabel`): the channel value is a
//! number of milliseconds (or seconds, [`TimeBase`]) that is either an absolute
//! count since the Unix epoch or an offset relative to "now" (`relative`,
//! default on). `value_changed` truncates the value to an integer in its base
//! unit, converts to a `QDateTime`, and renders it with the `textFormat`.
//!
//! The value→epoch and epoch→string conversions are the pure
//! [`value_epoch_ms`] / [`format_datetime_ms`], unit-tested; the egui shell is
//! thin.
//!
//! **Deviation:** only PyDM's default format `yyyy/MM/dd hh:mm:ss.zzz` is
//! produced (UTC), as `YYYY/MM/DD hh:mm:ss.zzz`. Arbitrary Qt format strings are
//! not ported (that would require porting Qt's date-format mini-language).

use std::time::{SystemTime, UNIX_EPOCH};

use rsplot::egui;

use crate::channel::{Channel, ChannelState, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::ChannelBase;

/// Time base of the channel value (PyDM `TimeBase`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TimeBase {
    /// The value is in milliseconds (PyDM default).
    #[default]
    Milliseconds,
    /// The value is in seconds.
    Seconds,
}

pub(crate) const MS_PER_DAY: i64 = 86_400_000;

/// Convert the channel `raw` value to epoch milliseconds (PyDM `value_changed`):
/// the value is truncated to an integer in its base unit (PyDM `int(new_val)`),
/// scaled to milliseconds, and either taken as absolute (since epoch) or added
/// to `now_ms` when `relative`.
pub fn value_epoch_ms(raw: f64, time_base: TimeBase, relative: bool, now_ms: i64) -> i64 {
    // PyDM truncates to an integer *before* scaling, so sub-base-unit precision
    // is dropped (`int(new_val)` then `*= 1000` for seconds).
    let truncated = raw.trunc() as i64;
    let ms = match time_base {
        TimeBase::Milliseconds => truncated,
        TimeBase::Seconds => truncated.saturating_mul(1000),
    };
    if relative {
        now_ms.saturating_add(ms)
    } else {
        ms
    }
}

/// Format epoch milliseconds as `YYYY/MM/DD hh:mm:ss.zzz` in UTC (PyDM's default
/// `textFormat`).
pub fn format_datetime_ms(epoch_ms: i64) -> String {
    let days = epoch_ms.div_euclid(MS_PER_DAY);
    let ms_of_day = epoch_ms.rem_euclid(MS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let secs_of_day = ms_of_day / 1000;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    let millis = ms_of_day % 1000;
    format!("{year:04}/{month:02}/{day:02} {hour:02}:{minute:02}:{second:02}.{millis:03}")
}

/// Civil date `(year, month, day)` from a count of days since 1970-01-01
/// (Howard Hinnant's `civil_from_days`; valid for the proleptic Gregorian
/// calendar, including negative counts before the epoch).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (year + i64::from(month <= 2), month as u32, day)
}

/// Count of days since 1970-01-01 for a civil date `(year, month, day)`
/// (Howard Hinnant's `days_from_civil`; the exact inverse of
/// [`civil_from_days`], valid for the proleptic Gregorian calendar including
/// dates before the epoch). `month` is 1–12, `day` is 1–31.
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let m = i64::from(month);
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Current wall-clock time in epoch milliseconds (UTC).
pub(crate) fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A read-only label rendering a numeric time channel as a date/time string
/// (PyDM `PyDMDateTimeLabel`).
pub struct RsdmDateTimeLabel {
    base: ChannelBase,
    time_base: TimeBase,
    relative: bool,
}

impl RsdmDateTimeLabel {
    /// Connect `address` and wrap it in a date/time label.
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            base: ChannelBase::new(engine.connect(address)?),
            time_base: TimeBase::default(),
            relative: true,
        })
    }

    /// Set the value's time base (builder style; PyDM `timeBase`).
    pub fn with_time_base(mut self, time_base: TimeBase) -> Self {
        self.time_base = time_base;
        self
    }

    /// Whether the value is relative to "now" rather than an absolute epoch count
    /// (builder style; PyDM `relative`, default on).
    pub fn with_relative(mut self, relative: bool) -> Self {
        self.relative = relative;
        self
    }

    /// The underlying channel.
    pub fn channel(&self) -> &Channel {
        self.base.channel()
    }

    /// The text to display for `state` given the current time `now_ms`: the
    /// formatted date/time, or empty when there is no numeric value (PyDM starts
    /// blank and only sets text on `value_changed`).
    pub fn display_text(&self, state: &ChannelState, now_ms: i64) -> String {
        match state.value.as_ref().and_then(PvValue::as_f64) {
            Some(raw) => {
                format_datetime_ms(value_epoch_ms(raw, self.time_base, self.relative, now_ms))
            }
            None => String::new(),
        }
    }

    /// Render the label this frame.
    pub fn show(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let state = self.base.channel().state();
        let text = self.display_text(&state, now_epoch_ms());
        self.base
            .framed(ui, &state, false, |ui| ui.label(text))
            .inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_epoch_zero_is_unix_epoch() {
        assert_eq!(format_datetime_ms(0), "1970/01/01 00:00:00.000");
    }

    #[test]
    fn format_known_timestamp() {
        // 2021-01-01 00:00:00.000 UTC = 1_609_459_200_000 ms.
        assert_eq!(
            format_datetime_ms(1_609_459_200_000),
            "2021/01/01 00:00:00.000"
        );
        // Add 1h 2m 3.456s.
        let ms = 1_609_459_200_000 + (3600 + 2 * 60 + 3) * 1000 + 456;
        assert_eq!(format_datetime_ms(ms), "2021/01/01 01:02:03.456");
    }

    #[test]
    fn format_before_epoch_is_correct() {
        // 1969-12-31 23:59:59.000 UTC = -1000 ms.
        assert_eq!(format_datetime_ms(-1000), "1969/12/31 23:59:59.000");
    }

    #[test]
    fn absolute_value_is_taken_as_epoch() {
        // relative = false: the value is epoch ms directly.
        assert_eq!(
            value_epoch_ms(1_609_459_200_000.0, TimeBase::Milliseconds, false, 999),
            1_609_459_200_000
        );
    }

    #[test]
    fn seconds_base_truncates_then_scales() {
        // PyDM int()s the value first, so 1.9 s → 1 s → 1000 ms (sub-second lost).
        assert_eq!(value_epoch_ms(1.9, TimeBase::Seconds, false, 0), 1000);
    }

    #[test]
    fn relative_value_is_added_to_now() {
        // relative ms offset from now.
        assert_eq!(
            value_epoch_ms(5000.0, TimeBase::Milliseconds, true, 1_000_000),
            1_005_000
        );
    }

    #[test]
    fn days_from_civil_inverts_civil_from_days() {
        // Round-trip several dates spanning the epoch and a leap day.
        for (y, m, d) in [
            (1970, 1, 1),
            (2021, 1, 1),
            (2020, 2, 29),
            (1969, 12, 31),
            (1900, 3, 1),
            (2000, 2, 29),
        ] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d), "round trip {y}/{m}/{d}");
        }
        // 1970-01-01 is day 0.
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn no_value_renders_empty() {
        let engine = crate::Engine::new();
        let label = RsdmDateTimeLabel::new(&engine, "loc://dt_empty").expect("connect");
        let state = ChannelState {
            connected: true,
            value: None,
            ..ChannelState::default()
        };
        assert_eq!(label.display_text(&state, 0), "");
    }
}
