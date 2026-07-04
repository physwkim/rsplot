//! Date-time tick layout on a numeric axis whose data values are epoch
//! seconds (UTC).
//!
//! A faithful port of silx `gui/plot/_utils/dtime_ticklayout.py`. The silx
//! original operates on `datetime.datetime` objects backed by `dateutil`; here
//! the data values are `f64` POSIX timestamps (seconds since 1970-01-01
//! 00:00:00 UTC) and the civil-date arithmetic is implemented directly via the
//! standard "days from civil" algorithm (Howard Hinnant,
//! <http://howardhinnant.github.io/date_algorithms.html>), so the civil
//! arithmetic itself carries no `chrono` dependency.
//!
//! Per-axis time zone: every entry point has a `_tz` variant taking a
//! [`TimeZone`], mirroring silx `Axis.setTimeZone`. The legacy non-`_tz`
//! functions are [`TimeZone::Utc`] wrappers, so existing UTC callers are
//! unchanged. [`TimeZone`] covers the full silx surface: [`TimeZone::Utc`]
//! (`"UTC"`) and [`TimeZone::FixedOffset`] (`datetime.timezone(timedelta(...))`)
//! need no tz database, while [`TimeZone::Named`] (an arbitrary `dateutil.tz`
//! zone) and [`TimeZone::local`] (`None`/local time) use the bundled IANA tz
//! database (`tzdb`/`tz-rs`) for an instant-dependent, DST-aware offset. The
//! layout code is identical across zones — only the single offset lookup
//! [`TimeZone::offset_at`] varies.
//!
//! The two entry points mirror the Python module:
//!
//! - [`best_unit`] (`bestUnit`): pick the tick unit + a fractional count for a
//!   duration in seconds.
//! - [`calc_ticks`] (`calcTicks`): given `(min, max)` epoch seconds and a
//!   target tick count, return the tick positions (epoch seconds), the nice
//!   spacing, and the chosen unit.
//!
//! [`format_tick`] mirrors `bestFormatString` + `strftime` for a single tick.

use crate::core::ticklayout::nice_num_generic;

// Constants mirror silx dtime_ticklayout.py:49-54.
const MICROSECONDS_PER_SECOND: f64 = 1_000_000.0;
const SECONDS_PER_MINUTE: f64 = 60.0;
const SECONDS_PER_HOUR: f64 = 60.0 * SECONDS_PER_MINUTE;
const SECONDS_PER_DAY: f64 = 24.0 * SECONDS_PER_HOUR;
const SECONDS_PER_YEAR: f64 = 365.25 * SECONDS_PER_DAY;
const SECONDS_PER_MONTH_AVERAGE: f64 = SECONDS_PER_YEAR / 12.0;

/// The unit a tick step is expressed in (silx `DtUnit`). Discriminants match
/// silx so the `< unit.value` ordering comparisons in `round_to_element` port
/// directly (`YEARS = 0` … `MICRO_SECONDS = 6`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DtUnit {
    /// Calendar years.
    Years = 0,
    /// Calendar months.
    Months = 1,
    /// Days.
    Days = 2,
    /// Hours.
    Hours = 3,
    /// Minutes.
    Minutes = 4,
    /// Seconds.
    Seconds = 5,
    /// A fraction of a second.
    MicroSeconds = 6,
}

impl DtUnit {
    /// The "nice" step values silx allows for this unit
    /// (`NICE_DATE_VALUES`, dtime_ticklayout.py:288-296). The last entry is the
    /// base used by [`nice_num_generic`].
    fn nice_values(self) -> &'static [f64] {
        match self {
            DtUnit::Years => &[1.0, 2.0, 5.0, 10.0],
            DtUnit::Months => &[1.0, 2.0, 3.0, 4.0, 6.0, 12.0],
            DtUnit::Days => &[1.0, 2.0, 3.0, 7.0, 14.0, 28.0],
            DtUnit::Hours => &[1.0, 2.0, 3.0, 4.0, 6.0, 12.0],
            DtUnit::Minutes => &[1.0, 2.0, 3.0, 5.0, 10.0, 15.0, 30.0],
            DtUnit::Seconds => &[1.0, 2.0, 3.0, 5.0, 10.0, 15.0, 30.0],
            DtUnit::MicroSeconds => &[1.0, 2.0, 3.0, 4.0, 5.0, 10.0],
        }
    }
}

/// The time zone a date-time axis is laid out in (silx `Axis.setTimeZone`).
///
/// silx accepts any `datetime.tzinfo`, the string `"UTC"`, or `None` (local
/// time). The variants cover that surface:
///
/// - [`TimeZone::Utc`] — silx `"UTC"`; zero offset, no tz database needed.
/// - [`TimeZone::FixedOffset`] — silx `datetime.timezone(timedelta(...))`; a
///   constant offset, no tz database needed.
/// - [`TimeZone::Named`] — a DST-aware IANA zone (e.g. `America/New_York`)
///   resolved from the bundled tz database; the offset is instant-dependent.
///   Build with [`TimeZone::named`]; silx `None`/local time is
///   [`TimeZone::local`].
///
/// The single offset lookup is [`TimeZone::offset_at`]; decompose, recompose,
/// and tick layout are all expressed in terms of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeZone {
    /// Coordinated Universal Time — zero offset (silx `setTimeZone("UTC")`).
    Utc,
    /// A constant offset east of UTC, in seconds (silx
    /// `setTimeZone(datetime.timezone(timedelta(seconds=seconds_east)))`).
    /// Positive is east (e.g. `+32400` for UTC+09:00); negative is west.
    FixedOffset {
        /// Seconds east of UTC. The wall-clock time equals UTC plus this.
        seconds_east: i32,
    },
    /// A DST-aware named IANA zone from the bundled tz database (silx accepting
    /// an arbitrary `dateutil.tz` zone). The borrowed [`tz::TimeZoneRef`] points
    /// into the statically-compiled database, so it stays `Copy`. Construct via
    /// [`TimeZone::named`] / [`TimeZone::local`].
    Named(tz::TimeZoneRef<'static>),
}

