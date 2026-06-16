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

use lowe_sift::{GrayImage, Sift, estimate_affine_from_pairs, match_features};

/// Lowe distance-ratio threshold for descriptor matching. The 0.8 value is the
/// one recommended in Lowe's paper; silx `MatchPlan` applies an equivalent
/// nearest-neighbour ratio gate.
pub const MATCH_RATIO_THRESHOLD: f32 = 0.8;

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

/// Register image B onto image A by SIFT keypoint matching + affine warp,
/// mirroring silx `CompareImages.__createSiftData`.
///
/// `a`/`b` are row-major scalar images of shape `(wa, ha)`/`(wb, hb)`. Both are
/// first zero-padded (top-left) onto the common grid `max(wa, wb) × max(ha, hb)`
/// — silx pads to the max shape before SIFT — then SIFT-aligned there.
///
/// Returns `None` when registration is not possible: an empty/degenerate image,
/// fewer than three matched keypoints (silx falls back to the default alignment
/// mode), or a singular least-squares system.
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

    // query = A, train = B → each match carries an A index and a B index.
    let raw = match_features(&feats_a, &feats_b, MATCH_RATIO_THRESHOLD);
    if raw.len() < 3 {
        return None;
    }

    let mut matches = Vec::with_capacity(raw.len());
    let mut pairs = Vec::with_capacity(raw.len());
    for m in &raw {
        let fa = feats_a.get(m.query_index)?;
        let fb = feats_b.get(m.train_index)?;
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

    let affine = estimate_affine_from_pairs(&pairs).ok()?;
    let matrix = [
        [affine.m11 as f64, affine.m12 as f64],
        [affine.m21 as f64, affine.m22 as f64],
    ];
    let offset = (affine.tx as f64, affine.ty as f64);
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
