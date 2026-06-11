//! Fidelity gate: the converter's output must compile against the real `sidm`.
//!
//! `adl2pydm` can only check that it emits well-formed XML; because `adl2sidm`
//! emits Rust, we can do far better — *compile* a generated screen against the
//! actual `sidm`/`siplot`/`eframe` APIs. This file does exactly that:
//!
//! 1. It `include!`s a committed generated module
//!    (`tests/fixtures/sample_screen.rs`, produced by the converter from
//!    `tests/fixtures/sample.adl`). Because this test crate carries `sidm`,
//!    `siplot`, and `eframe` as dev-dependencies, *building this test compiles
//!    the generated `Screen` against the real widget APIs* — if any sidm
//!    signature drifts, this fails to build. That is the fidelity gate.
//! 2. A drift test re-runs the converter on the same fixture and asserts the
//!    output still matches the committed module byte-for-byte, so the compiled
//!    artifact can never silently fall out of date with the emitter.
//!
//! The fixture exercises a broad widget surface (label / line edit / push button
//! / combo / slider / byte / scale indicator / drawing×2 / time plot / waveform
//! plot / frame, plus an arc placeholder and a CALC `// TODO`).

use adl2sidm::adl_parser::parse;
use adl2sidm::codegen::{Options, generate};

// Compiling this module IS the gate: the generated `Screen` is type-checked
// against the real sidm/siplot/eframe APIs. It is never instantiated here (no
// GPU/window in a unit test), only compiled.
#[allow(dead_code)]
mod sample_screen {
    include!("fixtures/sample_screen.rs");
}

/// The exact options the committed `sample_screen.rs` was generated with; the
/// drift test must match them or it will compare against differently-rendered
/// output.
fn sample_options() -> Options {
    Options {
        macros: vec![("P".to_string(), "DMM1:".to_string())],
        ..Options::default()
    }
}

#[test]
fn converter_output_matches_the_committed_module() {
    let adl = include_str!("fixtures/sample.adl");
    let generated = generate(&parse(adl), &sample_options());
    let committed = include_str!("fixtures/sample_screen.rs");
    assert_eq!(
        generated.source, committed,
        "converter output drifted from tests/fixtures/sample_screen.rs — \
         regenerate it with: cargo run -p adl2sidm -- \
         adl2sidm/tests/fixtures/sample.adl -o \
         adl2sidm/tests/fixtures/sample_screen.rs -m P=DMM1:"
    );
}

#[test]
fn sample_conversion_only_warns_for_the_known_unsupported_bits() {
    let adl = include_str!("fixtures/sample.adl");
    let generated = generate(&parse(adl), &sample_options());
    // The fixture's only gaps are the arc (no DrawingShape::Arc) and the
    // rectangle's CALC visibility rule — every other widget converts cleanly.
    assert_eq!(
        generated.warnings.len(),
        2,
        "unexpected warnings: {:?}",
        generated.warnings
    );
    assert!(generated.warnings.iter().any(|w| w.contains("arc")));
    assert!(
        generated
            .warnings
            .iter()
            .any(|w| w.contains("dynamic rule"))
    );
}
