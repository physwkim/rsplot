//! Basic curve fitting utilities.
//!
//! Provides traits and simple implementations for curve fitting (Linear, Gaussian estimation).
//!
//! Additionally provides an iterative Levenberg-Marquardt least-squares solver
//! ([`leastsq`]) ported from silx `silx/math/fit/leastsq.py`, together with the
//! peak models (Gaussian/Lorentzian/PseudoVoigt) from
//! `silx/math/fit/functions/src/funs.c` and their initial-parameter estimators
//! mirroring `silx/math/fit/fittheories.py`.

/// Result of a curve fit.
#[derive(Debug, Clone)]
pub struct FitResult {
    /// The fitted y values for the input x values.
    pub y_fit: Vec<f64>,
    /// The parameters of the fit function.
    pub parameters: Vec<f64>,
    /// Names of the parameters.
    pub param_names: Vec<String>,
}

/// A function that can be fitted to data.
pub trait FitFunction {
    /// Name of the function.
    fn name(&self) -> &str;

    /// Fit the function to the given data.
    fn fit(&self, x: &[f64], y: &[f64]) -> Option<FitResult>;
}

/// Simple linear fit: y = m*x + c
pub struct LinearFit;

impl FitFunction for LinearFit {
    fn name(&self) -> &str {
        "Linear"
    }

    fn fit(&self, x: &[f64], y: &[f64]) -> Option<FitResult> {
        if x.len() != y.len() || x.len() < 2 {
            return None;
        }
        let n = x.len() as f64;
        let sum_x: f64 = x.iter().sum();
        let sum_y: f64 = y.iter().sum();
        let sum_xy: f64 = x.iter().zip(y.iter()).map(|(&xi, &yi)| xi * yi).sum();
        let sum_xx: f64 = x.iter().map(|&xi| xi * xi).sum();

        let denominator = n * sum_xx - sum_x * sum_x;
        if denominator.abs() < 1e-12 {
            return None;
        }

        let m = (n * sum_xy - sum_x * sum_y) / denominator;
        let c = (sum_y - m * sum_x) / n;

        let y_fit = x.iter().map(|&xi| m * xi + c).collect();

        Some(FitResult {
            y_fit,
            parameters: vec![m, c],
            param_names: vec!["Slope (m)".to_string(), "Intercept (c)".to_string()],
        })
    }
}

/// Gaussian estimation: y = A * exp(-(x - mu)^2 / (2 * sigma^2)) + bg
/// Note: This is a direct analytical estimation based on moments/peak, not an iterative L-M fit.
pub struct GaussianEstimateFit;

impl FitFunction for GaussianEstimateFit {
    fn name(&self) -> &str {
        "Gaussian (Estimate)"
    }

    fn fit(&self, x: &[f64], y: &[f64]) -> Option<FitResult> {
        if x.len() != y.len() || x.len() < 3 {
            return None;
        }

        let bg = y.iter().copied().fold(f64::INFINITY, f64::min);
        let mut max_y = f64::NEG_INFINITY;
        let mut max_idx = 0;
        for (i, &yi) in y.iter().enumerate() {
            if yi > max_y {
                max_y = yi;
                max_idx = i;
            }
        }

        let a = max_y - bg;
        let mu = x[max_idx];

        // Estimate FWHM by finding first points below half max
        let half_max = bg + a / 2.0;
        let mut left_idx = max_idx;
        while left_idx > 0 && y[left_idx] > half_max {
            left_idx -= 1;
        }
        let mut right_idx = max_idx;
        while right_idx < y.len() - 1 && y[right_idx] > half_max {
            right_idx += 1;
        }

        let fwhm = x[right_idx] - x[left_idx];
        let sigma = if fwhm > 0.0 {
            fwhm / 2.355
        } else {
            (x.last().unwrap() - x.first().unwrap()) / 4.0
        };

        let y_fit = x
            .iter()
            .map(|&xi| {
                let z = (xi - mu) / sigma;
                a * (-0.5 * z * z).exp() + bg
            })
            .collect();

        Some(FitResult {
            y_fit,
            parameters: vec![a, mu, sigma, bg],
            param_names: vec![
                "Amplitude (A)".to_string(),
                "Center (mu)".to_string(),
                "Sigma".to_string(),
                "Background".to_string(),
            ],
        })
    }
}

// ---------------------------------------------------------------------------
// Iterative Levenberg-Marquardt least-squares core.
//
// Ported from silx `silx/math/fit/leastsq.py` (leastsq / chisq_alpha_beta),
// itself a refactor of PyMca Gefit. We port the *unconstrained* path: silx's
// CFREE branch where `n_free == nparameters`, `noigno == range(n)`, and
// `derivfactor == 1`. Constraints (positivity/quoted/factor/...) are DEFERRED.
// ---------------------------------------------------------------------------

/// `LOG2`, matching the C constant in `funs.c`
/// (`#define LOG2 0.69314718055994529`, i.e. `ln(2)`). Used to convert FWHM to
/// sigma: `sigma = fwhm / (2 * sqrt(2 * LOG2))`.
pub const LOG2: f64 = std::f64::consts::LN_2;

/// `2 * sqrt(2 * LOG2)`: the FWHM/sigma conversion factor for a Gaussian.
/// silx computes `inv_two_sqrt_two_log2 = 1 / (2*sqrt(2*LOG2))` and uses
/// `sigma = fwhm * inv_two_sqrt_two_log2`.
pub fn fwhm_to_sigma_factor() -> f64 {
    2.0 * (2.0 * LOG2).sqrt()
}

