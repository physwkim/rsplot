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
pub mod datetime_label;
pub mod display_format;
pub mod drawing;
pub mod enum_button;
pub mod enum_choice;
pub mod enum_combo_box;
pub mod event_plot;
pub mod frame;
pub mod image_file;
pub mod image_view;
pub mod label;
pub mod line_edit;
pub(crate) mod plot_menu;
pub mod plot_style;
pub mod push_button;
pub mod ring_buffer;
pub mod scale_indicator;
pub mod scatter_plot;
pub mod slider;
pub mod spinbox;
pub mod symbol;
pub mod time_plot;
pub mod waveform_plot;

pub use base::{
    AlarmPalette, BorderMode, BorderStyle, ChannelBase, alarm_border, control_range,
    middle_click_copy, severity_color, severity_color_medm,
};
pub use byte::{Orientation, SidmByteIndicator, extract_bits};
pub use checkbox::SidmCheckbox;
pub use datetime_label::{SidmDateTimeLabel, TimeBase, format_datetime_ms, value_epoch_ms};
pub use display_format::{DisplayFormat, FormatSpec, format_value};
pub use drawing::{DrawingShape, SidmDrawing, effective_colors};
pub use enum_button::{EnumButtonType, SidmEnumButton, order_indices};
pub use enum_choice::{enum_current_index, enum_index_value, enum_options};
pub use enum_combo_box::SidmEnumComboBox;
pub use event_plot::{SidmEventPlot, event_sample};
pub use frame::SidmFrame;
pub use image_file::{SidmImage, decode_color_image};
pub use image_view::{ReadingOrder, SidmImageView, color_range, reshape_image, value_to_image};
pub use label::{SidmLabel, TextAlign};
pub use line_edit::{SidmLineEdit, parse_input};
pub use plot_style::{CurveStyle, DEFAULT_LINE_WIDTH};
pub use push_button::{DEFAULT_CONFIRM_MESSAGE, SidmPushButton, compute_send_value};
pub use ring_buffer::{DEFAULT_BUFFER_SIZE, MINIMUM_BUFFER_SIZE, TimeSeriesBuffer};
pub use scale_indicator::{
    DEFAULT_NUM_DIVISIONS, SidmScaleIndicator, division_proportions, value_proportion,
};
pub use scatter_plot::{DEFAULT_SYMBOL_SIZE, SidmScatterPlot};
pub use slider::{DEFAULT_NUM_STEPS, SidmSlider};
pub use spinbox::SidmSpinbox;
pub use symbol::{SidmSymbol, SymbolState, symbol_index_for_value, value_as_state_key};
pub use time_plot::{
    DEFAULT_TIME_SPAN, DEFAULT_UPDATE_RATE_HZ, SidmTimePlot, TimeAxisMode, UpdateMode, is_rate_due,
    update_interval,
};
pub use waveform_plot::{RedrawMode, SidmWaveformPlot, mode_allows, value_to_waveform};

// The siplot data-margin type accepted by every plot widget's
// `with_data_margins` (time / waveform / scatter / event), re-exported so callers
// configure plot padding without reaching into `siplot`.
pub use siplot::DataMargins;
