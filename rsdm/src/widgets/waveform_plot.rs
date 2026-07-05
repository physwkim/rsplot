//! `RsdmWaveformPlot` — array-channel curves (Y versus X or index).
//!
//! Ports `pydm/widgets/waveformplot.py` (`PyDMWaveformPlot` +
//! `WaveformCurveItem`) onto a `rsplot` [`Plot1D`]. Each curve has a Y array
//! channel and an optional X array channel; when the X and Y lengths differ the
//! longer is truncated (PyDM `redrawCurve`), and with no X channel the Y array is
//! plotted against its sample index. The [`RedrawMode`] decides which channel's
//! arrival triggers a redraw (PyDM `redraw_mode` / `updateData`).
//!
//! The redraw gate ([`mode_allows`]) and the array extraction
//! ([`value_to_waveform`]) are pure and unit-tested; the GPU rendering is
//! exercised by a headless wgpu readback test.

use rsplot::egui::Color32;
use rsplot::egui_wgpu::RenderState;
use rsplot::{DataMargins, ItemHandle, Plot1D, PlotId, PlotResponse, YAxis, egui};

use crate::channel::{Channel, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::middle_click_copy;
use crate::widgets::plot_menu::{
    YAxisMenu, enable_y_autoscale, set_x_range, set_y_range, show_with_y_axis_menu,
};
use crate::widgets::plot_style::{CurveStyle, ensure_axis_autoscale};

/// When a waveform/scatter curve redraws relative to its X/Y channels (PyDM
/// `redraw_mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RedrawMode {
    /// Redraw after either X or Y receives new data (PyDM default).
    #[default]
    OnEither,
    /// Redraw only after X receives new data.
    OnX,
    /// Redraw only after Y receives new data.
    OnY,
    /// Redraw only after both X and Y receive new data since the last redraw.
    OnBoth,
}

/// Whether `mode` permits a redraw given whether new X / Y data has arrived since
/// the last redraw (PyDM `updateData` / `update_buffer` gating; `pending_*` is
/// the inverse of PyDM's `needs_new_*`). With no X channel `pending_x` stays
/// `false`, so `OnX` / `OnBoth` never fire — matching PyDM, where `needs_new_x`
/// never clears without an X channel.
pub fn mode_allows(mode: RedrawMode, pending_x: bool, pending_y: bool) -> bool {
    match mode {
        RedrawMode::OnEither => pending_x || pending_y,
        RedrawMode::OnX => pending_x,
        RedrawMode::OnY => pending_y,
        RedrawMode::OnBoth => pending_x && pending_y,
    }
}

/// Extract a waveform (array of `f64`) from a channel value: float/int arrays
/// convert element-wise; a scalar numeric value becomes a one-element waveform;
/// anything else (string/array-of-string/bytes) yields `None`.
pub fn value_to_waveform(value: &PvValue) -> Option<Vec<f64>> {
    match value {
        PvValue::FloatArray(a) => Some(a.to_vec()),
        PvValue::IntArray(a) => Some(a.iter().map(|&i| i as f64).collect()),
        PvValue::Int(_) | PvValue::Float(_) | PvValue::Bool(_) | PvValue::Enum { .. } => {
            value.as_f64().map(|v| vec![v])
        }
        PvValue::Str(_) | PvValue::StrArray(_) | PvValue::Bytes(_) => None,
    }
}

/// One waveform curve: a Y array channel, an optional X array channel, and the
/// arrival/commit bookkeeping the [`RedrawMode`] needs.
struct WaveformCurve {
    y_channel: Channel,
    x_channel: Option<Channel>,
    handle: ItemHandle,
    style: CurveStyle,
    mode: RedrawMode,
    last_y_stamp: u64,
    last_x_stamp: u64,
    /// New data arrived since the last redraw (inverse of PyDM `needs_new_*`).
    pending_x: bool,
    pending_y: bool,
    latest_x: Option<Vec<f64>>,
    latest_y: Option<Vec<f64>>,
    /// Reusable render buffers (committed, length-aligned data).
    xs: Vec<f64>,
    ys: Vec<f64>,
}

