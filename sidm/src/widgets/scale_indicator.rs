//! `PydmScaleIndicator` — a value shown as a bar/pointer on a tick scale.
//!
//! Ports `pydm/widgets/scale.py` (`QScale` + `PyDMScaleIndicator`) with the alarm
//! colouring of `pydm/widgets/analog_indicator.py` folded in: the value is mapped
//! to its proportion between the lower/upper limits (the user-defined limits, or
//! the PV control limits) and drawn either as a filled bar (`barIndicator`) or a
//! pointer, over a background with `num_divisions` tick marks, horizontally or
//! vertically. An optional value label shows the formatted value.
//!
//! The position maths is the pure [`value_proportion`] (mirroring PyDM
//! `calculate_position_for_value`: missing / non-finite / out-of-range / zero-span
//! values are off-scale) and [`division_proportions`]; the painting is verified
//! by a headless wgpu readback.
//!
//! **Consolidation:** PyDM ships the plain scale (`PyDMScaleIndicator`) and the
//! alarmed analog indicator (`PyDMAnalogIndicator`, which adds a set-point pointer
//! and alarm-region shading) as two widgets; here one widget covers the scale and
//! colours the bar by alarm severity when `alarmSensitiveContent` is set. The
//! analog indicator's separate set-point pointer and multi-region alarm shading
//! are not ported.

use siplot::egui::{self, Color32, Stroke, Vec2};

