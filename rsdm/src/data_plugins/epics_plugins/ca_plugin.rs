//! `ca://` — EPICS Channel Access backend (feature `ca`).
//!
//! Ports `pydm/data_plugins/epics_plugin.py` (the pyepics CA connection) onto
//! [`epics_ca_rs`]. One async task per pooled connection drives a single
//! [`CaChannel`] with a [`tokio::select!`] loop over four sources:
//!
//! - **connection events** ([`CaChannel::connection_events`]) — connect /
//!   disconnect / access-rights / native-type-changed,
//! - **the value monitor** (`subscribe_with_mask`, `DBE_VALUE | DBE_ALARM |
//!   DBE_PROPERTY` — pyepics' `auto_monitor` mask,
//!   `pyepics_plugin_component.py:59-64`) — value + alarm + timestamp,
//! - **the property monitor** (`DBE_PROPERTY` only) — each event triggers a
//!   ctrl-metadata refetch, the `update_ctrl_vars` path
//!   (`pyepics_plugin_component.py:120-177`),
//! - **the GUI write queue** — [`crate::Channel::put`] values, and
//! - **cancellation** — fired when the last [`crate::Channel`] drops.
//!
//! On connect (and reconnect / native-type change) the task issues one
//! `get_with_metadata(DbrClass::Ctrl)` to publish units / precision / limits /
//! enum strings together with the initial value; afterwards a runtime
//! metadata change (`caput PV.PREC` / `.EGU` / mbbo strings) posts a
//! `DBE_PROPERTY` event and the task refetches + re-applies the ctrl
//! metadata, so widgets track it live like PyDM. On disconnect the
//! stale value is kept and only `connected` flips, which drives
//! [`crate::AlarmSeverity::Disconnected`] styling.
//!
//! The [`CaClient`] is created lazily on first connect and shared across every
//! `ca://` connection (one client per engine), mirroring PyDM's process-wide
//! pyepics context.
//!
//! **Write path:** `pv_to_epics` coerces a queued [`PvValue`] to the record's
//! native field type (string→enum label resolution, float→long, number→string),
//! writes are dropped while disconnected, and there is no local echo — the value
//! only changes when the IOC confirms through the monitor. Writes go out as
//! plain `CA_PROTO_WRITE` (`put_nowait`) — the pyepics `PV.put` / MEDM `ca_put`
//! model — never as `WRITE_NOTIFY`, whose completion can be held by the record
//! (busy records hold it until they leave busy) and must not stall this task.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use epics_base_rs::server::snapshot::{DbrClass, Snapshot};
use epics_base_rs::types::{DbFieldType, EpicsValue, PvString};
use epics_ca_rs::CaError;
use epics_ca_rs::client::{CaChannel, CaClient, ConnectionEvent};
use epics_ca_rs::protocol::{DBE_ALARM, DBE_PROPERTY, DBE_VALUE};
use tokio::sync::{OnceCell, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::channel::{AlarmSeverity, ChannelState, PvValue, StateWriter};
use crate::data_plugins::{ConnectionCtx, DataPlugin};
use crate::engine::EngineError;

/// The `ca://` data plugin. Holds the lazily-initialized, engine-shared
/// [`CaClient`] (PyDM's process-wide pyepics context).
pub struct CaPlugin {
    client: Arc<OnceCell<Arc<CaClient>>>,
    /// Extra CA server addresses searched in addition to the environment's
    /// `EPICS_CA_ADDR_LIST` (the programmatic equivalent of that variable).
    /// Empty for the default plugin; tests point this at a loopback IOC.
    addresses: Vec<SocketAddr>,
    /// Global read-only mode (`RSDM_READ_ONLY`, read once at construction) —
    /// PyDM's `pydm --read-only` / `data_plugins.is_read_only()`. Forces the
    /// published `write_access` to false (`send_access_state`,
    /// pyepics_plugin_component.py:179-185), which both disables writable
    /// widgets and gates every put.
    read_only: bool,
}

impl CaPlugin {
    /// Create the plugin. The CA client is not built until the first
    /// `ca://` connection (so a plugin-less headless build pays nothing),
    /// and resolves servers via the standard EPICS environment.
    pub fn new() -> Self {
        Self::with_addresses(Vec::new())
    }

    /// Like [`CaPlugin::new`], but the CA client also searches `addresses`
    /// directly (`add_address`), in addition to the environment's
    /// `EPICS_CA_ADDR_LIST`. The programmatic equivalent of that variable —
    /// used to target a specific IOC / gateway / loopback test server without
    /// touching process-global env.
    pub fn with_addresses(addresses: Vec<SocketAddr>) -> Self {
        Self {
            client: Arc::new(OnceCell::new()),
            addresses,
            read_only: crate::data_plugins::env_read_only(),
        }
    }
}

impl Default for CaPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl DataPlugin for CaPlugin {
    fn protocol(&self) -> &'static str {
        "ca"
    }

    fn connect(&self, ctx: ConnectionCtx) -> Result<(), EngineError> {
        let ConnectionCtx {
            writer,
            writes,
            // `ca://` does not reconfigure per listener (see `ConnectionCtx::listeners`).
            listeners: _,
            cancel,
            runtime,
            address,
        } = ctx;
        let pv = address.full_address();
        let client = self.client.clone();
        let addresses = self.addresses.clone();
        let read_only = self.read_only;
        runtime.spawn(run_channel(
            client, addresses, pv, writer, writes, cancel, read_only,
        ));
        Ok(())
    }
}

