//! PyDM-style channel-driven widgets layered on `rsplot`.
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
pub mod datetime_edit;
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
pub mod multi_state;
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
pub mod waveform_table;

pub use base::{
    AlarmPalette, BorderMode, BorderStyle, ChannelBase, UserLimits, alarm_border, control_range,
    middle_click_copy, severity_color, severity_color_medm,
};
pub use byte::{Orientation, RsdmByteIndicator, extract_bits};
pub use checkbox::RsdmCheckbox;
pub use datetime_edit::{RsdmDateTimeEdit, parse_datetime_ms, send_value_epoch_ms};
pub use datetime_label::{RsdmDateTimeLabel, TimeBase, format_datetime_ms, value_epoch_ms};
pub use display_format::{DisplayFormat, FormatSpec, format_value};
pub use drawing::{DrawingShape, RsdmDrawing, effective_colors};
pub use enum_button::{EnumButtonType, RsdmEnumButton, order_indices};
pub use enum_choice::{enum_current_index, enum_index_value, enum_options};
pub use enum_combo_box::RsdmEnumComboBox;
pub use event_plot::{RsdmEventPlot, event_sample};
pub use frame::RsdmFrame;
pub use image_file::{RsdmImage, decode_color_image};
pub use image_view::{ReadingOrder, RsdmImageView, color_range, reshape_image, value_to_image};
pub use label::{RsdmLabel, TextAlign};
pub use line_edit::{RsdmLineEdit, parse_input};
pub use multi_state::{NUM_STATES, RsdmMultiStateIndicator, state_for_value};
pub use plot_style::{CurveStyle, DEFAULT_LINE_WIDTH};
pub use push_button::{DEFAULT_CONFIRM_MESSAGE, RsdmPushButton, compute_send_value};
pub use ring_buffer::{DEFAULT_BUFFER_SIZE, MINIMUM_BUFFER_SIZE, TimeSeriesBuffer};
pub use scale_indicator::{
    DEFAULT_NUM_DIVISIONS, RsdmScaleIndicator, division_proportions, value_proportion,
};
pub use scatter_plot::{DEFAULT_SYMBOL_SIZE, RsdmScatterPlot};
pub use slider::{DEFAULT_NUM_STEPS, RsdmSlider};
pub use spinbox::RsdmSpinbox;
pub use symbol::{RsdmSymbol, SymbolState, symbol_index_for_value, value_as_state_key};
pub use time_plot::{
    DEFAULT_TIME_SPAN, DEFAULT_UPDATE_RATE_HZ, RsdmTimePlot, TimeAxisMode, UpdateMode, is_rate_due,
    update_interval,
};
pub use waveform_plot::{RedrawMode, RsdmWaveformPlot, mode_allows, value_to_waveform};
pub use waveform_table::{
    DEFAULT_COLUMN_COUNT, RsdmWaveformTable, apply_cell_edit, cell_index, row_count,
};

// The rsplot data-margin type accepted by every plot widget's
// `with_data_margins` (time / waveform / scatter / event), re-exported so callers
// configure plot padding without reaching into `rsplot`.
pub use rsplot::DataMargins;
