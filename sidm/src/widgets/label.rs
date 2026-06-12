//! `SidmLabel` — a read-only value display.
//!
//! Ports `pydm/widgets/label.py`: a label that shows its channel's value,
//! formatted via [`format_value`], with alarm-severity border/text styling from
//! [`ChannelBase`]. While the channel is disconnected it shows the channel
//! address instead of a stale value (PyDM `check_enable_state`).

use siplot::egui;

use crate::channel::{Channel, ChannelState};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::{AlarmPalette, ChannelBase, layout_justify};
use crate::widgets::display_format::{DisplayFormat, FormatSpec, format_value};

/// Horizontal alignment of the label text within its rect (MEDM `align` / PyDM
/// `alignment`). Vertical layout is unchanged; only the cross-axis position moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextAlign {
    /// Left-aligned (MEDM `horiz. left`, the default).
    #[default]
    Left,
    /// Horizontally centered (MEDM `horiz. centered`).
    Center,
    /// Right-aligned (MEDM `horiz. right`).
    Right,
}

/// A read-only channel value display (PyDM `PyDMLabel`).
pub struct SidmLabel {
    base: ChannelBase,
    /// How the value is rendered (PyDM `displayFormat`).
    pub format: DisplayFormat,
    /// Precision override; `None` uses the PV's `PREC` (PyDM `precisionFromPV`).
    pub precision: Option<i32>,
    /// Append the engineering units (PyDM `showUnits`).
    pub show_units: bool,
    /// Horizontal text alignment (MEDM `align`).
    pub alignment: TextAlign,
}

impl SidmLabel {
    /// Connect `address` through `engine` and wrap it in a label with PyDM's
    /// defaults (native format, PV precision, no units, alarm border on).
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            base: ChannelBase::new(engine.connect(address)?),
            format: DisplayFormat::Default,
            precision: None,
            show_units: false,
            alignment: TextAlign::Left,
        })
    }

    /// Set the display format (builder style).
    pub fn with_format(mut self, format: DisplayFormat) -> Self {
        self.format = format;
        self
    }

    /// Set the horizontal text alignment (builder style; MEDM `align`).
    pub fn with_alignment(mut self, alignment: TextAlign) -> Self {
        self.alignment = alignment;
        self
    }

    /// Set a precision override (builder style).
    pub fn with_precision(mut self, precision: i32) -> Self {
        self.precision = Some(precision);
        self
    }

    /// Show engineering units (builder style).
    pub fn with_show_units(mut self, show_units: bool) -> Self {
        self.show_units = show_units;
        self
    }

    /// Recolour the text by alarm severity (PyDM `alarmSensitiveContent`,
    /// builder style).
    pub fn with_alarm_sensitive_content(mut self, on: bool) -> Self {
        self.base.alarm_sensitive_content = on;
        self
    }

    /// Draw or suppress the alarm-severity border (PyDM `alarmSensitiveBorder`,
    /// builder style).
    pub fn with_alarm_sensitive_border(mut self, on: bool) -> Self {
        self.base.alarm_sensitive_border = on;
        self
    }

    /// Choose the alarm palette severity styling draws from (builder style;
    /// `Medm` for converted `clrmod="alarm"` widgets).
    pub fn with_alarm_palette(mut self, palette: AlarmPalette) -> Self {
        self.base.alarm_palette = palette;
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

    /// The text the label would show for `state`: the formatted value while
    /// connected, the channel address while disconnected (PyDM shows the address
    /// rather than a stale value when the connection drops).
    pub fn display_text(&self, state: &ChannelState) -> String {
        if state.connected {
            format_value(state.value.as_ref(), state, self.format_spec())
        } else {
            self.base.channel().address().raw().to_owned()
        }
    }

    /// Render the label this frame, returning the widget response (carrying the
    /// hover tooltip).
    pub fn show(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let state = self.base.channel().state();
        let text = self.display_text(&state);
        let color = self.base.content_color(&state);
        // Horizontal alignment is the cross axis of a top-down layout, so the
        // vertical placement is unchanged and `Left` (`Align::Min`) is the default
        // layout — only `Center`/`Right` move the text.
        let halign = match self.alignment {
            TextAlign::Left => egui::Align::Min,
            TextAlign::Center => egui::Align::Center,
            TextAlign::Right => egui::Align::Max,
        };
        self.base
            .framed(ui, &state, false, |ui| {
                let mut rich = egui::RichText::new(text);
                if let Some(color) = color {
                    rich = rich.color(color);
                }
                // The alignment `with_layout` replaces an inherited justified
                // layout, so the label face would hug its galley while the
                // screen's bclr backing fills the whole rect (MEDM text-update
                // geometry) — re-fill each justified axis explicitly.
                let justify = layout_justify(ui);
                ui.with_layout(egui::Layout::top_down(halign), |ui| {
                    if justify.0 {
                        ui.set_min_width(ui.available_width());
                    }
                    if justify.1 {
                        ui.set_min_height(ui.available_height());
                    }
                    ui.label(rich);
                });
            })
            .response
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::channel::PvValue;

    fn wait_for(mut cond: impl FnMut() -> bool, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        cond()
    }

    fn connected_state(value: PvValue) -> ChannelState {
        ChannelState {
            connected: true,
            value: Some(value),
            ..ChannelState::default()
        }
    }

    #[test]
    fn alignment_defaults_left_and_builder_sets_it() {
        let engine = Engine::new();
        let label = SidmLabel::new(&engine, "loc://label_align").expect("connect");
        assert_eq!(label.alignment, TextAlign::Left);
        let centered = SidmLabel::new(&engine, "loc://label_align2")
            .expect("connect")
            .with_alignment(TextAlign::Center);
        assert_eq!(centered.alignment, TextAlign::Center);
    }

    #[test]
    fn formats_value_with_precision_and_units() {
        let engine = Engine::new();
        let label = SidmLabel::new(&engine, "loc://label_fmt")
            .expect("connect")
            .with_precision(2)
            .with_show_units(true);
        let mut state = connected_state(PvValue::Float(1.5));
        state.units = Some(Arc::from("V"));
        assert_eq!(label.display_text(&state), "1.50 V");
    }

    #[test]
    fn disconnected_shows_channel_address() {
        let engine = Engine::new();
        let label = SidmLabel::new(&engine, "loc://label_disc").expect("connect");
        let state = ChannelState {
            connected: false,
            value: Some(PvValue::Float(9.0)),
            ..ChannelState::default()
        };
        // Even with a stale value present, a disconnected label shows the address.
        assert_eq!(label.display_text(&state), "loc://label_disc");
    }

    #[test]
    fn enum_value_renders_label() {
        let engine = Engine::new();
        let label = SidmLabel::new(&engine, "loc://label_enum").expect("connect");
        let mut state = connected_state(PvValue::Int(1));
        state.enum_strings = Some(["Off".to_owned(), "On".to_owned()].into());
        assert_eq!(label.display_text(&state), "On");
    }

    #[test]
    fn live_value_flows_from_a_write() {
        let engine = Engine::new();
        let label = SidmLabel::new(&engine, "loc://label_live").expect("connect");
        let writer = engine.connect("loc://label_live").expect("second handle");
        assert!(
            wait_for(|| label.channel().is_connected(), Duration::from_secs(2)),
            "loc label channel never connected"
        );
        writer.put(PvValue::Float(7.0));
        assert!(
            wait_for(
                || label.display_text(&label.channel().state()) == "7",
                Duration::from_secs(2)
            ),
            "label did not observe the written value (got {:?})",
            label.display_text(&label.channel().state())
        );
    }
}
