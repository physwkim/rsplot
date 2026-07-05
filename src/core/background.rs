//! Background estimation theories.
//!
//! Ported from silx `silx.math.fit.bgtheories` and the strip/snip filters in
//! `silx.math.fit.filters` (C sources `strip.c` / `snip1d.c`). A background is a
//! low-curvature signal subtracted from the data before fitting peaks. silx
//! exposes these as a separate set of fit theories; here they are pure functions
//! plus a [`Background`] selector that mirrors silx's `THEORY` dict.
//!
//! The whole module is CPU-only and unit-tested headlessly.

use crate::core::fitting::invert_matrix;

/// silx `CONFIG["StripWidth"]` default (operator half-distance, in samples).
pub const DEFAULT_STRIP_WIDTH: usize = 2;
/// silx `CONFIG["StripIterations"]` default.
pub const DEFAULT_STRIP_ITERATIONS: usize = 5000;
/// silx `CONFIG["StripThresholdFactor"]` default.
pub const DEFAULT_STRIP_THRESHOLD_FACTOR: f64 = 1.0;
/// silx `CONFIG["SnipWidth"]` default.
pub const DEFAULT_SNIP_WIDTH: usize = 16;
/// silx `CONFIG["SmoothingWidth"]` default (Savitzky-Golay operator size, in
/// samples).
pub const DEFAULT_SMOOTHING_WIDTH: usize = 5;

/// The strip background filter (silx `filters.strip`, C `strip.c`).
///
/// Iterative peak stripping: at each iteration, every channel `i` (away from the
/// `width`-wide borders) whose value exceeds `factor *` the average of its
/// neighbours `width` samples away, `0.5*(y[i-width] + y[i+width])`, is replaced
/// by that average. Each iteration reads a frozen snapshot of the previous
/// iteration (Jacobi update), exactly like the C `memcpy(input, output)` at the
/// end of each pass. Channels listed in `anchors` (and the channels strictly
/// within `width` of them) are never modified. The first/last `width` channels
/// are left untouched. Returns a copy of `y` unchanged when it is shorter than
/// `2*width + 1` (the C function's `-1` early return).
pub fn strip_background(
    y: &[f64],
    width: usize,
    niterations: usize,
    factor: f64,
    anchors: &[usize],
) -> Vec<f64> {
    let len = y.len();
    let deltai = width.max(1); // C: `if (deltai <= 0) deltai = 1;`
    let mut out = y.to_vec();
    if len < 2 * deltai + 1 {
        return out;
    }
    let mut cur = y.to_vec();
    let near_anchor = |i: usize| -> bool {
        anchors.iter().any(|&a| {
            let (i, a, d) = (i as isize, a as isize, deltai as isize);
            i > a - d && i < a + d
        })
    };
    for _ in 0..niterations {
        for i in deltai..(len - deltai) {
            // Skipped/below-threshold channels keep their value: `out[i]` already
            // equals `cur[i]` (the two buffers are synced at the end of each pass).
            if near_anchor(i) {
                continue;
            }
            let t_mean = 0.5 * (cur[i - deltai] + cur[i + deltai]);
            if cur[i] > t_mean * factor {
                out[i] = t_mean;
            }
        }
        cur.copy_from_slice(&out);
    }
    out
}

/// Simple 3-sample smoothing pass (silx C `smooth1d`, smoothnd.c:53-72),
/// in place over `data`:
/// `ys_i = 0.25·(y_{i−1} + 2·y_i + y_{i+1})` with `0.75/0.25` end weights.
/// The C loop carries the pre-update left neighbour in `prev_sample`, so each
/// output reads original (not already-smoothed) neighbours. Slices shorter
/// than 3 are returned untouched (the C `size < 3` early return) — which makes
/// the edge treatment of [`savitsky_golay`] a no-op for `npoints = 5`.
fn smooth1d_inplace(data: &mut [f64]) {
    let size = data.len();
    if size < 3 {
        return;
    }
    let mut prev = data[0];
    for i in 0..size - 1 {
        let next = 0.25 * (prev + 2.0 * data[i] + data[i + 1]);
        prev = data[i];
        data[i] = next;
    }
    data[size - 1] = 0.25 * prev + 0.75 * data[size - 1];
}

