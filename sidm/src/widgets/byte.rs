//! `SidmByteIndicator` — per-bit LED display of an integer value.
//!
//! Ports `pydm/widgets/byte.py`: shifts the value, takes `num_bits` bits
//! LSB-first, and draws each as an on/off coloured square (or circle), laid out
//! horizontally or vertically with optional per-bit labels.
//!
//! The bit extraction and the per-bit colour are pure (`extract_bits`,
//! [`SidmByteIndicator::bit_color`]); the egui drawing is exercised by a
//! headless wgpu readback test. The on/off/disconnected/invalid colours are the
//! byte widget's own (`0,255,0` / `100,100,100` / `255,255,255` / `255,0,255`),
//! not the alarm-border palette. PyDM's blink mode is not ported.

use siplot::egui::{self, Color32};

use crate::channel::{AlarmSeverity, Channel, ChannelState, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::{AlarmPalette, ChannelBase, layout_justify};

/// Layout direction for the row/column of bit indicators.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Orientation {
    /// Bits stacked top-to-bottom.
    #[default]
    Vertical,
    /// Bits laid left-to-right.
    Horizontal,
}

/// Extract `num_bits` bits of `value`, LSB first, after applying `shift`
/// (PyDM `update_indicators`): a negative shift is a left shift by its
/// magnitude, a non-negative shift is an arithmetic right shift.
pub fn extract_bits(value: i64, shift: i32, num_bits: usize) -> Vec<bool> {
    let shifted = if shift < 0 {
        value.wrapping_shl(shift.unsigned_abs())
    } else {
        value.wrapping_shr(shift as u32)
    };
    (0..num_bits)
        .map(|i| {
            if i >= 64 {
                // Beyond the 64-bit width, replicate the sign bit (Python's
                // arbitrary-precision `>>` does the same for two's complement).
                shifted < 0
            } else {
                (shifted >> i) & 1 == 1
            }
        })
        .collect()
}

const ON_COLOR: Color32 = Color32::from_rgb(0, 255, 0);
const OFF_COLOR: Color32 = Color32::from_rgb(100, 100, 100);
const DISCONNECTED_COLOR: Color32 = Color32::WHITE;
const INVALID_COLOR: Color32 = Color32::from_rgb(255, 0, 255);

/// LED grid of an integer's bits (PyDM `PyDMByteIndicator`).
pub struct SidmByteIndicator {
    base: ChannelBase,
    /// Number of bits to display (PyDM `numBits`).
    pub num_bits: usize,
    /// Bit shift applied before extraction (PyDM `shift`).
    pub shift: i32,
    /// Layout direction (PyDM `orientation`).
    pub orientation: Orientation,
    /// Draw circles rather than squares (PyDM `circles`).
    pub circles: bool,
    /// Most-significant bit first in the display order (PyDM `bigEndian`).
    pub big_endian: bool,
    /// Show the per-bit labels (PyDM `showLabels`).
    pub show_labels: bool,
    /// Per-bit label text; a bit with no entry shows its index.
    pub labels: Vec<String>,
    /// Colour of a set bit (PyDM `onColor`).
    pub on_color: Color32,
    /// Colour of a clear bit (PyDM `offColor`).
    pub off_color: Color32,
    /// Colour of every bit while disconnected (PyDM `disconnectedColor`).
    pub disconnected_color: Color32,
    /// Colour of every bit while the alarm is `INVALID` (PyDM `invalidColor`).
    pub invalid_color: Color32,
}