impl WaveformCurve {
    /// Read both channels, recording new arrivals (PyDM `receiveXWaveform` /
    /// `receiveYWaveform`).
    fn poll(&mut self) {
        let ys = self.y_channel.state();
        if ys.connected && ys.stamp != self.last_y_stamp {
            self.last_y_stamp = ys.stamp;
            if let Some(w) = ys.value.as_ref().and_then(value_to_waveform) {
                self.latest_y = Some(w);
                self.pending_y = true;
            }
        }
        if let Some(xc) = &self.x_channel {
            let xs = xc.state();
            if xs.connected && xs.stamp != self.last_x_stamp {
                self.last_x_stamp = xs.stamp;
                if let Some(w) = xs.value.as_ref().and_then(value_to_waveform) {
                    self.latest_x = Some(w);
                    self.pending_x = true;
                }
            }
        }
    }

    /// Whether this curve should redraw now: it has a Y waveform and the redraw
    /// mode is satisfied (PyDM `updateData`).
    fn ready(&self) -> bool {
        self.latest_y.is_some() && mode_allows(self.mode, self.pending_x, self.pending_y)
    }

    /// Commit the latest waveforms to the plot, length-aligning X and Y and using
    /// the sample index for X when there is no X channel (PyDM `redrawCurve`).
    fn commit(&mut self, plot: &mut Plot1D) {
        let Some(y) = &self.latest_y else {
            return;
        };
        self.xs.clear();
        self.ys.clear();
        match &self.latest_x {
            Some(x) => {
                let n = x.len().min(y.len());
                self.xs.extend_from_slice(&x[..n]);
                self.ys.extend_from_slice(&y[..n]);
            }
            None => {
                self.ys.extend_from_slice(y);
                self.xs.extend((0..y.len()).map(|i| i as f64));
            }
        }
        plot.update_curve_spec(self.handle, self.style.to_spec(&self.xs, &self.ys));
        self.pending_x = false;
        self.pending_y = false;
    }

    /// Re-spec the curve from the already-committed data with the current style
    /// (used when the style changes between data updates).
    fn restyle(&self, plot: &mut Plot1D) {
        plot.update_curve_spec(self.handle, self.style.to_spec(&self.xs, &self.ys));
    }
}

/// A plot of array-valued PVs: Y versus X, or Y versus index (PyDM
/// `PyDMWaveformPlot`).
pub struct RsdmWaveformPlot {
    plot: Plot1D,
    curves: Vec<WaveformCurve>,
    /// State for the pyqtgraph-style Y-axis context menu (auto-scale + range).
    y_menu: YAxisMenu,
}

