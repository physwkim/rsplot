//! Median-filter original capture (R2-7): silx `MedianFilterDialog` keeps
//! `_originalImage` and refilters IT on every kernel change
//! (medfilt.py:83-102), so repeated Apply never compounds. rsplot mirrors
//! that with a per-handle capture that survives the filter's own replace and
//! colormap-only re-uploads, and is dropped when the pixels really change.
//!
//! Needs a GPU (real or software) for the image upload only; no frame is
//! rendered — mirrors `tests/alpha_slider_binding.rs`.

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use rsplot::actions::analysis::median_filter_2d;
use rsplot::{AutoscaleMode, Colormap, ImageSpec, ItemHandle, PlotWidget};

const W: usize = 5;
const H: usize = 5;

/// A 5×5 image with two hot pixels — medfilt3 and medfilt5 of it differ, and
/// medfilt5(medfilt3(x)) differs from medfilt5(x), which is what
/// distinguishes refilter-the-original from compounding.
fn hot_pixel_data() -> Vec<f64> {
    let mut data: Vec<f64> = (0..W * H).map(|i| (i % 7) as f64).collect();
    data[2 * W + 2] = 100.0;
    data[W + 3] = 50.0;
    data
}

fn plot_with_image(data: &[f64]) -> (PlotWidget, ItemHandle) {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);
    let mut plot = PlotWidget::new(&rs, 0);
    let pixels: Vec<f32> = data.iter().map(|&v| v as f32).collect();
    let spec = ImageSpec::scalar(W as u32, H as u32, &pixels, Colormap::viridis(0.0, 100.0));
    let handle = plot.add_image_spec(spec);
    plot.set_active_image(Some(handle));
    (plot, handle)
}

/// The active image's retained pixels.
fn retained_pixels(plot: &PlotWidget) -> Vec<f64> {
    plot.get_image_pixels_raw().expect("retained image data")
}

/// The reference: one direct filter pass over the pristine data.
fn direct_filter(data: &[f64], k: usize) -> Vec<f64> {
    median_filter_2d(data, W, H, k, k, false)
}

#[test]
fn repeated_apply_refilters_the_original_not_the_result() {
    let original = hot_pixel_data();
    let (mut plot, _handle) = plot_with_image(&original);

    // Sanity: the compounded and direct width-5 results actually differ,
    // otherwise this test cannot discriminate.
    let compounded = direct_filter(&direct_filter(&original, 3), 5);
    assert_ne!(compounded, direct_filter(&original, 5));

    // Apply width 3 then width 5: silx shows medfilt5(orig).
    assert!(plot.apply_median_filter(3, false));
    assert_eq!(retained_pixels(&plot), direct_filter(&original, 3));
    assert!(plot.apply_median_filter(5, false));
    assert_eq!(
        retained_pixels(&plot),
        direct_filter(&original, 5),
        "second Apply must refilter the ORIGINAL, not compound on width 3"
    );
    // And back down to 3 — still from the original.
    assert!(plot.apply_median_filter(3, false));
    assert_eq!(retained_pixels(&plot), direct_filter(&original, 3));
}

#[test]
fn colormap_only_reupload_keeps_the_capture() {
    // silx colormap edits don't re-add the image, so `_originalImage`
    // survives an autoscale between two kernel changes.
    let original = hot_pixel_data();
    let (mut plot, _handle) = plot_with_image(&original);

    assert!(plot.apply_median_filter(3, false));
    plot.autoscale_active_image(AutoscaleMode::MinMax)
        .expect("autoscale re-uploads with the same pixels");
    assert!(plot.apply_median_filter(5, false));
    assert_eq!(
        retained_pixels(&plot),
        direct_filter(&original, 5),
        "an autoscale between Applies must not clear the original"
    );
}

#[test]
fn replacing_the_pixels_recaptures() {
    // A real data replacement is silx's `sigActiveImageChanged`: the next
    // Apply captures (and filters) the NEW image.
    let original = hot_pixel_data();
    let (mut plot, handle) = plot_with_image(&original);
    assert!(plot.apply_median_filter(3, false));

    let mut replacement: Vec<f64> = (0..W * H).map(|i| (i % 5) as f64).collect();
    replacement[7] = 40.0;
    let pixels: Vec<f32> = replacement.iter().map(|&v| v as f32).collect();
    let spec = ImageSpec::scalar(W as u32, H as u32, &pixels, Colormap::viridis(0.0, 40.0));
    assert!(plot.update_image_spec(handle, spec));

    assert!(plot.apply_median_filter(3, false));
    assert_eq!(
        retained_pixels(&plot),
        direct_filter(&replacement, 3),
        "after a pixel replacement the filter must work from the new data"
    );
}
