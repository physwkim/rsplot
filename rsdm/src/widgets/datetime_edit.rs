//! `RsdmDateTimeEdit` — a writable date/time entry.
//!
//! Ports `pydm/widgets/datetime.py` (`PyDMDateTimeEdit`): the writable
//! counterpart of [`RsdmDateTimeLabel`](crate::widgets::RsdmDateTimeLabel). It
//! shows the channel's numeric time value as a date/time string (the same
//! conversion the label uses) and, on Enter, parses the typed string back into a
//! point in time and writes the channel value PyDM's `send_value` would emit:
//!
//! - `relative` (default): the milliseconds from "now" to the entered time
//!   (`now.msecsTo(val)`); otherwise the absolute milliseconds since the epoch
//!   (`val.toMSecsSinceEpoch()`),
//! - divided by 1000 when the [`TimeBase`] is `Seconds`,
//! - coerced to the channel's numeric type (PyDM `self.channeltype(new_value)`).
//!
//! `blockPastDate` (default on) refuses to send a time earlier than now (PyDM
//! logs an error and returns). Following the crate's single-owner model there is
//! no local echo: a committed value is written and the displayed text re-syncs
//! from the next monitor update; while the field has keyboard focus the buffer is
//! frozen so an incoming update does not overwrite typing.
//!
//! **Deviation:** PyDM's `QDateTimeEdit` is a calendar-popup / field-spin editor;
//! this port is a text entry of the `YYYY/MM/DD hh:mm:ss.zzz` string (the format
//! the label renders), parsed back on commit. The calendar popup and per-field
//! spin editing are Qt-widget affordances and are not ported. Only that default
//! format is accepted (matching the label's format deviation).

use rsplot::egui;

use crate::channel::{Channel, ChannelState, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::{BorderMode, ChannelBase, layout_justify};
use crate::widgets::datetime_label::{
    MS_PER_DAY, TimeBase, days_from_civil, format_datetime_ms, now_epoch_ms, value_epoch_ms,
};

/// Parse a `YYYY/MM/DD hh:mm:ss[.zzz]` UTC string to epoch milliseconds — the
/// inverse of [`format_datetime_ms`].
/// Returns `None` when the string is not in that form or carries an out-of-range
/// field. The fractional second is optional and is read as up to three digits
/// (`.5` → 500 ms, `.45` → 450 ms, `.456` → 456 ms).
pub fn parse_datetime_ms(text: &str) -> Option<i64> {
    let (date, time) = text.trim().split_once(' ')?;

    let mut date_parts = date.split('/');
    let year: i64 = date_parts.next()?.trim().parse().ok()?;
    let month: u32 = date_parts.next()?.trim().parse().ok()?;
    let day: u32 = date_parts.next()?.trim().parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let (hms, millis) = match time.trim().split_once('.') {
        Some((hms, frac)) => (hms, parse_millis(frac)?),
        None => (time.trim(), 0),
    };
    let mut time_parts = hms.split(':');
    let hour: i64 = time_parts.next()?.trim().parse().ok()?;
    let minute: i64 = time_parts.next()?.trim().parse().ok()?;
    let second: i64 = time_parts.next()?.trim().parse().ok()?;
    if time_parts.next().is_some()
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return None;
    }

    let days = days_from_civil(year, month, day);
    days.checked_mul(MS_PER_DAY)?
        .checked_add((hour * 3600 + minute * 60 + second) * 1000)?
        .checked_add(millis)
}

/// Read a fractional-second field as milliseconds: digits only, padded/truncated
/// to three places.
fn parse_millis(frac: &str) -> Option<i64> {
    let frac = frac.trim();
    if frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut digits = String::with_capacity(3);
    digits.extend(frac.chars().take(3));
    while digits.len() < 3 {
        digits.push('0');
    }
    digits.parse().ok()
}

/// The numeric value PyDM `send_value` would emit for the entered time
/// `edited_ms` (epoch ms) given the current time `now_ms`, or `None` when the
/// send is refused. With `block_past_date` an entered time earlier than now is
/// refused (PyDM logs an error and returns). The result is the milliseconds (or
/// seconds, for [`TimeBase::Seconds`]) from now (`relative`) or since the epoch
/// (absolute), *before* the channel-type coercion the widget applies.
pub fn send_value_epoch_ms(
    edited_ms: i64,
    now_ms: i64,
    relative: bool,
    time_base: TimeBase,
    block_past_date: bool,
) -> Option<f64> {
    if block_past_date && edited_ms < now_ms {
        return None;
    }
    let ms = if relative {
        edited_ms - now_ms
    } else {
        edited_ms
    };
    Some(match time_base {
        TimeBase::Milliseconds => ms as f64,
        TimeBase::Seconds => ms as f64 / 1000.0,
    })
}

