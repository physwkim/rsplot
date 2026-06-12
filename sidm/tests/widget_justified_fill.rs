//! The justified-fill rule for the fixed-size widgets (`SidmDrawing`,
//! `SidmSymbol`, `SidmScaleIndicator`, `SidmByteIndicator`, `SidmImage`).
//!
//! egui's justification only expands the *response* rect around an exact
//! allocation — `allocate_exact_size` returns the desired size aligned inside
//! it — so a widget that paints a fixed size must consult the layout itself
//! (`widgets::base::justified_size`) or it stays at its native size inside
//! justified containers (the adl2sidm responsive-layout regression: MEDM
//! group-box rectangles stopped tracking the window scale).
//!
//! One test per invariant boundary: a justified layout fills the available
//! rect; a plain layout keeps the native size. Drawing and image expose the
//! allocated rect through their response (no GPU needed); symbol, scale, and
//! byte return their framed outer response — which justification expands even
//! without the fix — so those are proven by pixel readback instead (the
//! established empirical pattern of the sibling `widget_*_render.rs` tests).
//!
//! The pixel tests need a GPU (real or software).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use sidm::Engine;
use sidm::widgets::{
    DrawingShape, SidmByteIndicator, SidmDrawing, SidmEnumComboBox, SidmImage, SidmLabel,
    SidmScaleIndicator, SidmSymbol,
};
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

fn count_red(raw: &[u8]) -> u32 {
    raw.chunks_exact(4)
        .filter(|px| px[0] > 200 && px[1] < 80 && px[2] < 80)
        .count() as u32
}

fn count_green(raw: &[u8]) -> u32 {
    raw.chunks_exact(4)
        .filter(|px| px[0] < 80 && px[1] > 200 && px[2] < 80)
        .count() as u32
}

/// Run `show` once per frame in a 300×200 harness, either inside a
/// both-axis-justified layout or in the plain default layout, and return
/// (allocated rect reported by `show`, the ui's available rect).
fn layout_probe(
    justified: bool,
    show: impl FnMut(&mut egui::Ui) -> egui::Rect + 'static,
) -> (egui::Rect, egui::Rect) {
    let got = Rc::new(Cell::new(egui::Rect::NOTHING));
    let avail = Rc::new(Cell::new(egui::Rect::NOTHING));
    let (got_ui, avail_ui) = (got.clone(), avail.clone());
    let show = RefCell::new(show);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(300.0, 200.0))
        .with_pixels_per_point(1.0)
        .build_ui(move |ui| {
            avail_ui.set(ui.available_rect_before_wrap());
            if justified {
                ui.with_layout(
                    egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                    |ui| got_ui.set(show.borrow_mut()(ui)),
                );
            } else {
                got_ui.set(show.borrow_mut()(ui));
            }
        });
    harness.run();
    (got.get(), avail.get())
}

#[test]
fn justified_drawing_fills_the_available_rect() {
    let engine = Engine::new();
    let mut drawing = SidmDrawing::new(&engine, "loc://fill_drawing", DrawingShape::Rectangle)
        .expect("connect")
        .with_fill(egui::Color32::from_rgb(255, 0, 0))
        .with_size(egui::vec2(120.0, 80.0));
    let (rect, avail) = layout_probe(true, move |ui| drawing.show(ui).rect);
    assert_eq!(
        rect.size(),
        avail.size(),
        "a justified drawing must fill the available rect"
    );
}

#[test]
fn plain_drawing_keeps_its_native_size() {
    let engine = Engine::new();
    let mut drawing = SidmDrawing::new(&engine, "loc://native_drawing", DrawingShape::Rectangle)
        .expect("connect")
        .with_fill(egui::Color32::from_rgb(255, 0, 0))
        .with_size(egui::vec2(120.0, 80.0));
    let (rect, _) = layout_probe(false, move |ui| drawing.show(ui).rect);
    assert_eq!(rect.size(), egui::vec2(120.0, 80.0));
}

#[test]
fn justified_image_fills_the_available_rect() {
    // The file is missing, so the placeholder paints — the allocation rule is
    // what is under test, and it is independent of decode success.
    let mut image =
        SidmImage::new("/nonexistent/justified_fill.gif").with_size(egui::vec2(120.0, 80.0));
    let (rect, avail) = layout_probe(true, move |ui| image.show(ui).rect);
    assert_eq!(
        rect.size(),
        avail.size(),
        "a justified image must fill the available rect"
    );
}