/// Outputs of a successful [`leastsq`] run.
///
/// Mirrors the silx `leastsq` return tuple (`fittedpar`, `cov`, `ddict`) with
/// the unconstrained-only subset of `ddict` we need: `chisq`, `reduced_chisq`,
/// `niter`, `nfev`.
#[derive(Debug, Clone)]
pub struct LeastSqResult {
    /// Optimal parameter values minimising the weighted sum of squared
    /// residuals (silx `fittedpar`).
    pub parameters: Vec<f64>,
    /// Estimated covariance matrix of the parameters, row-major
    /// `n_param x n_param` (silx `cov0 = inv(alpha0)`). Standard errors are the
    /// square roots of the diagonal: `perr[i] = sqrt(cov[i][i])`.
    pub covariance: Vec<Vec<f64>>,
    /// The chi-square `sum( weight * (model - y)^2 )` at the optimum
    /// (silx `chisq0`).
    pub chisq: f64,
    /// Reduced chi-square `chisq / (M - n_free)` where `M` is the number of
    /// data points and `n_free` the number of fitted parameters (silx
    /// `reduced_chisq`). `None` when degrees of freedom are non-positive.
    pub reduced_chisq: Option<f64>,
    /// Number of iterations performed (silx `niter`).
    pub niter: usize,
    /// Number of model function evaluations (silx `nfev`).
    pub nfev: usize,
}

impl LeastSqResult {
    /// Per-parameter standard error: `sqrt(abs(cov[i][i]))`.
    ///
    /// Mirrors the silx docstring note "To compute one standard deviation
    /// errors use `perr = np.sqrt(np.diag(pcov))`"; `abs` guards a tiny
    /// negative diagonal from round-off, matching silx `sqrt(abs(diag(cov0)))`.
    pub fn std_errors(&self) -> Vec<f64> {
        (0..self.parameters.len())
            .map(|i| self.covariance[i][i].abs().sqrt())
            .collect()
    }
}

/// Why a [`leastsq`] call could not run / converge to a covariance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FitError {
    /// `xdata` and `ydata` have different lengths.
    LengthMismatch,
    /// There are no free parameters (silx `raise ValueError("No free
    /// parameters to fit")`).
    NoFreeParameters,
    /// Fewer data points than free parameters: the problem is under-determined.
    NotEnoughData,
    /// A non-finite value (NaN/inf) was found in inputs while `check_finite`
    /// is on (silx `asarray_chkfinite`).
    NonFinite,
    /// The curvature matrix `alpha0` is singular and cannot be inverted, so no
    /// covariance is available (silx `LinAlgError` from `inv(alpha0)`).
    SingularMatrix,
}

/// Invert a square row-major matrix via Gauss-Jordan elimination with partial
/// pivoting. Returns `None` if the matrix is singular.
///
/// This stands in for numpy's `numpy.linalg.inv` used by silx `leastsq` (for
/// `inv(alpha)` in the LM step and `inv(alpha0)` for the covariance).
pub fn invert_matrix(m: &[Vec<f64>]) -> Option<Vec<Vec<f64>>> {
    let n = m.len();
    if n == 0 {
        return Some(Vec::new());
    }
    // Augment [ m | I ].
    let mut a: Vec<Vec<f64>> = Vec::with_capacity(n);
    for (i, row) in m.iter().enumerate() {
        if row.len() != n {
            return None;
        }
        let mut aug = row.clone();
        aug.extend((0..n).map(|j| if i == j { 1.0 } else { 0.0 }));
        a.push(aug);
    }
    for col in 0..n {
        // Partial pivot: largest magnitude in this column at/below the diagonal.
        let mut pivot = col;
        let mut best = a[col][col].abs();
        for (r, row) in a.iter().enumerate().skip(col + 1) {
            let v = row[col].abs();
            if v > best {
                best = v;
                pivot = r;
            }
        }
        if best == 0.0 {
            return None; // singular
        }
        a.swap(col, pivot);
        let pivot_val = a[col][col];
        for v in a[col].iter_mut() {
            *v /= pivot_val;
        }
        let pivot_row = a[col].clone();
        for (r, row) in a.iter_mut().enumerate() {
            if r == col {
                continue;
            }
            let factor = row[col];
            if factor != 0.0 {
                for (cell, &pv) in row.iter_mut().zip(pivot_row.iter()) {
                    *cell -= factor * pv;
                }
            }
        }
    }
    // Extract the right half.
    let inv = a
        .into_iter()
        .map(|row| row[n..].to_vec())
        .collect::<Vec<_>>();
    Some(inv)
}

/// Default `deltachi` (relative chi-square decrement, in percent) that stops
/// the LM iteration when an accepted step improves chi-square by less than
/// this. silx default is `0.001` (i.e. 0.1 %).
pub const DEFAULT_DELTACHI: f64 = 0.001;

/// Default maximum number of iterations (silx `max_iter=100`).
pub const DEFAULT_MAX_ITER: usize = 100;

