//! `pva://` — EPICS pvAccess backend (feature `pva`).
//!
//! Ports `pydm/data_plugins/epics_plugins/pva_plugin_component.py` (the p4p
//! pvAccess connection) onto [`epics_pva_rs`]. One async task per pooled
//! connection drives a single channel with a [`tokio::select!`] loop over three
//! sources:
//!
//! - **the monitor** ([`PvaClient::pvmonitor_events`]) — a long-running future
//!   whose callback owns a [`StateWriter`] clone and turns each
//!   [`MonitorEvent`] into a [`ChannelState`] update; the future reconnects
//!   internally, so it only returns on a *permanent* close,
//! - **the GUI write queue** — [`crate::Channel::put`] values, and
//! - **cancellation** — fired when the last [`crate::Channel`] drops.
//!
//! Every `MonitorEvent::Data` carries the FULL cumulative NT structure (the
//! client fills unmarked leaves from the prior value), so [`apply_ntscalar`]
//! extracts value + alarm + timestamp + display/control/valueAlarm metadata on
//! every event without tracking deltas. `Connected` un-gates the widget before
//! the first value; `Disconnected`/`Finished` flip `connected` to false while
//! keeping the stale value (PyDM behaviour), which drives
//! [`crate::AlarmSeverity::Disconnected`] styling.
//!
//! The [`PvaClient`] is created lazily on first connect and shared across every
//! `pva://` connection (one client per engine), mirroring PyDM's process-wide
//! p4p context.
//!
//! **Write path:** [`pv_to_pva_put`] routes a queued [`PvValue`] either to the
//! channel's `.value` field (an NTScalar string PUT) or, when the channel was
//! seen to be an NTEnum (its monitor delivered `value.choices`), to
//! `value.index` with the resolved index — a string label is matched against
//! the cached choices first, then taken as a numeric index. There is no local
//! echo: the value only changes when the server confirms through the monitor.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use epics_pva_rs::client_native::PvaClient;
use epics_pva_rs::client_native::ops_v2::{MonitorEvent, MonitorEventMask};
use epics_pva_rs::pvdata::TypedScalarArray;
use epics_pva_rs::{PvField, PvaError, ScalarValue};
use tokio::sync::{OnceCell, mpsc};
use tokio_util::sync::CancellationToken;

use crate::channel::{AlarmSeverity, ChannelState, PvValue, StateWriter};
use crate::data_plugins::{ConnectionCtx, DataPlugin};
use crate::engine::EngineError;

/// The `pva://` data plugin. Holds the lazily-initialized, engine-shared
/// [`PvaClient`] (PyDM's process-wide p4p context).
pub struct PvaPlugin {
    client: Arc<OnceCell<Arc<PvaClient>>>,
    /// A specific pvAccess server to connect to directly (TCP, no UDP search).
    /// `None` for the default plugin (environment-configured search); tests
    /// point this at a loopback `PvaServer`.
    server: Option<SocketAddr>,
}

impl PvaPlugin {
    /// Create the plugin. The PVA client is not built until the first `pva://`
    /// connection (so a plugin-less headless build pays nothing), and resolves
    /// servers via the standard EPICS pvAccess environment.
    pub fn new() -> Self {
        Self {
            client: Arc::new(OnceCell::new()),
            server: None,
        }
    }

    /// Like [`PvaPlugin::new`], but the PVA client connects directly to `server`
    /// over TCP (bypassing UDP search). Used to target a specific server /
    /// gateway / loopback test server without touching process-global env.
    pub fn with_server(server: SocketAddr) -> Self {
        Self {
            client: Arc::new(OnceCell::new()),
            server: Some(server),
        }
    }
}

impl Default for PvaPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl DataPlugin for PvaPlugin {
    fn protocol(&self) -> &'static str {
        "pva"
    }

    fn connect(&self, ctx: ConnectionCtx) -> Result<(), EngineError> {
        let ConnectionCtx {
            writer,
            writes,
            cancel,
            runtime,
            address,
        } = ctx;
        let pv = address.full_address();
        let client = self.client.clone();
        let server = self.server;
        runtime.spawn(run_channel(client, server, pv, writer, writes, cancel));
        Ok(())
    }
}