/// Savitzky-Golay smoothing (silx `filters.savitsky_golay`, C `SavitskyGolay`,
/// smoothnd.c:93-149).
///
/// Quadratic/cubic Savitzky-Golay convolution of width `npoints` (promoted to
/// the next odd value when even): coefficients
/// `3·(3m² + 3m − 1 − 5i²)` for offsets `i = −m..m`, `m = npoints/2`,
/// normalised by `(2m−1)(2m+1)(2m+3)`. Before the convolution, the first `m`
/// and last `m` samples (window ending one short of the final sample, as in
/// the C pointer arithmetic) get `npoints/3 + 1` rounds of [`smooth1d_inplace`].
/// A smoothed value is only written where the convolution sum is positive
/// (`dhelp > 0`), so non-positive regions keep their original samples. Returns
/// the input unchanged when `npoints` (after promotion) is below 3, above 101,
/// or longer than the data — the C error path, which still copies the input to
/// the output buffer.
pub fn savitsky_golay(y: &[f64], npoints: usize) -> Vec<f64> {
    let len = y.len();
    let mut output = y.to_vec();
    let npoints = if npoints.is_multiple_of(2) {
        npoints + 1
    } else {
        npoints
    };
    if !(3..=101).contains(&npoints) || len < npoints {
        return output;
    }
    let m = npoints / 2;
    let mi = m as i64;
    let den = ((2 * mi - 1) * (2 * mi + 1) * (2 * mi + 3)) as f64;
    let mut coeff = vec![0.0_f64; npoints];
    for i in 0..=m {
        // 3m² + 3m − 1 − 5i² goes negative near the window ends (e.g. m=2,
        // i=2), so compute in signed arithmetic.
        let ii = i as i64;
        let c = (3 * (3 * mi * mi + 3 * mi - 1 - 5 * ii * ii)) as f64;
        coeff[m + i] = c;
        coeff[m - i] = c;
    }

    // Simple smoothing at both ends (C: npoints/3 + 1 rounds of smooth1d over
    // m samples; the tail window is [len−m−1, len−2]).
    let rounds = npoints / 3 + 1;
    for _ in 0..rounds {
        smooth1d_inplace(&mut output[..m]);
    }
    for _ in 0..rounds {
        smooth1d_inplace(&mut output[len - m - 1..len - 1]);
    }

    // The actual SG convolution in the middle, reading a frozen snapshot taken
    // after the edge smoothing.
    let data = output.clone();
    for i in m..len - m {
        let mut dhelp = 0.0;
        for (j, &c) in coeff.iter().enumerate() {
            dhelp += c * data[i + j - m];
        }
        if dhelp > 0.0 {
            output[i] = dhelp / den;
        }
    }
    output
}

/// The estimation-time strip background with silx defaults
/// (`FitTheories.strip_bg`, fittheories.py:236-251):
/// `strip(savitsky_golay(y, SmoothingWidth), w=StripWidth,
/// niterations=StripIterations, factor=StripThresholdFactor)`, no anchors.
/// `DEFAULT_CONFIG` has `StripBackgroundFlag: True` and `SmoothingFlag: True`
/// (fittheories.py:142-147), so every silx theory estimation subtracts this
/// background before seeding peak heights.
pub fn estimation_strip_bg(y: &[f64]) -> Vec<f64> {
    let smoothed = savitsky_golay(y, DEFAULT_SMOOTHING_WIDTH);
    strip_background(
        &smoothed,
        DEFAULT_STRIP_WIDTH,
        DEFAULT_STRIP_ITERATIONS,
        DEFAULT_STRIP_THRESHOLD_FACTOR,
        &[],
    )
}