/// Service one CA connection until cancelled or the channel shuts down.
async fn run_channel(
    client_cell: Arc<OnceCell<Arc<CaClient>>>,
    addresses: Vec<SocketAddr>,
    pv: String,
    writer: StateWriter,
    mut writes: mpsc::UnboundedReceiver<PvValue>,
    cancel: CancellationToken,
    read_only: bool,
) {
    // One CA client per engine, created on first use.
    let client = match client_cell
        .get_or_try_init(|| async {
            let client = CaClient::new().await?;
            for addr in &addresses {
                client.add_address(*addr);
            }
            Ok::<_, CaError>(Arc::new(client))
        })
        .await
    {
        Ok(c) => c.clone(),
        Err(_) => {
            // Client construction failed — leave the channel disconnected.
            writer.update(|s| s.connected = false);
            return;
        }
    };

    let ch = client.create_channel(&pv);
    let mut events = ch.connection_events();
    // pyepics' auto_monitor mask: DBE_VALUE | DBE_ALARM | DBE_PROPERTY
    // (pyepics_plugin_component.py:59-64). Note DBE_LOG — part of the
    // library's default mask — is deliberately absent: PyDM does not
    // request archive-deadband (ADEL) events, so neither do we.
    let mut monitor = match ch
        .subscribe_with_mask(0.0, DBE_VALUE | DBE_ALARM | DBE_PROPERTY)
        .await
    {
        Ok(m) => m,
        Err(_) => {
            writer.update(|s| s.connected = false);
            return;
        }
    };
    // Property-only subscription: fires when the IOC posts DBE_PROPERTY
    // (a runtime `caput PV.PREC` / `.EGU` / limit / mbbo-string change).
    // The value monitor's snapshots are TIME-class (no ctrl metadata), so
    // this dedicated stream is what tells us to refetch the CTRL metadata —
    // the equivalent of pyepics' CTRL-form monitor feeding
    // `update_ctrl_vars` (pyepics_plugin_component.py:120-177).
    let mut prop_monitor = match ch.subscribe_with_mask(0.0, DBE_PROPERTY).await {
        Ok(m) => m,
        Err(_) => {
            writer.update(|s| s.connected = false);
            return;
        }
    };

    let mut enum_cache: Option<Arc<[String]>> = None;
    // Native field type, learned on connect and used to coerce writes to the
    // record's type (e.g. string→enum index, float→long). `None` until the
    // first metadata fetch; a write before then is coerced by value shape.
    let mut native_type: Option<DbFieldType> = None;
    let mut connected_now = false;
    // Last decoded value, PyDM's `self._value`: a monitor snapshot posts a
    // VALUE event only when the decoded value differs from this
    // (`if value is not None and not np.array_equal(value, self._value)`,
    // pyepics_plugin_component.py:102). Alarm-only (DBE_ALARM) and property
    // snapshots refresh severity/timestamp without emitting a sample.
    // Cleared on disconnect so the first value after a reconnect always
    // emits (PyDM clears the cache in `send_connection_state`,
    // pyepics_plugin_component.py:192-199).
    let mut last_value: Option<PvValue> = None;

    // Deterministic first-connect trigger. `connection_events` is a broadcast
    // subscribed just above, so a `Connected` posted before that subscribe
    // would be missed. `wait_connected` independently detects an established
    // channel, closing that race; `connected_now` dedups the metadata fetch
    // when both the probe and the `Connected` event fire for one connection.
    let initial = ch.wait_connected(Duration::from_secs(86_400));
    tokio::pin!(initial);
    let mut initial_done = false;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,

            res = &mut initial, if !initial_done => {
                initial_done = true;
                if res.is_ok() && !connected_now {
                    connected_now = true;
                    on_connect(&ch, &writer, &mut enum_cache, &mut native_type, &mut last_value, read_only)
                        .await;
                }
            }

            ev = events.recv() => match ev {
                Ok(ConnectionEvent::Connected) => {
                    if !connected_now {
                        connected_now = true;
                        on_connect(&ch, &writer, &mut enum_cache, &mut native_type, &mut last_value, read_only)
                            .await;
                    }
                }
                Ok(ConnectionEvent::Disconnected | ConnectionEvent::Unresponsive) => {
                    connected_now = false;
                    // Forget the value cache so the first value after a
                    // reconnect always emits (PyDM clear_cache parity).
                    last_value = None;
                    // Keep the stale value (PyDM behaviour); only `connected`
                    // flips, which drives Disconnected styling.
                    writer.update(|s| s.connected = false);
                }
                Ok(ConnectionEvent::AccessRightsChanged { write, .. }) => {
                    // Global read-only mode forces the published access to
                    // false regardless of the CA right — PyDM's
                    // send_access_state emits False and returns when
                    // is_read_only() (pyepics_plugin_component.py:179-185).
                    writer.update(move |s| s.write_access = write && !read_only);
                }
                Ok(ConnectionEvent::NativeTypeChanged { .. }) => {
                    // Record type changed under us — refetch metadata (units,
                    // enum strings, limits) against the new native type.
                    on_connect(&ch, &writer, &mut enum_cache, &mut native_type, &mut last_value, read_only)
                        .await;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },

            snap = monitor.recv() => match snap {
                Some(Ok(snap)) => {
                    connected_now = true;
                    let value = epics_to_pv(&snap.value, enum_cache.as_deref());
                    if last_value.as_ref() != Some(&value) {
                        // A NEW value: post it so value-event subscribers
                        // (strip charts) get one sample per actual change —
                        // PyDM gates every new_value emit on
                        // `not np.array_equal(value, self._value)`
                        // (pyepics_plugin_component.py:102).
                        last_value = Some(value.clone());
                        writer.post_value(move |s| apply_value(s, &snap, value));
                    } else {
                        // Same value (an alarm-only DBE_ALARM callback or a
                        // property snapshot): refresh severity/timestamp in
                        // the state without emitting a sample, like PyDM's
                        // severity-only signal path.
                        writer.update(move |s| apply_alarm(s, &snap));
                    }
                }
                Some(Err(_)) => {}  // transient monitor error; keep the connection
                None => break,      // subscription ended (channel shutdown)
            },

            prop = prop_monitor.recv() => match prop {
                Some(Ok(_)) => {
                    // DBE_PROPERTY: metadata changed on the IOC. The event's
                    // TIME-class snapshot carries no ctrl metadata, so
                    // refetch it and re-apply — PyDM's `update_ctrl_vars`
                    // re-emits precision/units/enum_strs/limits whenever a
                    // property event delivers a change
                    // (pyepics_plugin_component.py:120-177).
                    on_property_change(&ch, &writer, &mut enum_cache).await;
                }
                Some(Err(_)) => {}  // transient monitor error; keep the connection
                None => break,      // subscription ended (channel shutdown)
            },

            maybe = writes.recv() => match maybe {
                Some(value) => {
                    // CA cannot honour a write on a disconnected channel;
                    // log and discard (PyDM `put_value` logs the failure and
                    // drops the write). No local echo — the value only
                    // changes when the IOC confirms via the monitor.
                    //
                    // Fire-and-forget plain write (CA_PROTO_WRITE), matching
                    // pyepics `PV.put` (PyDM), MEDM's `ca_put`, and `caput`. A
                    // WRITE_NOTIFY (`put`) completes only when the record
                    // finishes processing — a busy record (areaDetector
                    // `Acquire`) holds that until acquisition ends, and
                    // awaiting it here froze this whole select loop: monitor
                    // updates stalled and queued writes (the Stop press)
                    // never reached the wire.
                    if !connected_now {
                        log::warn!("ca://{pv}: unable to put {value:?}: channel disconnected");
                    } else if !writer.read(|s| s.write_access) {
                        // PyDM's put_value drops the write when is_read_only()
                        // or the channel lacks write access
                        // (pyepics_plugin_component.py:205-213). Read-only
                        // mode is already folded into the published
                        // write_access, so this one gate covers both; pyepics
                        // drops silently, we log at debug for diagnosability.
                        log::debug!("ca://{pv}: dropping put {value:?}: no write access");
                    } else {
                        match pv_to_epics(&value, native_type, enum_cache.as_deref()) {
                            Some(ev) => {
                                if let Err(e) = ch.put_nowait(&ev).await {
                                    log::error!("ca://{pv}: unable to put {value:?}: {e}");
                                }
                            }
                            None => log::error!(
                                "ca://{pv}: unable to put {value:?}: not representable as {native_type:?}"
                            ),
                        }
                    }
                }
                None => break,  // all Channels dropped
            },
        }
    }
}