#[test]
fn plain_image_keeps_its_native_size() {
    let mut image =
        SidmImage::new("/nonexistent/justified_fill.gif").with_size(egui::vec2(120.0, 80.0));
    let (rect, _) = layout_probe(false, move |ui| image.show(ui).rect);
    assert_eq!(rect.size(), egui::vec2(120.0, 80.0));
}

/// The 400×400 harness size shared by every pixel probe (one row is
/// `PROBE_SIZE` RGBA pixels in the raw buffer).
const PROBE_SIZE: u32 = 400;

/// Render `show` in a [`PROBE_SIZE`]² harness, justified or plain, and return
/// the raw RGBA bytes.
fn pixel_probe_raw(justified: bool, show: impl FnMut(&mut egui::Ui) + 'static) -> Vec<u8> {
    let rs = create_render_state(default_wgpu_setup());
    let renderer = WgpuTestRenderer::from_render_state(rs);
    let show = RefCell::new(show);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(PROBE_SIZE as f32, PROBE_SIZE as f32))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            if justified {
                ui.with_layout(
                    egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                    |ui| show.borrow_mut()(ui),
                );
            } else {
                show.borrow_mut()(ui);
            }
        });
    harness.step();
    harness.step();
    let image = harness.render().expect("headless wgpu render");
    image.into_raw()
}

/// Render `show` in a [`PROBE_SIZE`]² harness, justified or plain, and return
/// the pixel count selected by `count`.
fn pixel_probe(
    justified: bool,
    show: impl FnMut(&mut egui::Ui) + 'static,
    count: fn(&[u8]) -> u32,
) -> u32 {
    count(&pixel_probe_raw(justified, show))
}