/// The SNIP background filter in 1D (silx `filters.snip1d`, C `snip1d.c`).
///
/// For decreasing window `p` from `snip_width` down to `1`, every channel `i`
/// (away from the `p`-wide borders) is replaced by
/// `min(y[i], 0.5*(y[i-p] + y[i+p]))`, using a scratch buffer so the whole pass
/// reads the previous values. Because each step can only lower a channel, the
/// result never exceeds the input. silx's `snip1d` wrapper applies the filter to
/// the raw data (no log-log-square transform), so neither is applied here.
pub fn snip_background(y: &[f64], snip_width: usize) -> Vec<f64> {
    let n = y.len();
    let mut data = y.to_vec();
    if n == 0 || snip_width == 0 {
        return data;
    }
    let mut w = vec![0.0; n];
    for p in (1..=snip_width).rev() {
        if 2 * p >= n {
            continue;
        }
        for i in p..(n - p) {
            w[i] = data[i].min(0.5 * (data[i - p] + data[i + p]));
        }
        data[p..(n - p)].copy_from_slice(&w[p..(n - p)]);
    }
    data
}

/// The SNIP background *theory* (silx `bgtheories.estimate_snip`), as distinct
/// from the raw [`snip_background`] filter above.
///
/// silx's Snip theory does not run [`snip_background`] over the whole array; with
/// its default config (`SmoothingFlag=False`, `AnchorsFlag=False`,
/// `bgtheories.py:78-80`) it uses implicit anchors `[0, len-1]` and snips each
/// inter-anchor segment independently (`bgtheories.py:229-243`):
/// `background[0:n-1] = snip1d(y[0:n-1], w)` for the body and
/// `background[n-1:] = snip1d(y[n-1:], w)` — a length-1 identity — for the tail.
/// Because [`snip_background`]'s descending-`p` passes never touch the first or
/// last sample of *their* sub-array, index `n-2` (the last sample of the body
/// segment) stays raw just like index `n-1` (the identity tail). So a peak
/// abutting the right edge is absorbed into the background exactly as in silx,
/// whereas a single snip over the whole array would strip `n-2`.
///
/// `SmoothingFlag` is `False` by default, so no Savitzky-Golay pre-smoothing is
/// applied here; rsplot exposes no anchor list, so the implicit `[0, len-1]`
/// split is the only case.
pub fn snip_background_theory(y: &[f64], snip_width: usize) -> Vec<f64> {
    let n = y.len();
    let mut bg = y.to_vec();
    if n < 2 {
        // A length-0/1 array is entirely the identity tail (`snip1d` of ≤1
        // sample returns it unchanged).
        return bg;
    }
    // Body segment y[0:n-1]; the length-1 tail y[n-1:] is left at its raw value.
    let body = snip_background(&y[0..n - 1], snip_width);
    bg[0..n - 1].copy_from_slice(&body);
    bg
}

/// Least-squares polynomial fit (silx uses `numpy.polyfit`).
///
/// Returns `degree + 1` coefficients highest-power-first (the `numpy.polyfit` /
/// `numpy.poly1d` convention), solving the normal equations
/// `(VᵀV) c = Vᵀy` over the Vandermonde matrix `V`. Returns `None` when the
/// lengths differ, the data is empty, there are fewer than `degree + 1` points,
/// or the normal-equation matrix is singular. Normal equations are adequate for
/// the low degrees silx uses (2–5); they are more sensitive to conditioning than
/// `numpy.polyfit`'s SVD for high degrees.
pub fn polyfit(x: &[f64], y: &[f64], degree: usize) -> Option<Vec<f64>> {
    let n = x.len();
    if n != y.len() || n == 0 || n < degree + 1 {
        return None;
    }
    let m = degree + 1;
    // Vandermonde row, highest power first: [x^degree, …, x^1, x^0].
    let vrow = |xi: f64| -> Vec<f64> {
        let mut row = vec![0.0; m];
        let mut p = 1.0;
        for k in 0..m {
            row[m - 1 - k] = p;
            p *= xi;
        }
        row
    };
    let mut ata = vec![vec![0.0; m]; m];
    let mut aty = vec![0.0; m];
    for i in 0..n {
        let v = vrow(x[i]);
        for r in 0..m {
            aty[r] += v[r] * y[i];
            for c in 0..m {
                ata[r][c] += v[r] * v[c];
            }
        }
    }
    let inv = invert_matrix(&ata)?;
    let mut coeffs = vec![0.0; m];
    for (r, coeff) in coeffs.iter_mut().enumerate() {
        for c in 0..m {
            *coeff += inv[r][c] * aty[c];
        }
    }
    Some(coeffs)
}

