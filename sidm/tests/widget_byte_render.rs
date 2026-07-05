//! Headless wgpu readback of [`SidmByteIndicator`]'s LED grid.
//!
//! The bit extraction and per-bit colour are unit-tested purely in
//! `widgets/byte.rs`; this proves the egui drawing puts the on/off colours on
//! screen. It writes an integer to a `loc://` channel, renders the indicator
//! (labels off, so only the coloured squares are present) inside
//! `egui_kittest`'s headless wgpu renderer, and counts the green (set) and grey
//! (clear) pixels — the same empirical pattern as `tests/widget_base_render.rs`.
//!
//! Needs a GPU (real or software).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use sidm::widgets::SidmByteIndicator;
use sidm::{Engine, PvValue};
use siplot::egui;

struct App {
    indicator: SidmByteIndicator,
}

impl App {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.indicator.show(ui);
    }
}

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

/// Counts of green (set-bit) and grey (clear-bit) pixels in a rendered frame.
struct Counts {
    green: u32,
    grey: u32,
}

/// Render a vertical byte indicator showing `value` over `num_bits` bits and
/// count its on/off pixels.
fn render(value: i64, num_bits: usize) -> Counts {
    let rs = create_render_state(default_wgpu_setup());
    let engine = Engine::new();
    // Config-bearing loc (type+init) so the connection is connected — a bare
    // `loc://` is disconnected until configured (R3-12), which would render the
    // indicator in its disconnected style instead of over the written value.
    let indicator = SidmByteIndicator::new(&engine, "loc://byte_render?type=int&init=0")
        .expect("connect")
        .with_num_bits(num_bits)
        .with_show_labels(false);
    // Drive the value through a second handle on the same loc variable.
    let writer = engine.connect("loc://byte_render").expect("writer handle");
    writer.put(PvValue::Int(value));
    assert!(
        wait_for(
            || indicator
                .channel()
                .read(|s| s.value == Some(PvValue::Int(value))),
            Duration::from_secs(2)
        ),
        "indicator channel never observed the written value {value}"
    );

    let app = Rc::new(RefCell::new(App { indicator }));
    let renderer = WgpuTestRenderer::from_render_state(rs);
    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(200.0, 200.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| app_ui.borrow_mut().ui(ui));

    harness.step();
    let image = harness.render().expect("headless wgpu render");
    let raw = image.as_raw();

    let (mut green, mut grey) = (0u32, 0u32);
    for px in raw.chunks_exact(4) {
        let (r, g, b) = (px[0], px[1], px[2]);
        if r < 80 && g > 200 && b < 80 {
            green += 1;
        } else if (70..=140).contains(&r)
            && (70..=140).contains(&g)
            && (70..=140).contains(&b)
            && r.abs_diff(g) < 30
            && g.abs_diff(b) < 30
        {
            grey += 1;
        }
    }
    Counts { green, grey }
}

#[test]
fn mixed_value_shows_both_on_and_off_leds() {
    // 0b0101 over 4 bits → 2 set (green) + 2 clear (grey).
    let c = render(0b0101, 4);
    assert!(
        c.green > 150 && c.grey > 150,
        "a mixed value should show both green and grey LEDs; got green={} grey={}",
        c.green,
        c.grey
    );
}

#[test]
fn all_bits_set_shows_only_green() {
    let c = render(0b1111, 4);
    assert!(
        c.green > 300,
        "all-set value should show green LEDs; got green={}",
        c.green
    );
    assert!(
        c.grey < 50,
        "all-set value should show no grey (clear) LEDs; got grey={}",
        c.grey
    );
}

#[test]
fn all_bits_clear_shows_only_grey() {
    let c = render(0, 4);
    assert!(
        c.grey > 300,
        "all-clear value should show grey LEDs; got grey={}",
        c.grey
    );
    assert!(
        c.green < 50,
        "all-clear value should show no green (set) LEDs; got green={}",
        c.green
    );
}
