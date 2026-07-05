//! The justified-fill rule for the fixed-size widgets (`RsdmDrawing`,
//! `RsdmSymbol`, `RsdmScaleIndicator`, `RsdmByteIndicator`, `RsdmImage`).
//!
//! egui's justification only expands the *response* rect around an exact
//! allocation — `allocate_exact_size` returns the desired size aligned inside
//! it — so a widget that paints a fixed size must consult the layout itself
//! (`widgets::base::justified_size`) or it stays at its native size inside
//! justified containers (the adl2rsdm responsive-layout regression: MEDM
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
use rsdm::Engine;
use rsdm::widgets::{
    DrawingShape, RsdmByteIndicator, RsdmDrawing, RsdmEnumComboBox, RsdmImage, RsdmLabel,
    RsdmScaleIndicator, RsdmSymbol,
};
use rsplot::egui;

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
    let mut drawing = RsdmDrawing::new(&engine, "loc://fill_drawing", DrawingShape::Rectangle)
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
    let mut drawing = RsdmDrawing::new(&engine, "loc://native_drawing", DrawingShape::Rectangle)
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
        RsdmImage::new("/nonexistent/justified_fill.gif").with_size(egui::vec2(120.0, 80.0));
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
        RsdmImage::new("/nonexistent/justified_fill.gif").with_size(egui::vec2(120.0, 80.0));
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

