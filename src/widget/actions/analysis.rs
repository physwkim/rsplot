//! Image-analysis actions, mirroring silx `silx.gui.plot.actions.medfilt` and
//! `silx.gui.plot.actions.histogram`.
//!
//! These actions read the active image's raw scalar pixels and either transform
//! them (median filter) or summarize them (pixel-intensity histogram). The
//! load-bearing numerics live here as pure functions so they are unit-testable
//! without a GPU backend; the toolbar buttons, the kernel/conditional popup, and
//! the histogram display window in [`crate::widget::high_level`] are thin shims
//! that call these functions and re-upload / draw the result.
//!
//! Fidelity anchors:
//! - [`median_filter_2d`] reproduces silx `silx.math.medianfilter.medfilt2d`
//!   (the C++ `median<T>()` in `include/median_filter.hpp`) for the default
//!   `mode='nearest'` edge handling that `MedianFilterAction` relies on.
//! - [`pixel_intensity_histogram`] reproduces the histogram + statistics that
//!   silx `PixelIntensitiesHistoAction` / `HistogramWidget` compute for an
//!   image (unweighted counts, `last_bin_closed=True`, finite range).

use medians::Medianf64;

/// Window-median selection matching silx's C++ `median<T>()`.
///
/// silx fills a window with the valid (non-NaN) values, then returns
/// `sorted[window_size / 2]` (integer floor division) via `std::nth_element`.
/// For an odd window that is the exact median; for an even window (which only
/// arises when NaNs reduce the count, since silx kernels are odd) it is the
/// *higher* of the two central values, as the silx docstring states ("the
/// highest of the 2 central sorted values is taken").
///
/// `values` must be non-empty (the caller handles the all-NaN empty-window case
/// by emitting NaN). `values` is consumed/reordered in place.
fn silx_window_median(values: &mut [f64]) -> f64 {
    let n = values.len();
    debug_assert!(n > 0, "silx_window_median requires a non-empty window");
    if n & 1 == 1 {
        // Odd window, NaN-free here: medians' medf_unchecked returns sorted[n/2],
        // exactly the silx selection.
        (&*values).medf_unchecked()
    } else {
        // Even window (NaNs reduced an odd kernel to an even valid count): silx
        // takes the higher of the two central values, i.e. sorted[n/2]. medians'
        // even path averages the two centrals, so select sorted[n/2] directly.
        let pivot = n / 2;
        values.select_nth_unstable_by(pivot, |a, b| a.total_cmp(b));
        values[pivot]
    }
}

/// Clamp `index` into `[0, length - 1]` (silx C++ `NEAREST` edge mode: the
/// out-of-bounds window index is clamped to the nearest valid pixel).
fn nearest_index(index: isize, length: usize) -> usize {
    if index < 0 {
        0
    } else if index as usize >= length {
        length - 1
    } else {
        index as usize
    }
}