/// Run an iterative Levenberg-Marquardt least-squares fit.
///
/// `model(x, params) -> y_hat` is evaluated over the whole `x` array. `p0` is
/// the initial parameter guess. `sigma` is the optional per-point uncertainty
/// used as weight (`weight = 1/sigma^2`); when `None`, every weight is 1, as in
/// silx (`sigma = numpy.ones(...)`).
///
/// Faithful to silx `leastsq` (unconstrained path): forward numerical
/// derivatives with step `delta[i] = (p[i] + (p[i]==0)) * sqrt(epsfcn)`,
/// `epsfcn = f64::EPSILON`, accept-if-chi-square-decreases with `flambda`
/// damping (start 0.001, `*10` on rejection up to 1000, `/10` on acceptance),
/// and the two-stop convergence test (`lastdeltachi < deltachi` or
/// `absdeltachi < sqrt(epsfcn)`), with the silx rule that the first iteration
/// always proceeds regardless of those limits.
pub fn leastsq<F>(
    model: F,
    xdata: &[f64],
    ydata: &[f64],
    p0: &[f64],
    sigma: Option<&[f64]>,
    max_iter: usize,
    deltachi: f64,
) -> Result<LeastSqResult, FitError>
where
    F: Fn(&[f64], &[f64]) -> Vec<f64>,
{
    if xdata.len() != ydata.len() {
        return Err(FitError::LengthMismatch);
    }
    let n_param = p0.len();
    if n_param == 0 {
        return Err(FitError::NoFreeParameters);
    }
    let m = ydata.len();
    if m < n_param {
        return Err(FitError::NotEnoughData);
    }
    // check_finite: silx asarray_chkfinite on xdata/ydata/sigma.
    if xdata.iter().chain(ydata.iter()).any(|v| !v.is_finite()) {
        return Err(FitError::NonFinite);
    }
    // weight0 = (1/sigma)^2 ; sigma==0 → divisor 1 (silx `sigma + (sigma==0)`).
    let weight0: Vec<f64> = match sigma {
        Some(s) => {
            if s.len() != m {
                return Err(FitError::LengthMismatch);
            }
            if s.iter().any(|v| !v.is_finite()) {
                return Err(FitError::NonFinite);
            }
            s.iter()
                .map(|&sv| {
                    let denom = if sv == 0.0 { 1.0 } else { sv };
                    let w = 1.0 / denom;
                    w * w
                })
                .collect()
        }
        None => vec![1.0; m],
    };

    let epsfcn = f64::EPSILON;
    let sqrt_epsfcn = epsfcn.sqrt();

    let mut fittedpar = p0.to_vec();
    let mut flambda = 0.001_f64;
    let mut iiter = max_iter as i64;
    let mut last_evaluation: Option<Vec<f64>> = None;
    let mut iteration_counter: usize = 0;
    let mut nfev: usize = 0;

    // Outputs of the most recent chisq_alpha_beta, captured for covariance.
    let mut chisq0: f64;
    let mut alpha0: Vec<Vec<f64>> = vec![vec![0.0; n_param]; n_param];

    loop {
        if iiter <= 0 {
            break;
        }
        iteration_counter += 1;

        // --- chisq_alpha_beta (unconstrained) ---
        // yfit at current parameters (reuse last_evaluation if available).
        let yfit0 = match &last_evaluation {
            Some(ev) => ev.clone(),
            None => {
                let ev = model(xdata, &fittedpar);
                nfev += 1;
                ev
            }
        };
        // delta[i] = (p[i] + (p[i]==0)) * sqrt(epsfcn)
        let delta: Vec<f64> = fittedpar
            .iter()
            .map(|&p| (p + if p == 0.0 { 1.0 } else { 0.0 }) * sqrt_epsfcn)
            .collect();
        // Forward numerical derivatives deriv[i][j] = (f(p+delta_i) - f0)/delta_i.
        let mut deriv: Vec<Vec<f64>> = Vec::with_capacity(n_param);
        for i in 0..n_param {
            let mut pwork = fittedpar.clone();
            pwork[i] = fittedpar[i] + delta[i];
            let f1 = model(xdata, &pwork);
            nfev += 1;
            let di = delta[i];
            let row: Vec<f64> = f1
                .iter()
                .zip(yfit0.iter())
                .map(|(&a, &b)| (a - b) / di)
                .collect();
            deriv.push(row);
        }
        // deltay = y - yfit ; help0 = weight * deltay
        let deltay: Vec<f64> = ydata
            .iter()
            .zip(yfit0.iter())
            .map(|(&y, &f)| y - f)
            .collect();
        let help0: Vec<f64> = weight0
            .iter()
            .zip(deltay.iter())
            .map(|(&w, &d)| w * d)
            .collect();
        // beta[i] = sum_j help0[j]*deriv[i][j]
        let mut beta = vec![0.0_f64; n_param];
        for i in 0..n_param {
            let mut s = 0.0;
            for j in 0..m {
                s += help0[j] * deriv[i][j];
            }
            beta[i] = s;
        }
        // alpha[i][k] = sum_j deriv[i][j]*weight[j]*deriv[k][j]
        let mut alpha = vec![vec![0.0_f64; n_param]; n_param];
        for i in 0..n_param {
            for k in 0..n_param {
                let mut s = 0.0;
                for j in 0..m {
                    s += deriv[i][j] * weight0[j] * deriv[k][j];
                }
                alpha[i][k] = s;
            }
        }
        // chisq = sum(help0 * deltay)
        chisq0 = help0.iter().zip(deltay.iter()).map(|(&h, &d)| h * d).sum();
        alpha0 = alpha.clone();

        // --- LM inner loop: pick a step that decreases chisq ---
        loop {
            // alpha' = alpha0 * (1 + flambda*I): only the diagonal is scaled.
            let mut alpha_lm = alpha0.clone();
            for (d, row) in alpha_lm.iter_mut().enumerate() {
                row[d] *= 1.0 + flambda;
            }
            let inv_alpha = match invert_matrix(&alpha_lm) {
                Some(inv) => inv,
                None => {
                    // Treat as a rejected step: damp harder.
                    flambda *= 10.0;
                    if flambda > 1000.0 {
                        iiter = 0;
                        break;
                    }
                    continue;
                }
            };
            // deltapar = beta · inv_alpha  (row-vector times matrix)
            // numpy: numpy.dot(beta, inv(alpha)) → sum_i beta[i]*inv[i][k]
            let mut deltapar = vec![0.0_f64; n_param];
            for (k, dp) in deltapar.iter_mut().enumerate() {
                let mut s = 0.0;
                for (i, &b) in beta.iter().enumerate() {
                    s += b * inv_alpha[i][k];
                }
                *dp = s;
            }
            let newpar: Vec<f64> = fittedpar
                .iter()
                .zip(deltapar.iter())
                .map(|(&p, &d)| p + d)
                .collect();
            let yfit = model(xdata, &newpar);
            nfev += 1;
            let chisq: f64 = weight0
                .iter()
                .zip(ydata.iter().zip(yfit.iter()))
                .map(|(&w, (&y, &f))| {
                    let r = y - f;
                    w * r * r
                })
                .sum();
            let absdeltachi = chisq0 - chisq;
            if absdeltachi < 0.0 {
                // Step worsened chi-square: reject, damp harder (silx flambda *= 10).
                flambda *= 10.0;
                if flambda > 1000.0 {
                    iiter = 0;
                    break;
                }
            } else {
                // Step improved chi-square: accept it.
                fittedpar = newpar;
                let lastdeltachi =
                    100.0 * (absdeltachi / (chisq + if chisq == 0.0 { 1.0 } else { 0.0 }));
                // silx convergence test: after the first iteration (which is
                // always allowed to proceed), stop when either the relative
                // chi-square decrement falls below `deltachi` OR the absolute
                // decrement falls below `sqrt(epsfcn)`. Both branches stop the
                // loop, so they are combined here.
                if iteration_counter >= 2 && (lastdeltachi < deltachi || absdeltachi < sqrt_epsfcn)
                {
                    iiter = 0;
                }
                // silx sets `chisq0 = chisq` here, but it is recomputed from
                // scratch at the top of every outer iteration via the
                // chisq_alpha_beta block, so persisting it has no effect.
                flambda /= 10.0;
                last_evaluation = Some(yfit);
                break;
            }
        }
        iiter -= 1;
    }

    // Covariance is inv(alpha0) (silx cov0).
    let covariance = invert_matrix(&alpha0).ok_or(FitError::SingularMatrix)?;
    let chisq_final = {
        // Recompute at the final parameters for a definite chisq value.
        let yfit = model(xdata, &fittedpar);
        nfev += 1;
        weight0
            .iter()
            .zip(ydata.iter().zip(yfit.iter()))
            .map(|(&w, (&y, &f))| {
                let r = y - f;
                w * r * r
            })
            .sum::<f64>()
    };
    let dof = m as i64 - n_param as i64;
    let reduced_chisq = if dof > 0 {
        Some(chisq_final / dof as f64)
    } else {
        None
    };

    Ok(LeastSqResult {
        parameters: fittedpar,
        covariance,
        chisq: chisq_final,
        reduced_chisq,
        niter: iteration_counter,
        nfev,
    })
}

