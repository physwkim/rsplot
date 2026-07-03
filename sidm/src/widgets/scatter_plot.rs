//! `SidmScatterPlot` — paired scalar channels as accumulated XY markers.
//!
//! Ports `pydm/widgets/scatterplot.py` (`PyDMScatterPlot` +
//! `ScatterPlotCurveItem`) onto a `siplot` [`Plot1D`] scatter item. Each curve
//! pairs an X scalar channel with a Y scalar channel; both channels' value-event
//! streams are drained and merged into one arrival-ordered sequence, and a pair
//! is appended whenever the [`RedrawMode`] is satisfied (and only once both
//! channels have a value) to a capacity-bounded [`TimeSeriesBuffer`] (PyDM
//! `receiveXValue`/`receiveYValue` → `update_buffer` rolling the
//! `(2, bufferSize)` array). Draining the event streams (rather than polling the
//! per-frame snapshot) means a burst arriving between two frames is paired
//! event-by-event, and a curve on a hidden tab keeps accumulating up to the
//! queue bound.
//!
//! The redraw gate is the shared [`mode_allows`]; the pairing state machine
//! (`PairAccumulator`) and the buffer ([`TimeSeriesBuffer`]) are unit-tested
//! purely. The GPU rendering is exercised by a headless wgpu readback test.

use std::time::SystemTime;

use siplot::egui::Color32;
use siplot::egui_wgpu::RenderState;
use siplot::{DataMargins, ItemHandle, Plot1D, PlotId, PlotResponse, Symbol, YAxis, egui};

use crate::channel::{Channel, ValueSubscription};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::middle_click_copy;
use crate::widgets::plot_menu::{
    YAxisMenu, enable_y_autoscale, set_x_range, set_y_range, show_with_y_axis_menu,
};
use crate::widgets::plot_style::{CurveStyle, ensure_axis_autoscale};
use crate::widgets::ring_buffer::{DEFAULT_BUFFER_SIZE, TimeSeriesBuffer};
use crate::widgets::waveform_plot::{RedrawMode, mode_allows};

/// Default marker size in points; owned by [`crate::widgets::plot_style`] and
/// re-exported here for the established public path.
pub use crate::widgets::plot_style::DEFAULT_SYMBOL_SIZE;

/// Which paired channel an arriving value belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Axis {
    X,
    Y,
}

/// The pure pairing state for one scatter curve: the latest X and Y scalar
/// values, which side has new data since the last commit (`pending_*`, the
/// inverse of PyDM `needs_new_*`), the [`RedrawMode`] gate, and the accumulated
/// `(x, y)` buffer. Factored out of [`ScatterCurve`] — which also holds the
/// channel / subscription / GPU handles — so the event-driven pairing is
/// unit-testable without a render state.
struct PairAccumulator {
    mode: RedrawMode,
    buffer: TimeSeriesBuffer,
    latest_x: Option<f64>,
    latest_y: Option<f64>,
    pending_x: bool,
    pending_y: bool,
}

impl PairAccumulator {
    fn new(mode: RedrawMode, buffer_size: usize) -> Self {
        Self {
            mode,
            buffer: TimeSeriesBuffer::new(buffer_size),
            latest_x: None,
            latest_y: None,
            pending_x: false,
            pending_y: false,
        }
    }

    /// Record one X or Y value arrival (PyDM `receiveXValue` / `receiveYValue`).
    fn apply(&mut self, axis: Axis, value: f64) {
        match axis {
            Axis::X => {
                self.latest_x = Some(value);
                self.pending_x = true;
            }
            Axis::Y => {
                self.latest_y = Some(value);
                self.pending_y = true;
            }
        }
    }

    /// Whether a pair should be appended now: both channels have a value and the
    /// redraw mode is satisfied (PyDM `update_buffer`).
    fn ready(&self) -> bool {
        self.latest_x.is_some()
            && self.latest_y.is_some()
            && mode_allows(self.mode, self.pending_x, self.pending_y)
    }

    /// Append the latest `(x, y)` pair and clear the pending flags (PyDM
    /// `update_buffer` roll).
    fn commit(&mut self) {
        if let (Some(x), Some(y)) = (self.latest_x, self.latest_y) {
            self.buffer.push(x, y);
            self.pending_x = false;
            self.pending_y = false;
        }
    }