/// Service one PVA connection until cancelled or the monitor permanently closes.
async fn run_channel(
    client_cell: Arc<OnceCell<Arc<PvaClient>>>,
    server: Option<SocketAddr>,
    pv: String,
    writer: StateWriter,
    mut writes: mpsc::UnboundedReceiver<PvValue>,
    cancel: CancellationToken,
) {
    // One PVA client per engine, created on first use.
    let client = match client_cell
        .get_or_try_init(|| async {
            let client = match server {
                Some(addr) => PvaClient::builder().server_addr(addr).build(),
                None => PvaClient::new()?,
            };
            Ok::<_, PvaError>(Arc::new(client))
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

    // Enum choices cache shared between the monitor callback (which learns them
    // from `value.choices`) and the write branch (which resolves a label or
    // index against them). `None` until an NTEnum value is seen; stays `None`
    // for NTScalar, where the write path takes the `.value` string PUT.
    let choices: Arc<Mutex<Option<Arc<[String]>>>> = Arc::new(Mutex::new(None));

    // The monitor future: its callback owns a writer clone + the choices cache.
    // `pvmonitor_events` reconnects internally and only returns on a permanent
    // close, so it sits in the select for the connection's whole life.
    let monitor = {
        let writer = writer.clone();
        let choices = choices.clone();
        let client = client.clone();
        let pv = pv.clone();
        async move {
            // pvxs defaults mask `Connected`; we want it (to un-gate the widget
            // before the first value) and `Disconnected`/`Finished` (to flip
            // `connected` off), so neither is masked.
            let mask = MonitorEventMask {
                mask_connected: false,
                mask_disconnected: false,
            };
            let callback = move |ev: MonitorEvent| match ev {
                MonitorEvent::Connected => writer.update(|s| s.connected = true),
                MonitorEvent::Data { value, .. } => {
                    if let Some(c) = enum_choices_of(&value) {
                        *choices.lock().expect("pva choices cache poisoned") = Some(c);
                    }
                    writer.update(move |s| apply_ntscalar(s, &value));
                }
                MonitorEvent::Disconnected | MonitorEvent::Finished => {
                    // Keep the stale value (PyDM behaviour); only `connected`
                    // flips, which drives Disconnected styling.
                    writer.update(|s| s.connected = false);
                }
            };
            let _ = client.pvmonitor_events(&pv, mask, callback).await;
        }
    };
    tokio::pin!(monitor);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,

            // The monitor returned: a permanent close (the client is gone).
            // Reflect the dead connection and stop the task.
            _ = &mut monitor => {
                writer.update(|s| s.connected = false);
                break;
            }

            maybe = writes.recv() => match maybe {
                Some(value) => {
                    // Decide the PUT shape against the cached choices, then drop
                    // the lock before the await. No local echo — the value only
                    // changes when the server confirms via the monitor.
                    let put = {
                        let guard = choices.lock().expect("pva choices cache poisoned");
                        pv_to_pva_put(&value, guard.as_deref())
                    };
                    match put {
                        Some(PvaPut::Value(s)) => {
                            let _ = client.pvput(&pv, &s).await;
                        }
                        Some(PvaPut::Field { path, value }) => {
                            let _ = client.pvput_field(&pv, path, &value).await;
                        }
                        None => {}
                    }
                }
                None => break,  // all Channels dropped
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Pure read path: NT structure → ChannelState.
// ---------------------------------------------------------------------------

/// Apply a full NT structure (`NTScalar`/`NTEnum`) to the channel state.
///
/// Every monitor `Data` carries the complete cumulative structure, so this is
/// safe to call per event: it sets `connected`, the value, alarm severity,
/// timestamp, and any present display/control/valueAlarm metadata. Metadata
/// fields absent from the structure are left untouched.
fn apply_ntscalar(s: &mut ChannelState, root: &PvField) {
    s.connected = true;
    if let Some(value) = field(root, "value").and_then(value_to_pv) {
        s.value = Some(value);
    }
    if let Some(sev) = scalar_field(root, "alarm.severity").and_then(scalar_i64) {
        // `from_epics` maps 0/1/2 and clamps everything else to INVALID; clamp
        // the signed wire value into its `u16` domain first.
        s.severity = AlarmSeverity::from_epics(sev.clamp(0, 3) as u16);
    }
    if let Some(ts) = timestamp_of(root) {
        s.timestamp = Some(ts);
    }
    if let Some(choices) = enum_choices_of(root) {
        s.enum_strings = Some(choices);
    }
    if let Some(units) = string_field(root, "display.units").filter(|u| !u.is_empty()) {
        s.units = Some(Arc::from(units));
    }
    if let Some(prec) = scalar_field(root, "display.precision").and_then(scalar_i64) {
        s.precision = Some(prec as i32);
    }
    if let Some(limits) = limit_pair(root, "display.limitLow", "display.limitHigh") {
        s.display_limits = Some(limits);
    }
    if let Some(limits) = limit_pair(root, "control.limitLow", "control.limitHigh") {
        s.ctrl_limits = Some(limits);
    }
    if let Some(limits) = limit_pair(
        root,
        "valueAlarm.lowWarningLimit",
        "valueAlarm.highWarningLimit",
    ) {
        s.warn_limits = Some(limits);
    }
    if let Some(limits) = limit_pair(
        root,
        "valueAlarm.lowAlarmLimit",
        "valueAlarm.highAlarmLimit",
    ) {
        s.alarm_limits = Some(limits);
    }
}

/// Navigate a dotted field path (`"alarm.severity"`, `"display.units"`) from a
/// structure root, returning the leaf field if every segment is a structure.
fn field<'a>(root: &'a PvField, path: &str) -> Option<&'a PvField> {
    let mut cur = root;
    for seg in path.split('.') {
        let PvField::Structure(s) = cur else {
            return None;
        };
        cur = s.get_field(seg)?;
    }
    Some(cur)
}

/// Borrow a dotted path's leaf as a scalar value.
fn scalar_field<'a>(root: &'a PvField, path: &str) -> Option<&'a ScalarValue> {
    match field(root, path)? {
        PvField::Scalar(sv) => Some(sv),
        _ => None,
    }
}