/// Fetch full control metadata and publish it (plus the value/alarm) as one
/// update. Caches enum strings (for monitor label resolution) and the native
/// field type (for write coercion).
async fn on_connect(
    ch: &CaChannel,
    writer: &StateWriter,
    enum_cache: &mut Option<Arc<[String]>>,
    native_type: &mut Option<DbFieldType>,
    last_value: &mut Option<PvValue>,
    read_only: bool,
) {
    // The native type is known once connected; cache it for the write path.
    *native_type = ch.native_field_type().ok();
    // Seed access rights at connect time — pyepics re-reads them on every
    // connection (reload_access_state, pyepics_plugin_component.py:187-190,
    // called from send_connection_state :197-198) — so a CA_PROTO_ACCESS_RIGHTS
    // broadcast that raced our event subscription is never missed. Read-only
    // mode forces the published access to false (send_access_state :179-185).
    if let Ok(info) = ch.info().await {
        let write = info.access_rights.write && !read_only;
        writer.update(move |s| s.write_access = write);
    }
    match ch.get_with_metadata(DbrClass::Ctrl).await {
        Ok(snap) => {
            let strings: Option<Arc<[String]>> = snap
                .enums
                .as_ref()
                .filter(|e| !e.strings.is_empty())
                .map(|e| latin1_strings(&e.strings));
            *enum_cache = strings.clone();
            // The connect-time snapshot carries the initial value, so post it as
            // a value event (not a bare snapshot update) — the first strip-chart
            // sample. Always a value event: PyDM clears its value cache on
            // (re)connect, so the first callback after connect emits
            // unconditionally (pyepics_plugin_component.py:192-199 → :102).
            let value = epics_to_pv(&snap.value, strings.as_deref());
            *last_value = Some(value.clone());
            writer.post_value(move |s| apply_metadata(s, &snap, value, strings));
        }
        Err(_) => {
            // Connected, but the metadata read failed; reflect the connection so
            // widgets un-gate and let the monitor stream supply the value.
            writer.update(|s| s.connected = true);
        }
    }
}

