//! Behaviour gate: clicking a related-display button must OPEN the converted
//! child screen, with the entry's `args` expanded into the child's runtime
//! macro table (MEDM `relatedDisplayCreateNewDisplay`).
//!
//! egui_kittest has no multi-viewport backend, so the generated
//! `show_viewport_immediate` takes egui's embedded fallback — the child screen
//! renders as an `egui::Window` in the same pass, which makes the open
//! headlessly verifiable through the accessibility tree: after the click, the
//! child's text (with the parent's baked `P=X:` substituted at *runtime*
//! through the args string `P=$(P)`) exists; before it, it does not.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use siplot::egui;

// `dead_code`: the generated module also carries the eframe entry point
// (`Screen::new(cc)`), which a headless test never calls.
#[allow(dead_code)]
mod rd_screen {
    include!("fixtures/rd_screen.rs");
}

/// A harness drawing the root `rd_parent.adl` screen (built lazily on the
/// first frame — `new_in` needs the harness's `egui::Context`).
fn harness<'a>() -> Harness<'a> {
    let mut screen: Option<rd_screen::Screen> = None;
    Harness::builder()
        .with_size(egui::vec2(600.0, 240.0))
        .build_ui(move |ui| {
            screen
                .get_or_insert_with(|| rd_screen::Screen::new_in(ui.ctx(), None, Vec::new()))
                .ui(ui);
        })
}

#[test]
fn clicking_a_related_display_opens_the_child_with_runtime_macros() {
    let mut harness = harness();
    harness.run();

    // Before the click: no child anywhere.
    assert!(
        harness.query_by_label("CHILD X:").is_none(),
        "child screen visible before any click"
    );

    // A real pointer click — the button is unobstructed before any window
    // opens. (Later clicks go through the accesskit action instead: the
    // embedded child window overlaps the scaled parent's buttons in this
    // small harness, so pointer hit-testing would exercise occlusion, not
    // the open machinery under test.)
    harness.get_by_label("Open Child").click();
    harness.run();

    // The child window opened, titled with its file name, and its static text
    // shows the runtime-expanded macro: the parent's baked `P=X:` flowed
    // through the entry's `args` (`P=$(P)`) into the child's macro table.
    harness.get_by_label("rd_child.adl");
    harness.get_by_label("CHILD X:");

    // A second click focuses the existing (module, args) window instead of
    // opening a duplicate (MEDM `popupExistingDisplay`).
    harness.get_by_label("Open Child").click_accesskit();
    harness.run();
    assert_eq!(
        harness.query_all_by_label("CHILD X:").count(),
        1,
        "a second click must focus the open child, not duplicate it"
    );

    // The child's own related display cycles back to the parent: a fresh root
    // instance opens as another window (the parent text then exists twice —
    // the hosting screen and the opened copy).
    harness.get_by_label("Back").click_accesskit();
    harness.run();
    assert_eq!(
        harness.query_all_by_label("RD PARENT X:").count(),
        2,
        "the child's Back button must open the parent screen as a window"
    );
}

#[test]
fn an_unconverted_target_still_renders_its_report_button() {
    // The missing target keeps the report-only button (clicking it only logs
    // to stderr) — the screen renders, nothing panics, no window opens.
    let mut harness = harness();
    harness.run();
    harness.get_by_label("Missing").click();
    harness.run();
    assert!(
        harness.query_by_label("rd_missing_fixture.adl").is_none(),
        "a missing target must not open a window"
    );
}