/// Borrow a dotted path's leaf as a string scalar.
fn string_field<'a>(root: &'a PvField, path: &str) -> Option<&'a str> {
    match field(root, path)? {
        PvField::Scalar(ScalarValue::String(s)) => Some(s),
        _ => None,
    }
}

/// Read a `(low, high)` numeric limit pair; `None` unless both are present and
/// numeric.
fn limit_pair(root: &PvField, low: &str, high: &str) -> Option<(f64, f64)> {
    let lo = scalar_field(root, low).and_then(scalar_f64)?;
    let hi = scalar_field(root, high).and_then(scalar_f64)?;
    Some((lo, hi))
}

/// Build a [`SystemTime`] from `timeStamp.secondsPastEpoch` + `nanoseconds`.
/// A non-positive seconds field is treated as "unset" (`None`), matching a
/// freshly-opened NT value whose timestamp is still zero.
fn timestamp_of(root: &PvField) -> Option<SystemTime> {
    let secs = scalar_field(root, "timeStamp.secondsPastEpoch").and_then(scalar_i64)?;
    if secs <= 0 {
        return None;
    }
    let nanos = scalar_field(root, "timeStamp.nanoseconds")
        .and_then(scalar_i64)
        .unwrap_or(0)
        .max(0) as u64;
    Some(UNIX_EPOCH + Duration::from_secs(secs as u64) + Duration::from_nanos(nanos))
}

/// Extract the `value.choices` string list (an NTEnum); `None` for an NTScalar
/// or an enum whose choices have not arrived yet.
fn enum_choices_of(root: &PvField) -> Option<Arc<[String]>> {
    let value = field(root, "value")?;
    let v = string_array_vec(field(value, "choices")?)?;
    (!v.is_empty()).then(|| Arc::from(v))
}

