//! Region-of-interest statistics: pure, GPU-free reductions over the scalar
//! data inside a [`Roi`], mirroring the numbers silx computes for its ROI
//! tables.
//!
//! - [`image_roi_stats`] reduces a scalar image over every pixel whose
//!   data-space center is inside the ROI (silx image-ROI statistics: count /
//!   min / max / mean / sum, plus the integral `sum × pixel_area`).
//! - [`curve_roi_stats`] reduces a curve's `y` over the points whose `x` falls
//!   in the ROI's `x`-span (silx `CurvesROIWidget` `computeRawAndNetCounts`
//!   raw-count selection: `from ≤ x ≤ to`, `CurvesROIWidget.py:1178-1217`).
//!
//! Everything here is pure (no egui / wgpu), so it is unit-testable on the CPU.
//! `NaN` samples are skipped in every reduction (silx ignores non-finite image
//! pixels in its stats).

use crate::core::roi::Roi;
use crate::core::stats::ComCoord;

/// Reduced statistics over the samples selected by a ROI (silx ROI stats).
///
/// `min`, `max`, and `mean` are `None` for an empty selection (no finite sample
/// inside the ROI); `sum` and `integral` are then `0.0`. `NaN` samples never
/// contribute.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct RoiStats {
    /// Number of finite samples inside the ROI.
    pub count: usize,
    /// Smallest finite sample, or `None` when `count == 0`.
    pub min: Option<f64>,
    /// Largest finite sample, or `None` when `count == 0`.
    pub max: Option<f64>,
    /// Arithmetic mean of the finite samples, or `None` when `count == 0`.
    pub mean: Option<f64>,
    /// Sum of the finite samples.
    pub sum: f64,
    /// Integral of the samples over the ROI. For an image this is
    /// `sum × pixel_area`; for a curve it is the same as `sum` (no area weight).
    pub integral: f64,
    /// Center of mass in data coordinates (silx `StatCOM`, stats.py:881),
    /// weighting each in-ROI sample's position by its value. Image: `(x, y)`;
    /// curve: `(x, None)`. [`ComCoord::NONE`] when `count == 0` or `sum == 0`
    /// (silx returns NaN for a zero-sum COM).
    pub com: ComCoord,
    /// Data coordinates of the first minimum in-ROI sample (silx `StatCoordMin`,
    /// stats.py:841). Image: `(x, y)`; curve: `(x, None)`.
    pub coord_min: ComCoord,
    /// Data coordinates of the first maximum in-ROI sample (silx `StatCoordMax`,
    /// stats.py:860). Image: `(x, y)`; curve: `(x, None)`.
    pub coord_max: ComCoord,
}

/// Accumulate finite samples into running count / min / max / sum plus the
/// center-of-mass moments and first-extremum positions, skipping `NaN` (and any
/// non-finite value), mirroring silx's NaN-ignoring stats. Each sample carries
/// its data-space position `(x, y)`; `y` is `NaN` for a 1D curve sample (its COM
/// and coords are then x-only).
#[derive(Clone, Copy, Debug, Default)]
struct Accumulator {
    count: usize,
    min: f64,
    max: f64,
    sum: f64,
    com_x_num: f64,
    com_y_num: f64,
    min_pos: (f64, f64),
    max_pos: (f64, f64),
}

impl Accumulator {
    fn push(&mut self, v: f64, x: f64, y: f64) {
        if !v.is_finite() {
            return;
        }
        self.sum += v;
        self.com_x_num += v * x;
        if y.is_finite() {
            self.com_y_num += v * y;
        }
        if self.count == 0 {
            self.min = v;
            self.max = v;
            self.min_pos = (x, y);
            self.max_pos = (x, y);
        } else {
            // Strictly-less / strictly-greater keeps the *first* extremum,
            // matching numpy argmin/argmax (silx stats.py:852, 873).
            if v < self.min {
                self.min = v;
                self.min_pos = (x, y);
            }
            if v > self.max {
                self.max = v;
                self.max_pos = (x, y);
            }
        }
        self.count += 1;
    }

