//! `PydmTimePlot` — scalar-versus-time strip chart over one or more channels.
//!
//! Ports `pydm/widgets/timeplot.py` (`PyDMTimePlot` + `TimePlotCurveItem`) onto
//! a `siplot` [`Plot1D`] in time-series tick mode. Each channel is a curve
//! backed by a [`TimeSeriesBuffer`]; on every frame the widget reads each
//! channel's snapshot, appends a `(time, value)` sample under the configured
//! [`UpdateMode`], redraws the changed curves, and scrolls the X window to
//! `[now - time_span, now]` (`updateXAxis`).
//!
//! Two update modes mirror PyDM's:
//! - [`UpdateMode::OnValueChange`] (`receiveNewValue`) — append when a new
//!   snapshot arrives (detected via the monotonic `stamp`).
//! - [`UpdateMode::AtFixedRate`] (`asyncUpdate` driven by a `QTimer`) — append
//!   the latest value at a fixed rate (PyDM `DEFAULT_UPDATE_INTERVAL`).
//!
//! Snapshot-model deviations from PyDM, documented honestly: the per-frame poll
//! coalesces updates that arrive faster than the frame rate, the sample
//! timestamp is the frame's wall clock at detection (PyDM uses `time.time()` in
//! the value callback — the same clock, but the snapshot model samples it one
//! frame later), and `OnValueChange` keys off the `stamp` so a reconnect (which
//! also bumps the stamp) yields one sample.
//!
//! X axis is **relative time** (seconds since the plot was created), not the
//! absolute datetime axis PyDM uses (`TickMode::TimeSeries`). This is a
//! deliberate, documented limitation: siplot's GPU curve path uploads vertices
//! as `f32` and its ortho matrix (`core/transform.rs`) is `f32`, so an
//! absolute epoch X (~`1.7e9`) collapses under catastrophic cancellation — a
//! five-second window is far below the `f32` ULP at that magnitude and no curve
//! renders. Storing absolute epochs in the buffer but feeding siplot
//! `t - t0` keeps the GPU coordinates small (and matches PyDM's documented
//! "plot by relative time" mode). Restoring an absolute datetime axis needs a
//! siplot-side `f64` vertex rebase (out of scope for this port).
//!
//! The sample-feeding logic ([`CurveFeed`]) and the fixed-rate timing
//! ([`is_rate_due`] / [`update_interval`]) are pure and unit-tested; the GPU
//! rendering is exercised by a headless wgpu readback test.

use std::time::{SystemTime, UNIX_EPOCH};

use siplot::egui::Color32;
use siplot::egui_wgpu::RenderState;
use siplot::{ItemHandle, Plot1D, PlotId, PlotResponse, YAxis, egui};

use crate::channel::{Channel, ChannelState, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::plot_style::CurveStyle;
use crate::widgets::ring_buffer::{DEFAULT_BUFFER_SIZE, TimeSeriesBuffer};

/// PyDM `DEFAULT_TIME_SPAN`: the X window width in seconds.
pub const DEFAULT_TIME_SPAN: f64 = 5.0;
/// PyDM `DEFAULT_UPDATE_INTERVAL` (1000 ms) expressed as a rate in Hz.
pub const DEFAULT_UPDATE_RATE_HZ: f64 = 1.0;

/// How a curve appends samples (PyDM `updateMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateMode {
    /// Append a sample whenever a new value arrives (PyDM
    /// `updateMode.OnValueChange`).
    OnValueChange,
    /// Append the latest value at a fixed rate (PyDM `updateMode.AtFixedRate`).
    AtFixedRate,
}

/// Seconds between fixed-rate samples for `rate_hz` (PyDM `update_interval`).
/// A non-positive rate yields an infinite interval (never due).
pub fn update_interval(rate_hz: f64) -> f64 {
    if rate_hz > 0.0 {
        1.0 / rate_hz
    } else {
        f64::INFINITY
    }
}

/// Whether a fixed-rate sample is due at `now` given the last push time and the
/// interval between samples.
pub fn is_rate_due(now: f64, last_push: f64, interval: f64) -> bool {
    now - last_push >= interval
}