/// The bounding-box size of the near-white pixels (the dashed disconnect
/// border, pure `Color32::WHITE`; default text at gray ~140 stays below the
/// threshold) in a [`PROBE_SIZE`]-wide probe image. `(0, 0)` when none.
fn white_bbox(raw: &[u8]) -> (u32, u32) {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for (i, px) in raw.chunks_exact(4).enumerate() {
        if px[0] > 230 && px[1] > 230 && px[2] > 230 {
            let (x, y) = (i as u32 % PROBE_SIZE, i as u32 / PROBE_SIZE);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x0 == u32::MAX {
        (0, 0)
    } else {
        (x1 - x0 + 1, y1 - y0 + 1)
    }
}

fn red_symbol(engine: &Engine, address: &str) -> SidmSymbol {
    let symbol = SidmSymbol::new(engine, address)
        .expect("connect")
        .with_state(
            1,
            DrawingShape::Rectangle,
            egui::Color32::from_rgb(255, 0, 0),
        )
        .with_size(egui::vec2(80.0, 80.0));
    assert!(
        wait_for(
            || symbol.channel().read(|s| s.value.is_some()),
            Duration::from_secs(2)
        ),
        "symbol channel never observed its init value"
    );
    symbol
}

#[test]
fn justified_symbol_fills_the_available_rect() {
    let engine = Engine::new();
    let mut filled = red_symbol(&engine, "loc://fill_symbol?type=int&init=1");
    let justified = pixel_probe(true, move |ui| drop(filled.show(ui)), count_red);
    let mut native = red_symbol(&engine, "loc://native_symbol?type=int&init=1");
    let plain = pixel_probe(false, move |ui| drop(native.show(ui)), count_red);
    // The native 80×80 shape covers ~6400 px; the justified shape must cover
    // most of the 400×400 harness. An area floor, not a ratio: a one-axis-only
    // fill (the egui combo-height failure mode) already clears a 4× ratio.
    eprintln!("symbol: justified={justified} plain={plain}");
    assert!(
        justified > 100_000 && plain > 3000 && plain < 10_000,
        "justified symbol should fill both axes: justified={justified} plain={plain}"
    );
}

fn red_scale(engine: &Engine, address: &str) -> SidmScaleIndicator {
    let scale = SidmScaleIndicator::new(engine, address)
        .expect("connect")
        .with_limits(0.0, 100.0)
        .with_bar_indicator(true)
        .with_value_label(false)
        .with_bar_color(egui::Color32::from_rgb(255, 0, 0))
        .with_size(egui::vec2(120.0, 40.0));
    assert!(
        wait_for(
            || scale.channel().read(|s| s.value.is_some()),
            Duration::from_secs(2)
        ),
        "scale channel never observed its init value"
    );
    scale
}

#[test]
fn justified_scale_indicator_fills_the_available_rect() {
    let engine = Engine::new();
    let mut filled = red_scale(&engine, "loc://fill_scale?type=float&init=100");
    let justified = pixel_probe(true, move |ui| drop(filled.show(ui)), count_red);
    let mut native = red_scale(&engine, "loc://native_scale?type=float&init=100");
    let plain = pixel_probe(false, move |ui| drop(native.show(ui)), count_red);
    eprintln!("scale: justified={justified} plain={plain}");
    assert!(
        justified > 100_000 && plain > 2000 && plain < 10_000,
        "justified scale should fill both axes: justified={justified} plain={plain}"
    );
}

fn red_combo(engine: &Engine, address: &str) -> SidmEnumComboBox {
    let combo = SidmEnumComboBox::new(engine, address).expect("connect");
    assert!(
        wait_for(
            || combo.channel().read(|s| s.connected),
            Duration::from_secs(2)
        ),
        "combo channel never connected"
    );
    combo
}

/// The combo face fills from `visuals.widgets.inactive.weak_bg_fill` (the same
/// override the adl2sidm `bclr` style prelude sets), so painting it red and
/// counting makes the face size observable.
#[test]
fn justified_enum_combo_box_fills_the_available_rect() {
    let engine = Engine::new();
    let mut filled = red_combo(&engine, "loc://fill_combo?type=int&init=0");
    let justified = pixel_probe(
        true,
        move |ui| {
            ui.style_mut().visuals.widgets.inactive.weak_bg_fill =
                egui::Color32::from_rgb(255, 0, 0);
            drop(filled.show(ui));
        },
        count_red,
    );
    let mut native = red_combo(&engine, "loc://native_combo?type=int&init=0");
    let plain = pixel_probe(
        false,
        move |ui| {
            ui.style_mut().visuals.widgets.inactive.weak_bg_fill =
                egui::Color32::from_rgb(255, 0, 0);
            drop(native.show(ui));
        },
        count_red,
    );
    // Native: the default combo_width face (~100×18) ≈ 2k px. The justified
    // face must cover most of the 400×400 harness — a ratio alone false-passes,
    // because egui's combo height (but never its width) tracks the justified
    // expansion by itself (~100×390 ≈ 39k px, already 19× the native count).
    assert!(
        justified > 100_000 && plain > 1000 && plain < 5_000,
        "justified combo should fill both axes: justified={justified} plain={plain}"
    );
}

/// A `ca://` channel with no IOC behind it stays disconnected, so the label
/// shows its address inside the dashed pure-white disconnect border — the
/// border's bounding box is the observable for how far the face extends
/// (the label's own response/count can't be used: justification expands the
/// outer allocation even when the face hugs its galley).
#[test]
fn justified_label_fills_the_available_rect() {
    let engine = Engine::new();
    let mut filled = SidmLabel::new(&engine, "ca://sidm:jf:l1").expect("connect");
    let justified = white_bbox(&pixel_probe_raw(true, move |ui| drop(filled.show(ui))));
    let mut native = SidmLabel::new(&engine, "ca://sidm:jf:l2").expect("connect");
    let plain = white_bbox(&pixel_probe_raw(false, move |ui| drop(native.show(ui))));
    // Justified: the dashed border must span both axes of the 400×400 harness.
    // Plain: it hugs the one-line address text (~110×24 with the frame inset).
    eprintln!("label: justified={justified:?} plain={plain:?}");
    assert!(
        justified.0 > 380 && justified.1 > 380,
        "justified label face should span both axes: justified={justified:?}"
    );
    assert!(
        plain.0 > 60 && plain.0 < 250 && plain.1 > 10 && plain.1 < 40,
        "plain label face should hug its text: plain={plain:?}"
    );
}

/// In-process CA IOC fixture for the enum-button probe (the only widget here
/// that needs enum strings, which `loc://` does not provide). Mirrors
/// `ca_ioc.rs::ioc_engine`: the engine's CA plugin searches exactly the
/// loopback server, so the test is parallel-safe.
#[cfg(feature = "ca")]
fn enum_ioc_engine() -> (Engine, tokio::runtime::Runtime) {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use epics_base_rs::server::records::bi::BiRecord;
    use epics_ca_rs::server::CaServer;

    let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("reserve free CA server port");
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let server_rt = tokio::runtime::Runtime::new().expect("server runtime");
    let server = server_rt.block_on(async {
        let mut rec = BiRecord::new(0);
        rec.znam = "Off".to_owned();
        rec.onam = "On".to_owned();
        CaServer::builder()
            .port(port)
            .record("sidm:jf:bi", rec)
            .build()
            .await
            .expect("build in-process CA server")
    });
    server_rt.spawn(async move {
        let _ = server.run().await;
    });
    std::thread::sleep(Duration::from_millis(300));

    let addr: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .expect("loopback server address");
    let engine = Engine::new();
    engine.register_plugin(Arc::new(sidm::CaPlugin::with_addresses(vec![addr])));
    (engine, server_rt)
}

#[cfg(feature = "ca")]
fn red_selected_enum_button(engine: &Engine) -> sidm::widgets::SidmEnumButton {
    let button = sidm::widgets::SidmEnumButton::new(engine, "ca://sidm:jf:bi").expect("connect");
    assert!(
        wait_for(
            || button.channel().read(|s| s.enum_strings.is_some()),
            Duration::from_secs(5)
        ),
        "enum button never received its enum strings"
    );
    button
}

/// The selected choice paints `visuals.selection.bg_fill`, so painting it red
/// and counting makes the per-button face size observable. MEDM divides the
/// choice-button rect equally among the buttons (medmChoiceButtons.c
/// XmNfractionBase): with two options, the selected one must cover about half
/// the justified rect.
#[test]
#[cfg(feature = "ca")]
fn justified_enum_button_divides_the_available_rect() {
    let (engine, _server_rt) = enum_ioc_engine();
    let mut filled = red_selected_enum_button(&engine);
    let justified = pixel_probe(
        true,
        move |ui| {
            ui.style_mut().visuals.selection.bg_fill = egui::Color32::from_rgb(255, 0, 0);
            drop(filled.show(ui));
        },
        count_red,
    );
    let mut native = red_selected_enum_button(&engine);
    let plain = pixel_probe(
        false,
        move |ui| {
            ui.style_mut().visuals.selection.bg_fill = egui::Color32::from_rgb(255, 0, 0);
            drop(native.show(ui));
        },
        count_red,
    );
    // Justified: the selected "Off" button is one of two equal vertical shares
    // of the 400×400 harness (~390×193 ≈ 75k px). Plain: it hugs its caption.
    // Area floors, not a ratio (see the symbol test).
    eprintln!("enum button: justified={justified} plain={plain}");
    assert!(
        justified > 60_000 && plain > 200 && plain < 5_000,
        "justified enum button should fill its equal share: justified={justified} plain={plain}"
    );
}

fn green_byte(engine: &Engine, address: &str) -> SidmByteIndicator {
    let byte = SidmByteIndicator::new(engine, address)
        .expect("connect")
        .with_num_bits(8)
        .with_show_labels(false);
    assert!(
        wait_for(
            || byte.channel().read(|s| s.value.is_some()),
            Duration::from_secs(2)
        ),
        "byte channel never observed its init value"
    );
    byte
}

#[test]
fn justified_byte_divides_the_available_rect_among_bits() {
    let engine = Engine::new();
    // All 8 bits set: every segment is the on colour (default green).
    let mut filled = green_byte(&engine, "loc://fill_byte?type=int&init=255");
    let justified = pixel_probe(true, move |ui| drop(filled.show(ui)), count_green);
    let mut native = green_byte(&engine, "loc://native_byte?type=int&init=255");
    let plain = pixel_probe(false, move |ui| drop(native.show(ui)), count_green);
    // Native: 8 squares of 16×16 ≈ 2k px; justified segments fill 400×400
    // minus the 7 inter-bit gaps. Area floor, not ratio (see the symbol test).
    eprintln!("byte: justified={justified} plain={plain}");
    assert!(
        justified > 80_000 && plain > 1000 && plain < 5_000,
        "justified byte should fill both axes: justified={justified} plain={plain}"
    );
}
