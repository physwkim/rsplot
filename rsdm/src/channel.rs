//! Channel value model and per-frame state snapshot.
//!
//! These are the pure data types every plugin produces and every widget reads.
//! They mirror the payload PyDM's `PyDMChannel` carries across its per-slot
//! signals (`pydm/widgets/channel.py`, `pydm/data_plugins/plugin.py`):
//! value, alarm severity, connection/write-access state, enum strings, units,
//! precision, control/alarm/warning limits, and a timestamp. In an immediate
//! mode GUI these collapse into one [`ChannelState`] snapshot that the tokio
//! side updates and widgets read each frame.
//!
//! The pure value/state types ([`AlarmSeverity`], [`PvValue`],
//! [`ChannelState`]) are headlessly testable on their own. The live
//! [`Channel`] handle and its `Connection`/`StateWriter` machinery (which a
//! plugin task drives over the async runtime) live at the bottom of the file.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::SystemTime;

use rsplot::egui;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::address::PvAddress;

/// EPICS alarm severity. Variants 0..=3 are the wire severities
/// (`NO_ALARM`/`MINOR`/`MAJOR`/`INVALID`); [`AlarmSeverity::Disconnected`] is a
/// widget-only state PyDM derives when a channel is not connected
/// (`ALARM_DISCONNECTED = 4`), never sent by a backend.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub enum AlarmSeverity {
    /// `NO_ALARM` (0).
    #[default]
    NoAlarm,
    /// `MINOR` (1).
    Minor,
    /// `MAJOR` (2).
    Major,
    /// `INVALID` (3).
    Invalid,
    /// Engine-derived disconnected state (4).
    Disconnected,
}

impl AlarmSeverity {
    /// Map an EPICS wire severity (0..=3) to a variant. Values `>= 3` clamp to
    /// [`AlarmSeverity::Invalid`] (the highest wire severity); `Disconnected`
    /// is never produced here.
    pub fn from_epics(severity: u16) -> Self {
        match severity {
            0 => Self::NoAlarm,
            1 => Self::Minor,
            2 => Self::Major,
            _ => Self::Invalid,
        }
    }

    /// The PyDM numeric code (`NO_ALARM=0` … `DISCONNECTED=4`).
    pub fn as_code(self) -> u16 {
        match self {
            Self::NoAlarm => 0,
            Self::Minor => 1,
            Self::Major => 2,
            Self::Invalid => 3,
            Self::Disconnected => 4,
        }
    }
}

/// A normalized channel value. Backends (`EpicsValue`, pvAccess `PvField`)
/// convert into this once, on the tokio side, so the per-frame path is
/// allocation-free: cloning a `PvValue` only bumps `Arc` refcounts.
#[derive(Clone, Debug, PartialEq)]
pub enum PvValue {
    /// Integer scalar.
    Int(i64),
    /// Floating-point scalar.
    Float(f64),
    /// Boolean scalar.
    Bool(bool),
    /// String scalar.
    Str(Arc<str>),
    /// Enumeration: the index plus its resolved label (when enum strings are
    /// known).
    Enum {
        /// Selected index.
        index: u16,
        /// Resolved label for `index`, if the enum strings are cached.
        label: Option<Arc<str>>,
    },
    /// Floating-point waveform.
    FloatArray(Arc<[f64]>),
    /// Integer waveform.
    IntArray(Arc<[i64]>),
    /// String array.
    StrArray(Arc<[String]>),
    /// Raw byte array (EPICS `CHAR` waveform; may carry a NUL-terminated
    /// string).
    Bytes(Arc<[u8]>),
}