impl SidmByteIndicator {
    /// Connect `address` and wrap it in a byte indicator with PyDM's defaults
    /// (1 bit, no shift, vertical, square, little-endian, labels on, the byte
    /// palette).
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            base: ChannelBase::new(engine.connect(address)?),
            num_bits: 1,
            shift: 0,
            orientation: Orientation::Vertical,
            circles: false,
            big_endian: false,
            show_labels: true,
            labels: Vec::new(),
            on_color: ON_COLOR,
            off_color: OFF_COLOR,
            disconnected_color: DISCONNECTED_COLOR,
            invalid_color: INVALID_COLOR,
        })
    }

    /// Set the bit count (builder style).
    pub fn with_num_bits(mut self, num_bits: usize) -> Self {
        self.num_bits = num_bits;
        self
    }

    /// Set the bit shift (builder style).
    pub fn with_shift(mut self, shift: i32) -> Self {
        self.shift = shift;
        self
    }

    /// Set the layout direction (builder style).
    pub fn with_orientation(mut self, orientation: Orientation) -> Self {
        self.orientation = orientation;
        self
    }

    /// Show the most-significant bit first in the display order (builder style;
    /// PyDM `bigEndian`).
    pub fn with_big_endian(mut self, big_endian: bool) -> Self {
        self.big_endian = big_endian;
        self
    }

    /// Draw circles rather than squares (builder style).
    pub fn with_circles(mut self, circles: bool) -> Self {
        self.circles = circles;
        self
    }

    /// Show or hide the per-bit labels (builder style).
    pub fn with_show_labels(mut self, show_labels: bool) -> Self {
        self.show_labels = show_labels;
        self
    }

    /// Set the per-bit labels (builder style).
    pub fn with_labels(mut self, labels: Vec<String>) -> Self {
        self.labels = labels;
        self
    }

    /// Set the colour drawn for an on bit (builder style; PyDM `onColor`).
    pub fn with_on_color(mut self, on_color: Color32) -> Self {
        self.on_color = on_color;
        self
    }

    /// Set the colour drawn for an off bit (builder style; PyDM `offColor`).
    pub fn with_off_color(mut self, off_color: Color32) -> Self {
        self.off_color = off_color;
        self
    }

    /// Recolour lit bits by alarm severity (PyDM `alarmSensitiveContent`, builder
    /// style). When on, an on bit follows the channel severity (falling back to
    /// [`Self::with_on_color`] for `NoAlarm`); off bits keep their colour.
    pub fn with_alarm_sensitive_content(mut self, on: bool) -> Self {
        self.base.alarm_sensitive_content = on;
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

    /// The bits to display for `state` (PyDM treats the value as an integer;
    /// arrays/strings and a missing value extract as 0).
    pub fn bits(&self, state: &ChannelState) -> Vec<bool> {
        let value = state.value.as_ref().and_then(PvValue::as_i64).unwrap_or(0);
        extract_bits(value, self.shift, self.num_bits)
    }

    /// Colour for one bit given the channel state (PyDM `update_indicators`):
    /// disconnected → disconnected colour, `INVALID` alarm → invalid colour,
    /// otherwise on/off colour. When alarm-sensitive content is on, a lit bit is
    /// recoloured by the channel severity (MEDM `clrmod="alarm"`) through the
    /// base's palette, falling back to the static on colour when the palette
    /// has no override.
    pub fn bit_color(&self, state: &ChannelState, bit_on: bool) -> Color32 {
        if !state.connected {
            self.disconnected_color
        } else if state.severity == AlarmSeverity::Invalid {
            self.invalid_color
        } else if bit_on {
            self.base.content_color(state).unwrap_or(self.on_color)
        } else {
            self.off_color
        }
    }

    fn label_for(&self, bit_index: usize) -> String {
        self.labels
            .get(bit_index)
            .cloned()
            .unwrap_or_else(|| bit_index.to_string())
    }

    /// Render the indicator this frame, returning the widget response.
    pub fn show(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let state = self.base.channel().state();
        let bits = self.bits(&state);
        // Display order: big-endian shows the most-significant bit first.
        let order: Vec<usize> = if self.big_endian {
            (0..bits.len()).rev().collect()
        } else {
            (0..bits.len()).collect()
        };

        self.base
            .framed(ui, &state, false, |ui| {
                // `ui.vertical`/`ui.horizontal` reset the layout, so capture
                // the caller's justify intent first. A justified byte divides
                // the available extent evenly among its bits along the
                // stacking axis (MEDM/PyDM segment the widget rect) and fills
                // it across; labels are not reserved for in the division.
                let (justify_h, justify_v) = layout_justify(ui);
                let n = order.len() as f32;
                match self.orientation {
                    Orientation::Vertical => {
                        ui.vertical(|ui| {
                            let gaps = ui.spacing().item_spacing.y * (n - 1.0);
                            let bit = egui::vec2(
                                if justify_h {
                                    ui.available_width()
                                } else {
                                    INDICATOR_SIZE
                                },
                                if justify_v {
                                    ((ui.available_height() - gaps) / n).max(0.0)
                                } else {
                                    INDICATOR_SIZE
                                },
                            );
                            for &i in &order {
                                ui.horizontal(|ui| self.draw_bit(ui, &state, i, bits[i], bit));
                            }
                        });
                    }
                    Orientation::Horizontal => {
                        ui.horizontal(|ui| {
                            let gaps = ui.spacing().item_spacing.x * (n - 1.0);
                            let bit = egui::vec2(
                                if justify_h {
                                    ((ui.available_width() - gaps) / n).max(0.0)
                                } else {
                                    INDICATOR_SIZE
                                },
                                if justify_v {
                                    ui.available_height()
                                } else {
                                    INDICATOR_SIZE
                                },
                            );
                            for &i in &order {
                                ui.vertical(|ui| self.draw_bit(ui, &state, i, bits[i], bit));
                            }
                        });
                    }
                }
            })
            .response
    }

    /// Draw a single bit at `size`: the coloured indicator and, optionally, its
    /// label.
    fn draw_bit(
        &self,
        ui: &mut egui::Ui,
        state: &ChannelState,
        bit_index: usize,
        bit_on: bool,
        size: egui::Vec2,
    ) {
        let color = self.bit_color(state, bit_on);
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let painter = ui.painter();
        let outline = egui::Stroke::new(1.0, Color32::from_gray(60));
        if self.circles {
            let radius = rect.width().min(rect.height()) / 2.0 - 1.0;
            painter.circle(rect.center(), radius, color, outline);
        } else {
            painter.rect(
                rect,
                egui::CornerRadius::ZERO,
                color,
                outline,
                egui::StrokeKind::Inside,
            );
        }
        if self.show_labels {
            ui.label(self.label_for(bit_index));
        }
    }
}

