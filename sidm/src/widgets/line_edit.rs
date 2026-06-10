//! `PydmLineEdit` — a writable text entry.
//!
//! Ports `pydm/widgets/line_edit.py`: a single-line text field that shows the
//! channel value (formatted like [`PydmLabel`]) and, on Enter, parses the typed
//! text back into a [`PvValue`] and writes it. The parse is keyed on the current
//! value's type (the PyDM `channeltype`) and the display format, mirroring
//! `send_value`:
//!
//! - numeric channels: `Hex`/`Binary` parse the digits in that radix,
//!   `Exponential`/`Decimal` parse a float, `Default`/`String` parse the native
//!   type; the result is coerced to the channel's type,
//! - bool channels: `strtobool` (`y/yes/t/true/on/1` ↔ `n/no/f/false/off/0`),
//!   enum channels: an index, or a label matched against the enum strings,
//!   string channels: the text verbatim,
//! - the units suffix the display added is stripped before parsing.
//!
//! Following the crate's single-owner model there is no local echo: a committed
//! value is written to the channel and the displayed text re-syncs from the
//! next monitor update, not from the edit buffer. While the field has keyboard
//! focus the buffer is frozen so an incoming update does not overwrite typing.

use std::sync::Arc;

use siplot::egui;

use crate::channel::{Channel, ChannelState, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::ChannelBase;
use crate::widgets::display_format::{DisplayFormat, FormatSpec, format_value};

/// A writable channel text entry (PyDM `PyDMLineEdit`).
pub struct PydmLineEdit {
    base: ChannelBase,
    /// How the value is rendered and how typed text is interpreted (PyDM
    /// `displayFormat`).
    pub format: DisplayFormat,
    /// Precision override for the displayed value; `None` uses the PV's `PREC`.
    pub precision: Option<i32>,
    /// Append/strip the engineering units (PyDM `showUnits`).
    pub show_units: bool,
    /// The text being edited. Frozen against incoming updates while focused.
    edit_buffer: String,
    /// Whether the field held keyboard focus at the end of the last frame.
    editing: bool,
}

impl PydmLineEdit {
    /// Connect `address` through `engine` and wrap it in a writable line edit
    /// with PyDM's defaults.
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            base: ChannelBase::new(engine.connect(address)?),
            format: DisplayFormat::Default,
            precision: None,
            show_units: false,
            edit_buffer: String::new(),
            editing: false,
        })
    }

    /// Set the display/parse format (builder style).
    pub fn with_format(mut self, format: DisplayFormat) -> Self {
        self.format = format;
        self
    }

    /// Set a precision override (builder style).
    pub fn with_precision(mut self, precision: i32) -> Self {
        self.precision = Some(precision);
        self
    }

    /// Show/strip engineering units (builder style).
    pub fn with_show_units(mut self, show_units: bool) -> Self {
        self.show_units = show_units;
        self
    }

    /// The underlying channel.
    pub fn channel(&self) -> &Channel {
        self.base.channel()
    }

    fn format_spec(&self) -> FormatSpec {
        FormatSpec {
            format: self.format,
            precision: self.precision,
            show_units: self.show_units,
        }
    }

    /// The text the field shows for `state`: the formatted value, or empty when
    /// no value has arrived. Unlike [`PydmLabel`], a line edit keeps showing the
    /// last value while disconnected (the field is merely disabled).
    pub fn current_text(&self, state: &ChannelState) -> String {
        format_value(state.value.as_ref(), state, self.format_spec())
    }

    /// Render the field this frame. Returns the value written this frame (on a
    /// successful Enter commit), or `None`. There is no local echo: the
    /// displayed text re-syncs from the channel's next update.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<PvValue> {
        let state = self.base.channel().state();
        let display = self.current_text(&state);
        // Keep the buffer in sync with the live value unless the user is editing.
        if !self.editing {
            self.edit_buffer = display.clone();
        }

        let inner = self.base.framed(ui, &state, true, |ui| {
            ui.add(egui::TextEdit::singleline(&mut self.edit_buffer))
        });
        let resp = inner.inner;
        self.editing = resp.has_focus();

        let mut submitted = None;
        if resp.lost_focus() {
            let committed = ui.input(|i| i.key_pressed(egui::Key::Enter));
            if committed
                && let Ok(value) = parse_input(&self.edit_buffer, &state, self.format_spec())
            {
                self.base.channel().put(value.clone());
                submitted = Some(value);
            }
            // Whether committed, cancelled, or a parse error: drop the edit and
            // resync to the live value (the commit shows up via the monitor).
            self.edit_buffer = display;
            self.editing = false;
        }
        submitted
    }
}