/// Wall-clock POSIX seconds (PyDM `time.time()`), or `0.0` before the epoch.
fn now_epoch_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The pure sample-feeding state for one curve: its buffer plus the bookkeeping
/// each [`UpdateMode`] needs. Appends a `(time, value)` sample per
/// [`CurveFeed::ingest`] and reports whether the buffer changed.
struct CurveFeed {
    buffer: TimeSeriesBuffer,
    /// Last observed snapshot stamp (PyDM value-change detection).
    last_stamp: u64,
    /// Most recent numeric value, held for the fixed-rate push (PyDM
    /// `latest_value`).
    latest_value: Option<f64>,
}

impl CurveFeed {
    fn new(buffer_size: usize) -> Self {
        Self {
            buffer: TimeSeriesBuffer::new(buffer_size),
            last_stamp: 0,
            latest_value: None,
        }
    }

    /// Feed the current channel snapshot at time `now`. Returns `true` when a
    /// sample was appended (the buffer changed and the curve needs a redraw).
    fn ingest(&mut self, now: f64, state: &ChannelState, mode: UpdateMode, rate_due: bool) -> bool {
        let value = state.value.as_ref().and_then(PvValue::as_f64);
        match mode {
            UpdateMode::OnValueChange => {
                // A new snapshot (only while connected — a disconnect bumps the
                // stamp but carries a stale value).
                if state.connected && state.stamp != self.last_stamp {
                    self.last_stamp = state.stamp;
                    if let Some(v) = value {
                        self.buffer.push(now, v);
                        return true;
                    }
                }
                false
            }
            UpdateMode::AtFixedRate => {
                if let Some(v) = value {
                    self.latest_value = Some(v);
                }
                if rate_due && let Some(v) = self.latest_value {
                    self.buffer.push(now, v);
                    return true;
                }
                false
            }
        }
    }
}

/// One channel-driven curve: its channel handle, its feed, and the siplot item
/// handle plus reusable scratch buffers for the render feed.
struct TimeCurve {
    channel: Channel,
    feed: CurveFeed,
    handle: ItemHandle,
    style: CurveStyle,
    xs: Vec<f64>,
    ys: Vec<f64>,
}

/// Redraw `curve` from its buffer, feeding siplot relative-time X (`t - t0`) so
/// the GPU coordinates stay small (see the module docs on `f32` precision). The
/// buffer keeps absolute epoch timestamps; only the render feed is offset.
fn redraw_curve(plot: &mut Plot1D, curve: &mut TimeCurve, t0: f64) {
    curve.feed.buffer.ordered_into(&mut curve.xs, &mut curve.ys);
    for x in &mut curve.xs {
        *x -= t0;
    }
    plot.update_curve_spec(curve.handle, curve.style.to_spec(&curve.xs, &curve.ys));
}

/// A scrolling strip chart of scalar PVs versus time (PyDM `PyDMTimePlot`).
pub struct PydmTimePlot {
    plot: Plot1D,
    curves: Vec<TimeCurve>,
    update_mode: UpdateMode,
    update_rate_hz: f64,
    time_span: f64,
    buffer_size: usize,
    /// Wall-clock time of the last fixed-rate push (`-inf` so the first frame
    /// samples immediately).
    last_fixed_push: f64,
    /// Reference epoch (creation time): the X feed to siplot is `t - t0` to keep
    /// GPU coordinates small (see the module docs).
    t0: f64,
}

impl PydmTimePlot {
    /// Create an empty time plot on the given GPU `render_state` and plot `id`.
    /// The Y axis autoscales; the X axis is relative time driven by the scroll
    /// window.
    pub fn new(render_state: &RenderState, id: PlotId) -> Self {
        let mut plot = Plot1D::new(render_state, id);
        plot.plot_mut().set_x_autoscale(false);
        plot.plot_mut().set_y_autoscale(true);
        plot.set_auto_reset_zoom(false);
        plot.set_graph_x_label("Time since start (s)");
        Self {
            plot,
            curves: Vec::new(),
            update_mode: UpdateMode::OnValueChange,
            update_rate_hz: DEFAULT_UPDATE_RATE_HZ,
            time_span: DEFAULT_TIME_SPAN,
            buffer_size: DEFAULT_BUFFER_SIZE,
            last_fixed_push: f64::NEG_INFINITY,
            t0: now_epoch_secs(),
        }
    }