impl TimeZone {
    /// Resolve an IANA zone name (e.g. `"America/New_York"`, `"Europe/Paris"`)
    /// from the bundled tz database into a [`TimeZone::Named`]. Returns `None`
    /// for an unknown name (silx would raise on an invalid tzinfo).
    pub fn named(name: &str) -> Option<TimeZone> {
        tzdb::tz_by_name(name).map(TimeZone::Named)
    }

    /// The system's local time zone as a [`TimeZone::Named`] (silx
    /// `setTimeZone(None)`, "interpret as local time"). Returns `None` if the
    /// local zone cannot be determined.
    pub fn local() -> Option<TimeZone> {
        tzdb::local_tz().map(TimeZone::Named)
    }

    /// The UTC offset in seconds (east positive) that applies at the given UTC
    /// instant `epoch_utc` (POSIX seconds). For [`TimeZone::Utc`] and
    /// [`TimeZone::FixedOffset`] the offset is constant and the argument is
    /// ignored; for [`TimeZone::Named`] it is looked up in the tz database
    /// (`find_local_time_type`), falling back to `0` only if the instant is
    /// outside the database's representable range.
    pub fn offset_at(self, epoch_utc: f64) -> i32 {
        match self {
            TimeZone::Utc => 0,
            TimeZone::FixedOffset { seconds_east } => seconds_east,
            TimeZone::Named(tz) => tz
                .find_local_time_type(epoch_utc.floor() as i64)
                .map(|lt| lt.ut_offset())
                .unwrap_or(0),
        }
    }
}

/// A UTC civil date-time decomposed to the resolution silx uses (microseconds).
///
/// This is the Rust stand-in for the `datetime.datetime` objects silx passes
/// around. Fields are not range-validated on construction beyond what
/// [`DateTime::from_civil`] guarantees; callers go through the constructors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    /// Civil year (may be negative; proleptic Gregorian).
    pub year: i64,
    /// Month, 1-12.
    pub month: u32,
    /// Day of month, 1-31.
    pub day: u32,
    /// Hour, 0-23.
    pub hour: u32,
    /// Minute, 0-59.
    pub minute: u32,
    /// Second, 0-59.
    pub second: u32,
    /// Microsecond, 0-999_999.
    pub microsecond: u32,
}

/// Number of days from the civil epoch (1970-01-01) to `y-m-d`.
///
/// Howard Hinnant's `days_from_civil`. Valid for any proleptic Gregorian date;
/// `m ∈ [1, 12]`, `d ∈ [1, last day of month]`. Negative results are days
/// before the epoch.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    // Shift so the year starts in March (leap day at year end).
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as i64 + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Inverse of [`days_from_civil`]: civil `(year, month, day)` for a day count
/// relative to 1970-01-01. Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Days in `month` of `year` (proleptic Gregorian).
fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            if leap { 29 } else { 28 }
        }
        _ => 30,
    }
}

