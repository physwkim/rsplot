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
use epics_pva_rs::nt::{NTEnum, NTScalar, NTTable};
use epics_pva_rs::pvdata::ScalarType;
use epics_pva_rs::server_native::{PvaServer, SharedPV, SharedSource};
use epics_pva_rs::{PvField, PvStructure, ScalarValue};
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

#[test]
fn pva_subfield_path_selects_into_the_structure() {
    // An NTTable with one double column `x` = [1.5, 2.5, 3.5]. A PyDM-style
    // address `pva://NAME/x/2` must monitor NAME (netloc only,
    // plugin.py:262-266) and drill the /path keys into the delivered value
    // (get_subfield + the p4p walk, plugin.py:269-280 /
    // p4p_plugin_component.py:262-284).
    let (engine, _server, _server_rt) = pva_engine(|source| {
        let table = NTTable::new().add_column(ScalarType::Double, "x", None);
        let desc = table.build();
        let mut init = table.create();
        let mut cols = PvStructure::new("");
        cols.set(
            "x",
            PvField::ScalarArray(vec![
                ScalarValue::Double(1.5),
                ScalarValue::Double(2.5),
                ScalarValue::Double(3.5),
            ]),
        );
        if let PvField::Structure(s) = &mut init {
            s.set("value", PvField::Structure(cols));
        }
        add_pv(source, "sidm:test:pva:tbl", desc, init);
    });

    // Column + row index → one element.
    let elem = engine
        .connect("pva://sidm:test:pva:tbl/x/2")
        .expect("connect subfield element channel");
    assert!(
        wait_for(
            || matches!(elem.read(|s| s.value.clone()), Some(PvValue::Float(v)) if (v - 3.5).abs() < 1e-9),
            Duration::from_secs(5)
        ),
        "did not observe table element x[2] = 3.5 (got {:?})",
        elem.read(|s| s.value.clone())
    );

    // Column alone → the whole waveform.
    let col = engine
        .connect("pva://sidm:test:pva:tbl/x")
        .expect("connect subfield column channel");
    assert!(
        wait_for(
            || matches!(
                col.read(|s| s.value.clone()),
                Some(PvValue::FloatArray(a)) if a.as_ref() == [1.5, 2.5, 3.5]
            ),
            Duration::from_secs(5)
        ),
        "did not observe column x = [1.5, 2.5, 3.5] (got {:?})",
        col.read(|s| s.value.clone())
    );
}

#[test]
fn pva_rpc_address_calls_the_function_and_publishes_the_result() {
    // PyDM RPC form (p4p_plugin_component.py:58-78): the trailing '&' marks
    // the address as RPC; the args become a typed NTURI request; with no
    // pydm_pollrate the call happens once. The server sums query.lhs +
    // query.rhs and the result's `value` must land in the channel state.
    let (engine, _server, _server_rt) = pva_engine(|source| {
        let desc = NTScalar::new(ScalarType::Int).build();
        let init = NTScalar::new(ScalarType::Int).create();
        let pv = SharedPV::build_mailbox();
        pv.open(desc, init).expect("open rpc pv");
        pv.on_rpc(|_pv, _desc, value| {
            let arg = |name: &str| -> i32 {
                let PvField::Structure(root) = &value else {
                    return 0;
                };
                let Some(PvField::Structure(q)) = root.get_field("query") else {
                    return 0;
                };
                match q.get_field(name) {
                    Some(PvField::Scalar(ScalarValue::Int(v))) => *v,
                    _ => 0,
                }
            };
            let sum = arg("lhs") + arg("rhs");
            let rdesc = NTScalar::new(ScalarType::Int).build();
            let mut rval = NTScalar::new(ScalarType::Int).create();
            if let PvField::Structure(s) = &mut rval {
                s.set("value", PvField::Scalar(ScalarValue::Int(sum)));
            }
            Ok((rdesc, rval))
        });
        source.add("sidm:test:pva:sum", pv);
    });

    let ch = engine
        .connect("pva://sidm:test:pva:sum?lhs=4&rhs=7&")
        .expect("connect rpc channel");

    assert!(
        wait_for(
            || matches!(ch.read(|s| s.value.clone()), Some(PvValue::Int(11))),
            Duration::from_secs(10)
        ),
        "did not observe the RPC result 4 + 7 = 11 (got {:?})",
        ch.read(|s| s.value.clone())
    );
    assert!(
        ch.is_connected(),
        "RPC result must mark the channel connected"
    );
}