    /// Set the update mode for new and existing curves (builder style).
    pub fn with_update_mode(mut self, mode: UpdateMode) -> Self {
        self.update_mode = mode;
        self
    }

    /// Set the fixed-rate sample rate in Hz (builder style; PyDM
    /// `updateInterval`).
    pub fn with_update_rate_hz(mut self, rate_hz: f64) -> Self {
        self.update_rate_hz = rate_hz;
        self
    }

    /// Set the X window width in seconds (builder style; PyDM `timeSpan`).
    pub fn with_time_span(mut self, time_span: f64) -> Self {
        self.time_span = time_span;
        self
    }

    /// Set the per-curve buffer capacity for curves added afterwards (builder
    /// style; PyDM `bufferSize`).
    pub fn with_buffer_size(mut self, buffer_size: usize) -> Self {
        self.buffer_size = buffer_size;
        self
    }

    /// The underlying plot, for styling (title, Y label, grid, legend).
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

    /// Connect `address` and add it as a curve in `color` with the given legend.
    /// Returns the new curve's index.
    pub fn add_channel(
        &mut self,
        engine: &Engine,
        address: &str,
        color: Color32,
        legend: impl Into<String>,
    ) -> Result<usize, EngineError> {
        let channel = engine.connect(address)?;
        let handle = self.plot.add_curve_with_legend(&[], &[], color, legend);
        self.curves.push(TimeCurve {
            channel,
            feed: CurveFeed::new(self.buffer_size),
            handle,
            style: CurveStyle::line(color),
            xs: Vec::new(),
            ys: Vec::new(),
        });
        Ok(self.curves.len() - 1)
    }

    /// Restyle curve `index` (PyDM `BasePlotCurveItem` properties: colour, line
    /// style/width, symbol, Y axis) and re-draw it immediately. Assigning the
    /// curve to [`YAxis::Right`] enables the right (y2) axis' autoscale so it gets
    /// its own scale. Returns `false` for an out-of-range index.
    pub fn set_curve_style(&mut self, index: usize, style: CurveStyle) -> bool {
        if index >= self.curves.len() {
            return false;
        }
        let right = style.y_axis == YAxis::Right;
        self.curves[index].style = style;
        if right {
            self.plot.plot_mut().set_y2_autoscale(true);
        }
        let t0 = self.t0;
        redraw_curve(&mut self.plot, &mut self.curves[index], t0);
        true
    }

    /// Inject a `(time, value)` sample (absolute epoch seconds) directly into
    /// curve `index` and redraw it (PyDM "you can call it yourself to inject data
    /// into the curve" — backfill / replay). Returns `false` for an out-of-range
    /// index.
    pub fn inject(&mut self, index: usize, time: f64, value: f64) -> bool {
        if index >= self.curves.len() {
            return false;
        }
        self.curves[index].feed.buffer.push(time, value);
        let t0 = self.t0;
        redraw_curve(&mut self.plot, &mut self.curves[index], t0);
        true
    }

