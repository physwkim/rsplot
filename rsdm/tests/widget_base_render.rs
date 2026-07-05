//! Headless wgpu readback of [`ChannelBase::framed`]'s alarm border.
//!
//! The border palette and the solid-vs-dashed decision are unit-tested purely in
//! `widgets/base.rs`; this proves the egui drawing actually puts those pixels on
//! screen. It renders a `framed` widget (neutral dark content so the border
//! colour is isolated) inside `egui_kittest`'s headless wgpu renderer, reads the
//! texture back, and measures the colour and continuity along the top edge —
//! the same empirical pattern as rsplot's `tests/mask_pointer_offset.rs`, with
//! no golden images.
//!
//! Needs a GPU (real or software): it builds a wgpu `RenderState` and reads back
//! the rendered frame.

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsdm::widgets::ChannelBase;
use rsdm::{AlarmSeverity, ChannelState, Engine};
use rsplot::egui;

const PPP: f32 = 1.0;
const CONTENT: egui::Vec2 = egui::vec2(220.0, 80.0);

/// The widget under test: a single `framed` block with neutral grey content.
struct App {
    base: ChannelBase,
    state: ChannelState,
    /// Frame outer rect captured from the last layout (logical points).
    last_rect: Option<egui::Rect>,
}

impl App {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let inner = self.base.framed(ui, &self.state, false, |ui| {
            let (rect, _) = ui.allocate_exact_size(CONTENT, egui::Sense::hover());
            // Dark grey fill: distinct from every alarm colour so a colour test
            // along the edge isolates the border, not the content.
            ui.painter()
                .rect_filled(rect, egui::CornerRadius::ZERO, egui::Color32::from_gray(40));
        });
        self.last_rect = Some(inner.response.rect);
    }
}

/// A rendered frame: RGBA8 pixels with dimensions, plus the captured frame rect.
struct Frame {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    rect: egui::Rect,
}

/// Render one frame for `severity`/`connected`.
fn render(severity: AlarmSeverity, connected: bool) -> Frame {
    let rs = create_render_state(default_wgpu_setup());
    let engine = Engine::new();
    let channel = engine
        .connect("loc://border_render_demo")
        .expect("connect loc channel");
    let app = Rc::new(RefCell::new(App {
        base: ChannelBase::new(channel),
        state: ChannelState {
            connected,
            severity,
            ..ChannelState::default()
        },
        last_rect: None,
    }));

    let renderer = WgpuTestRenderer::from_render_state(rs);
    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(400.0, 200.0))
        .with_pixels_per_point(PPP)
        .renderer(renderer)
        .build_ui(move |ui| app_ui.borrow_mut().ui(ui));

    harness.step();
    let rect = app.borrow().last_rect.expect("frame rect captured");
    let image = harness.render().expect("headless wgpu render");
    Frame {
        pixels: image.as_raw().clone(),
        width: image.width(),
        height: image.height(),
        rect,
    }
}

/// Fraction of the columns spanning the frame's top edge that carry a pixel
/// matching `pred` within a few-pixel band around the edge. ~1.0 for a solid
/// border, a partial value for a dashed one, ~0.0 for no border.
fn top_edge_coverage(frame: &Frame, pred: impl Fn(u8, u8, u8) -> bool) -> f32 {
    let (iw, ih) = (frame.width as i32, frame.height as i32);
    let raw = &frame.pixels;
    let rect = frame.rect;
    let top = (rect.top() * PPP).round() as i32;
    let left = (rect.left() * PPP).round() as i32;
    let right = (rect.right() * PPP).round() as i32;
    // The 2px inside stroke (and the centred dashed line) sit within ±2px of the
    // top edge; scan a small band to be robust to AA and stroke alignment.
    let (y0, y1) = ((top - 2).max(0), (top + 3).min(ih));

    let (mut covered, mut total) = (0u32, 0u32);
    for x in left..right {
        if x < 0 || x >= iw {
            continue;
        }
        total += 1;
        for y in y0..y1 {
            let i = ((y * iw + x) * 4) as usize;
            if pred(raw[i], raw[i + 1], raw[i + 2]) {
                covered += 1;
                break;
            }
        }
    }
    if total == 0 {
        0.0
    } else {
        covered as f32 / total as f32
    }
}

fn is_red(r: u8, g: u8, b: u8) -> bool {
    r > 180 && g < 80 && b < 80
}
fn is_white(r: u8, g: u8, b: u8) -> bool {
    r > 200 && g > 200 && b > 200
}
fn is_yellow(r: u8, g: u8, b: u8) -> bool {
    r > 180 && g > 180 && b < 80
}

#[test]
fn major_alarm_draws_a_solid_red_border() {
    let frame = render(AlarmSeverity::Major, true);
    let red = top_edge_coverage(&frame, is_red);
    assert!(
        red >= 0.8,
        "MAJOR border should be a near-continuous red line along the top edge; \
         got {red:.2} coverage"
    );
}

#[test]
fn minor_alarm_draws_a_solid_yellow_border() {
    let frame = render(AlarmSeverity::Minor, true);
    let yellow = top_edge_coverage(&frame, is_yellow);
    assert!(
        yellow >= 0.8,
        "MINOR border should be a near-continuous yellow line; got {yellow:.2} coverage"
    );
}

#[test]
fn disconnected_draws_a_dashed_white_border() {
    // Not connected → effective severity Disconnected → dashed white.
    let frame = render(AlarmSeverity::NoAlarm, false);
    let white = top_edge_coverage(&frame, is_white);
    assert!(
        white > 0.15,
        "disconnected border should put white pixels along the top edge; got {white:.2}"
    );
    assert!(
        white < 0.85,
        "disconnected border should be DASHED (gaps), not a solid line; got {white:.2} \
         coverage (too continuous)"
    );
}

#[test]
fn no_alarm_connected_draws_no_border() {
    let frame = render(AlarmSeverity::NoAlarm, true);
    let red = top_edge_coverage(&frame, is_red);
    let white = top_edge_coverage(&frame, is_white);
    let yellow = top_edge_coverage(&frame, is_yellow);
    assert!(
        red < 0.05 && white < 0.05 && yellow < 0.05,
        "a connected NO_ALARM channel must have no coloured border; \
         got red={red:.2} white={white:.2} yellow={yellow:.2}"
    );
}