fn red_symbol(engine: &Engine, address: &str) -> RsdmSymbol {
    let symbol = RsdmSymbol::new(engine, address)
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

fn red_scale(engine: &Engine, address: &str) -> RsdmScaleIndicator {
    let scale = RsdmScaleIndicator::new(engine, address)
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

fn red_combo(engine: &Engine, address: &str) -> RsdmEnumComboBox {
    let combo = RsdmEnumComboBox::new(engine, address).expect("connect");
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
/// override the adl2rsdm `bclr` style prelude sets), so painting it red and
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
    let mut filled = RsdmLabel::new(&engine, "ca://rsdm:jf:l1").expect("connect");
    let justified = white_bbox(&pixel_probe_raw(true, move |ui| drop(filled.show(ui))));
    let mut native = RsdmLabel::new(&engine, "ca://rsdm:jf:l2").expect("connect");
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
        rec.znam = "Off".into();
        rec.onam = "On".into();
        // The same enum with the SECOND state selected, for the narrow-rect
        // probe: the button that used to clip is the last one, so the
        // observable selection paint must sit on it.
        let mut rec_on = BiRecord::new(1);
        rec_on.znam = "Off".into();
        rec_on.onam = "On".into();
        CaServer::builder()
            .port(port)
            .record("rsdm:jf:bi", rec)
            .record("rsdm:jf:bi_on", rec_on)
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
    engine.register_plugin(Arc::new(rsdm::CaPlugin::with_addresses(vec![addr])));
    (engine, server_rt)
}

#[cfg(feature = "ca")]
fn red_selected_enum_button(engine: &Engine) -> rsdm::widgets::RsdmEnumButton {
    let button = rsdm::widgets::RsdmEnumButton::new(engine, "ca://rsdm:jf:bi").expect("connect");
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

/// Bounding box (in pixel coordinates) of the pixels matching `is_match` in a
/// `width`-pixels-wide RGBA image: `(min_x, min_y, max_x, max_y)`.
fn match_bbox(
    raw: &[u8],
    width: usize,
    is_match: impl Fn(&[u8]) -> bool,
) -> Option<(usize, usize, usize, usize)> {
    let mut bbox: Option<(usize, usize, usize, usize)> = None;
    for (i, px) in raw.chunks_exact(4).enumerate() {
        if is_match(px) {
            let (x, y) = (i % width, i / width);
            bbox = Some(match bbox {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
        }
    }
    bbox
}

/// MEDM choice buttons divide their rect EXACTLY, with zero spacing and zero
/// margins (medmChoiceButtons.c createToggleButtons: XmNspacing=0,
/// XmNmarginWidth=0, usedWidth = width/numButtons). Two regressions in one
/// observable, the red selection fill of the selected LAST button ("On")
/// inside a 36×18 MEDM rect rendered through the generated `place()` shape (a
/// clipped `Area`, content justified):
///
/// - fixed egui gaps/paddings grew the row past the rect and the clip cut the
///   last button entirely (red = 0);
/// - flow layouts (`ui.horizontal` floors its row at `interact_size.y`, the
///   justified parent re-centres the overflow) displaced the row ~5 px down,
///   so the fill survived (red ≈ 110) but rode the bottom clip edge and the
///   caption glyphs lost their bottoms — hence the centering assertion, which
///   the count alone missed.
#[test]
#[cfg(feature = "ca")]
fn justified_enum_button_fits_a_narrow_medm_rect() {
    let (engine, _server_rt) = enum_ioc_engine();
    let button = rsdm::widgets::RsdmEnumButton::new(&engine, "ca://rsdm:jf:bi_on")
        .expect("connect")
        .with_orientation(rsdm::widgets::Orientation::Horizontal);
    assert!(
        wait_for(
            || button.channel().read(|s| s.enum_strings.is_some()),
            Duration::from_secs(5)
        ),
        "enum button never received its enum strings"
    );
    let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(36.0, 18.0));
    let button = RefCell::new(button);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(100.0, 60.0))
        .with_pixels_per_point(1.0)
        .renderer(WgpuTestRenderer::from_render_state(create_render_state(
            default_wgpu_setup(),
        )))
        .build_ui(move |ui| {
            // The adl2rsdm `place()` shape: an Area pinned at the MEDM rect,
            // clipped and capped to it, content justified to fill it.
            egui::Area::new(ui.id().with("narrow_choice"))
                .fixed_pos(rect.min)
                .constrain(false)
                .show(ui.ctx(), |ui| {
                    ui.set_clip_rect(rect);
                    ui.set_max_size(rect.size());
                    ui.style_mut().override_font_id = Some(egui::FontId::proportional(11.0));
                    ui.style_mut().visuals.selection.bg_fill = egui::Color32::from_rgb(255, 0, 0);
                    ui.with_layout(
                        egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                        |ui| drop(button.borrow_mut().show(ui)),
                    );
                });
        });
    harness.step();
    harness.step();
    let raw = harness.render().expect("headless wgpu render").into_raw();
    let red = count_red(&raw);
    let (_, top, _, bottom) = match_bbox(&raw, 100, |px| px[0] > 200 && px[1] < 80 && px[2] < 80)
        .expect("the selected last choice button must paint its red fill");
    // Vertically centred inside the rect: the gap above the fill equals the
    // gap below it (±2 px for AA), and the fill does not ride the clip edge.
    let (gap_above, gap_below) = (top as f32 - rect.top(), rect.bottom() - 1.0 - bottom as f32);
    eprintln!(
        "narrow enum button: red={red} fill rows {top}..{bottom} gaps {gap_above}/{gap_below}"
    );
    assert!(
        red > 50,
        "the selected last choice button must render inside the narrow rect: red={red}"
    );
    assert!(
        (gap_above - gap_below).abs() <= 2.0 && gap_below >= 1.0,
        "the buttons must be vertically centred in the MEDM rect, not displaced \
         into the clip: fill rows {top}..{bottom}, gaps above/below {gap_above}/{gap_below}"
    );
}

/// A justified label-less byte in a short MEDM cell (160×15-ish) paints
/// contiguous exact-share segments inside the rect (xc/Byte.c Draw_display).
/// The flow path floored its row at `interact_size.y` (18 > the 9 px content
/// height), so the bits overflowed and re-centred into the clip edges.
#[test]
fn justified_byte_fits_a_short_medm_rect() {
    let engine = Engine::new();
    let byte = green_byte(&engine, "loc://short_byte?type=int&init=255")
        .with_orientation(rsdm::widgets::Orientation::Horizontal);
    let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(80.0, 15.0));
    let byte = RefCell::new(byte);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(100.0, 40.0))
        .with_pixels_per_point(1.0)
        .renderer(WgpuTestRenderer::from_render_state(create_render_state(
            default_wgpu_setup(),
        )))
        .build_ui(move |ui| {
            egui::Area::new(ui.id().with("short_byte"))
                .fixed_pos(rect.min)
                .constrain(false)
                .show(ui.ctx(), |ui| {
                    ui.set_clip_rect(rect);
                    ui.set_max_size(rect.size());
                    ui.with_layout(
                        egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                        |ui| drop(byte.borrow_mut().show(ui)),
                    );
                });
        });
    harness.step();
    harness.step();
    let raw = harness.render().expect("headless wgpu render").into_raw();
    let green = count_green(&raw);
    let (left, top, right, bottom) =
        match_bbox(&raw, 100, |px| px[0] < 80 && px[1] > 200 && px[2] < 80)
            .expect("the lit bits must paint inside the short rect");
    let (gap_above, gap_below) = (top as f32 - rect.top(), rect.bottom() - 1.0 - bottom as f32);
    eprintln!(
        "short byte: green={green} bbox ({left},{top})..({right},{bottom}) gaps {gap_above}/{gap_below}"
    );
    // All 8 contiguous segments span the content width and sit centred in the
    // cell instead of riding the clip edges (the frame reserves 3 px around).
    assert!(
        green > 300 && (right - left) as f32 > rect.width() - 10.0,
        "segments must divide the short rect: green={green} span={}",
        right - left
    );
    assert!(
        (gap_above - gap_below).abs() <= 2.0 && gap_below >= 1.0,
        "bits must stay centred inside the short rect, not displaced into the \
         clip: bbox rows {top}..{bottom}, gaps above/below {gap_above}/{gap_below}"
    );
}

fn green_byte(engine: &Engine, address: &str) -> RsdmByteIndicator {
    let byte = RsdmByteIndicator::new(engine, address)
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

/// Render `show` in an adl2rsdm MEDM cell — the generated `place()` shape (a
/// clipped Area pinned at the rect) with the generated style prelude (MEDM
/// bclr fill behind + on the widget face, black text, height-derived font) —
/// and return the (top, bottom, left, right) bounding box of the dark text
/// pixels inside the cell, scanning only the `x_scan` columns.
fn medm_cell_text_bbox(
    show: impl FnMut(&mut egui::Ui) + 'static,
    x_scan: std::ops::Range<usize>,
) -> (usize, usize, usize, usize) {
    const BCLR: egui::Color32 = egui::Color32::from_rgb(115, 223, 255);
    let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(90.0, 20.0));
    let show = RefCell::new(show);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(110.0, 40.0))
        .with_pixels_per_point(1.0)
        .renderer(WgpuTestRenderer::from_render_state(create_render_state(
            default_wgpu_setup(),
        )))
        .build_ui(move |ui| {
            egui::Area::new(ui.id().with("medm_cell"))
                .fixed_pos(rect.min)
                .constrain(false)
                .show(ui.ctx(), |ui| {
                    ui.set_clip_rect(rect);
                    ui.set_max_size(rect.size());
                    ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.0));
                    ui.painter()
                        .rect_filled(ui.max_rect(), egui::CornerRadius::ZERO, BCLR);
                    let v = &mut ui.style_mut().visuals;
                    v.widgets.inactive.weak_bg_fill = BCLR;
                    v.widgets.hovered.weak_bg_fill = BCLR;
                    v.widgets.active.weak_bg_fill = BCLR;
                    v.widgets.open.weak_bg_fill = BCLR;
                    v.text_edit_bg_color = Some(BCLR);
                    v.override_text_color = Some(egui::Color32::BLACK);
                    ui.with_layout(
                        egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                        |ui| show.borrow_mut()(ui),
                    );
                });
        });
    harness.step();
    harness.step();
    let raw = harness.render().expect("headless wgpu render").into_raw();
    // Confine the scan to the cell interior: outside it lies the dark harness
    // panel, which would swallow the band into rows 0..39.
    let (mut top, mut bottom, mut left, mut right) = (usize::MAX, 0usize, usize::MAX, 0usize);
    for (i, px) in raw.chunks_exact(4).enumerate() {
        let (x, y) = (i % 110, i / 110);
        if x_scan.contains(&x) && (10..30).contains(&y) && px[0] < 70 && px[1] < 70 && px[2] < 70 {
            top = top.min(y);
            bottom = bottom.max(y);
            left = left.min(x);
            right = right.max(x);
        }
    }
    assert!(
        top <= bottom,
        "the widget must draw dark text inside the MEDM cell"
    );
    (top, bottom, left, right)
}