impl DateTime {
    /// Build from civil components without normalization. The day is clamped to
    /// the last valid day of the month (mirrors how silx's `setDateElement`
    /// would otherwise raise; here a clamp keeps tick generation total).
    pub fn from_civil(
        year: i64,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
        microsecond: u32,
    ) -> Self {
        let month = month.clamp(1, 12);
        let day = day.clamp(1, days_in_month(year, month));
        Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
            microsecond,
        }
    }

    /// Decompose an epoch-seconds timestamp (UTC) into civil components.
    ///
    /// Uses floor division so timestamps before the epoch decompose correctly
    /// (e.g. `-0.5` is `1969-12-31 23:59:59.500000`).
    pub fn from_epoch_seconds(epoch: f64) -> Self {
        // Split into whole seconds (floor) and sub-second microseconds so the
        // microsecond field is always in [0, 999_999].
        let whole = epoch.floor();
        let frac = epoch - whole; // [0, 1)
        let microsecond = (frac * MICROSECONDS_PER_SECOND).round() as i64;
        // Rounding can push frac up to a full second.
        let (mut total_secs, microsecond) = if microsecond >= 1_000_000 {
            (whole as i64 + 1, 0u32)
        } else {
            (whole as i64, microsecond as u32)
        };

        let days = total_secs.div_euclid(SECONDS_PER_DAY as i64);
        let sod = total_secs.rem_euclid(SECONDS_PER_DAY as i64); // seconds of day, [0, 86399]
        total_secs = sod;
        let hour = (total_secs / 3600) as u32;
        let minute = ((total_secs % 3600) / 60) as u32;
        let second = (total_secs % 60) as u32;
        let (year, month, day) = civil_from_days(days);
        Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
            microsecond,
        }
    }

    /// Convert back to epoch seconds (UTC). Exact inverse of
    /// [`DateTime::from_epoch_seconds`] for in-range civil dates.
    pub fn to_epoch_seconds(self) -> f64 {
        let days = days_from_civil(self.year, self.month, self.day);
        let secs = days * (SECONDS_PER_DAY as i64)
            + self.hour as i64 * 3600
            + self.minute as i64 * 60
            + self.second as i64;
        secs as f64 + self.microsecond as f64 / MICROSECONDS_PER_SECOND
    }

    /// Decompose an epoch-seconds timestamp into the wall-clock civil
    /// components of `tz` — silx `datetime.fromtimestamp(epoch, tz=...)`.
    ///
    /// The epoch is shifted east by the zone offset at that instant and then
    /// decomposed as a UTC civil date-time, so the resulting fields read as the
    /// local wall clock. [`TimeZone::Utc`] reduces to
    /// [`DateTime::from_epoch_seconds`].
    pub fn from_epoch_seconds_tz(epoch: f64, tz: TimeZone) -> Self {
        DateTime::from_epoch_seconds(epoch + tz.offset_at(epoch) as f64)
    }

    /// Convert a wall-clock civil date-time in `tz` back to epoch seconds —
    /// silx `datetime.timestamp()` on a tz-aware datetime. Inverse of
    /// [`DateTime::from_epoch_seconds_tz`].
    ///
    /// For a DST-aware zone the offset depends on the local wall time, which is
    /// what we hold (not the UTC instant). We treat the wall clock as if it were
    /// UTC to get a first offset estimate, then refine once at the corrected
    /// instant — the standard two-step local→UTC inversion. For a constant
    /// zone both steps return the same offset, so this is exact.
    pub fn to_epoch_seconds_tz(self, tz: TimeZone) -> f64 {
        let wall = self.to_epoch_seconds();
        let off1 = tz.offset_at(wall);
        let off2 = tz.offset_at(wall - off1 as f64);
        wall - off2 as f64
    }

    /// The integer value of the date element for `unit` (silx
    /// `getDateElement`).
    fn get_element(self, unit: DtUnit) -> i64 {
        match unit {
            DtUnit::Years => self.year,
            DtUnit::Months => self.month as i64,
            DtUnit::Days => self.day as i64,
            DtUnit::Hours => self.hour as i64,
            DtUnit::Minutes => self.minute as i64,
            DtUnit::Seconds => self.second as i64,
            DtUnit::MicroSeconds => self.microsecond as i64,
        }
    }

    /// Return a copy with the element for `unit` set to `value` (silx
    /// `setDateElement`). The day is clamped via [`DateTime::from_civil`].
    fn set_element(self, value: i64, unit: DtUnit) -> Self {
        let mut dt = self;
        match unit {
            DtUnit::Years => dt.year = value,
            DtUnit::Months => dt.month = value as u32,
            DtUnit::Days => dt.day = value as u32,
            DtUnit::Hours => dt.hour = value as u32,
            DtUnit::Minutes => dt.minute = value as u32,
            DtUnit::Seconds => dt.second = value as u32,
            DtUnit::MicroSeconds => dt.microsecond = value as u32,
        }
        DateTime::from_civil(
            dt.year,
            dt.month,
            dt.day,
            dt.hour,
            dt.minute,
            dt.second,
            dt.microsecond,
        )
    }

    /// Round down to `unit`: zero out every element finer than `unit` (silx
    /// `roundToElement`). Years are never rounded; month/day floor to 1, the
    /// time fields floor to 0.
    fn round_to_element(self, unit: DtUnit) -> Self {
        let u = unit as i64;
        let month = if u < DtUnit::Months as i64 {
            1
        } else {
            self.month
        };
        let day = if u < DtUnit::Days as i64 { 1 } else { self.day };
        let hour = if u < DtUnit::Hours as i64 {
            0
        } else {
            self.hour
        };
        let minute = if u < DtUnit::Minutes as i64 {
            0
        } else {
            self.minute
        };
        let second = if u < DtUnit::Seconds as i64 {
            0
        } else {
            self.second
        };
        let microsecond = if u < DtUnit::MicroSeconds as i64 {
            0
        } else {
            self.microsecond
        };
        DateTime::from_civil(self.year, month, day, hour, minute, second, microsecond)
    }

    /// Add `value` of `unit` to this date-time (silx `addValueToDate`). Year and
    /// month additions truncate `value` to an integer (relativedelta has no
    /// fractional year/month); calendar overflow rolls correctly.
    fn add_value(self, value: f64, unit: DtUnit) -> Self {
        match unit {
            DtUnit::Years => {
                let n = value as i64;
                DateTime::from_civil(
                    self.year + n,
                    self.month,
                    self.day,
                    self.hour,
                    self.minute,
                    self.second,
                    self.microsecond,
                )
            }
            DtUnit::Months => {
                let n = value as i64;
                // Total months since year 0, then re-split. relativedelta
                // clamps the day to the target month's length, which
                // from_civil reproduces via its day clamp.
                let total = (self.year) * 12 + (self.month as i64 - 1) + n;
                let year = total.div_euclid(12);
                let month = (total.rem_euclid(12) + 1) as u32;
                DateTime::from_civil(
                    year,
                    month,
                    self.day,
                    self.hour,
                    self.minute,
                    self.second,
                    self.microsecond,
                )
            }
            DtUnit::Days => self.add_seconds(value * SECONDS_PER_DAY),
            DtUnit::Hours => self.add_seconds(value * SECONDS_PER_HOUR),
            DtUnit::Minutes => self.add_seconds(value * SECONDS_PER_MINUTE),
            DtUnit::Seconds => self.add_seconds(value),
            DtUnit::MicroSeconds => self.add_seconds(value / MICROSECONDS_PER_SECOND),
        }
    }

    /// Add a (possibly fractional) number of seconds via the epoch round-trip.
    fn add_seconds(self, seconds: f64) -> Self {
        DateTime::from_epoch_seconds(self.to_epoch_seconds() + seconds)
    }
}

/// Nice value for a date element (silx `niceDateTimeElement`). For years and
/// months the result is at least 1 and integral (no fractional year/month).
/// The per-unit step list is passed as custom `nice_fractions` to the shared
/// [`nice_num_generic`], whose non-default branch averages adjacent fractions
/// for the round table.
fn nice_date_time_element(value: f64, unit: DtUnit, is_round: bool) -> f64 {
    let elem = nice_num_generic(value, Some(unit.nice_values()), is_round);
    if unit == DtUnit::Years || unit == DtUnit::Months {
        (elem as i64).max(1) as f64
    } else {
        elem
    }
}

