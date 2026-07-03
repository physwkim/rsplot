//! Pure statistics engine ported from silx `gui/plot/stats/stats.py`.
//!
//! This module provides GPU-free, pure functions computing the full silx
//! statistic set over 1D curve data `(xs, ys)` and 2D scalar image data:
//!
//! - `min`, `max`, `delta` (`max - min`) — silx `StatMin` / `StatMax` /
//!   `StatDelta` (stats.py:783-813)
//! - `mean`, `sum` (integral), `std` — silx `("mean", numpy.mean)` /
//!   `("std", numpy.std)` (StatsWidget.py:1266-1276) / sum aggregation
//! - center of mass `COM = sum(pos * val) / sum(val)` — silx `StatCOM`
//!   (stats.py:881-910)
//! - coordinates of the first min / max via `argmin` / `argmax` mapped back
//!   to `x` (curve) or `(row, col)` (image) — silx `StatCoordMin` /
//!   `StatCoordMax` (stats.py:816-878)
//!
//! Masking matches silx's `clipData` (stats.py:216-300): an optional
//! [`StatScope`] restricts the data to the visible viewport
//! ([`StatScope::OnLimits`]) before computing, and [`Stats::for_curve_roi`]
//! restricts a curve to an x-range (the silx 1D `ROI` mask, stats.py:322).
//!
//! Non-finite values (`NaN`, `±inf`) are filtered out before any aggregation,
//! matching silx's reliance on finite data for min/max/com.

/// Which subset of the data to include before computing statistics.
///
/// Mirrors silx `clipData` (stats.py:216-300): with [`StatScope::All`] every
/// data point participates; with [`StatScope::OnLimits`] only points inside
/// the visible viewport rectangle participate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StatScope {
    /// Use every data point (silx: `onlimits=False`, `roi=None`).
    All,
    /// Restrict to the viewport rectangle `x in [x0, x1]`, `y in [y0, y1]`.
    ///
    /// For curves only the x-range gates inclusion (silx `_CurveContext`
    /// masks on `xData` alone, stats.py:331). For images the rectangle is
    /// intersected against the pixel grid (silx `_ImageContext`,
    /// stats.py:546-569). Bounds are inclusive on both ends, matching silx's
    /// `(minX <= xData) & (xData <= maxX)` (stats.py:331).
    OnLimits {
        /// Inclusive x range `(min, max)`.
        x_range: (f64, f64),
        /// Inclusive y range `(min, max)`.
        y_range: (f64, f64),
    },
}

/// Result of the full silx statistic set for one item.
///
/// Every field is `Option<f64>` (or a coordinate tuple): `None` means the
/// statistic is undefined for the input, e.g. empty data, all-non-finite
/// data, or (for [`Self::com`]) data whose finite values sum to zero — the
/// silx `StatCOM` returns `NaN` in that case (stats.py:894-895), which we
/// surface as `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    /// Number of input values (before finite filtering).
    pub count: usize,
    /// Number of finite values that participated in the aggregation.
    pub finite_count: usize,
    /// Minimum finite value (silx `StatMin`, stats.py:783).
    pub min: Option<f64>,
    /// Maximum finite value (silx `StatMax`, stats.py:794).
    pub max: Option<f64>,
    /// `max - min` (silx `StatDelta`, stats.py:805).
    pub delta: Option<f64>,
    /// Arithmetic mean of finite values (silx `("mean", numpy.mean)`).
    pub mean: Option<f64>,
    /// Population standard deviation (ddof = 0) of finite values — silx
    /// `("std", numpy.std)` in `DEFAULT_STATS` (StatsWidget.py:1266-1276).
    pub std: Option<f64>,
    /// Sum / integral of finite values.
    pub sum: Option<f64>,
    /// Center of mass (silx `StatCOM`, stats.py:881). For a curve this is a
    /// single x coordinate stored in `com[0]` with `com[1] == None`; for an
    /// image it is `(x, y)` in data coords stored as `com[0] = x`,
    /// `com[1] = y`.
    pub com: ComCoord,
    /// Data coordinates of the first minimum value (silx `StatCoordMin`,
    /// stats.py:841). Curve: `(x, None)`. Image: `(x, y)`.
    pub coord_min: ComCoord,
    /// Data coordinates of the first maximum value (silx `StatCoordMax`,
    /// stats.py:860). Curve: `(x, None)`. Image: `(x, y)`.
    pub coord_max: ComCoord,
}