/// The row band of the dark text pixels inside the MEDM cell.
fn medm_cell_text_rows(show: impl FnMut(&mut egui::Ui) + 'static) -> (usize, usize) {
    let (top, bottom, _, _) = medm_cell_text_bbox(show, 12..98);
    (top, bottom)
}

/// The column band of the dark caption pixels inside the MEDM cell, scanning
/// only left of the combo's drop-down icon strip (the rightmost ~20 px) so the
/// icon never registers as caption.
fn medm_cell_text_cols(show: impl FnMut(&mut egui::Ui) + 'static) -> (usize, usize) {
    let (_, _, left, right) = medm_cell_text_bbox(show, 12..78);
    (left, right)
}

/// The gap above the text band vs the gap below it inside the 20 px cell
/// (rows 10..=29): equal ±2 px when the content is vertically centred, and
/// at least 1 px so the glyphs do not ride the clip edge (MEDM never clips
/// its captions — the cell is the widget).
fn assert_centred(widget: &str, top: usize, bottom: usize) {
    let (gap_above, gap_below) = (top as f32 - 10.0, 29.0 - bottom as f32);
    eprintln!("{widget}: text rows {top}..{bottom} gaps {gap_above}/{gap_below}");
    assert!(
        (gap_above - gap_below).abs() <= 2.0 && gap_above >= 1.0 && gap_below >= 1.0,
        "{widget} text must be vertically centred in the MEDM cell: \
         rows {top}..{bottom}, gaps above/below {gap_above}/{gap_below}"
    );
}