impl PvValue {
    /// Numeric view of a scalar value: `Int`/`Float`/`Bool`/`Enum.index`.
    /// Arrays and strings yield `None`.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(v) => Some(*v as f64),
            Self::Float(v) => Some(*v),
            Self::Bool(v) => Some(if *v { 1.0 } else { 0.0 }),
            Self::Enum { index, .. } => Some(f64::from(*index)),
            _ => None,
        }
    }

    /// Integer view of a scalar value (truncating a float toward zero).
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(v) => Some(*v),
            Self::Float(v) => Some(*v as i64),
            Self::Bool(v) => Some(i64::from(*v)),
            Self::Enum { index, .. } => Some(i64::from(*index)),
            _ => None,
        }
    }

    /// Whether this value is an array (waveform) rather than a scalar.
    pub fn is_array(&self) -> bool {
        matches!(
            self,
            Self::FloatArray(_) | Self::IntArray(_) | Self::StrArray(_) | Self::Bytes(_)
        )
    }

    /// Borrow a float waveform without copying. Returns `None` for non-float
    /// arrays and scalars.
    pub fn as_f64_slice(&self) -> Option<&[f64]> {
        match self {
            Self::FloatArray(a) => Some(a),
            _ => None,
        }
    }

    /// Element count for arrays; `1` for scalars.
    pub fn len(&self) -> usize {
        match self {
            Self::FloatArray(a) => a.len(),
            Self::IntArray(a) => a.len(),
            Self::StrArray(a) => a.len(),
            Self::Bytes(a) => a.len(),
            _ => 1,
        }
    }

    /// Whether an array value is empty. Scalars are never empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The full snapshot of a channel, read by widgets each frame.
///
/// One field per PyDM channel slot. The tokio side replaces this behind a lock
/// and bumps [`ChannelState::stamp`] on every change so consumers can detect
/// updates (e.g. to skip a GPU re-upload) without comparing the whole struct.
#[derive(Clone, Debug, Default)]
pub struct ChannelState {
    /// Whether the underlying connection is established.
    pub connected: bool,
    /// Whether writes are permitted (CA access rights / pvAccess put).
    pub write_access: bool,
    /// Latest value, if one has arrived.
    pub value: Option<PvValue>,
    /// Wire alarm severity (see [`ChannelState::effective_severity`] for the
    /// value to use when styling).
    pub severity: AlarmSeverity,
    /// Enumeration strings (CA `DBR_GR/CTRL_ENUM`, pvAccess enum), if any.
    pub enum_strings: Option<Arc<[String]>>,
    /// Engineering units (`EGU`).
    pub units: Option<Arc<str>>,
    /// Display precision (`PREC`).
    pub precision: Option<i32>,
    /// Display low/high limits.
    pub display_limits: Option<(f64, f64)>,
    /// Control low/high limits (`DRVL`/`DRVH`).
    pub ctrl_limits: Option<(f64, f64)>,
    /// Warning low/high limits (`LOW`/`HIGH`).
    pub warn_limits: Option<(f64, f64)>,
    /// Alarm low/high limits (`LOLO`/`HIHI`).
    pub alarm_limits: Option<(f64, f64)>,
    /// Timestamp of the latest value.
    pub timestamp: Option<SystemTime>,
    /// Monotonic update counter, bumped on every change.
    pub stamp: u64,
}

impl ChannelState {
    /// Severity to use for styling: [`AlarmSeverity::Disconnected`] overrides
    /// the PV severity whenever the channel is not connected (PyDM
    /// `base.py` `connection_changed`).
    pub fn effective_severity(&self) -> AlarmSeverity {
        if self.connected {
            self.severity
        } else {
            AlarmSeverity::Disconnected
        }
    }
}

