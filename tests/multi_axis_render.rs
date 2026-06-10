//! Headless wgpu readback proving a curve bound to an extra (stacked) Y axis
//! renders against *that axis'* scale, not the left axis.
//!
//! A red flat line at y≈150 is drawn two ways: bound to an extra right axis with
//! range `[100, 200]` (so it lands mid-plot and renders), and left-bound with
//! the left axis pinned to `[0, 1]` (so y=150 is far above the top and the data
//! clip rect drops it). The pixel counts must differ sharply, which they only do
//! if the extra-axis transform is actually wired through the GPU path.
//!
//! Mirrors `tests/mask_pointer_offset.rs`' harness. Needs a GPU (real or
//! software).

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use siplot::egui::{self, Color32};
use siplot::{AxisSide, Plot1D, YAxis};

fn count_red(raw: &[u8]) -> u32 {
    raw.chunks_exact(4)
        .filter(|px| px[0] > 200 && px[1] < 80 && px[2] < 80)
        .count() as u32
}

/// Render a blue flat line (left axis, y=0.5) plus a red flat line at y=150.
/// When `bind_extra` is set the red line is bound to an extra right axis with
/// range `[100, 200]`; otherwise it stays on the left axis pinned to `[0, 1]`.
/// Returns the red pixel count.
fn render(bind_extra: bool) -> u32 {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);

    let mut plot = Plot1D::new(&rs, 0);
    // Deterministic view: pin every axis, no auto reset-zoom.
    plot.set_auto_reset_zoom(false);
    plot.set_graph_x_limits(0.0, 10.0);
    plot.set_graph_y_limits(0.0, 1.0, YAxis::Left);

    let xs: Vec<f64> = (0..=10).map(|i| i as f64).collect();
    let left_ys = vec![0.5; xs.len()];
    let right_ys = vec![150.0; xs.len()];

    let _a = plot.add_curve(&xs, &left_ys, Color32::from_rgb(0, 0, 255));
    let b = plot.add_curve(&xs, &right_ys, Color32::from_rgb(255, 0, 0));
    if bind_extra {
        let idx = plot.plot_mut().add_extra_axis(AxisSide::Right);
        plot.set_graph_y_limits(100.0, 200.0, YAxis::Extra(idx));
        assert!(plot.set_curve_y_axis(b, YAxis::Extra(idx)));
    }

    let app = Rc::new(RefCell::new(plot));
    let app_ui = app.clone();
    let renderer = WgpuTestRenderer::from_render_state(rs);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(400.0, 300.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            app_ui.borrow_mut().show(ui);
        });

    harness.step();
    harness.step();
    let image = harness.render().expect("headless wgpu render");
    count_red(image.as_raw())
}

#[test]
fn extra_axis_api_autoscales_and_toggles() {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);
    let mut plot = Plot1D::new(&rs, 0);

    let idx = plot.add_extra_y_axis(AxisSide::Right);
    assert_eq!(idx, 0);
    assert_eq!(plot.extra_y_axis_count(), 1);

    // Unknown index is rejected by the per-axis setters.
    assert!(!plot.set_extra_y_autoscale(9, false));
    assert!(!plot.set_extra_y_log(9, true));

    // Log toggle round-trips on a known index.
    assert!(plot.set_extra_y_log(idx, true));
    assert!(plot.is_extra_y_log(idx));
    assert!(plot.set_extra_y_log(idx, false));
    assert!(!plot.is_extra_y_log(idx));

    // With autoscale on (the default), binding a curve to the extra axis fits
    // that axis to the curve's data through the high-level accumulation path
    // (DataBounds.extra -> reset_extra_axes_to), with no explicit range set.
    let xs: Vec<f64> = (0..=10).map(|i| i as f64).collect();
    let ys: Vec<f64> = xs.iter().map(|&x| 140.0 + x).collect(); // 140..=150
    let h = plot.add_curve(&xs, &ys, Color32::from_rgb(255, 0, 0));
    assert!(plot.set_curve_y_axis(h, YAxis::Extra(idx)));
    let (lo, hi) = plot
        .get_graph_y_limits(YAxis::Extra(idx))
        .expect("extra axis range autoscaled from its curve");
    assert!(
        lo <= 140.0 && hi >= 150.0,
        "extra axis should fit its curve's 140..150 span; got {lo}..{hi}"
    );
}

#[test]
fn curve_on_extra_axis_renders_against_its_own_scale() {
    let with_extra = render(true);
    let without = render(false);
    assert!(
        with_extra > 100,
        "an extra-axis curve at y=150 (axis range 100..200) should render mid-plot; got {with_extra}"
    );
    assert!(
        without < with_extra / 4,
        "the same line left-bound (left axis 0..1) is off-scale above the top and clipped; \
         got {without} red px vs {with_extra} on the extra axis"
    );
}