    /// Process value events in arrival order, appending a pair whenever the
    /// redraw mode is satisfied. Returns `true` when at least one pair was
    /// appended (so the curve needs a redraw). Each event is one monitor
    /// callback, so a burst arriving between two frames is paired event-by-event
    /// — not coalesced into a single pair.
    fn ingest(&mut self, events: impl IntoIterator<Item = (Axis, f64)>) -> bool {
        let mut committed = false;
        for (axis, value) in events {
            self.apply(axis, value);
            if self.ready() {
                self.commit();
                committed = true;
            }
        }
        committed
    }
}

/// One scatter curve: paired X/Y scalar channels (held to keep the connections
/// alive), their value-event subscriptions, the pairing accumulator, and the GPU
/// item handle plus reusable render scratch.
struct ScatterCurve {
    x_channel: Channel,
    y_channel: Channel,
    x_subscription: ValueSubscription,
    y_subscription: ValueSubscription,
    handle: ItemHandle,
    style: CurveStyle,
    pair: PairAccumulator,
    /// Reusable render buffers.
    xs: Vec<f64>,
    ys: Vec<f64>,
}

impl ScatterCurve {
    /// Redraw the markers from the current buffer.
    fn redraw(&mut self, plot: &mut Plot1D) {
        self.pair.buffer.ordered_into(&mut self.xs, &mut self.ys);
        plot.update_curve_spec(self.handle, self.style.to_spec(&self.xs, &self.ys));
    }
}

/// A plot accumulating `(x, y)` pairs from paired scalar PVs (PyDM
/// `PyDMScatterPlot`).
pub struct SidmScatterPlot {
    plot: Plot1D,
    curves: Vec<ScatterCurve>,
    buffer_size: usize,
    /// State for the pyqtgraph-style Y-axis context menu (auto-scale + range).
    y_menu: YAxisMenu,
}

impl SidmScatterPlot {
    /// Create an empty scatter plot on the given GPU `render_state` and plot
    /// `id`.
    pub fn new(render_state: &RenderState, id: PlotId) -> Self {
        Self {
            plot: Plot1D::new(render_state, id),
            curves: Vec::new(),
            buffer_size: DEFAULT_BUFFER_SIZE,
            y_menu: YAxisMenu::new(),
        }
    }

    /// Set the per-curve buffer capacity for curves added afterwards (builder
    /// style; PyDM `bufferSize`).
    pub fn with_buffer_size(mut self, buffer_size: usize) -> Self {
        self.buffer_size = buffer_size;
        self
    }

    /// Add a per-side data margin around the autoscaled data (builder style; silx
    /// `setDataMargins` / pyqtgraph autorange `padding`). Each ratio expands that
    /// side of an autoscaled axis by `ratio * range` when it refits, so the data
    /// keeps a gap from the axis edge instead of touching it. Only axes that
    /// autoscale are padded; a pinned (manually ranged) axis is unaffected.
    /// Default is no margin (the data fits the axes exactly).
    pub fn with_data_margins(mut self, margins: DataMargins) -> Self {
        self.plot.plot_mut().set_data_margins(margins);
        self
    }

    /// Set the plot title (builder style; PyDM `BasePlot.setPlotTitle`, MEDM
    /// `plotcom` `title`).
    pub fn with_title(mut self, title: &str) -> Self {
        self.plot.set_graph_title(title);
        self
    }

    /// Set the X-axis label (builder style; PyDM `xLabels`, MEDM `plotcom`
    /// `xlabel`).
    pub fn with_x_label(mut self, label: &str) -> Self {
        self.plot.set_graph_x_label(label);
        self
    }

    /// Set the left Y-axis label (builder style; PyDM `yLabels`, MEDM
    /// `plotcom` `ylabel`).
    pub fn with_y_label(mut self, label: &str) -> Self {
        self.plot.set_graph_y_label(label, YAxis::Left);
        self
    }

    /// Pin a fixed X range, disabling X autoscale (builder style; PyDM
    /// `setAutoRangeX(False)` + `setMinXRange`/`setMaxXRange`).
    pub fn with_x_range(mut self, min: f64, max: f64) -> Self {
        set_x_range(&mut self.plot, min, max);
        self
    }

    /// Pin a fixed Y range, disabling live Y autoscale (builder style; PyDM
    /// `setAutoRangeY(False)` + `setMinYRange`/`setMaxYRange`). Same rule as
    /// the Y-axis context menu's manual range.
    pub fn with_y_range(mut self, min: f64, max: f64) -> Self {
        set_y_range(&mut self.plot, min, max);
        self
    }