/// A coordinate produced by COM / argmin / argmax.
///
/// `x` is always present when defined; `y` is `Some` only for 2D (image)
/// data. Both `None` means the statistic was undefined.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ComCoord {
    /// X (or sole curve) coordinate.
    pub x: Option<f64>,
    /// Y coordinate, present only for 2D image data.
    pub y: Option<f64>,
}

impl ComCoord {
    /// An undefined coordinate (`x == None`, `y == None`).
    pub const NONE: ComCoord = ComCoord { x: None, y: None };

    fn x_only(x: f64) -> Self {
        ComCoord {
            x: Some(x),
            y: None,
        }
    }

    fn xy(x: f64, y: f64) -> Self {
        ComCoord {
            x: Some(x),
            y: Some(y),
        }
    }
}

impl Stats {
    /// Compute the full statistic set for a curve `(xs, ys)`, scoping the data
    /// per `scope`.
    ///
    /// Mirrors silx `_CurveContext.clipData` (stats.py:309-342): the statistic
    /// values are the curve's `y` values, the position axis is `x`. With
    /// [`StatScope::OnLimits`] a point is included when its **x** lies inside
    /// the viewport x-range (silx masks on `xData` only, stats.py:331); the
    /// y-range is ignored for curves to match silx.
    ///
    /// Pairs `(x, y)` where either coordinate is non-finite are dropped, since
    /// silx's downstream `min_max` / `argmin` work on finite data.
    ///
    /// `xs` and `ys` must have equal length; the shorter length is used if
    /// they differ (matching numpy's element-wise pairing being undefined,
    /// we simply zip).
    pub fn for_curve(xs: &[f64], ys: &[f64], scope: StatScope) -> Self {
        Self::curve_inner(xs, ys, scope, None)
    }

    /// Compute curve statistics restricted to an x-range ROI `[from, to]`.
    ///
    /// Mirrors silx `_CurveContext` ROI masking (stats.py:322-332): a point is
    /// included when `from <= x <= to`. ROI is incompatible with on-limits in
    /// silx (stats.py:262-266); here the ROI range is applied as the sole
    /// mask, equivalent to calling with [`StatScope::All`] plus an x clamp.
    pub fn for_curve_roi(xs: &[f64], ys: &[f64], from: f64, to: f64) -> Self {
        Self::curve_inner(xs, ys, StatScope::All, Some((from, to)))
    }

    fn curve_inner(xs: &[f64], ys: &[f64], scope: StatScope, roi: Option<(f64, f64)>) -> Self {
        let count = xs.len().min(ys.len());
        let mut acc = Accumulator::default();
        for i in 0..count {
            let x = xs[i];
            let y = ys[i];
            if !x.is_finite() || !y.is_finite() {
                continue;
            }
            // ROI mask (1D x-range), silx stats.py:331.
            if let Some((from, to)) = roi {
                let (lo, hi) = order(from, to);
                if x < lo || x > hi {
                    continue;
                }
            }
            // On-limits mask: curve gates on x only (silx stats.py:331).
            if let StatScope::OnLimits { x_range, .. } = scope {
                let (lo, hi) = order(x_range.0, x_range.1);
                if x < lo || x > hi {
                    continue;
                }
            }
            acc.push(y, x, f64::NAN);
        }
        acc.finish(count, /* is_image */ false)
    }

