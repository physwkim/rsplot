//! Per-pixel-column min/max decimation for large polylines.
//!
//! When a curve has far more points than the data area is wide in pixels, every
//! pixel column holds many vertices whose only visible effect is the vertical
//! extent (lowest to highest y) they cover. [`min_max_decimate`] reduces the
//! polyline to, per column, the lowest-y and highest-y point in ascending-x
//! order, plus one boundary anchor on each side of the window so segments that
//! cross the viewport edge still draw. The result has at most `2 * columns + 2`
//! points yet preserves the inked envelope, so for `n ≫ columns` it looks
//! identical while drawing far fewer vertices (silx min/max decimation,
//! `doc/design.md` §13 D1).

/// Emit a column's representative point(s) into `out`, lowest-x first.
fn emit(out_x: &mut Vec<f64>, out_y: &mut Vec<f64>, x: &[f64], y: &[f64], lo: usize, hi: usize) {
    if lo == hi {
        out_x.push(x[lo]);
        out_y.push(y[lo]);
    } else if x[lo] <= x[hi] {
        out_x.push(x[lo]);
        out_y.push(y[lo]);
        out_x.push(x[hi]);
        out_y.push(y[hi]);
    } else {
        out_x.push(x[hi]);
        out_y.push(y[hi]);
        out_x.push(x[lo]);
        out_y.push(y[lo]);
    }
}