/// One value update, delivered to a subscriber as a discrete event.
///
/// The [`ChannelState`] snapshot keeps only the *latest* value, so consumers
/// that read it once per GUI frame coalesce every update that arrived since the
/// previous frame into one (acceptable for a label, lossy for a strip chart).
/// A `ValueEvent` is the event-driven complement: the engine emits exactly one
/// per value arrival (an EPICS monitor callback that actually changed the
/// value), so a consumer that drains the [`ValueSubscription`] queue sees
/// *every* value — at its own arrival time — even when many land between two
/// frames. This mirrors PyDM, whose `receiveNewValue` slot fires (and appends
/// to the plot buffer) once per *emitted* value, independent of repaint — and
/// the plugins emit only on an actual value change
/// (`pyepics_plugin_component.py:102`, `p4p_plugin_component.py:241-242`).
#[derive(Clone, Debug)]
pub struct ValueEvent {
    /// The value carried by this update.
    pub value: PvValue,
    /// Engine-side receive time, used as the sample's timestamp (PyDM uses
    /// `time.time()` in the value callback — the same wall clock the engine
    /// stamps here when it fans the event out).
    pub time: SystemTime,
}

/// A bounded FIFO of [`ValueEvent`]s shared by one producer (the connection
/// task) and one consumer (a widget on the GUI thread).
///
/// Bounded with drop-oldest: if the consumer falls behind, the oldest events are
/// dropped so memory stays bounded — the consumer keeps the most recent `cap`,
/// which matches the bounded plot buffer it feeds (PyDM `bufferSize`).
struct ValueQueue {
    events: Mutex<VecDeque<ValueEvent>>,
    cap: usize,
}

impl ValueQueue {
    fn push(&self, event: ValueEvent) {
        let mut events = self.events.lock().expect("value queue poisoned");
        while events.len() >= self.cap {
            events.pop_front();
        }
        events.push_back(event);
    }

    /// Take every queued event (oldest first), leaving the queue empty.
    fn take(&self) -> VecDeque<ValueEvent> {
        std::mem::take(&mut *self.events.lock().expect("value queue poisoned"))
    }
}

/// A widget's handle to a channel's value-event stream, obtained from
/// [`Channel::subscribe_values`]. Dropping it unsubscribes: the connection
/// prunes the now-dead queue on its next publish (the queue is held by a `Weak`
/// on the producer side).
pub struct ValueSubscription {
    queue: Arc<ValueQueue>,
}

impl ValueSubscription {
    /// Drain every event queued since the last call (oldest first) into `f`.
    /// Call this each frame: the queue buffers events between frames, so nothing
    /// is lost even when the widget is not drawn (an inactive tab).
    pub fn drain(&self, mut f: impl FnMut(ValueEvent)) {
        for event in self.queue.take() {
            f(event);
        }
    }
}

// ---------------------------------------------------------------------------
// Live channel machinery (engine side).
// ---------------------------------------------------------------------------

/// A shared, settable handle to the GUI repaint context.
///
/// Cloned into every connection's shared state so the tokio side can wake the
/// GUI when a value arrives. Engine-wide and late-settable: a connection
/// created before [`crate::Engine::attach_repaint`] still gets repaints once a
/// context is attached. `None` (the default) makes [`RepaintHook::notify`] a
/// no-op, which is exactly what headless tests want.
#[derive(Clone, Default)]
pub struct RepaintHook(Arc<Mutex<Option<egui::Context>>>);

impl RepaintHook {
    /// Install (or replace) the GUI context to repaint on updates.
    pub fn set(&self, ctx: egui::Context) {
        *self.0.lock().expect("repaint hook poisoned") = Some(ctx);
    }

    /// Request a repaint if a context is attached. Cheap and thread-safe; the
    /// context is cloned out so the lock is not held across the egui call.
    pub fn notify(&self) {
        let ctx = self.0.lock().expect("repaint hook poisoned").clone();
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    }
}

/// State shared between a [`Channel`] (GUI side) and its [`StateWriter`]
/// (plugin side): the locked [`ChannelState`] snapshot, the repaint hook, and
/// the live value-event subscribers (one [`ValueQueue`] per
/// [`Channel::subscribe_values`], held weakly so a dropped subscription is
/// pruned on the next publish).
pub(crate) struct ConnShared {
    state: RwLock<ChannelState>,
    repaint: RepaintHook,
    value_subs: Mutex<Vec<Weak<ValueQueue>>>,
}

