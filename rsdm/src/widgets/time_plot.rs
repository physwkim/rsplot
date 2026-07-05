//! `RsdmTimePlot` — scalar-versus-time strip chart over one or more channels.
//!
//! Ports `pydm/widgets/timeplot.py` (`PyDMTimePlot` + `TimePlotCurveItem`) onto
//! a `rsplot` [`Plot1D`] in time-series tick mode. Each channel is a curve
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
//! X axis defaults to **relative time** (seconds since the plot was created)
//! and can switch to the absolute datetime axis PyDM uses
//! ([`TimeAxisMode::WallClock`]). The relative default exists because rsplot's
//! GPU curve path uploads vertices as `f32` and its ortho matrix
//! (`core/transform.rs`) is `f32`, so an absolute epoch X (~`1.7e9`) collapses
//! under catastrophic cancellation — a five-second window is far below the
//! `f32` ULP at that magnitude and no curve renders. The buffer stores absolute
//! epochs but feeds rsplot `t - t0`, keeping the GPU coordinates small (and
//! matching PyDM's documented "plot by relative time" mode). The wall-clock
//! axis reuses those same `f32`-safe relative vertices and only offsets the
//! *tick labels* back to absolute time (rsplot `set_x_time_offset`), so it needs
//! no `f64` vertex rebase — rsplot lays out date-time ticks over the epoch
//! window `[min + t0, max + t0]` and shifts each tick position back by `t0` onto
//! the relative vertices. See [`TimeAxisMode`].
//!
//! The sample-feeding logic (`CurveFeed`) and the fixed-rate timing
//! ([`is_rate_due`] / [`update_interval`]) are pure and unit-tested; the GPU
//! rendering is exercised by a headless wgpu readback test.

use std::time::{SystemTime, UNIX_EPOCH};

use rsplot::egui::Color32;
use rsplot::egui_wgpu::RenderState;
use rsplot::{
    DataMargins, ItemHandle, Plot1D, PlotId, PlotResponse, TickMode, TimeZone, YAxis, egui,
};

use crate::channel::{Channel, ChannelState, PvValue, ValueEvent, ValueSubscription};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::middle_click_copy;
use crate::widgets::plot_menu::{
    YAxisMenu, enable_y_autoscale, set_y_range, show_with_y_axis_menu,
};
use crate::widgets::plot_style::{CurveStyle, ensure_axis_autoscale};
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

/// How the strip chart labels its X axis.
///
/// PyDM's `PyDMTimePlot` always shows an absolute date-time axis; this port
/// shipped with a relative-seconds axis because rsplot's GPU curve vertices are
/// `f32` and an absolute epoch (~`1.7e9`) collapses under `f32` precision (see
/// the module docs). [`TimeAxisMode::WallClock`] restores the absolute axis
/// without that collapse by keeping the vertices relative and only offsetting
/// the *tick labels* back to wall-clock (rsplot `set_x_time_offset`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TimeAxisMode {
    /// X tick labels are seconds since the plot was created ("Time since start
    /// (s)"). The default the port shipped with.
    #[default]
    SinceStart,
    /// X tick labels are the absolute wall-clock time of each sample (PyDM's
    /// date-time axis), laid out in the configured [`TimeZone`]. The GPU
    /// vertices stay relative (`f32`-safe); only the labels read absolute.
    WallClock,
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

