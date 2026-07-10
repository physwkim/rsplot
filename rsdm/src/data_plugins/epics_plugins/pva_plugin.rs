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
//! client fills unmarked leaves from the prior value), so `apply_ntscalar`
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
//! **Write path:** `pv_to_pva_put` routes a queued [`PvValue`] either to the
//! channel's `.value` field (an NTScalar string PUT) or, when the channel was
//! seen to be an NTEnum (its monitor delivered `value.choices`), to
//! `value.index` with the resolved index — a string label is matched against
//! the cached choices first, then taken as a numeric index. There is no local
//! echo: the value only changes when the server confirms through the monitor.
//!
//! **Address grammar** (PyDM's, `pydm/data_plugins/plugin.py:261-280` +
//! `p4p_plugin_component.py:58-78`):
//!
//! - `pva://NAME` — monitor the channel `NAME`.
//! - `pva://NAME/sub/field` — monitor `NAME` (the netloc only); the `/path`
//!   is a subfield selector drilled into each delivered value (an NTTable
//!   column, a nested struct member, an array element by integer index).
//! - `pva://fn?arg=..&pydm_pollrate=N` (or any address ending in `&`) — an
//!   RPC channel: no monitor; the function is called with an NTURI request
//!   every `N` seconds (once, with a 5 s timeout, when no pollrate is given)
//!   and the result's `value` is published. RPC channels drop writes.

use std::borrow::Cow;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use epics_pva_rs::client_native::PvaClient;
use epics_pva_rs::client_native::ops_v2::{MonitorEvent, MonitorEventMask};
use epics_pva_rs::nt::NTURI;
use epics_pva_rs::pvdata::TypedScalarArray;
use epics_pva_rs::{PvField, PvaError, ScalarValue};
use tokio::sync::{OnceCell, mpsc};
use tokio_util::sync::CancellationToken;

use crate::address::PvAddress;
use crate::channel::{AlarmSeverity, ChannelState, PvValue, StateWriter};
use crate::data_plugins::epics_plugins::pva_codec;
use crate::data_plugins::{ConnectionCtx, DataPlugin};
use crate::engine::EngineError;

/// PyDM's single-shot RPC timeout (seconds) — used as both the call timeout
/// and the effective poll period when no `pydm_pollrate` is given
/// (`DEFAULT_RPC_TIMEOUT = 5.0`, p4p_plugin_component.py:22, applied :96-98).
const DEFAULT_RPC_TIMEOUT: f64 = 5.0;

/// The `pva://` data plugin. Holds the lazily-initialized, engine-shared
/// [`PvaClient`] (PyDM's process-wide p4p context).
pub struct PvaPlugin {
    client: Arc<OnceCell<Arc<PvaClient>>>,
    /// A specific pvAccess server to connect to directly (TCP, no UDP search).
    /// `None` for the default plugin (environment-configured search); tests
    /// point this at a loopback `PvaServer`.
    server: Option<SocketAddr>,
    /// Global read-only mode (`RSDM_READ_ONLY`, read once at construction) —
    /// PyDM's `pydm --read-only` / `data_plugins.is_read_only()`. p4p's
    /// `put_value` warns and drops every write in read-only mode
    /// (p4p_plugin_component.py:409-411).
    read_only: bool,
}

impl PvaPlugin {
    /// Create the plugin. The PVA client is not built until the first `pva://`
    /// connection (so a plugin-less headless build pays nothing), and resolves
    /// servers via the standard EPICS pvAccess environment.
    pub fn new() -> Self {
        Self {
            client: Arc::new(OnceCell::new()),
            server: None,
            read_only: crate::data_plugins::env_read_only(),
        }
    }

    /// Like [`PvaPlugin::new`], but the PVA client connects directly to `server`
    /// over TCP (bypassing UDP search). Used to target a specific server /
    /// gateway / loopback test server without touching process-global env.
    pub fn with_server(server: SocketAddr) -> Self {
        Self {
            client: Arc::new(OnceCell::new()),
            server: Some(server),
            read_only: crate::data_plugins::env_read_only(),
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
            // `pva://` does not reconfigure per listener (see `ConnectionCtx::listeners`).
            listeners: _,
            cancel,
            runtime,
            address,
        } = ctx;
        let client = self.client.clone();
        let server = self.server;
        // RPC form first, like p4p's __init__ (is_rpc_address on the full
        // user-entered channel, p4p_plugin_component.py:69-72; no monitor is
        // created for it, :77-78).
        if is_rpc_address(address.raw()) {
            let rpc = parse_rpc_channel(&address);
            runtime.spawn(run_rpc(client, server, rpc, writer, writes, cancel));
            return Ok(());
        }
        let cfg = ChannelCfg {
            // The monitor name is the NETLOC only (plugin.py:262-266, passed
            // as the address at :291 and used for the monitor at
            // p4p_plugin_component.py:78) …
            pv: address.netloc().to_owned(),
            // … while the /path is a subfield selector, split on '/'
            // (get_subfield, plugin.py:269-280).
            subfield: subfield_keys(&address),
            read_only: self.read_only,
        };
        runtime.spawn(run_channel(client, server, cfg, writer, writes, cancel));
        Ok(())
    }
}

/// Parsed non-RPC channel address: the monitor name plus the subfield keys.
struct ChannelCfg {
    /// The channel to monitor — the address netloc.
    pv: String,
    /// Subfield keys drilled into each delivered value (`Arc` so the monitor
    /// callback can hold its own handle). Empty for a plain `pva://NAME`.
    subfield: Arc<[String]>,
    /// Global read-only mode (see [`PvaPlugin::read_only`]).
    read_only: bool,
}

/// The `/path` component as subfield keys — PyDM's `get_subfield`
/// (plugin.py:269-280): an empty path selects nothing; otherwise the leading
/// `/` is stripped and the rest split on `/`.
fn subfield_keys(address: &PvAddress) -> Arc<[String]> {
    let path = address.path();
    if path.is_empty() {
        Arc::from([])
    } else {
        path[1..].split('/').map(String::from).collect()
    }
}