    /// Finish into a [`RoiStats`], weighting the integral by `area_per_sample`
    /// (the pixel area for an image; `1.0` for a curve). `is_image` selects 2D
    /// `(x, y)` coordinates vs. an x-only curve coordinate (silx maps the flat
    /// index back through the axes; a curve has a single position axis).
    fn finish(self, area_per_sample: f64, is_image: bool) -> RoiStats {
        if self.count == 0 {
            return RoiStats::default();
        }
        let mean = self.sum / self.count as f64;
        let coord = |pos: (f64, f64)| {
            if is_image {
                ComCoord {
                    x: Some(pos.0),
                    y: Some(pos.1),
                }
            } else {
                ComCoord {
                    x: Some(pos.0),
                    y: None,
                }
            }
        };
        // COM is undefined (silx returns NaN, stats.py:894) when the weight sum
        // is zero; surface that as the empty coordinate.
        let com = if self.sum == 0.0 {
            ComCoord::NONE
        } else if is_image {
            ComCoord {
                x: Some(self.com_x_num / self.sum),
                y: Some(self.com_y_num / self.sum),
            }
        } else {
            ComCoord {
                x: Some(self.com_x_num / self.sum),
                y: None,
            }
        };
        RoiStats {
            count: self.count,
            min: Some(self.min),
            max: Some(self.max),
            mean: Some(mean),
            sum: self.sum,
            integral: self.sum * area_per_sample,
            com,
            coord_min: coord(self.min_pos),
            coord_max: coord(self.max_pos),
        }
    }
}

/// Statistics over a scalar image inside `roi`.
///
/// Visits every pixel `(col, row)` in row-major `data` (`data[row * width +
/// col]`, row 0 at the bottom — matching [`crate::ImageData`]). A pixel
/// contributes when its data-space **center** —
/// `(origin.x + (col + 0.5)·scale.x, origin.y + (row + 0.5)·scale.y)` — is
/// inside [`Roi::contains`]. The integral is `sum × pixel_area` with
/// `pixel_area = scale.x · scale.y` (its absolute value, so a flipped/negative
/// scale still gives a non-negative area weight).
///
/// `NaN` pixels are skipped. If `data.len() < width · height` the visit stops at
/// the available data; extra data beyond `width · height` is ignored.
pub fn image_roi_stats(
    roi: &Roi,
    data: &[f32],
    width: usize,
    height: usize,
    origin: [f64; 2],
    scale: [f64; 2],
) -> RoiStats {
    let mut acc = Accumulator::default();
    for row in 0..height {
        let cy = origin[1] + (row as f64 + 0.5) * scale[1];
        let base = row * width;
        for col in 0..width {
            let idx = base + col;
            let Some(&value) = data.get(idx) else {
                // Ragged / short buffer: nothing more to read in this row.
                break;
            };
            let cx = origin[0] + (col as f64 + 0.5) * scale[0];
            if roi.contains((cx, cy)) {
                acc.push(value as f64, cx, cy);
            }
        }
    }
    let pixel_area = (scale[0] * scale[1]).abs();
    acc.finish(pixel_area, /* is_image */ true)
}

/// Statistics over a curve's `y` values inside the `x`-span of `roi`.
///
/// Selects the points whose `x` lies within the ROI's `x`-extent and reduces
/// their `y`, mirroring silx `ROI.computeRawAndNetCounts` raw-count selection
/// (`from ≤ x ≤ to`, then `y.sum()`). The ROI's `x`-span is taken from
/// [`roi_x_span`]; ROIs with no meaningful `x`-span (e.g. an `HRange`, whose
/// selection is on `y`) select no points and return empty stats. `x` and `y`
/// are paired by index; points past the shorter of the two are ignored.
/// `NaN` `y` (or `NaN` `x`) samples are skipped. The integral equals `sum`
/// (a curve sum carries no area weight).
pub fn curve_roi_stats(roi: &Roi, x: &[f64], y: &[f64]) -> RoiStats {
    let mut acc = Accumulator::default();
    if let Some((x0, x1)) = roi_x_span(roi) {
        for (&xi, &yi) in x.iter().zip(y.iter()) {
            if xi.is_finite() && xi >= x0 && xi <= x1 {
                acc.push(yi, xi, f64::NAN);
            }
        }
    }
    acc.finish(1.0, /* is_image */ false)
}

