//! Headless wgpu readback proving a solid curve renders **round joins** and
//! **round caps** (silx's pygfx `LineMaterial` default), not the bare butt caps
//! the per-segment quads give on their own.
//!
//! rsplot draws a polyline as independent butt-capped segment quads, so a sharp
//! turn leaves a wedge-shaped gap on the outer side of the join and the two ends
//! are flat. `GpuCurve::draw_caps` stamps an antialiased disc of the line width
//! at every vertex; the union with the segment quads is a round-joined,
//! round-capped stroke. Both tests locate the geometry exactly via the widget's
//! cached `data_to_pixel`, then probe individual framebuffer pixels — no
//! hardcoded data-area mapping. Needs a GPU (real or software).

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui::{self, Color32, Pos2};
use rsplot::{CurveSpec, PlotWidget, YAxis};

const W: usize = 400;
const H: usize = 300;
/// Line width in physical pixels; the round join/cap disc has this diameter, so
/// its radius (the nominal half-width) is `HALF`.
const WIDTH: f32 = 24.0;
const HALF: f32 = WIDTH / 2.0;

/// A saturated-red pixel: the curve color. White background, black axes/text and
/// grey grid are all unsaturated or not red-dominant, so only the line registers.
fn is_red(px: [u8; 4]) -> bool {
    px[0] > 150 && px[1] < 100 && px[2] < 100
}

/// Render one solid red curve of [`WIDTH`] over the pinned view `x,y ∈ [0,1]`,
/// returning the raw RGBA framebuffer and the `data_to_pixel` projection of each
/// `probe` data point (the cached display transform after rendering).
fn render_curve(x: &[f64], y: &[f64], probes: &[(f64, f64)]) -> (Vec<u8>, Vec<Pos2>) {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);

    let mut plot = PlotWidget::new(&rs, 0);
    let mut spec = CurveSpec::new(x, y, Color32::RED);
    spec.line_width = WIDTH;
    plot.add_curve_spec(spec);

    // Pin the view to the data extent and drop the colorbar so nothing but the
    // curve is red in the data area.
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

    let projected = probes
        .iter()
        .map(|&(px, py)| {
            app.borrow()
                .data_to_pixel(px, py, YAxis::Left)
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
fn solid_curve_has_round_caps_at_endpoints() {
    // A thick horizontal line; probe its left endpoint's centerline.
    let (raw, p) = render_curve(&[0.2, 0.8], &[0.5, 0.5], &[(0.2, 0.5)]);
    let e0 = p[0];

    // Sanity: the line body just inside the left end is opaque red.
    assert!(
        is_red(pixel_at(&raw, e0.x + 6.0, e0.y)),
        "the line body must be red"
    );

    // Cap tip: 9 px LEFT of the endpoint on the centerline → inside the disc
    // (dist 9 < HALF) → red. A butt cap leaves everything left of the endpoint
    // empty, so this probe is the cap's signature.
    let tip = pixel_at(&raw, e0.x - (HALF - 3.0), e0.y);
    assert!(
        is_red(tip),
        "a round cap must extend ~half the width beyond the endpoint: tip={tip:?}"
    );

    // Rounded, not square: the bounding-box corner beyond the endpoint
    // (offset (-(HALF+2), -(HALF+2)) → dist ≈ 19.8 ≫ HALF+1) is background. A
    // square cap would paint this corner red.
    let corner = pixel_at(&raw, e0.x - (HALF + 2.0), e0.y - (HALF + 2.0));
    assert!(
        !is_red(corner),
        "a round cap's corner must be background (not a square cap): corner={corner:?}"
    );
}

#[test]
fn solid_curve_has_round_joins_at_turns() {
    // Right-angle bend: (0.3,0.3) → (0.7,0.3) → (0.7,0.7). The interior vertex
    // (0.7,0.3) joins a rightward segment to an upward one.
    let (raw, p) = render_curve(&[0.3, 0.7, 0.7], &[0.3, 0.3, 0.7], &[(0.7, 0.3)]);
    let v = p[0];

    // The outer side of this turn is down-right in screen space (+col,+row):
    // incoming +x, outgoing up (−row) is a CCW turn whose outer corner is +col,
    // +row. A point 8 px down-right of the vertex (dist ≈ 11.3 < HALF) lies in
    // that wedge — beyond BOTH butt segment quads (col > vertex+1 escapes the
    // horizontal segment, row > vertex+1 escapes the vertical one) — so it is
    // red ONLY because the round-join disc fills it.
    let wedge = pixel_at(&raw, v.x + 8.0, v.y + 8.0);
    assert!(
        is_red(wedge),
        "a round join must fill the outer wedge gap at a sharp turn: wedge={wedge:?}"
    );

    // Bounded (rounded, no miter spike): a point past the disc radius along the
    // same diagonal (dist ≈ 21 ≫ HALF+1) is background.
    let far = pixel_at(&raw, v.x + (HALF + 3.0), v.y + (HALF + 3.0));
    assert!(
        !is_red(far),
        "a round join must not extend past the line width (no miter spike): far={far:?}"
    );
}