/// Parse typed `text` into a [`PvValue`] to write, given the channel `state`
/// (which fixes the target type and enum strings) and the format `spec` (which
/// fixes the radix / float interpretation and the units suffix to strip).
///
/// Errors carry a short human-readable reason; the widget drops the edit on
/// error (PyDM logs and does not send).
pub fn parse_input(text: &str, state: &ChannelState, spec: FormatSpec) -> Result<PvValue, String> {
    // Strip the units suffix the display added, then trim.
    let mut s = text.trim();
    if spec.show_units
        && let Some(unit) = state.units.as_deref().filter(|u| !u.is_empty())
        && let Some(stripped) = s.strip_suffix(unit)
    {
        s = stripped.trim_end();
    }
    let s = s.trim();
    if s.is_empty() {
        return Err("empty input".to_owned());
    }

    match &state.value {
        Some(PvValue::Str(_)) | None => Ok(PvValue::Str(Arc::from(s))),
        Some(PvValue::Bool(_)) => parse_bool(s).map(PvValue::Bool),
        Some(PvValue::Int(_)) => parse_int(s, spec.format).map(PvValue::Int),
        Some(PvValue::Float(_)) => parse_float(s, spec.format).map(PvValue::Float),
        Some(PvValue::Enum { .. }) => parse_enum(s, state),
        Some(PvValue::FloatArray(_)) => parse_num_list(s)?
            .parse_floats()
            .map(|v| PvValue::FloatArray(v.into())),
        Some(PvValue::IntArray(_)) => parse_int_list(s).map(|v| PvValue::IntArray(v.into())),
        Some(PvValue::Bytes(_)) => Ok(parse_bytes(s, spec.format)),
        Some(PvValue::StrArray(_)) => {
            Err("writing string-array channels from a line edit is not supported".to_owned())
        }
    }
}

/// Integer parse keyed on the display format (PyDM `send_value` int branch).
fn parse_int(s: &str, format: DisplayFormat) -> Result<i64, String> {
    match format {
        DisplayFormat::Hex => from_radix(s, 16),
        DisplayFormat::Binary => from_radix(s, 2),
        // PyDM: Exponential/Decimal parse a float then coerce to int.
        DisplayFormat::Exponential | DisplayFormat::Decimal => parse_f64(s).map(|f| f as i64),
        DisplayFormat::Default | DisplayFormat::String => s
            .parse::<i64>()
            .map_err(|_| format!("not an integer: {s:?}")),
    }
}

/// Float parse keyed on the display format (PyDM `send_value` float branch:
/// Hex/Binary read an int then widen, everything else parses a float).
fn parse_float(s: &str, format: DisplayFormat) -> Result<f64, String> {
    match format {
        DisplayFormat::Hex => from_radix(s, 16).map(|n| n as f64),
        DisplayFormat::Binary => from_radix(s, 2).map(|n| n as f64),
        _ => parse_f64(s),
    }
}

fn parse_f64(s: &str) -> Result<f64, String> {
    s.parse::<f64>().map_err(|_| format!("not a number: {s:?}"))
}

/// `int(s, radix)` with Python's tolerance for an optional `0x`/`0b` prefix and
/// a leading sign.
fn from_radix(s: &str, radix: u32) -> Result<i64, String> {
    let t = s.trim();
    let (neg, body) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let prefix = if radix == 16 {
        ["0x", "0X"]
    } else {
        ["0b", "0B"]
    };
    let body = body
        .strip_prefix(prefix[0])
        .or_else(|| body.strip_prefix(prefix[1]))
        .unwrap_or(body);
    i64::from_str_radix(body, radix)
        .map(|v| if neg { -v } else { v })
        .map_err(|_| format!("not a base-{radix} integer: {s:?}"))
}