/// Coerce the computed send value to the channel's numeric type (PyDM
/// `self.channeltype(new_value)`): an integer channel truncates toward zero,
/// every other (or unknown) channel writes a float.
fn coerce_send_value(value: f64, state: &ChannelState) -> PvValue {
    match &state.value {
        Some(PvValue::Int(_)) | Some(PvValue::Bool(_)) => PvValue::Int(value as i64),
        _ => PvValue::Float(value),
    }
}

/// A writable date/time entry bound to a numeric time channel (PyDM
/// `PyDMDateTimeEdit`).
pub struct RsdmDateTimeEdit {
    base: ChannelBase,
    time_base: TimeBase,
    relative: bool,
    block_past_date: bool,
    /// The text being edited. Frozen against incoming updates while focused.
    edit_buffer: String,
    /// Whether the field held keyboard focus at the end of the last frame.
    editing: bool,
}

impl RsdmDateTimeEdit {
    /// Connect `address` and wrap it in a writable date/time entry with PyDM's
    /// defaults (milliseconds, relative to now, past dates blocked).
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            base: ChannelBase::new(engine.connect(address)?),
            time_base: TimeBase::default(),
            relative: true,
            block_past_date: true,
            edit_buffer: String::new(),
            editing: false,
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

    /// Whether to refuse sending a time earlier than now (builder style; PyDM
    /// `blockPastDate`, default on).
    pub fn with_block_past_date(mut self, block_past_date: bool) -> Self {
        self.block_past_date = block_past_date;
        self
    }

    /// Choose which severities draw a border (builder style; `DisconnectedOnly`
    /// for converted MEDM screens).
    pub fn with_border_mode(mut self, mode: BorderMode) -> Self {
        self.base.border_mode = mode;
        self
    }

    /// The underlying channel.
    pub fn channel(&self) -> &Channel {
        self.base.channel()
    }

    /// The text the field shows for `state` given the current time `now_ms`: the
    /// formatted date/time, or empty when there is no numeric value (PyDM seeds
    /// from `currentDateTime` but only the channel value is shown once it
    /// arrives; before any value the field is blank in this port).
    pub fn display_text(&self, state: &ChannelState, now_ms: i64) -> String {
        match state.value.as_ref().and_then(PvValue::as_f64) {
            Some(raw) => {
                format_datetime_ms(value_epoch_ms(raw, self.time_base, self.relative, now_ms))
            }
            None => String::new(),
        }
    }

    /// Render the field this frame. Returns the value written this frame (on a
    /// successful Enter commit), or `None`. There is no local echo: the displayed
    /// text re-syncs from the channel's next update.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<PvValue> {
        let now_ms = now_epoch_ms();
        let state = self.base.channel().state();
        let display = self.display_text(&state, now_ms);
        // Keep the buffer in sync with the live value unless the user is editing.
        if !self.editing {
            self.edit_buffer = display.clone();
        }

        let inner = self.base.framed(ui, &state, true, |ui| {
            let mut edit = egui::TextEdit::singleline(&mut self.edit_buffer);
            if layout_justify(ui).1 {
                // Match the line edit's vertical centring inside a justified
                // (MEDM) cell: `TextEdit` top-aligns its single row, so pad the
                // vertical margin by half the slack.
                let font = ui
                    .style()
                    .override_font_id
                    .clone()
                    .unwrap_or_else(|| egui::TextStyle::Body.resolve(ui.style()));
                let row = ui.fonts_mut(|f| f.row_height(&font));
                let slack = ((ui.available_height() - row) / 2.0).clamp(0.0, 127.0);
                edit = edit.margin(egui::Margin::symmetric(4, slack.round() as i8));
            }
            ui.add(edit)
        });
        let resp = inner.inner;
        self.editing = resp.has_focus();

        let mut submitted = None;
        if resp.lost_focus() {
            let committed = ui.input(|i| i.key_pressed(egui::Key::Enter));
            if committed
                && let Some(edited_ms) = parse_datetime_ms(&self.edit_buffer)
                && let Some(value) = send_value_epoch_ms(
                    edited_ms,
                    now_ms,
                    self.relative,
                    self.time_base,
                    self.block_past_date,
                )
            {
                let pv = coerce_send_value(value, &state);
                self.base.channel().put(pv.clone());
                submitted = Some(pv);
            }
            // Whether committed, cancelled, refused, or a parse error: drop the
            // edit and resync to the live value (a commit shows via the monitor).
            self.edit_buffer = display;
            self.editing = false;
        }
        submitted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::format_datetime_ms;