// ---------------------------------------------------------------------------
// Peak models (CPU). Each evaluates a single peak + flat background:
// y(x) = peak(x; params...) + background.
//
// The peak formulas are ported byte-for-byte from
// `silx/math/fit/functions/src/funs.c` (single-peak case of the sum_* loops).
// A trailing `background` parameter is appended (constant offset) so that a
// model is fully described by one parameter vector for `leastsq`.
// ---------------------------------------------------------------------------

/// Evaluate a Gaussian peak (height parameterisation) plus flat background.
///
/// `params = [height, centroid, fwhm, background]`. Mirrors C `sum_gauss`:
/// `sigma = fwhm / (2*sqrt(2*LOG2))`, `y = height*exp(-0.5*((x-c)/sigma)^2)`,
/// with the C guard `(x-c)/sigma <= 20` skipping far-tail terms.
pub fn gaussian_model(x: &[f64], params: &[f64]) -> Vec<f64> {
    let (height, centroid, fwhm, bg) = (params[0], params[1], params[2], params[3]);
    let sigma = fwhm / fwhm_to_sigma_factor();
    x.iter()
        .map(|&xi| {
            let mut y = bg;
            if sigma != 0.0 {
                let dhelp = (xi - centroid) / sigma;
                if dhelp <= 20.0 {
                    y += height * (-0.5 * dhelp * dhelp).exp();
                }
            }
            y
        })
        .collect()
}

/// Evaluate a Gaussian peak (area parameterisation) plus flat background.
///
/// `params = [area, centroid, fwhm, background]`. Mirrors C `sum_agauss`:
/// `sigma = fwhm/(2*sqrt(2*LOG2))`, `height = area/(sigma*sqrt(2*pi))`,
/// with the C guard `(x-c)/sigma <= 35`.
pub fn gaussian_area_model(x: &[f64], params: &[f64]) -> Vec<f64> {
    let (area, centroid, fwhm, bg) = (params[0], params[1], params[2], params[3]);
    let sigma = fwhm / fwhm_to_sigma_factor();
    let sqrt2pi = (2.0 * std::f64::consts::PI).sqrt();
    x.iter()
        .map(|&xi| {
            let mut y = bg;
            if sigma != 0.0 {
                let height = area / (sigma * sqrt2pi);
                let dhelp = (xi - centroid) / sigma;
                if dhelp <= 35.0 {
                    y += height * (-0.5 * dhelp * dhelp).exp();
                }
            }
            y
        })
        .collect()
}

/// Evaluate a Lorentzian peak (height parameterisation) plus flat background.
///
/// `params = [height, centroid, fwhm, background]`. Mirrors C `sum_lorentz`:
/// `dhelp = (x-c)/(0.5*fwhm)`, `y = height/(1 + dhelp^2)`.
pub fn lorentzian_model(x: &[f64], params: &[f64]) -> Vec<f64> {
    let (height, centroid, fwhm, bg) = (params[0], params[1], params[2], params[3]);
    x.iter()
        .map(|&xi| {
            let mut y = bg;
            if fwhm != 0.0 {
                let dhelp = (xi - centroid) / (0.5 * fwhm);
                y += height / (1.0 + dhelp * dhelp);
            }
            y
        })
        .collect()
}

