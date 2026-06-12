//! MEDM Btn2 / PyDM middle-click PV copy.
//!
//! MEDM `StartDrag` (actions.c) puts the space-joined record names of the
//! smallest touched element into the selection; PyDM `show_address_tooltip`
//! (pydm/widgets/base.py) puts the protocol-stripped addresses on the
//! clipboard. The copy must arbitrate overlapping widgets (smallest rect wins,
//! MEDM `findSmallestTouchedExecuteElement`) and must skip placeholder
//! channels (a channel-less MEDM element offers no PV).
//!
//! The clipboard is observed through `OutputCommand::CopyText` in the pass
//! output — no GPU needed.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sidm::Engine;
use sidm::widgets::{DrawingShape, SidmDrawing, SidmLabel, middle_click_copy};
use siplot::egui;

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

/// The `CopyText` payloads produced by the pass `output`.
fn copied_texts(harness: &Harness<'_>) -> Vec<String> {
    harness
        .output()
        .platform_output
        .commands
        .iter()
        .filter_map(|c| match c {
            egui::OutputCommand::CopyText(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

/// Drive a settle pass, a pointer move, then a middle press over `pos`, and
/// return the copies emitted by the press pass.
fn middle_press_at(harness: &mut Harness<'_>, pos: egui::Pos2) -> Vec<String> {
    harness.step();
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos));
    harness.step();
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Middle,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    let copies = copied_texts(harness);
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Middle,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    copies
}

#[test]
fn middle_click_copies_the_stripped_pv_name() {
    let engine = Engine::new();
    let label = SidmLabel::new(&engine, "loc://copy_lbl?type=float&init=1.0").expect("connect");
    assert!(
        wait_for(|| label.channel().is_connected(), Duration::from_secs(2)),
        "label channel never connected"
    );
    let label = RefCell::new(label);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(200.0, 60.0))
        .with_pixels_per_point(1.0)
        .build_ui(move |ui| drop(label.borrow_mut().show(ui)));
    let copies = middle_press_at(&mut harness, egui::pos2(20.0, 12.0));
    // PyDM strips the protocol and keeps the rest of the address verbatim.
    assert_eq!(
        copies,
        vec!["copy_lbl?type=float&init=1.0".to_owned()],
        "middle press must copy the protocol-stripped channel name once"
    );
}

#[test]
fn smallest_touched_widget_wins_the_copy() {
    let engine = Engine::new();
    // A large decoration with a REAL dynamic channel behind a small label —
    // both under the pointer; MEDM picks the smallest touched element.
    let drawing = SidmDrawing::new(
        &engine,
        "loc://copy_big?type=int&init=1",
        DrawingShape::Rectangle,
    )
    .expect("connect")
    .with_size(egui::vec2(220.0, 90.0));
    let label = SidmLabel::new(&engine, "loc://copy_small?type=float&init=2.0").expect("connect");
    assert!(
        wait_for(|| label.channel().is_connected(), Duration::from_secs(2)),
        "label channel never connected"
    );
    let widgets = RefCell::new((drawing, label));
    let mut harness = Harness::builder()
        .with_size(egui::vec2(260.0, 120.0))
        .with_pixels_per_point(1.0)
        .build_ui(move |ui| {
            let (drawing, label) = &mut *widgets.borrow_mut();
            // Back-to-front like the generated screens: decoration, then the
            // smaller widget pinned on top of it.
            egui::Area::new(ui.id().with("deco"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ui.ctx(), |ui| drop(drawing.show(ui)));
            egui::Area::new(ui.id().with("ctl"))
                .fixed_pos(egui::pos2(40.0, 30.0))
                .show(ui.ctx(), |ui| drop(label.show(ui)));
        });
    let copies = middle_press_at(&mut harness, egui::pos2(45.0, 40.0));
    assert_eq!(
        copies,
        vec!["copy_small?type=float&init=2.0".to_owned()],
        "the smallest touched widget must provide the copied name"
    );
}

#[test]
fn placeholder_channels_do_not_copy() {
    let engine = Engine::new();
    let drawing = SidmDrawing::new(&engine, "loc://adl2sidm_shape_9", DrawingShape::Rectangle)
        .expect("connect")
        .with_size(egui::vec2(120.0, 60.0))
        .with_placeholder_channel();
    let drawing = RefCell::new(drawing);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(200.0, 100.0))
        .with_pixels_per_point(1.0)
        .build_ui(move |ui| drop(drawing.borrow_mut().show(ui)));
    let copies = middle_press_at(&mut harness, egui::pos2(30.0, 20.0));
    assert_eq!(
        copies,
        Vec::<String>::new(),
        "a placeholder channel must not reach the clipboard"
    );
}

#[test]
fn address_tooltip_shows_only_while_middle_is_held() {
    // PyDM shows the address tooltip ON the middle press (show_address_tooltip
    // is bound to the middle-button event filter), never on plain hover; MEDM
    // shows no hover tooltip at all.
    let engine = Engine::new();
    let address = "loc://copy_tip?type=float&init=3.0";
    let label = SidmLabel::new(&engine, address).expect("connect");
    assert!(
        wait_for(|| label.channel().is_connected(), Duration::from_secs(2)),
        "label channel never connected"
    );
    let label = RefCell::new(label);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(200.0, 60.0))
        .with_pixels_per_point(1.0)
        .build_ui(move |ui| drop(label.borrow_mut().show(ui)));
    let pos = egui::pos2(20.0, 12.0);
    harness.step();
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos));
    // Hover long enough for egui's tooltip delay to elapse.
    for _ in 0..120 {
        harness.step();
    }
    assert!(
        harness.query_by_label(address).is_none(),
        "plain hover must not show the address tooltip"
    );
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Middle,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    // The plugin records the winner at the end of the press pass; the held
    // tooltip renders from the following pass.
    harness.step();
    harness.step();
    assert!(
        harness.query_by_label(address).is_some(),
        "the held middle button must show the full address tooltip"
    );
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Middle,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.step();
    assert!(
        harness.query_by_label(address).is_none(),
        "releasing the middle button must drop the tooltip"
    );
}

#[test]
fn helper_joins_multiple_channels_like_medm_start_drag() {
    // The plots hand the helper several addresses; MEDM space-joins the
    // record names (StartDrag strcat " "), PyDM space-joins the stripped ones.
    let mut harness = Harness::builder()
        .with_size(egui::vec2(200.0, 100.0))
        .with_pixels_per_point(1.0)
        .build_ui(move |ui| {
            let (_, response) =
                ui.allocate_exact_size(egui::vec2(180.0, 80.0), egui::Sense::hover());
            middle_click_copy(ui, &response, ["ca://plot:y", "ca://plot:x?q=1"]);
        });
    let copies = middle_press_at(&mut harness, egui::pos2(50.0, 40.0));
    assert_eq!(
        copies,
        vec!["plot:y plot:x?q=1".to_owned()],
        "multiple channels must space-join, each protocol-stripped"
    );
}