/// Service one PVA connection until cancelled or the monitor permanently closes.
async fn run_channel(
    client_cell: Arc<OnceCell<Arc<PvaClient>>>,
    server: Option<SocketAddr>,
    cfg: ChannelCfg,
    writer: StateWriter,
    mut writes: mpsc::UnboundedReceiver<PvValue>,
    cancel: CancellationToken,
) {
    let ChannelCfg {
        pv,
        subfield,
        read_only,
    } = cfg;
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
        let subfield = subfield.clone();
        async move {
            // pvxs defaults mask `Connected`; we want it (to un-gate the widget
            // before the first value) and `Disconnected`/`Finished` (to flip
            // `connected` off), so neither is masked.
            let mask = MonitorEventMask {
                mask_connected: false,
                mask_disconnected: false,
            };
            // Warn once per connection about a compressed NTNDArray codec.
            let mut warned_codec = false;
            let pv_cb = pv.clone();
            let callback = move |ev: MonitorEvent| match ev {
                MonitorEvent::Connected { .. } => writer.update(|s| {
                    s.connected = true;
                    // pvAccess exposes no access-rights signal, so PyDM
                    // defaults write access to True on connect ("no way to
                    // get the actual write access value from p4p, so
                    // defaulting to True", p4p_plugin_component.py:233-237).
                    // Without this every writable widget stays permanently
                    // disabled over pva:// (base.rs gates enabled on
                    // state.write_access, which defaults to false).
                    s.write_access = true;
                }),
                MonitorEvent::Data { value, marked, .. } => {
                    if let Some(c) = enum_choices_of(&value) {
                        *choices.lock().expect("pva choices cache poisoned") = Some(c);
                    }
                    // PyDM emits a value only when "value" is in the
                    // monitor's changedSet (p4p_plugin_component.py:241-242,
                    // matching leaves named `value` or `value.*`); a
                    // metadata-only update (alarm / display / timeStamp)
                    // refreshes the state but appends no strip-chart sample.
                    // `marked` is `None` on the first update of a connect
                    // cycle (a complete snapshot with no prior to delta
                    // against) — always a value event, like PyDM's first
                    // callback after `clear_cache`.
                    let value_changed = value_marked(marked.as_ref());
                    // A non-empty codec name marks a compressed NTNDArray.
                    // PyDM decompresses it (`decompress(value)` via
                    // pva_codec, p4p_plugin_component.py:287-290); rsdm
                    // decodes the lz4/bslz4/blosc/jpeg codecs in
                    // `pva_codec`. On any failure (an unknown codec name or
                    // a malformed stream) the value is skipped with a
                    // one-time warning — a deliberate deviation from PyDM,
                    // which logs and then emits the raw compressed bytes
                    // as the value. Metadata flows either way.
                    if let Some(codec) =
                        string_field(&value, "codec.name").filter(|n| !n.is_empty())
                    {
                        let codec = codec.into_owned();
                        // Subfield addressing into a compressed array would
                        // index the decoded flat array; PyDM's subfield walk
                        // predates its decompress call and has no defined
                        // semantics here either — treat as unsupported.
                        let decoded = if subfield.is_empty() {
                            decompressed_array_value(&value)
                        } else {
                            Err("subfield addressing into a compressed \
                                 NTNDArray is not supported"
                                .into())
                        };
                        match decoded {
                            Ok(pv) => {
                                let apply = move |s: &mut ChannelState| {
                                    apply_nt_metadata(s, &value);
                                    s.value = Some(pv);
                                };
                                if value_changed {
                                    writer.post_value(apply);
                                } else {
                                    writer.update(apply);
                                }
                            }
                            Err(err) => {
                                if !warned_codec {
                                    warned_codec = true;
                                    log::warn!(
                                        "pva://{pv_cb}: compressed NTNDArray \
                                         codec {codec:?}: {err}; ignoring the \
                                         array data"
                                    );
                                }
                                writer.update(move |s| apply_nt_metadata(s, &value));
                            }
                        }
                        return;
                    }
                    let sf = subfield.clone();
                    if value_changed {
                        writer.post_value(move |s| apply_ntscalar(s, &value, &sf));
                    } else {
                        writer.update(move |s| apply_ntscalar(s, &value, &sf));
                    }
                }
                MonitorEvent::Disconnected | MonitorEvent::Finished => {
                    // Keep the stale value (PyDM behaviour); only `connected`
                    // flips, which drives Disconnected styling.
                    writer.update(|s| s.connected = false);
                }
            };
            // `None` = the default all-fields pvRequest (the 0.18 behaviour).
            let _ = client.pvmonitor_events(&pv, None, mask, callback).await;
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
                    // p4p's put_value warns and drops every write in global
                    // read-only mode (p4p_plugin_component.py:409-411).
                    if read_only {
                        log::warn!(
                            "pva://{pv}: read-only mode is enabled (RSDM_READ_ONLY), \
                             could not write value: {value:?}"
                        );
                        continue;
                    }
                    // Gate on the published write access — false until the
                    // monitor connects (it becomes true on Connected, since
                    // pvAccess exposes no access-rights signal); p4p's
                    // pre-connect put would just fail inside context.put.
                    if !writer.read(|s| s.write_access) {
                        log::warn!("pva://{pv}: dropping put {value:?}: no write access");
                        continue;
                    }
                    // Subfield writes need the whole-structure rewrite PyDM
                    // does for NTTable (set_value_by_keys + full-table put,
                    // p4p_plugin_component.py:379-408) — that write model is
                    // part of the recorded NTTable deferral (rsdm's value
                    // model is flat by design), so drop with a warning.
                    if !subfield.is_empty() {
                        log::warn!(
                            "pva://{pv}: subfield writes are not supported, \
                             dropping put {value:?} to /{}",
                            subfield.join("/")
                        );
                        continue;
                    }
                    // Decide the PUT shape against the cached choices, then drop
                    // the lock before the await. No local echo — the value only
                    // changes when the server confirms via the monitor. Failed
                    // puts are logged and discarded (PyDM's p4p `put_value` logs
                    // "Unable to put value" and drops the write).
                    let put = {
                        let guard = choices.lock().expect("pva choices cache poisoned");
                        pv_to_pva_put(&value, guard.as_deref())
                    };
                    match put {
                        Some(PvaPut::Value(s)) => {
                            if let Err(e) = client.pvput(&pv, &s).await {
                                log::error!("pva://{pv}: unable to put {s:?}: {e}");
                            }
                        }
                        Some(PvaPut::Field { path, value }) => {
                            if let Err(e) = client.pvput_field(&pv, path, &value).await {
                                log::error!("pva://{pv}: unable to put {value:?} to {path}: {e}");
                            }
                        }
                        None => log::error!(
                            "pva://{pv}: unable to put {value:?}: no PUT shape for this value"
                        ),
                    }
                }
                None => break,  // all Channels dropped
            },
        }
    }
}

