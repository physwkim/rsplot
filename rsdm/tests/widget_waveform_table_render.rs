//! Headless wgpu render smoke test of [`RsdmWaveformTable`].
//!
//! The layout math and the cell→array write-back are unit-tested purely in the
//! module; this exercises the egui `Grid` render path — which is new to the crate
//! — to prove it runs without panicking (a mismatched `end_row` count or a
//! runtime borrow conflict would panic here) and that the data cells add painted
//! content: a populated table draws more text than an empty-channel table (which
//! shows only the header row). Light (text) pixels are counted on egui's dark
//! default theme.
//!
//! Needs a GPU (real or software).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsdm::widgets::RsdmWaveformTable;
use rsdm::{Engine, PvValue};
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

/// Light pixels — the rendered glyphs/widget chrome on egui's dark default theme.
fn count_light(raw: &[u8]) -> u32 {
    raw.chunks_exact(4)
        .filter(|px| px[0] > 150 && px[1] > 150 && px[2] > 150)
        .count() as u32
}

/// Render a 2-column waveform table over a `loc://` int-array channel, optionally
/// seeded with `values`, and return the count of light (text) pixels.
fn render_table(address: &str, values: Option<&[i64]>) -> u32 {
    let rs = create_render_state(default_wgpu_setup());
    let engine = Engine::new();
    let table = RsdmWaveformTable::new(&engine, address)
        .expect("connect")
        .with_column_count(2);

    if let Some(values) = values {
        let writer = engine.connect(address).expect("writer handle");
        writer.put(PvValue::IntArray(Arc::from(values)));
        assert!(
            wait_for(
                || writer.read(|s| matches!(s.value, Some(PvValue::IntArray(_)))),
                Duration::from_secs(2)
            ),
            "table channel never observed the array"
        );
    }

    let app = Rc::new(RefCell::new(table));
    let renderer = WgpuTestRenderer::from_render_state(rs);
    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(300.0, 240.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            app_ui.borrow_mut().show(ui);
        });
    harness.step();
    harness.step();
    let image = harness.render().expect("headless wgpu render");
    let light = count_light(image.as_raw());
    drop(engine);
    light
}

#[test]
fn populated_table_renders_more_than_an_empty_one() {
    // Empty channel: only the header row renders.
    let empty = render_table("loc://wf_table_empty", None);
    // Six elements across two columns: header + three data rows of editable cells.
    let populated = render_table("loc://wf_table_full", Some(&[10, 20, 30, 40, 50, 60]));
    assert!(
        populated > empty + 50,
        "the populated table should render more text than the empty one; \
         populated={populated}, empty={empty}"
    );
}