/// The framed flow path floored the content row at `interact_size.y` (18) and
/// the justified parent re-centred the overflowing frame, displacing every
/// framed control ~2.5 px down (text-entry rows 18..25 in a cell whose centred
/// band is 16..23) — the bottoms of the glyphs clipped in short MEDM cells.
#[test]
fn justified_line_edit_centres_text_in_a_medm_cell() {
    let engine = Engine::new();
    let mut edit = rsdm::widgets::RsdmLineEdit::new(&engine, "loc://cell_le?type=float&init=1.0")
        .expect("connect");
    assert!(
        wait_for(|| edit.channel().is_connected(), Duration::from_secs(2)),
        "line edit channel never connected"
    );
    let (top, bottom) = medm_cell_text_rows(move |ui| drop(edit.show(ui)));
    assert_centred("line edit", top, bottom);
}

#[test]
fn justified_push_button_centres_caption_in_a_medm_cell() {
    let engine = Engine::new();
    let mut button =
        rsdm::widgets::RsdmPushButton::new(&engine, "loc://cell_pb?type=int&init=0", "Start", "1")
            .expect("connect");
    assert!(
        wait_for(|| button.channel().is_connected(), Duration::from_secs(2)),
        "push button channel never connected"
    );
    let (top, bottom) = medm_cell_text_rows(move |ui| drop(button.show(ui)));
    assert_centred("push button", top, bottom);
}

/// The label was only accidentally centred before the framed pinning (the
/// frame displacement happened to cancel its top alignment); with the pinned
/// frame it needs its own explicit centring (MEDM textUpdate fills the cell
/// height with its font — medmTextUpdate.c — so the visual band is centred).
#[test]
fn justified_label_centres_text_in_a_medm_cell() {
    let engine = Engine::new();
    let mut label = RsdmLabel::new(&engine, "loc://cell_lb?type=float&init=1.0").expect("connect");
    assert!(
        wait_for(|| label.channel().is_connected(), Duration::from_secs(2)),
        "label channel never connected"
    );
    let (top, bottom) = medm_cell_text_rows(move |ui| drop(label.show(ui)));
    assert_centred("label", top, bottom);
}

/// MEDM `align="horiz. centered"` on a text update (the commonPlugins Port
/// column) → `RsdmLabel::with_alignment(Center)` must centre the text
/// horizontally in the MEDM cell; the builder default keeps the left anchor.
#[test]
fn label_centres_horizontally_with_center_alignment() {
    use rsdm::widgets::TextAlign;

    let engine = Engine::new();
    let mut centred = RsdmLabel::new(&engine, "loc://cell_lc?type=str&init=DOT")
        .expect("connect")
        .with_alignment(TextAlign::Center);
    assert!(
        wait_for(|| centred.channel().is_connected(), Duration::from_secs(2)),
        "centred label channel never connected"
    );
    let (left, right) = medm_cell_text_cols(move |ui| drop(centred.show(ui)));
    // No icon strip on a label: the text area is the framed cell interior
    // (x 12..98), middle 55. ±4 px absorbs glyph bearings.
    let mid = (left + right) as f32 / 2.0;
    eprintln!("centred label text: cols {left}..{right} mid {mid}");
    assert!(
        (mid - 55.0).abs() <= 4.0,
        "a Center-aligned label must sit at the cell middle: cols {left}..{right} mid {mid}"
    );

    let mut left_aligned =
        RsdmLabel::new(&engine, "loc://cell_ll?type=str&init=DOT").expect("connect");
    assert!(
        wait_for(
            || left_aligned.channel().is_connected(),
            Duration::from_secs(2)
        ),
        "left label channel never connected"
    );
    let (l, r) = medm_cell_text_cols(move |ui| drop(left_aligned.show(ui)));
    eprintln!("default label text: cols {l}..{r}");
    assert!(
        l <= 17 && r < 45,
        "the default label must keep the left anchor: cols {l}..{r}"
    );
}