/// Convert the `value` field into a [`PvValue`]. Scalars and arrays map by type;
/// an NTEnum (`value` is a `{index, choices}` structure) becomes
/// [`PvValue::Enum`] with the label resolved from `choices`.
fn value_to_pv(value: &PvField) -> Option<PvValue> {
    match value {
        PvField::Scalar(sv) => Some(scalar_to_pv(sv)),
        PvField::ScalarArray(arr) => Some(scalar_vec_to_pv(arr)),
        PvField::ScalarArrayTyped(t) => Some(typed_array_to_pv(t)),
        // NTEnum: `value` is itself a structure with `index` + `choices`.
        PvField::Structure(_) => {
            let index = scalar_field(value, "index").and_then(scalar_i64)?;
            let index = index.clamp(0, i64::from(u16::MAX)) as u16;
            let label = field(value, "choices")
                .and_then(string_array_vec)
                .and_then(|c| c.get(usize::from(index)).map(|s| Arc::from(s.as_str())));
            Some(PvValue::Enum { index, label })
        }
        _ => None,
    }
}

/// Normalize a scalar pvData value into a [`PvValue`].
fn scalar_to_pv(sv: &ScalarValue) -> PvValue {
    match sv {
        ScalarValue::Boolean(b) => PvValue::Bool(*b),
        ScalarValue::Float(v) => PvValue::Float(f64::from(*v)),
        ScalarValue::Double(v) => PvValue::Float(*v),
        ScalarValue::String(s) => PvValue::Str(Arc::from(s.as_str())),
        ScalarValue::Byte(v) => PvValue::Int(i64::from(*v)),
        ScalarValue::Short(v) => PvValue::Int(i64::from(*v)),
        ScalarValue::Int(v) => PvValue::Int(i64::from(*v)),
        ScalarValue::Long(v) => PvValue::Int(*v),
        ScalarValue::UByte(v) => PvValue::Int(i64::from(*v)),
        ScalarValue::UShort(v) => PvValue::Int(i64::from(*v)),
        ScalarValue::UInt(v) => PvValue::Int(i64::from(*v)),
        ScalarValue::ULong(v) => PvValue::Int(*v as i64),
    }
}

/// Normalize a typed (zero-copy) scalar array into a [`PvValue`] waveform.
/// `UByte` arrays become [`PvValue::Bytes`] (an EPICS `CHAR` waveform / string),
/// matching the CA backend; signed `Byte` arrays stay integer waveforms.
fn typed_array_to_pv(t: &TypedScalarArray) -> PvValue {
    match t {
        TypedScalarArray::Double(a) => PvValue::FloatArray(Arc::from(&a[..])),
        TypedScalarArray::Float(a) => {
            PvValue::FloatArray(a.iter().map(|x| f64::from(*x)).collect::<Vec<_>>().into())
        }
        TypedScalarArray::Long(a) => PvValue::IntArray(Arc::from(&a[..])),
        TypedScalarArray::Int(a) => {
            PvValue::IntArray(a.iter().map(|x| i64::from(*x)).collect::<Vec<_>>().into())
        }
        TypedScalarArray::Short(a) => {
            PvValue::IntArray(a.iter().map(|x| i64::from(*x)).collect::<Vec<_>>().into())
        }
        TypedScalarArray::Byte(a) => {
            PvValue::IntArray(a.iter().map(|x| i64::from(*x)).collect::<Vec<_>>().into())
        }
        TypedScalarArray::UByte(a) => PvValue::Bytes(Arc::from(&a[..])),
        TypedScalarArray::UShort(a) => {
            PvValue::IntArray(a.iter().map(|x| i64::from(*x)).collect::<Vec<_>>().into())
        }
        TypedScalarArray::UInt(a) => {
            PvValue::IntArray(a.iter().map(|x| i64::from(*x)).collect::<Vec<_>>().into())
        }
        TypedScalarArray::ULong(a) => {
            PvValue::IntArray(a.iter().map(|x| *x as i64).collect::<Vec<_>>().into())
        }
        TypedScalarArray::Boolean(a) => {
            PvValue::IntArray(a.iter().map(|x| i64::from(*x)).collect::<Vec<_>>().into())
        }
        TypedScalarArray::String(a) => PvValue::StrArray(Arc::from(&a[..])),
    }
}

