//! PyDM-style channel-driven widgets layered on `siplot`.
//!
//! Each widget reads its [`Channel`]'s [`ChannelState`] snapshot every frame and
//! draws with alarm-severity styling, connection gating, and precision/unit
//! formatting (PyDM's `widgets/` package). The pure, headlessly-testable cores
//! land first; the egui-drawing widget structs build on them in later commits.
//!
//! [`Channel`]: crate::Channel
//! [`ChannelState`]: crate::ChannelState

pub mod base;
pub mod byte;
pub mod display_format;
pub mod label;
pub mod line_edit;

pub use base::{BorderStyle, ChannelBase, alarm_border, severity_color};
pub use byte::{Orientation, PydmByteIndicator, extract_bits};
pub use display_format::{DisplayFormat, FormatSpec, format_value};
pub use label::PydmLabel;
pub use line_edit::{PydmLineEdit, parse_input};