/// Seconds since the UNIX epoch for `t` — the strip chart's `f64` time axis —
/// or `0.0` for a pre-epoch time.
fn secs_since_epoch(t: SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Wall-clock POSIX seconds now (PyDM `time.time()`).
fn now_epoch_secs() -> f64 {
    secs_since_epoch(SystemTime::now())
}

/// The sample-feeding state for one curve: its buffer plus the fixed-rate
/// bookkeeping. Appends `(time, value)` samples and reports whether the buffer
/// changed (so the curve needs a redraw).
///
/// The two update modes feed it differently, matching PyDM:
/// - [`UpdateMode::OnValueChange`] drains the channel's value-event stream and
///   appends each event ([`CurveFeed::ingest_event`]) — event-driven, lossless,
///   one sample per `camonitor` callback at its own receive time.
/// - [`UpdateMode::AtFixedRate`] tracks the latest snapshot value and appends it
///   at the fixed rate ([`CurveFeed::ingest_fixed`]) — rate-driven, the latest
///   value resampled on a timer.
struct CurveFeed {
    buffer: TimeSeriesBuffer,
    /// Most recent numeric value, held for the fixed-rate push (PyDM
    /// `latest_value`). Used only by [`CurveFeed::ingest_fixed`].
    latest_value: Option<f64>,
}

impl CurveFeed {
    fn new(buffer_size: usize) -> Self {
        Self {
            buffer: TimeSeriesBuffer::new(buffer_size),
            latest_value: None,
        }
    }

    /// `OnValueChange`: append one value event at its receive time. Returns
    /// `true` when a sample was appended (a numeric value). Each event is one
    /// distinct monitor callback, so there is no stamp dedup — a repeated value
    /// still appends, showing the time progression (PyDM `receiveNewValue`).
    fn ingest_event(&mut self, event: &ValueEvent) -> bool {
        if let Some(v) = event.value.as_f64() {
            self.buffer.push(secs_since_epoch(event.time), v);
            return true;
        }
        false
    }

    /// `AtFixedRate`: track the latest snapshot value, and append it at `now`
    /// when the fixed interval is due (PyDM `asyncUpdate` on a `QTimer`).
    /// Returns `true` when a sample was appended.
    fn ingest_fixed(&mut self, now: f64, state: &ChannelState, rate_due: bool) -> bool {
        if let Some(v) = state.value.as_ref().and_then(PvValue::as_f64) {
            self.latest_value = Some(v);
        }
        if rate_due && let Some(v) = self.latest_value {
            self.buffer.push(now, v);
            return true;
        }
        false
    }
}

/// One channel-driven curve: its channel handle, its value-event subscription
/// (drained in `OnValueChange`), its feed, and the rsplot item handle plus
/// reusable scratch buffers for the render feed.
struct TimeCurve {
    channel: Channel,
    subscription: ValueSubscription,
    feed: CurveFeed,
    handle: ItemHandle,
    style: CurveStyle,
    /// Per-pen normalization onto the shared [0,1] axis (MEDM strip chart), or
    /// `None` for a raw curve on the auto-scaled axis.
    norm: Option<PenNorm>,
    xs: Vec<f64>,
    ys: Vec<f64>,
}

/// One end of a pen's normalization range. MEDM resolves each end independently
/// (per-bound source, R2-66): an authored constant (`*Src="default"` →
/// `*Default`) or the channel's own display limit (`PV_LIMITS_CHANNEL`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum NormBound {
    /// An authored constant (MEDM `*Default`).
    Fixed(f64),
    /// Resolve from the channel's display limits at draw time, falling back to
    /// 0.0 (lo) / 1.0 (hi) until they arrive — MEDM's unset-channel `[0,1]`
    /// guard (`medmStripChart.c:1441-1443`).
    Channel,
}

/// A pen's normalization onto the shared `[0,1]` axis (MEDM per-pen `[lopr,hopr]`
/// mapping, `medmStripChart.c:1878-1898`): the displayed value is
/// `(v - lopr) / (hopr - lopr)`, so pens with wildly different ranges (300 K vs
/// 1e-6 Torr) become comparable traces.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PenNorm {
    lo: NormBound,
    hi: NormBound,
}

/// The concrete `(lo, hi)` for a pen normalization: resolve each `Channel` end
/// against `display_limits` (→ 0.0/1.0 when absent), then widen a degenerate
/// range (`lo >= hi`) to `[lo, lo + 1]` — generalizing MEDM's both-zero
/// `hopr += 1` nudge (`medmStripChart.c:1441-1443`), which otherwise divides by
/// zero.
fn norm_bounds(norm: PenNorm, display_limits: Option<(f64, f64)>) -> (f64, f64) {
    let lo = match norm.lo {
        NormBound::Fixed(v) => v,
        NormBound::Channel => display_limits.map_or(0.0, |(lo, _)| lo),
    };
    let hi = match norm.hi {
        NormBound::Fixed(v) => v,
        NormBound::Channel => display_limits.map_or(1.0, |(_, hi)| hi),
    };
    if hi > lo { (lo, hi) } else { (lo, lo + 1.0) }
}