impl RsdmWaveformPlot {
    /// Create an empty waveform plot on the given GPU `render_state` and plot
    /// `id`.
    pub fn new(render_state: &RenderState, id: PlotId) -> Self {
        Self {
            plot: Plot1D::new(render_state, id),
            curves: Vec::new(),
            y_menu: YAxisMenu::new(),
        }
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

    /// Add a Y array channel as a curve (X is the sample index). Returns the new
    /// curve's index.
    pub fn add_channel(
        &mut self,
        engine: &Engine,
        y_address: &str,
        color: Color32,
        legend: impl Into<String>,
    ) -> Result<usize, EngineError> {
        self.add_xy_channel(engine, y_address, None, color, legend)
    }

    /// Add a Y array channel plotted against an X array channel. Returns the new
    /// curve's index.
    pub fn add_xy_channel(
        &mut self,
        engine: &Engine,
        y_address: &str,
        x_address: Option<&str>,
        color: Color32,
        legend: impl Into<String>,
    ) -> Result<usize, EngineError> {
        let y_channel = engine.connect(y_address)?;
        let x_channel = match x_address {
            Some(addr) => Some(engine.connect(addr)?),
            None => None,
        };
        let handle = self.plot.add_curve_with_legend(&[], &[], color, legend);
        self.curves.push(WaveformCurve {
            y_channel,
            x_channel,
            handle,
            style: CurveStyle::line(color),
            mode: RedrawMode::default(),
            last_y_stamp: 0,
            last_x_stamp: 0,
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

    /// Restyle curve `index` (PyDM `BasePlotCurveItem` properties: colour, line
    /// style/width, symbol, Y axis) and re-draw it immediately. Assigning the
    /// curve to a secondary axis ([`YAxis::Right`](rsplot::YAxis::Right) or an
    /// [`YAxis::Extra`](rsplot::YAxis::Extra) stacked axis) enables that axis'
    /// autoscale. Returns `false` for an out-of-range index.
    pub fn set_curve_style(&mut self, index: usize, style: CurveStyle) -> bool {
        if index >= self.curves.len() {
            return false;
        }
        let axis = style.y_axis;
        self.curves[index].style = style;
        ensure_axis_autoscale(&mut self.plot, axis);
        self.curves[index].restyle(&mut self.plot);
        true
    }

    /// Poll every channel, redraw the curves whose redraw mode is satisfied, and
    /// render the plot this frame.
    pub fn show(&mut self, ui: &mut egui::Ui) -> PlotResponse {
        for curve in &mut self.curves {
            curve.poll();
            if curve.ready() {
                curve.commit(&mut self.plot);
            }
        }
        ui.ctx().request_repaint();
        let response = show_with_y_axis_menu(&mut self.plot, &mut self.y_menu, ui);
        // MEDM Btn2 copies every record the plot carries (trace Y then X, PyDM channels() order).
        middle_click_copy(
            ui,
            &response.response,
            self.curves.iter().flat_map(|c| {
                std::iter::once(c.y_channel.address().raw())
                    .chain(c.x_channel.as_ref().map(|x| x.address().raw()))
            }),
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
    use std::sync::Arc;

    #[test]
    fn mode_allows_matches_pydm_gating() {
        // OnEither: any arrival.
        assert!(mode_allows(RedrawMode::OnEither, true, false));
        assert!(mode_allows(RedrawMode::OnEither, false, true));
        assert!(!mode_allows(RedrawMode::OnEither, false, false));
        // OnX / OnY: only the named channel.
        assert!(mode_allows(RedrawMode::OnX, true, false));
        assert!(!mode_allows(RedrawMode::OnX, false, true));
        assert!(mode_allows(RedrawMode::OnY, false, true));
        assert!(!mode_allows(RedrawMode::OnY, true, false));
        // OnBoth: needs both.
        assert!(mode_allows(RedrawMode::OnBoth, true, true));
        assert!(!mode_allows(RedrawMode::OnBoth, true, false));
        assert!(!mode_allows(RedrawMode::OnBoth, false, true));
    }

    #[test]
    fn value_to_waveform_converts_arrays_and_scalars() {
        assert_eq!(
            value_to_waveform(&PvValue::FloatArray(Arc::from([1.0, 2.0, 3.0]))),
            Some(vec![1.0, 2.0, 3.0])
        );
        assert_eq!(
            value_to_waveform(&PvValue::IntArray(Arc::from([4_i64, 5, 6]))),
            Some(vec![4.0, 5.0, 6.0])
        );
        // A scalar becomes a single-point waveform.
        assert_eq!(value_to_waveform(&PvValue::Float(7.5)), Some(vec![7.5]));
        assert_eq!(value_to_waveform(&PvValue::Int(8)), Some(vec![8.0]));
        // Non-numeric kinds have no waveform.
        assert_eq!(value_to_waveform(&PvValue::Str("x".into())), None);
        assert_eq!(
            value_to_waveform(&PvValue::StrArray(Arc::from([String::from("a")]))),
            None
        );
    }
}
