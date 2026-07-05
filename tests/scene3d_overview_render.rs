//! GPU render tests of the orientation indicator (R1-20): silx
//! `_OverviewViewport` (`Plot3DWidget.py:51-93`) draws a half-transparent
//! white disc and the RGB axes into a 100×100 px viewport pinned to the
//! top-right corner (`:387-388`), with a camera slaved to the main camera's
//! orientation. It is visible by default (`:165`) and toggled through
//! `setOrientationIndicatorVisible` (`:320-336`).

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use rsplot::{OVERVIEW_SIZE_PX, SceneWidget};

const W: u32 = 300;
const H: u32 = 240;

/// Pixel coordinates (x, y) whose RGBA differs between `a` and `b`
/// (`width`-wide tightly packed RGBA8, top row first).
fn diff_pixels(a: &[u8], b: &[u8], width: u32) -> Vec<(u32, u32)> {
    assert_eq!(a.len(), b.len());
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .enumerate()
        .filter(|(_, (pa, pb))| pa != pb)
        .map(|(i, _)| (i as u32 % width, i as u32 / width))
        .collect()
}

#[test]
fn indicator_is_on_by_default_and_toggles() {
    let rs = create_render_state(default_wgpu_setup());
    let mut w = SceneWidget::new(&rs, 61);
    // silx viewports = [viewport, overview] by default (Plot3DWidget.py:165).
    assert!(w.is_orientation_indicator_visible());
    w.set_orientation_indicator_visible(false);
    assert!(!w.is_orientation_indicator_visible());
    w.set_orientation_indicator_visible(true);
    assert!(w.is_orientation_indicator_visible());
}

#[test]
fn indicator_draws_only_in_the_top_right_corner() {
    let rs = create_render_state(default_wgpu_setup());
    let mut w = SceneWidget::new(&rs, 62);
    let on = w.snapshot(&rs, (W, H)).expect("snapshot");
    w.set_orientation_indicator_visible(false);
    let off = w.snapshot(&rs, (W, H)).expect("snapshot");

    let diffs = diff_pixels(&on, &off, W);
    assert!(!diffs.is_empty(), "the indicator must draw something");
    for &(x, y) in &diffs {
        assert!(
            x >= W - OVERVIEW_SIZE_PX && y < OVERVIEW_SIZE_PX,
            "indicator pixel leaked outside the corner viewport at ({x}, {y})"
        );
    }
    // The viewport centre is covered by the disc (and the axes converging at
    // the origin), so it must be among the changed pixels.
    let centre = (W - OVERVIEW_SIZE_PX / 2, OVERVIEW_SIZE_PX / 2);
    assert!(
        diffs.contains(&centre),
        "the disc must cover the viewport centre {centre:?}"
    );
    // Off the axis lines, the half-transparent white disc blends over the
    // grey-51 background: some changed pixel is a strictly brighter grey.
    let brightened_grey = diffs.iter().any(|&(x, y)| {
        let i = ((y * W + x) * 4) as usize;
        let (r, g, b) = (on[i], on[i + 1], on[i + 2]);
        r == g && g == b && r > 51 && r < 255
    });
    assert!(
        brightened_grey,
        "the disc backdrop must blend white at 50% over the background"
    );
}

#[test]
fn indicator_skipped_when_target_smaller_than_its_viewport() {
    let rs = create_render_state(default_wgpu_setup());
    let mut w = SceneWidget::new(&rs, 63);
    let small = (OVERVIEW_SIZE_PX - 20, OVERVIEW_SIZE_PX - 20);
    let on = w.snapshot(&rs, small).expect("snapshot");
    w.set_orientation_indicator_visible(false);
    let off = w.snapshot(&rs, small).expect("snapshot");
    assert_eq!(on, off, "no indicator fits a target smaller than 100 px");
}