/// Python `distutils.util.strtobool`.
fn parse_bool(s: &str) -> Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" | "t" | "true" | "on" | "1" => Ok(true),
        "n" | "no" | "f" | "false" | "off" | "0" => Ok(false),
        _ => Err(format!("not a boolean: {s:?}")),
    }
}

/// Enum write: a numeric index, or a label matched against the channel's enum
/// strings. Produces an `Int` index (the write path resolves it to the enum
/// field).
fn parse_enum(s: &str, state: &ChannelState) -> Result<PvValue, String> {
    if let Ok(index) = s.parse::<i64>() {
        return Ok(PvValue::Int(index));
    }
    if let Some(strings) = state.enum_strings.as_deref()
        && let Some(index) = strings.iter().position(|label| label == s)
    {
        return Ok(PvValue::Int(index as i64));
    }
    Err(format!("not an enum index or known label: {s:?}"))
}

/// Char-waveform write: `String` format sends the text as bytes; any other
/// format parses a numeric byte list (PyDM `ndarray` branch).
fn parse_bytes(s: &str, format: DisplayFormat) -> PvValue {
    if format == DisplayFormat::String {
        PvValue::Bytes(Arc::from(s.as_bytes()))
    } else {
        // Best effort: a numeric byte list; non-numeric tokens are dropped, as
        // PyDM filters empties.
        let bytes: Vec<u8> = strip_brackets(s)
            .split([' ', ',', '\t'])
            .filter_map(|t| t.trim().parse::<u8>().ok())
            .collect();
        PvValue::Bytes(Arc::from(bytes.as_slice()))
    }
}

/// A parsed list of numeric tokens, convertible to the array type the channel
/// needs.
struct NumList(Vec<String>);

impl NumList {
    fn parse_floats(&self) -> Result<Vec<f64>, String> {
        self.0
            .iter()
            .map(|t| t.parse::<f64>().map_err(|_| format!("not a number: {t:?}")))
            .collect()
    }
}