/// Evaluate a polynomial given coefficients highest-power-first (`numpy.poly1d`).
///
/// `poly_eval(&[a, b, c], x) = a*x^2 + b*x + c` via Horner's method. Empty
/// coefficients evaluate to zero everywhere.
pub fn poly_eval(coeffs: &[f64], x: &[f64]) -> Vec<f64> {
    x.iter()
        .map(|&xi| coeffs.iter().fold(0.0, |acc, &c| acc * xi + c))
        .collect()
}

/// A selectable background theory (silx `bgtheories.THEORY`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Background {
    /// No background (silx "No Background"): zero everywhere.
    None,
    /// Constant background (silx "Constant"): `min(y)`.
    Constant,
    /// Linear background (silx "Linear"): a least-squares line fitted to the
    /// strip background of the data.
    Linear,
    /// Strip filter background (silx "Strip"): see [`strip_background`].
    Strip {
        /// Operator half-distance in samples.
        width: usize,
        /// Number of stripping iterations.
        niterations: usize,
        /// Threshold scaling factor.
        factor: f64,
    },
    /// SNIP filter background (silx "Snip" theory): see
    /// [`snip_background_theory`] (the default-anchor segment split, not the raw
    /// [`snip_background`] filter).
    Snip {
        /// Snip operator width in samples.
        width: usize,
    },
    /// Polynomial background (silx "Internal" poly theories): a least-squares
    /// polynomial of `degree` fitted to the strip background of the data
    /// (silx `EstimatePolyOnStrip = True`).
    Polynomial {
        /// Polynomial degree (silx ships 2–5).
        degree: usize,
    },
}

impl Background {
    /// A strip background with silx's default parameters.
    pub fn strip() -> Self {
        Background::Strip {
            width: DEFAULT_STRIP_WIDTH,
            niterations: DEFAULT_STRIP_ITERATIONS,
            factor: DEFAULT_STRIP_THRESHOLD_FACTOR,
        }
    }

    /// A SNIP background with silx's default width.
    pub fn snip() -> Self {
        Background::Snip {
            width: DEFAULT_SNIP_WIDTH,
        }
    }