/// Refetch ctrl metadata after a `DBE_PROPERTY` event and re-apply it.
///
/// The `update_ctrl_vars` path (pyepics_plugin_component.py:120-177):
/// units / precision / all limits / enum strings are re-published; the
/// value stays untouched (a value change flows through the value monitor),
/// so no spurious value event is emitted. The enum cache is refreshed so
/// the write path resolves labels against the new mbbo strings.
async fn on_property_change(
    ch: &CaChannel,
    writer: &StateWriter,
    enum_cache: &mut Option<Arc<[String]>>,
) {
    // On a transient read failure the current metadata is simply kept.
    if let Ok(snap) = ch.get_with_metadata(DbrClass::Ctrl).await {
        let strings: Option<Arc<[String]>> = snap
            .enums
            .as_ref()
            .filter(|e| !e.strings.is_empty())
            .map(|e| latin1_strings(&e.strings));
        if strings.is_some() {
            *enum_cache = strings.clone();
        }
        writer.update(move |s| apply_property_metadata(s, &snap, strings));
    }
}

/// Apply a `DBR_CTRL_*` snapshot: value + alarm + timestamp + units / precision
/// / limits / enum strings. `value` is the already-decoded snapshot value (the
/// caller also records it as the dedup cache); `enum_strings` is moved into the
/// state.
fn apply_metadata(
    s: &mut ChannelState,
    snap: &Snapshot,
    value: PvValue,
    enum_strings: Option<Arc<[String]>>,
) {
    s.value = Some(value);
    s.enum_strings = enum_strings;
    apply_alarm(s, snap);
    apply_display_control(s, snap);
}

/// Re-apply only the metadata a `DBE_PROPERTY` refetch delivers: severity +
/// units / precision / limits / enum strings (PyDM `update_ctrl_vars`,
/// pyepics_plugin_component.py:120-177). Value and timestamp are left alone
/// so a metadata-only change never looks like a value arrival.
fn apply_property_metadata(
    s: &mut ChannelState,
    snap: &Snapshot,
    enum_strings: Option<Arc<[String]>>,
) {
    s.severity = AlarmSeverity::from_epics(snap.alarm.severity);
    if enum_strings.is_some() {
        s.enum_strings = enum_strings;
    }
    apply_display_control(s, snap);
}

/// Shared display/control application: units, precision, display / warning /
/// alarm limits (`DisplayInfo`) and control limits (`ControlInfo`).
fn apply_display_control(s: &mut ChannelState, snap: &Snapshot) {
    if let Some(d) = &snap.display {
        s.units = (!d.units.is_empty()).then(|| Arc::from(latin1_str(&d.units)));
        s.precision = Some(i32::from(d.precision));
        s.display_limits = Some((d.lower_disp_limit, d.upper_disp_limit));
        s.warn_limits = Some((d.lower_warning_limit, d.upper_warning_limit));
        s.alarm_limits = Some((d.lower_alarm_limit, d.upper_alarm_limit));
    }
    if let Some(c) = &snap.control {
        s.ctrl_limits = Some((c.lower_ctrl_limit, c.upper_ctrl_limit));
    }
}

/// Apply a monitor snapshot that carries a NEW value: value + alarm +
/// timestamp (metadata is fetched on connect and on `DBE_PROPERTY`, not
/// re-published on every monitor event). `value` is decoded by the caller,
/// which also uses it for the changed-value gate.
fn apply_value(s: &mut ChannelState, snap: &Snapshot, value: PvValue) {
    s.value = Some(value);
    apply_alarm(s, snap);
}

/// Apply a monitor snapshot whose value is unchanged: connected + alarm +
/// timestamp only. The PyDM counterpart of an alarm-only callback, which
/// re-emits severity but no value (`send_new_value` gates the value emit on
/// `np.array_equal`, pyepics_plugin_component.py:99-102).
fn apply_alarm(s: &mut ChannelState, snap: &Snapshot) {
    s.connected = true;
    s.severity = AlarmSeverity::from_epics(snap.alarm.severity);
    s.timestamp = Some(snap.timestamp.into());
}

/// Normalize an [`EpicsValue`] into a [`PvValue`], resolving an enum label from
/// `enum_strings` when available.
fn epics_to_pv(value: &EpicsValue, enum_strings: Option<&[String]>) -> PvValue {
    match value {
        EpicsValue::String(v) => PvValue::Str(Arc::from(latin1_str(v))),
        EpicsValue::Short(v) => PvValue::Int(i64::from(*v)),
        EpicsValue::Float(v) => PvValue::Float(f64::from(*v)),
        EpicsValue::Enum(i) => PvValue::Enum {
            index: *i,
            label: enum_label(enum_strings, *i),
        },
        // Transient pvalink-only carrier; upstream docs say to read it
        // exactly like `Enum` (the choices are for a server-side put_field).
        EpicsValue::EnumWithChoices { index, .. } => PvValue::Enum {
            index: *index,
            label: enum_label(enum_strings, *index),
        },
        EpicsValue::Char(v) => PvValue::Int(i64::from(*v)),
        EpicsValue::Long(v) => PvValue::Int(i64::from(*v)),
        EpicsValue::Double(v) => PvValue::Float(*v),
        EpicsValue::Int64(v) => PvValue::Int(*v),
        EpicsValue::UInt64(v) => PvValue::Int(*v as i64),
        EpicsValue::UShort(v) => PvValue::Int(i64::from(*v)),
        EpicsValue::ULong(v) => PvValue::Int(i64::from(*v)),
        EpicsValue::UChar(v) => PvValue::Int(i64::from(*v)),
        EpicsValue::ShortArray(a) => {
            PvValue::IntArray(a.iter().map(|x| i64::from(*x)).collect::<Vec<_>>().into())
        }
        EpicsValue::FloatArray(a) => {
            PvValue::FloatArray(a.iter().map(|x| f64::from(*x)).collect::<Vec<_>>().into())
        }
        EpicsValue::EnumArray(a) => {
            PvValue::IntArray(a.iter().map(|x| i64::from(*x)).collect::<Vec<_>>().into())
        }
        EpicsValue::DoubleArray(a) => PvValue::FloatArray(Arc::from(a.as_slice())),
        EpicsValue::LongArray(a) => {
            PvValue::IntArray(a.iter().map(|x| i64::from(*x)).collect::<Vec<_>>().into())
        }
        // Both CHAR waveform shapes stay raw bytes (the formatter decides
        // string vs array); signed/unsigned share the identical byte layout.
        EpicsValue::CharArray(a) | EpicsValue::UCharArray(a) => {
            PvValue::Bytes(Arc::from(a.as_slice()))
        }
        EpicsValue::Int64Array(a) => PvValue::IntArray(Arc::from(a.as_slice())),
        EpicsValue::UInt64Array(a) => {
            PvValue::IntArray(a.iter().map(|x| *x as i64).collect::<Vec<_>>().into())
        }
        EpicsValue::UShortArray(a) => {
            PvValue::IntArray(a.iter().map(|x| i64::from(*x)).collect::<Vec<_>>().into())
        }
        EpicsValue::ULongArray(a) => {
            PvValue::IntArray(a.iter().map(|x| i64::from(*x)).collect::<Vec<_>>().into())
        }
        EpicsValue::StringArray(a) => PvValue::StrArray(latin1_strings(a)),
    }
}