    /// Set the axis/label/title foreground colour, grid lines included
    /// (builder style; PyDM `BasePlot.setAxisColor`, MEDM `plotcom` `clr`).
    pub fn with_axis_color(mut self, color: Color32) -> Self {
        self.plot.set_foreground_colors(color, color);
        self
    }

    /// Set the plot background colour, data area included (builder style; PyDM
    /// `BasePlot.setBackgroundColor`, MEDM `plotcom` `bclr`).
    pub fn with_background_color(mut self, color: Color32) -> Self {
        self.plot.set_background_colors(color, color);
        self
    }

    /// The underlying plot, for styling.
    pub fn plot(&self) -> &Plot1D {
        &self.plot
    }

    /// The underlying plot, mutably, for styling.
    pub fn plot_mut(&mut self) -> &mut Plot1D {
        &mut self.plot
    }

    /// Number of curves.
    pub fn curve_count(&self) -> usize {
        self.curves.len()
    }

    /// Add a paired X/Y scalar channel as a scatter curve. Returns the new
    /// curve's index.
    pub fn add_xy_channel(
        &mut self,
        engine: &Engine,
        x_address: &str,
        y_address: &str,
        color: Color32,
        legend: impl Into<String>,
    ) -> Result<usize, EngineError> {
        let x_channel = engine.connect(x_address)?;
        let y_channel = engine.connect(y_address)?;
        // Subscribe to both value streams so paired samples accumulate
        // event-by-event (every monitor callback), not from a per-frame snapshot:
        // a burst between frames is preserved and a hidden tab keeps accumulating.
        let x_subscription = x_channel.subscribe_values(self.buffer_size);
        let y_subscription = y_channel.subscribe_values(self.buffer_size);
        let handle =
            self.plot
                .add_scatter_with_symbol(&[], &[], color, Symbol::Circle, DEFAULT_SYMBOL_SIZE);
        self.plot.set_item_legend(handle, legend);
        self.curves.push(ScatterCurve {
            x_channel,
            y_channel,
            x_subscription,
            y_subscription,
            handle,
            style: CurveStyle::markers(color),
            pair: PairAccumulator::new(RedrawMode::default(), self.buffer_size),
            xs: Vec::new(),
            ys: Vec::new(),
        });
        Ok(self.curves.len() - 1)
    }

    /// Set the redraw mode of curve `index` (PyDM `redraw_mode`). No-op for an
    /// out-of-range index.
    pub fn set_redraw_mode(&mut self, index: usize, mode: RedrawMode) {
        if let Some(curve) = self.curves.get_mut(index) {
            curve.pair.mode = mode;
        }
    }

    /// Restyle curve `index` (PyDM `BasePlotCurveItem` properties: colour, marker
    /// symbol/size, Y axis) and re-draw it immediately. Assigning the curve to a
    /// secondary axis ([`YAxis::Right`](siplot::YAxis::Right) or an
    /// [`YAxis::Extra`](siplot::YAxis::Extra) stacked axis) enables that axis'
    /// autoscale. Returns `false` for an out-of-range index.
    pub fn set_curve_style(&mut self, index: usize, style: CurveStyle) -> bool {
        if index >= self.curves.len() {
            return false;
        }
        let axis = style.y_axis;
        self.curves[index].style = style;
        ensure_axis_autoscale(&mut self.plot, axis);
        self.curves[index].redraw(&mut self.plot);
        true
    }

    /// Inject an `(x, y)` pair directly into curve `index` and redraw (PyDM "you
    /// can call this yourself to inject data into the curve" — replay). Returns
    /// `false` for an out-of-range index.
    pub fn inject(&mut self, index: usize, x: f64, y: f64) -> bool {
        if index >= self.curves.len() {
            return false;
        }
        self.curves[index].pair.buffer.push(x, y);
        self.curves[index].redraw(&mut self.plot);
        true
    }

