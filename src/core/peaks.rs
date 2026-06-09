//! Peak search.
//!
//! Ported from silx `silx.math.fit.peaks` (C source `peaks/src/peaks.c`,
//! function `seek`, and the Python `guess_fwhm`). [`peak_search`] convolves the
//! data with the second derivative of a Gaussian to smooth it, then walks the
//! smoothed significance signal with a small state machine to locate peaks whose
//! significance rises above `sensitivity`. [`guess_fwhm`] estimates a typical
//! peak width to feed the search.
//!
//! Pure / CPU-only and unit-tested headlessly.

use crate::core::background::strip_background;

/// silx `peak_search` default `sensitivity` (3.5).
pub const DEFAULT_PEAK_SENSITIVITY: f64 = 3.5;

/// A peak located by [`peak_search`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peak {
    /// Sample index of the peak in the input array.
    pub index: usize,
    /// Peak relevance (the smoothed significance at the peak, silx
    /// `relevances`).
    pub relevance: f64,
}

/// Search the whole array for peaks (silx `peak_search(y, fwhm, sensitivity)`).
pub fn peak_search(y: &[f64], fwhm: f64, sensitivity: f64) -> Vec<Peak> {
    peak_search_range(y, fwhm, sensitivity, 0, y.len().saturating_sub(1))
}

/// Search `y[begin_index..=end_index]` for peaks (silx C `seek`).
///
/// `fwhm` is the expected peak full-width at half maximum (in samples) used for
/// the Gaussian smoothing; `sensitivity` is the significance threshold (a peak
/// must exceed `sensitivity` standard deviations). Returns the located peaks in
/// ascending index order; empty when the data is too short or no peak clears the
/// threshold.
pub fn peak_search_range(
    y: &[f64],
    fwhm: f64,
    sensitivity: f64,
    begin_index: usize,
    end_index: usize,
) -> Vec<Peak> {
    let nsamples = y.len();
    if nsamples < 2 || fwhm <= 0.0 || begin_index >= nsamples {
        return Vec::new();
    }
    // seek() mutates `data[0] = data[1]` before the main loop; work on a copy.
    let mut data = y.to_vec();

    // silx uses the literal 2.35482 (≈ 2*sqrt(2*ln2)) for fwhm→sigma.
    let sigma = fwhm / 2.35482;
    let sigma2 = sigma * sigma;
    let sigma4 = sigma2 * sigma2;
    let lowthreshold = 0.01 / sigma2;

    // Gaussian second-derivative convolution factors, until the low threshold.
    let max_gfactor = 100usize;
    let span = end_index as isize - begin_index as isize - 2;
    let max_cfac = {
        let m = (span / 2) - 1;
        if m < 0 {
            0
        } else {
            (m as usize).min(max_gfactor)
        }
    };
    let mut gfactor: Vec<f64> = Vec::with_capacity(max_gfactor);
    for cfac in 0..max_cfac {
        let cfac2 = ((cfac + 1) * (cfac + 1)) as f64;
        let g = (sigma2 - cfac2) * (-cfac2 / (sigma2 * 2.0)).exp() / sigma4;
        gfactor.push(g);
        if g < lowthreshold && g > -lowthreshold {
            break;
        }
    }
    let nr_factor = gfactor.len();
    if nr_factor == 0 {
        return Vec::new();
    }
    // (silx computes `lld`/`channel1` here, but the main loop never uses them.)

    let clamp = |i: isize| -> usize {
        if i < 0 {
            0
        } else if i as usize >= nsamples {
            nsamples - 1
        } else {
            i as usize
        }
    };

    // Initial smoothed significance at cch = begin_index.
    let mut cch = begin_index;
    let mut nom = data[cch] / sigma2;
    let mut den2 = data[cch] / sigma4;
    for (cfac, &g) in gfactor.iter().enumerate() {
        let i1 = clamp(cch as isize - cfac as isize);
        let i2 = clamp(cch as isize + cfac as isize);
        nom += g * (data[i2] + data[i1]);
        den2 += g * g * (data[i2] + data[i1]);
    }
    let mut data2_1 = if den2 <= 0.0 { 0.0 } else { nom / den2.sqrt() };
    data[0] = data[1];

    let mut peaks: Vec<Peak> = Vec::new();
    let mut peakstarted = 0u8;
    let limit = end_index.min(nsamples - 2);
    while cch <= limit {
        let data2_0 = data2_1;
        cch += 1;
        nom = data[cch] / sigma2;
        den2 = data[cch] / sigma4;
        // Note the silx off-by-one vs. the initial pass: the loop runs cfac in
        // 1..nr_factor but weights with gfactor[cfac-1].
        for cfac in 1..nr_factor {
            let i1 = clamp(cch as isize - cfac as isize);
            let i2 = clamp(cch as isize + cfac as isize);
            let g = gfactor[cfac - 1];
            nom += g * (data[i2] + data[i1]);
            den2 += g * g * (data[i2] + data[i1]);
        }
        data2_1 = if den2 <= 0.0 { 0.0 } else { nom / den2.sqrt() };

        if data2_1 > sensitivity {
            if peakstarted == 0 && data2_1 > data2_0 {
                peakstarted = 1;
            }
            if peakstarted == 1 && data2_1 < data2_0 {
                // Just past the top of a peak: record the previous channel.
                peaks.push(Peak {
                    index: cch - 1,
                    relevance: data2_0,
                });
                peakstarted = 2;
            }
            if peakstarted == 2 {
                let last = peaks[peaks.len() - 1].index;
                if (cch as f64 - last as f64) > 0.6 * fwhm && data2_1 > data2_0 {
                    // Far enough past the last peak and rising again: a doublet.
                    peakstarted = 1;
                }
            }
        } else {
            peakstarted = 0;
        }
    }
    peaks
}

