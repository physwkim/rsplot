//! `calc://` derived channels over `loc://` children — fully headless (no IOC).
//!
//! Exercises the whole path: the engine-injected child connector opens the
//! `loc://` children, the connection task polls their values, evaluates the
//! expression with [`evalexpr`], and republishes the result. Children share by
//! name with directly-connected `loc://` handles, so a write through one drives
//! the calc recompute.

#![cfg(feature = "calc")]

use std::time::{Duration, Instant};

use sidm::{Engine, PvValue};

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

#[test]
fn calc_sums_two_local_children_and_recomputes_on_write() {
    let engine = Engine::new();
    let calc = engine
        .connect("calc://sum?expr=a+b&a=loc://calc_x&b=loc://calc_y")
        .expect("connect calc channel");
    // The children share by name with these direct handles.
    let a = engine.connect("loc://calc_x").expect("connect child a");
    let b = engine.connect("loc://calc_y").expect("connect child b");

    // Both children connect immediately (loc:// is in-process) → calc connects.
    assert!(
        wait_for(|| calc.is_connected(), Duration::from_secs(2)),
        "calc channel never connected (children did not all connect)"
    );

    // Children default to float 0.0, so the initial derived value is 0.0.
    assert!(
        wait_for(
            || matches!(calc.read(|s| s.value.clone()), Some(PvValue::Float(v)) if v.abs() < 1e-9),
            Duration::from_secs(2)
        ),
        "did not observe the initial derived value 0.0 (got {:?})",
        calc.read(|s| s.value.clone())
    );

    // Drive the children; the calc recomputes a + b = 5.0.
    a.put(PvValue::Float(2.0));
    b.put(PvValue::Float(3.0));
    assert!(
        wait_for(
            || matches!(calc.read(|s| s.value.clone()), Some(PvValue::Float(v)) if (v - 5.0).abs() < 1e-9),
            Duration::from_secs(2)
        ),
        "did not observe the recomputed sum 5.0 (got {:?})",
        calc.read(|s| s.value.clone())
    );
}

#[test]
fn calc_update_list_restricts_which_child_triggers_recompute() {
    let engine = Engine::new();
    // Only `a` is allowed to trigger a recompute.
    let calc = engine
        .connect("calc://restricted?expr=a+b&a=loc://calc_ua&b=loc://calc_ub&update=a")
        .expect("connect calc channel");
    let a = engine.connect("loc://calc_ua").expect("connect child a");
    let b = engine.connect("loc://calc_ub").expect("connect child b");

    assert!(
        wait_for(|| calc.is_connected(), Duration::from_secs(2)),
        "calc channel never connected"
    );
    assert!(
        wait_for(
            || matches!(calc.read(|s| s.value.clone()), Some(PvValue::Float(v)) if v.abs() < 1e-9),
            Duration::from_secs(2)
        ),
        "did not observe the initial derived value 0.0"
    );

    // A change to `b` (not in the update list) must NOT recompute: after several
    // poll intervals the value is still 0.0, even though b is now 5.0.
    b.put(PvValue::Float(5.0));
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        matches!(calc.read(|s| s.value.clone()), Some(PvValue::Float(v)) if v.abs() < 1e-9),
        "b is not in the update list yet it triggered a recompute (got {:?})",
        calc.read(|s| s.value.clone())
    );

    // A change to `a` (in the update list) recomputes, now seeing b = 5.0 too.
    a.put(PvValue::Float(1.0));
    assert!(
        wait_for(
            || matches!(calc.read(|s| s.value.clone()), Some(PvValue::Float(v)) if (v - 6.0).abs() < 1e-9),
            Duration::from_secs(2)
        ),
        "a (in update list) did not trigger a recompute to 6.0 (got {:?})",
        calc.read(|s| s.value.clone())
    );
}