/// 2D median filter with silx's default `mode='nearest'` edge handling.
///
/// Reproduces silx `silx.math.medianfilter.medfilt2d(data, (kernel_h, kernel_w),
/// conditional)` (the C++ `median<T>()` in `median_filter.hpp`) for the default
/// `mode='nearest'`, which is the mode `MedianFilterAction` relies on (it calls
/// `medfilt2d` without a `mode`).
///
/// - `data` is row-major, `width * height` long.
/// - `kernel_h`, `kernel_w` are the kernel height (rows / y) and width
///   (cols / x); both must be odd and >= 1 (silx asserts `(k - 1) % 2 == 0`).
/// - Edge windows clamp out-of-range indices to the nearest valid pixel
///   (`NEAREST`).
/// - NaN values inside a window are ignored; if a window is entirely NaN the
///   output pixel is NaN.
/// - `conditional`: when true, the center pixel is replaced by the window median
///   *only* if it equals the window min or max (an extremum); otherwise it is
///   kept unchanged. NaN centers are never an extremum, so they propagate
///   unchanged.
///
/// # Panics
///
/// Panics if `data.len() != width * height`, or if `kernel_h` / `kernel_w` is
/// zero or even (mirroring silx's odd-kernel assertion).
pub fn median_filter_2d(
    data: &[f64],
    width: usize,
    height: usize,
    kernel_h: usize,
    kernel_w: usize,
    conditional: bool,
) -> Vec<f64> {
    assert_eq!(
        data.len(),
        width * height,
        "median_filter_2d: data length {} != width*height {}",
        data.len(),
        width * height
    );
    assert!(
        kernel_h >= 1 && kernel_w >= 1,
        "median_filter_2d: kernel dimensions must be >= 1"
    );
    assert!(
        kernel_h % 2 == 1 && kernel_w % 2 == 1,
        "median_filter_2d: kernel dimensions must be odd (silx odd-kernel assertion)"
    );

    let mut output = vec![0.0_f64; data.len()];
    if width == 0 || height == 0 {
        return output;
    }

    let half_y = (kernel_h - 1) / 2;
    let half_x = (kernel_w - 1) / 2;
    // Reused per-pixel scratch buffer (silx allocates kernel_h*kernel_w then
    // tracks the count of valid values pushed).
    let mut window: Vec<f64> = Vec::with_capacity(kernel_h * kernel_w);

    for y in 0..height {
        for x in 0..width {
            window.clear();
            for ky in 0..kernel_h {
                let win_y = y as isize + ky as isize - half_y as isize;
                let iy = nearest_index(win_y, height);
                for kx in 0..kernel_w {
                    let win_x = x as isize + kx as isize - half_x as isize;
                    let ix = nearest_index(win_x, width);
                    let value = data[iy * width + ix];
                    if !value.is_nan() {
                        window.push(value);
                    }
                }
            }

            let center = data[y * width + x];
            if window.is_empty() {
                // Entire window is NaN.
                output[y * width + x] = f64::NAN;
                continue;
            }

            if conditional {
                // Conditional: replace only if center is the window min or max.
                let mut win_min = window[0];
                let mut win_max = window[0];
                for &v in &window[1..] {
                    if v > win_max {
                        win_max = v;
                    }
                    if v < win_min {
                        win_min = v;
                    }
                }
                // NaN center is never == min/max, so it propagates unchanged.
                if center == win_max || center == win_min {
                    output[y * width + x] = silx_window_median(&mut window);
                } else {
                    output[y * width + x] = center;
                }
            } else {
                output[y * width + x] = silx_window_median(&mut window);
            }
        }
    }

    output
}

/// 1D median filter, matching silx `MedianFilter1DAction` which calls
/// `medfilt2d(image, (kernel_width, 1), conditional)`.
///
/// silx's 1D action uses a kernel of `(kernel_width, 1)` — i.e. it filters along
/// the rows (height direction / y), with a width-1 column. This is a thin
/// wrapper over [`median_filter_2d`] with `kernel_h = kernel_width`,
/// `kernel_w = 1`.
///
/// # Panics
///
/// Panics under the same conditions as [`median_filter_2d`].
pub fn median_filter_1d(
    data: &[f64],
    width: usize,
    height: usize,
    kernel_width: usize,
    conditional: bool,
) -> Vec<f64> {
    median_filter_2d(data, width, height, kernel_width, 1, conditional)
}