impl ConnShared {
    /// Register a new bounded value-event queue and return it. The producer
    /// keeps only a `Weak`, so dropping the returned [`ValueSubscription`]
    /// (which owns the `Arc`) ends the subscription.
    fn subscribe_values(&self, cap: usize) -> Arc<ValueQueue> {
        let queue = Arc::new(ValueQueue {
            events: Mutex::new(VecDeque::new()),
            cap: cap.max(1),
        });
        self.value_subs
            .lock()
            .expect("value subscribers poisoned")
            .push(Arc::downgrade(&queue));
        queue
    }

    /// Fan a value out to every live subscriber, stamping it with the current
    /// receive time, and prune any whose subscription has been dropped. A no-op
    /// (beyond the empty-list check) when nothing subscribes — the common case
    /// for scalar widgets that only read the snapshot.
    fn publish_value(&self, value: PvValue) {
        let mut subs = self.value_subs.lock().expect("value subscribers poisoned");
        if subs.is_empty() {
            return;
        }
        let event = ValueEvent {
            value,
            time: SystemTime::now(),
        };
        subs.retain(|weak| match weak.upgrade() {
            Some(queue) => {
                queue.push(event.clone());
                true
            }
            None => false,
        });
    }
}

/// The plugin-side handle used to publish [`ChannelState`] updates.
///
/// Cloneable so one connection task can fan a single variable out to several
/// internal updaters. Every [`StateWriter::update`] bumps
/// [`ChannelState::stamp`] and requests a GUI repaint.
#[derive(Clone)]
pub struct StateWriter {
    shared: Arc<ConnShared>,
}

impl StateWriter {
    /// Apply `f` to the channel state under the write lock, bump the update
    /// stamp, and request a repaint.
    ///
    /// Use this for connection / metadata / access-rights changes — anything
    /// that does **not** deliver a new value. A value arrival must instead go
    /// through [`post_value`](Self::post_value) so it is also emitted to
    /// value-event subscribers; routing it through `update` would update the
    /// snapshot but silently drop the event (a strip chart would miss the
    /// sample).
    pub fn update(&self, f: impl FnOnce(&mut ChannelState)) {
        {
            let mut state = self.shared.state.write().expect("channel state poisoned");
            f(&mut state);
            state.stamp = state.stamp.wrapping_add(1);
        }
        self.shared.repaint.notify();
    }

    /// Apply `f` (which **must** set `state.value`), bump the stamp, request a
    /// repaint, and emit the resulting value as a [`ValueEvent`] to every
    /// subscriber.
    ///
    /// This is the single owner of "a value arrived": updating the latest-value
    /// snapshot and fanning the event out happen together, so a value can never
    /// reach the snapshot without also reaching the event stream. Call it at
    /// every value-arrival site (the monitor callback, the connect-time initial
    /// value); connection/metadata-only changes use [`update`](Self::update) so
    /// they do not emit a spurious sample.
    ///
    /// `post_value` itself emits one event per call, unconditionally — the
    /// *caller* decides what counts as a value arrival. The PV backends gate
    /// on an actual value change before calling it, matching PyDM, which
    /// dedups before `receiveNewValue` ever fires: pyepics emits only when
    /// `not np.array_equal(value, self._value)`
    /// (`pyepics_plugin_component.py:102`) and p4p only when `"value"` is in
    /// the monitor's `changedSet()` (`p4p_plugin_component.py:241-242`). So
    /// an alarm-only or metadata-only update refreshes the snapshot via
    /// [`update`](Self::update) and appends no sample.
    pub fn post_value(&self, f: impl FnOnce(&mut ChannelState)) {
        {
            let mut state = self.shared.state.write().expect("channel state poisoned");
            f(&mut state);
            state.stamp = state.stamp.wrapping_add(1);
            // Fan the event out to the *current* subscribers while the write
            // lock is still held, so the snapshot write and the emission are
            // atomic. Were the publish moved after the lock releases (its own
            // statement), a reader could observe `state.value` in the window
            // between the release and the publish, subscribe there, and then
            // receive this already-past value — violating the
            // [`Channel::subscribe_values`] "values before subscribing are not
            // replayed" contract and leaking a spurious strip-chart sample.
            // Holding the lock across the publish makes "value observable in the
            // snapshot ⟹ its event was already fanned out to exactly the
            // then-current subscribers" true by construction, not by timing.
            if let Some(value) = state.value.clone() {
                self.shared.publish_value(value);
            }
        }
        self.shared.repaint.notify();
    }