/// Curve-ROI raw/net counts and raw/net area, mirroring silx
/// `ROI.computeRawAndNetCounts` / `computeRawAndNetArea`
/// (`CurvesROIWidget.py:1178-1256`) — the per-ROI columns of silx's 1D
/// `CurvesROIWidget`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurveRoiCounts {
    /// Sum of the curve's `y` over the points inside the ROI's `x`-span (silx
    /// "Raw Counts" = `yw.sum()`).
    pub raw_counts: f64,
    /// Raw counts minus a straight-line background joining the first and last
    /// selected points (silx "Net Counts"). `0` when the selected span has zero
    /// `x`-width.
    pub net_counts: f64,
    /// Trapezoidal integral of the curve over the selected points, in selection
    /// (array) order (silx "Raw Area" = `trapezoid(yw, xw)`).
    pub raw_area: f64,
    /// Raw area minus the trapezoidal area under the straight background joining
    /// the points closest to the ROI's `from`/`to` edges (silx "Net Area").
    pub net_area: f64,
}

/// Per-ROI raw/net counts and raw/net area for a curve, mirroring silx
/// `CurvesROIWidget` (`ROI.computeRawAndNetCounts` / `computeRawAndNetArea`).
///
/// Selects the curve points whose `x` lies in the ROI's inclusive `x`-span
/// (`from ≤ x ≤ to`, from [`roi_x_span`]), preserving the curve's array order
/// (silx does **not** sort, and neither does this — `numpy.trapezoid` integrates
/// in sequence, so a curve whose `x` is not monotonic yields the same
/// order-dependent areas silx produces). Unlike [`curve_roi_stats`], `NaN` `y`
/// samples are **not** filtered (silx `computeRawAndNetCounts` sums them as-is,
/// so a `NaN` in range propagates to the counts) — this is faithful to silx, not
/// an oversight. Points with non-finite `x` fall outside any finite span and are
/// excluded. `x` and `y` are paired by index; the shorter array bounds the pair
/// count.
///
/// Returns `None` for a ROI with no `x`-span (e.g. an `HRange`, which selects on
/// `y`); such ROIs are not curve ROIs in silx's 1D sense.
///
/// The background models:
/// - **Net counts:** a line through the first and last selected points
///   (`background[i] = y₀ + slope·(xᵢ − x₀)`, `slope = (yₙ − y₀)/(xₙ − x₀)`),
///   subtracting `Σ background`. When `xₙ == x₀` (zero width) net counts is `0`.
/// - **Net area:** the trapezoid under the straight line joining the `y` values
///   at the points closest to `from` and to `to` (silx's `numpy.trapezoid` over a
///   two-value background, which reduces to `(x_last − x_first)·(y_left + y_right)/2`).
pub fn curve_roi_counts(roi: &Roi, x: &[f64], y: &[f64]) -> Option<CurveRoiCounts> {
    let (from, to) = roi_x_span(roi)?;

    // Selection in array order (silx does not sort): pair by index, keep points
    // whose x is inside [from, to]. Non-finite x fails the comparison and is
    // dropped, exactly as numpy's `(from <= x) & (x <= to)` mask.
    let mut xw = Vec::new();
    let mut yw = Vec::new();
    for (&xi, &yi) in x.iter().zip(y.iter()) {
        if xi >= from && xi <= to {
            xw.push(xi);
            yw.push(yi);
        }
    }

    if xw.is_empty() {
        return Some(CurveRoiCounts {
            raw_counts: 0.0,
            net_counts: 0.0,
            raw_area: 0.0,
            net_area: 0.0,
        });
    }

    let raw_counts: f64 = yw.iter().sum();

    // Net counts: subtract a line through the first and last selected points.
    let x0 = xw[0];
    let xn = xw[xw.len() - 1];
    let y0 = yw[0];
    let yn = yw[yw.len() - 1];
    let delta_x = xn - x0;
    let net_counts = if delta_x > 0.0 {
        let slope = (yn - y0) / delta_x;
        let background: f64 = xw.iter().map(|&xi| y0 + slope * (xi - x0)).sum();
        raw_counts - background
    } else {
        0.0
    };

    // Raw area: trapezoid over the selected points in array order.
    let raw_area = trapezoid(&yw, &xw);

    // Net area: subtract the trapezoid under the straight line joining the y
    // values closest to `from` and to `to`. silx's `trapezoid(yBackground, x=x)`
    // with a 2-element background reduces (via numpy broadcasting) to
    // (x_last - x_first) * (y_left + y_right) / 2.
    let left = argmin_abs_diff(&xw, from);
    let right = argmin_abs_diff(&xw, to);
    let background_area = (xn - x0) * (yw[left] + yw[right]) / 2.0;
    let net_area = raw_area - background_area;

    Some(CurveRoiCounts {
        raw_counts,
        net_counts,
        raw_area,
        net_area,
    })
}

