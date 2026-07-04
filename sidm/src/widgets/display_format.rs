//! Pure value → display-string formatter.
//!
//! Ports the three places PyDM turns a channel value into the text a label or
//! line-edit shows:
//!
//! - `pydm/widgets/display_format.py` `parse_value_for_display` — the
//!   [`DisplayFormat`] dispatch (string / decimal / exponential / hex / binary),
//! - `pydm/widgets/base.py` `TextFormatter.update_format_string` — fixed-point
//!   precision and the ` {unit}` suffix,
//! - `pydm/widgets/label.py` `value_changed` — enum index → label (or
//!   `**INVALID**`).
//!
//! This module is the pure core: no egui, no channel handle, just
//! `(value, metadata, options) -> String`, so it is exhaustively unit-tested
//! headlessly. The widgets that call it land in later commits.
//!
//! ## Deliberate deviations from PyDM
//!
//! - **No value → empty string, no unit suffix.** PyDM's `value_changed`
//!   appends the unit even to the empty string a `None` value formats to,
//!   yielding a stray ` V`. Here a value-less channel renders as `""`; showing
//!   the address while disconnected is the widget's job, not the formatter's.
//! - **Negative enum index → `**INVALID**`.** PyDM indexes `enum_strings` with
//!   a raw Python index, so a negative value silently wraps to a label from the
//!   end of the list. An out-of-range index (negative or `>= len`) is reported
//!   as invalid here.

use crate::channel::{ChannelState, PvValue};

/// How a channel value is rendered to text. Mirrors PyDM's `DisplayFormat`
/// (`pydm/widgets/display_format.py`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DisplayFormat {
    /// Native rendering: numbers as fixed-point (PV precision), enums as labels.
    #[default]
    Default,
    /// Decode a `CHAR` waveform as a UTF-8 string (stopping at the first NUL);
    /// scalars render exactly as [`DisplayFormat::Default`].
    String,
    /// Behaves identically to [`DisplayFormat::Default`]; PyDM keeps it as a
    /// separate menu entry.
    Decimal,
    /// Scientific notation, e.g. `1.50e+02`.
    Exponential,
    /// Hexadecimal of `floor(value)`, e.g. `0x1a` (or `-0x5`).
    Hex,
    /// Binary of `floor(value)`, e.g. `0b101` (or `-0b101`).
    Binary,
}

/// Per-widget formatting options layered on top of the channel metadata.
#[derive(Clone, Copy, Debug, Default)]
pub struct FormatSpec {
    /// Representation to use.
    pub format: DisplayFormat,
    /// Precision override. `None` falls back to the PV's `PREC`, then `0`
    /// (PyDM `precisionFromPV`).
    pub precision: Option<i32>,
    /// Append ` {unit}` when the PV reports non-empty engineering units (PyDM
    /// `showUnits`).
    pub show_units: bool,
}

/// Render `value` to a display string the way PyDM's `parse_value_for_display`
/// followed by `label.value_changed` would.
///
/// `None` yields an empty string (see the module-level deviation note).
pub fn format_value(value: Option<&PvValue>, state: &ChannelState, spec: FormatSpec) -> String {
    format_value_impl(value, state, spec, EnumSubstitution::Label)
}

/// Like [`format_value`] but WITHOUT enum-label substitution — the display path
/// for [`SidmLineEdit`](crate::widgets::SidmLineEdit).
///
/// PyDM's `PyDMLineEdit.set_display` (`line_edit.py:294-311`) runs only
/// `parse_value_for_display` (whose Default path returns the value unchanged)
/// then `format_string.format(...)`; unlike `PyDMLabel.value_changed`
/// (`label.py:137-141`) it has **no** `enum_strings` branch. So an enum channel's
/// integer index displays as its number (`"1"`, precision-formatted), not its
/// label, and `send_value` parses the field back with `int(...)`
/// ([`parse_enum`](super::line_edit) also accepts the numeric index, so the
/// display/write pair round-trips).
pub fn format_value_for_edit(
    value: Option<&PvValue>,
    state: &ChannelState,
    spec: FormatSpec,
) -> String {
    format_value_impl(value, state, spec, EnumSubstitution::Numeric)
}

