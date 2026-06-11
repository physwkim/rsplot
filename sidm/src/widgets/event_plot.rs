//! `SidmEventPlot` — scalar pairs extracted from an event array, accumulated as
//! XY markers.
//!
//! Ports `pydm/widgets/eventplot.py` (`PyDMEventPlot` + `EventPlotCurveItem`)
//! onto a `siplot` [`Plot1D`] scatter item. Unlike
//! [`SidmScatterPlot`](crate::widgets::SidmScatterPlot) (which
//! pairs two scalar channels), each event curve subscribes to **one** array
//! channel: every update delivers an event array, and a fixed `(x_idx, y_idx)`
//! pair selects the `(x, y)` sample to append to a capacity-bounded
//! [`TimeSeriesBuffer`] (PyDM `receiveValue` rolling the `(2, bufferSize)`
//! array). When either index is out of range for the array, the update is
//! ignored (PyDM `len(new_data) <= idx → return`).
//!
//! The index selection ([`event_sample`]) and the array extraction (the shared
//! [`value_to_waveform`]) are pure and unit-tested; the GPU rendering is
//! exercised by a headless wgpu readback test that drives a real `loc://` event
//! array through the widget.

use siplot::egui::Color32;
use siplot::egui_wgpu::RenderState;
use siplot::{DataMargins, ItemHandle, Plot1D, PlotId, PlotResponse, Symbol, egui};

use crate::channel::{Channel, ValueEvent, ValueSubscription};
use crate::engine::{Engine, EngineError};
use crate::widgets::plot_menu::{
    YAxisMenu, enable_y_autoscale, set_y_range, show_with_y_axis_menu,
};
use crate::widgets::plot_style::{CurveStyle, DEFAULT_SYMBOL_SIZE, ensure_axis_autoscale};
use crate::widgets::ring_buffer::{DEFAULT_BUFFER_SIZE, TimeSeriesBuffer};
use crate::widgets::waveform_plot::value_to_waveform;

/// Select the `(x, y)` sample from an event array at `(x_idx, y_idx)`, or `None`
/// when either index is out of range (PyDM `receiveValue`: `len <= idx` skips the
/// update). `x_idx == y_idx` is allowed (both coordinates read the same element).
pub fn event_sample(wave: &[f64], x_idx: usize, y_idx: usize) -> Option<(f64, f64)> {
    match (wave.get(x_idx), wave.get(y_idx)) {
        (Some(&x), Some(&y)) => Some((x, y)),
        _ => None,
    }
}

/// One event curve: an array channel (held to keep the connection alive), its
/// value-event subscription, the `(x_idx, y_idx)` selectors, and the accumulated
/// `(x, y)` buffer.
struct EventCurve {
    channel: Channel,
    subscription: ValueSubscription,
    x_idx: usize,
    y_idx: usize,
    handle: ItemHandle,
    style: CurveStyle,
    buffer: TimeSeriesBuffer,
    /// Reusable render buffers.
    xs: Vec<f64>,
    ys: Vec<f64>,
}

impl EventCurve {
    /// Redraw the markers from the current buffer.
    fn redraw(&mut self, plot: &mut Plot1D) {
        self.buffer.ordered_into(&mut self.xs, &mut self.ys);
        plot.update_curve_spec(self.handle, self.style.to_spec(&self.xs, &self.ys));
    }
}

/// Select the `(x, y)` sample at `(x_idx, y_idx)` from one event's array value
/// and append it to `buffer`; returns `true` when a sample was appended. Each
/// [`ValueEvent`] is one monitor callback (PyDM `receiveValue`), so draining a
/// burst that arrived between two frames appends one point per event — no
/// coalescing. A non-array value or an out-of-range index is skipped (PyDM
/// `len(new_data) <= idx → return`).
fn ingest_event(
    buffer: &mut TimeSeriesBuffer,
    x_idx: usize,
    y_idx: usize,
    event: &ValueEvent,
) -> bool {
    if let Some(wave) = value_to_waveform(&event.value)
        && let Some((x, y)) = event_sample(&wave, x_idx, y_idx)
    {
        buffer.push(x, y);
        return true;
    }
    false
}

/// A plot accumulating `(x, y)` pairs selected from a single event array PV
/// (PyDM `PyDMEventPlot`).
pub struct SidmEventPlot {
    plot: Plot1D,
    curves: Vec<EventCurve>,
    buffer_size: usize,
    /// State for the pyqtgraph-style Y-axis context menu (auto-scale + range).
    y_menu: YAxisMenu,
}

impl SidmEventPlot {
    /// Create an empty event plot on the given GPU `render_state` and plot `id`.
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

    /// The channel backing curve `index`, if any.
    pub fn channel(&self, index: usize) -> Option<&Channel> {
        self.curves.get(index).map(|c| &c.channel)
    }

    /// Number of `(x, y)` points accumulated for curve `index` (PyDM
    /// `points_accumulated`), or `None` for an out-of-range index.
    pub fn point_count(&self, index: usize) -> Option<usize> {
        self.curves.get(index).map(|c| c.buffer.len())
    }

