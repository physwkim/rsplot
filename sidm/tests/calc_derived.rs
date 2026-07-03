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

// ---------------------------------------------------------------------------
// MEDM CALC dialect (`?dialect=medm`) — the adl2sidm visibility-gate language.
// ---------------------------------------------------------------------------

/// Wait until the calc channel's value is a Float within 1e-9 of `expected`.
fn wait_for_float(calc: &sidm::Channel, expected: f64) -> bool {
    wait_for(
        || matches!(calc.read(|s| s.value.clone()), Some(PvValue::Float(v)) if (v - expected).abs() < 1e-9),
        Duration::from_secs(2),
    )
}

#[test]
fn medm_if_zero_gate_tracks_a_float_channel() {
    // R1-33 regression: MEDM `A=0` over a FLOAT channel. The evalexpr dialect
    // translated this to `A == 0`, which is type-strict
    // (`Value::Float(0.0) != Value::Int(0)`) and never true for an analog PV;
    // MEDM compares doubles (`utils.c:4476`: `records[0]->value == 0.0`).
    let engine = Engine::new();
    let calc = engine
        .connect("calc://medm_zero?dialect=medm&expr=A=0&A=loc://medm_zero_a&update=A")
        .expect("connect medm calc channel");
    let a = engine.connect("loc://medm_zero_a").expect("connect child");

    // loc:// children start at Float(0.0) → "if zero" is TRUE (1.0: shown).
    assert!(
        wait_for_float(&calc, 1.0),
        "A=0 over Float(0.0) did not evaluate to 1.0 (got {:?})",
        calc.read(|s| s.value.clone())
    );
    // Non-zero → 0.0 (hidden).
    a.put(PvValue::Float(1.0));
    assert!(
        wait_for_float(&calc, 0.0),
        "A=0 over Float(1.0) did not evaluate to 0.0 (got {:?})",
        calc.read(|s| s.value.clone())
    );
}

#[test]
fn medm_if_not_zero_gate_is_the_inverse() {
    let engine = Engine::new();
    let calc = engine
        .connect("calc://medm_nz?dialect=medm&expr=A%230&A=loc://medm_nz_a&update=A")
        .expect("connect medm calc channel");
    let a = engine.connect("loc://medm_nz_a").expect("connect child");

    // Float(0.0) → `A#0` is FALSE (0.0: hidden).
    assert!(
        wait_for_float(&calc, 0.0),
        "A#0 over Float(0.0) did not evaluate to 0.0 (got {:?})",
        calc.read(|s| s.value.clone())
    );
    a.put(PvValue::Float(2.5));
    assert!(
        wait_for_float(&calc, 1.0),
        "A#0 over Float(2.5) did not evaluate to 1.0 (got {:?})",
        calc.read(|s| s.value.clone())
    );
}

#[test]
fn medm_ternary_functions_and_encoded_and_evaluate() {
    // Ternary + functions (`medm/medmCalc.c:249-250` `?:`, plus SQRT/MIN), and
    // the `%26` transport encoding for `&&` in a second gate.
    let engine = Engine::new();
    let calc = engine
        .connect("calc://medm_tern?dialect=medm&expr=A>2?SQRT(B):MIN(A,B)&A=loc://medm_t_a&B=loc://medm_t_b")
        .expect("connect medm ternary channel");
    let a = engine.connect("loc://medm_t_a").expect("connect child a");
    let b = engine.connect("loc://medm_t_b").expect("connect child b");
    assert!(
        wait_for(|| calc.is_connected(), Duration::from_secs(2)),
        "medm ternary channel never connected"
    );

    a.put(PvValue::Float(3.0));
    b.put(PvValue::Float(9.0));
    assert!(
        wait_for_float(&calc, 3.0), // A>2 → SQRT(9) = 3
        "ternary/SQRT arm did not evaluate (got {:?})",
        calc.read(|s| s.value.clone())
    );
    a.put(PvValue::Float(1.0));
    assert!(
        wait_for_float(&calc, 1.0), // A<=2 → MIN(1, 9) = 1
        "ternary/MIN arm did not evaluate (got {:?})",
        calc.read(|s| s.value.clone())
    );

    // `A#0&&B#0`, transported as `A%230%26%26B%230`.
    let both = engine
        .connect(
            "calc://medm_and?dialect=medm&expr=A%230%26%26B%230&A=loc://medm_t_a&B=loc://medm_t_b",
        )
        .expect("connect medm && channel");
    assert!(
        wait_for_float(&both, 1.0), // 1.0 and 9.0 are both non-zero
        "percent-encoded && gate did not evaluate to 1.0 (got {:?})",
        both.read(|s| s.value.clone())
    );
}

#[test]
fn medm_lowercase_operands_bind_like_uppercase() {
    // `medm/medmCalc.c:212-236` accepts operands in both cases.
    let engine = Engine::new();
    let calc = engine
        .connect("calc://medm_lc?dialect=medm&expr=a%230&A=loc://medm_lc_a")
        .expect("connect medm lowercase channel");
    let a = engine.connect("loc://medm_lc_a").expect("connect child");

    assert!(
        wait_for_float(&calc, 0.0),
        "lowercase a#0 over Float(0.0) did not evaluate to 0.0 (got {:?})",
        calc.read(|s| s.value.clone())
    );
    a.put(PvValue::Float(4.0));
    assert!(
        wait_for_float(&calc, 1.0),
        "lowercase a#0 over Float(4.0) did not evaluate to 1.0 (got {:?})",
        calc.read(|s| s.value.clone())
    );
}

#[test]
fn medm_metadata_operand_g_reads_first_channel_element_count() {
    // `G` = element count of the first channel (MEDM `utils.c:4501`
    // `valueArray[6] = pr->elementCount`); a scalar loc:// value counts 1.
    let engine = Engine::new();
    let calc = engine
        .connect("calc://medm_g?dialect=medm&expr=G&A=loc://medm_g_a")
        .expect("connect medm G channel");
    let _a = engine.connect("loc://medm_g_a").expect("connect child");

    assert!(
        wait_for_float(&calc, 1.0),
        "G over a scalar first channel did not evaluate to 1.0 (got {:?})",
        calc.read(|s| s.value.clone())
    );
}

#[test]
fn medm_invalid_expression_fails_visible() {
    // An expression that does not compile publishes 1.0 (fail-visible) instead
    // of leaving the gate silently empty (which would hide the widget forever).
    let engine = Engine::new();
    let calc = engine
        .connect("calc://medm_bad?dialect=medm&expr=SIN(&A=loc://medm_bad_a")
        .expect("connect medm invalid-expr channel");
    let a = engine.connect("loc://medm_bad_a").expect("connect child");

    assert!(
        wait_for_float(&calc, 1.0),
        "invalid MEDM expression did not publish the fail-visible 1.0 (got {:?})",
        calc.read(|s| s.value.clone())
    );
    // Later child updates must not replace the fail-visible value.
    a.put(PvValue::Float(7.0));
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        matches!(calc.read(|s| s.value.clone()), Some(PvValue::Float(v)) if (v - 1.0).abs() < 1e-9),
        "fail-visible 1.0 was overwritten after a child update (got {:?})",
        calc.read(|s| s.value.clone())
    );
}