    /// Drain both value streams of every curve, merge them into one
    /// arrival-ordered list, append the pairs whose redraw mode is satisfied, and
    /// render the plot this frame.
    ///
    /// The queues are filled by the engine independent of repaint, so a scatter
    /// plot on an inactive tab keeps accumulating (up to the queue bound), and a
    /// burst arriving between two frames is paired event-by-event rather than
    /// coalesced (PyDM processes each `receiveXValue` / `receiveYValue` callback
    /// in order).
    pub fn show(&mut self, ui: &mut egui::Ui) -> PlotResponse {
        for curve in &mut self.curves {
            // Tag each drained event with its axis and engine receive time, then
            // sort by time so X and Y events pair in their true arrival order even
            // when several arrive between two frames.
            let mut events: Vec<(Axis, f64, SystemTime)> = Vec::new();
            curve.x_subscription.drain(|e| {
                if let Some(v) = e.value.as_f64() {
                    events.push((Axis::X, v, e.time));
                }
            });
            curve.y_subscription.drain(|e| {
                if let Some(v) = e.value.as_f64() {
                    events.push((Axis::Y, v, e.time));
                }
            });
            if events.is_empty() {
                continue;
            }
            events.sort_by(|a, b| a.2.cmp(&b.2));
            if curve
                .pair
                .ingest(events.iter().map(|&(axis, v, _)| (axis, v)))
            {
                curve.redraw(&mut self.plot);
            }
        }
        ui.ctx().request_repaint();
        let response = show_with_y_axis_menu(&mut self.plot, &mut self.y_menu, ui);
        // MEDM Btn2 copies every record the plot carries (Y then X, PyDM channels() order).
        middle_click_copy(
            ui,
            &response.response,
            self.curves
                .iter()
                .flat_map(|c| [c.y_channel.address().raw(), c.x_channel.address().raw()]),
        );
        response
    }

    /// Pin a fixed Y range, disabling live autoscale (pyqtgraph `setYRange`);
    /// the range survives data updates until autoscale is re-enabled. Same effect
    /// as the context menu's "Set Y range".
    pub fn set_y_range(&mut self, min: f64, max: f64) {
        set_y_range(&mut self.plot, min, max);
    }

    /// Re-enable live Y autoscale and refit to the data now (pyqtgraph
    /// auto-range); same effect as the context menu's "Auto-scale".
    pub fn enable_y_autoscale(&mut self) {
        enable_y_autoscale(&mut self.plot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acc(mode: RedrawMode) -> PairAccumulator {
        PairAccumulator::new(mode, 16)
    }

    #[test]
    fn on_either_commits_for_every_event_once_both_have_a_value() {
        let mut a = acc(RedrawMode::OnEither);
        // Only X so far: Y has no value yet, so no pair is appendable.
        a.ingest([(Axis::X, 1.0)]);
        assert_eq!(a.buffer.len(), 0);
        // Y arrives: both now have a value → pair (1.0, 2.0).
        a.ingest([(Axis::Y, 2.0)]);
        assert_eq!(a.buffer.newest(), Some((1.0, 2.0)));
        // Three X events in one frame → three points (no coalescing), each
        // pairing with the latest Y.
        a.ingest([(Axis::X, 3.0), (Axis::X, 4.0), (Axis::X, 5.0)]);
        assert_eq!(a.buffer.len(), 4);
        assert_eq!(a.buffer.newest(), Some((5.0, 2.0)));
    }

    #[test]
    fn on_both_commits_only_after_each_side_updates() {
        let mut a = acc(RedrawMode::OnBoth);
        // X twice with no Y since the last commit: still waiting for a Y.
        a.ingest([(Axis::X, 1.0), (Axis::X, 2.0)]);
        assert_eq!(a.buffer.len(), 0);
        // Y arrives → both pending → one pair (2.0, 9.0), pending cleared.
        a.ingest([(Axis::Y, 9.0)]);
        assert_eq!(a.buffer.newest(), Some((2.0, 9.0)));
        assert_eq!(a.buffer.len(), 1);
        // Another Y with no new X since the commit: not both → no new pair.
        a.ingest([(Axis::Y, 10.0)]);
        assert_eq!(a.buffer.len(), 1);
    }

    #[test]
    fn merged_events_pair_in_arrival_order() {
        // Interleaved x, y, x, y, x produces arrival-ordered pairs (PyDM
        // processes each callback in turn), not all-X-then-all-Y.
        let mut a = acc(RedrawMode::OnEither);
        a.ingest([
            (Axis::X, 1.0),
            (Axis::Y, 10.0), // (1, 10)
            (Axis::X, 2.0),  // (2, 10)
            (Axis::Y, 20.0), // (2, 20)
            (Axis::X, 3.0),  // (3, 20)
        ]);
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        a.buffer.ordered_into(&mut xs, &mut ys);
        assert_eq!(xs, vec![1.0, 2.0, 2.0, 3.0]);
        assert_eq!(ys, vec![10.0, 10.0, 20.0, 20.0]);
    }
}