/// Trapezoidal integral `Σ (xᵢ₊₁ − xᵢ)·(yᵢ₊₁ + yᵢ)/2` over the paired samples in
/// the given order (matching `numpy.trapezoid(y, x)`; not sorted). Fewer than two
/// samples integrate to `0`.
fn trapezoid(y: &[f64], x: &[f64]) -> f64 {
    x.windows(2)
        .zip(y.windows(2))
        .map(|(xs, ys)| (xs[1] - xs[0]) * (ys[1] + ys[0]) / 2.0)
        .sum()
}

/// Index of the value in `values` closest to `target` (`argmin |vᵢ − target|`),
/// returning the first on ties to match `numpy.argmin`. `values` is non-empty at
/// every call site.
fn argmin_abs_diff(values: &[f64], target: f64) -> usize {
    let mut best = 0;
    let mut best_diff = (values[0] - target).abs();
    for (i, &v) in values.iter().enumerate().skip(1) {
        let diff = (v - target).abs();
        if diff < best_diff {
            best = i;
            best_diff = diff;
        }
    }
    best
}

/// The inclusive `x`-span `(x_min, x_max)` a ROI selects curve points over, or
/// `None` when the ROI does not bound `x` (its selection is on another axis).
///
/// Mirrors the silx curve ROIs whose extent is an `x` interval: a `VRange`'s
/// `x`-band, a `Rect`'s `x`-extent, and a `Line`'s `x`-extent (the span between
/// its endpoints). An `HRange` bounds only `y`, so it returns `None`.
pub fn roi_x_span(roi: &Roi) -> Option<(f64, f64)> {
    match roi {
        Roi::VRange { x } => Some(norm(x.0, x.1)),
        Roi::Rect { x, .. } => Some(norm(x.0, x.1)),
        Roi::Line { start, end } => Some(norm(start.0, end.0)),
        _ => None,
    }
}