    /// Poll every channel, append samples, redraw changed curves, scroll the X
    /// window, and render the plot this frame.
    pub fn show(&mut self, ui: &mut egui::Ui) -> PlotResponse {
        let now = now_epoch_secs();
        let mode = self.update_mode;
        let t0 = self.t0;
        let interval = update_interval(self.update_rate_hz);
        let rate_due =
            mode == UpdateMode::AtFixedRate && is_rate_due(now, self.last_fixed_push, interval);
        if rate_due {
            self.last_fixed_push = now;
        }

        for curve in &mut self.curves {
            let state = curve.channel.state();
            if curve.feed.ingest(now, &state, mode, rate_due) {
                redraw_curve(&mut self.plot, curve, t0);
            }
        }

        // Relative-time scroll window: [now - t0 - span, now - t0].
        let right = now - t0;
        self.plot.set_graph_x_limits(right - self.time_span, right);
        // A strip chart animates: keep frames coming even between channel updates
        // so the X window scrolls smoothly.
        ui.ctx().request_repaint();
        self.plot.show(ui)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(connected: bool, stamp: u64, value: Option<PvValue>) -> ChannelState {
        ChannelState {
            connected,
            value,
            stamp,
            ..ChannelState::default()
        }
    }

    #[test]
    fn update_interval_is_inverse_rate_and_guards_zero() {
        assert_eq!(update_interval(1.0), 1.0);
        assert_eq!(update_interval(2.0), 0.5);
        assert_eq!(update_interval(0.0), f64::INFINITY);
        assert_eq!(update_interval(-3.0), f64::INFINITY);
    }

    #[test]
    fn is_rate_due_compares_elapsed_against_interval() {
        assert!(is_rate_due(10.0, 9.0, 1.0));
        assert!(is_rate_due(10.0, 9.0, 0.5));
        assert!(!is_rate_due(10.0, 9.5, 1.0));
        // First push: last = -inf is always due.
        assert!(is_rate_due(0.0, f64::NEG_INFINITY, 1.0));
    }

    #[test]
    fn on_value_change_appends_on_new_stamp_with_numeric_value() {
        let mut feed = CurveFeed::new(8);
        // First connected snapshot with a value appends.
        assert!(feed.ingest(
            1.0,
            &state(true, 1, Some(PvValue::Float(5.0))),
            UpdateMode::OnValueChange,
            false
        ));
        assert_eq!(feed.buffer.newest(), Some((1.0, 5.0)));
        // Same stamp: no append.
        assert!(!feed.ingest(
            2.0,
            &state(true, 1, Some(PvValue::Float(5.0))),
            UpdateMode::OnValueChange,
            false
        ));
        // New stamp: appends with the new time.
        assert!(feed.ingest(
            3.0,
            &state(true, 2, Some(PvValue::Float(6.0))),
            UpdateMode::OnValueChange,
            false
        ));
        assert_eq!(feed.buffer.newest(), Some((3.0, 6.0)));
        assert_eq!(feed.buffer.len(), 2);
    }

    #[test]
    fn on_value_change_skips_disconnected_and_non_numeric() {
        let mut feed = CurveFeed::new(8);
        // Disconnected: no append even with a value and a new stamp.
        assert!(!feed.ingest(
            1.0,
            &state(false, 1, Some(PvValue::Float(5.0))),
            UpdateMode::OnValueChange,
            false
        ));
        // Connected but non-numeric: stamp advances, but nothing is plotted.
        assert!(!feed.ingest(
            2.0,
            &state(true, 2, Some(PvValue::Str("x".into()))),
            UpdateMode::OnValueChange,
            false
        ));
        assert!(feed.buffer.is_empty());
        // A later numeric value at the same advanced stamp does NOT re-append
        // (stamp already consumed) — only a fresh stamp does.
        assert!(!feed.ingest(
            3.0,
            &state(true, 2, Some(PvValue::Float(9.0))),
            UpdateMode::OnValueChange,
            false
        ));
        assert!(feed.buffer.is_empty());
    }

    #[test]
    fn at_fixed_rate_appends_latest_value_only_when_due() {
        let mut feed = CurveFeed::new(8);
        // Not due: tracks latest, appends nothing.
        assert!(!feed.ingest(
            1.0,
            &state(true, 1, Some(PvValue::Float(5.0))),
            UpdateMode::AtFixedRate,
            false
        ));
        assert!(feed.buffer.is_empty());
        // A newer value updates the tracked latest.
        assert!(!feed.ingest(
            1.5,
            &state(true, 2, Some(PvValue::Float(7.0))),
            UpdateMode::AtFixedRate,
            false
        ));
        // Due: appends the latest tracked value at the current time.
        assert!(feed.ingest(
            2.0,
            &state(true, 2, Some(PvValue::Float(7.0))),
            UpdateMode::AtFixedRate,
            true
        ));
        assert_eq!(feed.buffer.newest(), Some((2.0, 7.0)));
        assert_eq!(feed.buffer.len(), 1);
    }
}
