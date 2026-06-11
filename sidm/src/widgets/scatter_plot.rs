//! `SidmScatterPlot` — paired scalar channels as accumulated XY markers.
//!
//! Ports `pydm/widgets/scatterplot.py` (`PyDMScatterPlot` +
//! `ScatterPlotCurveItem`) onto a `siplot` [`Plot1D`] scatter item. Each curve
//! pairs an X scalar channel with a Y scalar channel; when a new pair is ready
//! (per the [`RedrawMode`], and only once both channels have a value) the
//! `(x, y)` pair is appended to a capacity-bounded [`TimeSeriesBuffer`] and the
//! markers are redrawn (PyDM `update_buffer` rolling the `(2, bufferSize)`
//! array).
//!
//! The redraw gate is the shared [`mode_allows`]; the buffer is the shared
//! [`TimeSeriesBuffer`]; both are unit-tested purely. The GPU rendering is
//! exercised by a headless wgpu readback test.

use siplot::egui::Color32;
use siplot::egui_wgpu::RenderState;
use siplot::{DataMargins, ItemHandle, Plot1D, PlotId, PlotResponse, Symbol, egui};

use crate::channel::{Channel, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::plot_menu::{
    YAxisMenu, enable_y_autoscale, set_y_range, show_with_y_axis_menu,
};
use crate::widgets::plot_style::{CurveStyle, ensure_axis_autoscale};
use crate::widgets::ring_buffer::{DEFAULT_BUFFER_SIZE, TimeSeriesBuffer};
use crate::widgets::waveform_plot::{RedrawMode, mode_allows};

/// Default marker size in points; owned by [`crate::widgets::plot_style`] and
/// re-exported here for the established public path.
pub use crate::widgets::plot_style::DEFAULT_SYMBOL_SIZE;

/// One scatter curve: paired X/Y scalar channels, the accumulated `(x, y)`
/// buffer, and the arrival/commit bookkeeping the [`RedrawMode`] needs.
struct ScatterCurve {
    x_channel: Channel,
    y_channel: Channel,
    handle: ItemHandle,
    style: CurveStyle,
    mode: RedrawMode,
    buffer: TimeSeriesBuffer,
    last_x_stamp: u64,
    last_y_stamp: u64,
    /// New data arrived since the last redraw (inverse of PyDM `needs_new_*`).
    pending_x: bool,
    pending_y: bool,
    latest_x: Option<f64>,
    latest_y: Option<f64>,
    /// Reusable render buffers.
    xs: Vec<f64>,
    ys: Vec<f64>,
}

impl ScatterCurve {
    /// Read both scalar channels, recording new arrivals (PyDM `receiveXValue` /
    /// `receiveYValue`).
    fn poll(&mut self) {
        let xs = self.x_channel.state();
        if xs.connected && xs.stamp != self.last_x_stamp {
            self.last_x_stamp = xs.stamp;
            if let Some(v) = xs.value.as_ref().and_then(PvValue::as_f64) {
                self.latest_x = Some(v);
                self.pending_x = true;
            }
        }
        let ys = self.y_channel.state();
        if ys.connected && ys.stamp != self.last_y_stamp {
            self.last_y_stamp = ys.stamp;
            if let Some(v) = ys.value.as_ref().and_then(PvValue::as_f64) {
                self.latest_y = Some(v);
                self.pending_y = true;
            }
        }
    }

    /// Whether a new pair should be appended now: both channels have a value and
    /// the redraw mode is satisfied (PyDM `update_buffer`).
    fn ready(&self) -> bool {
        self.latest_x.is_some()
            && self.latest_y.is_some()
            && mode_allows(self.mode, self.pending_x, self.pending_y)
    }

    /// Append the latest `(x, y)` pair and redraw (PyDM `update_buffer` roll).
    fn commit(&mut self, plot: &mut Plot1D) {
        let (Some(x), Some(y)) = (self.latest_x, self.latest_y) else {
            return;
        };
        self.buffer.push(x, y);
        self.redraw(plot);
        self.pending_x = false;
        self.pending_y = false;
    }

    /// Redraw the markers from the current buffer.
    fn redraw(&mut self, plot: &mut Plot1D) {
        self.buffer.ordered_into(&mut self.xs, &mut self.ys);
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
        let handle =
            self.plot
                .add_scatter_with_symbol(&[], &[], color, Symbol::Circle, DEFAULT_SYMBOL_SIZE);
        self.plot.set_item_legend(handle, legend);
        self.curves.push(ScatterCurve {
            x_channel,
            y_channel,
            handle,
            style: CurveStyle::markers(color),
            mode: RedrawMode::default(),
            buffer: TimeSeriesBuffer::new(self.buffer_size),
            last_x_stamp: 0,
            last_y_stamp: 0,
            pending_x: false,
            pending_y: false,
            latest_x: None,
            latest_y: None,
            xs: Vec::new(),
            ys: Vec::new(),
        });
        Ok(self.curves.len() - 1)
    }

    /// Set the redraw mode of curve `index` (PyDM `redraw_mode`). No-op for an
    /// out-of-range index.
    pub fn set_redraw_mode(&mut self, index: usize, mode: RedrawMode) {
        if let Some(curve) = self.curves.get_mut(index) {
            curve.mode = mode;
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
        self.curves[index].buffer.push(x, y);
        let curve = &mut self.curves[index];
        curve.redraw(&mut self.plot);
        true
    }

    /// Poll every channel, append pairs whose redraw mode is satisfied, and
    /// render the plot this frame.
    pub fn show(&mut self, ui: &mut egui::Ui) -> PlotResponse {
        for curve in &mut self.curves {
            curve.poll();
            if curve.ready() {
                curve.commit(&mut self.plot);
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
