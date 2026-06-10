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
pub mod checkbox;
pub mod display_format;
pub mod enum_combo_box;
pub mod image_view;
pub mod label;
pub mod line_edit;
pub mod push_button;
pub mod ring_buffer;
pub mod scatter_plot;
pub mod slider;
pub mod spinbox;
pub mod time_plot;
pub mod waveform_plot;

pub use base::{BorderStyle, ChannelBase, alarm_border, control_range, severity_color};
pub use byte::{Orientation, PydmByteIndicator, extract_bits};
pub use checkbox::PydmCheckbox;
pub use display_format::{DisplayFormat, FormatSpec, format_value};
pub use enum_combo_box::PydmEnumComboBox;
pub use image_view::{PydmImageView, ReadingOrder, color_range, reshape_image, value_to_image};
pub use label::PydmLabel;
pub use line_edit::{PydmLineEdit, parse_input};
pub use push_button::{DEFAULT_CONFIRM_MESSAGE, PydmPushButton, compute_send_value};
pub use ring_buffer::{DEFAULT_BUFFER_SIZE, MINIMUM_BUFFER_SIZE, TimeSeriesBuffer};
pub use scatter_plot::{DEFAULT_SYMBOL_SIZE, PydmScatterPlot};
pub use slider::{DEFAULT_NUM_STEPS, PydmSlider};
pub use spinbox::PydmSpinbox;
pub use time_plot::{
    DEFAULT_TIME_SPAN, DEFAULT_UPDATE_RATE_HZ, PydmTimePlot, UpdateMode, is_rate_due,
    update_interval,
};
pub use waveform_plot::{PydmWaveformPlot, RedrawMode, mode_allows, value_to_waveform};