/// Pick the best tick unit for a duration (silx `bestUnit`).
///
/// Returns `(count, unit)` where `count` is the duration expressed in `unit`s
/// (fractional). The thresholds (and their per-unit factors) match silx exactly
/// (dtime_ticklayout.py:272-285).
pub fn best_unit(duration_seconds: f64) -> (f64, DtUnit) {
    if duration_seconds > SECONDS_PER_YEAR * 3.0 {
        (duration_seconds / SECONDS_PER_YEAR, DtUnit::Years)
    } else if duration_seconds > SECONDS_PER_MONTH_AVERAGE * 3.0 {
        (duration_seconds / SECONDS_PER_MONTH_AVERAGE, DtUnit::Months)
    } else if duration_seconds > SECONDS_PER_DAY * 2.0 {
        (duration_seconds / SECONDS_PER_DAY, DtUnit::Days)
    } else if duration_seconds > SECONDS_PER_HOUR * 2.0 {
        (duration_seconds / SECONDS_PER_HOUR, DtUnit::Hours)
    } else if duration_seconds > SECONDS_PER_MINUTE * 2.0 {
        (duration_seconds / SECONDS_PER_MINUTE, DtUnit::Minutes)
    } else if duration_seconds > 2.0 {
        (duration_seconds, DtUnit::Seconds)
    } else {
        (
            duration_seconds * MICROSECONDS_PER_SECOND,
            DtUnit::MicroSeconds,
        )
    }
}

/// Round a date down to the nearest nice start tick (silx `findStartDate`).
///
/// Returns `(start, spacing, unit)`. `n_ticks` is the target tick count.
fn find_start_date(d_min: DateTime, d_max: DateTime, n_ticks: usize) -> (DateTime, f64, DtUnit) {
    let min_epoch = d_min.to_epoch_seconds();
    let max_epoch = d_max.to_epoch_seconds();
    debug_assert!(max_epoch >= min_epoch, "d_min should come before d_max");

    if min_epoch == max_epoch {
        // Range smaller than microsecond resolution.
        return (d_min, 1.0, DtUnit::MicroSeconds);
    }

    let length_sec = max_epoch - min_epoch;
    let (length, unit) = best_unit(length_sec);
    let nice_length = nice_date_time_element(length, unit, false);
    let nice_spacing = nice_date_time_element(nice_length / n_ticks as f64, unit, true);

    let d_val = d_min.get_element(unit) as f64;

    let nice_val = if unit == DtUnit::Months || unit == DtUnit::Days {
        ((d_val - 1.0) / nice_spacing).floor() * nice_spacing + 1.0
    } else {
        (d_val / nice_spacing).floor() * nice_spacing
    };

    // silx: dt.MINYEAR == 1. Guard a degenerate year start.
    let nice_val = if unit == DtUnit::Years && nice_val <= 1.0 {
        nice_spacing.max(1.0)
    } else {
        nice_val
    };

    let start = d_min.round_to_element(unit);
    let start = start.set_element(nice_val as i64, unit);

    (start, nice_spacing, unit)
}

/// Generate tick date-times in `[d_min, d_max)` stepping by `step` `unit`s
/// (silx `dateRange`). When `include_first_beyond` is set, the first tick at or
/// past `d_max` is also emitted (silx default for `calcTicks`).
fn date_range(
    d_min: DateTime,
    d_max: DateTime,
    step: f64,
    unit: DtUnit,
    include_first_beyond: bool,
) -> Vec<DateTime> {
    // Year/month/microsecond have integral steps of at least 1.
    let step = if unit == DtUnit::Years || unit == DtUnit::Months || unit == DtUnit::MicroSeconds {
        step.max(1.0)
    } else {
        debug_assert!(step > 0.0, "tickstep is 0");
        step
    };

    let max_epoch = d_max.to_epoch_seconds();
    let mut out = Vec::new();
    let mut dt = d_min;
    // Bound the loop defensively: even a degenerate step cannot exceed the
    // theoretical tick count by much; cap to avoid an infinite loop if the
    // arithmetic ever fails to advance.
    let mut guard = 0usize;
    while dt.to_epoch_seconds() < max_epoch {
        out.push(dt);
        let next = dt.add_value(step, unit);
        if next.to_epoch_seconds() <= dt.to_epoch_seconds() {
            // No forward progress (out of representable range / zero step):
            // stop to stay total.
            dt = next;
            break;
        }
        dt = next;
        guard += 1;
        if guard > 1_000_000 {
            break;
        }
    }
    if include_first_beyond {
        out.push(dt);
    }
    out
}

/// Compute tick positions for a datetime axis (silx `calcTicks`).
///
/// `min`/`max` are epoch seconds (UTC); `n_ticks` is the target tick count (the
/// actual count may differ). Returns `(ticks, spacing, unit)` where `ticks` are
/// epoch seconds. The returned ticks always bracket `[min, max]`: the first is
/// at or below `min` (rounded-down start) and the last is at or beyond `max`
/// (the `include_first_beyond` tick).
pub fn calc_ticks(min: f64, max: f64, n_ticks: usize) -> (Vec<f64>, f64, DtUnit) {
    calc_ticks_tz(min, max, n_ticks, TimeZone::Utc)
}

/// As [`calc_ticks`] but laying the ticks out in the wall-clock calendar of
/// `tz` (silx passes the tz-aware `dMin`/`dMax` into `calcTicks`). The endpoints
/// are decomposed with the zone offset, all the nice-date arithmetic runs in
/// that wall-clock space (identical to the UTC path), and each tick is converted
/// back to epoch seconds with the offset. The returned ticks are epoch seconds.
pub fn calc_ticks_tz(min: f64, max: f64, n_ticks: usize, tz: TimeZone) -> (Vec<f64>, f64, DtUnit) {
    let n_ticks = n_ticks.max(1);
    let d_min = DateTime::from_epoch_seconds_tz(min, tz);
    let d_max = DateTime::from_epoch_seconds_tz(max, tz);
    let (start, spacing, unit) = find_start_date(d_min, d_max, n_ticks);
    let dates = date_range(start, d_max, spacing, unit, true);
    let ticks = dates.iter().map(|d| d.to_epoch_seconds_tz(tz)).collect();
    (ticks, spacing, unit)
}

/// As [`calc_ticks`] but derives the target tick count from an axis length in
/// pixels and a tick density (silx `calcTicksAdaptive`). At least 2 ticks.
pub fn calc_ticks_adaptive(
    min: f64,
    max: f64,
    axis_length: f64,
    tick_density: f64,
) -> (Vec<f64>, f64, DtUnit) {
    calc_ticks_adaptive_tz(min, max, axis_length, tick_density, TimeZone::Utc)
}

