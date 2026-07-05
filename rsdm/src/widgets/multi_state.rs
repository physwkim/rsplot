//! `RsdmMultiStateIndicator` — a 16-state coloured status light.
//!
//! Ports `pydm/widgets/byte.py` (`PyDMMultiStateIndicator`): the connected
//! channel selects one of 16 states (0–15), each painted as a configurable fill
//! colour inside a filled circle (default) or rectangle, both with PyDM's red
//! border. PyDM's `value_changed` accepts the value only when it is numeric and
//! `0 <= new_val <= 15` (a string raises the comparison and is ignored), takes
//! `int(new_val)` (truncation toward zero) as the state, and otherwise leaves the
//! current state — and so the current colour — unchanged. That selection rule is
//! the pure, unit-tested core ([`state_for_value`]); the drawing is verified by a
//! headless wgpu readback.
//!
//! The 16 state colours default to opaque black (PyDM `[QColor(Qt.black)] * 16`);
//! configure the ones in use with [`RsdmMultiStateIndicator::with_state_color`].
//! Before any in-range value arrives the indicator shows black, matching PyDM's
//! `_curr_color = QColor(Qt.black)` initialisation (independent of state 0's
//! colour, which only takes effect once a value of 0 is received).

use rsplot::egui::{self, Color32, Vec2};