/// Evaluate a pseudo-Voigt peak (height parameterisation) plus flat background.
///
/// `params = [height, centroid, fwhm, eta, background]`. Mirrors C
/// `sum_pvoigt`: `PV = eta*L + (1-eta)*G` where `L = height/(1+((x-c)/(0.5*fwhm))^2)`
/// and `G = height*exp(-0.5*((x-c)/sigma)^2)` with `sigma = fwhm/(2*sqrt(2*LOG2))`,
/// C guard `(x-c)/sigma <= 35` on the Gaussian term.
pub fn pseudo_voigt_model(x: &[f64], params: &[f64]) -> Vec<f64> {
    let (height, centroid, fwhm, eta, bg) = (params[0], params[1], params[2], params[3], params[4]);
    let sigma = fwhm / fwhm_to_sigma_factor();
    x.iter()
        .map(|&xi| {
            let mut y = bg;
            if fwhm != 0.0 {
                // Lorentzian term.
                let dl = (xi - centroid) / (0.5 * fwhm);
                y += eta * height / (1.0 + dl * dl);
            }
            if sigma != 0.0 {
                // Gaussian term.
                let dg = (xi - centroid) / sigma;
                if dg <= 35.0 {
                    y += (1.0 - eta) * height * (-0.5 * dg * dg).exp();
                }
            }
            y
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Initial-parameter estimators.
//
// silx `estimate_height_position_fwhm` runs a peak search + strip background +
// a 4-iteration constrained micro-fit. The strip/snip background estimator and
// multi-peak search are DEFERRED; we port the single-peak analytical seed:
// background = min(y), height = max(y) - background, centroid = x[argmax],
// fwhm from the half-maximum crossing (the same shape silx ends up with for a
// single dominant peak). Area/eta conversions follow `estimate_agauss` /
// `estimate_pvoigt`.
// ---------------------------------------------------------------------------

/// Analytical single-peak seed: `(height, centroid, fwhm, background)`.
///
/// `background = min(y)`; `height = max(y) - background`; `centroid` is the
/// `x` at the maximum; `fwhm` is the width between the outermost half-maximum
/// crossings around the peak. Returns `None` if there are fewer than 3 points
/// or lengths differ.
pub fn estimate_height_position_fwhm(x: &[f64], y: &[f64]) -> Option<(f64, f64, f64, f64)> {
    if x.len() != y.len() || x.len() < 3 {
        return None;
    }
    let bg = y.iter().copied().fold(f64::INFINITY, f64::min);
    let mut max_y = f64::NEG_INFINITY;
    let mut max_idx = 0;
    for (i, &yi) in y.iter().enumerate() {
        if yi > max_y {
            max_y = yi;
            max_idx = i;
        }
    }
    let height = max_y - bg;
    let centroid = x[max_idx];
    let half_max = bg + height / 2.0;
    let mut left = max_idx;
    while left > 0 && y[left] > half_max {
        left -= 1;
    }
    let mut right = max_idx;
    while right < y.len() - 1 && y[right] > half_max {
        right += 1;
    }
    let fwhm = if right > left {
        x[right] - x[left]
    } else {
        (x[x.len() - 1] - x[0]).abs() / 4.0
    };
    let fwhm = if fwhm > 0.0 {
        fwhm
    } else {
        (x[x.len() - 1] - x[0]).abs() / 4.0
    };
    Some((height, centroid, fwhm, bg))
}

/// Seed for [`gaussian_model`]: `[height, centroid, fwhm, background]`.
pub fn estimate_gaussian(x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
    let (h, c, f, bg) = estimate_height_position_fwhm(x, y)?;
    Some(vec![h, c, f, bg])
}

/// Seed for [`gaussian_area_model`]: `[area, centroid, fwhm, background]`.
///
/// Area conversion mirrors silx `estimate_agauss`:
/// `area = sqrt(2*pi) * height * fwhm / (2*sqrt(2*ln2))`.
pub fn estimate_gaussian_area(x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
    let (h, c, f, bg) = estimate_height_position_fwhm(x, y)?;
    let area = (2.0 * std::f64::consts::PI).sqrt() * h * f / fwhm_to_sigma_factor();
    Some(vec![area, c, f, bg])
}

/// Seed for [`lorentzian_model`]: `[height, centroid, fwhm, background]`.
///
/// Same height/position/fwhm seed as Gaussian (silx `estimate_lorentz` reuses
/// `estimate_height_position_fwhm` without converting height).
pub fn estimate_lorentzian(x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
    let (h, c, f, bg) = estimate_height_position_fwhm(x, y)?;
    Some(vec![h, c, f, bg])
}

/// Seed for [`pseudo_voigt_model`]: `[height, centroid, fwhm, eta, background]`.
///
/// Eta seeds to 0.5, mirroring silx `estimate_pvoigt` (`newpar.append(0.5)`).
pub fn estimate_pseudo_voigt(x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
    let (h, c, f, bg) = estimate_height_position_fwhm(x, y)?;
    Some(vec![h, c, f, 0.5, bg])
}

// ---------------------------------------------------------------------------
// Iterative fit models exposed through the FitFunction trait, and fit range.
// ---------------------------------------------------------------------------

/// Which peak model an [`IterativeFit`] fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeakModel {
    /// Gaussian, height parameterisation: `[height, centroid, fwhm, bg]`.
    Gaussian,
    /// Gaussian, area parameterisation: `[area, centroid, fwhm, bg]`.
    GaussianArea,
    /// Lorentzian, height parameterisation: `[height, centroid, fwhm, bg]`.
    Lorentzian,
    /// Pseudo-Voigt: `[height, centroid, fwhm, eta, bg]`.
    PseudoVoigt,
}

impl PeakModel {
    /// Display name for this model.
    pub fn name(self) -> &'static str {
        match self {
            PeakModel::Gaussian => "Gaussian",
            PeakModel::GaussianArea => "Gaussian (Area)",
            PeakModel::Lorentzian => "Lorentzian",
            PeakModel::PseudoVoigt => "Pseudo-Voigt",
        }
    }

    /// Parameter names for this model, in parameter-vector order.
    pub fn param_names(self) -> Vec<String> {
        let owned = |s: &str| s.to_string();
        match self {
            PeakModel::Gaussian => vec![
                owned("Height"),
                owned("Center"),
                owned("FWHM"),
                owned("Background"),
            ],
            PeakModel::GaussianArea => vec![
                owned("Area"),
                owned("Center"),
                owned("FWHM"),
                owned("Background"),
            ],
            PeakModel::Lorentzian => vec![
                owned("Height"),
                owned("Center"),
                owned("FWHM"),
                owned("Background"),
            ],
            PeakModel::PseudoVoigt => vec![
                owned("Height"),
                owned("Center"),
                owned("FWHM"),
                owned("Eta"),
                owned("Background"),
            ],
        }
    }

    /// Evaluate this model over `x` with the given parameter vector.
    pub fn eval(self, x: &[f64], params: &[f64]) -> Vec<f64> {
        match self {
            PeakModel::Gaussian => gaussian_model(x, params),
            PeakModel::GaussianArea => gaussian_area_model(x, params),
            PeakModel::Lorentzian => lorentzian_model(x, params),
            PeakModel::PseudoVoigt => pseudo_voigt_model(x, params),
        }
    }

    /// Estimate an initial parameter vector for this model from the data.
    pub fn estimate(self, x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
        match self {
            PeakModel::Gaussian => estimate_gaussian(x, y),
            PeakModel::GaussianArea => estimate_gaussian_area(x, y),
            PeakModel::Lorentzian => estimate_lorentzian(x, y),
            PeakModel::PseudoVoigt => estimate_pseudo_voigt(x, y),
        }
    }
}

/// Outcome of an iterative peak fit: the [`FitResult`] plus the solver
/// diagnostics needed for a results table (errors + reduced chi-square).
#[derive(Debug, Clone)]
pub struct IterativeFitResult {
    /// The fitted curve and parameters (compatible with the simple fitters).
    pub fit: FitResult,
    /// Full solver output (covariance, chi-square, iteration counts).
    pub solver: LeastSqResult,
}

impl IterativeFitResult {
    /// Per-parameter standard errors (`sqrt(diag(covariance))`).
    pub fn std_errors(&self) -> Vec<f64> {
        self.solver.std_errors()
    }

    /// Reduced chi-square, if degrees of freedom were positive.
    pub fn reduced_chisq(&self) -> Option<f64> {
        self.solver.reduced_chisq
    }
}

/// An iterative (Levenberg-Marquardt) peak fitter for one [`PeakModel`].
///
/// Estimates initial parameters with [`PeakModel::estimate`], then refines them
/// with [`leastsq`]. The [`FitFunction`] impl returns the refined [`FitResult`];
/// use [`IterativeFit::fit_full`] to also obtain the covariance / chi-square.
pub struct IterativeFit {
    /// The peak model fitted by this instance.
    pub model: PeakModel,
    /// Maximum LM iterations (defaults to [`DEFAULT_MAX_ITER`]).
    pub max_iter: usize,
    /// Relative chi-square stop threshold (defaults to [`DEFAULT_DELTACHI`]).
    pub deltachi: f64,
}

impl IterativeFit {
    /// Create an iterative fitter for `model` with silx default iteration
    /// controls.
    pub fn new(model: PeakModel) -> Self {
        Self {
            model,
            max_iter: DEFAULT_MAX_ITER,
            deltachi: DEFAULT_DELTACHI,
        }
    }

    /// Fit and return the full solver diagnostics (covariance, chi-square).
    pub fn fit_full(&self, x: &[f64], y: &[f64]) -> Option<IterativeFitResult> {
        let p0 = self.model.estimate(x, y)?;
        let model = self.model;
        let solver = leastsq(
            |xx, pp| model.eval(xx, pp),
            x,
            y,
            &p0,
            None,
            self.max_iter,
            self.deltachi,
        )
        .ok()?;
        let y_fit = self.model.eval(x, &solver.parameters);
        let fit = FitResult {
            y_fit,
            parameters: solver.parameters.clone(),
            param_names: self.model.param_names(),
        };
        Some(IterativeFitResult { fit, solver })
    }
}

impl FitFunction for IterativeFit {
    fn name(&self) -> &str {
        self.model.name()
    }

    fn fit(&self, x: &[f64], y: &[f64]) -> Option<FitResult> {
        self.fit_full(x, y).map(|r| r.fit)
    }
}

/// Fit `model` to only the data points whose `x` falls within `[xmin, xmax]`
/// (inclusive), mirroring silx `FitWidget` xmin/xmax range restriction.
///
/// Points outside the range are dropped before fitting, so they cannot
/// influence the fitted parameters. `xmin`/`xmax` may be given in any order.
/// Returns `None` if fewer than 3 points remain in range.
pub fn fit_in_range(
    xs: &[f64],
    ys: &[f64],
    xmin: f64,
    xmax: f64,
    model: &IterativeFit,
) -> Option<IterativeFitResult> {
    if xs.len() != ys.len() {
        return None;
    }
    let (lo, hi) = if xmin <= xmax {
        (xmin, xmax)
    } else {
        (xmax, xmin)
    };
    let mut xr = Vec::new();
    let mut yr = Vec::new();
    for (&xi, &yi) in xs.iter().zip(ys.iter()) {
        if xi >= lo && xi <= hi {
            xr.push(xi);
            yr.push(yi);
        }
    }
    if xr.len() < 3 {
        return None;
    }
    model.fit_full(&xr, &yr)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic noiseless Gaussian sampled on a grid.
    fn synth_gaussian(xs: &[f64], height: f64, center: f64, fwhm: f64, bg: f64) -> Vec<f64> {
        gaussian_model(xs, &[height, center, fwhm, bg])
    }

    fn linspace(a: f64, b: f64, n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| a + (b - a) * (i as f64) / ((n - 1) as f64))
            .collect()
    }

    #[test]
    fn invert_identity() {
        let id = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        let inv = invert_matrix(&id).unwrap();
        assert_eq!(inv, id);
    }

    #[test]
    fn invert_known_2x2() {
        // [[4,7],[2,6]] inverse = [[0.6,-0.7],[-0.2,0.4]]
        let m = vec![vec![4.0, 7.0], vec![2.0, 6.0]];
        let inv = invert_matrix(&m).unwrap();
        let expected = [[0.6, -0.7], [-0.2, 0.4]];
        for i in 0..2 {
            for j in 0..2 {
                assert!((inv[i][j] - expected[i][j]).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn invert_singular_returns_none() {
        let m = vec![vec![1.0, 2.0], vec![2.0, 4.0]];
        assert!(invert_matrix(&m).is_none());
    }

    #[test]
    fn leastsq_recovers_noiseless_line_exactly() {
        // Model: y = a*x + b, params [a, b]. Noiseless data with a=2.5, b=-1.0.
        let xs = linspace(-5.0, 5.0, 21);
        let (a_true, b_true) = (2.5, -1.0);
        let ys: Vec<f64> = xs.iter().map(|&x| a_true * x + b_true).collect();
        let model = |x: &[f64], p: &[f64]| x.iter().map(|&xi| p[0] * xi + p[1]).collect::<Vec<_>>();
        let res = leastsq(
            model,
            &xs,
            &ys,
            &[0.0, 0.0],
            None,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        assert!(
            (res.parameters[0] - a_true).abs() < 1e-6,
            "slope {} vs {}",
            res.parameters[0],
            a_true
        );
        assert!(
            (res.parameters[1] - b_true).abs() < 1e-6,
            "intercept {} vs {}",
            res.parameters[1],
            b_true
        );
        // Noiseless → chisq essentially zero.
        assert!(res.chisq < 1e-12, "chisq {}", res.chisq);
    }

    #[test]
    fn leastsq_converges_on_noisy_gaussian() {
        // Synthetic gaussian + small deterministic "noise" so the test is
        // reproducible. height=10, center=2, fwhm=1.5, bg=1.
        let xs = linspace(-3.0, 7.0, 101);
        let clean = synth_gaussian(&xs, 10.0, 2.0, 1.5, 1.0);
        // Deterministic pseudo-noise: small sinusoidal perturbation.
        let ys: Vec<f64> = clean
            .iter()
            .enumerate()
            .map(|(i, &c)| c + 0.05 * ((i as f64) * 0.7).sin())
            .collect();
        let fit = IterativeFit::new(PeakModel::Gaussian)
            .fit_full(&xs, &ys)
            .expect("fit should succeed");
        let p = &fit.fit.parameters;
        assert!((p[0] - 10.0).abs() < 0.2, "height {}", p[0]);
        assert!((p[1] - 2.0).abs() < 0.05, "center {}", p[1]);
        assert!((p[2] - 1.5).abs() < 0.1, "fwhm {}", p[2]);
        assert!((p[3] - 1.0).abs() < 0.1, "bg {}", p[3]);
        // Reduced chi-square (sigma=1) is on the order of the perturbation
        // variance, not enormous.
        let rc = fit.reduced_chisq().unwrap();
        assert!(rc < 0.01, "reduced chisq {}", rc);
    }

    #[test]
    fn gaussian_model_recovers_own_peak() {
        let xs = linspace(0.0, 20.0, 201);
        let ys = synth_gaussian(&xs, 5.0, 8.0, 2.0, 0.5);
        let fit = IterativeFit::new(PeakModel::Gaussian)
            .fit_full(&xs, &ys)
            .unwrap();
        let p = &fit.fit.parameters;
        assert!((p[0] - 5.0).abs() < 1e-3, "height {}", p[0]);
        assert!((p[1] - 8.0).abs() < 1e-3, "center {}", p[1]);
        assert!((p[2] - 2.0).abs() < 1e-3, "fwhm {}", p[2]);
        assert!((p[3] - 0.5).abs() < 1e-3, "bg {}", p[3]);
        // Noiseless fit → reduced chisq near 0.
        assert!(fit.reduced_chisq().unwrap() < 1e-6);
    }

    #[test]
    fn gaussian_area_model_recovers_own_peak() {
        // Build data from the area model with a known area.
        let xs = linspace(0.0, 20.0, 201);
        let area = 12.0;
        let ys = gaussian_area_model(&xs, &[area, 9.0, 2.5, 0.2]);
        let fit = IterativeFit::new(PeakModel::GaussianArea)
            .fit_full(&xs, &ys)
            .unwrap();
        let p = &fit.fit.parameters;
        assert!((p[0] - area).abs() < 1e-2, "area {}", p[0]);
        assert!((p[1] - 9.0).abs() < 1e-3, "center {}", p[1]);
        assert!((p[2] - 2.5).abs() < 1e-3, "fwhm {}", p[2]);
        assert!((p[3] - 0.2).abs() < 1e-3, "bg {}", p[3]);
        assert!(fit.reduced_chisq().unwrap() < 1e-6);
    }

    #[test]
    fn lorentzian_model_recovers_own_peak() {
        let xs = linspace(0.0, 20.0, 201);
        let ys = lorentzian_model(&xs, &[7.0, 11.0, 3.0, 1.0]);
        let fit = IterativeFit::new(PeakModel::Lorentzian)
            .fit_full(&xs, &ys)
            .unwrap();
        let p = &fit.fit.parameters;
        assert!((p[0] - 7.0).abs() < 1e-2, "height {}", p[0]);
        assert!((p[1] - 11.0).abs() < 1e-3, "center {}", p[1]);
        assert!((p[2] - 3.0).abs() < 1e-2, "fwhm {}", p[2]);
        assert!((p[3] - 1.0).abs() < 1e-2, "bg {}", p[3]);
        assert!(fit.reduced_chisq().unwrap() < 1e-6);
    }

    #[test]
    fn pseudo_voigt_model_recovers_own_peak() {
        let xs = linspace(0.0, 20.0, 301);
        let ys = pseudo_voigt_model(&xs, &[6.0, 10.0, 2.0, 0.4, 0.5]);
        let fit = IterativeFit::new(PeakModel::PseudoVoigt)
            .fit_full(&xs, &ys)
            .unwrap();
        let p = &fit.fit.parameters;
        assert!((p[0] - 6.0).abs() < 5e-2, "height {}", p[0]);
        assert!((p[1] - 10.0).abs() < 1e-2, "center {}", p[1]);
        assert!((p[2] - 2.0).abs() < 5e-2, "fwhm {}", p[2]);
        assert!((p[3] - 0.4).abs() < 5e-2, "eta {}", p[3]);
        assert!((p[4] - 0.5).abs() < 5e-2, "bg {}", p[4]);
        assert!(fit.reduced_chisq().unwrap() < 1e-4);
    }

    #[test]
    fn pseudo_voigt_eta_limits_match_gauss_and_lorentz() {
        // eta=0 → pure Gaussian; eta=1 → pure Lorentzian (same height/center/fwhm).
        let xs = linspace(0.0, 10.0, 51);
        let g = gaussian_model(&xs, &[3.0, 5.0, 2.0, 0.0]);
        let pv_g = pseudo_voigt_model(&xs, &[3.0, 5.0, 2.0, 0.0, 0.0]);
        for (a, b) in g.iter().zip(pv_g.iter()) {
            assert!((a - b).abs() < 1e-12);
        }
        let l = lorentzian_model(&xs, &[3.0, 5.0, 2.0, 0.0]);
        let pv_l = pseudo_voigt_model(&xs, &[3.0, 5.0, 2.0, 1.0, 0.0]);
        for (a, b) in l.iter().zip(pv_l.iter()) {
            assert!((a - b).abs() < 1e-12);
        }
    }

    #[test]
    fn fit_in_range_ignores_outside_points() {
        // A clean gaussian inside [4, 12]; outside the range we plant a wildly
        // different curve. If out-of-range points were used, the fit would be
        // pulled away from the true peak.
        let xs = linspace(0.0, 20.0, 201);
        let in_range: Vec<f64> = xs
            .iter()
            .map(|&x| {
                if (4.0..=12.0).contains(&x) {
                    // true gaussian
                    let sigma = 2.0 / fwhm_to_sigma_factor();
                    let d = (x - 8.0) / sigma;
                    5.0 * (-0.5 * d * d).exp() + 0.5
                } else {
                    // garbage outside the range
                    100.0 + 50.0 * x
                }
            })
            .collect();
        let fitter = IterativeFit::new(PeakModel::Gaussian);
        let res = fit_in_range(&xs, &in_range, 4.0, 12.0, &fitter).unwrap();
        let p = &res.fit.parameters;
        assert!((p[1] - 8.0).abs() < 0.05, "center pulled to {}", p[1]);
        assert!((p[2] - 2.0).abs() < 0.1, "fwhm {}", p[2]);
        assert!((p[0] - 5.0).abs() < 0.2, "height {}", p[0]);
    }

    #[test]
    fn fit_in_range_reversed_bounds_equivalent() {
        let xs = linspace(0.0, 20.0, 201);
        let ys = synth_gaussian(&xs, 4.0, 10.0, 2.0, 0.3);
        let fitter = IterativeFit::new(PeakModel::Gaussian);
        let a = fit_in_range(&xs, &ys, 6.0, 14.0, &fitter).unwrap();
        let b = fit_in_range(&xs, &ys, 14.0, 6.0, &fitter).unwrap();
        for (pa, pb) in a.fit.parameters.iter().zip(b.fit.parameters.iter()) {
            assert!((pa - pb).abs() < 1e-12);
        }
    }

    #[test]
    fn std_errors_from_covariance_diagonal() {
        // Construct a LeastSqResult with a known covariance and verify the
        // error extraction (sqrt of the diagonal).
        let res = LeastSqResult {
            parameters: vec![1.0, 2.0, 3.0],
            covariance: vec![
                vec![4.0, 0.1, 0.0],
                vec![0.1, 9.0, 0.2],
                vec![0.0, 0.2, 16.0],
            ],
            chisq: 0.0,
            reduced_chisq: Some(0.0),
            niter: 1,
            nfev: 1,
        };
        let errs = res.std_errors();
        assert!((errs[0] - 2.0).abs() < 1e-12);
        assert!((errs[1] - 3.0).abs() < 1e-12);
        assert!((errs[2] - 4.0).abs() < 1e-12);
    }

    #[test]
    fn std_errors_guard_negative_diagonal() {
        // A tiny negative diagonal (round-off) must not produce NaN; abs first.
        let res = LeastSqResult {
            parameters: vec![1.0],
            covariance: vec![vec![-1e-15]],
            chisq: 0.0,
            reduced_chisq: None,
            niter: 0,
            nfev: 0,
        };
        let e = res.std_errors();
        assert!(e[0].is_finite() && e[0] >= 0.0);
    }

    #[test]
    fn leastsq_length_mismatch_errors() {
        let r = leastsq(
            |x: &[f64], _p: &[f64]| x.to_vec(),
            &[1.0, 2.0, 3.0],
            &[1.0, 2.0],
            &[0.0],
            None,
            10,
            DEFAULT_DELTACHI,
        );
        assert_eq!(r.unwrap_err(), FitError::LengthMismatch);
    }

    #[test]
    fn leastsq_rejects_nonfinite() {
        let r = leastsq(
            |x: &[f64], p: &[f64]| x.iter().map(|&xi| p[0] * xi).collect::<Vec<_>>(),
            &[1.0, f64::NAN, 3.0],
            &[1.0, 2.0, 3.0],
            &[1.0],
            None,
            10,
            DEFAULT_DELTACHI,
        );
        assert_eq!(r.unwrap_err(), FitError::NonFinite);
    }

    #[test]
    fn estimate_seeds_are_close() {
        let xs = linspace(0.0, 20.0, 201);
        let ys = synth_gaussian(&xs, 5.0, 8.0, 2.0, 0.5);
        let (h, c, f, bg) = estimate_height_position_fwhm(&xs, &ys).unwrap();
        assert!((h - 5.0).abs() < 0.5, "height seed {}", h);
        assert!((c - 8.0).abs() < 0.2, "center seed {}", c);
        assert!((f - 2.0).abs() < 0.5, "fwhm seed {}", f);
        assert!((bg - 0.5).abs() < 0.1, "bg seed {}", bg);
    }
}