/// As [`calc_ticks_adaptive`] but laying the ticks out in the wall-clock
/// calendar of `tz` (silx `calcTicksAdaptive` on tz-aware datetimes).
pub fn calc_ticks_adaptive_tz(
    min: f64,
    max: f64,
    axis_length: f64,
    tick_density: f64,
    tz: TimeZone,
) -> (Vec<f64>, f64, DtUnit) {
    let n = (tick_density * axis_length).round() as i64;
    let n = n.max(2) as usize;
    calc_ticks_tz(min, max, n, tz)
}

/// Zero-pad an integer to `width` digits.
fn pad(value: i64, width: usize) -> String {
    if value < 0 {
        format!("-{:0width$}", -value, width = width)
    } else {
        format!("{value:0width$}")
    }
}

/// Format a single tick (epoch seconds, UTC) for the given `spacing`/`unit`,
/// mirroring silx `bestFormatString` + `datetime.strftime`.
///
/// For [`DtUnit::MicroSeconds`] the silx code additionally strips a common run
/// of trailing zeros across all labels; that cross-label step needs the whole
/// tick set, so it is done in [`format_ticks`]. This single-tick helper returns
/// the raw `%S.%f` form for microseconds.
pub fn format_tick(epoch: f64, spacing: f64, unit: DtUnit) -> String {
    format_tick_tz(epoch, spacing, unit, TimeZone::Utc)
}

/// As [`format_tick`] but rendering the wall-clock label in `tz` (silx formats
/// the tz-aware tick datetime). The epoch is decomposed with the zone offset
/// before formatting.
pub fn format_tick_tz(epoch: f64, spacing: f64, unit: DtUnit, tz: TimeZone) -> String {
    let d = DateTime::from_epoch_seconds_tz(epoch, tz);
    let is_small = spacing < 1.0;
    match unit {
        DtUnit::Years => {
            if is_small {
                // silx uses "%Y-m" here (literal "m"); reproduced faithfully.
                format!("{}-m", pad(d.year, 4))
            } else {
                pad(d.year, 4)
            }
        }
        DtUnit::Months => {
            if is_small {
                format!(
                    "{}-{}-{}",
                    pad(d.year, 4),
                    pad(d.month as i64, 2),
                    pad(d.day as i64, 2)
                )
            } else {
                format!("{}-{}", pad(d.year, 4), pad(d.month as i64, 2))
            }
        }
        DtUnit::Days => {
            if is_small {
                format!("{}:{}", pad(d.hour as i64, 2), pad(d.minute as i64, 2))
            } else {
                format!(
                    "{}-{}-{}",
                    pad(d.year, 4),
                    pad(d.month as i64, 2),
                    pad(d.day as i64, 2)
                )
            }
        }
        DtUnit::Hours => {
            format!("{}:{}", pad(d.hour as i64, 2), pad(d.minute as i64, 2))
        }
        DtUnit::Minutes => {
            if is_small {
                format!(
                    "{}:{}:{}",
                    pad(d.hour as i64, 2),
                    pad(d.minute as i64, 2),
                    pad(d.second as i64, 2)
                )
            } else {
                format!("{}:{}", pad(d.hour as i64, 2), pad(d.minute as i64, 2))
            }
        }
        DtUnit::Seconds => {
            if is_small {
                format!(
                    "{}.{}",
                    pad(d.second as i64, 2),
                    pad(d.microsecond as i64, 6)
                )
            } else {
                format!(
                    "{}:{}:{}",
                    pad(d.hour as i64, 2),
                    pad(d.minute as i64, 2),
                    pad(d.second as i64, 2)
                )
            }
        }
        DtUnit::MicroSeconds => {
            format!(
                "{}.{}",
                pad(d.second as i64, 2),
                pad(d.microsecond as i64, 6)
            )
        }
    }
}

/// Format a whole tick set (silx `formatDatetimes`). For
/// [`DtUnit::MicroSeconds`] this applies silx's cross-label trailing-zero strip:
/// it finds the minimum number of trailing `'0'` shared by every label (capped
/// at 5), drops a leading `'0'` per label, and trims that many trailing chars.
pub fn format_ticks(ticks: &[f64], spacing: f64, unit: DtUnit) -> Vec<String> {
    format_ticks_tz(ticks, spacing, unit, TimeZone::Utc)
}