/// Down-sample a polyline to a per-pixel-column min/max envelope over the
/// visible window `[x_min, x_max]`, split into `columns` equal bins.
///
/// For each bin that contains points, emits the lowest-y and highest-y point in
/// ascending-x order. The last point left of the window and the first point
/// right of it are kept as anchors so boundary-crossing segments still draw.
/// Points are otherwise dropped if they fall outside the window.
///
/// Assumes `x` is monotonically non-decreasing (time-series / waveform) and `y`
/// is finite; the caller must not decimate unsorted x, since the envelope would
/// reorder points. `x` and `y` must have equal length. Returns `(out_x, out_y)`.
///
/// No-op (returns the inputs cloned) when `columns == 0`, the window is
/// degenerate (`x_max <= x_min`), or there are no points.
pub fn min_max_decimate(
    x: &[f64],
    y: &[f64],
    x_min: f64,
    x_max: f64,
    columns: u32,
) -> (Vec<f64>, Vec<f64>) {
    assert_eq!(x.len(), y.len(), "x and y must have equal length");
    let n = x.len();
    // Positive form so a NaN bound makes `valid_window` false (and we no-op),
    // and so we negate a bool rather than a partial-ord comparison.
    let valid_window = x_max > x_min;
    if columns == 0 || n == 0 || !valid_window {
        return (x.to_vec(), y.to_vec());
    }

    let width = (x_max - x_min) / columns as f64;
    let last_col = columns as i64 - 1;

    let mut out_x: Vec<f64> = Vec::new();
    let mut out_y: Vec<f64> = Vec::new();
    // Last index strictly left of the window (anchor for the entry segment).
    let mut before: Option<usize> = None;
    // First index strictly right of the window (anchor for the exit segment).
    let mut after: Option<usize> = None;

    // Streaming min/max per column; valid only once `cur_col >= 0`.
    let mut cur_col: i64 = -1;
    let mut lo = 0usize;
    let mut hi = 0usize;

    for i in 0..n {
        let xi = x[i];
        if xi < x_min {
            before = Some(i);
            continue;
        }
        if xi > x_max {
            after = Some(i);
            // x is sorted, so every later point is also outside the window.
            break;
        }
        let col = (((xi - x_min) / width).floor() as i64).clamp(0, last_col);
        if col != cur_col {
            if cur_col >= 0 {
                emit(&mut out_x, &mut out_y, x, y, lo, hi);
            }
            cur_col = col;
            lo = i;
            hi = i;
        } else {
            if y[i] < y[lo] {
                lo = i;
            }
            if y[i] > y[hi] {
                hi = i;
            }
        }
    }
    if cur_col >= 0 {
        emit(&mut out_x, &mut out_y, x, y, lo, hi);
    }

    // Prepend / append the boundary anchors so the polyline still reaches the
    // viewport edges. Both preserve ascending-x order.
    if let Some(b) = before {
        out_x.insert(0, x[b]);
        out_y.insert(0, y[b]);
    }
    if let Some(a) = after {
        out_x.push(x[a]);
        out_y.push(y[a]);
    }

    (out_x, out_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_on_degenerate_inputs() {
        let x = vec![0.0, 1.0, 2.0];
        let y = vec![0.0, 1.0, 0.0];
        // Zero columns, collapsed window, and empty input all pass through.
        assert_eq!(
            min_max_decimate(&x, &y, 0.0, 2.0, 0),
            (x.clone(), y.clone())
        );
        assert_eq!(
            min_max_decimate(&x, &y, 2.0, 2.0, 4),
            (x.clone(), y.clone())
        );
        assert_eq!(min_max_decimate(&[], &[], 0.0, 1.0, 4), (vec![], vec![]));
    }

    #[test]
    fn reduces_count_and_keeps_envelope() {
        // 1000 points of a sine packed into 10 columns over x∈[0,1].
        let n = 1000;
        let x: Vec<f64> = (0..n).map(|i| i as f64 / (n - 1) as f64).collect();
        let y: Vec<f64> = x
            .iter()
            .map(|&t| (t * std::f64::consts::TAU * 3.0).sin())
            .collect();
        let (dx, dy) = min_max_decimate(&x, &y, 0.0, 1.0, 10);

        // At most 2 points per column (no boundary anchors at the exact extent).
        assert!(dx.len() <= 20, "decimated to {} points", dx.len());
        assert!(dx.len() < n, "must reduce vertex count");
        assert_eq!(dx.len(), dy.len());

        // Output x is ascending.
        assert!(dx.windows(2).all(|w| w[0] <= w[1]), "x must stay sorted");

        // The global min and max y of the source survive (envelope preserved).
        let src_min = y.iter().cloned().fold(f64::INFINITY, f64::min);
        let src_max = y.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let out_min = dy.iter().cloned().fold(f64::INFINITY, f64::min);
        let out_max = dy.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!((out_min - src_min).abs() < 1e-9, "{out_min} vs {src_min}");
        assert!((out_max - src_max).abs() < 1e-9, "{out_max} vs {src_max}");
    }

    #[test]
    fn single_point_per_column_emitted_once() {
        // One point per column: min == max, so each emits exactly once (lossless).
        let x = vec![0.05, 0.15, 0.25, 0.35];
        let y = vec![1.0, 2.0, 3.0, 4.0];
        let (dx, dy) = min_max_decimate(&x, &y, 0.0, 0.4, 4);
        assert_eq!(dx, x);
        assert_eq!(dy, y);
    }

    #[test]
    fn keeps_boundary_anchors_outside_window() {
        // Points span [0,10]; window is [3,7]. The last point left of 3 and the
        // first right of 7 are kept so the line reaches both edges.
        let x: Vec<f64> = (0..=10).map(|i| i as f64).collect();
        let y = x.clone();
        let (dx, _) = min_max_decimate(&x, &y, 3.0, 7.0, 4);
        // Anchor before (x=2) and after (x=8) bracket the in-window points.
        assert_eq!(*dx.first().unwrap(), 2.0, "before anchor");
        assert_eq!(*dx.last().unwrap(), 8.0, "after anchor");
        assert!(dx.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn emits_extremes_in_x_order() {
        // One column [0,1] with a max then a min by x: output keeps x order.
        let x = vec![0.1, 0.2, 0.3, 0.4];
        let y = vec![0.0, 5.0, -5.0, 0.0]; // max at x=0.2, min at x=0.3
        let (dx, dy) = min_max_decimate(&x, &y, 0.0, 1.0, 1);
        assert_eq!(dx, vec![0.2, 0.3]);
        assert_eq!(dy, vec![5.0, -5.0]);
    }
}