/// Whether an integer-like value with known `enum_strings` renders as its label
/// (PyDM label widgets) or its numeric index (PyDM `PyDMLineEdit`). The one place
/// the two toolkits' widget methods diverge on enum display.
#[derive(Clone, Copy, PartialEq)]
enum EnumSubstitution {
    /// Substitute the enum label (PyDM `PyDMLabel.value_changed`).
    Label,
    /// Keep the numeric index (PyDM `PyDMLineEdit.set_display`).
    Numeric,
}

fn format_value_impl(
    value: Option<&PvValue>,
    state: &ChannelState,
    spec: FormatSpec,
    enums: EnumSubstitution,
) -> String {
    let Some(value) = value else {
        return String::new();
    };
    // precision: override → PV PREC → 0, never negative.
    let precision = spec.precision.or(state.precision).unwrap_or(0).max(0) as usize;
    let units = if spec.show_units {
        state.units.as_deref().filter(|u| !u.is_empty())
    } else {
        None
    };

    match spec.format {
        DisplayFormat::String => match value {
            // Only a byte waveform is decoded; a scalar under String renders as
            // Default (PyDM byte-decodes ndarrays only), and so do other arrays.
            PvValue::Bytes(bytes) => append_units(decode_char_waveform(bytes), units),
            _ => format_default(value, state, precision, units, enums),
        },
        DisplayFormat::Exponential => match value.as_f64() {
            Some(n) => append_units(python_exponential(n, precision), units),
            None => format_default(value, state, precision, units, enums),
        },
        DisplayFormat::Hex => match numeric_floor_i64(value) {
            Some(n) => append_units(format_hex(n), units),
            None => format_default(value, state, precision, units, enums),
        },
        DisplayFormat::Binary => match numeric_floor_i64(value) {
            Some(n) => append_units(format_binary(n), units),
            None => format_default(value, state, precision, units, enums),
        },
        DisplayFormat::Default | DisplayFormat::Decimal => {
            format_default(value, state, precision, units, enums)
        }
    }
}

/// The Default/Decimal rendering, also the scalar fallback for String and the
/// non-numeric fallback for Exponential/Hex/Binary.
fn format_default(
    value: &PvValue,
    state: &ChannelState,
    precision: usize,
    units: Option<&str>,
    enums: EnumSubstitution,
) -> String {
    // Enum substitution: an integer-like value with known enum strings renders
    // as its label, and the unit suffix is NOT appended (PyDM's enum branch
    // returns before the unit code). A float is never enum-substituted, matching
    // Python's `isinstance(value, int)` gate. Skipped for the line edit
    // (`EnumSubstitution::Numeric`), which shows the numeric index like
    // `PyDMLineEdit.set_display`.
    if enums == EnumSubstitution::Label
        && let Some(strings) = state.enum_strings.as_deref()
        && let Some(index) = enum_index(value)
    {
        return enum_label_or_invalid(strings, index);
    }
    match value {
        PvValue::Str(s) => append_units(s.to_string(), units),
        PvValue::Float(f) => append_units(format!("{f:.precision$}"), units),
        PvValue::Int(n) => append_units(format_int_fixed(*n, precision), units),
        PvValue::Bool(b) => append_units(format_int_fixed(i64::from(*b), precision), units),
        PvValue::Enum { index, .. } => {
            append_units(format_int_fixed(i64::from(*index), precision), units)
        }
        PvValue::FloatArray(_)
        | PvValue::IntArray(_)
        | PvValue::StrArray(_)
        | PvValue::Bytes(_) => append_units(format_array(value), units),
    }
}

/// Fixed-point of an integer without an `f64` round-trip, so even values beyond
/// `2^53` stay exact: PyDM's `"{:.Nf}".format(int)` keeps integer precision.
fn format_int_fixed(n: i64, precision: usize) -> String {
    if precision == 0 {
        format!("{n}")
    } else {
        let zeros = "0".repeat(precision);
        format!("{n}.{zeros}")
    }
}