    /// Display name (silx theory name).
    pub fn name(self) -> &'static str {
        match self {
            Background::None => "No Background",
            Background::Constant => "Constant",
            Background::Linear => "Linear",
            Background::Strip { .. } => "Strip",
            Background::Snip { .. } => "Snip",
            Background::Polynomial { .. } => "Polynomial",
        }
    }

    /// Compute the background curve sampled at `x` for the data `y`.
    ///
    /// `x` is used only by the `Linear` / `Polynomial` theories (which fit a
    /// curve in `x`); the strip/snip/constant theories ignore it. Falls back to
    /// zeros when a polynomial fit is not solvable (e.g. mismatched lengths).
    pub fn compute(self, x: &[f64], y: &[f64]) -> Vec<f64> {
        let n = y.len();
        match self {
            Background::None => vec![0.0; n],
            Background::Constant => {
                let c = y.iter().copied().fold(f64::INFINITY, f64::min);
                vec![if c.is_finite() { c } else { 0.0 }; n]
            }
            Background::Linear => self.poly_on_strip(x, y, 1),
            Background::Strip {
                width,
                niterations,
                factor,
            } => strip_background(y, width, niterations, factor, &[]),
            Background::Snip { width } => snip_background_theory(y, width),
            Background::Polynomial { degree } => self.poly_on_strip(x, y, degree),
        }
    }

    /// Subtract the background from `y`, returning `y - background(x, y)`.
    pub fn subtract(self, x: &[f64], y: &[f64]) -> Vec<f64> {
        let bg = self.compute(x, y);
        y.iter().zip(bg).map(|(&yi, bi)| yi - bi).collect()
    }

    /// silx `estimate_poly`: strip the data, then least-squares-fit a polynomial
    /// of `degree` over the strip background and evaluate it at `x`.
    fn poly_on_strip(self, x: &[f64], y: &[f64], degree: usize) -> Vec<f64> {
        let bg = strip_background(
            y,
            DEFAULT_STRIP_WIDTH,
            DEFAULT_STRIP_ITERATIONS,
            DEFAULT_STRIP_THRESHOLD_FACTOR,
            &[],
        );
        match polyfit(x, &bg, degree) {
            Some(coeffs) => poly_eval(&coeffs, x),
            None => vec![0.0; y.len()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linspace(a: f64, b: f64, n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| a + (b - a) * (i as f64) / ((n - 1) as f64))
            .collect()
    }

    /// The R2-29 Savitzky-Golay fixture: `sin(0.3·i)·10 + 0.05·i² − 3·[i==7]`
    /// over 16 samples, and its goldens from silx's own C `SavitskyGolay`
    /// (smoothnd.c compiled directly and driven over this array).
    fn sg_fixture() -> Vec<f64> {
        (0..16)
            .map(|i| {
                (0.3 * i as f64).sin() * 10.0 + 0.05 * (i * i) as f64
                    - if i == 7 { 3.0 } else { 0.0 }
            })
            .collect()
    }

    #[test]
    fn savitsky_golay_matches_the_silx_c_filter_npoints_5() {
        // npoints=5 (the strip_bg default): m=2, so the edge treatment is a
        // no-op (smooth1d returns for size < 3) and the first/last 2 samples
        // pass through.
        let golden = [
            0.0,
            3.0052020666133954,
            5.842562910079942,
            8.277911598919477,
            10.114016258114743,
            11.47527044159569,
            10.50324433268838,
            9.619046962762674,
            8.921440604327952,
            8.578018631368701,
            6.410234902381666,
            4.473621946741542,
            2.7778221477491485,
            1.577042325941772,
            1.0842422758641188,
            1.47469882334903,
        ];
        let y = sg_fixture();
        let out = savitsky_golay(&y, 5);
        for (i, (&o, &g)) in out.iter().zip(golden.iter()).enumerate() {
            assert!((o - g).abs() < 1e-12, "sample {i}: {o} vs {g}");
        }
        // Even npoints promotes to the next odd (C `npoints += 1`).
        assert_eq!(savitsky_golay(&y, 4), out);
    }

    #[test]
    fn savitsky_golay_matches_the_silx_c_filter_npoints_7() {
        // npoints=7: m=3, so the edge smoothing is ACTIVE (3 rounds of
        // smooth1d over 3-sample windows at each end) — this golden pins the
        // edge-treatment path, including the tail window stopping one short
        // of the final sample.
        let golden = [
            1.7168850198513144,
            2.9513963262258147,
            4.18334545448662,
            7.6106484513528425,
            10.142406433141652,
            10.921332659109693,
            10.648691788332117,
            10.053160403608413,
            9.074848608117552,
            7.938604770945253,
            6.583306858301657,
            4.30283019002851,
            2.707708907459852,
            1.8067381200117714,
            1.4557179806722114,
            1.47469882334903,
        ];
        let y = sg_fixture();
        let out = savitsky_golay(&y, 7);
        for (i, (&o, &g)) in out.iter().zip(golden.iter()).enumerate() {
            assert!((o - g).abs() < 1e-12, "sample {i}: {o} vs {g}");
        }
    }

    #[test]
    fn savitsky_golay_positive_sum_guard_keeps_negative_data() {
        // The C filter only writes a smoothed value where the convolution sum
        // is positive — an all-negative curve passes through unchanged
        // (verified against the compiled C filter).
        let y = vec![-1.0, -2.0, -3.0, -2.5, -1.5, -2.0, -3.0, -2.0, -1.0, -2.0];
        assert_eq!(savitsky_golay(&y, 5), y);
    }

    #[test]
    fn savitsky_golay_short_or_invalid_width_returns_the_input() {
        // len < npoints and npoints < 3 are the C error paths: the output is
        // the unsmoothed copy.
        let y = vec![1.0, 2.0, 3.0];
        assert_eq!(savitsky_golay(&y, 5), y);
        let y2 = sg_fixture();
        assert_eq!(savitsky_golay(&y2, 1), y2);
        assert_eq!(savitsky_golay(&y2, 103), y2);
    }

    #[test]
    fn estimation_strip_bg_recovers_a_flat_baseline_under_a_peak() {
        // A gaussian on a constant baseline: the default strip background
        // (savitsky_golay(5) then strip(w=2, n=5000, factor=1)) erodes the
        // peak down to the baseline, so y − bg isolates the peak.
        let x = linspace(0.0, 40.0, 81);
        let y: Vec<f64> = x
            .iter()
            .map(|&xi| 100.0 + 10.0 * (-((xi - 20.0) / 2.0).powi(2)).exp())
            .collect();
        let bg = estimation_strip_bg(&y);
        // At the peak centre the background is the baseline, not the peak.
        assert!((bg[40] - 100.0).abs() < 1.0, "bg at peak {}", bg[40]);
        // Away from the peak the background tracks the data.
        assert!((bg[5] - y[5]).abs() < 0.5);
    }

    #[test]
    fn strip_removes_a_spike_and_keeps_flat_regions() {
        let mut y = vec![1.0; 41];
        y[20] = 11.0;
        let bg = strip_background(&y, 1, 100, 1.0, &[]);
        // Spike clipped to the neighbour average (1.0); flat regions unchanged.
        assert!(
            (bg[20] - 1.0).abs() < 1e-9,
            "spike not stripped: {}",
            bg[20]
        );
        assert!((bg[5] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn strip_leaves_the_borders_untouched() {
        let mut y = vec![1.0; 21];
        y[0] = 7.0;
        y[20] = 9.0;
        let bg = strip_background(&y, 2, 50, 1.0, &[]);
        // The first/last `width` channels are never modified.
        assert_eq!(bg[0], 7.0);
        assert_eq!(bg[20], 9.0);
    }

    #[test]
    fn strip_anchor_preserves_the_anchored_channel() {
        let mut y = vec![1.0; 41];
        y[20] = 11.0;
        // Anchoring index 20 keeps it (it is within `width` of the anchor).
        let bg = strip_background(&y, 1, 100, 1.0, &[20]);
        assert!((bg[20] - 11.0).abs() < 1e-12, "anchor not preserved");
    }

    #[test]
    fn strip_too_short_returns_input_copy() {
        let y = vec![3.0, 9.0, 3.0];
        // len 3 < 2*width+1 = 5 → unchanged.
        assert_eq!(strip_background(&y, 2, 10, 1.0, &[]), y);
    }

    #[test]
    fn strip_preserves_a_linear_ramp() {
        // A pure ramp has y[i] == mean(y[i-w], y[i+w]); never strictly greater,
        // so the strip leaves it untouched.
        let y: Vec<f64> = (0..30).map(|i| 2.0 + 0.5 * i as f64).collect();
        let bg = strip_background(&y, 2, 100, 1.0, &[]);
        for (a, b) in bg.iter().zip(&y) {
            assert!((a - b).abs() < 1e-9);
        }
    }

    #[test]
    fn snip_clips_a_spike_and_never_exceeds_input() {
        let mut y = vec![1.0; 41];
        y[20] = 11.0;
        let bg = snip_background(&y, 8);
        assert!(bg.iter().zip(&y).all(|(&b, &yi)| b <= yi + 1e-9));
        assert!((bg[20] - 1.0).abs() < 1e-9, "spike not snipped: {}", bg[20]);
    }

    #[test]
    fn snip_theory_leaves_last_two_samples_raw_like_silx_anchors() {
        // R2-43: silx `bgtheories.estimate_snip` with default anchors [0, n-1]
        // snips y[0:n-1] and leaves y[n-1:] identity; because snip1d never touches
        // the last sample of its sub-array, index n-2 (last of the body segment)
        // and n-1 (the identity tail) both stay raw. A right-edge peak at n-2 is
        // therefore absorbed into the background, whereas a whole-array snip strips
        // it.
        let y = vec![1.0, 1.0, 1.0, 1.0, 100.0, 1.0]; // n=6, peak at index 4 = n-2
        let n = y.len();
        let theory = snip_background_theory(&y, 2);
        assert_eq!(theory[n - 2], 100.0, "n-2 peak must stay raw in background");
        assert_eq!(theory[n - 1], 1.0, "n-1 identity tail must stay raw");
        // The interior is still snipped down.
        assert!(theory[2] <= 1.0 + 1e-9);
        // A single snip over the whole array strips the n-2 peak — the divergence.
        let full = snip_background(&y, 2);
        assert_eq!(full[n - 2], 1.0, "whole-array snip strips the n-2 peak");
    }

    #[test]
    fn snip_theory_short_arrays_are_identity() {
        // ≤1-sample arrays are entirely the identity tail.
        assert_eq!(snip_background_theory(&[], 4), Vec::<f64>::new());
        assert_eq!(snip_background_theory(&[7.0], 4), vec![7.0]);
    }

    #[test]
    fn polyfit_recovers_known_quadratic() {
        let x = linspace(-3.0, 3.0, 13);
        let y: Vec<f64> = x.iter().map(|&xi| 2.0 + 3.0 * xi + xi * xi).collect();
        let c = polyfit(&x, &y, 2).unwrap();
        assert!((c[0] - 1.0).abs() < 1e-6, "x^2 coeff {}", c[0]);
        assert!((c[1] - 3.0).abs() < 1e-6, "x^1 coeff {}", c[1]);
        assert!((c[2] - 2.0).abs() < 1e-6, "x^0 coeff {}", c[2]);
        for (a, b) in poly_eval(&c, &x).iter().zip(&y) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn polyfit_needs_enough_points() {
        assert!(polyfit(&[1.0, 2.0], &[1.0, 2.0], 2).is_none());
        assert!(polyfit(&[1.0, 2.0], &[1.0], 1).is_none());
    }

    #[test]
    fn poly_eval_uses_horner_highest_first() {
        // a*x^2 + b*x + c with [1, 3, 2] at x=2 = 4 + 6 + 2 = 12.
        assert_eq!(poly_eval(&[1.0, 3.0, 2.0], &[2.0]), vec![12.0]);
        // Empty coefficients → zero.
        assert_eq!(poly_eval(&[], &[5.0, -1.0]), vec![0.0, 0.0]);
    }

    #[test]
    fn background_none_and_constant() {
        let y = vec![4.0, 2.0, 9.0, 3.0];
        assert_eq!(Background::None.compute(&[], &y), vec![0.0; 4]);
        assert_eq!(Background::Constant.compute(&[], &y), vec![2.0; 4]);
    }

    #[test]
    fn background_linear_recovers_trend_under_a_peak() {
        let x = linspace(0.0, 40.0, 41);
        let mut y: Vec<f64> = x.iter().map(|&xi| 1.0 + 0.1 * xi).collect();
        y[20] += 10.0; // a peak on top of the linear background
        let bg = Background::Linear.compute(&x, &y);
        assert!(
            (bg[10] - (1.0 + 0.1 * 10.0)).abs() < 0.2,
            "bg[10] {}",
            bg[10]
        );
        assert!(
            (bg[30] - (1.0 + 0.1 * 30.0)).abs() < 0.2,
            "bg[30] {}",
            bg[30]
        );
        // Subtraction leaves the peak in the residual.
        let resid = Background::Linear.subtract(&x, &y);
        assert!(resid[20] > 5.0, "peak not retained: {}", resid[20]);
    }

    #[test]
    fn background_strip_and_snip_constructors_use_silx_defaults() {
        assert_eq!(
            Background::strip(),
            Background::Strip {
                width: 2,
                niterations: 5000,
                factor: 1.0,
            }
        );
        assert_eq!(Background::snip(), Background::Snip { width: 16 });
        assert_eq!(Background::strip().name(), "Strip");
        assert_eq!(Background::snip().name(), "Snip");
    }

    #[test]
    fn background_strip_subtract_isolates_a_peak() {
        let mut y = vec![5.0; 51];
        y[25] = 25.0;
        let resid = Background::strip().subtract(&[], &y);
        // The flat background is removed; the peak survives.
        assert!(resid[25] > 15.0, "peak residual {}", resid[25]);
        assert!(resid[5].abs() < 1e-6, "flat residual {}", resid[5]);
    }
}