use crate::channel::{Channel, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::{ChannelBase, control_range, severity_color};
use crate::widgets::byte::Orientation;
use crate::widgets::display_format::{DisplayFormat, FormatSpec, format_value};

/// Default number of tick divisions (PyDM `QScale._num_divisions`).
pub const DEFAULT_NUM_DIVISIONS: u32 = 10;
const DEFAULT_SIZE: Vec2 = Vec2::new(220.0, 44.0);

/// The proportion in `[0, 1]` of `value` between `lower` and `upper`, or `None`
/// when the value is non-finite, out of `[lower, upper]`, or the span is zero
/// (PyDM `calculate_position_for_value`: these are off-scale and not drawn).
pub fn value_proportion(value: f64, lower: f64, upper: f64) -> Option<f64> {
    if !value.is_finite() || value < lower || value > upper || upper - lower == 0.0 {
        None
    } else {
        Some((value - lower) / (upper - lower))
    }
}

/// Tick proportions at `i / num_divisions` for `i` in `0..=num_divisions` (PyDM
/// `draw_ticks`). `num_divisions` is clamped to at least 1.
pub fn division_proportions(num_divisions: u32) -> Vec<f64> {
    let n = num_divisions.max(1);
    (0..=n).map(|i| f64::from(i) / f64::from(n)).collect()
}

/// A value indicator on a tick scale (PyDM `PyDMScaleIndicator`).
pub struct PydmScaleIndicator {
    base: ChannelBase,
    user_limits: Option<(f64, f64)>,
    num_divisions: u32,
    orientation: Orientation,
    bar_indicator: bool,
    show_value_label: bool,
    precision: Option<i32>,
    bar_color: Color32,
    tick_color: Color32,
    background: Color32,
    size: Vec2,
}

impl PydmScaleIndicator {
    /// Connect `address` and wrap it in a scale indicator (horizontal, pointer
    /// style, value label on — PyDM defaults).
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            base: ChannelBase::new(engine.connect(address)?),
            user_limits: None,
            num_divisions: DEFAULT_NUM_DIVISIONS,
            orientation: Orientation::Horizontal,
            bar_indicator: false,
            show_value_label: true,
            precision: None,
            bar_color: Color32::from_rgb(0, 150, 220),
            tick_color: Color32::from_gray(160),
            background: Color32::from_gray(40),
            size: DEFAULT_SIZE,
        })
    }

    /// Override the scale limits (builder style; PyDM `userDefinedLimits` /
    /// `userLowerLimit` / `userUpperLimit`). Without this the PV control limits
    /// are used.
    pub fn with_limits(mut self, lower: f64, upper: f64) -> Self {
        self.user_limits = Some((lower, upper));
        self
    }

    /// Set the number of tick divisions (builder style; PyDM `numDivisions`).
    pub fn with_num_divisions(mut self, num_divisions: u32) -> Self {
        self.num_divisions = num_divisions;
        self
    }

    /// Lay the scale out horizontally (default) or vertically (builder style;
    /// PyDM `orientation`).
    pub fn with_orientation(mut self, orientation: Orientation) -> Self {
        self.orientation = orientation;
        self
    }

    /// Draw the value as a filled bar rather than a pointer (builder style; PyDM
    /// `barIndicator`).
    pub fn with_bar_indicator(mut self, bar: bool) -> Self {
        self.bar_indicator = bar;
        self
    }

    /// Show the formatted value next to the scale (builder style; PyDM
    /// `showValue`).
    pub fn with_value_label(mut self, show: bool) -> Self {
        self.show_value_label = show;
        self
    }

    /// Override the value-label precision (builder style; PyDM `precision`).
    pub fn with_precision(mut self, precision: i32) -> Self {
        self.precision = Some(precision);
        self
    }

    /// Set the bar/pointer colour (builder style).
    pub fn with_bar_color(mut self, color: Color32) -> Self {
        self.bar_color = color;
        self
    }

    /// Set the scale size in points (builder style).
    pub fn with_size(mut self, size: Vec2) -> Self {
        self.size = size;
        self
    }

    /// The underlying channel.
    pub fn channel(&self) -> &Channel {
        self.base.channel()
    }

    /// Render the scale this frame.
    pub fn show(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let state = self.base.channel().state();
        let value = state.value.as_ref().and_then(PvValue::as_f64);
        let limits = control_range(&state, self.user_limits);
        let proportion = match (value, limits) {
            (Some(v), Some((lo, hi))) => value_proportion(v, lo, hi),
            _ => None,
        };
        // PyDM analog indicator: colour the bar by alarm severity when content is
        // alarm-sensitive.
        let bar_color = if self.base.alarm_sensitive_content {
            severity_color(state.effective_severity()).unwrap_or(self.bar_color)
        } else {
            self.bar_color
        };
        let label_text = if self.show_value_label {
            format_value(
                state.value.as_ref(),
                &state,
                FormatSpec {
                    format: DisplayFormat::Default,
                    precision: self.precision,
                    show_units: true,
                },
            )
        } else {
            String::new()
        };

        self.base
            .framed(ui, &state, false, |ui| {
                ui.vertical(|ui| {
                    if self.show_value_label {
                        ui.label(label_text);
                    }
                    let (rect, _) = ui.allocate_exact_size(self.size, egui::Sense::hover());
                    if ui.is_rect_visible(rect) {
                        self.paint(ui.painter(), rect, proportion, bar_color);
                    }
                });
            })
            .response
    }

    /// Paint the background, ticks, and the value bar/pointer.
    fn paint(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        proportion: Option<f64>,
        bar_color: Color32,
    ) {
        painter.rect_filled(rect, egui::CornerRadius::ZERO, self.background);

        let horizontal = self.orientation == Orientation::Horizontal;
        let tick_stroke = Stroke::new(1.0, self.tick_color);
        for tp in division_proportions(self.num_divisions) {
            let (a, b) = self.axis_line(rect, tp, horizontal);
            painter.line_segment([a, b], tick_stroke);
        }

        let Some(p) = proportion else {
            return;
        };
        if self.bar_indicator {
            painter.rect_filled(
                self.bar_rect(rect, p, horizontal),
                egui::CornerRadius::ZERO,
                bar_color,
            );
        } else {
            let (a, b) = self.axis_line(rect, p, horizontal);
            painter.line_segment([a, b], Stroke::new(3.0, bar_color));
        }
    }

    /// Endpoints of the cross-axis line at proportion `p` along the main axis.
    fn axis_line(&self, rect: egui::Rect, p: f64, horizontal: bool) -> (egui::Pos2, egui::Pos2) {
        let p = p as f32;
        if horizontal {
            let x = rect.left() + p * rect.width();
            (egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom()))
        } else {
            // Vertical: the value grows upward, so proportion 0 is at the bottom.
            let y = rect.bottom() - p * rect.height();
            (egui::pos2(rect.left(), y), egui::pos2(rect.right(), y))
        }
    }

    /// The filled bar rectangle from the origin to proportion `p`.
    fn bar_rect(&self, rect: egui::Rect, p: f64, horizontal: bool) -> egui::Rect {
        let p = p as f32;
        if horizontal {
            egui::Rect::from_min_max(
                rect.left_top(),
                egui::pos2(rect.left() + p * rect.width(), rect.bottom()),
            )
        } else {
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.bottom() - p * rect.height()),
                rect.right_bottom(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proportion_at_limits_and_midpoint() {
        assert_eq!(value_proportion(0.0, 0.0, 100.0), Some(0.0));
        assert_eq!(value_proportion(100.0, 0.0, 100.0), Some(1.0));
        assert_eq!(value_proportion(25.0, 0.0, 100.0), Some(0.25));
    }

    #[test]
    fn out_of_range_and_degenerate_are_off_scale() {
        assert_eq!(value_proportion(-1.0, 0.0, 100.0), None);
        assert_eq!(value_proportion(101.0, 0.0, 100.0), None);
        // Zero span.
        assert_eq!(value_proportion(5.0, 5.0, 5.0), None);
        // Non-finite.
        assert_eq!(value_proportion(f64::NAN, 0.0, 100.0), None);
        assert_eq!(value_proportion(f64::INFINITY, 0.0, 100.0), None);
    }

    #[test]
    fn divisions_span_zero_to_one_inclusive() {
        assert_eq!(division_proportions(4), vec![0.0, 0.25, 0.5, 0.75, 1.0]);
        // Clamped to at least one division.
        assert_eq!(division_proportions(0), vec![0.0, 1.0]);
    }
}
