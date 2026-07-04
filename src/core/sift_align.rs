//! SIFT-based affine image registration — the data layer behind
//! [`CompareImages`](crate::CompareImages)' `AUTO` alignment mode.
//!
//! This ports silx's SIFT auto-alignment pipeline
//! (`silx.gui.plot.CompareImages.__createSiftData`):
//!
//! 1. detect SIFT keypoints + 128-d descriptors on both images
//!    (silx `SiftPlan`),
//! 2. match the two descriptor sets with Lowe's nearest-neighbour
//!    distance-ratio test (silx `MatchPlan`),
//! 3. fit the 6-parameter affine that maps image-A keypoints to their image-B
//!    matches by least squares (silx `matching_correction`,
//!    `numpy.linalg.lstsq` of the `x' = a·x + b·y + c`, `y' = d·x + e·y + f`
//!    normal equations),
//! 4. warp image B onto image A's grid through that affine (silx
//!    `LinearAlign.align`), and
//! 5. decompose the affine into `(tx, ty, sx, sy, rotation)` (silx
//!    `CompareImages.__toAffineTransformation`).
//!
//! The SIFT detector, descriptor, ratio matcher, and affine least-squares all
//! come from the pure-Rust [`lowe_sift`] crate (silx uses an OpenCL SIFT; the
//! algorithm — Lowe's 2004 IJCV pipeline — is the same). Coordinates stay in
//! image `(x, y)` end to end (estimation, warp, decomposition); silx instead
//! swaps to `(y, x)` for its OpenCL warp kernel and decomposes in that order,
//! an addressing artefact of the GPU kernel rather than a semantic difference,
//! so siplot's `sx`/`sy`/`rotation` read out in the natural image-axis order.

use lowe_sift::{Feature, GrayImage, Sift, estimate_affine_from_pairs};

/// silx `MatchPlan`'s descriptor ratio value: `MatchRatio = 0.73`
/// (`param.py:78`), NOT the 0.8 from Lowe's paper. silx's matcher works on L1
/// descriptor distances and its ratio gate is intentionally tighter.
pub const MATCH_RATIO: f32 = 0.73;

/// The Lowe distance-ratio threshold silx compares against — `MatchRatio²`
/// (`0.73² = 0.5329`), applied to the **L1** nearest/second-nearest ratio
/// (`match.py:199`, `matching_cpu.cl:113`: "0.73*0.73 for L1 distance"). The old
/// 0.8 value gated the *L2* ratio (Lowe's paper), accepting substantially looser
/// matches than silx.
pub const MATCH_RATIO_THRESHOLD: f32 = MATCH_RATIO * MATCH_RATIO;

/// Minimum matched keypoints for silx to fit the 6-DOF affine — 3 points per
/// degree of freedom (`alignment.py:309`: `len_match < 3 * 6`). Below this, silx
/// falls back to a shift-only median translation.
const AFFINE_MIN_MATCHES: usize = 3 * 6;

/// The affine transform applied to image B to align it to image A, decomposed
/// into translation, per-axis scale, and rotation — silx
/// `CompareImages.AffineTransformation` (built by `__toAffineTransformation`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AffineTransformation {
    /// Translation along x (silx `offset[0]`).
    pub tx: f64,
    /// Translation along y (silx `offset[1]`).
    pub ty: f64,
    /// Scale along x: `sign(a)·sqrt(a² + b²)` of the 2×2 matrix `[[a, b], [c, d]]`.
    pub sx: f64,
    /// Scale along y: `sign(d)·sqrt(c² + d²)`.
    pub sy: f64,
    /// Rotation in radians: `atan2(-b, a)`.
    pub rotation: f64,
}

/// One matched keypoint pair (silx `__matching_keypoints`): the keypoint
/// coordinate on the common display grid in image A and the corresponding
/// location in image B, plus the image-A keypoint scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatchedKeypoint {
    /// Keypoint x in image A (common-grid coordinate).
    pub ax: f32,
    /// Keypoint y in image A.
    pub ay: f32,
    /// Matched keypoint x in image B.
    pub bx: f32,
    /// Matched keypoint y in image B.
    pub by: f32,
    /// Image-A keypoint scale (silx `match[:].scale[:, 0]`).
    pub scale: f32,
}