/// Estimate the FWHM (in samples) of the largest peak (silx `guess_fwhm`).
///
/// Removes a strip background, finds the global maximum of the residual, and
/// measures the half-maximum width around it. Floored at silx's minimum of `4`;
/// returns `0` for empty input.
pub fn guess_fwhm(y: &[f64]) -> f64 {
    const FWHM_MIN: f64 = 4.0;
    if y.is_empty() {
        return 0.0;
    }
    let background = strip_background(y, 1, 1000, 1.0, &[]);
    let yfit: Vec<f64> = y.iter().zip(&background).map(|(&yi, &b)| yi - b).collect();
    let maximum = yfit.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    // silx takes the last index that equals the maximum.
    let posindex = match yfit.iter().rposition(|&v| v == maximum) {
        Some(p) => p,
        None => return 0.0,
    };
    let height = yfit[posindex];
    let mut imin = posindex;
    while yfit[imin] > 0.5 * height && imin > 0 {
        imin -= 1;
    }
    let mut imax = posindex;
    while yfit[imax] > 0.5 * height && imax < yfit.len() - 1 {
        imax += 1;
    }
    let fwhm = imax as isize - imin as isize - 1;
    (fwhm as f64).max(FWHM_MIN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::fitting::gaussian_model;

    fn gauss(center: f64, fwhm: f64, height: f64, n: usize) -> Vec<f64> {
        let x: Vec<f64> = (0..n).map(|i| i as f64).collect();
        gaussian_model(&x, &[height, center, fwhm, 0.0])
    }

    #[test]
    fn finds_a_single_peak_near_its_center() {
        let y = gauss(50.0, 8.0, 100.0, 100);
        let peaks = peak_search(&y, 8.0, DEFAULT_PEAK_SENSITIVITY);
        assert!(!peaks.is_empty(), "no peak found");
        // Some located peak sits within a couple of samples of the true centre.
        assert!(
            peaks.iter().any(|p| (p.index as isize - 50).abs() <= 3),
            "peaks {:?} not near 50",
            peaks
        );
        assert!(peaks.iter().all(|p| p.relevance > 0.0));
    }

    #[test]
    fn finds_two_separated_peaks() {
        let mut y = gauss(30.0, 8.0, 100.0, 100);
        for (yi, g) in y.iter_mut().zip(gauss(70.0, 8.0, 100.0, 100)) {
            *yi += g;
        }
        let peaks = peak_search(&y, 8.0, DEFAULT_PEAK_SENSITIVITY);
        assert!(peaks.len() >= 2, "expected >=2 peaks, got {:?}", peaks);
        assert!(peaks.iter().any(|p| (p.index as isize - 30).abs() <= 4));
        assert!(peaks.iter().any(|p| (p.index as isize - 70).abs() <= 4));
    }

    #[test]
    fn flat_data_has_no_peaks() {
        let y = vec![5.0; 100];
        assert!(peak_search(&y, 8.0, DEFAULT_PEAK_SENSITIVITY).is_empty());
    }

    #[test]
    fn higher_sensitivity_finds_no_more_peaks() {
        let y = gauss(50.0, 8.0, 100.0, 100);
        let low = peak_search(&y, 8.0, 2.0).len();
        let high = peak_search(&y, 8.0, 50.0).len();
        assert!(high <= low, "high {high} should not exceed low {low}");
    }

    #[test]
    fn peak_indices_are_in_bounds() {
        let y = gauss(50.0, 8.0, 100.0, 100);
        let peaks = peak_search(&y, 8.0, DEFAULT_PEAK_SENSITIVITY);
        assert!(peaks.iter().all(|p| p.index < y.len()));
    }

    #[test]
    fn guess_fwhm_recovers_a_known_width() {
        let y = gauss(50.0, 8.0, 100.0, 100);
        let f = guess_fwhm(&y);
        assert!((f - 8.0).abs() <= 3.0, "guessed fwhm {f}");
        assert!(f >= 4.0);
    }

    #[test]
    fn guess_fwhm_floor_and_empty() {
        assert_eq!(guess_fwhm(&[]), 0.0);
        // A 1-sample-wide spike is floored to the silx minimum of 4.
        let mut y = vec![0.0; 50];
        y[25] = 100.0;
        assert_eq!(guess_fwhm(&y), 4.0);
    }
}