    /// Read the current published state — the same snapshot the GUI-side
    /// [`Channel::read`] sees.
    ///
    /// Lets the connection task gate decisions on what it has *published*
    /// rather than on a shadow copy: the write path checks
    /// `state.write_access` here before a put (PyDM's
    /// `if self.pv.write_access:` gate, `pyepics_plugin_component.py:209`),
    /// so "widget shown enabled" and "put allowed" can never diverge.
    pub fn read<R>(&self, f: impl FnOnce(&ChannelState) -> R) -> R {
        f(&self.shared.state.read().expect("channel state poisoned"))
    }
}

/// One per-PV connection: owns the shared state, the GUI→engine write queue,
/// and the cancellation token that stops the plugin task. Created by the
/// engine, referenced by [`Channel`]s (clone = add listener); dropping the last
/// `Channel` drops this, which cancels the task and prunes the pool entry —
/// the structural equivalent of PyDM's `remove_listener` refcount → `close()`.
pub(crate) struct Connection {
    shared: Arc<ConnShared>,
    address: PvAddress,
    writes_tx: mpsc::UnboundedSender<PvValue>,
    /// Forwards each later listener's full address to the plugin task (see
    /// [`Connection::forward_listener`]). Only the `loc://` task reads the
    /// paired receiver; for other protocols the receiver is simply never
    /// polled, so sends here are dropped harmlessly.
    listeners_tx: mpsc::UnboundedSender<PvAddress>,
    cancel: CancellationToken,
    pool: Weak<Mutex<std::collections::HashMap<String, Weak<Connection>>>>,
    pool_key: String,
}

impl Connection {
    /// Create a connection and the [`StateWriter`] / write-queue receiver /
    /// cancellation token a plugin needs to drive it. The caller (engine) wires
    /// the pool fields and spawns the plugin task.
    pub(crate) fn new(
        address: PvAddress,
        repaint: RepaintHook,
        pool: Weak<Mutex<std::collections::HashMap<String, Weak<Connection>>>>,
        pool_key: String,
    ) -> (
        Arc<Connection>,
        StateWriter,
        mpsc::UnboundedReceiver<PvValue>,
        mpsc::UnboundedReceiver<PvAddress>,
        CancellationToken,
    ) {
        let shared = Arc::new(ConnShared {
            state: RwLock::new(ChannelState::default()),
            repaint,
            value_subs: Mutex::new(Vec::new()),
        });
        let (writes_tx, writes_rx) = mpsc::unbounded_channel();
        let (listeners_tx, listeners_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let conn = Arc::new(Connection {
            shared: shared.clone(),
            address,
            writes_tx,
            listeners_tx,
            cancel: cancel.clone(),
            pool,
            pool_key,
        });
        let writer = StateWriter { shared };
        (conn, writer, writes_rx, listeners_rx, cancel)
    }

    /// Notify the plugin task that a new `Channel` is attaching to this
    /// already-pooled connection, handing it the listener's full address (with
    /// query). The `loc://` task consumes this to configure on the first
    /// config-bearing address regardless of connect order (PyDM's
    /// per-`add_listener` `_configure_local_plugin`); tasks that never poll the
    /// receiver simply let the send fall on the floor. A closed channel (task
    /// already exited) is ignored — there is nothing left to configure.
    pub(crate) fn forward_listener(&self, address: PvAddress) {
        let _ = self.listeners_tx.send(address);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Stop the plugin task.
        self.cancel.cancel();
        // Prune our pool entry — but only if it still points at *this* (now
        // dying) connection. A concurrent reconnect for the same key inserts a
        // fresh Weak with a non-zero strong count, which we must not evict.
        if let Some(pool) = self.pool.upgrade() {
            let mut map = pool.lock().expect("connection pool poisoned");
            if let Some(entry) = map.get(&self.pool_key)
                && entry.strong_count() == 0
            {
                map.remove(&self.pool_key);
            }
        }
    }
}

/// A handle to a channel's live state, obtained from
/// [`crate::Engine::connect`]. Cloning a `Channel` is PyDM's `add_listener`
/// (refcount up); dropping the last clone closes the underlying connection.
#[derive(Clone)]
pub struct Channel {
    conn: Arc<Connection>,
}

impl Channel {
    pub(crate) fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }

    /// Read the channel state under a short-lived read lock and return whatever
    /// `f` extracts (a zero-clone frame read).
    pub fn read<R>(&self, f: impl FnOnce(&ChannelState) -> R) -> R {
        f(&self
            .conn
            .shared
            .state
            .read()
            .expect("channel state poisoned"))
    }

    /// Clone the full current state.
    pub fn state(&self) -> ChannelState {
        self.conn
            .shared
            .state
            .read()
            .expect("channel state poisoned")
            .clone()
    }

    /// Subscribe to this channel's value-event stream, returning a
    /// [`ValueSubscription`] backed by a bounded `cap`-event queue.
    ///
    /// Every subsequent value arrival is queued as one [`ValueEvent`]; the
    /// caller drains it each frame (see [`ValueSubscription::drain`]). Unlike
    /// the latest-value [`state`](Self::state) snapshot, this loses no update
    /// when several arrive between two frames — the model an event-driven
    /// `camonitor` stream needs. Each subscription has its own queue, so two
    /// widgets on the same PV both receive every event. Only values posted
    /// *after* subscribing are delivered (the current snapshot value is not
    /// replayed).
    pub fn subscribe_values(&self, cap: usize) -> ValueSubscription {
        ValueSubscription {
            queue: self.conn.shared.subscribe_values(cap),
        }
    }

    /// The current update stamp (monotonic per connection).
    pub fn stamp(&self) -> u64 {
        self.read(|s| s.stamp)
    }

    /// Whether the connection is currently established.
    pub fn is_connected(&self) -> bool {
        self.read(|s| s.connected)
    }

    /// Queue a value to write back to the source (non-blocking). Dropped
    /// silently if the connection's task has already gone away.
    pub fn put(&self, value: PvValue) {
        let _ = self.conn.writes_tx.send(value);
    }

