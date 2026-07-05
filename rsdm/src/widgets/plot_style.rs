//! Shared per-curve styling for the plot widgets.
//!
//! PyDM's plot curves all derive from `BasePlotCurveItem` (`baseplot.py`), whose
//! styling properties are `color`, `lineStyle`, `lineWidth`, `symbol`,
//! `symbolSize`, and `yAxisName` (which named Y axis the curve is plotted
//! against). rsplot's [`CurveSpec`] carries the exact same knobs, so this module
//! is the one owner of the PyDM-curve-property → `CurveSpec` mapping that
//! `RsdmTimePlot` / `RsdmWaveformPlot` / `RsdmScatterPlot` / `RsdmEventPlot` all
//! build their specs from.
//!
//! PyDM (via `MultiAxisPlot`) supports an arbitrary number of named Y axes
//! (`yAxisName` is a free string). rsplot now models N stacked Y axes too
//! ([`YAxis::Left`], [`YAxis::Right`], and [`YAxis::Extra(n)`](YAxis::Extra)),
//! so a curve maps to any of them: create an extra axis with
//! [`Plot1D::add_extra_y_axis`](rsplot::PlotWidget::add_extra_y_axis) and bind the curve via
//! [`CurveStyle::with_y_axis`]. PyDM's free-string axis *names* are not modelled
//! (axes are addressed by index), and per-extra-axis interactive pan/zoom is not
//! wired (see the rsplot multi-axis deferred list).

use rsplot::egui::Color32;
use rsplot::{CurveSpec, LineStyle, Plot1D, Symbol, YAxis};

/// Default marker size in points (matches rsplot `add_scatter`). Single owner;
/// `scatter_plot` re-exports it for backwards compatibility.
pub const DEFAULT_SYMBOL_SIZE: f32 = 7.0;
/// Default solid-line width in points (rsplot / PyDM curve default).
pub const DEFAULT_LINE_WIDTH: f32 = 1.0;

/// The per-curve drawing style, mirroring PyDM `BasePlotCurveItem`'s styling
/// properties. Maps to a rsplot [`CurveSpec`] via [`CurveStyle::to_spec`]. The
/// `line_style` / `symbol` / `y_axis` enums are rsplot's (`LineStyle`, `Symbol`,
/// `YAxis`). Not `Copy` because [`LineStyle::Custom`] carries a dash pattern.
#[derive(Clone, Debug, PartialEq)]
pub struct CurveStyle {
    /// Curve / marker colour (PyDM `color`).
    pub color: Color32,
    /// Line stroke style (PyDM `lineStyle`). [`LineStyle::None`] draws no line.
    pub line_style: LineStyle,
    /// Line width in points (PyDM `lineWidth`).
    pub line_width: f32,
    /// Marker symbol (PyDM `symbol`); `None` draws no marker.
    pub symbol: Option<Symbol>,
    /// Marker size in points (PyDM `symbolSize`).
    pub symbol_size: f32,
    /// Which Y axis the curve is plotted against (PyDM `yAxisName`): the left
    /// axis, the right (y2) axis, or one of the extra stacked axes
    /// ([`YAxis::Extra`]).
    pub y_axis: YAxis,
}

impl CurveStyle {
    /// A solid line of `color`, no markers, on the left axis (the default for a
    /// line plot).
    pub fn line(color: Color32) -> Self {
        Self {
            color,
            line_style: LineStyle::Solid,
            line_width: DEFAULT_LINE_WIDTH,
            symbol: None,
            symbol_size: DEFAULT_SYMBOL_SIZE,
            y_axis: YAxis::Left,
        }
    }

    /// Circle markers of `color` with no connecting line, on the left axis (the
    /// default for a scatter/event plot).
    pub fn markers(color: Color32) -> Self {
        Self {
            color,
            line_style: LineStyle::None,
            line_width: 0.0,
            symbol: Some(Symbol::Circle),
            symbol_size: DEFAULT_SYMBOL_SIZE,
            y_axis: YAxis::Left,
        }
    }

    /// Set the colour (builder style).
    pub fn with_color(mut self, color: Color32) -> Self {
        self.color = color;
        self
    }

    /// Set the line style (builder style; PyDM `lineStyle`).
    pub fn with_line_style(mut self, line_style: LineStyle) -> Self {
        self.line_style = line_style;
        self
    }

    /// Set the line width in points (builder style; PyDM `lineWidth`).
    pub fn with_line_width(mut self, line_width: f32) -> Self {
        self.line_width = line_width;
        self
    }

    /// Set the marker symbol (builder style; PyDM `symbol`).
    pub fn with_symbol(mut self, symbol: Option<Symbol>) -> Self {
        self.symbol = symbol;
        self
    }