    /// Compute the full statistic set for a 2D scalar image in row-major
    /// order (`data[row * width + col]`), with pixel `(col, row)` mapped to
    /// data coords by `origin + scale * index`.
    ///
    /// Mirrors silx `_ImageContext.clipData` (stats.py:533-591): the x axis is
    /// `origin.0 + scale.0 * col`, the y axis is `origin.1 + scale.1 * row`.
    /// With [`StatScope::OnLimits`] the viewport rectangle is converted to
    /// pixel index bounds (`int((v - origin) / scale)`), clipped to the array
    /// extent, and pixels outside the rectangle are masked (stats.py:554-569).
    ///
    /// COM and coords are reported in **data coordinates** (silx maps the flat
    /// index back through the axes, stats.py:819-838 / 897-906).
    ///
    /// `data.len()` must equal `width * height`; extra trailing elements are
    /// ignored and a short slice is treated as missing pixels (skipped).
    pub fn for_image(
        data: &[f64],
        width: usize,
        height: usize,
        origin: (f64, f64),
        scale: (f64, f64),
        scope: StatScope,
    ) -> Self {
        let count = width.saturating_mul(height);
        if width == 0 || height == 0 {
            return Stats {
                count,
                ..Stats::default()
            };
        }

        // Pixel index window [xmin, xmax] x [ymin, ymax], inclusive.
        let (xmin, xmax, ymin, ymax) = match scope {
            StatScope::All => (0usize, width - 1, 0usize, height - 1),
            StatScope::OnLimits { x_range, y_range } => {
                if scale.0 == 0.0 || scale.1 == 0.0 {
                    return Stats {
                        count,
                        ..Stats::default()
                    };
                }
                let (lx, hx) = order(x_range.0, x_range.1);
                let (ly, hy) = order(y_range.0, y_range.1);
                // silx: index = int((value - origin) / scale) (stats.py:554-557).
                // A negative scale flips the order, so re-order the indices.
                let to_ix = |v: f64| ((v - origin.0) / scale.0) as i64;
                let to_iy = |v: f64| ((v - origin.1) / scale.1) as i64;
                let mut ix0 = to_ix(lx);
                let mut ix1 = to_ix(hx);
                let mut iy0 = to_iy(ly);
                let mut iy1 = to_iy(hy);
                if ix0 > ix1 {
                    std::mem::swap(&mut ix0, &mut ix1);
                }
                if iy0 > iy1 {
                    std::mem::swap(&mut iy0, &mut iy1);
                }
                // silx clips to [0, size-1] (stats.py:559-560).
                let cx0 = ix0.clamp(0, width as i64 - 1);
                let cx1 = ix1.clamp(0, width as i64 - 1);
                let cy0 = iy0.clamp(0, height as i64 - 1);
                let cy1 = iy1.clamp(0, height as i64 - 1);
                // silx collapses to empty when xmax <= xmin or ymax <= ymin
                // *after* clipping (stats.py:562-566): a single-column or
                // single-row selection counts as empty.
                if cx1 <= cx0 || cy1 <= cy0 {
                    return Stats {
                        count,
                        ..Stats::default()
                    };
                }
                (cx0 as usize, cx1 as usize, cy0 as usize, cy1 as usize)
            }
        };

        let mut acc = Accumulator::default();
        for row in ymin..=ymax {
            for col in xmin..=xmax {
                let idx = row * width + col;
                if idx >= data.len() {
                    continue;
                }
                let v = data[idx];
                if !v.is_finite() {
                    continue;
                }
                let x = origin.0 + scale.0 * col as f64;
                let y = origin.1 + scale.1 * row as f64;
                acc.push(v, x, y);
            }
        }
        acc.finish(count, /* is_image */ true)
    }
}

/// Single-pass accumulator shared by curve and image paths.
///
/// Tracks finite count, running min/max (with the first-occurrence position
/// for argmin/argmax to match silx `argmin`/`argmax` returning the *first*
/// extremum), running sums for mean and COM.
#[derive(Default)]
struct Accumulator {
    finite_count: usize,
    sum: f64,
    // Welford running mean / squared-deviation sum for the population std
    // (numpy.std, ddof = 0 — silx `("std", numpy.std)`).
    welford_mean: f64,
    welford_m2: f64,
    // COM numerators: sum(val * pos).
    com_x_num: f64,
    com_y_num: f64,
    min: f64,
    max: f64,
    min_pos: (f64, f64),
    max_pos: (f64, f64),
    first: bool,
}

impl Accumulator {
    fn push(&mut self, value: f64, x: f64, y: f64) {
        self.finite_count += 1;
        self.sum += value;
        // Welford update: numerically stable running mean + M2.
        let delta = value - self.welford_mean;
        self.welford_mean += delta / self.finite_count as f64;
        self.welford_m2 += delta * (value - self.welford_mean);
        self.com_x_num += value * x;
        if y.is_finite() {
            self.com_y_num += value * y;
        }
        if !self.first {
            self.first = true;
            self.min = value;
            self.max = value;
            self.min_pos = (x, y);
            self.max_pos = (x, y);
        } else {
            // Strictly-less / strictly-greater keeps the *first* extremum,
            // matching numpy argmin/argmax (silx stats.py:852, 873).
            if value < self.min {
                self.min = value;
                self.min_pos = (x, y);
            }
            if value > self.max {
                self.max = value;
                self.max_pos = (x, y);
            }
        }
    }