/// Result of [`sift_auto_align`]: image B warped onto the common grid plus the
/// estimated transform and the matched keypoints used to find it.
#[derive(Clone, Debug)]
pub struct SiftAlignment {
    /// Common-grid width (`max(w_a, w_b)`).
    pub width: usize,
    /// Common-grid height (`max(h_a, h_b)`).
    pub height: usize,
    /// Image B resampled onto the common grid so it aligns with A, row-major
    /// `width × height` (silx `LinearAlign.align` result).
    pub aligned: Vec<f32>,
    /// 2×2 linear part `[[a, b], [c, d]]` mapping an A coordinate `(x, y)` to its
    /// location in B: `x_b = a·x + b·y + tx`, `y_b = c·x + d·y + ty`.
    pub matrix: [[f64; 2]; 2],
    /// Translation `(tx, ty)` of that mapping.
    pub offset: (f64, f64),
    /// The decomposed `(tx, ty, sx, sy, rotation)` form (silx `getTransformation`).
    pub transformation: AffineTransformation,
    /// The matched keypoint pairs (silx `__matching_keypoints`).
    pub matches: Vec<MatchedKeypoint>,
}

/// Min–max normalise scalar pixels to `[0, 1]` for the SIFT detector (which
/// expects `f32` pixels in that range). Non-finite samples are treated as the
/// minimum; a flat image (or empty input) maps to all zeros.
fn normalize01(data: &[f32]) -> Vec<f32> {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for &v in data {
        if v.is_finite() {
            min = min.min(v);
            max = max.max(v);
        }
    }
    if !min.is_finite() || !max.is_finite() || max <= min {
        return vec![0.0; data.len()];
    }
    let inv = 1.0 / (max - min);
    data.iter()
        .map(|&v| if v.is_finite() { (v - min) * inv } else { 0.0 })
        .collect()
}

/// Zero-pad a row-major `src_w × src_h` image into a top-left-anchored
/// `dst_w × dst_h` grid (silx `__createMarginImage` with `center=False`, the
/// AUTO branch's default). When the source already fills the grid this is a
/// plain copy.
fn pad_top_left(src: &[f32], src_w: usize, src_h: usize, dst_w: usize, dst_h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; dst_w * dst_h];
    if src_w == 0 || src_h == 0 || src_w > dst_w || src_h > dst_h {
        return out;
    }
    for r in 0..src_h {
        let dst_base = r * dst_w;
        let src_base = r * src_w;
        out[dst_base..dst_base + src_w].copy_from_slice(&src[src_base..src_base + src_w]);
    }
    out
}

/// Decompose an affine `matrix = [[a, b], [c, d]]` + `offset = (tx, ty)` into
/// silx's `AffineTransformation(tx, ty, sx, sy, rotation)`
/// (`CompareImages.__toAffineTransformation`):
/// `rotation = atan2(-b, a)`, `sx = sign(a)·sqrt(a² + b²)`,
/// `sy = sign(d)·sqrt(c² + d²)`.
pub fn decompose_affine(matrix: [[f64; 2]; 2], offset: (f64, f64)) -> AffineTransformation {
    let [[a, b], [c, d]] = matrix;
    let sign = |v: f64| if v < 0.0 { -1.0 } else { 1.0 };
    AffineTransformation {
        tx: offset.0,
        ty: offset.1,
        sx: sign(a) * (a * a + b * b).sqrt(),
        sy: sign(d) * (c * c + d * d).sqrt(),
        rotation: (-b).atan2(a),
    }
}

/// Bilinearly sample row-major `w × h` `data` at continuous `(x, y)`, returning
/// `0.0` for coordinates outside the image (silx's OpenCL warp uses a
/// clamp-to-border-0 sampler).
fn sample_bilinear_border0(data: &[f32], w: usize, h: usize, x: f64, y: f64) -> f32 {
    if w == 0 || h == 0 || x < 0.0 || y < 0.0 || x > (w - 1) as f64 || y > (h - 1) as f64 {
        return 0.0;
    }
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = x - x0 as f64;
    let fy = y - y0 as f64;
    let at = |r: usize, col: usize| data[r * w + col] as f64;
    let top = at(y0, x0) * (1.0 - fx) + at(y0, x1) * fx;
    let bot = at(y1, x0) * (1.0 - fx) + at(y1, x1) * fx;
    (top * (1.0 - fy) + bot * fy) as f32
}