    /// Set the marker size in points (builder style; PyDM `symbolSize`).
    pub fn with_symbol_size(mut self, symbol_size: f32) -> Self {
        self.symbol_size = symbol_size;
        self
    }

    /// Assign the curve to a Y axis (builder style; PyDM `yAxisName`). Pass
    /// [`YAxis::Extra(n)`](YAxis::Extra) to bind to a stacked extra axis created
    /// via [`Plot1D::add_extra_y_axis`](rsplot::PlotWidget::add_extra_y_axis).
    pub fn with_y_axis(mut self, y_axis: YAxis) -> Self {
        self.y_axis = y_axis;
        self
    }

    /// Build a rsplot [`CurveSpec`] over `x`/`y` carrying this style.
    pub fn to_spec<'a>(&self, x: &'a [f64], y: &'a [f64]) -> CurveSpec<'a> {
        let mut spec = CurveSpec::new(x, y, self.color);
        spec.line_style = self.line_style.clone();
        spec.line_width = self.line_width;
        spec.symbol = self.symbol;
        spec.symbol_size = self.symbol_size;
        spec.y_axis = self.y_axis;
        spec
    }
}

/// Ensure the secondary Y axis a curve is bound to autoscales to fit it. This is
/// the single owner of the "binding a curve to a non-left axis re-enables that
/// axis' autoscale" rule shared by every plot widget's `set_curve_style`: the
/// right (y2) and extra axes default to autoscaling, so a curve added to one
/// gets a sensible range without the caller pinning it. The extra axis must
/// already exist (created via [`Plot1D::add_extra_y_axis`](rsplot::PlotWidget::add_extra_y_axis)); binding to an
/// unknown index is a no-op here and the curve falls back to the left axis at
/// render time. [`YAxis::Left`] needs nothing — it always autoscales with the
/// primary data.
pub(crate) fn ensure_axis_autoscale(plot: &mut Plot1D, axis: YAxis) {
    match axis {
        YAxis::Left => {}
        YAxis::Right => {
            plot.plot_mut().set_y2_autoscale(true);
        }
        YAxis::Extra(n) => {
            plot.set_extra_y_autoscale(n, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_default_is_solid_left_no_marker() {
        let s = CurveStyle::line(Color32::RED);
        assert_eq!(s.color, Color32::RED);
        assert_eq!(s.line_style, LineStyle::Solid);
        assert_eq!(s.line_width, DEFAULT_LINE_WIDTH);
        assert_eq!(s.symbol, None);
        assert_eq!(s.y_axis, YAxis::Left);
    }

    #[test]
    fn markers_default_is_circle_no_line() {
        let s = CurveStyle::markers(Color32::GREEN);
        assert_eq!(s.line_style, LineStyle::None);
        assert_eq!(s.line_width, 0.0);
        assert_eq!(s.symbol, Some(Symbol::Circle));
        assert_eq!(s.symbol_size, DEFAULT_SYMBOL_SIZE);
        assert_eq!(s.y_axis, YAxis::Left);
    }

    #[test]
    fn builders_set_every_field() {
        let s = CurveStyle::line(Color32::WHITE)
            .with_color(Color32::BLUE)
            .with_line_style(LineStyle::Dashed)
            .with_line_width(3.5)
            .with_symbol(Some(Symbol::Square))
            .with_symbol_size(12.0)
            .with_y_axis(YAxis::Right);
        assert_eq!(s.color, Color32::BLUE);
        assert_eq!(s.line_style, LineStyle::Dashed);
        assert_eq!(s.line_width, 3.5);
        assert_eq!(s.symbol, Some(Symbol::Square));
        assert_eq!(s.symbol_size, 12.0);
        assert_eq!(s.y_axis, YAxis::Right);
    }

    #[test]
    fn to_spec_maps_every_styling_field() {
        let x = [0.0, 1.0];
        let y = [2.0, 3.0];
        let s = CurveStyle::line(Color32::BLUE)
            .with_line_style(LineStyle::Dotted)
            .with_line_width(2.0)
            .with_symbol(Some(Symbol::Diamond))
            .with_symbol_size(9.0)
            .with_y_axis(YAxis::Right);
        let spec = s.to_spec(&x, &y);
        assert_eq!(spec.line_style, LineStyle::Dotted);
        assert_eq!(spec.line_width, 2.0);
        assert_eq!(spec.symbol, Some(Symbol::Diamond));
        assert_eq!(spec.symbol_size, 9.0);
        assert_eq!(spec.y_axis, YAxis::Right);
        assert_eq!(spec.x, &x);
        assert_eq!(spec.y, &y);
    }
}