    fn finish(self, count: usize, is_image: bool) -> Stats {
        if self.finite_count == 0 {
            return Stats {
                count,
                finite_count: 0,
                ..Stats::default()
            };
        }
        let mean = self.sum / self.finite_count as f64;
        let coord = |pos: (f64, f64)| {
            if is_image {
                ComCoord::xy(pos.0, pos.1)
            } else {
                ComCoord::x_only(pos.0)
            }
        };
        // COM: undefined (silx NaN, stats.py:894) when sum == 0.
        let com = if self.sum == 0.0 {
            ComCoord::NONE
        } else if is_image {
            ComCoord::xy(self.com_x_num / self.sum, self.com_y_num / self.sum)
        } else {
            ComCoord::x_only(self.com_x_num / self.sum)
        };
        Stats {
            count,
            finite_count: self.finite_count,
            min: Some(self.min),
            max: Some(self.max),
            delta: Some(self.max - self.min),
            mean: Some(mean),
            // Population std (ddof = 0), matching numpy.std as used by silx
            // `DEFAULT_STATS` (StatsWidget.py:1275).
            std: Some((self.welford_m2 / self.finite_count as f64).sqrt()),
            sum: Some(self.sum),
            com,
            coord_min: coord(self.min_pos),
            coord_max: coord(self.max_pos),
        }
    }
}

