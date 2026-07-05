//! Headless engine tests using the `loc://` plugin — no egui, no IOC.
//!
//! The engine runs its own tokio runtime; these tests drive it purely from the
//! GUI-side `Channel` API and poll the shared state with a small sleep-loop
//! helper (honest and dependency-free).

use std::time::{Duration, Instant};

use rsdm::{Engine, PvValue};

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
fn bare_local_is_disconnected_with_no_value() {
    // R3-12: a bare `loc://name` (no type+init) is not config-bearing, so PyDM
    // leaves it disconnected with no value — never a fabricated `0.0`
    // (`_configure_local_plugin` returns early, local_plugin.py:47-61).
    let engine = Engine::new();
    let ch = engine.connect("loc://bare_only").unwrap();

    // It must never connect on its own, and must carry no value.
    assert!(
        !wait_for(|| ch.is_connected(), Duration::from_millis(300)),
        "a bare loc:// must stay disconnected until configured"
    );
    assert_eq!(
        ch.read(|s| s.value.clone()),
        None,
        "a bare loc:// must not fabricate a value"
    );
}

#[test]
fn partial_local_missing_a_required_key_is_disconnected() {
    // R3-12: `type` without `init` (or vice versa) is a partial address — PyDM
    // requires both (`_required_config_keys`), so it stays disconnected.
    let engine = Engine::new();
    let type_only = engine.connect("loc://partial_t?type=int").unwrap();
    let init_only = engine.connect("loc://partial_i?init=5").unwrap();

    assert!(
        !wait_for(|| type_only.is_connected(), Duration::from_millis(300)),
        "type without init must stay disconnected"
    );
    assert!(
        !wait_for(|| init_only.is_connected(), Duration::from_millis(300)),
        "init without type must stay disconnected"
    );
    assert_eq!(type_only.read(|s| s.value.clone()), None);
    assert_eq!(init_only.read(|s| s.value.clone()), None);
}

#[test]
fn unparsable_init_connects_with_no_value() {
    // R3-12: a config-bearing address whose `init` cannot be converted connects
    // but carries no value — PyDM's `convert_value` returns `None`
    // (local_plugin.py:318-323), never a fabricated type-zero.
    let engine = Engine::new();
    let ch = engine
        .connect("loc://unparsable?type=int&init=notanint")
        .unwrap();

    assert!(
        wait_for(|| ch.is_connected(), Duration::from_secs(1)),
        "a config-bearing address connects even when init is unparsable"
    );
    assert_eq!(
        ch.read(|s| s.value.clone()),
        None,
        "unparsable init must yield no value, not a type-zero"
    );
}

#[test]
fn config_bearing_listener_configures_a_bare_connection_regardless_of_order() {
    // R3-12 (the primary defect): a bare reader connecting FIRST must not lock
    // the variable disconnected — the first config-bearing listener configures
    // the shared connection whichever order the engine reaches them (PyDM
    // re-runs `_configure_local_plugin` on every `add_listener`, :333-335).
    let engine = Engine::new();

    // Bare reader creates the pooled connection, disconnected with no value.
    let reader = engine.connect("loc://ordered").unwrap();
    assert!(
        !wait_for(|| reader.is_connected(), Duration::from_millis(200)),
        "bare-first reader is disconnected until a config-bearing listener arrives"
    );

    // A later config-bearing declarer configures the same connection.
    let declarer = engine.connect("loc://ordered?type=int&init=5").unwrap();
    assert_eq!(
        engine.connection_count(),
        1,
        "same name → one pooled connection"
    );

    // Both handles now observe the configured, connected value.
    assert!(
        wait_for(
            || reader.is_connected() && declarer.is_connected(),
            Duration::from_secs(1)
        ),
        "the config-bearing listener must connect the bare-first connection"
    );
    assert_eq!(reader.read(|s| s.value.clone()), Some(PvValue::Int(5)));

    // And a write through either handle still flows.
    let before = reader.stamp();
    declarer.put(PvValue::Int(11));
    assert!(wait_for(|| reader.stamp() > before, Duration::from_secs(1)));
    assert_eq!(reader.read(|s| s.value.clone()), Some(PvValue::Int(11)));
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