/// Warp `b` (row-major `w × h`) onto the same `w × h` grid through the affine
/// that maps an output A-coordinate `(x, y)` to its source location in B,
/// bilinearly resampling with a zero border (silx `LinearAlign.align`).
fn warp_affine(
    b: &[f32],
    w: usize,
    h: usize,
    matrix: [[f64; 2]; 2],
    offset: (f64, f64),
) -> Vec<f32> {
    let [[a, bb], [c, d]] = matrix;
    let (tx, ty) = offset;
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let xf = x as f64;
            let yf = y as f64;
            let bx = a * xf + bb * yf + tx;
            let by = c * xf + d * yf + ty;
            out[y * w + x] = sample_bilinear_border0(b, w, h, bx, by);
        }
    }
    out
}

/// Match descriptor set `a` against `b` with silx `MatchPlan`'s L1
/// nearest-neighbour ratio test (`matching_cpu.cl` `matching` kernel): for each
/// query descriptor, scan every train descriptor tracking the nearest (`dist1`)
/// and second-nearest (`dist2`) **L1** distances, and accept the nearest when
/// `dist2 != 0 && dist1 / dist2 < ratio`. Returns `(query_index, train_index)`
/// pairs. silx computes L1 over uint8 descriptors; lowe-sift's are a global
/// rescaling of the same gradient histogram, and that scale cancels in the
/// `dist1 / dist2` ratio, so the gate matches silx up to the detector's own
/// descriptor differences (whereas `lowe_sift::match_features` gates the L2
/// ratio at a different value).
fn match_features_l1(a: &[Feature], b: &[Feature], ratio: f32) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    for (qi, fa) in a.iter().enumerate() {
        let da = fa.descriptor.as_slice();
        // MAXFLOAT seeds like the kernel: a lone train descriptor leaves dist2 at
        // the seed, so dist1 / dist2 ≈ 0 accepts it (matching silx's MAXFLOAT).
        let mut dist1 = f32::MAX;
        let mut dist2 = f32::MAX;
        let mut best = 0usize;
        for (ti, fb) in b.iter().enumerate() {
            let db = fb.descriptor.as_slice();
            let dist: f32 = da.iter().zip(db.iter()).map(|(x, y)| (x - y).abs()).sum();
            if dist < dist1 {
                dist2 = dist1;
                dist1 = dist;
                best = ti;
            } else if dist < dist2 {
                dist2 = dist;
            }
        }
        if dist2 != 0.0 && dist1 / dist2 < ratio {
            pairs.push((qi, best));
        }
    }
    pairs
}

/// `numpy.median` of `values`: the middle element for odd length, the mean of
/// the two middle elements for even length. Sorts `values` in place; returns
/// `0.0` for an empty slice (callers pass a non-empty match set).
fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// The shift-only registration silx falls back to when there are too few matches
/// to fit an affine (`alignment.py:311-319`): an identity linear part with the
/// median keypoint translation `(median(bx − ax), median(by − ay))`. Robust to
/// the outliers a 3–17-pair least-squares fit would chase into spurious
/// scale/rotation. (silx computes the median in `(y, x)` for its GPU kernel;
/// siplot stays in image `(x, y)` end to end, per this module's convention.)
fn shift_only_transform(matches: &[MatchedKeypoint]) -> ([[f64; 2]; 2], (f64, f64)) {
    let mut dxs: Vec<f64> = matches.iter().map(|m| (m.bx - m.ax) as f64).collect();
    let mut dys: Vec<f64> = matches.iter().map(|m| (m.by - m.ay) as f64).collect();
    (
        [[1.0, 0.0], [0.0, 1.0]],
        (median(&mut dxs), median(&mut dys)),
    )
}

