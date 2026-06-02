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
}

/// Accumulate finite samples into running count / min / max / sum, skipping
/// `NaN` (and any non-finite value), mirroring silx's NaN-ignoring stats.
#[derive(Clone, Copy, Debug, Default)]
struct Accumulator {
    count: usize,
    min: f64,
    max: f64,
    sum: f64,
}

impl Accumulator {
    fn push(&mut self, v: f64) {
        if !v.is_finite() {
            return;
        }
        if self.count == 0 {
            self.min = v;
            self.max = v;
        } else {
            self.min = self.min.min(v);
            self.max = self.max.max(v);
        }
        self.sum += v;
        self.count += 1;
    }

    /// Finish into a [`RoiStats`], weighting the integral by `area_per_sample`
    /// (the pixel area for an image; `1.0` for a curve).
    fn finish(self, area_per_sample: f64) -> RoiStats {
        if self.count == 0 {
            return RoiStats::default();
        }
        let mean = self.sum / self.count as f64;
        RoiStats {
            count: self.count,
            min: Some(self.min),
            max: Some(self.max),
            mean: Some(mean),
            sum: self.sum,
            integral: self.sum * area_per_sample,
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
                acc.push(value as f64);
            }
        }
    }
    let pixel_area = (scale[0] * scale[1]).abs();
    acc.finish(pixel_area)
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
                acc.push(yi);
            }
        }
    }
    acc.finish(1.0)
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
}