/// Order a pair so the smaller value is first.
fn norm(a: f64, b: f64) -> (f64, f64) {
    if a <= b { (a, b) } else { (b, a) }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 4×4 image with value = (row * 4 + col) as f32, row 0 at the bottom.
    // Pixel (col, row) center in data space (origin 0, scale 1) is
    // (col + 0.5, row + 0.5).
    fn ramp_4x4() -> Vec<f32> {
        (0..16).map(|v| v as f32).collect()
    }

    #[test]
    fn image_empty_region_yields_no_stats() {
        // A rect entirely outside the image selects nothing.
        let roi = Roi::Rect {
            x: (100.0, 200.0),
            y: (100.0, 200.0),
        };
        let s = image_roi_stats(&roi, &ramp_4x4(), 4, 4, [0.0, 0.0], [1.0, 1.0]);
        assert_eq!(s.count, 0);
        assert_eq!(s.min, None);
        assert_eq!(s.max, None);
        assert_eq!(s.mean, None);
        assert_eq!(s.sum, 0.0);
        assert_eq!(s.integral, 0.0);
    }

    #[test]
    fn image_single_pixel_region() {
        // A rect tightly around pixel (1, 2) center (1.5, 2.5): value = 2*4+1 = 9.
        let roi = Roi::Rect {
            x: (1.4, 1.6),
            y: (2.4, 2.6),
        };
        let s = image_roi_stats(&roi, &ramp_4x4(), 4, 4, [0.0, 0.0], [1.0, 1.0]);
        assert_eq!(s.count, 1);
        assert_eq!(s.min, Some(9.0));
        assert_eq!(s.max, Some(9.0));
        assert_eq!(s.mean, Some(9.0));
        assert_eq!(s.sum, 9.0);
        assert_eq!(s.integral, 9.0); // pixel area 1
    }

    #[test]
    fn image_full_region_covers_every_pixel() {
        // A rect that contains every pixel center [0.5, 3.5] in both axes.
        let roi = Roi::Rect {
            x: (0.0, 4.0),
            y: (0.0, 4.0),
        };
        let s = image_roi_stats(&roi, &ramp_4x4(), 4, 4, [0.0, 0.0], [1.0, 1.0]);
        assert_eq!(s.count, 16);
        assert_eq!(s.min, Some(0.0));
        assert_eq!(s.max, Some(15.0));
        assert_eq!(s.sum, (0..16).sum::<i32>() as f64); // 120
        assert_eq!(s.mean, Some(120.0 / 16.0));
        assert_eq!(s.integral, 120.0);
    }

    #[test]
    fn image_com_and_coords_are_value_weighted_in_data_space() {
        // Full 4x4 ramp (value = 4*row+col, center (col+0.5, row+0.5)).
        // COM_x = Σ v·(col+0.5) / Σ v = 260/120 = 13/6;
        // COM_y = Σ v·(row+0.5) / Σ v = 320/120 = 8/3 (pulled toward the larger
        // values, top-right). First min (value 0) at center (0.5, 0.5); first
        // max (value 15) at center (3.5, 3.5).
        let roi = Roi::Rect {
            x: (0.0, 4.0),
            y: (0.0, 4.0),
        };
        let s = image_roi_stats(&roi, &ramp_4x4(), 4, 4, [0.0, 0.0], [1.0, 1.0]);
        assert!(
            (s.com.x.unwrap() - 13.0 / 6.0).abs() < 1e-9,
            "com.x = {:?}",
            s.com.x
        );
        assert!(
            (s.com.y.unwrap() - 8.0 / 3.0).abs() < 1e-9,
            "com.y = {:?}",
            s.com.y
        );
        assert_eq!(s.coord_min.x, Some(0.5));
        assert_eq!(s.coord_min.y, Some(0.5));
        assert_eq!(s.coord_max.x, Some(3.5));
        assert_eq!(s.coord_max.y, Some(3.5));
    }

    #[test]
    fn curve_com_and_coords_are_x_only() {
        // x=[0,1,2,3], y=[1,2,3,4], VRange x∈[0,3] selects all four points.
        // COM_x = Σ y·x / Σ y = (0+2+6+12)/10 = 2.0, y component undefined (1D).
        // First min y=1 at x=0; first max y=4 at x=3.
        let roi = Roi::VRange { x: (0.0, 3.0) };
        let x = [0.0, 1.0, 2.0, 3.0];
        let y = [1.0, 2.0, 3.0, 4.0];
        let s = curve_roi_stats(&roi, &x, &y);
        assert_eq!(s.count, 4);
        assert_eq!(s.com.x, Some(2.0));
        assert_eq!(s.com.y, None);
        assert_eq!(
            s.coord_min,
            ComCoord {
                x: Some(0.0),
                y: None
            }
        );
        assert_eq!(
            s.coord_max,
            ComCoord {
                x: Some(3.0),
                y: None
            }
        );
    }

    #[test]
    fn image_all_nan_pixels_are_skipped() {
        let data = vec![f32::NAN; 16];
        let roi = Roi::Rect {
            x: (0.0, 4.0),
            y: (0.0, 4.0),
        };
        let s = image_roi_stats(&roi, &data, 4, 4, [0.0, 0.0], [1.0, 1.0]);
        assert_eq!(s.count, 0);
        assert_eq!(s.min, None);
        assert_eq!(s.sum, 0.0);
    }

    #[test]
    fn image_skips_nan_but_keeps_finite_pixels() {
        // One NaN among finite pixels: it does not contribute, others do.
        let mut data = ramp_4x4();
        data[2 * 4 + 1] = f32::NAN; // the pixel worth 9
        let roi = Roi::Rect {
            x: (0.0, 4.0),
            y: (0.0, 4.0),
        };
        let s = image_roi_stats(&roi, &data, 4, 4, [0.0, 0.0], [1.0, 1.0]);
        assert_eq!(s.count, 15);
        assert_eq!(s.sum, 120.0 - 9.0);
        assert_eq!(s.max, Some(15.0));
    }

    #[test]
    fn image_integral_scales_with_pixel_area() {
        let roi = Roi::Rect {
            x: (-10.0, 10.0),
            y: (-10.0, 10.0),
        };
        // scale (2, 3) -> pixel area 6; sum is unchanged (120), integral *= 6.
        let s = image_roi_stats(&roi, &ramp_4x4(), 4, 4, [-10.0, -10.0], [2.0, 3.0]);
        assert_eq!(s.count, 16);
        assert_eq!(s.sum, 120.0);
        assert_eq!(s.integral, 120.0 * 6.0);
    }

    #[test]
    fn image_circle_selects_fewer_than_its_bounding_rect() {
        // Circle centered on pixel (1,1)'s center (1.5,1.5) radius 1.1 selects the
        // center pixel and its 4 edge neighbours (dist 1.0), not the 4 corners
        // (dist ~1.41). The bounding 3×3 rect would select all 9.
        let circle = Roi::Circle {
            center: (1.5, 1.5),
            radius: 1.1,
        };
        let s = image_roi_stats(&circle, &ramp_4x4(), 4, 4, [0.0, 0.0], [1.0, 1.0]);
        assert_eq!(s.count, 5);
        // Center (1,1)=5, neighbours (0,1)=4,(2,1)=6,(1,0)=1,(1,2)=9 -> sum 25.
        assert_eq!(s.sum, 25.0);

        let rect = Roi::Rect {
            x: (0.0, 3.0),
            y: (0.0, 3.0),
        };
        let sr = image_roi_stats(&rect, &ramp_4x4(), 4, 4, [0.0, 0.0], [1.0, 1.0]);
        assert_eq!(sr.count, 9);
    }

    #[test]
    fn image_polygon_triangle_containment() {
        // Lower-left triangle (0,0)-(4,0)-(0,4): selects pixel centers below the
        // hypotenuse x + y < 4. Centers are at (col+0.5, row+0.5).
        let tri = Roi::Polygon {
            vertices: vec![(0.0, 0.0), (4.0, 0.0), (0.0, 4.0)],
        };
        let s = image_roi_stats(&tri, &ramp_4x4(), 4, 4, [0.0, 0.0], [1.0, 1.0]);
        // Count of (col,row) with (col+0.5)+(row+0.5) < 4, i.e. col+row < 3:
        // row0: col 0,1,2 (3); row1: col 0,1 (2); row2: col 0 (1); row3: none.
        assert_eq!(s.count, 6);
    }

    #[test]
    fn image_ragged_buffer_stops_at_available_data() {
        // Only the first 8 of 16 pixels are present (rows 0 and 1).
        let data: Vec<f32> = (0..8).map(|v| v as f32).collect();
        let roi = Roi::Rect {
            x: (0.0, 4.0),
            y: (0.0, 4.0),
        };
        let s = image_roi_stats(&roi, &data, 4, 4, [0.0, 0.0], [1.0, 1.0]);
        assert_eq!(s.count, 8);
        assert_eq!(s.sum, (0..8).sum::<i32>() as f64); // 28
    }

    // --- curve stats ---

    #[test]
    fn curve_vrange_selects_points_in_x_span() {
        let roi = Roi::VRange { x: (2.0, 4.0) };
        let x = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let y = vec![10.0, 20.0, 30.0, 40.0, 50.0];
        let s = curve_roi_stats(&roi, &x, &y);
        // x in [2,4] -> y 20, 30, 40.
        assert_eq!(s.count, 3);
        assert_eq!(s.min, Some(20.0));
        assert_eq!(s.max, Some(40.0));
        assert_eq!(s.sum, 90.0);
        assert_eq!(s.mean, Some(30.0));
        assert_eq!(s.integral, 90.0); // curve integral == sum
    }

    #[test]
    fn curve_span_is_inclusive_at_both_edges() {
        let roi = Roi::VRange { x: (2.0, 4.0) };
        let x = vec![2.0, 4.0];
        let y = vec![7.0, 8.0];
        let s = curve_roi_stats(&roi, &x, &y);
        assert_eq!(s.count, 2); // both edges included
        assert_eq!(s.sum, 15.0);
    }

    #[test]
    fn curve_empty_selection_when_no_point_in_span() {
        let roi = Roi::VRange { x: (100.0, 200.0) };
        let x = vec![1.0, 2.0, 3.0];
        let y = vec![1.0, 2.0, 3.0];
        let s = curve_roi_stats(&roi, &x, &y);
        assert_eq!(s.count, 0);
        assert_eq!(s.min, None);
        assert_eq!(s.sum, 0.0);
    }

    #[test]
    fn curve_skips_nan_y_and_nan_x() {
        let roi = Roi::VRange { x: (0.0, 10.0) };
        let x = vec![1.0, f64::NAN, 3.0, 4.0];
        let y = vec![1.0, 2.0, f64::NAN, 4.0];
        let s = curve_roi_stats(&roi, &x, &y);
        // NaN x (index 1) and NaN y (index 2) are skipped; 1.0 and 4.0 remain.
        assert_eq!(s.count, 2);
        assert_eq!(s.sum, 5.0);
    }

    #[test]
    fn curve_rect_uses_its_x_extent() {
        let roi = Roi::Rect {
            x: (2.0, 4.0),
            y: (-100.0, 100.0),
        };
        let x = vec![1.0, 3.0, 5.0];
        let y = vec![1.0, 3.0, 5.0];
        let s = curve_roi_stats(&roi, &x, &y);
        assert_eq!(s.count, 1); // only x=3 in [2,4]
        assert_eq!(s.sum, 3.0);
    }

    #[test]
    fn curve_line_uses_unordered_endpoint_x_extent() {
        // Endpoints given right-to-left; the span is still [2, 6].
        let roi = Roi::Line {
            start: (6.0, 0.0),
            end: (2.0, 0.0),
        };
        let x = vec![1.0, 2.0, 4.0, 6.0, 7.0];
        let y = vec![1.0, 1.0, 1.0, 1.0, 1.0];
        let s = curve_roi_stats(&roi, &x, &y);
        assert_eq!(s.count, 3); // x 2,4,6
    }

    #[test]
    fn curve_hrange_has_no_x_span_and_selects_nothing() {
        let roi = Roi::HRange { y: (0.0, 10.0) };
        assert_eq!(roi_x_span(&roi), None);
        let x = vec![1.0, 2.0, 3.0];
        let y = vec![1.0, 2.0, 3.0];
        let s = curve_roi_stats(&roi, &x, &y);
        assert_eq!(s.count, 0);
    }

    #[test]
    fn curve_paired_by_index_ignores_unpaired_tail() {
        let roi = Roi::VRange { x: (0.0, 10.0) };
        let x = vec![1.0, 2.0, 3.0];
        let y = vec![1.0, 2.0]; // shorter
        let s = curve_roi_stats(&roi, &x, &y);
        assert_eq!(s.count, 2); // x=3 has no paired y
        assert_eq!(s.sum, 3.0);
    }

    #[test]
    fn curve_counts_linear_data_has_zero_net() {
        // Linear y over a VRange: raw = Σy; the linear-endpoint background
        // matches the data exactly, so net counts and net area are both 0.
        let roi = Roi::VRange { x: (1.0, 3.0) };
        let x = vec![0.0, 1.0, 2.0, 3.0, 4.0];
        let y = vec![0.0, 1.0, 2.0, 3.0, 4.0];
        let c = curve_roi_counts(&roi, &x, &y).expect("VRange has an x-span");
        assert_eq!(c.raw_counts, 6.0); // 1+2+3
        assert_eq!(c.net_counts, 0.0);
        assert_eq!(c.raw_area, 4.0); // trapezoid([1,2,3],[1,2,3])
        assert_eq!(c.net_area, 0.0);
    }

    #[test]
    fn curve_counts_peak_over_flat_background() {
        // A triangular peak on a zero baseline: the flat endpoint background is
        // 0, so net == raw for both counts and area.
        let roi = Roi::VRange { x: (0.0, 4.0) };
        let x = vec![0.0, 1.0, 2.0, 3.0, 4.0];
        let y = vec![0.0, 0.0, 10.0, 0.0, 0.0];
        let c = curve_roi_counts(&roi, &x, &y).expect("VRange has an x-span");
        assert_eq!(c.raw_counts, 10.0);
        assert_eq!(c.net_counts, 10.0); // slope 0 -> background 0
        assert_eq!(c.raw_area, 10.0); // 5 + 5 over the two triangle halves
        assert_eq!(c.net_area, 10.0); // endpoints both 0 -> background area 0
    }

    #[test]
    fn curve_counts_sloped_background_subtracted() {
        // Sloped endpoints: background follows the y0->yn line. x=[0,1,2],
        // y=[0,5,2] over (0,2): raw=7; line 0->2 gives background [0,1,2] (Σ=3)
        // so net counts = 4; raw area = 6, background trapezoid = 2 -> net 4.
        let roi = Roi::VRange { x: (0.0, 2.0) };
        let x = vec![0.0, 1.0, 2.0];
        let y = vec![0.0, 5.0, 2.0];
        let c = curve_roi_counts(&roi, &x, &y).expect("VRange has an x-span");
        assert_eq!(c.raw_counts, 7.0);
        assert_eq!(c.net_counts, 4.0);
        assert_eq!(c.raw_area, 6.0);
        assert_eq!(c.net_area, 4.0);
    }

    #[test]
    fn curve_counts_empty_selection_is_all_zero() {
        // A span outside the data selects nothing: silx returns 0 for every
        // count/area (not None).
        let roi = Roi::VRange { x: (100.0, 200.0) };
        let x = vec![0.0, 1.0, 2.0];
        let y = vec![5.0, 6.0, 7.0];
        let c = curve_roi_counts(&roi, &x, &y).expect("VRange has an x-span");
        assert_eq!(
            c,
            CurveRoiCounts {
                raw_counts: 0.0,
                net_counts: 0.0,
                raw_area: 0.0,
                net_area: 0.0,
            }
        );
    }

    #[test]
    fn curve_counts_single_point_has_zero_net_and_area() {
        // One selected point: deltaX is 0 so net counts is 0 by silx's guard,
        // and a single point has no trapezoid (raw/net area 0).
        let roi = Roi::VRange { x: (1.9, 2.1) };
        let x = vec![0.0, 1.0, 2.0, 3.0];
        let y = vec![0.0, 0.0, 9.0, 0.0];
        let c = curve_roi_counts(&roi, &x, &y).expect("VRange has an x-span");
        assert_eq!(c.raw_counts, 9.0);
        assert_eq!(c.net_counts, 0.0);
        assert_eq!(c.raw_area, 0.0);
        assert_eq!(c.net_area, 0.0);
    }

    #[test]
    fn curve_counts_preserve_array_order_like_numpy_trapezoid() {
        // silx does not sort the selection; with non-monotonic x the trapezoid
        // is order-dependent. x=[0,2,1], y=[0,10,0] over (0,2): selection keeps
        // array order, so segments are (0->2) and (2->1) — the second has a
        // negative dx, giving raw area 10 + (-5) = 5.
        let roi = Roi::VRange { x: (0.0, 2.0) };
        let x = vec![0.0, 2.0, 1.0];
        let y = vec![0.0, 10.0, 0.0];
        let c = curve_roi_counts(&roi, &x, &y).expect("VRange has an x-span");
        assert_eq!(c.raw_counts, 10.0);
        assert_eq!(c.raw_area, 5.0); // (2-0)*(0+10)/2 + (1-2)*(10+0)/2 = 10 - 5
    }

    #[test]
    fn curve_counts_none_without_x_span() {
        // An HRange selects on y, not x: not a curve ROI -> no counts.
        let roi = Roi::HRange { y: (0.0, 10.0) };
        let x = vec![0.0, 1.0, 2.0];
        let y = vec![1.0, 2.0, 3.0];
        assert_eq!(curve_roi_counts(&roi, &x, &y), None);
    }
}
