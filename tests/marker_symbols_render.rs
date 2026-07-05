//! Headless wgpu readback proving the on-plot marker painter
//! (`chrome::draw_marker_symbol`, reached via `chrome::draw_markers`) actually
//! emits geometry for symbols that were **unreachable** before the marker
//! catalog was unified with the curve `Symbol` set (commit `ec25d22`).
//!
//! Before that change point markers used a 7-variant `MarkerSymbol`
//! (circle/point/pixel/plus/cross/diamond/square); the line, tick, caret and
//! heart glyphs silx markers share with curve vertices could not be drawn at
//! all. The painter's 11 new match arms had no rendered-pixel coverage — only
//! the compiler enforced exhaustiveness. This test closes that gap by drawing
//! one filled glyph (`Heart`) and two stroke glyphs (`HorizontalLine`,
//! `VerticalLine`) — all new — and probing the framebuffer at each marker's
//! center, located exactly via the widget's cached `data_to_pixel`. Needs a GPU
//! (real or software).

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui::{self, Color32, Pos2};
use rsplot::{PlotWidget, Symbol, YAxis};

const W: usize = 400;
const H: usize = 300;

/// A saturated-red pixel: the marker color. White background, black axes/text
/// and grey grid are all unsaturated or not red-dominant, so only a marker glyph
/// registers as red.
fn is_red(px: [u8; 4]) -> bool {
    px[0] > 150 && px[1] < 100 && px[2] < 100
}

/// Place one red point marker per `(symbol, x, y)` over the pinned view
/// `x,y ∈ [0,1]`, render headlessly, and return the raw RGBA framebuffer plus
/// the `data_to_pixel` projection of each marker center (the cached display
/// transform after rendering).
fn render_markers(markers: &[(Symbol, f64, f64)]) -> (Vec<u8>, Vec<Pos2>) {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);

    let mut plot = PlotWidget::new(&rs, 0);
    for &(symbol, x, y) in markers {
        plot.add_point_marker(x, y, Color32::RED, symbol);
    }

    // Pin the view to the unit square and drop the colorbar so nothing but the
    // markers is red in the data area.
    plot.set_show_colorbar(false);
    plot.set_auto_reset_zoom(false);
    plot.set_graph_x_limits(0.0, 1.0);
    plot.set_graph_y_limits(0.0, 1.0, YAxis::Left);

    let app = Rc::new(RefCell::new(plot));
    let app_ui = app.clone();
    let renderer = WgpuTestRenderer::from_render_state(rs);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(W as f32, H as f32))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            app_ui.borrow_mut().show(ui);
        });

    harness.step();
    harness.step();
    let image = harness.render().expect("headless wgpu render");
    let raw = image.as_raw().to_vec();

    let projected = markers
        .iter()
        .map(|&(_, x, y)| {
            app.borrow()
                .data_to_pixel(x, y, YAxis::Left)
                .expect("display transform cached after render")
        })
        .collect();
    (raw, projected)
}

/// The RGBA pixel at framebuffer position `(x, y)` (rounded). Panics if the probe
/// falls outside the frame, so a mislocated probe fails loudly rather than
/// silently reading the wrong pixel.
fn pixel_at(raw: &[u8], x: f32, y: f32) -> [u8; 4] {
    let col = x.round() as i64;
    let row = y.round() as i64;
    assert!(
        col >= 0 && (col as usize) < W && row >= 0 && (row as usize) < H,
        "probe ({x:.1},{y:.1}) is outside the {W}x{H} frame"
    );
    let i = (row as usize * W + col as usize) * 4;
    [raw[i], raw[i + 1], raw[i + 2], raw[i + 3]]
}

#[test]
fn newly_catalogued_marker_symbols_render_geometry() {
    // Three glyphs that were impossible under the old 7-variant `MarkerSymbol`,
    // each whose geometry covers the marker center at the default size (8 pt):
    //   * Heart        — filled cardioid stand-in; center lies inside the lower
    //                    filled triangle.
    //   * HorizontalLine — a stroke through the center along x.
    //   * VerticalLine   — a stroke through the center along y.
    // Spaced 0.25 data units apart so their ~±4 px glyphs never overlap.
    let (raw, centers) = render_markers(&[
        (Symbol::Heart, 0.25, 0.5),
        (Symbol::HorizontalLine, 0.5, 0.5),
        (Symbol::VerticalLine, 0.75, 0.5),
    ]);

    for (label, c) in [
        ("Heart", centers[0]),
        ("HorizontalLine", centers[1]),
        ("VerticalLine", centers[2]),
    ] {
        let px = pixel_at(&raw, c.x, c.y);
        assert!(
            is_red(px),
            "the {label} marker glyph must paint red at its center: px={px:?}"
        );
    }

    // A point in the data area well clear of every marker row is background:
    // proves the red above is the glyph, not a full-canvas fill.
    let empty = pixel_at(&raw, centers[1].x, centers[1].y - 60.0);
    assert!(
        !is_red(empty),
        "the data area away from any marker must be background, not red: empty={empty:?}"
    );
}