    #[test]
    fn parse_inverts_format() {
        for ms in [0_i64, 1_609_459_200_000, 1_609_459_323_456, -1000] {
            assert_eq!(
                parse_datetime_ms(&format_datetime_ms(ms)),
                Some(ms),
                "ms={ms}"
            );
        }
    }

    #[test]
    fn parse_reads_known_string() {
        // 2021-01-01 01:02:03.456 UTC.
        let ms = 1_609_459_200_000 + (3600 + 2 * 60 + 3) * 1000 + 456;
        assert_eq!(parse_datetime_ms("2021/01/01 01:02:03.456"), Some(ms));
        // Fractional second optional → 0 ms.
        assert_eq!(
            parse_datetime_ms("2021/01/01 01:02:03"),
            Some(1_609_459_200_000 + (3600 + 2 * 60 + 3) * 1000)
        );
        // Short fraction pads to the right (`.5` → 500 ms).
        assert_eq!(
            parse_datetime_ms("2021/01/01 00:00:00.5"),
            Some(1_609_459_200_000 + 500)
        );
    }

    #[test]
    fn parse_rejects_malformed_or_out_of_range() {
        assert_eq!(parse_datetime_ms("not a date"), None);
        assert_eq!(parse_datetime_ms("2021-01-01 00:00:00"), None); // wrong separators
        assert_eq!(parse_datetime_ms("2021/13/01 00:00:00.000"), None); // month 13
        assert_eq!(parse_datetime_ms("2021/01/01 24:00:00.000"), None); // hour 24
        assert_eq!(parse_datetime_ms("2021/01/01 00:60:00.000"), None); // minute 60
        assert_eq!(parse_datetime_ms("2021/01/01 00:00:00.abc"), None); // non-digit frac
        assert_eq!(parse_datetime_ms("2021/01/01"), None); // no time
    }

    #[test]
    fn absolute_send_value_is_the_epoch_count() {
        // relative = false: send the absolute epoch ms (or seconds).
        assert_eq!(
            send_value_epoch_ms(1_609_459_200_000, 0, false, TimeBase::Milliseconds, false),
            Some(1_609_459_200_000.0)
        );
        assert_eq!(
            send_value_epoch_ms(1_609_459_200_000, 0, false, TimeBase::Seconds, false),
            Some(1_609_459_200.0)
        );
    }

    #[test]
    fn relative_send_value_is_the_offset_from_now() {
        let now = 1_000_000_000_000;
        // 5 s in the future.
        assert_eq!(
            send_value_epoch_ms(now + 5000, now, true, TimeBase::Milliseconds, true),
            Some(5000.0)
        );
        assert_eq!(
            send_value_epoch_ms(now + 5000, now, true, TimeBase::Seconds, true),
            Some(5.0)
        );
    }

    #[test]
    fn block_past_date_refuses_times_before_now() {
        let now = 1_000_000_000_000;
        // Past time, blocked → no send.
        assert_eq!(
            send_value_epoch_ms(now - 1, now, true, TimeBase::Milliseconds, true),
            None
        );
        // Past time, not blocked → a negative relative offset is sent.
        assert_eq!(
            send_value_epoch_ms(now - 1000, now, true, TimeBase::Milliseconds, false),
            Some(-1000.0)
        );
    }

    #[test]
    fn coerce_matches_channel_type() {
        let int_state = ChannelState {
            connected: true,
            value: Some(PvValue::Int(0)),
            ..ChannelState::default()
        };
        // An int channel truncates toward zero.
        assert_eq!(coerce_send_value(5.9, &int_state), PvValue::Int(5));
        let float_state = ChannelState {
            connected: true,
            value: Some(PvValue::Float(0.0)),
            ..ChannelState::default()
        };
        assert_eq!(coerce_send_value(5.9, &float_state), PvValue::Float(5.9));
    }

    #[test]
    fn commit_pipeline_writes_expected_value() {
        // Full commit path as pure functions: parse the displayed string, compute
        // the absolute-ms send value, coerce to the channel type.
        let float_state = ChannelState {
            connected: true,
            value: Some(PvValue::Float(0.0)),
            ..ChannelState::default()
        };
        let edited = parse_datetime_ms("2021/01/01 00:00:00.000").expect("parse");
        let value =
            send_value_epoch_ms(edited, 0, false, TimeBase::Milliseconds, false).expect("send");
        assert_eq!(
            coerce_send_value(value, &float_state),
            PvValue::Float(1_609_459_200_000.0)
        );
    }
}