/// Decode one CA wire string ([`PvString`]: raw, not-guaranteed-UTF-8 bytes)
/// as latin-1: every raw byte maps 1:1 to the same U+00XX codepoint, so no
/// byte is ever destroyed. PyDM decodes every CA string this way — it sets
/// pyepics' `utils3.EPICS_STR_ENCODING = "latin-1"`
/// (`pyepics_plugin_component.py:14-19`) — so IOC strings written as latin-1
/// (`µm` 0xB5, `Å` 0xC5, `°C` 0xB0) render as the intended glyphs instead of
/// the U+FFFD a UTF-8-lossy decode would produce.
fn latin1_str(s: &PvString) -> String {
    s.as_bytes().iter().map(|&b| b as char).collect()
}

/// Render CA wire strings into rsdm's `Arc<[String]>` display text via the
/// latin-1 decode of [`latin1_str`] (enum labels, string arrays).
fn latin1_strings(strings: &[PvString]) -> Arc<[String]> {
    strings.iter().map(latin1_str).collect()
}

/// Resolve an enum index to its label string, if `enum_strings` covers it.
fn enum_label(enum_strings: Option<&[String]>, index: u16) -> Option<Arc<str>> {
    enum_strings
        .and_then(|s| s.get(usize::from(index)))
        .map(|label| Arc::from(label.as_str()))
}

/// Coerce a [`PvValue`] write to the record's native field type.
///
/// Scalars are coerced to `native` (e.g. a label string or numeric string to an
/// enum index, a float to a long, a number to the display string). Arrays pass
/// through with their element type (the IOC coerces element types on write,
/// exactly as it does for scalars over the wire). Returns `None` when the value
/// cannot be represented as the target type (e.g. a non-numeric, non-label
/// string written to an enum), in which case the write is dropped.
fn pv_to_epics(
    value: &PvValue,
    native: Option<DbFieldType>,
    enum_strings: Option<&[String]>,
) -> Option<EpicsValue> {
    match value {
        // Waveforms keep their element type; the IOC coerces to the native FTVL.
        PvValue::FloatArray(a) => Some(EpicsValue::DoubleArray(a.to_vec())),
        PvValue::IntArray(a) => Some(EpicsValue::Int64Array(a.to_vec())),
        PvValue::StrArray(a) => Some(EpicsValue::StringArray(
            a.iter().map(PvString::from).collect(),
        )),
        PvValue::Bytes(a) => Some(EpicsValue::CharArray(a.to_vec())),
        scalar => scalar_to_native(scalar, native, enum_strings),
    }
}

/// Coerce a scalar [`PvValue`] to the native field type (see [`pv_to_epics`]).
fn scalar_to_native(
    value: &PvValue,
    native: Option<DbFieldType>,
    enum_strings: Option<&[String]>,
) -> Option<EpicsValue> {
    match native {
        Some(DbFieldType::Enum) => coerce_to_enum(value, enum_strings),
        Some(DbFieldType::String) => Some(EpicsValue::String(scalar_to_string(value).into())),
        Some(DbFieldType::Float) => scalar_f64(value).map(|v| EpicsValue::Float(v as f32)),
        Some(DbFieldType::Short) => scalar_i64(value).map(|v| EpicsValue::Short(v as i16)),
        Some(DbFieldType::UShort) => scalar_i64(value).map(|v| EpicsValue::UShort(v as u16)),
        Some(DbFieldType::Char) => scalar_i64(value).map(|v| EpicsValue::Char(v as u8)),
        Some(DbFieldType::UChar) => scalar_i64(value).map(|v| EpicsValue::UChar(v as u8)),
        Some(DbFieldType::Long) => scalar_i64(value).map(|v| EpicsValue::Long(v as i32)),
        Some(DbFieldType::ULong) => scalar_i64(value).map(|v| EpicsValue::ULong(v as u32)),
        Some(DbFieldType::Int64) => scalar_i64(value).map(EpicsValue::Int64),
        Some(DbFieldType::UInt64) => scalar_i64(value).map(|v| EpicsValue::UInt64(v as u64)),
        Some(DbFieldType::Double) => scalar_f64(value).map(EpicsValue::Double),
        // Native type not yet known (write before metadata): pick the
        // widest-fidelity representation for the value's shape.
        None => untyped_scalar(value),
    }
}