/// As [`format_ticks`] but rendering the wall-clock labels in `tz`.
pub fn format_ticks_tz(ticks: &[f64], spacing: f64, unit: DtUnit, tz: TimeZone) -> Vec<String> {
    if unit != DtUnit::MicroSeconds {
        return ticks
            .iter()
            .map(|&t| format_tick_tz(t, spacing, unit, tz))
            .collect();
    }

    let texts: Vec<String> = ticks
        .iter()
        .map(|&t| format_tick_tz(t, spacing, unit, tz))
        .collect();
    if texts.is_empty() {
        return texts;
    }

    let nzeros = texts
        .iter()
        .map(|t| t.len() - t.trim_end_matches('0').len())
        .min()
        .unwrap_or(0);
    let trim = nzeros.min(5);

    texts
        .iter()
        .map(|text| {
            let chars: Vec<char> = text.chars().collect();
            // text[0 if text[0] != '0' else 1 : -min(nzeros, 5)]
            let start = if chars.first() == Some(&'0') { 1 } else { 0 };
            // Python slice with negative stop: end index counted from the end.
            let end = chars.len().saturating_sub(trim);
            if start >= end {
                String::new()
            } else {
                chars[start..end].iter().collect()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn civil_epoch_round_trip_known_dates() {
        // (epoch seconds, expected civil). Values cross-checked against known
        // UTC instants.
        let cases = [
            (0.0_f64, (1970, 1, 1, 0, 0, 0)),
            (86_400.0, (1970, 1, 2, 0, 0, 0)),
            // 2000-02-29 (leap day) 12:30:45 UTC.
            (951_827_445.0, (2000, 2, 29, 12, 30, 45)),
            // 2021-01-01 00:00:00 UTC.
            (1_609_459_200.0, (2021, 1, 1, 0, 0, 0)),
            // 2024-02-29 (leap day) 23:59:59 UTC.
            (1_709_251_199.0, (2024, 2, 29, 23, 59, 59)),
        ];
        for (epoch, (y, m, d, hh, mm, ss)) in cases {
            let dt = DateTime::from_epoch_seconds(epoch);
            assert_eq!(
                (dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second),
                (y, m, d, hh, mm, ss),
                "decompose {epoch}"
            );
            // And the round-trip back is exact.
            assert!(
                close(dt.to_epoch_seconds(), epoch, 1e-6),
                "round-trip {epoch}"
            );
        }
    }

    #[test]
    fn civil_round_trip_before_epoch() {
        // Negative timestamp: 1969-12-31 23:59:59 UTC.
        let dt = DateTime::from_epoch_seconds(-1.0);
        assert_eq!(
            (dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second),
            (1969, 12, 31, 23, 59, 59)
        );
        assert!(close(dt.to_epoch_seconds(), -1.0, 1e-6));
    }

    #[test]
    fn civil_to_epoch_for_leap_day() {
        // 2000-02-29 is day 11016 from epoch; verify both directions.
        let days = days_from_civil(2000, 2, 29);
        assert_eq!(civil_from_days(days), (2000, 2, 29));
        let dt = DateTime::from_civil(2000, 2, 29, 0, 0, 0, 0);
        assert_eq!(dt.to_epoch_seconds(), days as f64 * SECONDS_PER_DAY);
    }

    #[test]
    fn best_unit_picks_day_for_a_week() {
        // ~7-day range -> DAYS (between 2 days and 2 months threshold).
        let (count, unit) = best_unit(7.0 * SECONDS_PER_DAY);
        assert_eq!(unit, DtUnit::Days);
        assert!(close(count, 7.0, 1e-9), "count={count}");
    }

    #[test]
    fn best_unit_picks_hour_for_six_hours() {
        // ~6-hour range -> HOURS (between 2 hours and 2 days threshold).
        let (count, unit) = best_unit(6.0 * SECONDS_PER_HOUR);
        assert_eq!(unit, DtUnit::Hours);
        assert!(close(count, 6.0, 1e-9), "count={count}");
    }

    #[test]
    fn best_unit_boundary_units() {
        // Each branch boundary: just over the threshold selects the coarser unit.
        assert_eq!(best_unit(SECONDS_PER_YEAR * 3.0 + 1.0).1, DtUnit::Years);
        assert_eq!(
            best_unit(SECONDS_PER_MONTH_AVERAGE * 3.0 + 1.0).1,
            DtUnit::Months
        );
        assert_eq!(best_unit(SECONDS_PER_DAY * 2.0 + 1.0).1, DtUnit::Days);
        assert_eq!(best_unit(SECONDS_PER_HOUR * 2.0 + 1.0).1, DtUnit::Hours);
        assert_eq!(best_unit(SECONDS_PER_MINUTE * 2.0 + 1.0).1, DtUnit::Minutes);
        assert_eq!(best_unit(2.0 + 0.5).1, DtUnit::Seconds);
        // <= 2 seconds falls through to microseconds.
        assert_eq!(best_unit(1.0).1, DtUnit::MicroSeconds);
        assert_eq!(best_unit(0.0).1, DtUnit::MicroSeconds);
    }

    #[test]
    fn calc_ticks_endpoints_bracket_the_range() {
        // A one-week window starting at 2021-01-04 00:00:00 UTC.
        let min = DateTime::from_civil(2021, 1, 4, 0, 0, 0, 0).to_epoch_seconds();
        let max = DateTime::from_civil(2021, 1, 11, 0, 0, 0, 0).to_epoch_seconds();
        let (ticks, _spacing, unit) = calc_ticks(min, max, 5);
        assert_eq!(unit, DtUnit::Days);
        assert!(ticks.len() >= 2, "ticks={ticks:?}");
        // First tick at or below min, last tick at or beyond max (brackets range).
        assert!(ticks[0] <= min + 1e-6, "first {} > min {min}", ticks[0]);
        assert!(
            *ticks.last().unwrap() >= max - 1e-6,
            "last {} < max {max}",
            ticks.last().unwrap()
        );
        // Ticks are strictly increasing.
        for w in ticks.windows(2) {
            assert!(w[1] > w[0], "non-increasing: {:?}", w);
        }
    }

    #[test]
    fn calc_ticks_six_hour_window_uses_hours() {
        let min = DateTime::from_civil(2021, 6, 1, 8, 0, 0, 0).to_epoch_seconds();
        let max = DateTime::from_civil(2021, 6, 1, 14, 0, 0, 0).to_epoch_seconds();
        let (ticks, _spacing, unit) = calc_ticks(min, max, 6);
        assert_eq!(unit, DtUnit::Hours);
        assert!(ticks[0] <= min + 1e-6);
        assert!(*ticks.last().unwrap() >= max - 1e-6);
    }

    #[test]
    fn calc_ticks_degenerate_range_is_total() {
        // min == max: silx returns the microsecond fallback; must not loop.
        let t = DateTime::from_civil(2021, 1, 1, 0, 0, 0, 0).to_epoch_seconds();
        let (ticks, spacing, unit) = calc_ticks(t, t, 5);
        assert_eq!(unit, DtUnit::MicroSeconds);
        assert_eq!(spacing, 1.0);
        // include_first_beyond emits the single start tick.
        assert_eq!(ticks.len(), 1);
        assert!(close(ticks[0], t, 1e-6));
    }

    #[test]
    fn nice_num_generic_matches_default_fractions() {
        // With default-style fractions [1,2,5,10] and isRound=false, frac<=1 -> 1.
        let v = nice_num_generic(1.0, Some(&[1.0, 2.0, 5.0, 10.0]), false);
        assert!(close(v, 1.0, 1e-12), "v={v}");
        // 7 rounds up to 10 (frac 7 > 5).
        let v = nice_num_generic(7.0, Some(&[1.0, 2.0, 5.0, 10.0]), false);
        assert!(close(v, 10.0, 1e-12), "v={v}");
        // 3 with isRound: thresholds become [1.5, 3.5, 7.5, 10]; 3 <= 3.5 -> 2.
        let v = nice_num_generic(3.0, Some(&[1.0, 2.0, 5.0, 10.0]), true);
        assert!(close(v, 2.0, 1e-12), "v={v}");
    }

    #[test]
    fn nice_date_time_element_floors_years_to_one() {
        // A sub-1 nice year value is clamped up to 1.
        let v = nice_date_time_element(0.3, DtUnit::Years, true);
        assert_eq!(v, 1.0);
    }

    #[test]
    fn format_tick_day_unit_is_iso_date() {
        // Days unit, spacing >= 1 -> "%Y-%m-%d".
        let t = DateTime::from_civil(2021, 3, 9, 13, 5, 0, 0).to_epoch_seconds();
        assert_eq!(format_tick(t, 1.0, DtUnit::Days), "2021-03-09");
    }

    #[test]
    fn format_tick_hours_is_hh_mm() {
        let t = DateTime::from_civil(2021, 3, 9, 13, 5, 0, 0).to_epoch_seconds();
        assert_eq!(format_tick(t, 1.0, DtUnit::Hours), "13:05");
    }

    #[test]
    fn format_ticks_microseconds_strips_shared_trailing_zeros() {
        // Two ticks at .100000 and .200000 -> shared 5 trailing zeros stripped,
        // and the leading '0' of the seconds field dropped.
        let base = DateTime::from_civil(2021, 1, 1, 0, 0, 0, 100_000).to_epoch_seconds();
        let t2 = DateTime::from_civil(2021, 1, 1, 0, 0, 0, 200_000).to_epoch_seconds();
        let out = format_ticks(&[base, t2], 0.5, DtUnit::MicroSeconds);
        // Raw labels: "00.100000", "00.200000". Min trailing zeros = 5 (capped).
        // Leading '0' dropped -> start at index 1; trim 5 from the end.
        // "00.100000"[1..len-5] = "0.100000"[.. ] => "0.1" .. let's assert content.
        assert_eq!(out, vec!["0.1".to_string(), "0.2".to_string()]);
    }

    #[test]
    fn time_zone_offset_at_constant_zones_ignore_instant() {
        // The constant zones return the same offset regardless of the instant.
        assert_eq!(TimeZone::Utc.offset_at(0.0), 0);
        assert_eq!(TimeZone::Utc.offset_at(1_700_000_000.0), 0);
        let jst = TimeZone::FixedOffset {
            seconds_east: 32400,
        };
        assert_eq!(jst.offset_at(0.0), 32400);
        assert_eq!(jst.offset_at(1_700_000_000.0), 32400);
        let est = TimeZone::FixedOffset {
            seconds_east: -18000,
        };
        assert_eq!(est.offset_at(0.0), -18000);
    }

    #[test]
    fn named_zone_offset_is_dst_aware() {
        let ny = TimeZone::named("America/New_York").expect("America/New_York in bundled tz db");
        // Winter -> EST (UTC-05:00); summer -> EDT (UTC-04:00).
        let winter = DateTime::from_civil(2021, 1, 15, 12, 0, 0, 0).to_epoch_seconds();
        let summer = DateTime::from_civil(2021, 7, 15, 12, 0, 0, 0).to_epoch_seconds();
        assert_eq!(ny.offset_at(winter), -18000, "EST should be UTC-5");
        assert_eq!(ny.offset_at(summer), -14400, "EDT should be UTC-4");
        // An unknown zone name resolves to None (silx would raise).
        assert!(TimeZone::named("Not/AZone").is_none());
    }

    #[test]
    fn named_zone_decompose_and_round_trips_both_seasons() {
        let ny = TimeZone::named("America/New_York").unwrap();
        // 2021-01-15 12:00 UTC == 2021-01-15 07:00 EST.
        let winter_utc = DateTime::from_civil(2021, 1, 15, 12, 0, 0, 0).to_epoch_seconds();
        let d = DateTime::from_epoch_seconds_tz(winter_utc, ny);
        assert_eq!(
            (d.year, d.month, d.day, d.hour, d.minute, d.second),
            (2021, 1, 15, 7, 0, 0)
        );
        assert!(close(d.to_epoch_seconds_tz(ny), winter_utc, 1e-6));
        // 2021-07-15 12:00 UTC == 2021-07-15 08:00 EDT.
        let summer_utc = DateTime::from_civil(2021, 7, 15, 12, 0, 0, 0).to_epoch_seconds();
        let d = DateTime::from_epoch_seconds_tz(summer_utc, ny);
        assert_eq!(
            (d.year, d.month, d.day, d.hour, d.minute, d.second),
            (2021, 7, 15, 8, 0, 0)
        );
        assert!(close(d.to_epoch_seconds_tz(ny), summer_utc, 1e-6));
    }

    #[test]
    fn calc_ticks_tz_named_zone_handles_dst_transition() {
        // A six-day window spanning US spring-forward (2021-03-14 02:00 EST ->
        // 03:00 EDT). For n=5 ticks this picks Days unit with a 1-day spacing.
        let ny = TimeZone::named("America/New_York").unwrap();
        let min = DateTime::from_civil(2021, 3, 11, 0, 0, 0, 0).to_epoch_seconds_tz(ny);
        let max = DateTime::from_civil(2021, 3, 17, 0, 0, 0, 0).to_epoch_seconds_tz(ny);
        let (ticks, spacing, unit) = calc_ticks_tz(min, max, 5, ny);
        assert_eq!(unit, DtUnit::Days);
        assert_eq!(spacing, 1.0, "expected a 1-day spacing for this window");
        // Every daily tick is local New York midnight, even across the change.
        for &t in &ticks {
            let d = DateTime::from_epoch_seconds_tz(t, ny);
            assert_eq!(
                (d.hour, d.minute, d.second),
                (0, 0, 0),
                "tick {t} not NY midnight: {d:?}"
            );
        }
        // The spring-forward civil day is only 23 hours of real time: the gap
        // between consecutive local midnights straddling the change is 23h,
        // which exercises the DST-aware local->UTC inversion on each side
        // (03-14 midnight is EST -5, 03-15 midnight is EDT -4).
        let mar14 = DateTime::from_civil(2021, 3, 14, 0, 0, 0, 0).to_epoch_seconds_tz(ny);
        let mar15 = DateTime::from_civil(2021, 3, 15, 0, 0, 0, 0).to_epoch_seconds_tz(ny);
        assert!(
            close(mar15 - mar14, 23.0 * 3600.0, 1e-6),
            "spring-forward day should be 23h, got {}s",
            mar15 - mar14
        );
    }

    #[test]
    fn from_to_epoch_tz_applies_offset_and_round_trips() {
        // epoch 0 == 1970-01-01 00:00:00 UTC. In UTC+09:00 the wall clock reads
        // 09:00 the same day; in UTC-05:00 it reads 19:00 the previous day.
        let jst = TimeZone::FixedOffset {
            seconds_east: 32400,
        };
        let est = TimeZone::FixedOffset {
            seconds_east: -18000,
        };

        let d = DateTime::from_epoch_seconds_tz(0.0, jst);
        assert_eq!(
            (d.year, d.month, d.day, d.hour, d.minute, d.second),
            (1970, 1, 1, 9, 0, 0)
        );
        // Round-trip back to the original epoch.
        assert!(close(d.to_epoch_seconds_tz(jst), 0.0, 1e-6));

        let d = DateTime::from_epoch_seconds_tz(0.0, est);
        assert_eq!(
            (d.year, d.month, d.day, d.hour, d.minute, d.second),
            (1969, 12, 31, 19, 0, 0)
        );
        assert!(close(d.to_epoch_seconds_tz(est), 0.0, 1e-6));

        // UTC is the identity case (matches the non-tz helpers).
        let d = DateTime::from_epoch_seconds_tz(86_400.0, TimeZone::Utc);
        assert_eq!(d, DateTime::from_epoch_seconds(86_400.0));
        assert!(close(d.to_epoch_seconds_tz(TimeZone::Utc), 86_400.0, 1e-6));
    }

    #[test]
    fn format_tick_tz_renders_wall_clock_in_zone() {
        // 2021-03-09 13:05:00 UTC. Hours unit -> "%H:%M" of the zone wall clock.
        let epoch = DateTime::from_civil(2021, 3, 9, 13, 5, 0, 0).to_epoch_seconds();
        assert_eq!(
            format_tick_tz(epoch, 1.0, DtUnit::Hours, TimeZone::Utc),
            "13:05"
        );
        assert_eq!(
            format_tick_tz(
                epoch,
                1.0,
                DtUnit::Hours,
                TimeZone::FixedOffset {
                    seconds_east: 32400
                }
            ),
            "22:05"
        );
        // UTC-05:00 rolls back across midnight to the previous calendar day.
        assert_eq!(
            format_tick_tz(
                DateTime::from_civil(2021, 3, 9, 2, 5, 0, 0).to_epoch_seconds(),
                1.0,
                DtUnit::Hours,
                TimeZone::FixedOffset {
                    seconds_east: -18000
                }
            ),
            "21:05"
        );
    }

    #[test]
    fn calc_ticks_tz_utc_matches_legacy() {
        let min = DateTime::from_civil(2021, 1, 4, 0, 0, 0, 0).to_epoch_seconds();
        let max = DateTime::from_civil(2021, 1, 11, 0, 0, 0, 0).to_epoch_seconds();
        let (a_ticks, a_spacing, a_unit) = calc_ticks(min, max, 5);
        let (b_ticks, b_spacing, b_unit) = calc_ticks_tz(min, max, 5, TimeZone::Utc);
        assert_eq!(a_ticks, b_ticks);
        assert_eq!(a_spacing, b_spacing);
        assert_eq!(a_unit, b_unit);
    }

    #[test]
    fn calc_ticks_tz_daily_ticks_land_on_zone_midnight() {
        // A one-week window whose endpoints are local midnight in UTC+09:00.
        let jst = TimeZone::FixedOffset {
            seconds_east: 32400,
        };
        let min = DateTime::from_civil(2021, 1, 4, 0, 0, 0, 0).to_epoch_seconds_tz(jst);
        let max = DateTime::from_civil(2021, 1, 11, 0, 0, 0, 0).to_epoch_seconds_tz(jst);
        let (ticks, _spacing, unit) = calc_ticks_tz(min, max, 5, jst);
        assert_eq!(unit, DtUnit::Days);
        assert!(ticks.len() >= 2, "ticks={ticks:?}");
        // Every daily tick is exactly local midnight in the zone.
        for &t in &ticks {
            let d = DateTime::from_epoch_seconds_tz(t, jst);
            assert_eq!(
                (d.hour, d.minute, d.second),
                (0, 0, 0),
                "tick {t} not at zone midnight: {d:?}"
            );
        }
        // The first label is the zone-local date, and the ticks bracket [min,max].
        let labels = format_ticks_tz(&ticks, _spacing, unit, jst);
        assert_eq!(labels[0], "2021-01-04");
        assert!(ticks[0] <= min + 1e-6);
        assert!(*ticks.last().unwrap() >= max - 1e-6);
        // The offset really moved the ticks: under UTC the same epochs lay out at
        // a different set of positions (their wall clock is 15:00 the prior day).
        let (utc_ticks, _, _) = calc_ticks_tz(min, max, 5, TimeZone::Utc);
        assert_ne!(ticks, utc_ticks);
    }
}