/// Order a pair so the first element is the smaller. Handles negative scales
/// / reversed limits, matching silx's reliance on `min_max` semantics.
fn order(a: f64, b: f64) -> (f64, f64) {
    if a <= b { (a, b) } else { (b, a) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {b}, got {a}");
    }

    #[test]
    fn curve_empty_yields_none() {
        let s = Stats::for_curve(&[], &[], StatScope::All);
        assert_eq!(s.count, 0);
        assert_eq!(s.finite_count, 0);
        assert_eq!(s.min, None);
        assert_eq!(s.max, None);
        assert_eq!(s.delta, None);
        assert_eq!(s.mean, None);
        assert_eq!(s.std, None);
        assert_eq!(s.sum, None);
        assert_eq!(s.com, ComCoord::NONE);
        assert_eq!(s.coord_min, ComCoord::NONE);
        assert_eq!(s.coord_max, ComCoord::NONE);
    }

    #[test]
    fn curve_std_is_population_std() {
        // numpy.std (ddof = 0) of [2, 4, 4, 4, 5, 5, 7, 9] is exactly 2
        // (mean 5, squared deviations sum 32, 32 / 8 = 4, sqrt = 2).
        let ys = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        let xs: Vec<f64> = (0..ys.len()).map(|i| i as f64).collect();
        let s = Stats::for_curve(&xs, &ys, StatScope::All);
        approx(s.std.unwrap(), 2.0);
    }

    #[test]
    fn curve_std_single_point_is_zero() {
        let s = Stats::for_curve(&[1.0], &[5.0], StatScope::All);
        approx(s.std.unwrap(), 0.0);
    }

    #[test]
    fn image_std_is_population_std() {
        // [1, 2, 3, 4]: mean 2.5, variance (2.25+0.25+0.25+2.25)/4 = 1.25.
        let data = [1.0, 2.0, 3.0, 4.0];
        let s = Stats::for_image(&data, 2, 2, (0.0, 0.0), (1.0, 1.0), StatScope::All);
        approx(s.std.unwrap(), 1.25f64.sqrt());
    }

    #[test]
    fn curve_single_point() {
        let s = Stats::for_curve(&[2.0], &[5.0], StatScope::All);
        assert_eq!(s.count, 1);
        assert_eq!(s.finite_count, 1);
        approx(s.min.unwrap(), 5.0);
        approx(s.max.unwrap(), 5.0);
        approx(s.delta.unwrap(), 0.0);
        approx(s.mean.unwrap(), 5.0);
        approx(s.sum.unwrap(), 5.0);
        // COM x = sum(x*y)/sum(y) = (2*5)/5 = 2.
        approx(s.com.x.unwrap(), 2.0);
        assert_eq!(s.com.y, None);
        approx(s.coord_min.x.unwrap(), 2.0);
        approx(s.coord_max.x.unwrap(), 2.0);
    }

    #[test]
    fn curve_all_nan_yields_none() {
        let s = Stats::for_curve(&[1.0, 2.0], &[f64::NAN, f64::INFINITY], StatScope::All);
        assert_eq!(s.count, 2);
        assert_eq!(s.finite_count, 0);
        assert_eq!(s.min, None);
        assert_eq!(s.com, ComCoord::NONE);
    }

    #[test]
    fn curve_drops_non_finite_x() {
        // x non-finite -> pair dropped even though y is finite.
        let s = Stats::for_curve(&[f64::NAN, 3.0], &[10.0, 4.0], StatScope::All);
        assert_eq!(s.finite_count, 1);
        approx(s.sum.unwrap(), 4.0);
        approx(s.coord_max.x.unwrap(), 3.0);
    }

    #[test]
    fn curve_com_symmetric_lands_at_center() {
        // Symmetric weights about x=2 -> COM x = 2.
        let xs = [0.0, 1.0, 2.0, 3.0, 4.0];
        let ys = [1.0, 2.0, 3.0, 2.0, 1.0];
        let s = Stats::for_curve(&xs, &ys, StatScope::All);
        approx(s.com.x.unwrap(), 2.0);
    }

    #[test]
    fn curve_com_all_zero_is_none() {
        // sum(y) == 0 -> silx returns NaN -> we surface None.
        let s = Stats::for_curve(&[0.0, 1.0, 2.0], &[0.0, 0.0, 0.0], StatScope::All);
        assert_eq!(s.finite_count, 3);
        approx(s.sum.unwrap(), 0.0);
        assert_eq!(s.com, ComCoord::NONE);
    }

    #[test]
    fn curve_argmax_argmin_coordinates() {
        let xs = [10.0, 11.0, 12.0, 13.0];
        let ys = [3.0, 9.0, -1.0, 5.0];
        let s = Stats::for_curve(&xs, &ys, StatScope::All);
        approx(s.coord_max.x.unwrap(), 11.0); // y=9 at x=11
        approx(s.coord_min.x.unwrap(), 12.0); // y=-1 at x=12
    }

    #[test]
    fn curve_argmax_first_occurrence_on_tie() {
        // Two equal maxima: first wins (numpy argmax semantics).
        let xs = [0.0, 1.0, 2.0];
        let ys = [5.0, 5.0, 1.0];
        let s = Stats::for_curve(&xs, &ys, StatScope::All);
        approx(s.coord_max.x.unwrap(), 0.0);
    }

    #[test]
    fn curve_on_limits_excludes_out_of_range() {
        let xs = [0.0, 1.0, 2.0, 3.0, 4.0];
        let ys = [10.0, 20.0, 30.0, 40.0, 50.0];
        // Keep only x in [1, 3] -> y in {20, 30, 40}.
        let s = Stats::for_curve(
            &xs,
            &ys,
            StatScope::OnLimits {
                x_range: (1.0, 3.0),
                y_range: (-1e9, 1e9),
            },
        );
        assert_eq!(s.finite_count, 3);
        approx(s.min.unwrap(), 20.0);
        approx(s.max.unwrap(), 40.0);
        approx(s.sum.unwrap(), 90.0);
    }

    #[test]
    fn curve_on_limits_ignores_y_range() {
        // silx curve mask gates on x only; a tight y-range must NOT exclude.
        let xs = [0.0, 1.0, 2.0];
        let ys = [100.0, 200.0, 300.0];
        let s = Stats::for_curve(
            &xs,
            &ys,
            StatScope::OnLimits {
                x_range: (0.0, 2.0),
                y_range: (0.0, 1.0), // would exclude all if applied
            },
        );
        assert_eq!(s.finite_count, 3);
        approx(s.sum.unwrap(), 600.0);
    }

    #[test]
    fn curve_roi_x_range_filters() {
        let xs = [0.0, 1.0, 2.0, 3.0];
        let ys = [1.0, 2.0, 3.0, 4.0];
        let s = Stats::for_curve_roi(&xs, &ys, 1.0, 2.0);
        assert_eq!(s.finite_count, 2);
        approx(s.sum.unwrap(), 5.0);
        approx(s.min.unwrap(), 2.0);
        approx(s.max.unwrap(), 3.0);
    }

    #[test]
    fn curve_roi_reversed_bounds_ordered() {
        let xs = [0.0, 1.0, 2.0, 3.0];
        let ys = [1.0, 2.0, 3.0, 4.0];
        // from > to: should still filter to [1,2].
        let s = Stats::for_curve_roi(&xs, &ys, 2.0, 1.0);
        assert_eq!(s.finite_count, 2);
        approx(s.sum.unwrap(), 5.0);
    }

    #[test]
    fn image_empty_dims_yield_none() {
        let s = Stats::for_image(&[], 0, 0, (0.0, 0.0), (1.0, 1.0), StatScope::All);
        assert_eq!(s.count, 0);
        assert_eq!(s.min, None);
        assert_eq!(s.com, ComCoord::NONE);
    }

    #[test]
    fn image_single_pixel() {
        let s = Stats::for_image(&[7.0], 1, 1, (5.0, 6.0), (1.0, 1.0), StatScope::All);
        assert_eq!(s.finite_count, 1);
        approx(s.min.unwrap(), 7.0);
        approx(s.max.unwrap(), 7.0);
        // COM data coords = origin (col=0,row=0).
        approx(s.com.x.unwrap(), 5.0);
        approx(s.com.y.unwrap(), 6.0);
        approx(s.coord_max.x.unwrap(), 5.0);
        approx(s.coord_max.y.unwrap(), 6.0);
    }

    #[test]
    fn image_argmax_coordinate_correct() {
        // 2x2 image, max=9 at (row=1, col=0). data row-major.
        // [ [1, 2],
        //   [9, 3] ]
        let data = [1.0, 2.0, 9.0, 3.0];
        let s = Stats::for_image(&data, 2, 2, (0.0, 0.0), (1.0, 1.0), StatScope::All);
        approx(s.max.unwrap(), 9.0);
        // col=0 -> x=0; row=1 -> y=1.
        approx(s.coord_max.x.unwrap(), 0.0);
        approx(s.coord_max.y.unwrap(), 1.0);
        approx(s.min.unwrap(), 1.0);
        approx(s.coord_min.x.unwrap(), 0.0);
        approx(s.coord_min.y.unwrap(), 0.0);
    }

    #[test]
    fn image_argmax_with_scale_and_origin() {
        // 2x2 with origin (10,20), scale (2,3). max at col=1,row=1.
        let data = [1.0, 2.0, 3.0, 9.0];
        let s = Stats::for_image(&data, 2, 2, (10.0, 20.0), (2.0, 3.0), StatScope::All);
        approx(s.coord_max.x.unwrap(), 10.0 + 2.0 * 1.0); // 12
        approx(s.coord_max.y.unwrap(), 20.0 + 3.0 * 1.0); // 23
    }

    #[test]
    fn image_com_symmetric_lands_at_center() {
        // 3x3 uniform image -> COM at the geometric center pixel (1,1).
        let data = vec![1.0; 9];
        let s = Stats::for_image(&data, 3, 3, (0.0, 0.0), (1.0, 1.0), StatScope::All);
        approx(s.com.x.unwrap(), 1.0);
        approx(s.com.y.unwrap(), 1.0);
    }

    #[test]
    fn image_com_all_zero_is_none() {
        let data = vec![0.0; 4];
        let s = Stats::for_image(&data, 2, 2, (0.0, 0.0), (1.0, 1.0), StatScope::All);
        assert_eq!(s.finite_count, 4);
        assert_eq!(s.com, ComCoord::NONE);
    }

    #[test]
    fn image_skips_non_finite() {
        let data = [1.0, f64::NAN, 3.0, f64::INFINITY];
        let s = Stats::for_image(&data, 2, 2, (0.0, 0.0), (1.0, 1.0), StatScope::All);
        assert_eq!(s.finite_count, 2);
        approx(s.sum.unwrap(), 4.0);
        approx(s.max.unwrap(), 3.0);
    }

    #[test]
    fn image_on_limits_clips_to_window() {
        // 4x4 ascending. Keep data-coord window x in [1,2], y in [1,2]
        // -> pixels col 1..2, row 1..2 (origin 0, scale 1).
        let mut data = vec![0.0; 16];
        for (i, v) in data.iter_mut().enumerate() {
            *v = i as f64;
        }
        // rows: 0:[0..3], 1:[4..7], 2:[8..11], 3:[12..15]
        let s = Stats::for_image(
            &data,
            4,
            4,
            (0.0, 0.0),
            (1.0, 1.0),
            StatScope::OnLimits {
                x_range: (1.0, 2.0),
                y_range: (1.0, 2.0),
            },
        );
        // Included pixels: (1,1)=5,(2,1)=6,(1,2)=9,(2,2)=10.
        assert_eq!(s.finite_count, 4);
        approx(s.min.unwrap(), 5.0);
        approx(s.max.unwrap(), 10.0);
        approx(s.sum.unwrap(), 30.0);
    }

    #[test]
    fn image_on_limits_zero_scale_yields_empty() {
        let data = vec![1.0; 4];
        let s = Stats::for_image(
            &data,
            2,
            2,
            (0.0, 0.0),
            (0.0, 1.0),
            StatScope::OnLimits {
                x_range: (0.0, 1.0),
                y_range: (0.0, 1.0),
            },
        );
        assert_eq!(s.min, None);
        assert_eq!(s.finite_count, 0);
    }
}