/// Register image B onto image A by SIFT keypoint matching + affine warp,
/// mirroring silx `CompareImages.__createSiftData`.
///
/// `a`/`b` are row-major scalar images of shape `(wa, ha)`/`(wb, hb)`. Both are
/// first zero-padded (top-left) onto the common grid `max(wa, wb) × max(ha, hb)`
/// — silx pads to the max shape before SIFT — then SIFT-aligned there.
///
/// Returns `None` when registration is not possible: an empty/degenerate image,
/// no matched keypoints (silx's `len_match == 0` early return), or a singular
/// least-squares system. With 1–17 matches it returns a shift-only alignment
/// (identity linear part + median translation), matching silx's
/// fewer-than-3-points-per-DOF fallback; the affine is fitted only at ≥ 18
/// matches.
pub fn sift_auto_align(
    a: &[f32],
    wa: usize,
    ha: usize,
    b: &[f32],
    wb: usize,
    hb: usize,
) -> Option<SiftAlignment> {
    if a.len() != wa * ha || b.len() != wb * hb {
        return None;
    }
    let cw = wa.max(wb);
    let ch = ha.max(hb);
    if cw < 3 || ch < 3 {
        return None;
    }
    let a_pad = pad_top_left(a, wa, ha, cw, ch);
    let b_pad = pad_top_left(b, wb, hb, cw, ch);

    let sift = Sift::default();
    let gray_a = GrayImage::new(cw, ch, normalize01(&a_pad)).ok()?;
    let gray_b = GrayImage::new(cw, ch, normalize01(&b_pad)).ok()?;
    let feats_a = sift.detect_and_compute(&gray_a);
    let feats_b = sift.detect_and_compute(&gray_b);

    // query = A, train = B → each match carries an A index and a B index. silx
    // returns None only for an empty match set (`len_match == 0`).
    let raw = match_features_l1(&feats_a, &feats_b, MATCH_RATIO_THRESHOLD);
    if raw.is_empty() {
        return None;
    }

    let mut matches = Vec::with_capacity(raw.len());
    let mut pairs = Vec::with_capacity(raw.len());
    for &(qi, ti) in &raw {
        let fa = feats_a.get(qi)?;
        let fb = feats_b.get(ti)?;
        matches.push(MatchedKeypoint {
            ax: fa.keypoint.x,
            ay: fa.keypoint.y,
            bx: fb.keypoint.x,
            by: fb.keypoint.y,
            scale: fa.keypoint.scale,
        });
        // silx `matching_correction` fits A (source) → B (target).
        pairs.push((
            (fa.keypoint.x, fa.keypoint.y),
            (fb.keypoint.x, fb.keypoint.y),
        ));
    }

    // silx needs 3 point-correspondences per affine DOF; with fewer matches it
    // registers by a robust median translation (identity linear part) rather
    // than fitting scale/rotation to a handful of noisy pairs
    // (`alignment.py:309-319`). The affine least-squares runs only at ≥ 18.
    let (matrix, offset) = if raw.len() < AFFINE_MIN_MATCHES {
        shift_only_transform(&matches)
    } else {
        let affine = estimate_affine_from_pairs(&pairs).ok()?;
        (
            [
                [affine.m11 as f64, affine.m12 as f64],
                [affine.m21 as f64, affine.m22 as f64],
            ],
            (affine.tx as f64, affine.ty as f64),
        )
    };
    let aligned = warp_affine(&b_pad, cw, ch, matrix, offset);

    Some(SiftAlignment {
        width: cw,
        height: ch,
        aligned,
        matrix,
        offset,
        transformation: decompose_affine(matrix, offset),
        matches,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lowe_sift::{Descriptor, Keypoint};

    /// A [`Feature`] with the given 128-d descriptor and a placeholder keypoint —
    /// `match_features_l1` only reads the descriptor.
    fn feature(descriptor: [f32; 128]) -> Feature {
        Feature {
            keypoint: Keypoint {
                x: 0.0,
                y: 0.0,
                scale: 1.0,
                size: 2.0,
                angle: 0.0,
                response: 1.0,
                octave: 0,
                layer: 0,
            },
            descriptor: Descriptor::new(descriptor),
        }
    }

    /// A descriptor that is `value` at `index` and 0 elsewhere, so its L1 distance
    /// to the all-zero descriptor is exactly `value`.
    fn one_hot(index: usize, value: f32) -> [f32; 128] {
        let mut d = [0.0f32; 128];
        d[index] = value;
        d
    }

    #[test]
    fn match_features_l1_accepts_when_ratio_below_threshold() {
        // Query at the origin; nearest train at L1 0.5, second-nearest at L1 1.0.
        // ratio 0.5 < 0.5329 → accept, pointing at the nearest (train index 0).
        let q = vec![feature([0.0; 128])];
        let train = vec![feature(one_hot(0, 0.5)), feature(one_hot(0, 1.0))];
        let pairs = match_features_l1(&q, &train, MATCH_RATIO_THRESHOLD);
        assert_eq!(pairs, vec![(0, 0)]);
    }

    #[test]
    fn match_features_l1_rejects_when_ratio_at_or_above_threshold() {
        // ratio 0.6 / 1.0 = 0.6 > 0.5329 → ambiguous match, rejected (the old L2
        // gate at 0.8 would have accepted it).
        let q = vec![feature([0.0; 128])];
        let train = vec![feature(one_hot(0, 0.6)), feature(one_hot(0, 1.0))];
        assert!(match_features_l1(&q, &train, MATCH_RATIO_THRESHOLD).is_empty());
    }

    #[test]
    fn match_features_l1_rejects_when_second_neighbour_is_zero_distance() {
        // Two identical train descriptors at distance 0 → dist2 == 0; silx's
        // `dist2 != 0` guard rejects rather than dividing by zero.
        let q = vec![feature([0.0; 128])];
        let train = vec![feature([0.0; 128]), feature([0.0; 128])];
        assert!(match_features_l1(&q, &train, MATCH_RATIO_THRESHOLD).is_empty());
    }

    #[test]
    fn match_features_l1_single_train_descriptor_is_accepted() {
        // A lone train descriptor leaves dist2 at the MAXFLOAT seed, so
        // dist1 / dist2 ≈ 0 accepts it (matching silx's MAXFLOAT-seeded kernel).
        let q = vec![feature([0.0; 128])];
        let train = vec![feature(one_hot(0, 3.0))];
        assert_eq!(
            match_features_l1(&q, &train, MATCH_RATIO_THRESHOLD),
            vec![(0, 0)]
        );
    }

    #[test]
    fn median_odd_length_is_the_middle_element() {
        let mut v = vec![3.0, 1.0, 2.0];
        assert_eq!(median(&mut v), 2.0);
    }

    #[test]
    fn median_even_length_averages_the_two_middle() {
        // numpy.median([1, 2, 3, 4]) = (2 + 3) / 2 = 2.5.
        let mut v = vec![4.0, 1.0, 3.0, 2.0];
        assert_eq!(median(&mut v), 2.5);
    }

    #[test]
    fn median_single_and_empty() {
        assert_eq!(median(&mut [7.0]), 7.0);
        assert_eq!(median(&mut []), 0.0);
    }

    #[test]
    fn shift_only_transform_is_identity_plus_median_translation() {
        // Three pairs with x-deltas {2, 4, 3} and y-deltas {-1, -1, 5}: the
        // shift-only fallback is identity linear part + (median dx, median dy) =
        // (3, -1), independent of the outlier y-delta 5 that a fit would chase.
        let m = |ax, ay, bx, by| MatchedKeypoint {
            ax,
            ay,
            bx,
            by,
            scale: 1.0,
        };
        let matches = [
            m(0.0, 0.0, 2.0, -1.0),
            m(10.0, 10.0, 14.0, 9.0),
            m(5.0, 5.0, 8.0, 10.0),
        ];
        let (matrix, offset) = shift_only_transform(&matches);
        assert_eq!(matrix, [[1.0, 0.0], [0.0, 1.0]]);
        assert_eq!(offset, (3.0, -1.0));
    }

    #[test]
    fn normalize01_maps_min_to_zero_max_to_one() {
        let out = normalize01(&[2.0, 4.0, 6.0]);
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-6);
        assert!((out[2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn normalize01_flat_or_nan_is_all_zero() {
        assert_eq!(normalize01(&[3.0, 3.0, 3.0]), vec![0.0, 0.0, 0.0]);
        let out = normalize01(&[f32::NAN, 1.0, 3.0]);
        assert_eq!(out[0], 0.0); // NaN treated as the minimum → 0
        assert!((out[2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn pad_top_left_anchors_at_origin() {
        let src = [1.0f32, 2.0, 3.0, 4.0]; // 2×2
        let out = pad_top_left(&src, 2, 2, 3, 3);
        #[rustfmt::skip]
        let expected = vec![
            1.0, 2.0, 0.0,
            3.0, 4.0, 0.0,
            0.0, 0.0, 0.0,
        ];
        assert_eq!(out, expected);
    }

    #[test]
    fn decompose_affine_identity() {
        let t = decompose_affine([[1.0, 0.0], [0.0, 1.0]], (5.0, -3.0));
        assert!((t.tx - 5.0).abs() < 1e-12);
        assert!((t.ty + 3.0).abs() < 1e-12);
        assert!((t.sx - 1.0).abs() < 1e-12);
        assert!((t.sy - 1.0).abs() < 1e-12);
        assert!(t.rotation.abs() < 1e-12);
    }

    #[test]
    fn decompose_affine_rotation_and_scale() {
        // 90° rotation scaled by 2: [[0, -2], [2, 0]] → rotation +90°, sx = sy = 2.
        let t = decompose_affine([[0.0, -2.0], [2.0, 0.0]], (0.0, 0.0));
        assert!((t.rotation - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        assert!((t.sx - 2.0).abs() < 1e-9);
        assert!((t.sy - 2.0).abs() < 1e-9);
    }

    #[test]
    fn warp_affine_identity_returns_input() {
        let b = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]; // 3×3
        let out = warp_affine(&b, 3, 3, [[1.0, 0.0], [0.0, 1.0]], (0.0, 0.0));
        assert_eq!(out, b.to_vec());
    }

    #[test]
    fn warp_affine_translation_shifts_and_zero_fills() {
        // Map A(x,y) → B(x+1, y): the output column x reads B column x+1, and the
        // last output column samples outside B → 0.
        let b = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0]; // 3×2 (w=3, h=2)
        let out = warp_affine(&b, 3, 2, [[1.0, 0.0], [0.0, 1.0]], (1.0, 0.0));
        assert_eq!(out, vec![2.0, 3.0, 0.0, 5.0, 6.0, 0.0]);
    }

    /// A sum of Gaussian blobs at distinct positions/scales — the canonical SIFT
    /// (blob-detector) target, deterministic so the test is reproducible.
    fn blob_image(w: usize, h: usize) -> Vec<f32> {
        let blobs = [
            (25.0f32, 30.0, 4.0, 1.0f32),
            (60.0, 25.0, 6.0, 0.9),
            (40.0, 60.0, 5.0, 1.0),
            (70.0, 65.0, 3.0, 0.8),
            (50.0, 45.0, 7.0, 0.7),
            (30.0, 70.0, 4.5, 0.85),
        ];
        let mut img = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut v = 0.0f32;
                for &(cx, cy, sigma, amp) in &blobs {
                    let dx = x as f32 - cx;
                    let dy = y as f32 - cy;
                    v += amp * (-(dx * dx + dy * dy) / (2.0 * sigma * sigma)).exp();
                }
                img[y * w + x] = v;
            }
        }
        img
    }

    /// Shift an image so its content moves by `(+dx, +dy)`: `b(x, y) = a(x-dx, y-dy)`.
    fn shift_image(a: &[f32], w: usize, h: usize, dx: isize, dy: isize) -> Vec<f32> {
        let mut b = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let sx = x as isize - dx;
                let sy = y as isize - dy;
                if sx >= 0 && sy >= 0 && (sx as usize) < w && (sy as usize) < h {
                    b[y * w + x] = a[sy as usize * w + sx as usize];
                }
            }
        }
        b
    }

    #[test]
    fn sift_auto_align_recovers_a_known_translation() {
        let (w, h) = (96usize, 96usize);
        let a = blob_image(w, h);
        // B is A shifted by (+3, +2); a feature at A(kx,ky) reappears at B(kx+3,ky+2).
        let b = shift_image(&a, w, h, 3, 2);

        let result = sift_auto_align(&a, w, h, &b, w, h).expect("alignment");
        assert_eq!((result.width, result.height), (w, h));
        assert!(
            result.matches.len() >= 3,
            "expected >=3 matches, got {}",
            result.matches.len()
        );

        let t = result.transformation;
        // A → B affine recovers the translation, near-identity linear part.
        assert!((t.tx - 3.0).abs() < 1.0, "tx={}", t.tx);
        assert!((t.ty - 2.0).abs() < 1.0, "ty={}", t.ty);
        assert!((t.sx - 1.0).abs() < 0.1, "sx={}", t.sx);
        assert!((t.sy - 1.0).abs() < 0.1, "sy={}", t.sy);
        assert!(t.rotation.abs() < 0.1, "rot={}", t.rotation);

        // Warping B back onto A's grid reproduces A in the interior (away from the
        // zero-filled shift border).
        let mut sum_abs = 0.0f64;
        let mut count = 0usize;
        for y in 10..(h - 10) {
            for x in 10..(w - 10) {
                sum_abs += (result.aligned[y * w + x] - a[y * w + x]).abs() as f64;
                count += 1;
            }
        }
        let mean_abs = sum_abs / count as f64;
        assert!(mean_abs < 0.05, "mean |aligned - A| = {mean_abs}");
    }

    #[test]
    fn sift_auto_align_rejects_too_few_matches() {
        // A nearly flat image yields no usable SIFT keypoints → None.
        let flat = vec![0.5f32; 32 * 32];
        assert!(sift_auto_align(&flat, 32, 32, &flat, 32, 32).is_none());
    }

    #[test]
    fn sift_auto_align_rejects_bad_lengths() {
        assert!(sift_auto_align(&[0.0; 3], 2, 2, &[0.0; 4], 2, 2).is_none());
    }
}
