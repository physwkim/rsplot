//! Shared enum-selection logic for the enum-driven writable widgets
//! ([`RsdmEnumComboBox`], [`RsdmEnumButton`]).
//!
//! PyDM's `PyDMEnumComboBox` and `PyDMEnumButton` share the same semantics: the
//! choices come from the channel's enum strings (`enum_strings_changed`), the
//! current selection is derived from the value (`value_changed`), and selecting
//! one writes its integer index (`send_value`). One owner keeps the two widgets
//! from drifting apart.
//!
//! [`RsdmEnumComboBox`]: crate::widgets::RsdmEnumComboBox
//! [`RsdmEnumButton`]: crate::widgets::RsdmEnumButton

use crate::channel::{ChannelState, PvValue};

/// The choices for the channel: its enum strings, or empty when none are known
/// yet (PyDM `enum_strings_changed`).
pub fn enum_options(state: &ChannelState) -> Vec<String> {
    state
        .enum_strings
        .as_deref()
        .map(<[String]>::to_vec)
        .unwrap_or_default()
}

/// The index currently selected for `state` (PyDM `value_changed`): an
/// integer/enum/bool value is the index directly; a string is matched against
/// the enum strings (Qt `findText`). Any other value (or no value) selects
/// nothing.
pub fn enum_current_index(state: &ChannelState) -> Option<usize> {
    match &state.value {
        Some(PvValue::Int(n)) => usize::try_from(*n).ok(),
        Some(PvValue::Enum { index, .. }) => Some(*index as usize),
        Some(PvValue::Bool(b)) => Some(usize::from(*b)),
        Some(PvValue::Str(s)) => state
            .enum_strings
            .as_deref()
            .and_then(|items| items.iter().position(|item| item == s.as_ref())),
        _ => None,
    }
}

/// The value written when the user selects `index` (PyDM emits the integer
/// index).
pub fn enum_index_value(index: usize) -> PvValue {
    PvValue::Int(index as i64)
}