use crate::channel::{Channel, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::{ChannelBase, justified_size, layout_justify};

/// Number of selectable states (PyDM uses states 0–15).
pub const NUM_STATES: usize = 16;

/// Default indicator size in points (PyDM leaves Qt's default; this is a usable
/// status-light size).
const DEFAULT_SIZE: Vec2 = Vec2::new(24.0, 24.0);

/// The state index a channel value selects, or `None` when the value leaves the
/// state unchanged. Mirrors PyDM `value_changed`: the value must be numeric and
/// satisfy `0 <= new_val <= 15` (a non-numeric value such as a string fails the
/// comparison in PyDM and is ignored), and the state is `int(new_val)`
/// (truncation toward zero). An out-of-range, non-finite, or non-numeric value
/// yields `None`.
pub fn state_for_value(value: &PvValue) -> Option<usize> {
    let v = value.as_f64()?;
    if v.is_finite() && (0.0..=15.0).contains(&v) {
        // `as f64 -> as usize` truncates toward zero, matching Python `int()`;
        // the range check above guarantees 0..=15.
        Some(v.trunc() as usize)
    } else {
        None
    }
}

/// A 16-state coloured status light driven by a channel (PyDM
/// `PyDMMultiStateIndicator`).
pub struct RsdmMultiStateIndicator {
    base: ChannelBase,
    /// Fill colour for each of the 16 states (PyDM `_state_colors`).
    state_colors: [Color32; NUM_STATES],
    /// Draw a filled rectangle rather than a circle (PyDM `renderAsRectangle`).
    render_as_rectangle: bool,
    /// Indicator size in points.
    size: Vec2,
    /// Last in-range state (PyDM `_curr_state`, retained across out-of-range
    /// updates). `None` until the first in-range value, where PyDM shows black.
    curr_state: Option<usize>,
}

impl RsdmMultiStateIndicator {
    /// Connect `address` and wrap it as a multi-state indicator with PyDM's
    /// defaults (16 black states, circle render).
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            base: ChannelBase::new(engine.connect(address)?),
            state_colors: [Color32::BLACK; NUM_STATES],
            render_as_rectangle: false,
            size: DEFAULT_SIZE,
            curr_state: None,
        })
    }

    /// Set the fill colour for one state (builder style; PyDM `stateNColor`).
    /// An index outside `0..16` is ignored.
    pub fn with_state_color(mut self, index: usize, color: Color32) -> Self {
        if let Some(slot) = self.state_colors.get_mut(index) {
            *slot = color;
        }
        self
    }

    /// Set all 16 state colours at once (builder style).
    pub fn with_state_colors(mut self, colors: [Color32; NUM_STATES]) -> Self {
        self.state_colors = colors;
        self
    }

    /// Draw a filled rectangle rather than a circle (builder style; PyDM
    /// `renderAsRectangle`).
    pub fn with_render_as_rectangle(mut self, render_as_rectangle: bool) -> Self {
        self.render_as_rectangle = render_as_rectangle;
        self
    }

    /// Set the indicator size in points (builder style).
    pub fn with_size(mut self, size: Vec2) -> Self {
        self.size = size;
        self
    }

    /// The underlying channel.
    pub fn channel(&self) -> &Channel {
        self.base.channel()
    }

    /// The fill colour for the current state: the configured colour of the last
    /// in-range value, or black before any in-range value has arrived (PyDM
    /// `_curr_color`).
    fn current_color(&self) -> Color32 {
        self.curr_state
            .map_or(Color32::BLACK, |s| self.state_colors[s])
    }

    /// Render the indicator for the current value this frame.
    pub fn show(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let state = self.base.channel().state();
        // PyDM updates `_curr_state` only when the value is in range; an
        // out-of-range or non-numeric value keeps the last state and colour.
        if let Some(s) = state.value.as_ref().and_then(state_for_value) {
            self.curr_state = Some(s);
        }
        let color = self.current_color();

        self.base
            .framed(ui, &state, false, |ui| {
                let size = justified_size(layout_justify(ui), ui, self.size);
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                if ui.is_rect_visible(rect) {
                    // PyDM paints a red 1 px border around the state-coloured fill.
                    let border = egui::Stroke::new(1.0, Color32::RED);
                    if self.render_as_rectangle {
                        ui.painter().rect(
                            rect,
                            egui::CornerRadius::ZERO,
                            color,
                            border,
                            egui::StrokeKind::Inside,
                        );
                    } else {
                        // PyDM circle: r = min(w,h)/2 - 2*max(pen_width,1), pen 1 px.
                        let radius = (rect.width().min(rect.height()) / 2.0 - 2.0).max(0.0);
                        ui.painter().circle(rect.center(), radius, color, border);
                    }
                }
            })
            .response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn in_range_integers_select_their_state() {
        assert_eq!(state_for_value(&PvValue::Int(0)), Some(0));
        assert_eq!(state_for_value(&PvValue::Int(7)), Some(7));
        assert_eq!(state_for_value(&PvValue::Int(15)), Some(15));
    }

    #[test]
    fn out_of_range_integers_leave_the_state_unchanged() {
        assert_eq!(state_for_value(&PvValue::Int(-1)), None);
        assert_eq!(state_for_value(&PvValue::Int(16)), None);
    }

    #[test]
    fn floats_truncate_toward_zero_within_range() {
        // PyDM `int(new_val)` after the `0 <= new_val <= 15` guard.
        assert_eq!(state_for_value(&PvValue::Float(3.7)), Some(3));
        assert_eq!(state_for_value(&PvValue::Float(15.0)), Some(15));
        // 15.5 > 15 → the guard rejects it (PyDM keeps the prior state).
        assert_eq!(state_for_value(&PvValue::Float(15.5)), None);
        // -0.5 < 0 → rejected before truncation.
        assert_eq!(state_for_value(&PvValue::Float(-0.5)), None);
    }

    #[test]
    fn bool_and_enum_map_to_their_numeric_value() {
        assert_eq!(state_for_value(&PvValue::Bool(false)), Some(0));
        assert_eq!(state_for_value(&PvValue::Bool(true)), Some(1));
        assert_eq!(
            state_for_value(&PvValue::Enum {
                index: 5,
                label: None
            }),
            Some(5)
        );
        // An enum index past 15 is out of range.
        assert_eq!(
            state_for_value(&PvValue::Enum {
                index: 20,
                label: None
            }),
            None
        );
    }

    #[test]
    fn non_finite_and_non_numeric_values_leave_the_state_unchanged() {
        assert_eq!(state_for_value(&PvValue::Float(f64::NAN)), None);
        assert_eq!(state_for_value(&PvValue::Float(f64::INFINITY)), None);
        // PyDM: `"3" >= 0` raises → the update is ignored.
        assert_eq!(state_for_value(&PvValue::Str(Arc::from("3"))), None);
        assert_eq!(
            state_for_value(&PvValue::FloatArray(Arc::from(vec![1.0, 2.0]))),
            None
        );
    }
}