    /// The parsed address this channel connects to.
    pub fn address(&self) -> &PvAddress {
        &self.conn.address
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epics_severity_maps_and_clamps() {
        assert_eq!(AlarmSeverity::from_epics(0), AlarmSeverity::NoAlarm);
        assert_eq!(AlarmSeverity::from_epics(1), AlarmSeverity::Minor);
        assert_eq!(AlarmSeverity::from_epics(2), AlarmSeverity::Major);
        assert_eq!(AlarmSeverity::from_epics(3), AlarmSeverity::Invalid);
        // Out-of-range wire severities clamp to INVALID, never Disconnected.
        assert_eq!(AlarmSeverity::from_epics(99), AlarmSeverity::Invalid);
    }

    #[test]
    fn severity_codes_match_pydm() {
        assert_eq!(AlarmSeverity::NoAlarm.as_code(), 0);
        assert_eq!(AlarmSeverity::Minor.as_code(), 1);
        assert_eq!(AlarmSeverity::Major.as_code(), 2);
        assert_eq!(AlarmSeverity::Invalid.as_code(), 3);
        assert_eq!(AlarmSeverity::Disconnected.as_code(), 4);
    }

    #[test]
    fn scalar_numeric_views() {
        assert_eq!(PvValue::Int(7).as_f64(), Some(7.0));
        assert_eq!(PvValue::Float(1.5).as_i64(), Some(1));
        assert_eq!(PvValue::Bool(true).as_f64(), Some(1.0));
        assert_eq!(
            PvValue::Enum {
                index: 2,
                label: None
            }
            .as_f64(),
            Some(2.0)
        );
        assert_eq!(PvValue::Str(Arc::from("x")).as_f64(), None);
    }

    #[test]
    fn array_views() {
        let a = PvValue::FloatArray(Arc::from([1.0, 2.0, 3.0].as_slice()));
        assert!(a.is_array());
        assert_eq!(a.len(), 3);
        assert!(!a.is_empty());
        assert_eq!(a.as_f64_slice(), Some([1.0, 2.0, 3.0].as_slice()));
        assert_eq!(a.as_f64(), None);

        let scalar = PvValue::Int(1);
        assert!(!scalar.is_array());
        assert_eq!(scalar.len(), 1);
        assert_eq!(scalar.as_f64_slice(), None);

        let empty = PvValue::IntArray(Arc::from([].as_slice()));
        assert!(empty.is_empty());
    }

    #[test]
    fn effective_severity_is_disconnected_when_not_connected() {
        let mut s = ChannelState {
            connected: false,
            severity: AlarmSeverity::Minor,
            ..Default::default()
        };
        assert_eq!(s.effective_severity(), AlarmSeverity::Disconnected);
        s.connected = true;
        assert_eq!(s.effective_severity(), AlarmSeverity::Minor);
    }

    #[test]
    fn default_state_is_disconnected_no_alarm() {
        let s = ChannelState::default();
        assert!(!s.connected);
        assert_eq!(s.severity, AlarmSeverity::NoAlarm);
        assert_eq!(s.effective_severity(), AlarmSeverity::Disconnected);
        assert_eq!(s.stamp, 0);
    }

    /// Build a connected `(Channel, StateWriter)` pair with a dangling pool weak
    /// (the pool prune in `Drop` is a no-op) for exercising the value-event path.
    fn channel_pair() -> (Channel, StateWriter) {
        let (conn, writer, _writes, _listeners, _cancel) = Connection::new(
            crate::address::PvAddress::parse("loc://value_events"),
            RepaintHook::default(),
            Weak::new(),
            "loc://value_events".to_owned(),
        );
        // Keep the write queue / cancel token alive for the test's lifetime by
        // leaking them into the Channel's connection (they live on `conn`); the
        // returned receivers are only dropped here, which is harmless.
        (Channel::new(conn), writer)
    }

    fn drain_values(sub: &ValueSubscription) -> Vec<f64> {
        let mut out = Vec::new();
        sub.drain(|ev| out.push(ev.value.as_f64().expect("numeric")));
        out
    }

    #[test]
    fn post_value_delivers_every_value_to_a_subscriber() {
        let (channel, writer) = channel_pair();
        let sub = channel.subscribe_values(16);
        // Three values posted with no drain between: a snapshot would coalesce
        // these to the last one; the event queue must keep all three, in order.
        for v in [1.0, 2.0, 3.0] {
            writer.post_value(|s| {
                s.connected = true;
                s.value = Some(PvValue::Float(v));
            });
        }
        assert_eq!(drain_values(&sub), vec![1.0, 2.0, 3.0]);
        // post_value itself never dedups — a repeated value still emits an
        // event. Change-gating is the CALLER's job: the PV backends compare
        // against the previous value (PyDM parity) and route unchanged
        // updates through `update` instead.
        writer.post_value(|s| s.value = Some(PvValue::Float(3.0)));
        assert_eq!(drain_values(&sub), vec![3.0]);
        // Drain leaves the queue empty.
        assert_eq!(drain_values(&sub), Vec::<f64>::new());
    }

    #[test]
    fn update_does_not_emit_a_value_event() {
        let (channel, writer) = channel_pair();
        let sub = channel.subscribe_values(16);
        // A connection/metadata-only change must not enqueue a sample, even
        // though a stale value sits in the snapshot.
        writer.post_value(|s| {
            s.connected = true;
            s.value = Some(PvValue::Float(5.0));
        });
        assert_eq!(drain_values(&sub), vec![5.0]);
        writer.update(|s| s.connected = false);
        writer.update(|s| s.write_access = true);
        assert_eq!(drain_values(&sub), Vec::<f64>::new());
    }

    #[test]
    fn subscriber_queue_drops_oldest_past_capacity() {
        let (channel, writer) = channel_pair();
        let sub = channel.subscribe_values(2);
        for v in [1.0, 2.0, 3.0, 4.0] {
            writer.post_value(|s| s.value = Some(PvValue::Float(v)));
        }
        // cap = 2: only the two most recent survive.
        assert_eq!(drain_values(&sub), vec![3.0, 4.0]);
    }

    #[test]
    fn dropped_subscription_is_pruned_and_others_keep_receiving() {
        let (channel, writer) = channel_pair();
        let live = channel.subscribe_values(16);
        let gone = channel.subscribe_values(16);
        drop(gone);
        // Publishing after a subscriber drops must not panic and must still
        // deliver to the live one (the dead queue is pruned on publish).
        writer.post_value(|s| s.value = Some(PvValue::Float(7.0)));
        assert_eq!(drain_values(&live), vec![7.0]);
    }

    #[test]
    fn values_posted_before_subscribing_are_not_replayed() {
        let (channel, writer) = channel_pair();
        writer.post_value(|s| s.value = Some(PvValue::Float(1.0)));
        // Subscribing after the fact starts from empty (no snapshot replay).
        let sub = channel.subscribe_values(16);
        assert_eq!(drain_values(&sub), Vec::<f64>::new());
        writer.post_value(|s| s.value = Some(PvValue::Float(2.0)));
        assert_eq!(drain_values(&sub), vec![2.0]);
    }

    #[test]
    fn value_visible_in_snapshot_is_never_replayed_to_a_concurrent_late_subscriber() {
        // Concurrency regression guard for the `post_value` atomicity fix. The
        // single-threaded `values_posted_before_subscribing_are_not_replayed`
        // above cannot catch the real defect, which is an *interleaving*: pre-
        // fix, `post_value` released the state write lock and only then
        // published the event, so a reader could observe `state.value` in the
        // window between the release and the publish, subscribe there, and
        // receive the already-past value — a spurious strip-chart sample and a
        // `subscribe_values` "not replayed" contract violation. That window is
        // exactly what made `ca_property_event_refreshes_metadata_live` flake
        // under concurrent nextest load. The fix publishes under the write lock,
        // so "value observable in the snapshot ⟹ its event was already fanned
        // out" holds by construction. Spinning a reader against a concurrent
        // poster exercises the interleaving; under the fix it can never leak.
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        for _ in 0..2000 {
            let (channel, writer) = channel_pair();
            let armed = Arc::new(AtomicBool::new(false));
            let poster = {
                let armed = armed.clone();
                std::thread::spawn(move || {
                    // Arm, then post the single initial value; the reader is
                    // already spinning to catch the release→publish window.
                    armed.store(true, Ordering::SeqCst);
                    writer.post_value(|s| {
                        s.connected = true;
                        s.value = Some(PvValue::Float(1.0));
                    });
                })
            };
            while !armed.load(Ordering::SeqCst) {
                std::hint::spin_loop();
            }
            // Subscribe the instant the value becomes observable in the snapshot.
            let sub = loop {
                if channel.read(|s| s.value.is_some()) {
                    break channel.subscribe_values(16);
                }
                std::hint::spin_loop();
            };
            poster.join().expect("poster thread");
            // The value was posted before this subscription existed, so it must
            // never be replayed into it.
            assert!(
                drain_values(&sub).is_empty(),
                "a value observable before subscribe leaked into the new subscriber"
            );
        }
    }
}