/// Redraw `curve` from its buffer, feeding rsplot relative-time X (`t - t0`) so
/// the GPU coordinates stay small (see the module docs on `f32` precision). The
/// buffer keeps absolute epoch timestamps; only the render feed is offset. A
/// normalized pen additionally maps each Y through `(v - lo) / (hi - lo)` onto
/// the shared `[0,1]` axis (MEDM per-pen normalization); the buffer keeps the
/// raw values.
fn redraw_curve(plot: &mut Plot1D, curve: &mut TimeCurve, t0: f64) {
    curve.feed.buffer.ordered_into(&mut curve.xs, &mut curve.ys);
    for x in &mut curve.xs {
        *x -= t0;
    }
    if let Some(norm) = curve.norm {
        let (lo, hi) = norm_bounds(norm, curve.channel.state().display_limits);
        let base = hi - lo;
        for y in &mut curve.ys {
            *y = (*y - lo) / base;
        }
    }
    plot.update_curve_spec(curve.handle, curve.style.to_spec(&curve.xs, &curve.ys));
}

/// A scrolling strip chart of scalar PVs versus time (PyDM `PyDMTimePlot`).
pub struct RsdmTimePlot {
    plot: Plot1D,
    curves: Vec<TimeCurve>,
    update_mode: UpdateMode,
    update_rate_hz: f64,
    time_span: f64,
    buffer_size: usize,
    /// Wall-clock time of the last fixed-rate push (`-inf` so the first frame
    /// samples immediately).
    last_fixed_push: f64,
    /// Reference epoch (creation time): the X feed to rsplot is `t - t0` to keep
    /// GPU coordinates small (see the module docs). Also the epoch offset that
    /// turns the relative tick positions back into wall-clock labels under
    /// [`TimeAxisMode::WallClock`].
    t0: f64,
    /// How the X axis is labeled (relative seconds vs absolute wall-clock).
    time_axis_mode: TimeAxisMode,
    /// Time zone for the wall-clock X tick labels (silx `setTimeZone`).
    time_zone: TimeZone,
    /// State for the pyqtgraph-style Y-axis context menu (auto-scale + range).
    y_menu: YAxisMenu,
    /// Set once the first per-pen normalized curve is added
    /// ([`RsdmTimePlot::add_normalized_channel`]); the shared left axis is then
    /// pinned to `[0,1]` and every normalized pen maps into it (MEDM strip
    /// chart). Raw `add_channel` curves stay on the auto-scaled axis.
    normalized: bool,
}

impl RsdmTimePlot {
    /// Create an empty time plot on the given GPU `render_state` and plot `id`.
    /// The Y axis autoscales; the X axis is relative time driven by the scroll
    /// window.
    pub fn new(render_state: &RenderState, id: PlotId) -> Self {
        let mut plot = Plot1D::new(render_state, id);
        // Live Y autoscale over a manually scrolled X window. `x_autoscale` stays
        // off so `reset_zoom_to_data_range` preserves the X limits the scroll sets
        // each frame; `y_autoscale` on + `auto_reset_zoom` on makes every data
        // update refit Y (and any y2/Extra axis) to the data, so a streaming curve
        // is visible without a manual reset-zoom. Pinning a fixed Y range is done
        // by turning `y_autoscale` off (the context-menu "Set range" path), after
        // which `apply_auto_limits` leaves that Y untouched while X keeps scrolling.
        plot.plot_mut().set_x_autoscale(false);
        plot.plot_mut().set_y_autoscale(true);
        plot.set_auto_reset_zoom(true);
        let mut chart = Self {
            plot,
            curves: Vec::new(),
            update_mode: UpdateMode::OnValueChange,
            update_rate_hz: DEFAULT_UPDATE_RATE_HZ,
            time_span: DEFAULT_TIME_SPAN,
            buffer_size: DEFAULT_BUFFER_SIZE,
            last_fixed_push: f64::NEG_INFINITY,
            t0: now_epoch_secs(),
            time_axis_mode: TimeAxisMode::SinceStart,
            // Default to the system local zone so the wall-clock axis reads the
            // user's actual clock (PyDM's date axis is local too); fall back to
            // UTC if the local zone can't be resolved.
            time_zone: TimeZone::local().unwrap_or(TimeZone::Utc),
            y_menu: YAxisMenu::new(),
            normalized: false,
        };
        // Set the X tick mode + label for the default (relative) axis.
        chart.apply_time_axis();
        chart
    }

