//! `ca://` round-trips against an in-process EPICS Channel Access IOC.
//!
//! Each test brings up an [`epics_ca_rs`] `CaServer` on a free loopback port,
//! then builds an [`Engine`] whose CA plugin searches exactly that server via
//! [`rsdm::CaPlugin::with_addresses`] — the programmatic equivalent of
//! `EPICS_CA_ADDR_LIST`. Using the plugin's address list (rather than the
//! process-global `EPICS_CA_*` env vars) keeps the tests parallel-safe. No
//! external IOC is required; the server runs in this process.

#![cfg(feature = "ca")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use epics_base_rs::server::database::PvDatabase;
use epics_ca_rs::EpicsValue;
use epics_ca_rs::server::{CaServer, CaServerBuilder};
use rsdm::{CaPlugin, Engine, PvValue};

/// Poll `cond` until it holds or `timeout` elapses; returns the final result.
fn wait_for(mut cond: impl FnMut() -> bool, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

/// Reserve then release a free localhost TCP port for the `CaServer` to bind.
fn free_port() -> u16 {
    let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("reserve free CA server port");
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    port
}

/// Bring up an in-process IOC on a free loopback port (records configured by
/// `setup`) plus an engine whose CA client searches exactly that server.
///
/// `Engine::new()` builds its OWN runtime, so it must not be created inside the
/// server runtime — `block_on` is used only for setup, and we are back on a
/// plain thread before the engine is built. The returned server runtime is kept
/// alive for the test's duration (dropping it stops the IOC). The returned
/// database handle lets a test mutate server-side PVs at runtime (e.g. post a
/// `DBE_PROPERTY` metadata change).
fn ioc_engine(
    setup: impl FnOnce(CaServerBuilder) -> CaServerBuilder,
) -> (Engine, Arc<PvDatabase>, tokio::runtime::Runtime) {
    let port = free_port();
    let server_rt = tokio::runtime::Runtime::new().expect("server runtime");
    let server = server_rt.block_on(async {
        setup(CaServer::builder().port(port))
            .build()
            .await
            .expect("build in-process CA server")
    });
    let db = server.database().clone();
    server_rt.spawn(async move {
        let _ = server.run().await;
    });
    // Let the server bind its TCP/UDP sockets before the client searches.
    std::thread::sleep(Duration::from_millis(300));

    let addr: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .expect("loopback server address");
    let engine = Engine::new();
    // Override the default CA plugin with one that searches the loopback IOC
    // directly — no process-global EPICS_CA_* env, so tests stay parallel-safe.
    engine.register_plugin(Arc::new(CaPlugin::with_addresses(vec![addr])));
    (engine, db, server_rt)
}

#[test]
fn ca_roundtrip_monitor_and_put() {
    let (engine, _db, _server_rt) = ioc_engine(|b| b.pv("rsdm:test:ao", EpicsValue::Double(1.5)));
    let ch = engine
        .connect("ca://rsdm:test:ao")
        .expect("connect ca channel");

    assert!(
        wait_for(|| ch.is_connected(), Duration::from_secs(5)),
        "channel never connected to the in-process IOC"
    );

    // The metadata fetch / initial monitor delivers the seeded value.
    assert!(
        wait_for(
            || matches!(ch.read(|s| s.value.clone()), Some(PvValue::Float(v)) if (v - 1.5).abs() < 1e-9),
            Duration::from_secs(5)
        ),
        "did not observe the seeded value 1.5 (got {:?})",
        ch.read(|s| s.value.clone())
    );

    // Write back through the GUI→engine queue and observe the echo via monitor.
    ch.put(PvValue::Float(2.5));
    assert!(
        wait_for(
            || matches!(ch.read(|s| s.value.clone()), Some(PvValue::Float(v)) if (v - 2.5).abs() < 1e-9),
            Duration::from_secs(5)
        ),
        "did not observe the written value 2.5 (got {:?})",
        ch.read(|s| s.value.clone())
    );
}

#[test]
fn ca_enum_put_via_label_resolves_index() {
    use epics_base_rs::server::records::bi::BiRecord;

    let (engine, _db, _server_rt) = ioc_engine(|b| {
        let mut rec = BiRecord::new(0);
        rec.znam = "Off".into();
        rec.onam = "On".into();
        b.record("rsdm:test:bi", rec)
    });
    let ch = engine
        .connect("ca://rsdm:test:bi")
        .expect("connect ca enum channel");

    assert!(
        wait_for(|| ch.is_connected(), Duration::from_secs(5)),
        "enum channel never connected"
    );

    // Initial VAL=0 arrives with its resolved state label "Off" (proving the
    // enum strings were fetched and cached on connect).
    assert!(
        wait_for(
            || matches!(
                ch.read(|s| s.value.clone()),
                Some(PvValue::Enum { index: 0, label }) if label.as_deref() == Some("Off")
            ),
            Duration::from_secs(5)
        ),
        "did not observe initial enum index 0 / label Off (got {:?})",
        ch.read(|s| s.value.clone())
    );

    // Write the state LABEL; the write path resolves it to index 1 against the
    // cached enum strings, and the IOC echoes index 1 / label "On".
    ch.put(PvValue::Str(Arc::from("On")));
    assert!(
        wait_for(
            || matches!(
                ch.read(|s| s.value.clone()),
                Some(PvValue::Enum { index: 1, label }) if label.as_deref() == Some("On")
            ),
            Duration::from_secs(5)
        ),
        "did not observe enum index 1 / label On after writing the label (got {:?})",
        ch.read(|s| s.value.clone())
    );
}

#[test]
fn ca_property_event_refreshes_metadata_live() {
    use epics_base_rs::server::snapshot::{DisplayInfo, Snapshot};
    use std::time::SystemTime;

    let (engine, db, server_rt) = ioc_engine(|b| b.pv("rsdm:test:prop", EpicsValue::Double(1.5)));
    let ch = engine
        .connect("ca://rsdm:test:prop")
        .expect("connect ca channel");

    assert!(
        wait_for(|| ch.is_connected(), Duration::from_secs(5)),
        "channel never connected to the in-process IOC"
    );
    assert!(
        wait_for(|| ch.read(|s| s.value.is_some()), Duration::from_secs(5)),
        "initial value never arrived"
    );

    // Subscribe to value events AFTER the initial value: a metadata-only
    // property change must not append a strip-chart sample. PyDM gates every
    // value emit on an actual value change (np.array_equal,
    // pyepics_plugin_component.py:102); the property path re-emits metadata
    // only.
    let values = ch.subscribe_values(16);

    // Change the PV's display metadata at runtime and post DBE_PROPERTY —
    // the server-side equivalent of `caput PV.EGU` / `PV.PREC`. PyDM's
    // pyepics monitor (DBE_VALUE|DBE_ALARM|DBE_PROPERTY) picks this up live
    // via update_ctrl_vars; pre-fix rsdm kept the connect-time metadata
    // until a reconnect.
    let mut snap = Snapshot::new(EpicsValue::Double(1.5), 0, 0, SystemTime::now());
    snap.display = Some(DisplayInfo {
        units: "um".into(),
        precision: 5,
        ..Default::default()
    });
    server_rt
        .block_on(db.post_pv_property("rsdm:test:prop", snap))
        .expect("post DBE_PROPERTY for a simple PV");

    assert!(
        wait_for(
            || ch.read(|s| s.units.as_deref() == Some("um") && s.precision == Some(5)),
            Duration::from_secs(5)
        ),
        "metadata never refreshed after DBE_PROPERTY (units={:?}, precision={:?})",
        ch.read(|s| s.units.clone()),
        ch.read(|s| s.precision)
    );

    // The property event's snapshot carried the SAME value (1.5), so no
    // value event may have been emitted for it (R1-31: alarm/property
    // callbacks with an unchanged value are severity/metadata refreshes,
    // not samples).
    let mut sampled = Vec::new();
    values.drain(|ev| sampled.push(ev.value));
    assert!(
        sampled.is_empty(),
        "metadata-only DBE_PROPERTY emitted value events: {sampled:?}"
    );
}