/// `floor(value)` as an integer for hex/binary. Distinct from
/// [`PvValue::as_i64`], which truncates toward zero; PyDM uses
/// `int(math.floor(value))`, so `-1.5` floors to `-2`, not `-1`.
fn numeric_floor_i64(value: &PvValue) -> Option<i64> {
    match value {
        PvValue::Float(f) => Some(f.floor() as i64),
        PvValue::Int(n) => Some(*n),
        PvValue::Bool(b) => Some(i64::from(*b)),
        PvValue::Enum { index, .. } => Some(i64::from(*index)),
        _ => None,
    }
}

/// Index of an integer-like value for enum-label lookup. A float yields `None`
/// (Python's `isinstance(value, int)` is false for floats).
fn enum_index(value: &PvValue) -> Option<i64> {
    match value {
        PvValue::Int(n) => Some(*n),
        PvValue::Bool(b) => Some(i64::from(*b)),
        PvValue::Enum { index, .. } => Some(i64::from(*index)),
        _ => None,
    }
}

/// `enum_strings[index]`, or `**INVALID**` when the index is out of range
/// (PyDM `label.py`).
fn enum_label_or_invalid(strings: &[String], index: i64) -> String {
    usize::try_from(index)
        .ok()
        .and_then(|i| strings.get(i))
        .cloned()
        .unwrap_or_else(|| "**INVALID**".to_owned())
}