/// MEDM (`XmTextField`, no `XmNalignment`) and PyDM (Qt `QLineEdit`) both
/// left-align text entries; the converter centres them so the editable fields
/// match the centred control captions. `with_alignment(Center)` must centre the
/// field text horizontally in the MEDM cell; the builder default keeps the left
/// anchor (the reference behaviour).
#[test]
fn line_edit_centres_horizontally_with_center_alignment() {
    use rsdm::widgets::TextAlign;

    let engine = Engine::new();
    let mut centred =
        rsdm::widgets::RsdmLineEdit::new(&engine, "loc://cell_lec?type=float&init=1.0")
            .expect("connect")
            .with_alignment(TextAlign::Center);
    assert!(
        wait_for(|| centred.channel().is_connected(), Duration::from_secs(2)),
        "centred line edit channel never connected"
    );
    let (left, right) = medm_cell_text_cols(move |ui| drop(centred.show(ui)));
    // No icon strip on a line edit: the text area is the framed cell interior
    // (x 12..98) less the 4 px TextEdit margins, middle ≈ 55. ±5 px absorbs the
    // narrow "1" glyph's bearings.
    let mid = (left + right) as f32 / 2.0;
    eprintln!("centred line edit text: cols {left}..{right} mid {mid}");
    assert!(
        (mid - 55.0).abs() <= 5.0,
        "a Center-aligned line edit must sit at the cell middle: cols {left}..{right} mid {mid}"
    );

    let mut left_aligned =
        rsdm::widgets::RsdmLineEdit::new(&engine, "loc://cell_lel?type=float&init=1.0")
            .expect("connect");
    assert!(
        wait_for(
            || left_aligned.channel().is_connected(),
            Duration::from_secs(2)
        ),
        "left line edit channel never connected"
    );
    let (l, r) = medm_cell_text_cols(move |ui| drop(left_aligned.show(ui)));
    eprintln!("default line edit text: cols {l}..{r}");
    assert!(
        l <= 22 && r < 45,
        "the default line edit must keep the left anchor: cols {l}..{r}"
    );
}

/// The combo face (an MEDM menu) carries its selected enum string; `loc://`
/// cannot seed enum strings, so this rides the in-process CA fixture.
#[test]
#[cfg(feature = "ca")]
fn justified_combo_centres_face_text_in_a_medm_cell() {
    let (engine, _server_rt) = enum_ioc_engine();
    let mut combo = RsdmEnumComboBox::new(&engine, "ca://rsdm:jf:bi").expect("connect");
    assert!(
        wait_for(
            || combo.channel().read(|s| s.enum_strings.is_some()),
            Duration::from_secs(5)
        ),
        "combo never received its enum strings"
    );
    let (top, bottom) = medm_cell_text_rows(move |ui| drop(combo.show(ui)));
    assert_centred("combo", top, bottom);
}

/// An MEDM menu centres its caption: the option-menu face is a Motif XmLabel
/// whose default `XmNalignment` is centred, and medmMenu.c never overrides it
/// — `with_alignment(TextAlign::Center)` reproduces that. The builder default
/// keeps PyDM's Qt `QComboBox` left anchor.
#[test]
#[cfg(feature = "ca")]
fn combo_caption_centres_with_center_alignment_and_defaults_left() {
    use rsdm::widgets::TextAlign;

    let (engine, _server_rt) = enum_ioc_engine();
    let mut centred = RsdmEnumComboBox::new(&engine, "ca://rsdm:jf:bi")
        .expect("connect")
        .with_alignment(TextAlign::Center);
    assert!(
        wait_for(
            || centred.channel().read(|s| s.enum_strings.is_some()),
            Duration::from_secs(5)
        ),
        "centred combo never received its enum strings"
    );
    let (left, right) = medm_cell_text_cols(move |ui| drop(centred.show(ui)));
    // The caption area is the cell (x 10..100, inset 2 by the framed border)
    // minus the icon strip (icon_width + icon_spacing ≈ 18-20 px) → its middle
    // sits near x 45-46. ±4 px absorbs glyph bearings and the spacing default.
    let mid = (left + right) as f32 / 2.0;
    eprintln!("centred combo caption: cols {left}..{right} mid {mid}");
    assert!(
        (mid - 45.5).abs() <= 4.0,
        "a Center-aligned caption must sit at the caption-area middle: \
         cols {left}..{right} mid {mid}"
    );

    let mut left_aligned = RsdmEnumComboBox::new(&engine, "ca://rsdm:jf:bi").expect("connect");
    assert!(
        wait_for(
            || left_aligned.channel().read(|s| s.enum_strings.is_some()),
            Duration::from_secs(5)
        ),
        "left combo never received its enum strings"
    );
    let (l, r) = medm_cell_text_cols(move |ui| drop(left_aligned.show(ui)));
    eprintln!("default combo caption: cols {l}..{r}");
    assert!(
        l <= 17 && r < 40,
        "the default caption must keep the Qt/PyDM left anchor: cols {l}..{r}"
    );
}
