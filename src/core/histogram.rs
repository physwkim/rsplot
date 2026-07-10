//! Data-distribution histogram of a scalar image, faithful to silx
//! `ColormapDialog.computeHistogram` (`gui/dialog/ColormapDialog.py:1227-1295`).
//!
//! Shared between the modal [`ColormapDialog`](crate::ColormapDialog) and the
//! inline `HistogramColorBar`; this is the single home for the binning logic so
//! the two never drift.

/// Index of the bin holding `t`, or `None` when `t` is outside the grid.
///
/// The grid is the `nbins` uniform bins over the half-open extent
/// `[xmin, xmax)`, `span == xmax - xmin`. `Histogramnd` rejects a coordinate
/// below `g_min`, and rejects a coordinate at or above `g_max` unless
/// `last_bin_closed` is set (`math/histogramnd/src/histogramnd_template.c:174-222`).
/// `ColormapDialog.computeHistogram` does not set it (`ColormapDialog.py:1288`),
/// so `t == xmax` is dropped rather than folded into the last bin — and a
/// degenerate extent (`span == 0`) admits no sample at all.
///
/// The arithmetic multiplies before dividing, as C does, so the bin boundaries
/// fall on the same samples.
fn bin_index(t: f64, xmin: f64, xmax: f64, span: f64, nbins: usize) -> Option<usize> {
    if !t.is_finite() || t < xmin || t >= xmax {
        return None;
    }
    let idx = ((t - xmin) * nbins as f64 / span) as usize;
    (idx < nbins).then_some(idx)
}