    /// Configure rsplot's X tick mode, epoch offset, time zone, and axis label
    /// for the current [`TimeAxisMode`]. The single owner of the X-axis labeling
    /// so the tick mode and the label can never disagree.
    fn apply_time_axis(&mut self) {
        let (t0, tz) = (self.t0, self.time_zone);
        {
            let plot = self.plot.plot_mut();
            match self.time_axis_mode {
                // Relative seconds: numeric ticks (offset/zone are inert here).
                TimeAxisMode::SinceStart => plot.set_x_tick_mode(TickMode::Numeric),
                // Absolute wall-clock: date-time ticks laid out over the epoch
                // window [min+t0, max+t0] then shifted back by t0 so they land on
                // the relative (f32-safe) vertices (see [`TimeAxisMode`]).
                TimeAxisMode::WallClock => {
                    plot.set_x_tick_mode(TickMode::TimeSeries);
                    plot.set_x_time_offset(t0);
                    plot.set_x_time_zone(tz);
                }
            }
        }
        // The relative axis needs a unit label; the wall-clock axis is
        // self-describing (date-time ticks), so it carries none.
        match self.time_axis_mode {
            TimeAxisMode::SinceStart => self.plot.set_graph_x_label("Time since start (s)"),
            TimeAxisMode::WallClock => self.plot.clear_graph_x_label(),
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

    /// Add a per-side data margin around the autoscaled data (builder style;
    /// silx `setDataMargins` / pyqtgraph autorange `padding`). Each ratio expands
    /// that side of the axis by `ratio * range` whenever the axis refits, so the
    /// curve no longer touches the axis edge. For a strip chart the `y_min` /
    /// `y_max` ratios are what matter — they pad the live Y autoscale top and
    /// bottom; the `x_min` / `x_max` ratios are inert while the X axis is the
    /// scrolling time window (X autoscale is off). Default is no margin (the data
    /// fits the axes exactly).
    pub fn with_data_margins(mut self, margins: DataMargins) -> Self {
        self.plot.plot_mut().set_data_margins(margins);
        self
    }

    /// Set how the X axis is labeled — relative "Time since start (s)" (the
    /// default) or absolute wall-clock time (builder style). See
    /// [`TimeAxisMode`].
    pub fn with_time_axis_mode(mut self, mode: TimeAxisMode) -> Self {
        self.time_axis_mode = mode;
        self.apply_time_axis();
        self
    }

    /// Switch the X-axis labeling between relative and absolute time at runtime
    /// (so a UI toggle can flip it live). See [`TimeAxisMode`].
    pub fn set_time_axis_mode(&mut self, mode: TimeAxisMode) {
        self.time_axis_mode = mode;
        self.apply_time_axis();
    }

    /// The current X-axis labeling mode.
    pub fn time_axis_mode(&self) -> TimeAxisMode {
        self.time_axis_mode
    }

    /// Set the time zone used to lay out the wall-clock X tick labels (builder
    /// style; silx `setTimeZone`). Defaults to the system local zone
    /// ([`TimeZone::local`], UTC fallback); only affects
    /// [`TimeAxisMode::WallClock`]. Pass [`TimeZone::Utc`] or a
    /// [`TimeZone::FixedOffset`] to override (e.g. `seconds_east: 32400` for
    /// KST/UTC+9).
    pub fn with_time_zone(mut self, tz: TimeZone) -> Self {
        self.time_zone = tz;
        self.apply_time_axis();
        self
    }

    /// Show a hover crosshair with an `(x, y)` coordinate readout over the data
    /// area (builder style; silx crosshair cursor). The X readout follows the
    /// current [`TimeAxisMode`]: relative seconds under [`TimeAxisMode::SinceStart`],
    /// or the absolute wall-clock time under [`TimeAxisMode::WallClock`] (it reads
    /// the same as the X tick labels).
    pub fn with_crosshair(mut self, enabled: bool) -> Self {
        self.plot.plot_mut().crosshair = enabled;
        self
    }

    /// Toggle the hover crosshair + `(x, y)` readout at runtime. See
    /// [`Self::with_crosshair`].
    pub fn set_crosshair(&mut self, enabled: bool) {
        self.plot.plot_mut().crosshair = enabled;
    }

    /// Set the plot title (builder style; PyDM `BasePlot.setPlotTitle`, MEDM
    /// `plotcom` `title`).
    pub fn with_title(mut self, title: &str) -> Self {
        self.plot.set_graph_title(title);
        self
    }

    /// Set the X-axis label (builder style; PyDM `xLabels`, MEDM `plotcom`
    /// `xlabel`). Overrides the [`TimeAxisMode`] default label, so apply this
    /// AFTER [`Self::with_time_axis_mode`] when combining the two.
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

    /// Whether the hover crosshair + `(x, y)` readout is enabled.
    pub fn crosshair(&self) -> bool {
        self.plot.plot().crosshair
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
        // Subscribe to the value-event stream so `OnValueChange` gets every
        // monitor callback (not a per-frame snapshot poll). Cap the queue at the
        // buffer size — the same bound the curve buffer keeps.
        let subscription = channel.subscribe_values(self.buffer_size);
        let handle = self.plot.add_curve_with_legend(&[], &[], color, legend);
        self.curves.push(TimeCurve {
            channel,
            subscription,
            feed: CurveFeed::new(self.buffer_size),
            handle,
            style: CurveStyle::line(color),
            norm: None,
            xs: Vec::new(),
            ys: Vec::new(),
        });
        Ok(self.curves.len() - 1)
    }

    /// Connect `address` and add it as a pen **normalized** onto the shared
    /// `[0,1]` axis (MEDM strip-chart per-pen `[lopr,hopr]` mapping,
    /// `medmStripChart.c:1878-1898`): each Y is displayed as
    /// `(v - lopr) / (hopr - lopr)`, so pens with different engineering ranges
    /// stay readable on one axis. `lo`/`hi`: `Some(v)` pins that end (MEDM
    /// `*Src="default"` → `*Default`); `None` resolves that end from the
    /// channel's display limits at draw time (MEDM `PV_LIMITS_CHANNEL`), falling
    /// back to 0.0/1.0 until they arrive. Adding the first normalized pen pins
    /// the shared left axis to `[0,1]`. Returns the new curve's index.
    pub fn add_normalized_channel(
        &mut self,
        engine: &Engine,
        address: &str,
        color: Color32,
        legend: impl Into<String>,
        lo: Option<f64>,
        hi: Option<f64>,
    ) -> Result<usize, EngineError> {
        let index = self.add_channel(engine, address, color, legend)?;
        self.curves[index].norm = Some(PenNorm {
            lo: lo.map_or(NormBound::Channel, NormBound::Fixed),
            hi: hi.map_or(NormBound::Channel, NormBound::Fixed),
        });
        if !self.normalized {
            self.normalized = true;
            // Pin the shared left axis to [0,1]; every normalized pen maps into
            // it. `set_y_range` turns off Y autoscale so the pin survives the
            // per-frame data updates.
            set_y_range(&mut self.plot, 0.0, 1.0);
        }
        Ok(index)
    }

    /// Restyle curve `index` (PyDM `BasePlotCurveItem` properties: colour, line
    /// style/width, symbol, Y axis) and re-draw it immediately. Assigning the
    /// curve to a secondary axis ([`YAxis::Right`](rsplot::YAxis::Right) or an
    /// [`YAxis::Extra`](rsplot::YAxis::Extra) stacked axis) enables that axis'
    /// autoscale so it gets its own scale. Returns `false` for an out-of-range
    /// index.
    pub fn set_curve_style(&mut self, index: usize, style: CurveStyle) -> bool {
        if index >= self.curves.len() {
            return false;
        }
        let axis = style.y_axis;
        self.curves[index].style = style;
        ensure_axis_autoscale(&mut self.plot, axis);
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

    /// Ingest pending data, redraw changed curves, scroll the X window, and
    /// render the plot this frame.
    ///
    /// In [`UpdateMode::OnValueChange`] this drains each curve's value-event
    /// queue: every value that arrived since the last frame lands as its own
    /// sample at its own receive time, so a burst arriving between two frames is
    /// not coalesced. The queue is filled by the engine independent of repaint,
    /// so a chart on an inactive tab keeps accumulating (up to the queue bound)
    /// and renders its full recent history when the tab is shown again — no need
    /// for the app to poll a hidden chart. In [`UpdateMode::AtFixedRate`] it
    /// resamples the latest snapshot value on the fixed timer instead.
    pub fn show(&mut self, ui: &mut egui::Ui) -> PlotResponse {
        let now = now_epoch_secs();
        let t0 = self.t0;

        match self.update_mode {
            UpdateMode::OnValueChange => {
                for curve in &mut self.curves {
                    let feed = &mut curve.feed;
                    let mut changed = false;
                    curve.subscription.drain(|event| {
                        if feed.ingest_event(&event) {
                            changed = true;
                        }
                    });
                    if changed {
                        redraw_curve(&mut self.plot, curve, t0);
                    }
                }
            }
            UpdateMode::AtFixedRate => {
                let interval = update_interval(self.update_rate_hz);
                let rate_due = is_rate_due(now, self.last_fixed_push, interval);
                if rate_due {
                    self.last_fixed_push = now;
                }
                for curve in &mut self.curves {
                    let state = curve.channel.state();
                    if curve.feed.ingest_fixed(now, &state, rate_due) {
                        redraw_curve(&mut self.plot, curve, t0);
                    }
                }
            }
        }

        // Relative-time scroll window: [now - t0 - span, now - t0].
        let right = now - t0;
        self.plot.set_graph_x_limits(right - self.time_span, right);
        // A strip chart animates: keep frames coming even between channel updates
        // so the X window scrolls smoothly.
        ui.ctx().request_repaint();
        let response = show_with_y_axis_menu(&mut self.plot, &mut self.y_menu, ui);
        // MEDM Btn2 copies every record the plot carries (strip-chart pens).
        middle_click_copy(
            ui,
            &response.response,
            self.curves.iter().map(|c| c.channel.address().raw()),
        );
        response
    }

    /// Pin a fixed Y range, disabling live autoscale (pyqtgraph `setYRange`);
    /// the range survives streaming updates until autoscale is re-enabled. Same
    /// effect as the context menu's "Set Y range".
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
    use std::time::{Duration, UNIX_EPOCH};

    fn state(connected: bool, stamp: u64, value: Option<PvValue>) -> ChannelState {
        ChannelState {
            connected,
            value,
            stamp,
            ..ChannelState::default()
        }
    }

    /// A value event at `secs` seconds past the epoch (the strip chart's time
    /// axis), carrying `value`.
    fn event(value: PvValue, secs: f64) -> ValueEvent {
        ValueEvent {
            value,
            time: UNIX_EPOCH + Duration::from_secs_f64(secs),
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
    fn ingest_event_appends_each_numeric_value_at_its_receive_time() {
        let mut feed = CurveFeed::new(8);
        // Each event appends a sample stamped at the event's receive time.
        assert!(feed.ingest_event(&event(PvValue::Float(5.0), 1.0)));
        assert_eq!(feed.buffer.newest(), Some((1.0, 5.0)));
        // A second event carrying a *repeated* value still appends (no stamp
        // dedup — the strip chart must show the time progression).
        assert!(feed.ingest_event(&event(PvValue::Float(5.0), 2.0)));
        assert_eq!(feed.buffer.newest(), Some((2.0, 5.0)));
        assert!(feed.ingest_event(&event(PvValue::Float(6.0), 3.0)));
        assert_eq!(feed.buffer.newest(), Some((3.0, 6.0)));
        // Three events between two frames → three samples (no coalescing).
        assert_eq!(feed.buffer.len(), 3);
    }

    #[test]
    fn ingest_event_skips_non_numeric_values() {
        let mut feed = CurveFeed::new(8);
        assert!(!feed.ingest_event(&event(PvValue::Str("x".into()), 1.0)));
        assert!(feed.buffer.is_empty());
    }

    #[test]
    fn ingest_fixed_appends_latest_value_only_when_due() {
        let mut feed = CurveFeed::new(8);
        // Not due: tracks latest, appends nothing.
        assert!(!feed.ingest_fixed(1.0, &state(true, 1, Some(PvValue::Float(5.0))), false));
        assert!(feed.buffer.is_empty());
        // A newer value updates the tracked latest.
        assert!(!feed.ingest_fixed(1.5, &state(true, 2, Some(PvValue::Float(7.0))), false));
        // Due: appends the latest tracked value at the current time.
        assert!(feed.ingest_fixed(2.0, &state(true, 2, Some(PvValue::Float(7.0))), true));
        assert_eq!(feed.buffer.newest(), Some((2.0, 7.0)));
        assert_eq!(feed.buffer.len(), 1);
    }

    /// Map a raw value through a resolved normalization, the way `redraw_curve`
    /// feeds rsplot: `(v - lo) / (hi - lo)` over `norm_bounds`.
    fn normalize(norm: PenNorm, display_limits: Option<(f64, f64)>, v: f64) -> f64 {
        let (lo, hi) = norm_bounds(norm, display_limits);
        (v - lo) / (hi - lo)
    }

    #[test]
    fn fixed_range_pen_normalizes_to_unit_axis() {
        // R3-18: an authored [0,300] pen maps 0→0, 150→0.5, 300→1 with no clamp.
        let n = PenNorm {
            lo: NormBound::Fixed(0.0),
            hi: NormBound::Fixed(300.0),
        };
        assert_eq!(normalize(n, None, 0.0), 0.0);
        assert_eq!(normalize(n, None, 150.0), 0.5);
        assert_eq!(normalize(n, None, 300.0), 1.0);
        assert!((normalize(n, None, -30.0) - -0.1).abs() < 1e-12, "no clamp");
    }

    #[test]
    fn mixed_range_pens_share_the_normalized_axis() {
        // R3-18 (the readability goal): a 300 K pen and a 1e-6 Torr pen both land
        // mid-axis at their mid-range value, so both traces are visible together.
        let temp = PenNorm {
            lo: NormBound::Fixed(0.0),
            hi: NormBound::Fixed(300.0),
        };
        let press = PenNorm {
            lo: NormBound::Fixed(0.0),
            hi: NormBound::Fixed(1e-6),
        };
        assert_eq!(normalize(temp, None, 150.0), 0.5);
        assert_eq!(normalize(press, None, 5e-7), 0.5);
    }

    #[test]
    fn degenerate_range_is_widened_not_divided_by_zero() {
        // R3-18: lopr == hopr would divide by zero; widen to [lo, lo+1].
        let n = PenNorm {
            lo: NormBound::Fixed(5.0),
            hi: NormBound::Fixed(5.0),
        };
        assert_eq!(norm_bounds(n, None), (5.0, 6.0));
        assert_eq!(normalize(n, None, 5.0), 0.0);
        assert_eq!(normalize(n, None, 6.0), 1.0);
        assert!(normalize(n, None, 5.5).is_finite());
    }

    #[test]
    fn channel_sourced_ends_resolve_from_display_limits() {
        // R3-18: a `Channel` end reads the channel's display limits; absent, it
        // falls back to 0.0 (lo) / 1.0 (hi) — MEDM's unset-channel [0,1] guard.
        let both = PenNorm {
            lo: NormBound::Channel,
            hi: NormBound::Channel,
        };
        assert_eq!(norm_bounds(both, Some((10.0, 20.0))), (10.0, 20.0));
        assert_eq!(normalize(both, Some((10.0, 20.0)), 15.0), 0.5);
        assert_eq!(norm_bounds(both, None), (0.0, 1.0));
        assert_eq!(normalize(both, None, 0.25), 0.25);
        // Per-end (R2-66): a fixed lo with a channel-sourced hi.
        let mixed = PenNorm {
            lo: NormBound::Fixed(0.0),
            hi: NormBound::Channel,
        };
        assert_eq!(norm_bounds(mixed, Some((-999.0, 50.0))), (0.0, 50.0));
        assert_eq!(normalize(mixed, Some((0.0, 50.0)), 25.0), 0.5);
    }
}
