//! Headless test of the live `fake://` generator over the engine — no IOC.
//!
//! The pure waveform/severity math is unit-tested in the plugin; this only
//! confirms the live task advances the stamp and stays in bounds.

use std::time::{Duration, Instant};

use rsdm::{Engine, PvValue};

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
fn fake_sine_advances_stamp_and_stays_in_bounds() {
    let engine = Engine::new();
    let ch = engine
        .connect("fake://gen?wave=sine&period=0.2&rate=50&min=-1&max=1")
        .unwrap();

    // First sample arrives quickly and marks the channel connected.
    assert!(wait_for(|| ch.is_connected(), Duration::from_secs(1)));

    let s0 = ch.stamp();
    // Several updates should accumulate over a short wait at 50 Hz.
    assert!(
        wait_for(|| ch.stamp() >= s0 + 5, Duration::from_secs(1)),
        "fake generator should produce multiple updates"
    );

    // Every observed value stays within the configured amplitude.
    for _ in 0..20 {
        if let Some(PvValue::Float(v)) = ch.read(|s| s.value.clone()) {
            assert!((-1.0..=1.0).contains(&v), "sample {v} out of bounds");
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    // The fake source is read-only.
    assert!(!ch.read(|s| s.write_access));
}
