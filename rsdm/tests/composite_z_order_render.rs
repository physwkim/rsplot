//! Pixel proof of the egui Area-stacking contract that adl2rsdm's z-order model
//! depends on: within one `egui::Order`, the Area created LATER renders on top.
//!
//! adl2rsdm converts a MEDM screen into a sequence of `egui::Area`s, one per
//! widget, each pinned to a z-layer via `Area::order`. Within a layer, file
//! order is reproduced purely by *creation order* (the converter emits the
//! placements sorted by layer, stable within a layer). The round-12 fix — a
//! composite frame is emitted as one placement PER child layer so a child's
//! Area is created at its own layer's statement position — only restores the
//! visible MEDM result (e.g. ADBuffers.adl's "Buffers" title drawn over its
//! background chip) IF this egui contract holds. This test pins it end-to-end
//! in rendered pixels, so an egui upgrade that changed same-layer stacking
//! would fail here rather than silently break every converted screen.
//!
//! Needs a GPU (real or software).

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui;

/// Count near-black pixels (the title glyphs) inside the chip's red rectangle.
fn black_over_red(raw: &[u8], width: usize, chip: egui::Rect) -> u32 {
    let mut black = 0u32;
    let mut red = 0u32;
    for (i, px) in raw.chunks_exact(4).enumerate() {
        let (x, y) = ((i % width) as f32, (i / width) as f32);
        if !chip.contains(egui::pos2(x, y)) {
            continue;
        }
        if px[0] < 60 && px[1] < 60 && px[2] < 60 {
            black += 1;
        } else if px[0] > 180 && px[1] < 80 && px[2] < 80 {
            red += 1;
        }
    }
    // The chip must still be visibly red around the glyphs (it did not vanish),
    // and the glyphs must be present on top of it.
    assert!(
        red > 200,
        "the chip rectangle should remain visible: red={red}"
    );
    black
}

/// Two `Background`-order Areas at overlapping positions, created chip-first
/// then text — exactly the order adl2rsdm emits for a title chip (inside a
/// composite, file-earlier) and its overlapping title text. The text, created
/// LATER, must paint on top.
#[test]
fn later_same_order_area_renders_on_top() {
    const W: u32 = 260;
    const H: u32 = 60;
    let chip = egui::Rect::from_min_size(egui::pos2(20.0, 8.0), egui::vec2(220.0, 40.0));
    let text_pos = egui::pos2(28.0, 18.0);

    let renderer = WgpuTestRenderer::from_render_state(create_render_state(default_wgpu_setup()));
    let mut harness = Harness::builder()
        .with_size(egui::vec2(W as f32, H as f32))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            let ctx = ui.ctx().clone();
            // Chip FIRST (Background) — a solid red rectangle.
            egui::Area::new(egui::Id::new("chip"))
                .order(egui::Order::Background)
                .fixed_pos(chip.min)
                .constrain(false)
                .show(&ctx, |ui| {
                    ui.set_clip_rect(chip);
                    ui.painter().rect_filled(
                        chip,
                        egui::CornerRadius::ZERO,
                        egui::Color32::from_rgb(220, 0, 0),
                    );
                });
            // Title text SECOND (also Background) — must render over the chip.
            egui::Area::new(egui::Id::new("title"))
                .order(egui::Order::Background)
                .fixed_pos(text_pos)
                .constrain(false)
                .show(&ctx, |ui| {
                    ui.label(
                        egui::RichText::new("Buffers")
                            .color(egui::Color32::BLACK)
                            .size(22.0),
                    );
                });
        });
    harness.step();
    harness.step();
    let image = harness.render().expect("headless wgpu render");
    let black = black_over_red(image.as_raw(), W as usize, chip);
    assert!(
        black > 80,
        "the later-created title Area must render its glyphs over the chip; \
         got {black} black pixels inside the chip (0 ⇒ the chip painted over it)"
    );
}
