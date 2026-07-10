//! `RsdmScaleIndicator` — a value shown as a bar/pointer on a tick scale.
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

use rsplot::egui::{self, Color32, Stroke, Vec2};

use crate::channel::{Channel, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::{
    AlarmPalette, BorderMode, ChannelBase, UserLimits, control_range, justified_size,
    layout_justify,
};
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
pub struct RsdmScaleIndicator {
    base: ChannelBase,
    user_limits: UserLimits,
    num_divisions: u32,
    orientation: Orientation,
    inverted_appearance: bool,
    origin_at_center: bool,
    bar_indicator: bool,
    show_value_label: bool,
    precision: Option<i32>,
    bar_color: Color32,
    tick_color: Color32,
    background: Color32,
    size: Vec2,
}

impl RsdmScaleIndicator {
    /// Connect `address` and wrap it in a scale indicator (horizontal, pointer
    /// style, value label on — PyDM defaults).
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            base: ChannelBase::new(engine.connect(address)?),
            user_limits: UserLimits::default(),
            num_divisions: DEFAULT_NUM_DIVISIONS,
            orientation: Orientation::Horizontal,
            inverted_appearance: false,
            origin_at_center: false,
            bar_indicator: false,
            show_value_label: true,
            precision: None,
            bar_color: Color32::from_rgb(0, 150, 220),
            tick_color: Color32::from_gray(160),
            background: Color32::from_gray(40),
            size: DEFAULT_SIZE,
        })
    }

    /// Override both scale limits (builder style; PyDM `userDefinedLimits`).
    /// Without this the PV control limits are used.
    pub fn with_limits(mut self, lower: f64, upper: f64) -> Self {
        self.user_limits = UserLimits::both(lower, upper);
        self
    }

    /// Pin only the lower bound, leaving the upper channel-driven (builder style;
    /// MEDM single-sided `loprSrc="default"`, R2-66).
    pub fn with_lower_limit(mut self, lower: f64) -> Self {
        self.user_limits.lower = Some(lower);
        self
    }

    /// Pin only the upper bound, leaving the lower channel-driven (builder style;
    /// MEDM single-sided `hoprSrc="default"`, R2-66).
    pub fn with_upper_limit(mut self, upper: f64) -> Self {
        self.user_limits.upper = Some(upper);
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

    /// Grow the value from the right/top edge instead of the left/bottom one
    /// (builder style; PyDM `QScale.invertedAppearance`). This is MEDM's bar
    /// with `direction="down"`/`"left"`: `xc/BarGraph.c` fills `XcVertDown`
    /// from the top edge (`:939-954`) and `XcHorizLeft` from the right edge
    /// (`:973-988`).
    pub fn with_inverted_appearance(mut self, inverted: bool) -> Self {
        self.inverted_appearance = inverted;
        self
    }

    /// Fill the bar from the scale's geometric midpoint instead of its origin
    /// edge (builder style; MEDM bar `fillmod="from center"`). The C widget
    /// anchors the fill at `mid = len/2` — the CENTRE of the widget, not the
    /// value-zero position (`xc/BarGraph.c:911,921-988`), which is where this
    /// deliberately differs from PyDM `QScale.originAtZero`. Only the bar
    /// indicator uses the origin; the pointer is a single position.
    pub fn with_origin_at_center(mut self, center: bool) -> Self {
        self.origin_at_center = center;
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

    /// Recolour the bar/pointer by alarm severity (PyDM `alarmSensitiveContent`,
    /// builder style). When on, [`Self::show`] tints by severity and falls back
    /// to [`Self::with_bar_color`] for `NoAlarm`.
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

    /// Set the scale size in points (builder style).
    pub fn with_size(mut self, size: Vec2) -> Self {
        self.size = size;
        self
    }

    /// Choose which severities draw a border (builder style;
    /// `DisconnectedOnly` for converted MEDM screens — MEDM draws no severity
    /// border, the dash is the RsDM disconnect marker).
    pub fn with_border_mode(mut self, mode: BorderMode) -> Self {
        self.base.border_mode = mode;
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
        // alarm-sensitive (through the base's palette).
        let bar_color = self.base.content_color(&state).unwrap_or(self.bar_color);
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
                // `ui.vertical` resets the layout, so capture the caller's
                // justify intent first; the bar then fills the space left
                // after the optional value label.
                let justify = layout_justify(ui);
                ui.vertical(|ui| {
                    if self.show_value_label {
                        ui.label(label_text);
                    }
                    let size = justified_size(justify, ui, self.size);
                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
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
        let tick_stroke = Stroke::new(1.0_f32, self.tick_color);
        for tp in division_proportions(self.num_divisions) {
            let (a, b) = self.axis_line(rect, tp, horizontal);
            painter.line_segment([a, b], tick_stroke);
        }

        let Some(p) = proportion else {
            return;
        };
        // Inversion remaps the drawn position, not the proportion: the value
        // grows from the opposite edge (xc/BarGraph.c XcVertDown/XcHorizLeft).
        if self.bar_indicator {
            painter.rect_filled(
                self.bar_rect(rect, self.pos(self.fill_origin()), self.pos(p), horizontal),
                egui::CornerRadius::ZERO,
                bar_color,
            );
        } else {
            let (a, b) = self.axis_line(rect, self.pos(p), horizontal);
            painter.line_segment([a, b], Stroke::new(3.0_f32, bar_color));
        }
    }

    /// The drawn main-axis position for value proportion `q`: mirrored when the
    /// appearance is inverted.
    fn pos(&self, q: f64) -> f64 {
        if self.inverted_appearance { 1.0 - q } else { q }
    }

    /// The bar fill's origin proportion: the geometric midpoint under
    /// "from center" (`xc/BarGraph.c:911` `mid = len/2`), else the origin
    /// edge. `pos(0.5) == 0.5` under inversion too, matching the C's XcCenter
    /// arms in all four orientations.
    fn fill_origin(&self) -> f64 {
        if self.origin_at_center { 0.5 } else { 0.0 }
    }

    /// Endpoints of the cross-axis line at drawn proportion `p` along the main
    /// axis.
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

    /// The filled bar rectangle between drawn main-axis proportions `a` and `b`
    /// (the bar spans the fill origin's position and the value's position, in
    /// either order — inversion or a centred origin can put `b` before `a`).
    fn bar_rect(&self, rect: egui::Rect, a: f64, b: f64, horizontal: bool) -> egui::Rect {
        let (lo, hi) = (a.min(b) as f32, a.max(b) as f32);
        if horizontal {
            egui::Rect::from_min_max(
                egui::pos2(rect.left() + lo * rect.width(), rect.top()),
                egui::pos2(rect.left() + hi * rect.width(), rect.bottom()),
            )
        } else {
            // Vertical: proportion 0 is at the bottom, so the higher proportion
            // is the smaller Y.
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.bottom() - hi * rect.height()),
                egui::pos2(rect.right(), rect.bottom() - lo * rect.height()),
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

    #[test]
    fn inverted_appearance_fills_from_the_opposite_edge() {
        let engine = Engine::new();
        let scale = RsdmScaleIndicator::new(&engine, "loc://scale_inv")
            .expect("connect")
            .with_bar_indicator(true);

        let h_rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 10.0));
        // Non-inverted horizontal control: 25% fills 0..25 from the left.
        let bar = scale.bar_rect(h_rect, scale.pos(0.0), scale.pos(0.25), true);
        assert_eq!((bar.min.x, bar.max.x), (0.0, 25.0));

        let scale = scale.with_inverted_appearance(true);
        // Inverted horizontal (XcHorizLeft, BarGraph.c:973-988: x = x0+len-d,
        // width = d): 25% fills the rightmost quarter.
        let bar = scale.bar_rect(h_rect, scale.pos(0.0), scale.pos(0.25), true);
        assert_eq!((bar.min.x, bar.max.x), (75.0, 100.0));

        // Inverted vertical (XcVertDown, BarGraph.c:939-954: y = y0, height =
        // d): 25% fills the topmost quarter (proportion 0 = bottom otherwise).
        let v_rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(10.0, 100.0));
        let bar = scale.bar_rect(v_rect, scale.pos(0.0), scale.pos(0.25), false);
        assert_eq!((bar.min.y, bar.max.y), (0.0, 25.0));

        // The pointer position mirrors the same way.
        let (a, _) = scale.axis_line(h_rect, scale.pos(0.25), true);
        assert_eq!(a.x, 75.0);
    }

    #[test]
    fn origin_at_center_fills_between_midpoint_and_value() {
        let engine = Engine::new();
        let scale = RsdmScaleIndicator::new(&engine, "loc://scale_center")
            .expect("connect")
            .with_bar_indicator(true);
        // Default: the fill anchors on the origin edge.
        assert_eq!(scale.fill_origin(), 0.0);
        let scale = scale.with_origin_at_center(true);
        assert_eq!(scale.fill_origin(), 0.5);

        let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 10.0));
        // MEDM XcCenter (BarGraph.c:956-971): below the midpoint fills
        // [value, mid]; above fills [mid, value] — anchored at len/2, not at
        // the value-zero position.
        let origin = scale.pos(scale.fill_origin());
        let below = scale.bar_rect(rect, origin, scale.pos(0.25), true);
        assert_eq!((below.min.x, below.max.x), (25.0, 50.0));
        let above = scale.bar_rect(rect, origin, scale.pos(0.75), true);
        assert_eq!((above.min.x, above.max.x), (50.0, 75.0));
        // At the midpoint the bar is empty (KE's .49 rounding note: no bar at
        // the centre value).
        let at_mid = scale.bar_rect(rect, origin, scale.pos(0.5), true);
        assert_eq!(at_mid.width(), 0.0);

        // Center + inverted (XcHorizLeft/XcCenter, BarGraph.c:973-986): the
        // same span mirrors, still anchored on the midpoint.
        let scale = scale.with_inverted_appearance(true);
        let origin = scale.pos(scale.fill_origin());
        let below = scale.bar_rect(rect, origin, scale.pos(0.25), true);
        assert_eq!((below.min.x, below.max.x), (50.0, 75.0));
    }
}