/// Split a bracketed/space/comma-separated list into tokens (PyDM's
/// `[1.2 3.4]` array entry format).
fn parse_num_list(s: &str) -> Result<NumList, String> {
    let tokens: Vec<String> = strip_brackets(s)
        .split([' ', ',', '\t'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect();
    if tokens.is_empty() {
        return Err("empty array".to_owned());
    }
    Ok(NumList(tokens))
}

fn parse_int_list(s: &str) -> Result<Vec<i64>, String> {
    parse_num_list(s)?
        .0
        .iter()
        .map(|t| {
            t.parse::<i64>()
                .map_err(|_| format!("not an integer: {t:?}"))
        })
        .collect()
}

fn strip_brackets(s: &str) -> String {
    s.replace(['[', ']'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_of(value: PvValue) -> ChannelState {
        ChannelState {
            connected: true,
            value: Some(value),
            ..ChannelState::default()
        }
    }

    fn spec(format: DisplayFormat, show_units: bool) -> FormatSpec {
        FormatSpec {
            format,
            precision: None,
            show_units,
        }
    }

    #[test]
    fn float_channel_parses_decimal() {
        let st = state_of(PvValue::Float(0.0));
        assert_eq!(
            parse_input("3.25", &st, spec(DisplayFormat::Default, false)),
            Ok(PvValue::Float(3.25))
        );
    }

    #[test]
    fn int_channel_default_rejects_fractional() {
        let st = state_of(PvValue::Int(0));
        assert!(parse_input("5.5", &st, spec(DisplayFormat::Default, false)).is_err());
        assert_eq!(
            parse_input("5", &st, spec(DisplayFormat::Default, false)),
            Ok(PvValue::Int(5))
        );
    }

    #[test]
    fn int_channel_decimal_format_truncates_float() {
        let st = state_of(PvValue::Int(0));
        // Decimal format on an int channel parses a float then coerces to int.
        assert_eq!(
            parse_input("5.9", &st, spec(DisplayFormat::Decimal, false)),
            Ok(PvValue::Int(5))
        );
    }

    #[test]
    fn hex_format_parses_with_and_without_prefix() {
        let st = state_of(PvValue::Int(0));
        assert_eq!(
            parse_input("0x1a", &st, spec(DisplayFormat::Hex, false)),
            Ok(PvValue::Int(26))
        );
        assert_eq!(
            parse_input("1a", &st, spec(DisplayFormat::Hex, false)),
            Ok(PvValue::Int(26))
        );
        assert_eq!(
            parse_input("-0x5", &st, spec(DisplayFormat::Hex, false)),
            Ok(PvValue::Int(-5))
        );
    }

    #[test]
    fn binary_format_parses() {
        let st = state_of(PvValue::Int(0));
        assert_eq!(
            parse_input("0b101", &st, spec(DisplayFormat::Binary, false)),
            Ok(PvValue::Int(5))
        );
    }

    #[test]
    fn float_channel_hex_format_widens_to_float() {
        let st = state_of(PvValue::Float(0.0));
        assert_eq!(
            parse_input("ff", &st, spec(DisplayFormat::Hex, false)),
            Ok(PvValue::Float(255.0))
        );
    }

    #[test]
    fn units_suffix_is_stripped_before_parsing() {
        let mut st = state_of(PvValue::Float(0.0));
        st.units = Some(Arc::from("mm"));
        assert_eq!(
            parse_input("3.5 mm", &st, spec(DisplayFormat::Default, true)),
            Ok(PvValue::Float(3.5))
        );
        // No-space form is stripped too.
        assert_eq!(
            parse_input("3.5mm", &st, spec(DisplayFormat::Default, true)),
            Ok(PvValue::Float(3.5))
        );
    }

    #[test]
    fn bool_channel_accepts_strtobool_spellings() {
        let st = state_of(PvValue::Bool(false));
        for t in ["on", "TRUE", "Yes", "1", "t"] {
            assert_eq!(
                parse_input(t, &st, spec(DisplayFormat::Default, false)),
                Ok(PvValue::Bool(true)),
                "{t:?} should be true"
            );
        }
        for f in ["off", "False", "no", "0", "f"] {
            assert_eq!(
                parse_input(f, &st, spec(DisplayFormat::Default, false)),
                Ok(PvValue::Bool(false)),
                "{f:?} should be false"
            );
        }
        assert!(parse_input("maybe", &st, spec(DisplayFormat::Default, false)).is_err());
    }

    #[test]
    fn enum_channel_accepts_index_or_label() {
        let mut st = state_of(PvValue::Enum {
            index: 0,
            label: None,
        });
        st.enum_strings = Some(["Off".to_owned(), "On".to_owned()].into());
        assert_eq!(
            parse_input("1", &st, spec(DisplayFormat::Default, false)),
            Ok(PvValue::Int(1))
        );
        assert_eq!(
            parse_input("On", &st, spec(DisplayFormat::Default, false)),
            Ok(PvValue::Int(1))
        );
        assert!(parse_input("Nope", &st, spec(DisplayFormat::Default, false)).is_err());
    }

    #[test]
    fn string_channel_passes_text_through() {
        let st = state_of(PvValue::Str(Arc::from("old")));
        assert_eq!(
            parse_input("hello world", &st, spec(DisplayFormat::Default, false)),
            Ok(PvValue::Str(Arc::from("hello world")))
        );
    }

    #[test]
    fn float_array_round_trips_bracketed_list() {
        let st = state_of(PvValue::FloatArray(Arc::from([0.0].as_slice())));
        assert_eq!(
            parse_input("[1, 2.5, 3]", &st, spec(DisplayFormat::Default, false)),
            Ok(PvValue::FloatArray(Arc::from([1.0, 2.5, 3.0].as_slice())))
        );
    }

    #[test]
    fn char_waveform_string_format_sends_bytes() {
        let st = state_of(PvValue::Bytes(Arc::from(b"x".as_slice())));
        assert_eq!(
            parse_input("hi", &st, spec(DisplayFormat::String, false)),
            Ok(PvValue::Bytes(Arc::from(b"hi".as_slice())))
        );
    }

    #[test]
    fn empty_input_is_an_error() {
        let st = state_of(PvValue::Float(0.0));
        assert!(parse_input("   ", &st, spec(DisplayFormat::Default, false)).is_err());
    }

    #[test]
    fn no_value_yet_falls_back_to_string() {
        let st = ChannelState {
            connected: true,
            value: None,
            ..ChannelState::default()
        };
        assert_eq!(
            parse_input("anything", &st, spec(DisplayFormat::Default, false)),
            Ok(PvValue::Str(Arc::from("anything")))
        );
    }
}