/// Resolve a scalar to an enum index: a label-string match first (PyDM
/// `put` of a state name), then a numeric string / number as the index.
fn coerce_to_enum(value: &PvValue, enum_strings: Option<&[String]>) -> Option<EpicsValue> {
    match value {
        PvValue::Str(s) => {
            if let Some(idx) =
                enum_strings.and_then(|labels| labels.iter().position(|label| label == s.as_ref()))
            {
                return Some(EpicsValue::Enum(idx as u16));
            }
            s.trim().parse::<u16>().ok().map(EpicsValue::Enum)
        }
        PvValue::Enum { index, .. } => Some(EpicsValue::Enum(*index)),
        other => other.as_i64().map(|n| EpicsValue::Enum(n as u16)),
    }
}

/// Float view of a scalar for a write, parsing a string value.
fn scalar_f64(value: &PvValue) -> Option<f64> {
    match value {
        PvValue::Str(s) => s.trim().parse().ok(),
        other => other.as_f64(),
    }
}

/// Integer view of a scalar for a write, parsing a string value (a decimal
/// string falls back through `f64` so `"2.0"` writes to a long as `2`).
fn scalar_i64(value: &PvValue) -> Option<i64> {
    match value {
        PvValue::Str(s) => s
            .trim()
            .parse::<i64>()
            .ok()
            .or_else(|| s.trim().parse::<f64>().ok().map(|f| f as i64)),
        other => other.as_i64(),
    }
}

/// String form of a scalar for a write to a `DBF_STRING` field.
fn scalar_to_string(value: &PvValue) -> String {
    match value {
        PvValue::Str(s) => s.to_string(),
        PvValue::Int(n) => n.to_string(),
        PvValue::Float(f) => f.to_string(),
        PvValue::Bool(b) => i32::from(*b).to_string(),
        PvValue::Enum {
            label: Some(label), ..
        } => label.to_string(),
        PvValue::Enum { index, .. } => index.to_string(),
        // Arrays do not reach here (handled in `pv_to_epics`).
        _ => String::new(),
    }
}

