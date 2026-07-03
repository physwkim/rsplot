//! `pva://` round-trips against an in-process EPICS pvAccess server.
//!
//! Each test brings up an [`epics_pva_rs`] `PvaServer::isolated` on a free
//! loopback port serving a `SharedPV`, then builds an [`Engine`] whose PVA
//! plugin connects directly to that server via [`sidm::PvaPlugin::with_server`]
//! (TCP, no UDP search) — keeping the tests self-contained and parallel-safe.
//! No external server is required; it runs in this process.

#![cfg(feature = "pva")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use epics_pva_rs::FieldDesc;
use epics_pva_rs::nt::{NTEnum, NTScalar};
use epics_pva_rs::pvdata::ScalarType;
use epics_pva_rs::server_native::{PvaServer, SharedPV, SharedSource};
use epics_pva_rs::{PvField, ScalarValue};
use sidm::{Engine, PvValue, PvaPlugin};

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

/// Bring up an in-process pvAccess server (PVs added by `build`) plus an engine
/// whose PVA plugin connects straight to it.
///
/// `Engine::new()` builds its OWN runtime, so it must not be created inside the
/// server runtime — `block_on` is used only to construct the server within a
/// runtime context (its tasks are spawned there), and we are back on a plain
/// thread before the engine is built. The returned server + runtime are kept
/// alive for the test's duration (dropping them stops the server).
fn pva_engine(build: impl FnOnce(&SharedSource)) -> (Engine, PvaServer, tokio::runtime::Runtime) {
    let server_rt = tokio::runtime::Runtime::new().expect("server runtime");
    let server = server_rt.block_on(async {
        let source = SharedSource::new();
        build(&source);
        PvaServer::isolated(Arc::new(source)).expect("build isolated pva server")
    });
    // Let the server bind its TCP/UDP sockets before the client connects.
    std::thread::sleep(Duration::from_millis(300));

    let addr = server.tcp_addr();
    let engine = Engine::new();
    // Override the default PVA plugin with one pointed directly at the loopback
    // server — no UDP search, no process-global env, so tests stay parallel-safe.
    engine.register_plugin(Arc::new(PvaPlugin::with_server(addr)));
    (engine, server, server_rt)
}

/// Open a `SharedPV` with the given descriptor + initial value, and register it.
fn add_pv(source: &SharedSource, name: &str, desc: FieldDesc, value: PvField) {
    // `build_mailbox`, not `new`: since 0.21 a plain `SharedPV` rejects PUTs
    // (pvxs parity); the round-trip tests write to these PVs.
    let pv = SharedPV::build_mailbox();
    pv.open(desc, value).expect("open freshly-created SharedPV");
    source.add(name, pv);
}

#[test]
fn pva_roundtrip_monitor_and_put() {
    let (engine, _server, _server_rt) = pva_engine(|source| {
        // Seed an NTScalar double at 1.5.
        let desc = NTScalar::new(ScalarType::Double).build();
        let mut init = NTScalar::new(ScalarType::Double).create();
        if let PvField::Structure(s) = &mut init {
            s.set("value", PvField::Scalar(ScalarValue::Double(1.5)));
        }
        add_pv(source, "sidm:test:pva:ao", desc, init);
    });

    let ch = engine
        .connect("pva://sidm:test:pva:ao")
        .expect("connect pva channel");

    assert!(
        wait_for(|| ch.is_connected(), Duration::from_secs(5)),
        "channel never connected to the in-process pva server"
    );

    // Connect must also publish write access: p4p gives no access-rights
    // signal, so PyDM defaults it to True on connect
    // (p4p_plugin_component.py:233-237). The widget enable gate reads
    // exactly this state — without it every writable widget stays disabled.
    assert!(
        wait_for(|| ch.read(|s| s.write_access), Duration::from_secs(5)),
        "write_access never became true after pva connect"
    );

    // The initial monitor update delivers the seeded value.
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
fn pva_enum_put_via_label_resolves_index() {
    let (engine, _server, _server_rt) = pva_engine(|source| {
        // NTEnum with choices Off/On, initial index 0.
        let desc = NTEnum::new().with_choices(["Off", "On"]).build();
        let init = NTEnum::new().with_choices(["Off", "On"]).create();
        add_pv(source, "sidm:test:pva:bo", desc, init);
    });

    let ch = engine
        .connect("pva://sidm:test:pva:bo")
        .expect("connect pva enum channel");

    assert!(
        wait_for(|| ch.is_connected(), Duration::from_secs(5)),
        "enum channel never connected"
    );

    // Initial index 0 arrives with its resolved label "Off" (proving the
    // choices were delivered and the label resolved).
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
    // cached choices and PUTs `value.index`, and the server echoes index 1.
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