/// Side length (points) of one bit indicator.
const INDICATOR_SIZE: f32 = 14.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::base::severity_color;

    #[test]
    fn extract_bits_lsb_first_no_shift() {
        // 0b0101 = 5 → bit0=1, bit1=0, bit2=1, bit3=0.
        assert_eq!(extract_bits(0b0101, 0, 4), vec![true, false, true, false]);
    }

    #[test]
    fn extract_bits_right_shift_drops_low_bits() {
        // 0b1011_0000 >> 4 = 0b1011 → bits 1,1,0,1.
        assert_eq!(
            extract_bits(0b1011_0000, 4, 4),
            vec![true, true, false, true]
        );
    }

    #[test]
    fn extract_bits_negative_shift_is_left_shift() {
        // value 1, shift -2 → 1 << 2 = 0b100 → bit0=0, bit1=0, bit2=1.
        assert_eq!(extract_bits(1, -2, 4), vec![false, false, true, false]);
    }

    #[test]
    fn extract_bits_masks_to_num_bits() {
        // Only the requested low bits are returned.
        assert_eq!(extract_bits(0xFF, 0, 3), vec![true, true, true]);
        assert_eq!(extract_bits(0b110, 0, 2), vec![false, true]);
    }

    fn state(connected: bool, severity: AlarmSeverity) -> ChannelState {
        ChannelState {
            connected,
            severity,
            ..ChannelState::default()
        }
    }

    fn indicator() -> SidmByteIndicator {
        let engine = Engine::new();
        SidmByteIndicator::new(&engine, "loc://byte_test").expect("connect")
    }

    #[test]
    fn bit_color_on_off_when_connected_no_alarm() {
        let b = indicator();
        let s = state(true, AlarmSeverity::NoAlarm);
        assert_eq!(b.bit_color(&s, true), ON_COLOR);
        assert_eq!(b.bit_color(&s, false), OFF_COLOR);
    }

    #[test]
    fn bit_color_invalid_overrides_on_off() {
        let b = indicator();
        let s = state(true, AlarmSeverity::Invalid);
        // Both set and clear bits show the invalid colour.
        assert_eq!(b.bit_color(&s, true), INVALID_COLOR);
        assert_eq!(b.bit_color(&s, false), INVALID_COLOR);
    }

    #[test]
    fn bit_color_disconnected_overrides_everything() {
        let b = indicator();
        // Disconnected wins even if the last wire severity was INVALID.
        let s = state(false, AlarmSeverity::Invalid);
        assert_eq!(b.bit_color(&s, true), DISCONNECTED_COLOR);
        assert_eq!(b.bit_color(&s, false), DISCONNECTED_COLOR);
    }

    #[test]
    fn alarm_sensitive_content_recolours_lit_bits_by_severity() {
        let b = indicator().with_alarm_sensitive_content(true);
        // A lit bit follows the channel severity; an unlit bit keeps its colour.
        let minor = state(true, AlarmSeverity::Minor);
        assert_eq!(
            b.bit_color(&minor, true),
            severity_color(AlarmSeverity::Minor).unwrap()
        );
        assert_eq!(b.bit_color(&minor, false), OFF_COLOR);
        // NoAlarm has no severity colour, so a lit bit falls back to the on colour.
        let ok = state(true, AlarmSeverity::NoAlarm);
        assert_eq!(b.bit_color(&ok, true), ON_COLOR);
        // Without alarm sensitivity, a Minor alarm leaves the lit bit at on colour.
        let plain = indicator();
        assert_eq!(plain.bit_color(&minor, true), ON_COLOR);
    }

    #[test]
    fn bits_reads_integer_value_with_shift_and_width() {
        let b = indicator().with_num_bits(4).with_shift(1);
        let s = ChannelState {
            connected: true,
            value: Some(PvValue::Int(0b1010)),
            ..ChannelState::default()
        };
        // 0b1010 >> 1 = 0b101 → bits 1,0,1,0.
        assert_eq!(b.bits(&s), vec![true, false, true, false]);
    }

    #[test]
    fn label_defaults_to_bit_index() {
        let b = indicator();
        assert_eq!(b.label_for(2), "2");
        let b = b.with_labels(vec!["A".to_owned(), "B".to_owned()]);
        assert_eq!(b.label_for(0), "A");
        assert_eq!(b.label_for(1), "B");
        // Past the supplied labels, falls back to the index.
        assert_eq!(b.label_for(2), "2");
    }
}