// ---------------------------------------------------------------------------
// RPC channels (`pva://fn?arg=..&pydm_pollrate=N`).
// ---------------------------------------------------------------------------

/// A parsed RPC address: function name, typed args, poll rate.
struct RpcChannel {
    /// The RPC channel (function) name — the address netloc.
    function: String,
    /// `(name, value)` request args with types inferred per
    /// `get_arg_datatype` (p4p_plugin_component.py:129-144).
    args: Vec<(String, ScalarValue)>,
    /// Seconds between calls; `0` means a single shot
    /// (poll_rpc_channel, p4p_plugin_component.py:94-98).
    poll_rate: f64,
}

/// PyDM's RPC-address test (p4p_plugin_component.py:200-209): the raw
/// user-entered channel is an RPC iff it ends with `&` or with
/// `&pydm_pollrate=<number>` (regex `(&|\&pydm_pollrate=\d+(\.\d+)?)$`).
fn is_rpc_address(raw: &str) -> bool {
    if raw.ends_with('&') {
        return true;
    }
    match raw.rfind("&pydm_pollrate=") {
        Some(idx) => is_pollrate_number(&raw[idx + "&pydm_pollrate=".len()..]),
        None => false,
    }
}

/// `\d+(\.\d+)?` — the number grammar in PyDM's RPC-address regex.
fn is_pollrate_number(s: &str) -> bool {
    let mut parts = s.splitn(2, '.');
    let int = parts.next().unwrap_or("");
    if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    match parts.next() {
        None => true,
        Some(frac) => !frac.is_empty() && frac.bytes().all(|b| b.is_ascii_digit()),
    }
}

/// Parse an RPC address — PyDM's `parse_rpc_channel`
/// (p4p_plugin_component.py:164-198). Handles the three shapes:
/// `pva://fn?a=1&b=2&pydm_pollrate=10` (query form), `pva://fn&` (no args,
/// no pollrate; the trailing `&` lands in the netloc, :173-174), and
/// `pva://fn&pydm_pollrate=5` (no `?`, so the pollrate lands in the netloc,
/// :176-184). An unparseable pollrate becomes 0 (single shot) where PyDM's
/// `float(...)` would raise and kill the connection.
fn parse_rpc_channel(address: &PvAddress) -> RpcChannel {
    let mut function = address.netloc().to_owned();
    if function.ends_with('&') {
        function.pop();
    }
    let mut poll_rate = 0.0f64;
    let mut args: Vec<(String, ScalarValue)> = Vec::new();
    if let Some(idx) = function.find("&pydm_pollrate=") {
        poll_rate = function[idx + "&pydm_pollrate=".len()..]
            .parse()
            .unwrap_or(0.0);
        function.truncate(function.find('&').unwrap_or(function.len()));
    } else {
        for (k, v) in address.query_params() {
            // parse_qs drops blank values by default (keep_blank_values
            // False), so `a=` contributes nothing.
            if v.is_empty() {
                continue;
            }
            if k == "pydm_pollrate" {
                poll_rate = v.parse().unwrap_or(0.0);
            } else {
                args.push((k, rpc_arg_scalar(&v)));
            }
        }
    }
    RpcChannel {
        function,
        args,
        poll_rate,
    }
}

/// Infer an RPC arg's wire type from its string form — PyDM's
/// `get_arg_datatype` (p4p_plugin_component.py:129-144): int parse → `'i'`
/// (int32), float parse → `'f'` (float32), else string. (PyDM's bool branch
/// is dead code — `"true".lower() == "True"` never holds — so booleans fall
/// through to string there too.)
fn rpc_arg_scalar(v: &str) -> ScalarValue {
    if let Ok(i) = v.parse::<i32>() {
        return ScalarValue::Int(i);
    }
    if let Ok(f) = v.parse::<f32>() {
        return ScalarValue::Float(f);
    }
    ScalarValue::String(v.into())
}