/// A pixel-intensity histogram with summary statistics, matching silx
/// `PixelIntensitiesHistoAction` / `HistogramWidget`.
///
/// Built by [`pixel_intensity_histogram`]. `edges` has `n_bins + 1` entries
/// (the bin boundaries, `edges[0] == min`, `edges[n_bins] == max`), and `counts`
/// has `n_bins` entries (unweighted pixel counts, silx default — the action's
/// weight checkbox defaults off). The statistics are computed over the finite
/// pixels only (silx `numpy.nanmean` / `nanstd` / `nansum`, and finite min/max
/// from `min_max(..., finite=True)`).
#[derive(Clone, Debug, PartialEq)]
pub struct PixelHistogram {
    /// Bin edges, `n_bins + 1` entries (`edges[0] == min`, last == `max`).
    pub edges: Vec<f64>,
    /// Per-bin pixel counts, `n_bins` entries (unweighted).
    pub counts: Vec<u64>,
    /// Finite minimum pixel value (histogram range lower bound).
    pub min: f64,
    /// Finite maximum pixel value (histogram range upper bound).
    pub max: f64,
    /// Mean of the finite pixels (silx `numpy.nanmean`).
    pub mean: f64,
    /// Population standard deviation of the finite pixels (silx `numpy.nanstd`).
    pub std: f64,
    /// Sum of the finite pixels (silx `numpy.nansum`).
    pub sum: f64,
    /// Number of bins (`counts.len()`).
    pub n_bins: usize,
}