/// Decode a byte waveform as UTF-8 up to the first NUL (EPICS `CHAR` string).
fn decode_char_waveform(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Hexadecimal with a Python-`hex()` shape: `0x1a`, `-0x5`.
fn format_hex(n: i64) -> String {
    if n < 0 {
        let m = n.unsigned_abs();
        format!("-0x{m:x}")
    } else {
        format!("0x{n:x}")
    }
}

/// Binary with a Python-`bin()` shape: `0b101`, `-0b101`.
fn format_binary(n: i64) -> String {
    if n < 0 {
        let m = n.unsigned_abs();
        format!("-0b{m:b}")
    } else {
        format!("0b{n:b}")
    }
}

/// Scientific notation matching Python's `"{:.<prec>e}"`: a signed,
/// zero-padded-to-two-digits exponent (`1.50e+02`, `1.5e-03`, `0.00e+00`),
/// which Rust's native `{:e}` does not produce on its own.
fn python_exponential(value: f64, precision: usize) -> String {
    let raw = format!("{value:.precision$e}");
    match raw.split_once('e') {
        Some((mantissa, exp)) => {
            let exp: i64 = exp.parse().unwrap_or(0);
            let sign = if exp < 0 { '-' } else { '+' };
            let mag = exp.unsigned_abs();
            format!("{mantissa}e{sign}{mag:02}")
        }
        None => raw,
    }
}

/// `str(ndarray)`-style bracketed list for the rare array-in-a-label case.
fn format_array(value: &PvValue) -> String {
    let inner = match value {
        PvValue::FloatArray(a) => a.iter().map(f64::to_string).collect::<Vec<_>>().join(", "),
        PvValue::IntArray(a) => a.iter().map(i64::to_string).collect::<Vec<_>>().join(", "),
        PvValue::StrArray(a) => a.join(", "),
        PvValue::Bytes(a) => a.iter().map(u8::to_string).collect::<Vec<_>>().join(", "),
        // Scalars never reach here.
        _ => String::new(),
    };
    format!("[{inner}]")
}

/// Append ` {unit}` when units are present.
fn append_units(text: String, units: Option<&str>) -> String {
    match units {
        Some(u) => format!("{text} {u}"),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// Build a [`ChannelState`] carrying only the metadata the formatter reads.
    fn state(
        precision: Option<i32>,
        units: Option<&str>,
        enum_strings: Option<&[&str]>,
    ) -> ChannelState {
        ChannelState {
            precision,
            units: units.map(Arc::from),
            enum_strings: enum_strings
                .map(|s| s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>().into()),
            ..ChannelState::default()
        }
    }

    fn spec(format: DisplayFormat, precision: Option<i32>, show_units: bool) -> FormatSpec {
        FormatSpec {
            format,
            precision,
            show_units,
        }
    }

    fn fmt(value: PvValue, st: &ChannelState, sp: FormatSpec) -> String {
        format_value(Some(&value), st, sp)
    }

    // --- precision ---------------------------------------------------------

    #[test]
    fn none_value_is_empty_with_units() {
        let st = state(Some(2), Some("V"), None);
        assert_eq!(
            format_value(None, &st, spec(DisplayFormat::Default, None, true)),
            ""
        );
    }

    #[test]
    fn float_uses_pv_precision() {
        let st = state(Some(3), None, None);
        assert_eq!(
            fmt(
                PvValue::Float(1.23456),
                &st,
                spec(DisplayFormat::Default, None, false)
            ),
            "1.235"
        );
    }

    #[test]
    fn precision_override_beats_pv_precision() {
        let st = state(Some(3), None, None);
        assert_eq!(
            fmt(
                PvValue::Float(1.23456),
                &st,
                spec(DisplayFormat::Default, Some(1), false)
            ),
            "1.2"
        );
    }

    #[test]
    fn precision_defaults_to_zero() {
        let st = state(None, None, None);
        assert_eq!(
            fmt(
                PvValue::Float(1.9),
                &st,
                spec(DisplayFormat::Default, None, false)
            ),
            "2"
        );
    }

    #[test]
    fn negative_precision_clamps_to_zero() {
        let st = state(Some(-4), None, None);
        assert_eq!(
            fmt(
                PvValue::Float(1.9),
                &st,
                spec(DisplayFormat::Default, None, false)
            ),
            "2"
        );
    }

    #[test]
    fn int_zero_precision_is_exact() {
        let st = state(Some(0), None, None);
        assert_eq!(
            fmt(
                PvValue::Int(42),
                &st,
                spec(DisplayFormat::Default, None, false)
            ),
            "42"
        );
    }

    #[test]
    fn int_with_precision_is_fixed_point_and_exact_beyond_f64() {
        let st = state(None, None, None);
        // 2^53 + 1 — not representable in f64; the integer path must keep it.
        assert_eq!(
            fmt(
                PvValue::Int(9_007_199_254_740_993),
                &st,
                spec(DisplayFormat::Default, Some(2), false)
            ),
            "9007199254740993.00"
        );
    }

    // --- units -------------------------------------------------------------

    #[test]
    fn units_appended_when_requested() {
        let st = state(Some(1), Some("mm"), None);
        assert_eq!(
            fmt(
                PvValue::Float(3.0),
                &st,
                spec(DisplayFormat::Default, None, true)
            ),
            "3.0 mm"
        );
    }

    #[test]
    fn units_omitted_when_flag_off() {
        let st = state(Some(1), Some("mm"), None);
        assert_eq!(
            fmt(
                PvValue::Float(3.0),
                &st,
                spec(DisplayFormat::Default, None, false)
            ),
            "3.0"
        );
    }

    #[test]
    fn empty_units_string_not_appended() {
        let st = state(Some(1), Some(""), None);
        assert_eq!(
            fmt(
                PvValue::Float(3.0),
                &st,
                spec(DisplayFormat::Default, None, true)
            ),
            "3.0"
        );
    }

    // --- string scalars & char waveforms -----------------------------------

    #[test]
    fn string_scalar_passthrough_with_units() {
        let st = state(None, Some("x"), None);
        assert_eq!(
            fmt(
                PvValue::Str(Arc::from("hello")),
                &st,
                spec(DisplayFormat::Default, None, true)
            ),
            "hello x"
        );
    }

    #[test]
    fn string_format_decodes_char_waveform_stops_at_nul() {
        let st = state(None, None, None);
        let bytes = PvValue::Bytes(Arc::from(b"abc\0def".as_slice()));
        assert_eq!(
            format_value(Some(&bytes), &st, spec(DisplayFormat::String, None, false)),
            "abc"
        );
    }

    #[test]
    fn string_format_char_waveform_no_nul_uses_whole_buffer() {
        let st = state(None, None, None);
        let bytes = PvValue::Bytes(Arc::from(b"abc".as_slice()));
        assert_eq!(
            format_value(Some(&bytes), &st, spec(DisplayFormat::String, None, false)),
            "abc"
        );
    }

    #[test]
    fn string_format_on_scalar_renders_like_default() {
        let st = state(Some(2), None, None);
        assert_eq!(
            fmt(
                PvValue::Float(1.5),
                &st,
                spec(DisplayFormat::String, None, false)
            ),
            "1.50"
        );
    }

    // --- exponential -------------------------------------------------------

    #[test]
    fn exponential_positive_exponent() {
        let st = state(None, None, None);
        assert_eq!(
            fmt(
                PvValue::Float(150.0),
                &st,
                spec(DisplayFormat::Exponential, Some(2), false)
            ),
            "1.50e+02"
        );
    }

    #[test]
    fn exponential_negative_exponent() {
        let st = state(None, None, None);
        assert_eq!(
            fmt(
                PvValue::Float(0.0015),
                &st,
                spec(DisplayFormat::Exponential, Some(1), false)
            ),
            "1.5e-03"
        );
    }

    #[test]
    fn exponential_zero() {
        let st = state(None, None, None);
        assert_eq!(
            fmt(
                PvValue::Float(0.0),
                &st,
                spec(DisplayFormat::Exponential, Some(2), false)
            ),
            "0.00e+00"
        );
    }

    #[test]
    fn exponential_negative_value_with_units() {
        let st = state(None, Some("A"), None);
        assert_eq!(
            fmt(
                PvValue::Float(-150.0),
                &st,
                spec(DisplayFormat::Exponential, Some(2), true)
            ),
            "-1.50e+02 A"
        );
    }

    #[test]
    fn exponential_on_string_falls_back_to_default() {
        let st = state(None, None, None);
        assert_eq!(
            fmt(
                PvValue::Str(Arc::from("oops")),
                &st,
                spec(DisplayFormat::Exponential, Some(2), false)
            ),
            "oops"
        );
    }

    // --- hex / binary ------------------------------------------------------

    #[test]
    fn hex_positive() {
        let st = state(None, None, None);
        assert_eq!(
            fmt(PvValue::Int(26), &st, spec(DisplayFormat::Hex, None, false)),
            "0x1a"
        );
    }

    #[test]
    fn hex_negative() {
        let st = state(None, None, None);
        assert_eq!(
            fmt(PvValue::Int(-5), &st, spec(DisplayFormat::Hex, None, false)),
            "-0x5"
        );
    }

    #[test]
    fn hex_floors_float_toward_negative_infinity() {
        let st = state(None, None, None);
        // floor(-1.5) = -2, not the -1 a truncating cast would give.
        assert_eq!(
            fmt(
                PvValue::Float(-1.5),
                &st,
                spec(DisplayFormat::Hex, None, false)
            ),
            "-0x2"
        );
    }

    #[test]
    fn binary_positive() {
        let st = state(None, None, None);
        assert_eq!(
            fmt(
                PvValue::Int(5),
                &st,
                spec(DisplayFormat::Binary, None, false)
            ),
            "0b101"
        );
    }

    #[test]
    fn binary_negative() {
        let st = state(None, None, None);
        assert_eq!(
            fmt(
                PvValue::Int(-5),
                &st,
                spec(DisplayFormat::Binary, None, false)
            ),
            "-0b101"
        );
    }

    #[test]
    fn hex_appends_units() {
        let st = state(None, Some("cnt"), None);
        assert_eq!(
            fmt(PvValue::Int(255), &st, spec(DisplayFormat::Hex, None, true)),
            "0xff cnt"
        );
    }

    // --- enums -------------------------------------------------------------

    #[test]
    fn enum_index_resolves_to_label_without_units() {
        let st = state(None, Some("V"), Some(&["Off", "On"]));
        // Even with show_units, the enum branch does not append the unit.
        assert_eq!(
            fmt(
                PvValue::Int(1),
                &st,
                spec(DisplayFormat::Default, None, true)
            ),
            "On"
        );
    }

    #[test]
    fn enum_typed_value_resolves_to_label() {
        let st = state(None, None, Some(&["Off", "On", "Trip"]));
        assert_eq!(
            fmt(
                PvValue::Enum {
                    index: 2,
                    label: None
                },
                &st,
                spec(DisplayFormat::Default, None, false)
            ),
            "Trip"
        );
    }

    #[test]
    fn enum_index_out_of_range_is_invalid() {
        let st = state(None, None, Some(&["Off", "On"]));
        assert_eq!(
            fmt(
                PvValue::Int(7),
                &st,
                spec(DisplayFormat::Default, None, false)
            ),
            "**INVALID**"
        );
    }

    #[test]
    fn enum_negative_index_is_invalid() {
        let st = state(None, None, Some(&["Off", "On"]));
        assert_eq!(
            fmt(
                PvValue::Int(-1),
                &st,
                spec(DisplayFormat::Default, None, false)
            ),
            "**INVALID**"
        );
    }

    #[test]
    fn bool_with_enum_strings_resolves_label() {
        let st = state(None, None, Some(&["Off", "On"]));
        assert_eq!(
            fmt(
                PvValue::Bool(true),
                &st,
                spec(DisplayFormat::Default, None, false)
            ),
            "On"
        );
    }

    #[test]
    fn edit_variant_shows_enum_index_not_label() {
        // R2-60: PyDMLineEdit.set_display has no enum_strings branch, so the line
        // edit shows the numeric index (precision-formatted) where the label
        // shows "On". The write path (parse_enum) accepts that index, so the pair
        // round-trips.
        let st = state(None, None, Some(&["Off", "On"]));
        // Label path substitutes.
        assert_eq!(
            fmt(
                PvValue::Enum {
                    index: 1,
                    label: None
                },
                &st,
                spec(DisplayFormat::Default, None, false)
            ),
            "On"
        );
        // Line-edit path keeps the index.
        assert_eq!(
            format_value_for_edit(
                Some(&PvValue::Enum {
                    index: 1,
                    label: None
                }),
                &st,
                spec(DisplayFormat::Default, None, false)
            ),
            "1"
        );
        // Int and Bool enum channels likewise show the number, with units and
        // precision still applied (unlike the label branch, which returns early).
        assert_eq!(
            format_value_for_edit(
                Some(&PvValue::Int(1)),
                &state(Some(2), Some("V"), Some(&["Off", "On"])),
                spec(DisplayFormat::Default, None, true)
            ),
            "1.00 V"
        );
    }

    #[test]
    fn float_with_enum_strings_is_not_substituted() {
        // A float is not integer-like, so it formats numerically even when enum
        // strings are present (Python `isinstance(float, int)` is false).
        let st = state(Some(1), None, Some(&["Off", "On"]));
        assert_eq!(
            fmt(
                PvValue::Float(1.0),
                &st,
                spec(DisplayFormat::Default, None, false)
            ),
            "1.0"
        );
    }

    #[test]
    fn enum_under_hex_shows_index_not_label() {
        // Hex converts the index to a string before the label branch can run, so
        // the enum index is shown in hex and the unit suffix applies.
        let st = state(None, Some("st"), Some(&["Off", "On"]));
        assert_eq!(
            fmt(PvValue::Int(1), &st, spec(DisplayFormat::Hex, None, true)),
            "0x1 st"
        );
    }

    #[test]
    fn enum_value_without_strings_formats_index() {
        // Enum value but enum strings not yet received → format the index.
        let st = state(None, None, None);
        assert_eq!(
            fmt(
                PvValue::Enum {
                    index: 3,
                    label: None
                },
                &st,
                spec(DisplayFormat::Default, None, false)
            ),
            "3"
        );
    }

    // --- misc --------------------------------------------------------------

    #[test]
    fn bool_default_formats_as_one_or_zero() {
        let st = state(None, None, None);
        assert_eq!(
            fmt(
                PvValue::Bool(true),
                &st,
                spec(DisplayFormat::Default, None, false)
            ),
            "1"
        );
        assert_eq!(
            fmt(
                PvValue::Bool(false),
                &st,
                spec(DisplayFormat::Default, None, false)
            ),
            "0"
        );
    }

    #[test]
    fn float_array_renders_bracketed() {
        let st = state(None, None, None);
        let arr = PvValue::FloatArray(Arc::from([1.0, 2.5, 3.0].as_slice()));
        assert_eq!(
            format_value(Some(&arr), &st, spec(DisplayFormat::Default, None, false)),
            "[1, 2.5, 3]"
        );
    }
}