/// Compute the data-distribution histogram of a flattened scalar image.
///
/// `data` is the flattened scalar image. `range` optionally fixes the histogram
/// extent `(min, max)`; when `None` the finite min/max of `data` is used. `log`
/// selects logarithmic binning (silx `scale == LOGARITHM`): the samples and the
/// range are taken to `log10` and binned uniformly in log space, but the
/// returned edges are mapped back to linear (`10**edge`) so they plot on a log
/// x-axis.
///
/// Returns `(counts, edges)` with `counts.len() + 1 == edges.len()`, or `None`
/// when there is no finite data / no valid range (silx returns `(None, None)`).
/// The bin count is `clamp(2, min(256, floor(sqrt(N))))` — silx `nbins` (the
/// integer-data 256-bin special case does not apply to scalar `f64` images).
///
/// The histogram extent is half-open, `[xmin, xmax)` — see [`bin_index`].
pub fn compute_histogram(
    data: &[f64],
    range: Option<(f64, f64)>,
    log: bool,
) -> Option<(Vec<u64>, Vec<f64>)> {
    if data.is_empty() {
        return None;
    }
    // In log mode silx transforms the samples (and the range) to log10 first,
    // bins uniformly in log space, then converts the edges back to linear.
    let xform = |v: f64| if log { v.log10() } else { v };

    // Histogram extent in the (log-)transformed space.
    let (mut xmin, mut xmax) = match range {
        Some((lo, hi)) => (xform(lo), xform(hi)),
        None => {
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for &v in data {
                let t = xform(v);
                if t.is_finite() {
                    lo = lo.min(t);
                    hi = hi.max(t);
                }
            }
            (lo, hi)
        }
    };
    if !xmin.is_finite() || !xmax.is_finite() {
        return None;
    }
    if xmax < xmin {
        std::mem::swap(&mut xmin, &mut xmax);
    }

    // silx: nbins = max(2, min(256, int(sqrt(N)))).
    let nbins = ((data.len() as f64).sqrt().floor() as usize).clamp(2, 256);
    let span = xmax - xmin;
    let mut counts = vec![0u64; nbins];
    for &v in data {
        if let Some(idx) = bin_index(xform(v), xmin, xmax, span, nbins) {
            counts[idx] += 1;
        }
    }

    // Edges in the transformed space, mapped back to linear when log.
    let inv = |e: f64| if log { 10f64.powf(e) } else { e };
    let edges: Vec<f64> = (0..=nbins)
        .map(|i| inv(xmin + span * (i as f64) / nbins as f64))
        .collect();

    Some((counts, edges))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_histogram_bin_count_follows_silx_sqrt_rule() {
        // nbins = clamp(2, min(256, floor(sqrt(N)))).
        // N=100 -> floor(sqrt)=10 bins; edges has nbins+1 entries.
        let data: Vec<f64> = (0..100).map(|i| i as f64).collect();
        let (counts, edges) = compute_histogram(&data, None, false).expect("histogram");
        assert_eq!(counts.len(), 10);
        assert_eq!(edges.len(), 11);
        // Every sample below the upper edge is binned exactly once; 99.0 sits
        // *on* the open upper edge and is dropped.
        assert_eq!(counts.iter().sum::<u64>(), 99);
        // Extent spans the finite data min/max.
        assert_eq!(edges[0], 0.0);
        assert_eq!(*edges.last().unwrap(), 99.0);

        // Cap at 256 bins for large N (floor(sqrt(1_000_000)) = 1000 -> 256).
        let big = vec![0.5; 1_000_000];
        let (c, _) = compute_histogram(&big, Some((0.0, 1.0)), false).expect("histogram");
        assert_eq!(c.len(), 256);

        // Floor below 2 is lifted to 2 (N=1 -> floor(sqrt)=1 -> 2).
        let (c1, e1) = compute_histogram(&[3.0], Some((0.0, 6.0)), false).expect("histogram");
        assert_eq!(c1.len(), 2);
        assert_eq!(e1.len(), 3);
    }

    /// One case per boundary of `bin_index`, not one per user story.
    /// `nbins = 2` over `[0, 5]` gives bins `[0, 2.5)` and `[2.5, 5)`.
    #[test]
    fn bin_index_boundaries_match_histogramnd() {
        let bin = |t: f64| bin_index(t, 0.0, 5.0, 5.0, 2);

        assert_eq!(bin(-0.001), None, "below the lower edge");
        assert_eq!(bin(0.0), Some(0), "on the lower edge: closed");
        assert_eq!(bin(2.4999), Some(0), "just below the interior edge");
        assert_eq!(
            bin(2.5),
            Some(1),
            "on the interior edge: belongs to the upper bin"
        );
        assert_eq!(bin(4.9999), Some(1), "just below the upper edge");
        assert_eq!(bin(5.0), None, "on the upper edge: open, so dropped");
        assert_eq!(bin(5.001), None, "above the upper edge");
        assert_eq!(bin(f64::NAN), None, "non-finite");
        assert_eq!(bin(f64::INFINITY), None, "non-finite");

        // Degenerate extent: `t >= xmax` for every `t`, so nothing is admitted
        // and `span == 0` is never divided by.
        assert_eq!(bin_index(7.0, 7.0, 7.0, 0.0, 2), None);
    }

    #[test]
    fn compute_histogram_uses_supplied_range_and_drops_the_open_upper_edge() {
        // With an explicit range, out-of-range samples are dropped, and so are
        // samples sitting exactly on the (open) upper edge.
        let data = vec![-5.0, 0.0, 2.5, 5.0, 5.0, 10.0];
        let (counts, edges) = compute_histogram(&data, Some((0.0, 5.0)), false).expect("histogram");
        assert_eq!(edges[0], 0.0);
        assert_eq!(*edges.last().unwrap(), 5.0);
        // Admitted: 0.0, 2.5. Dropped: -5.0 (below), 5.0 twice (open upper
        // edge), 10.0 (above).
        assert_eq!(counts.iter().sum::<u64>(), 2);
    }

    #[test]
    fn compute_histogram_log_bins_uniformly_in_log_space() {
        // Decade-spaced data over [1, 1000]: log10 -> [0, 3] uniform bins, edges
        // mapped back to linear (10**edge).
        let data = vec![1.0, 10.0, 100.0, 1000.0];
        let (counts, edges) = compute_histogram(&data, None, true).expect("histogram");
        // floor(sqrt(4)) = 2 bins -> edges 10^0, 10^1.5, 10^3.
        assert_eq!(counts.len(), 2);
        assert!((edges[0] - 1.0).abs() < 1e-9, "{}", edges[0]);
        assert!((edges[1] - 10f64.powf(1.5)).abs() < 1e-6, "{}", edges[1]);
        assert!((edges[2] - 1000.0).abs() < 1e-6, "{}", edges[2]);
        // 1000.0 transforms to the open upper edge (log10 = 3) and is dropped.
        assert_eq!(counts.iter().sum::<u64>(), 3);
    }

    #[test]
    fn compute_histogram_empty_or_nonfinite_is_none() {
        assert!(compute_histogram(&[], None, false).is_none());
        assert!(compute_histogram(&[f64::NAN, f64::INFINITY], None, false).is_none());
    }

    #[test]
    fn compute_histogram_degenerate_range_admits_no_sample() {
        // All-equal data: the extent collapses, so every sample sits on the
        // open upper edge. `ColormapDialog.computeHistogram` has no guard for
        // this (`ColormapDialog.py:1264-1288`) and `Histogramnd` drops them
        // all, leaving an empty histogram over a zero-width extent.
        let data = vec![7.0; 5];
        let (counts, edges) = compute_histogram(&data, None, false).expect("histogram");
        assert_eq!(counts.iter().sum::<u64>(), 0);
        assert_eq!(edges[0], 7.0);
        assert_eq!(*edges.last().unwrap(), 7.0);
    }
}