/// Compute a pixel-intensity histogram with statistics, matching silx
/// `PixelIntensitiesHistoAction` / `HistogramWidget._updateFromItem`.
///
/// silx defaults reproduced here:
/// - Bin count: `min(1024, floor(sqrt(finite_count)))`, then clamped to a
///   minimum of 2 (silx `max(2, guessed_nbins)`). When `n_bins` is `Some(n)`
///   the caller's value is used instead (still floored at 2), mirroring the
///   widget's editable bin-count field. (silx's integer-dtype `xmax - xmin`
///   clamp does not apply: these pixels are real-valued.)
/// - Range: the finite `(min, max)` of the pixels (silx `min_max(finite=True)`).
///   Non-finite pixels (NaN, ±inf) are excluded from both the range and the
///   counts. A degenerate range (`min == max`) is enlarged exactly as silx does
///   (`(-0.01, 0.01)` when the value is 0, else `(v*0.99, v*1.01)` sorted) so
///   the equal pixels land in a real bin.
/// - Binning: silx `Histogramnd` with `last_bin_closed=True` — a value equal to
///   the max lands in the last bin (not discarded), and an interior value `v`
///   lands in `floor((v - min) * n_bins / (max - min))`.
/// - Statistics: `min`/`max` are the finite range bounds; `mean`/`std`/`sum`
///   are over the finite pixels (silx `nanmean`/`nanstd`/`nansum`), `std` being
///   the population standard deviation.
///
/// Returns `None` when there is no finite pixel (silx `HistogramWidget.reset`
/// on all-not-finite data).
pub fn pixel_intensity_histogram(pixels: &[f64], n_bins: Option<usize>) -> Option<PixelHistogram> {
    // Finite pass: range bounds and running stats over finite pixels only.
    let mut finite_count: usize = 0;
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut sum = 0.0_f64;
    for &v in pixels {
        if v.is_finite() {
            finite_count += 1;
            if v < min {
                min = v;
            }
            if v > max {
                max = v;
            }
            sum += v;
        }
    }
    if finite_count == 0 {
        // All-not-finite data: silx resets (no histogram).
        return None;
    }

    let mean = sum / finite_count as f64;
    // Population standard deviation (numpy.nanstd default ddof=0).
    let mut var_acc = 0.0_f64;
    for &v in pixels {
        if v.is_finite() {
            let d = v - mean;
            var_acc += d * d;
        }
    }
    let std = (var_acc / finite_count as f64).sqrt();

    // Bin count: silx guessed = min(1024, floor(sqrt(size))), then max(2, ...).
    // Here `size` is the finite-pixel count (the values that actually populate
    // the histogram). A caller-supplied count overrides the guess.
    let guessed = (finite_count as f64).sqrt().floor() as usize;
    let nbins = n_bins.unwrap_or_else(|| guessed.min(1024)).max(2);

    // Histogram range: enlarge a degenerate (min == max) range as silx does.
    let (g_min, g_max) = if min == max {
        if min == 0.0 {
            (-0.01, 0.01)
        } else {
            let a = min * 0.99;
            let b = min * 1.01;
            if a <= b { (a, b) } else { (b, a) }
        }
    } else {
        (min, max)
    };
    let range = g_max - g_min;

    // Bin edges: silx Histogramnd builds edge[k] = g_min + k*(range/n_bins) for
    // k in 0..n_bins, and sets the final edge exactly to g_max.
    let mut edges = Vec::with_capacity(nbins + 1);
    for k in 0..nbins {
        edges.push(g_min + k as f64 * (range / nbins as f64));
    }
    edges.push(g_max);

    // Counts: silx Histogramnd binning with last_bin_closed = True.
    let mut counts = vec![0_u64; nbins];
    for &v in pixels {
        if !v.is_finite() {
            continue; // NaN/inf excluded from counts.
        }
        if v < g_min {
            continue; // Below range: discarded.
        }
        let bin = if v < g_max {
            // floor((v - g_min) * n_bins / range), truncating a non-negative
            // value exactly like silx's (long) cast.
            (((v - g_min) * nbins as f64) / range) as usize
        } else if v == g_max {
            // last_bin_closed: the max value lands in the last bin.
            nbins - 1
        } else {
            continue; // Above range: discarded.
        };
        // Guard against a floating-point bin index landing exactly at n_bins
        // (silx's (long) truncation keeps interior v < g_max strictly below
        // n_bins, but clamp defensively to the last bin).
        let bin = bin.min(nbins - 1);
        counts[bin] += 1;
    }

    Some(PixelHistogram {
        edges,
        counts,
        min,
        max,
        mean,
        std,
        sum,
        n_bins: nbins,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3x3 image with a salt spike in the center; a 3x3 median removes it.
    /// The interior pixel (1,1) sees all 9 values {0..8 with the center bumped
    /// to 100}; the sorted middle is unaffected by the spike, so the median is
    /// the value that would be there for a smooth ramp.
    #[test]
    fn median_filter_2d_removes_salt_spike() {
        // Smooth field with one hot pixel in the middle.
        let data = [
            1.0, 1.0, 1.0, //
            1.0, 100.0, 1.0, //
            1.0, 1.0, 1.0,
        ];
        let out = median_filter_2d(&data, 3, 3, 3, 3, false);
        // Center window is all nine pixels: eight 1.0 and one 100.0 -> median 1.0.
        assert_eq!(out[4], 1.0, "salt spike at center should be removed");
    }

    /// Interior median of a known 3x3 window with distinct values.
    #[test]
    fn median_filter_2d_interior_median_is_sorted_middle() {
        // 3x3, center window = {1,2,3,4,5,6,7,8,9}, median (sorted[4]) = 5.
        let data = [
            1.0, 2.0, 3.0, //
            4.0, 5.0, 6.0, //
            7.0, 8.0, 9.0,
        ];
        let out = median_filter_2d(&data, 3, 3, 3, 3, false);
        assert_eq!(out[4], 5.0);
    }

    /// Edge pixel: silx NEAREST clamps out-of-range window indices to the
    /// nearest valid pixel, so the corner (0,0) window replicates edge values.
    #[test]
    fn median_filter_2d_edge_nearest_clamping() {
        // Corner (0,0) of this image. With NEAREST, the 3x3 window indices clamp:
        //   (-1,-1)(-1,0)(-1,1)   ->  (0,0)(0,0)(0,1)
        //   ( 0,-1)( 0,0)( 0,1)   ->  (0,0)(0,0)(0,1)
        //   ( 1,-1)( 1,0)( 1,1)   ->  (1,0)(1,0)(1,1)
        // Values: a=data[0], b=data[1], c=data[width], d=data[width+1].
        let data = [
            10.0, 20.0, 30.0, //
            40.0, 50.0, 60.0, //
            70.0, 80.0, 90.0,
        ];
        // Clamped window for (0,0): a=10 (x4), b=20 (x2), c=40 (x2), d=50 (x1).
        // Sorted: [10,10,10,10,20,20,40,40,50] (9 values), sorted[4] = 20.
        let out = median_filter_2d(&data, 3, 3, 3, 3, false);
        assert_eq!(out[0], 20.0, "corner pixel uses nearest-clamped window");
    }

    /// All-equal degenerate window: every output equals the constant value.
    #[test]
    fn median_filter_2d_constant_image_unchanged() {
        let data = vec![7.0; 5 * 4];
        let out = median_filter_2d(&data, 5, 4, 3, 3, false);
        assert!(out.iter().all(|&v| v == 7.0));
    }

    /// Conditional: a non-extremum center is left unchanged, while an extremum
    /// center is replaced by the window median.
    #[test]
    fn median_filter_2d_conditional_keeps_non_extremum_fixes_extremum() {
        // 1x5 row: [1, 2, 100, 4, 5]. kernel (1,3): for each pixel the window is
        // the 3 nearest neighbours (NEAREST-clamped at the ends).
        //
        // Pixel index 2 (value 100): window {2,100,4}. min=2,max=100 -> 100 IS
        // the max, so it gets replaced by median(sorted[1] of {2,4,100}) = 4.
        //
        // Pixel index 1 (value 2): window {1,2,100}. min=1,max=100 -> 2 is
        // neither, so it is KEPT unchanged at 2.
        let data = [1.0, 2.0, 100.0, 4.0, 5.0];
        let out = median_filter_2d(&data, 5, 1, 1, 3, true);
        assert_eq!(out[2], 4.0, "extremum center replaced by window median");
        assert_eq!(out[1], 2.0, "non-extremum center kept unchanged");
    }

    /// Conditional false replaces every center by its window median, including
    /// non-extrema.
    #[test]
    fn median_filter_2d_unconditional_replaces_non_extremum() {
        let data = [1.0, 2.0, 100.0, 4.0, 5.0];
        let out = median_filter_2d(&data, 5, 1, 1, 3, false);
        // Pixel index 1 (value 2): window {1,2,100} -> median 2 (sorted middle).
        assert_eq!(out[1], 2.0);
        // Pixel index 2 (value 100): window {2,100,4} -> sorted {2,4,100} -> 4.
        assert_eq!(out[2], 4.0);
    }

    /// NaN in the window is ignored; an even valid count takes the higher of the
    /// two central values (silx "highest of the 2 central").
    #[test]
    fn median_filter_2d_nan_ignored_even_count_takes_higher_central() {
        // 1x4 row with a NaN. kernel (1,3).
        // Pixel index 1 (value 20): window indices {0,1,2} = {10, 20, NaN}.
        // NaN dropped -> valid {10, 20} (even, size 2). silx takes sorted[1] = 20.
        let data = [10.0, 20.0, f64::NAN, 40.0];
        let out = median_filter_2d(&data, 4, 1, 1, 3, false);
        assert_eq!(
            out[1], 20.0,
            "even valid count takes higher of two centrals"
        );
    }

    /// An all-NaN window yields a NaN output pixel.
    #[test]
    fn median_filter_2d_all_nan_window_is_nan() {
        let data = [f64::NAN, f64::NAN, f64::NAN];
        let out = median_filter_2d(&data, 3, 1, 1, 3, false);
        assert!(out[1].is_nan(), "all-NaN window must produce NaN");
    }

    /// A NaN center under conditional mode propagates unchanged (NaN is never an
    /// extremum, so it is never replaced).
    #[test]
    fn median_filter_2d_conditional_nan_center_propagates() {
        // 1x3 row, center is NaN, neighbours finite.
        let data = [1.0, f64::NAN, 3.0];
        let out = median_filter_2d(&data, 3, 1, 1, 3, true);
        assert!(
            out[1].is_nan(),
            "NaN center is not an extremum, propagates unchanged"
        );
    }

    /// 1D filter wraps 2D with kernel (kernel_width, 1): it filters along rows
    /// (the height/y direction), matching silx `MedianFilter1DAction`.
    #[test]
    fn median_filter_1d_matches_2d_with_height_kernel() {
        // 1 column, 5 rows: a salt spike at row 2.
        let data = [1.0, 2.0, 100.0, 4.0, 5.0];
        // kernel_width = 3 -> kernel (3, 1): each pixel's window is its column
        // neighbours. Row 2 (100): window rows {1,2,3} = {2,100,4} -> median 4.
        let out = median_filter_1d(&data, 1, 5, 3, false);
        assert_eq!(out[2], 4.0);
        let direct = median_filter_2d(&data, 1, 5, 3, 1, false);
        assert_eq!(out, direct);
    }

    /// Kernel size 1x1 is the identity (each window is the single center pixel).
    #[test]
    fn median_filter_2d_kernel_one_is_identity() {
        let data = [3.0, 1.0, 4.0, 1.0, 5.0, 9.0];
        let out = median_filter_2d(&data, 3, 2, 1, 1, false);
        assert_eq!(out, data.to_vec());
    }

    // ---- pixel_intensity_histogram ----

    /// The default bin formula: 100 finite pixels -> floor(sqrt(100)) = 10 bins.
    #[test]
    fn histogram_default_bin_count_is_sqrt_floor() {
        let pixels: Vec<f64> = (0..100).map(|i| i as f64).collect();
        let h = pixel_intensity_histogram(&pixels, None).expect("finite pixels");
        assert_eq!(h.n_bins, 10, "100 pixels -> floor(sqrt(100)) = 10 bins");
        assert_eq!(h.counts.len(), 10);
        assert_eq!(h.edges.len(), 11, "edges = n_bins + 1");
    }

    /// Known pixel array -> exact bin counts, including the last-bin-closed
    /// boundary value (the max lands in the last bin, not a new one).
    #[test]
    fn histogram_exact_counts_last_bin_closed() {
        // Values 0..=10, n_bins = 5, range [0, 10], bin width 2.
        // bin = floor(v * 5 / 10) = floor(v / 2) for v < 10; v == 10 -> last bin.
        //   0,1   -> bin 0    (0 <= v < 2)
        //   2,3   -> bin 1
        //   4,5   -> bin 2
        //   6,7   -> bin 3
        //   8,9   -> bin 4
        //   10    -> bin 4 (last_bin_closed)
        let pixels: Vec<f64> = (0..=10).map(|i| i as f64).collect();
        let h = pixel_intensity_histogram(&pixels, Some(5)).expect("finite pixels");
        assert_eq!(h.n_bins, 5);
        assert_eq!(h.counts, vec![2, 2, 2, 2, 3]);
        assert_eq!(h.counts.iter().sum::<u64>(), 11, "every pixel is counted");
        assert_eq!(h.min, 0.0);
        assert_eq!(h.max, 10.0);
        assert_eq!(h.edges[0], 0.0);
        assert_eq!(*h.edges.last().unwrap(), 10.0);
    }

    /// Statistics: mean / std / sum / min / max over a known array.
    #[test]
    fn histogram_statistics_match_known_values() {
        let pixels = [1.0, 2.0, 3.0, 4.0, 5.0];
        let h = pixel_intensity_histogram(&pixels, Some(4)).expect("finite pixels");
        assert_eq!(h.min, 1.0);
        assert_eq!(h.max, 5.0);
        assert_eq!(h.sum, 15.0);
        assert_eq!(h.mean, 3.0);
        // Population std of {1,2,3,4,5}: sqrt(mean of squared deviations)
        //   = sqrt((4+1+0+1+4)/5) = sqrt(2).
        assert!((h.std - 2.0_f64.sqrt()).abs() < 1e-12);
    }

    /// NaN and inf pixels are excluded from range, counts, and statistics.
    #[test]
    fn histogram_excludes_non_finite() {
        // Finite values {0,1,2,3} plus a NaN and a +inf.
        let pixels = [0.0, 1.0, f64::NAN, 2.0, f64::INFINITY, 3.0];
        let h = pixel_intensity_histogram(&pixels, Some(4)).expect("finite pixels");
        // Range from finite only: min 0, max 3.
        assert_eq!(h.min, 0.0);
        assert_eq!(h.max, 3.0);
        // Stats over the 4 finite pixels only.
        assert_eq!(h.sum, 6.0);
        assert_eq!(h.mean, 1.5);
        // Only the 4 finite pixels are counted.
        assert_eq!(h.counts.iter().sum::<u64>(), 4);
        // Default bin formula would use sqrt(4)=2, but we forced 4 bins.
        assert_eq!(h.n_bins, 4);
    }

    /// All-equal pixels: degenerate range is enlarged so the pixels land in a
    /// real bin, and all pixels are counted.
    #[test]
    fn histogram_all_equal_degenerate_range() {
        let pixels = [5.0; 8];
        let h = pixel_intensity_histogram(&pixels, Some(4)).expect("finite pixels");
        // Range reported as the true min/max (both 5.0).
        assert_eq!(h.min, 5.0);
        assert_eq!(h.max, 5.0);
        // Enlarged range (5*0.99, 5*1.01) = (4.95, 5.05) used for binning.
        assert!((h.edges[0] - 4.95).abs() < 1e-12);
        assert!((h.edges.last().unwrap() - 5.05).abs() < 1e-12);
        // Every pixel counted (5.0 is interior to (4.95, 5.05)).
        assert_eq!(h.counts.iter().sum::<u64>(), 8);
    }

    /// All-zero pixels: degenerate range centered at 0 uses the (-0.01, 0.01)
    /// enlargement (silx's special case for value == 0).
    #[test]
    fn histogram_all_zero_degenerate_range() {
        let pixels = [0.0; 4];
        let h = pixel_intensity_histogram(&pixels, Some(2)).expect("finite pixels");
        assert_eq!(h.min, 0.0);
        assert_eq!(h.max, 0.0);
        assert!((h.edges[0] - (-0.01)).abs() < 1e-12);
        assert!((h.edges.last().unwrap() - 0.01).abs() < 1e-12);
        assert_eq!(h.counts.iter().sum::<u64>(), 4);
    }

    /// All-not-finite data returns None (silx HistogramWidget.reset).
    #[test]
    fn histogram_all_non_finite_is_none() {
        let pixels = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY];
        assert!(pixel_intensity_histogram(&pixels, None).is_none());
        assert!(pixel_intensity_histogram(&[], None).is_none());
    }

    /// The bin count is floored at 2 even when sqrt(size) would be smaller.
    #[test]
    fn histogram_bin_count_floored_at_two() {
        // 1 finite pixel -> floor(sqrt(1)) = 1, clamped up to 2.
        let h = pixel_intensity_histogram(&[3.0, f64::NAN], None).expect("one finite pixel");
        assert_eq!(h.n_bins, 2);
    }

    /// A value strictly below the range minimum is discarded (cannot happen for
    /// the finite-derived range, but the guard must hold for an explicit range
    /// produced by the degenerate enlargement skipping low outliers — here we
    /// verify the interior-floor formula on a midrange value directly).
    #[test]
    fn histogram_interior_floor_formula() {
        // range [0, 4], n_bins 4, bin width 1. v = 2.5 -> floor(2.5) = bin 2.
        let pixels = [0.0, 2.5, 4.0];
        let h = pixel_intensity_histogram(&pixels, Some(4)).expect("finite pixels");
        // 0.0 -> bin 0; 2.5 -> bin 2; 4.0 -> last bin (3).
        assert_eq!(h.counts, vec![1, 0, 1, 1]);
    }
}