/// Best-effort coercion when the native type is not yet known, preferring the
/// representation that loses the least (i64 over i32, f64 over f32).
fn untyped_scalar(value: &PvValue) -> Option<EpicsValue> {
    Some(match value {
        PvValue::Int(n) => EpicsValue::Int64(*n),
        PvValue::Float(f) => EpicsValue::Double(*f),
        PvValue::Bool(b) => EpicsValue::Long(i32::from(*b)),
        PvValue::Str(s) => EpicsValue::String(s.to_string().into()),
        PvValue::Enum { index, .. } => EpicsValue::Enum(*index),
        // Arrays do not reach here (handled in `pv_to_epics`).
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use epics_base_rs::server::snapshot::{ControlInfo, DisplayInfo, EnumInfo};
    use std::time::{Duration, UNIX_EPOCH};

    fn ts() -> std::time::SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    #[test]
    fn scalars_map_to_normalized_values() {
        assert_eq!(
            epics_to_pv(&EpicsValue::Double(1.5), None),
            PvValue::Float(1.5)
        );
        assert_eq!(epics_to_pv(&EpicsValue::Long(7), None), PvValue::Int(7));
        assert_eq!(epics_to_pv(&EpicsValue::Short(-3), None), PvValue::Int(-3));
        assert_eq!(epics_to_pv(&EpicsValue::Char(65), None), PvValue::Int(65));
        assert_eq!(
            epics_to_pv(&EpicsValue::Int64(1 << 40), None),
            PvValue::Int(1 << 40)
        );
        assert_eq!(
            epics_to_pv(&EpicsValue::Float(0.5), None),
            PvValue::Float(0.5)
        );
        assert_eq!(
            epics_to_pv(&EpicsValue::String("hi".into()), None),
            PvValue::Str(Arc::from("hi"))
        );
    }

    #[test]
    fn enum_resolves_label_from_strings() {
        let strings = vec!["OFF".to_owned(), "ON".to_owned()];
        assert_eq!(
            epics_to_pv(&EpicsValue::Enum(1), Some(&strings)),
            PvValue::Enum {
                index: 1,
                label: Some(Arc::from("ON")),
            }
        );
        // No cache, or index out of range → no label.
        assert_eq!(
            epics_to_pv(&EpicsValue::Enum(1), None),
            PvValue::Enum {
                index: 1,
                label: None,
            }
        );
        assert_eq!(enum_label(Some(&strings), 9), None);
    }

    #[test]
    fn arrays_map_to_typed_waveforms() {
        assert_eq!(
            epics_to_pv(&EpicsValue::DoubleArray(vec![1.0, 2.0]), None),
            PvValue::FloatArray(Arc::from([1.0_f64, 2.0].as_slice()))
        );
        assert_eq!(
            epics_to_pv(&EpicsValue::LongArray(vec![3, 4]), None),
            PvValue::IntArray(Arc::from([3_i64, 4].as_slice()))
        );
        assert_eq!(
            epics_to_pv(&EpicsValue::FloatArray(vec![1.5_f32]), None),
            PvValue::FloatArray(Arc::from([1.5_f64].as_slice()))
        );
        // CHAR waveform stays raw bytes (the formatter decides string vs array).
        assert_eq!(
            epics_to_pv(&EpicsValue::CharArray(vec![104, 105, 0]), None),
            PvValue::Bytes(Arc::from([104_u8, 105, 0].as_slice()))
        );
        assert_eq!(
            epics_to_pv(&EpicsValue::StringArray(vec!["a".into(), "b".into()]), None),
            PvValue::StrArray(Arc::from(["a".to_owned(), "b".to_owned()].as_slice()))
        );
    }

    #[test]
    fn metadata_snapshot_populates_units_precision_and_limits() {
        let mut snap = Snapshot::new(EpicsValue::Double(2.5), 0, 1, ts());
        snap.display = Some(DisplayInfo {
            units: "mm".into(),
            precision: 3,
            lower_disp_limit: -10.0,
            upper_disp_limit: 10.0,
            lower_warning_limit: -5.0,
            upper_warning_limit: 5.0,
            lower_alarm_limit: -8.0,
            upper_alarm_limit: 8.0,
            ..Default::default()
        });
        snap.control = Some(ControlInfo {
            lower_ctrl_limit: -9.0,
            upper_ctrl_limit: 9.0,
        });

        let mut state = ChannelState::default();
        let value = epics_to_pv(&snap.value, None);
        apply_metadata(&mut state, &snap, value, None);

        assert!(state.connected);
        assert_eq!(state.value, Some(PvValue::Float(2.5)));
        assert_eq!(state.severity, AlarmSeverity::Minor);
        assert_eq!(state.units.as_deref(), Some("mm"));
        assert_eq!(state.precision, Some(3));
        assert_eq!(state.display_limits, Some((-10.0, 10.0)));
        assert_eq!(state.warn_limits, Some((-5.0, 5.0)));
        assert_eq!(state.alarm_limits, Some((-8.0, 8.0)));
        assert_eq!(state.ctrl_limits, Some((-9.0, 9.0)));
        assert_eq!(state.timestamp, Some(ts()));
    }

    #[test]
    fn metadata_snapshot_caches_enum_strings_and_resolves_label() {
        let mut snap = Snapshot::new(EpicsValue::Enum(1), 0, 0, ts());
        snap.enums = Some(EnumInfo {
            strings: vec!["OFF".into(), "ON".into()],
        });

        let strings: Option<Arc<[String]>> =
            snap.enums.as_ref().map(|e| latin1_strings(&e.strings));
        let mut state = ChannelState::default();
        let value = epics_to_pv(&snap.value, strings.as_deref());
        apply_metadata(&mut state, &snap, value, strings);

        assert_eq!(
            state.value,
            Some(PvValue::Enum {
                index: 1,
                label: Some(Arc::from("ON")),
            })
        );
        assert_eq!(state.enum_strings.as_deref().map(|s| s.len()), Some(2));
    }

    #[test]
    fn property_metadata_refreshes_without_touching_the_value() {
        // A DBE_PROPERTY refetch must re-apply units/precision/limits/enum
        // strings (PyDM update_ctrl_vars) but never look like a value
        // arrival: value and timestamp stay whatever the value monitor
        // last delivered.
        let mut state = ChannelState {
            connected: true,
            value: Some(PvValue::Float(4.0)),
            units: Some(Arc::from("mm")),
            precision: Some(3),
            timestamp: Some(ts()),
            ..Default::default()
        };
        // The refetched CTRL snapshot carries a (stale) value 9.9 that must
        // NOT overwrite the monitor's 4.0.
        let mut snap = Snapshot::new(EpicsValue::Double(9.9), 0, 1, ts());
        snap.display = Some(DisplayInfo {
            units: "um".into(),
            precision: 5,
            ..Default::default()
        });
        snap.control = Some(ControlInfo {
            lower_ctrl_limit: -1.0,
            upper_ctrl_limit: 1.0,
        });
        apply_property_metadata(&mut state, &snap, None);

        assert_eq!(state.value, Some(PvValue::Float(4.0)));
        assert_eq!(state.timestamp, Some(ts()));
        assert_eq!(state.units.as_deref(), Some("um"));
        assert_eq!(state.precision, Some(5));
        assert_eq!(state.ctrl_limits, Some((-1.0, 1.0)));
        assert_eq!(state.severity, AlarmSeverity::Minor);
    }

    #[test]
    fn latin1_wire_bytes_decode_one_to_one() {
        // 0xB5 is "µ" in latin-1 — the classic accelerator EGU byte. A
        // UTF-8-lossy decode destroys it to U+FFFD; PyDM's latin-1 decode
        // (pyepics EPICS_STR_ENCODING="latin-1") keeps it.
        let mut snap = Snapshot::new(EpicsValue::Double(1.0), 0, 0, ts());
        snap.display = Some(DisplayInfo {
            units: PvString::from_bytes(vec![0xB5, b'm']),
            ..Default::default()
        });
        let mut state = ChannelState::default();
        let value = epics_to_pv(&snap.value, None);
        apply_metadata(&mut state, &snap, value, None);
        assert_eq!(state.units.as_deref(), Some("µm"));

        // String scalar values decode latin-1 too (0xC5 = "Å").
        assert_eq!(
            epics_to_pv(&EpicsValue::String(PvString::from_bytes(vec![0xC5])), None),
            PvValue::Str(Arc::from("Å"))
        );
        // ... and string arrays / enum labels via `latin1_strings`
        // (0xB0 0x43 = "°C").
        let labels = latin1_strings(&[PvString::from_bytes(vec![0xB0, b'C'])]);
        assert_eq!(labels.as_ref(), ["°C".to_owned()]);
    }

    #[test]
    fn monitor_value_keeps_metadata_and_updates_alarm() {
        let mut state = ChannelState {
            units: Some(Arc::from("mm")),
            precision: Some(3),
            ..Default::default()
        };
        let snap = Snapshot::new(EpicsValue::Double(4.0), 0, 2, ts());
        let value = epics_to_pv(&snap.value, None);
        apply_value(&mut state, &snap, value);

        assert!(state.connected);
        assert_eq!(state.value, Some(PvValue::Float(4.0)));
        assert_eq!(state.severity, AlarmSeverity::Major);
        // Connect-time metadata is untouched by a value update.
        assert_eq!(state.units.as_deref(), Some("mm"));
        assert_eq!(state.precision, Some(3));
    }

    #[test]
    fn write_coerces_scalar_to_native_numeric_type() {
        // A float written to a LONG record truncates toward zero.
        assert_eq!(
            pv_to_epics(&PvValue::Float(2.9), Some(DbFieldType::Long), None),
            Some(EpicsValue::Long(2))
        );
        // An i64 written to an INT64 record keeps full width (a LONG would
        // truncate at 2^31).
        assert_eq!(
            pv_to_epics(&PvValue::Int(1 << 40), Some(DbFieldType::Int64), None),
            Some(EpicsValue::Int64(1 << 40))
        );
        // A float to a FLOAT record narrows to f32.
        assert_eq!(
            pv_to_epics(&PvValue::Float(0.5), Some(DbFieldType::Float), None),
            Some(EpicsValue::Float(0.5))
        );
        // A double record takes the value verbatim.
        assert_eq!(
            pv_to_epics(&PvValue::Int(3), Some(DbFieldType::Double), None),
            Some(EpicsValue::Double(3.0))
        );
    }

    #[test]
    fn write_parses_numeric_strings_for_numeric_records() {
        assert_eq!(
            pv_to_epics(
                &PvValue::Str(Arc::from("2.5")),
                Some(DbFieldType::Double),
                None
            ),
            Some(EpicsValue::Double(2.5))
        );
        // A decimal string to a LONG falls through f64 then truncates.
        assert_eq!(
            pv_to_epics(
                &PvValue::Str(Arc::from("7.9")),
                Some(DbFieldType::Long),
                None
            ),
            Some(EpicsValue::Long(7))
        );
        // Non-numeric strings cannot be written to a numeric record.
        assert_eq!(
            pv_to_epics(
                &PvValue::Str(Arc::from("nope")),
                Some(DbFieldType::Double),
                None
            ),
            None
        );
    }

    #[test]
    fn write_resolves_string_label_to_enum_index() {
        let strings = vec!["Off".to_owned(), "On".to_owned()];
        // A label string resolves to its index.
        assert_eq!(
            pv_to_epics(
                &PvValue::Str(Arc::from("On")),
                Some(DbFieldType::Enum),
                Some(&strings)
            ),
            Some(EpicsValue::Enum(1))
        );
        // A numeric string is taken as the index directly when no label matches.
        assert_eq!(
            pv_to_epics(
                &PvValue::Str(Arc::from("1")),
                Some(DbFieldType::Enum),
                Some(&strings)
            ),
            Some(EpicsValue::Enum(1))
        );
        // An index already carried through.
        assert_eq!(
            pv_to_epics(
                &PvValue::Enum {
                    index: 1,
                    label: None
                },
                Some(DbFieldType::Enum),
                Some(&strings)
            ),
            Some(EpicsValue::Enum(1))
        );
        // A string that is neither a label nor a number is unresolvable.
        assert_eq!(
            pv_to_epics(
                &PvValue::Str(Arc::from("Bogus")),
                Some(DbFieldType::Enum),
                Some(&strings)
            ),
            None
        );
    }

    #[test]
    fn write_formats_scalar_for_string_record() {
        assert_eq!(
            pv_to_epics(&PvValue::Int(7), Some(DbFieldType::String), None),
            Some(EpicsValue::String("7".into()))
        );
        assert_eq!(
            pv_to_epics(
                &PvValue::Str(Arc::from("hi")),
                Some(DbFieldType::String),
                None
            ),
            Some(EpicsValue::String("hi".into()))
        );
    }

    #[test]
    fn write_without_known_native_type_uses_widest_representation() {
        // i64 preserved (not narrowed to Long), f64 preserved.
        assert_eq!(
            pv_to_epics(&PvValue::Int(1 << 40), None, None),
            Some(EpicsValue::Int64(1 << 40))
        );
        assert_eq!(
            pv_to_epics(&PvValue::Float(1.5), None, None),
            Some(EpicsValue::Double(1.5))
        );
    }

    #[test]
    fn write_arrays_pass_through_with_element_type() {
        assert_eq!(
            pv_to_epics(
                &PvValue::FloatArray(Arc::from([1.0_f64, 2.0].as_slice())),
                Some(DbFieldType::Double),
                None
            ),
            Some(EpicsValue::DoubleArray(vec![1.0, 2.0]))
        );
        assert_eq!(
            pv_to_epics(
                &PvValue::IntArray(Arc::from([3_i64, 4].as_slice())),
                Some(DbFieldType::Long),
                None
            ),
            Some(EpicsValue::Int64Array(vec![3, 4]))
        );
        assert_eq!(
            pv_to_epics(&PvValue::Bytes(Arc::from([1_u8, 2].as_slice())), None, None),
            Some(EpicsValue::CharArray(vec![1, 2]))
        );
    }
}
