//! Headless wgpu readback proving a Band ROI in silx **UnboundedMode**
//! (`RoiInteractionMode::BandUnbounded`) draws the three view-spanning parallel
//! lines, whereas the default **BoundedMode** draws only the corner polygon
//! confined to the band's `begin → end` extent.
//!
//! The band `begin=(5,5) end=(9,5) width=2` sits in the right half of an
//! `x∈[0,10] y∈[0,10]` view (pinned, with a blue anchor curve so an empty
//! plot's view cannot auto-fit to the ROI): its BoundedMode polygon spans data
//! `x∈[5,9]` and never reaches the left of the data area, while its
//! UnboundedMode lines (`y=4,5,6`) span the full `x∈[0,10]` and so paint red all
//! the way to the data-area left edge (data `x=0`). The discriminator is the
//! *leftmost image column carrying the ROI's red* — margin-independent:
//! UnboundedMode reaches the data-area left edge, BoundedMode stops at data
//! `x=5`, half the data width to the right. The span only reaches the left edge
//! if `chrome::draw_roi` actually swapped the polygon for the
//! `band_unbounded_segments` lines based on the ROI's interaction mode. A thick
//! ROI line keeps the otherwise 1 px horizontal lines solidly red instead of
//! anti-aliasing to pink against the white background.
//!
//! Mirrors `tests/multi_axis_render.rs`' harness. Needs a GPU (real or
//! software).

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui::{self, Color32};
use rsplot::{ManagedRoi, PlotWidget, Roi, RoiInteractionMode, YAxis};

const W: usize = 400;
const H: usize = 300;

fn is_red(px: &[u8]) -> bool {
    px[0] > 200 && px[1] < 80 && px[2] < 80
}

/// Per-column red-ish (ROI default `Color32::RED`) pixel histogram over the
/// frame, indexed by image column `0..W`.
fn red_columns(raw: &[u8]) -> Vec<u32> {
    let mut cols = vec![0u32; W];
    for (i, px) in raw.chunks_exact(4).enumerate() {
        if is_red(px) {
            cols[i % W] += 1;
        }
    }
    cols
}

/// `(leftmost image column with >=3 red px, total red px)`. The >=3 floor
/// rejects stray anti-aliased specks so the column reflects a real thick line or
/// edge. Leftmost is `W` (sentinel, past the right edge) when nothing qualifies.
fn render(mode: RoiInteractionMode) -> (usize, u32) {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);

    let mut plot = PlotWidget::new(&rs, 0);
    // Anchor the view to x∈[0,10] y∈[0,10] with a non-red (blue) curve at the
    // corners — an empty plot's view auto-fits to the ROI extent, which would
    // make the unbounded lines span only the band's own [5,9] and erase the
    // distinction. The blue corners are not counted by `is_red`.
    plot.add_curve(&[0.0, 10.0], &[0.0, 10.0], Color32::from_rgb(0, 0, 255));
    plot.set_auto_reset_zoom(false);
    plot.set_graph_x_limits(0.0, 10.0);
    plot.set_graph_y_limits(0.0, 10.0, YAxis::Left);

    // Thick line so the horizontal unbounded lines stay solidly red.
    let mut managed = ManagedRoi::new(Roi::Band {
        begin: (5.0, 5.0),
        end: (9.0, 5.0),
        width: 2.0,
    });
    managed.line_width = 4.0;
    let idx = plot.add_managed_roi(managed);
    assert!(
        plot.set_roi_interaction_mode(idx, mode),
        "Band must accept {mode:?}"
    );

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
    let cols = red_columns(image.as_raw());
    let leftmost = cols.iter().position(|&c| c >= 3).unwrap_or(W);
    let total: u32 = cols.iter().sum();
    (leftmost, total)
}

#[test]
fn band_unbounded_mode_spans_the_view_bounded_mode_does_not() {
    let (unbounded_left, unbounded_total) = render(RoiInteractionMode::BandUnbounded);
    let (bounded_left, bounded_total) = render(RoiInteractionMode::BandBounded);

    // Both modes draw the band somewhere.
    assert!(
        unbounded_total > 0 && bounded_total > 0,
        "both modes must draw the band: unbounded={unbounded_total} bounded={bounded_total} total red px"
    );
    // Both must qualify a leftmost column (not the W sentinel).
    assert!(
        unbounded_left < W && bounded_left < W,
        "both modes must paint a real line/edge: unbounded_left={unbounded_left} bounded_left={bounded_left} (W={W})"
    );
    // UnboundedMode's lines reach the data-area left edge (data x=0); the
    // BoundedMode polygon's leftmost edge is at data x=5, half the data width to
    // the right. Require a clear separation so a stray speck can't pass it.
    assert!(
        bounded_left > unbounded_left + 80,
        "UnboundedMode must reach far left of BoundedMode: unbounded_left={unbounded_left} bounded_left={bounded_left}"
    );
}
