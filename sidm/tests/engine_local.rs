//! Headless engine tests using the `loc://` plugin — no egui, no IOC.
//!
//! The engine runs its own tokio runtime; these tests drive it purely from the
//! GUI-side `Channel` API and poll the shared state with a small sleep-loop
//! helper (honest and dependency-free).

use std::time::{Duration, Instant};

use sidm::{Engine, PvValue};

/// Poll `cond` until it is true or `timeout` elapses. Returns the final value.
fn wait_for(mut cond: impl FnMut() -> bool, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    cond()
}

#[test]
fn local_channel_connects_with_init_and_accepts_writes() {
    let engine = Engine::new();
    let ch = engine.connect("loc://x?type=float&init=1.5").unwrap();

    assert!(
        wait_for(|| ch.is_connected(), Duration::from_secs(1)),
        "loc channel should connect"
    );
    assert_eq!(ch.read(|s| s.value.clone()), Some(PvValue::Float(1.5)));

    let before = ch.stamp();
    ch.put(PvValue::Float(2.0));
    assert!(
        wait_for(|| ch.stamp() > before, Duration::from_secs(1)),
        "write should bump the stamp"
    );
    assert_eq!(ch.read(|s| s.value.clone()), Some(PvValue::Float(2.0)));
}

#[test]
fn same_loc_name_shares_one_connection() {
    let engine = Engine::new();
    let writer = engine.connect("loc://shared?type=int&init=7").unwrap();
    let reader = engine.connect("loc://shared").unwrap();

    // One pooled connection for the two addresses with the same name.
    assert_eq!(engine.connection_count(), 1);

    assert!(wait_for(
        || writer.is_connected() && reader.is_connected(),
        Duration::from_secs(1)
    ));
    // The bare address sees the first connection's init value.
    assert_eq!(reader.read(|s| s.value.clone()), Some(PvValue::Int(7)));

    let before = reader.stamp();
    writer.put(PvValue::Int(42));
    assert!(wait_for(|| reader.stamp() > before, Duration::from_secs(1)));
    assert_eq!(reader.read(|s| s.value.clone()), Some(PvValue::Int(42)));
}

#[test]
fn dropping_last_channel_closes_pool_entry() {
    let engine = Engine::new();
    let a = engine.connect("loc://drop").unwrap();
    let b = engine.connect("loc://drop").unwrap();
    assert_eq!(engine.connection_count(), 1);

    drop(a);
    assert_eq!(engine.connection_count(), 1, "b keeps the connection alive");

    drop(b);
    assert_eq!(
        engine.connection_count(),
        0,
        "dropping the last channel prunes the pool entry"
    );
}

#[test]
fn reconnecting_after_drop_makes_a_fresh_connection() {
    let engine = Engine::new();
    let a = engine.connect("loc://re?type=int&init=1").unwrap();
    assert!(wait_for(|| a.is_connected(), Duration::from_secs(1)));
    drop(a);
    assert_eq!(engine.connection_count(), 0);

    let b = engine.connect("loc://re?type=int&init=9").unwrap();
    assert_eq!(engine.connection_count(), 1);
    assert!(wait_for(|| b.is_connected(), Duration::from_secs(1)));
    assert_eq!(b.read(|s| s.value.clone()), Some(PvValue::Int(9)));
}

#[test]
fn local_float_write_rederives_precision() {
    // PyDM re-derives a float variable's precision from the written value
    // when no explicit precision is configured (local_plugin.py:377-388).
    let engine = Engine::new();
    let ch = engine.connect("loc://prec?type=float&init=1.5").unwrap();
    assert!(wait_for(|| ch.is_connected(), Duration::from_secs(1)));
    assert_eq!(ch.read(|s| s.precision), Some(1));

    ch.put(PvValue::Float(2.125));
    assert!(
        wait_for(
            || ch.read(|s| s.precision) == Some(3),
            Duration::from_secs(1)
        ),
        "precision should follow the written value (got {:?})",
        ch.read(|s| s.precision)
    );

    // With an explicit precision the derived one must not override it.
    let fixed = engine
        .connect("loc://prec2?type=float&init=1.5&precision=5")
        .unwrap();
    assert!(wait_for(|| fixed.is_connected(), Duration::from_secs(1)));
    fixed.put(PvValue::Float(2.125));
    assert!(
        wait_for(
            || matches!(fixed.read(|s| s.value.clone()), Some(PvValue::Float(v)) if v == 2.125),
            Duration::from_secs(1)
        ),
        "write should land"
    );
    assert_eq!(fixed.read(|s| s.precision), Some(5));
}

#[test]
fn local_array_type_serves_a_waveform() {
    // loc://name?type=array&init=[..] (PyDM local_plugin.py:32, :321-323).
    let engine = Engine::new();
    let ch = engine
        .connect("loc://wave?type=array&init=[1.5, 2.5, 3.5]")
        .unwrap();
    assert!(wait_for(|| ch.is_connected(), Duration::from_secs(1)));
    assert_eq!(
        ch.read(|s| s.value.clone()),
        Some(PvValue::FloatArray(std::sync::Arc::from(
            [1.5, 2.5, 3.5].as_slice()
        )))
    );
}

#[test]
fn unknown_protocol_errors() {
    let engine = Engine::new();
    assert!(engine.connect("zz://nope").is_err());
}

#[test]
fn bare_address_without_default_protocol_errors() {
    let engine = Engine::new();
    assert!(engine.connect("BARE:PV").is_err());
}

#[test]
fn bare_address_uses_default_protocol() {
    let engine = Engine::new();
    engine.set_default_protocol("loc");
    let ch = engine.connect("bare?type=int&init=3").unwrap();
    assert!(wait_for(|| ch.is_connected(), Duration::from_secs(1)));
    assert_eq!(ch.read(|s| s.value.clone()), Some(PvValue::Int(3)));
}