/// Service one RPC channel: call the function every `poll_rate` seconds and
/// publish the result's `value` — PyDM's `poll_rpc_channel`
/// (p4p_plugin_component.py:91-127). With no pollrate the call happens once
/// (with the 5 s default timeout) and the task then just services
/// cancellation. Writes are dropped (p4p's `put_value` returns for RPC
/// channels, :413-414).
async fn run_rpc(
    client_cell: Arc<OnceCell<Arc<PvaClient>>>,
    server: Option<SocketAddr>,
    rpc: RpcChannel,
    writer: StateWriter,
    mut writes: mpsc::UnboundedReceiver<PvValue>,
    cancel: CancellationToken,
) {
    // One PVA client per engine, created on first use (same cell as the
    // monitor channels).
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
            writer.update(|s| s.connected = false);
            return;
        }
    };

    // Pollrate 0 = a single request with the default timeout
    // (p4p_plugin_component.py:94-98).
    let single_shot = rpc.poll_rate == 0.0;
    let poll_rate = if single_shot {
        DEFAULT_RPC_TIMEOUT
    } else {
        rpc.poll_rate
    };
    let period = Duration::from_secs_f64(poll_rate);
    // The NTURI request is invariant across polls; build it once
    // (create_request, p4p_plugin_component.py:146-162).
    let (desc, value) = NTURI::request("pva", &rpc.function, &rpc.args);
    let fn_name = rpc.function;

    loop {
        let started = Instant::now();
        // PyDM bounds each call by the poll rate (`timeout=self._rpc_poll_rate`,
        // p4p_plugin_component.py:105-107).
        let result = tokio::select! {
            _ = cancel.cancelled() => return,
            r = tokio::time::timeout(period, client.pvrpc(&fn_name, &desc, &value)) => r,
        };
        match result {
            Ok(Ok((_, response))) => {
                // Result → connected + emit; only int/float/bool/str results
                // are emitted (emit_for_type, p4p_plugin_component.py:80-89,
                // called at :112-114). A result without a scalar `value`
                // still marks the channel connected (PyDM would crash on
                // `result.value` there; we stay connected without a sample).
                match field(&response, "value").and_then(value_to_pv) {
                    Some(
                        v @ (PvValue::Int(_)
                        | PvValue::Float(_)
                        | PvValue::Bool(_)
                        | PvValue::Str(_)),
                    ) => writer.post_value(move |s| {
                        s.connected = true;
                        s.value = Some(v);
                    }),
                    _ => writer.update(|s| s.connected = true),
                }
            }
            // Call error or timeout: the RPC channel reads as disconnected
            // (p4p_plugin_component.py:108-116).
            _ => writer.update(|s| s.connected = false),
        }

        if single_shot {
            break;
        }
        // Wait out the remainder of the poll period (p4p_plugin_component.py:
        // 121-127 — wall clock here, where PyDM's time.process_time() barely
        // advances during the blocking call and effectively sleeps the full
        // period *after* the call). Writes arriving meanwhile are drained and
        // dropped (p4p's put_value returns for RPC channels, :413-414).
        let wait = period.saturating_sub(started.elapsed());
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep_until(deadline) => break,
                maybe = writes.recv() => match maybe {
                    Some(v) => {
                        log::debug!("pva://{fn_name}: RPC channels drop writes ({v:?})");
                    }
                    None => return,  // all Channels dropped
                },
            }
        }
    }

    // Single shot done: keep draining (and dropping) writes until the last
    // Channel goes away. p4p's put_value silently returns for RPC channels
    // (p4p_plugin_component.py:413-414).
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            maybe = writes.recv() => match maybe {
                Some(v) => log::debug!("pva://{fn_name}: RPC channels drop writes ({v:?})"),
                None => break,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Pure read path: NT structure → ChannelState.
// ---------------------------------------------------------------------------

/// Whether a monitor update's changed-leaf marks include the value field —
/// PyDM's `changed_value == "value" or changed_value.split(".")[0] == "value"`
/// over `changedSet()` (p4p_plugin_component.py:241-242). `None` (the first
/// update of a connect cycle, a full snapshot) counts as changed: PyDM's
/// first callback after `clear_cache` always emits.
fn value_marked(marked: Option<&std::collections::HashSet<String>>) -> bool {
    marked.is_none_or(|m| m.iter().any(|p| p == "value" || p.starts_with("value.")))
}

/// Apply a full NT structure (`NTScalar`/`NTEnum`) to the channel state.
///
/// Every monitor `Data` carries the complete cumulative structure, so this is
/// safe to call per event: it sets `connected`, the value, alarm severity,
/// timestamp, and any present display/control/valueAlarm metadata. Metadata
/// fields absent from the structure are left untouched.
///
/// `subfield` (from a `pva://NAME/sub/field` address) is drilled into the
/// delivered `value` before conversion — PyDM's `nttable_data_location` walk
/// over the emitted value (p4p_plugin_component.py:262-284). When a key does
/// not resolve the value is left untouched (PyDM logs the same condition as
/// an exception and emits nothing).
fn apply_ntscalar(s: &mut ChannelState, root: &PvField, subfield: &[String]) {
    apply_nt_metadata(s, root);
    let extracted = field(root, "value").and_then(|v| {
        if subfield.is_empty() {
            return value_to_pv(v);
        }
        let mut cur = drill(v, &subfield[0])?;
        for key in &subfield[1..] {
            cur = drill(&cur, key)?;
        }
        value_to_pv(&cur)
    });
    if let Some(value) = extracted {
        s.value = Some(value);
    } else if !subfield.is_empty() {
        log::debug!(
            "pva subfield /{} did not resolve to a value",
            subfield.join("/")
        );
    }
}

/// The metadata half of [`apply_ntscalar`]: `connected`, alarm severity,
/// timestamp, enum choices, display/control/valueAlarm metadata — everything
/// except the value. Used alone when a compressed NTNDArray's data must be
/// skipped while its alarm/timestamp still flow.
fn apply_nt_metadata(s: &mut ChannelState, root: &PvField) {
    s.connected = true;
    if let Some(sev) = scalar_field(root, "alarm.severity").and_then(scalar_i64) {
        // `from_epics` maps 0/1/2 and clamps everything else to INVALID; clamp
        // the signed wire value into its `u16` domain first.
        s.severity = AlarmSeverity::from_epics(sev.clamp(0, 3) as u16);
    }
    if let Some(stat) = scalar_field(root, "alarm.status").and_then(scalar_i64) {
        s.status = stat.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16;
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

/// Decode a compressed NTNDArray's `value` into a [`PvValue`] — the rsdm
/// counterpart of PyDM's `pva_codec.decompress` (pva_codec.py:23-60).
///
/// The compressed payload is the union's selected ubyte array
/// (NDPluginCodec stores its output as `ubyteValue`,
/// ntndArrayConverter.cpp:422-429); `codec.parameters` carries the original
/// element type as a pvData `ScalarType` ordinal (:414-419, "The
/// uncompressed data type would be lost … codec.parameters seems like a
/// good place"), and `uncompressedSize` the decoded byte count. The decoded
/// bytes are the producer's native array memory — little-endian on every
/// realistic EPICS host, and PyDM's `np.frombuffer` makes the same
/// assumption. One deliberate deviation: ordinal 9 (`pvFloat`) decodes as
/// f32 — PyDM's ScalarType tuple maps index 9 to the 64-bit Python `float`,
/// which mis-sizes every compressed f32 array.
fn decompressed_array_value(root: &PvField) -> Result<PvValue, String> {
    let payload = match field(root, "value") {
        Some(PvField::Union {
            selector, value, ..
        }) if *selector >= 0 => match &**value {
            PvField::ScalarArrayTyped(TypedScalarArray::UByte(a)) => &a[..],
            _ => return Err("the compressed payload is not a ubyte array".into()),
        },
        _ => return Err("the value is not a selected union".into()),
    };
    let name = string_field(root, "codec.name")
        .unwrap_or_default()
        .into_owned();
    let uncompressed = scalar_field(root, "uncompressedSize")
        .and_then(scalar_i64)
        .ok_or("missing uncompressedSize")?;
    let uncompressed = usize::try_from(uncompressed)
        .map_err(|_| format!("invalid uncompressedSize {uncompressed}"))?;
    // A missing/non-integer `parameters` falls back to the carrier type
    // (ubyte), like PyDM's `data.dtype` fallback (pva_codec.py:36-38).
    let ordinal = match field(root, "codec.parameters") {
        Some(PvField::Variant(v)) => match &v.value {
            PvField::Scalar(sv) => scalar_i64(sv).unwrap_or(5),
            _ => 5,
        },
        _ => 5,
    };
    let (elem_size, kind) = scalar_type_layout(ordinal)?;
    let bytes = pva_codec::decompress(&name, payload, uncompressed, elem_size)?;
    decoded_bytes_to_pv(&bytes, elem_size, kind)
}

/// The original-element kinds a compressed NTNDArray can decode to.
#[derive(Clone, Copy)]
enum NdElemKind {
    Bool,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
}

/// Byte width and kind for a pvData `ScalarType` ordinal (pvData's
/// `enum ScalarType`: pvBoolean=0, pvByte..pvLong=1..4, pvUByte..pvULong=
/// 5..8, pvFloat=9, pvDouble=10; 11 is pvString, which is not an array
/// element type NDPluginCodec can produce).
fn scalar_type_layout(ordinal: i64) -> Result<(usize, NdElemKind), String> {
    use NdElemKind::*;
    Ok(match ordinal {
        0 => (1, Bool),
        1 => (1, I8),
        2 => (2, I16),
        3 => (4, I32),
        4 => (8, I64),
        5 => (1, U8),
        6 => (2, U16),
        7 => (4, U32),
        8 => (8, U64),
        9 => (4, F32),
        10 => (8, F64),
        other => return Err(format!("unsupported ScalarType ordinal {other}")),
    })
}

/// Reinterpret decoded little-endian array memory as a [`PvValue`], with
/// the same type mapping as [`typed_array_to_pv`] (u8 → `Bytes`, floats →
/// `FloatArray`, every integer/bool width → `IntArray`).
fn decoded_bytes_to_pv(
    bytes: &[u8],
    elem_size: usize,
    kind: NdElemKind,
) -> Result<PvValue, String> {
    if !bytes.len().is_multiple_of(elem_size) {
        return Err(format!(
            "decoded {} bytes is not a whole number of {elem_size}-byte elements",
            bytes.len()
        ));
    }
    let ints = |f: fn(&[u8]) -> i64| -> PvValue {
        PvValue::IntArray(
            bytes
                .chunks_exact(elem_size)
                .map(f)
                .collect::<Vec<_>>()
                .into(),
        )
    };
    Ok(match kind {
        NdElemKind::U8 => PvValue::Bytes(Arc::from(bytes)),
        NdElemKind::Bool => ints(|c| i64::from(c[0] != 0)),
        NdElemKind::I8 => ints(|c| i64::from(c[0] as i8)),
        NdElemKind::I16 => ints(|c| i64::from(i16::from_le_bytes(c.try_into().expect("2B")))),
        NdElemKind::U16 => ints(|c| i64::from(u16::from_le_bytes(c.try_into().expect("2B")))),
        NdElemKind::I32 => ints(|c| i64::from(i32::from_le_bytes(c.try_into().expect("4B")))),
        NdElemKind::U32 => ints(|c| i64::from(u32::from_le_bytes(c.try_into().expect("4B")))),
        NdElemKind::I64 => ints(|c| i64::from_le_bytes(c.try_into().expect("8B"))),
        // ULong wraps to i64 like `typed_array_to_pv`'s `*x as i64`.
        NdElemKind::U64 => ints(|c| u64::from_le_bytes(c.try_into().expect("8B")) as i64),
        NdElemKind::F32 => PvValue::FloatArray(
            bytes
                .chunks_exact(4)
                .map(|c| f64::from(f32::from_le_bytes(c.try_into().expect("4B"))))
                .collect::<Vec<_>>()
                .into(),
        ),
        NdElemKind::F64 => PvValue::FloatArray(
            bytes
                .chunks_exact(8)
                .map(|c| f64::from_le_bytes(c.try_into().expect("8B")))
                .collect::<Vec<_>>()
                .into(),
        ),
    })
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

/// One step of the subfield walk (p4p_plugin_component.py:262-284): a string
/// key selects a structure member (an NTTable column, a nested struct field);
/// failing that, a key that parses as an integer indexes into an array.
/// Returns an owned piece — array handles are `Arc`-backed, so the clones are
/// cheap.
fn drill(f: &PvField, key: &str) -> Option<PvField> {
    if let PvField::Structure(s) = f
        && let Some(sub) = s.get_field(key)
    {
        return Some(sub.clone());
    }
    let idx: usize = key.parse().ok()?;
    match f {
        PvField::ScalarArray(a) => a.get(idx).cloned().map(PvField::Scalar),
        PvField::ScalarArrayTyped(t) => typed_array_element(t, idx).map(PvField::Scalar),
        PvField::StructureArray(a) => a.get(idx)?.clone().map(PvField::Structure),
        _ => None,
    }
}

/// One element of a typed scalar array as an owned [`ScalarValue`].
fn typed_array_element(t: &TypedScalarArray, i: usize) -> Option<ScalarValue> {
    Some(match t {
        TypedScalarArray::Boolean(a) => ScalarValue::Boolean(*a.get(i)?),
        TypedScalarArray::Byte(a) => ScalarValue::Byte(*a.get(i)?),
        TypedScalarArray::UByte(a) => ScalarValue::UByte(*a.get(i)?),
        TypedScalarArray::Short(a) => ScalarValue::Short(*a.get(i)?),
        TypedScalarArray::UShort(a) => ScalarValue::UShort(*a.get(i)?),
        TypedScalarArray::Int(a) => ScalarValue::Int(*a.get(i)?),
        TypedScalarArray::UInt(a) => ScalarValue::UInt(*a.get(i)?),
        TypedScalarArray::Long(a) => ScalarValue::Long(*a.get(i)?),
        TypedScalarArray::ULong(a) => ScalarValue::ULong(*a.get(i)?),
        TypedScalarArray::Float(a) => ScalarValue::Float(*a.get(i)?),
        TypedScalarArray::Double(a) => ScalarValue::Double(*a.get(i)?),
        TypedScalarArray::String(a) => ScalarValue::String(a.get(i)?.clone()),
    })
}

/// Borrow a dotted path's leaf as a scalar value.
fn scalar_field<'a>(root: &'a PvField, path: &str) -> Option<&'a ScalarValue> {
    match field(root, path)? {
        PvField::Scalar(sv) => Some(sv),
        _ => None,
    }
}

/// Borrow a dotted path's leaf as a string scalar, rendered as (lossy) UTF-8
/// display text (the wire string is raw bytes with no UTF-8 guarantee).
fn string_field<'a>(root: &'a PvField, path: &str) -> Option<Cow<'a, str>> {
    match field(root, path)? {
        PvField::Scalar(ScalarValue::String(s)) => Some(s.as_str_lossy()),
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
        // NTNDArray (and any union-typed value): the selected variant is the
        // real value — unwrap and recurse. p4p hands PyDM the union already
        // unwrapped (send_new_value sees a plain ndarray and emits it,
        // p4p_plugin_component.py:286-290); a ubyte image lands as
        // `PvValue::Bytes` via the array paths above. `selector == -1` is a
        // null union (no variant selected) — no value.
        PvField::Union {
            selector, value, ..
        } => {
            if *selector < 0 {
                return None;
            }
            value_to_pv(value)
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
        ScalarValue::String(s) => PvValue::Str(Arc::from(s.as_str_lossy())),
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
        TypedScalarArray::String(a) => {
            PvValue::StrArray(a.iter().map(|s| s.as_str_lossy().into_owned()).collect())
        }
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
        PvField::ScalarArrayTyped(TypedScalarArray::String(a)) => {
            Some(a.iter().map(|s| s.as_str_lossy().into_owned()).collect())
        }
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
            .map(|c| ScalarValue::String((*c).into()))
            .collect();
        value.set("choices", PvField::ScalarArray(arr));
        root.set("value", PvField::Structure(value));
        PvField::Structure(root)
    }

    #[test]
    fn value_marked_matches_pydm_changedset_semantics() {
        use std::collections::HashSet;
        let set =
            |paths: &[&str]| -> HashSet<String> { paths.iter().map(|p| (*p).to_owned()).collect() };
        // First update of a connect cycle: full snapshot, always a value.
        assert!(value_marked(None));
        // Scalar value leaf / NTEnum sub-leaf both count.
        assert!(value_marked(Some(&set(&["value"]))));
        assert!(value_marked(Some(&set(&[
            "value.index",
            "timeStamp.userTag"
        ]))));
        // Metadata-only updates do not.
        assert!(!value_marked(Some(&set(&[
            "alarm.severity",
            "timeStamp.secondsPastEpoch"
        ]))));
        assert!(!value_marked(Some(&set(&["display.units"]))));
        // A field merely *named like* value must not match (PyDM splits on
        // '.', so "valueAlarm.lowAlarmLimit" is not a value change).
        assert!(!value_marked(Some(&set(&["valueAlarm.lowAlarmLimit"]))));
        assert!(!value_marked(Some(&set(&[]))));
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
        apply_ntscalar(&mut s, &root, &[]);

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
            &[],
        );
        assert_eq!(s.value, Some(PvValue::Int(7)));

        let mut s = ChannelState::default();
        apply_ntscalar(
            &mut s,
            &ntscalar(PvField::Scalar(ScalarValue::String("hi".into())), 0, 1, 0),
            &[],
        );
        assert_eq!(s.value, Some(PvValue::Str(Arc::from("hi"))));

        let mut s = ChannelState::default();
        apply_ntscalar(
            &mut s,
            &ntscalar(PvField::Scalar(ScalarValue::Boolean(true)), 0, 1, 0),
            &[],
        );
        assert_eq!(s.value, Some(PvValue::Bool(true)));
    }

    #[test]
    fn unset_timestamp_is_none() {
        let root = ntscalar(PvField::Scalar(ScalarValue::Double(1.0)), 0, 0, 0);
        let mut s = ChannelState::default();
        apply_ntscalar(&mut s, &root, &[]);
        assert_eq!(s.timestamp, None);
    }

    #[test]
    fn out_of_range_severity_clamps_to_invalid() {
        let root = ntscalar(PvField::Scalar(ScalarValue::Double(1.0)), 9, 1, 0);
        let mut s = ChannelState::default();
        apply_ntscalar(&mut s, &root, &[]);
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
        apply_ntscalar(&mut s, &root, &[]);
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
        apply_ntscalar(&mut s, &root, &[]);
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
        apply_ntscalar(&mut s, &root, &[]);
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
        apply_ntscalar(&mut s, &root, &[]);
        assert_eq!(
            s.value,
            Some(PvValue::FloatArray(Arc::from([1.5, 2.5].as_slice())))
        );
    }

    #[test]
    fn ntenum_value_resolves_index_label_and_caches_choices() {
        let root = ntenum(1, &["Off", "On"]);
        let mut s = ChannelState::default();
        apply_ntscalar(&mut s, &root, &[]);

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
        apply_ntscalar(&mut s, &root, &[]);
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
        apply_ntscalar(&mut s, &root, &[]);

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

    #[test]
    fn rpc_address_detection_matches_pydm_regex() {
        // PyDM's `(&|\&pydm_pollrate=\d+(\.\d+)?)$` (p4p :200-209).
        assert!(is_rpc_address(
            "pva://pv:call:add?lhs=4&rhs=7&pydm_pollrate=10"
        ));
        assert!(is_rpc_address("pva://pv:call:add&"));
        assert!(is_rpc_address("pva://fn?a=1&"));
        assert!(is_rpc_address("pva://fn&pydm_pollrate=5.5"));
        // The number grammar is \d+(\.\d+)? — a bare trailing dot or a
        // non-number does not match.
        assert!(!is_rpc_address("pva://fn&pydm_pollrate=5."));
        assert!(!is_rpc_address("pva://fn&pydm_pollrate=abc"));
        // Plain monitors, with or without subfield/query, are not RPC.
        assert!(!is_rpc_address("pva://NAME"));
        assert!(!is_rpc_address("pva://NAME/sub/field"));
        assert!(!is_rpc_address("pva://fn?a=1"));
    }

    #[test]
    fn rpc_channel_parsing_matches_pydm() {
        // Query form (the p4p docstring example, :59-65).
        let rpc = parse_rpc_channel(&PvAddress::parse(
            "pva://pv:call:add?lhs=4&rhs=7&pydm_pollrate=10",
        ));
        assert_eq!(rpc.function, "pv:call:add");
        assert_eq!(
            rpc.args,
            vec![
                ("lhs".to_owned(), ScalarValue::Int(4)),
                ("rhs".to_owned(), ScalarValue::Int(7)),
            ]
        );
        assert_eq!(rpc.poll_rate, 10.0);

        // Bare single-shot form: trailing '&' stripped from the netloc
        // (p4p :173-174), no args, pollrate 0.
        let rpc = parse_rpc_channel(&PvAddress::parse("pva://fn&"));
        assert_eq!(rpc.function, "fn");
        assert!(rpc.args.is_empty());
        assert_eq!(rpc.poll_rate, 0.0);

        // No-'?' pollrate form: both land in the netloc (p4p :176-184).
        let rpc = parse_rpc_channel(&PvAddress::parse("pva://fn&pydm_pollrate=5"));
        assert_eq!(rpc.function, "fn");
        assert!(rpc.args.is_empty());
        assert_eq!(rpc.poll_rate, 5.0);

        // Arg typing (get_arg_datatype :129-144): int → int32,
        // float → float32, everything else (including "True" — PyDM's bool
        // branch is dead code) → string. Blank values are dropped like
        // parse_qs does.
        let rpc = parse_rpc_channel(&PvAddress::parse("pva://fn?x=4.5&s=hello&flag=True&a=&"));
        assert_eq!(rpc.function, "fn");
        assert_eq!(
            rpc.args,
            vec![
                ("x".to_owned(), ScalarValue::Float(4.5)),
                ("s".to_owned(), ScalarValue::String("hello".into())),
                ("flag".to_owned(), ScalarValue::String("True".into())),
            ]
        );
        assert_eq!(rpc.poll_rate, 0.0);
    }

    #[test]
    fn subfield_keys_come_from_the_path() {
        // get_subfield (plugin.py:269-280): path split on '/'.
        assert_eq!(
            subfield_keys(&PvAddress::parse("pva://TBL/col/1")).as_ref(),
            ["col".to_owned(), "1".to_owned()]
        );
        assert!(subfield_keys(&PvAddress::parse("pva://NAME")).is_empty());
    }

    #[test]
    fn subfield_drills_into_structure_and_array() {
        // An NTTable-shaped value: a structure of column arrays.
        let mut table = PvStructure::new("");
        table.set(
            "col",
            PvField::ScalarArrayTyped(TypedScalarArray::Double(Arc::from([1.5, 2.5].as_slice()))),
        );
        let mut root = PvStructure::new("epics:nt/NTTable:1.0");
        root.set("value", PvField::Structure(table));
        let root = PvField::Structure(root);

        // Column key selects the array (p4p :262-284 string-key branch).
        let mut s = ChannelState::default();
        apply_ntscalar(&mut s, &root, &["col".to_owned()]);
        assert_eq!(
            s.value,
            Some(PvValue::FloatArray(Arc::from([1.5, 2.5].as_slice())))
        );

        // Column + integer row index selects one element (int-key branch).
        let mut s = ChannelState::default();
        apply_ntscalar(&mut s, &root, &["col".to_owned(), "1".to_owned()]);
        assert_eq!(s.value, Some(PvValue::Float(2.5)));

        // An unresolvable key leaves the value untouched.
        let mut s = ChannelState::default();
        apply_ntscalar(&mut s, &root, &["nope".to_owned()]);
        assert_eq!(s.value, None);
    }

    #[test]
    fn union_value_unwraps_the_selected_variant() {
        // An NTNDArray's `value` is a union of typed arrays
        // (nt/nd_array.rs); the selected variant is the real value. A ubyte
        // image maps to Bytes like every other u8 waveform.
        let union = PvField::Union {
            selector: 5,
            variant_name: "ubyteValue".to_owned(),
            value: Box::new(PvField::ScalarArrayTyped(TypedScalarArray::UByte(
                Arc::from([1u8, 2, 3, 4].as_slice()),
            ))),
        };
        assert_eq!(
            value_to_pv(&union),
            Some(PvValue::Bytes(Arc::from([1u8, 2, 3, 4].as_slice())))
        );

        // A double-array variant maps to a float waveform.
        let union = PvField::Union {
            selector: 10,
            variant_name: "doubleValue".to_owned(),
            value: Box::new(PvField::ScalarArrayTyped(TypedScalarArray::Double(
                Arc::from([0.5, 1.5].as_slice()),
            ))),
        };
        assert_eq!(
            value_to_pv(&union),
            Some(PvValue::FloatArray(Arc::from([0.5, 1.5].as_slice())))
        );

        // A null union (selector -1, no variant selected) has no value.
        let union = PvField::Union {
            selector: -1,
            variant_name: String::new(),
            value: Box::new(PvField::Null),
        };
        assert_eq!(value_to_pv(&union), None);
    }

    /// A compressed-NTNDArray-shaped root: ubyte payload union, codec
    /// name + `parameters` ordinal, `uncompressedSize` — the fields
    /// `decompressed_array_value` consumes (ntndArrayConverter.cpp:410-429).
    fn compressed_ntnd_root(
        codec: &str,
        ordinal: i32,
        uncompressed: usize,
        payload: &[u8],
    ) -> PvField {
        use epics_pva_rs::pvdata::VariantValue;
        let mut codec_s = PvStructure::new("codec_t");
        codec_s.set("name", PvField::Scalar(ScalarValue::String(codec.into())));
        codec_s.set(
            "parameters",
            PvField::Variant(Box::new(VariantValue::scalar(ScalarValue::Int(ordinal)))),
        );
        let mut root = PvStructure::new("epics:nt/NTNDArray:1.0");
        root.set(
            "value",
            PvField::Union {
                selector: 5,
                variant_name: "ubyteValue".to_owned(),
                value: Box::new(PvField::ScalarArrayTyped(TypedScalarArray::UByte(
                    Arc::from(payload),
                ))),
            },
        );
        root.set("codec", PvField::Structure(codec_s));
        root.set(
            "uncompressedSize",
            PvField::Scalar(ScalarValue::Long(uncompressed as i64)),
        );
        PvField::Structure(root)
    }

    #[test]
    fn compressed_ntndarray_lz4_decodes_to_typed_values() {
        // An f32 array (ordinal 9 = pvFloat) — decodes as 4-byte floats,
        // the deviation from PyDM's f64-typed index 9.
        let raw: Vec<f32> = (0..64).map(|i| i as f32 * 0.5).collect();
        let bytes: Vec<u8> = raw.iter().flat_map(|v| v.to_le_bytes()).collect();
        let comp = lz4_flex::block::compress(&bytes);
        let root = compressed_ntnd_root("lz4", 9, bytes.len(), &comp);
        assert_eq!(
            decompressed_array_value(&root).unwrap(),
            PvValue::FloatArray(
                (0..64)
                    .map(|i| f64::from(i as f32 * 0.5))
                    .collect::<Vec<_>>()
                    .into()
            )
        );

        // A ubyte image (ordinal 5) lands as Bytes, like the uncompressed
        // `typed_array_to_pv` path.
        let img: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let comp = lz4_flex::block::compress(&img);
        let root = compressed_ntnd_root("lz4", 5, img.len(), &comp);
        assert_eq!(
            decompressed_array_value(&root).unwrap(),
            PvValue::Bytes(Arc::from(img.as_slice()))
        );

        // An i16 waveform (ordinal 2) becomes an IntArray.
        let vals: Vec<i16> = (0..256).map(|i| (i as i16) - 128).collect();
        let bytes: Vec<u8> = vals.iter().flat_map(|v| v.to_le_bytes()).collect();
        let comp = lz4_flex::block::compress(&bytes);
        let root = compressed_ntnd_root("lz4", 2, bytes.len(), &comp);
        assert_eq!(
            decompressed_array_value(&root).unwrap(),
            PvValue::IntArray(
                vals.iter()
                    .map(|v| i64::from(*v))
                    .collect::<Vec<_>>()
                    .into()
            )
        );
    }

    #[test]
    fn compressed_ntndarray_failures_keep_the_error() {
        // A truncated JPEG stream fails to decode, naming the codec — the
        // plugin turns this into the one-time warn + metadata-only path.
        let root = compressed_ntnd_root("jpeg", 5, 100, &[0xFF, 0xD8]);
        assert!(
            decompressed_array_value(&root)
                .unwrap_err()
                .contains("jpeg")
        );

        // A size lying about the stream is an error, not a torn value.
        let img = vec![7u8; 100];
        let comp = lz4_flex::block::compress(&img);
        let root = compressed_ntnd_root("lz4", 5, 101, &comp);
        assert!(decompressed_array_value(&root).is_err());

        // uncompressedSize not divisible by the element width.
        let comp = lz4_flex::block::compress(&[0u8; 10]);
        let root = compressed_ntnd_root("lz4", 9, 10, &comp);
        assert!(decompressed_array_value(&root).is_err());
    }
}