    /// Connect `address` (an event array PV) and add a curve selecting the
    /// `(x_idx, y_idx)` sample from each update, drawn as markers in `color`.
    /// Returns the new curve's index.
    pub fn add_channel(
        &mut self,
        engine: &Engine,
        address: &str,
        x_idx: usize,
        y_idx: usize,
        color: Color32,
        legend: impl Into<String>,
    ) -> Result<usize, EngineError> {
        let channel = engine.connect(address)?;
        // Subscribe to the value-event stream so every event array is ingested
        // (not a per-frame snapshot poll): a burst between frames is preserved,
        // and a hidden tab keeps accumulating up to the queue bound.
        let subscription = channel.subscribe_values(self.buffer_size);
        let handle =
            self.plot
                .add_scatter_with_symbol(&[], &[], color, Symbol::Circle, DEFAULT_SYMBOL_SIZE);
        self.plot.set_item_legend(handle, legend);
        self.curves.push(EventCurve {
            channel,
            subscription,
            x_idx,
            y_idx,
            handle,
            style: CurveStyle::markers(color),
            buffer: TimeSeriesBuffer::new(self.buffer_size),
            xs: Vec::new(),
            ys: Vec::new(),
        });
        Ok(self.curves.len() - 1)
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
        self.curves[index].buffer.push(x, y);
        self.curves[index].redraw(&mut self.plot);
        true
    }

    /// Drain every channel's value-event queue, append each event's selected
    /// `(x, y)` sample, redraw the curves that changed, and render the plot this
    /// frame.
    ///
    /// The queue is filled by the engine independent of repaint, so an event
    /// plot on an inactive tab keeps accumulating (up to the queue bound) and
    /// renders its recent history when shown again; a burst arriving between two
    /// frames is appended event-by-event rather than coalesced (PyDM
    /// `receiveValue` fires once per monitor callback).
    pub fn show(&mut self, ui: &mut egui::Ui) -> PlotResponse {
        for curve in &mut self.curves {
            let buffer = &mut curve.buffer;
            let (x_idx, y_idx) = (curve.x_idx, curve.y_idx);
            let mut changed = false;
            curve.subscription.drain(|event| {
                if ingest_event(buffer, x_idx, y_idx, &event) {
                    changed = true;
                }
            });
            if changed {
                curve.redraw(&mut self.plot);
            }
        }
        ui.ctx().request_repaint();
        show_with_y_axis_menu(&mut self.plot, &mut self.y_menu, ui)
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
    use crate::channel::PvValue;
    use std::sync::Arc;
    use std::time::UNIX_EPOCH;

    /// A value event carrying `value`; the event plot keys its buffer on the
    /// selected `(x, y)`, not on the event time, so a fixed time suffices.
    fn event(value: PvValue) -> ValueEvent {
        ValueEvent {
            value,
            time: UNIX_EPOCH,
        }
    }

    #[test]
    fn event_sample_selects_indices_in_range() {
        let wave = [10.0, 20.0, 30.0];
        assert_eq!(event_sample(&wave, 0, 1), Some((10.0, 20.0)));
        assert_eq!(event_sample(&wave, 2, 0), Some((30.0, 10.0)));
        // Same index for both coordinates is allowed.
        assert_eq!(event_sample(&wave, 1, 1), Some((20.0, 20.0)));
    }

    #[test]
    fn event_sample_rejects_out_of_range_indices() {
        let wave = [10.0, 20.0];
        assert_eq!(event_sample(&wave, 2, 0), None);
        assert_eq!(event_sample(&wave, 0, 5), None);
        assert_eq!(event_sample(&[], 0, 0), None);
    }

    #[test]
    fn ingest_event_appends_one_sample_per_event_no_coalescing() {
        let mut buffer = TimeSeriesBuffer::new(8);
        for wave in [[1.0, 2.0], [3.0, 4.0], [5.0, 6.0]] {
            assert!(ingest_event(
                &mut buffer,
                0,
                1,
                &event(PvValue::FloatArray(Arc::from(wave.as_slice()))),
            ));
        }
        // Three event arrays between two frames → three accumulated points (a
        // per-frame snapshot poll would have kept only the last array's sample).
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.newest(), Some((5.0, 6.0)));
    }

    #[test]
    fn ingest_event_skips_out_of_range_or_non_array() {
        let mut buffer = TimeSeriesBuffer::new(8);
        // y_idx out of range for a 2-element array.
        assert!(!ingest_event(
            &mut buffer,
            0,
            5,
            &event(PvValue::FloatArray(Arc::from([1.0, 2.0].as_slice()))),
        ));
        // A non-numeric value yields no waveform.
        assert!(!ingest_event(
            &mut buffer,
            0,
            0,
            &event(PvValue::Str("x".into()))
        ));
        assert!(buffer.is_empty());
    }
}
