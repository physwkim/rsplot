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
//! / combo / slider / byte / scale indicator / drawing×3 incl. an arc / time plot
//! / waveform plot / frame, plus a wired CALC visibility gate).

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
        // The sample is committed in absolute mode (CLI `--absolute`) so the
        // compile gate covers the fixed-pixel emission path; the example below
        // covers the default responsive path.
        use_layout: false,
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
         adl2sidm/tests/fixtures/sample_screen.rs -m P=DMM1: --absolute"
    );
}

#[test]
fn example_screen_matches_the_committed_module() {
    // The runnable example (`examples/local_panel.rs`) `include!`s
    // `examples/local_panel_screen.rs`; guard it against drift the same way as
    // the fixture above. The example's channels already carry their
    // `loc://`/`fake://` scheme, so it is generated with an empty protocol.
    let adl = include_str!("../examples/local_panel.adl");
    let options = Options {
        protocol: String::new(),
        // The panel embeds `embed_child.adl`; resolve it from the examples dir so
        // the embedded display inlines exactly as the CLI produced the committed
        // module (the CLI sets `source_dir` to the input's directory).
        source_dir: Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples")),
        // The example uses the default responsive layout, so the runnable panel
        // reflows to fill its window; this also compile-gates the layout-mode
        // emission path against the real sidm APIs.
        ..Options::default()
    };
    let generated = generate(&parse(adl), &options);
    let committed = include_str!("../examples/local_panel_screen.rs");
    assert_eq!(
        generated.source, committed,
        "example output drifted from adl2sidm/examples/local_panel_screen.rs — \
         regenerate it with: cargo run -p adl2sidm -- \
         adl2sidm/examples/local_panel.adl -o \
         adl2sidm/examples/local_panel_screen.rs --protocol \"\""
    );
}

#[test]
fn sample_conversion_warns_only_with_the_informational_visibility_note() {
    let adl = include_str!("fixtures/sample.adl");
    let generated = generate(&parse(adl), &sample_options());
    // Every widget converts to a real SiDM widget; the one warning is the
    // rectangle's CALC visibility rule, now wired as a calc:// gate (an
    // informational note, not an unsupported gap).
    assert_eq!(
        generated.warnings.len(),
        1,
        "unexpected warnings: {:?}",
        generated.warnings
    );
    assert!(
        generated
            .warnings
            .iter()
            .any(|w| w.contains("dynamic visibility wired"))
    );
}