/// Normalize a generic (enum-tagged) scalar array into a [`PvValue`] waveform,
/// keyed on the first element's type (pvData arrays are homogeneous).
fn scalar_vec_to_pv(arr: &[ScalarValue]) -> PvValue {
    match arr.first() {
        Some(ScalarValue::String(_)) => {
            PvValue::StrArray(arr.iter().map(|v| v.to_string()).collect::<Vec<_>>().into())
        }
        Some(ScalarValue::Float(_) | ScalarValue::Double(_)) => {
            PvValue::FloatArray(arr.iter().filter_map(scalar_f64).collect::<Vec<_>>().into())
        }
        Some(ScalarValue::UByte(_)) => PvValue::Bytes(
            arr.iter()
                .filter_map(|v| match v {
                    ScalarValue::UByte(b) => Some(*b),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .into(),
        ),
        Some(_) => PvValue::IntArray(arr.iter().filter_map(scalar_i64).collect::<Vec<_>>().into()),
        // An empty array has no element type to key on; default to a float
        // waveform (the common NTScalarArray case).
        None => PvValue::FloatArray(Arc::from(&[][..])),
    }
}

/// Float view of a scalar pvData value; `None` for strings.
fn scalar_f64(sv: &ScalarValue) -> Option<f64> {
    Some(match sv {
        ScalarValue::Boolean(b) => f64::from(*b),
        ScalarValue::Byte(v) => f64::from(*v),
        ScalarValue::Short(v) => f64::from(*v),
        ScalarValue::Int(v) => f64::from(*v),
        ScalarValue::Long(v) => *v as f64,
        ScalarValue::UByte(v) => f64::from(*v),
        ScalarValue::UShort(v) => f64::from(*v),
        ScalarValue::UInt(v) => f64::from(*v),
        ScalarValue::ULong(v) => *v as f64,
        ScalarValue::Float(v) => f64::from(*v),
        ScalarValue::Double(v) => *v,
        ScalarValue::String(_) => return None,
    })
}

/// Integer view of a scalar pvData value (truncating floats); `None` for
/// strings.
fn scalar_i64(sv: &ScalarValue) -> Option<i64> {
    Some(match sv {
        ScalarValue::Boolean(b) => i64::from(*b),
        ScalarValue::Byte(v) => i64::from(*v),
        ScalarValue::Short(v) => i64::from(*v),
        ScalarValue::Int(v) => i64::from(*v),
        ScalarValue::Long(v) => *v,
        ScalarValue::UByte(v) => i64::from(*v),
        ScalarValue::UShort(v) => i64::from(*v),
        ScalarValue::UInt(v) => i64::from(*v),
        ScalarValue::ULong(v) => *v as i64,
        ScalarValue::Float(v) => *v as i64,
        ScalarValue::Double(v) => *v as i64,
        ScalarValue::String(_) => return None,
    })
}

/// Collect a string scalar array (either array representation) into a `Vec`.
fn string_array_vec(value: &PvField) -> Option<Vec<String>> {
    match value {
        PvField::ScalarArray(arr) => Some(arr.iter().map(|v| v.to_string()).collect()),
        PvField::ScalarArrayTyped(TypedScalarArray::String(a)) => Some(a.to_vec()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Pure write path: PvValue → pvAccess PUT.
// ---------------------------------------------------------------------------

/// The decided shape of a pvAccess PUT for one queued [`PvValue`].
#[derive(Debug, PartialEq)]
enum PvaPut {
    /// PUT the channel's `.value` field with this string (NTScalar).
    Value(String),
    /// PUT a single dotted field path with this string (NTEnum `value.index`).
    Field { path: &'static str, value: String },
}

/// Decide how to PUT a [`PvValue`]. When `choices` is `Some`, the channel is a
/// known NTEnum and the value is resolved to an index written to `value.index`;
/// otherwise it is an NTScalar and the value is formatted as a `.value` string.
fn pv_to_pva_put(value: &PvValue, choices: Option<&[String]>) -> Option<PvaPut> {
    match choices {
        Some(labels) => resolve_enum_index(value, labels).map(|idx| PvaPut::Field {
            path: "value.index",
            value: idx.to_string(),
        }),
        None => scalar_put_string(value).map(PvaPut::Value),
    }
}

/// Resolve a write to an enum index: an existing index, a numeric scalar, or a
/// label-string match against `labels` (then a bare numeric string).
fn resolve_enum_index(value: &PvValue, labels: &[String]) -> Option<i64> {
    match value {
        PvValue::Enum { index, .. } => Some(i64::from(*index)),
        PvValue::Int(n) => Some(*n),
        PvValue::Float(f) => Some(*f as i64),
        PvValue::Bool(b) => Some(i64::from(*b)),
        PvValue::Str(s) => labels
            .iter()
            .position(|l| l == s.as_ref())
            .map(|i| i as i64)
            .or_else(|| s.trim().parse::<i64>().ok()),
        // Arrays cannot select an enum.
        _ => None,
    }
}

/// Format a scalar/array [`PvValue`] as the string `op_put` parses against the
/// channel's `.value` descriptor. Arrays are comma-separated tokens (the form
/// `build_put_value` splits on for a scalar array).
fn scalar_put_string(value: &PvValue) -> Option<String> {
    Some(match value {
        PvValue::Int(n) => n.to_string(),
        PvValue::Float(f) => f.to_string(),
        // "1"/"0" parse for both a boolean and a numeric `.value`.
        PvValue::Bool(b) => if *b { "1" } else { "0" }.to_string(),
        PvValue::Str(s) => s.to_string(),
        PvValue::Enum { index, .. } => index.to_string(),
        PvValue::FloatArray(a) => a
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(","),
        PvValue::IntArray(a) => a
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(","),
        PvValue::StrArray(a) => a.join(","),
        PvValue::Bytes(a) => a
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(","),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use epics_pva_rs::PvStructure;

    /// Build an `NTScalar`-shaped structure with the given value/alarm/time.
    fn ntscalar(value: PvField, severity: i32, secs: i64, nanos: i32) -> PvField {
        let mut root = PvStructure::new("epics:nt/NTScalar:1.0");
        root.set("value", value);
        let mut alarm = PvStructure::new("alarm_t");
        alarm.set("severity", PvField::Scalar(ScalarValue::Int(severity)));
        alarm.set("status", PvField::Scalar(ScalarValue::Int(0)));
        root.set("alarm", PvField::Structure(alarm));
        let mut ts = PvStructure::new("time_t");
        ts.set("secondsPastEpoch", PvField::Scalar(ScalarValue::Long(secs)));
        ts.set("nanoseconds", PvField::Scalar(ScalarValue::Int(nanos)));
        root.set("timeStamp", PvField::Structure(ts));
        PvField::Structure(root)
    }

    /// Build an `NTEnum`-shaped structure with the given index + choices.
    fn ntenum(index: i32, choices: &[&str]) -> PvField {
        let mut root = PvStructure::new("epics:nt/NTEnum:1.0");
        let mut value = PvStructure::new("enum_t");
        value.set("index", PvField::Scalar(ScalarValue::Int(index)));
        let arr = choices
            .iter()
            .map(|c| ScalarValue::String((*c).to_string()))
            .collect();
        value.set("choices", PvField::ScalarArray(arr));
        root.set("value", PvField::Structure(value));
        PvField::Structure(root)
    }

    #[test]
    fn scalar_value_maps_and_sets_alarm_and_timestamp() {
        let root = ntscalar(
            PvField::Scalar(ScalarValue::Double(2.5)),
            1,
            1_700_000_000,
            250,
        );
        let mut s = ChannelState::default();
        apply_ntscalar(&mut s, &root);

        assert!(s.connected);
        assert_eq!(s.value, Some(PvValue::Float(2.5)));
        assert_eq!(s.severity, AlarmSeverity::Minor);
        assert_eq!(
            s.timestamp,
            Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000) + Duration::from_nanos(250))
        );
    }

    #[test]
    fn integer_string_and_bool_scalars_map() {
        let mut s = ChannelState::default();
        apply_ntscalar(
            &mut s,
            &ntscalar(PvField::Scalar(ScalarValue::Long(7)), 0, 1, 0),
        );
        assert_eq!(s.value, Some(PvValue::Int(7)));

        let mut s = ChannelState::default();
        apply_ntscalar(
            &mut s,
            &ntscalar(PvField::Scalar(ScalarValue::String("hi".into())), 0, 1, 0),
        );
        assert_eq!(s.value, Some(PvValue::Str(Arc::from("hi"))));

        let mut s = ChannelState::default();
        apply_ntscalar(
            &mut s,
            &ntscalar(PvField::Scalar(ScalarValue::Boolean(true)), 0, 1, 0),
        );
        assert_eq!(s.value, Some(PvValue::Bool(true)));
    }

    #[test]
    fn unset_timestamp_is_none() {
        let root = ntscalar(PvField::Scalar(ScalarValue::Double(1.0)), 0, 0, 0);
        let mut s = ChannelState::default();
        apply_ntscalar(&mut s, &root);
        assert_eq!(s.timestamp, None);
    }

    #[test]
    fn out_of_range_severity_clamps_to_invalid() {
        let root = ntscalar(PvField::Scalar(ScalarValue::Double(1.0)), 9, 1, 0);
        let mut s = ChannelState::default();
        apply_ntscalar(&mut s, &root);
        assert_eq!(s.severity, AlarmSeverity::Invalid);
    }

    #[test]
    fn typed_arrays_map_to_waveforms() {
        let root = ntscalar(
            PvField::ScalarArrayTyped(TypedScalarArray::Double(Arc::from([1.0, 2.0, 3.0]))),
            0,
            1,
            0,
        );
        let mut s = ChannelState::default();
        apply_ntscalar(&mut s, &root);
        assert_eq!(
            s.value,
            Some(PvValue::FloatArray(Arc::from([1.0, 2.0, 3.0].as_slice())))
        );

        let root = ntscalar(
            PvField::ScalarArrayTyped(TypedScalarArray::Long(Arc::from([3_i64, 4]))),
            0,
            1,
            0,
        );
        let mut s = ChannelState::default();
        apply_ntscalar(&mut s, &root);
        assert_eq!(
            s.value,
            Some(PvValue::IntArray(Arc::from([3_i64, 4].as_slice())))
        );

        // A UByte array is a CHAR waveform → raw bytes (matching CA).
        let root = ntscalar(
            PvField::ScalarArrayTyped(TypedScalarArray::UByte(Arc::from([104_u8, 105, 0]))),
            0,
            1,
            0,
        );
        let mut s = ChannelState::default();
        apply_ntscalar(&mut s, &root);
        assert_eq!(
            s.value,
            Some(PvValue::Bytes(Arc::from([104_u8, 105, 0].as_slice())))
        );
    }

    #[test]
    fn generic_scalar_array_maps_by_first_element() {
        let root = ntscalar(
            PvField::ScalarArray(vec![ScalarValue::Double(1.5), ScalarValue::Double(2.5)]),
            0,
            1,
            0,
        );
        let mut s = ChannelState::default();
        apply_ntscalar(&mut s, &root);
        assert_eq!(
            s.value,
            Some(PvValue::FloatArray(Arc::from([1.5, 2.5].as_slice())))
        );
    }

    #[test]
    fn ntenum_value_resolves_index_label_and_caches_choices() {
        let root = ntenum(1, &["Off", "On"]);
        let mut s = ChannelState::default();
        apply_ntscalar(&mut s, &root);

        assert_eq!(
            s.value,
            Some(PvValue::Enum {
                index: 1,
                label: Some(Arc::from("On")),
            })
        );
        assert_eq!(s.enum_strings.as_deref().map(<[String]>::len), Some(2));
        // The write-path cache extraction sees the same choices.
        assert_eq!(
            enum_choices_of(&root).as_deref().map(<[String]>::len),
            Some(2)
        );
    }

    #[test]
    fn ntenum_index_out_of_range_has_no_label() {
        let root = ntenum(5, &["Off", "On"]);
        let mut s = ChannelState::default();
        apply_ntscalar(&mut s, &root);
        assert_eq!(
            s.value,
            Some(PvValue::Enum {
                index: 5,
                label: None,
            })
        );
    }

    #[test]
    fn ntscalar_value_is_not_treated_as_enum() {
        let root = ntscalar(PvField::Scalar(ScalarValue::Double(1.0)), 0, 1, 0);
        assert_eq!(enum_choices_of(&root), None);
    }

    #[test]
    fn display_control_and_valuealarm_metadata_extracted() {
        let mut root_s = PvStructure::new("epics:nt/NTScalar:1.0");
        root_s.set("value", PvField::Scalar(ScalarValue::Double(2.5)));
        let mut display = PvStructure::new("");
        display.set("units", PvField::Scalar(ScalarValue::String("mm".into())));
        display.set("precision", PvField::Scalar(ScalarValue::Int(3)));
        display.set("limitLow", PvField::Scalar(ScalarValue::Double(-10.0)));
        display.set("limitHigh", PvField::Scalar(ScalarValue::Double(10.0)));
        root_s.set("display", PvField::Structure(display));
        let mut control = PvStructure::new("");
        control.set("limitLow", PvField::Scalar(ScalarValue::Double(-9.0)));
        control.set("limitHigh", PvField::Scalar(ScalarValue::Double(9.0)));
        root_s.set("control", PvField::Structure(control));
        let mut va = PvStructure::new("");
        va.set(
            "lowWarningLimit",
            PvField::Scalar(ScalarValue::Double(-5.0)),
        );
        va.set(
            "highWarningLimit",
            PvField::Scalar(ScalarValue::Double(5.0)),
        );
        va.set("lowAlarmLimit", PvField::Scalar(ScalarValue::Double(-8.0)));
        va.set("highAlarmLimit", PvField::Scalar(ScalarValue::Double(8.0)));
        root_s.set("valueAlarm", PvField::Structure(va));
        let root = PvField::Structure(root_s);

        let mut s = ChannelState::default();
        apply_ntscalar(&mut s, &root);

        assert_eq!(s.units.as_deref(), Some("mm"));
        assert_eq!(s.precision, Some(3));
        assert_eq!(s.display_limits, Some((-10.0, 10.0)));
        assert_eq!(s.ctrl_limits, Some((-9.0, 9.0)));
        assert_eq!(s.warn_limits, Some((-5.0, 5.0)));
        assert_eq!(s.alarm_limits, Some((-8.0, 8.0)));
    }

    #[test]
    fn write_scalar_formats_value_string() {
        assert_eq!(
            pv_to_pva_put(&PvValue::Float(2.5), None),
            Some(PvaPut::Value("2.5".to_owned()))
        );
        assert_eq!(
            pv_to_pva_put(&PvValue::Int(7), None),
            Some(PvaPut::Value("7".to_owned()))
        );
        assert_eq!(
            pv_to_pva_put(&PvValue::Bool(true), None),
            Some(PvaPut::Value("1".to_owned()))
        );
        assert_eq!(
            pv_to_pva_put(&PvValue::Str(Arc::from("hi")), None),
            Some(PvaPut::Value("hi".to_owned()))
        );
    }

    #[test]
    fn write_scalar_array_is_comma_separated() {
        assert_eq!(
            pv_to_pva_put(&PvValue::FloatArray(Arc::from([1.0, 2.0].as_slice())), None),
            Some(PvaPut::Value("1,2".to_owned()))
        );
        assert_eq!(
            pv_to_pva_put(&PvValue::IntArray(Arc::from([3_i64, 4].as_slice())), None),
            Some(PvaPut::Value("3,4".to_owned()))
        );
    }

    #[test]
    fn write_enum_routes_index_to_value_index_field() {
        let choices = vec!["Off".to_owned(), "On".to_owned()];
        // A label resolves to its index.
        assert_eq!(
            pv_to_pva_put(&PvValue::Str(Arc::from("On")), Some(&choices)),
            Some(PvaPut::Field {
                path: "value.index",
                value: "1".to_owned(),
            })
        );
        // A bare index carries through.
        assert_eq!(
            pv_to_pva_put(
                &PvValue::Enum {
                    index: 1,
                    label: None,
                },
                Some(&choices)
            ),
            Some(PvaPut::Field {
                path: "value.index",
                value: "1".to_owned(),
            })
        );
        // A numeric string is taken as the index when no label matches.
        assert_eq!(
            pv_to_pva_put(&PvValue::Str(Arc::from("1")), Some(&choices)),
            Some(PvaPut::Field {
                path: "value.index",
                value: "1".to_owned(),
            })
        );
        // A string that is neither a label nor a number is unresolvable.
        assert_eq!(
            pv_to_pva_put(&PvValue::Str(Arc::from("Bogus")), Some(&choices)),
            None
        );
    }
}
