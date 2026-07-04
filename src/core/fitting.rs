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
    /// Per-parameter uncertainties propagated through the applied constraints
    /// (silx `ddict["uncertainties"]` via `_get_sigma_parameters`). For an
    /// unconstrained fit this equals [`LeastSqResult::std_errors`]; with
    /// constraints it additionally carries the QUOTED `B·cos` factor and ties
    /// FACTOR/DELTA/SUM uncertainties to their reference parameter.
    pub uncertainties: Vec<f64>,
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
    /// A QUOTED constraint has equal min/max (`B == 0`), so the `sin/arcsin`
    /// reparametrisation is undefined (silx `raise ValueError("Invalid
    /// parameter limits")`).
    InvalidConstraint,
    /// A FACTOR/DELTA/SUM constraint references a parameter index outside the
    /// parameter vector.
    BadConstraintReference,
}

/// A per-parameter fit constraint for [`leastsq_constrained`].
///
/// Faithful to silx `silx.math.fit.leastsq` constraint codes (`CFREE`=0 …
/// `CIGNORED`=7). The constraint set has one entry per parameter; a parameter
/// is either *fitted* (Free/Positive/Quoted), *held* (Fixed/Ignored), or
/// *derived* from another parameter (Factor/Delta/Sum).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Constraint {
    /// `CFREE` — varied with no restriction.
    Free,
    /// `CPOSITIVE` — varied but kept positive (silx takes `abs(value)` rather
    /// than the commented-out square reparametrisation).
    Positive,
    /// `CQUOTED` — confined to `[min, max]` via silx's `A + B·sin(arcsin(...) +
    /// Δ)` reparametrisation (`A = (max+min)/2`, `B = (max−min)/2`). A start
    /// value outside `[min, max]` is held fixed at its start (silx warning
    /// path).
    Quoted {
        /// Lower bound (silx `min(constraints[i][1], constraints[i][2])`).
        min: f64,
        /// Upper bound (silx `max(...)`).
        max: f64,
    },
    /// `CFIXED` — held at its starting value; not fitted, gets 100 %
    /// uncertainty in the covariance.
    Fixed,
    /// `CFACTOR` — `value = factor · params[reference]` (silx `CFACTOR`).
    Factor {
        /// Index of the parameter this one is tied to.
        reference: usize,
        /// Multiplicative factor.
        factor: f64,
    },
    /// `CDELTA` — `value = delta + params[reference]` (silx `CDELTA`).
    Delta {
        /// Index of the parameter this one is tied to.
        reference: usize,
        /// Additive offset.
        delta: f64,
    },
    /// `CSUM` — `value = sum − params[reference]` (silx `CSUM`).
    Sum {
        /// Index of the parameter this one is tied to.
        reference: usize,
        /// Constant the pair sums to.
        sum: f64,
    },
    /// `CIGNORED` — set to 0 and stripped from the model call (silx `CIGNORED`;
    /// the model must accept the reduced parameter count).
    Ignored,
}

/// Apply the dependent-parameter relations to a full parameter vector (silx
/// `_get_parameters`). Free parameters pass through, Positive is `abs`, Quoted
/// passes through (its bounding is done at the step site), Factor/Delta/Sum are
/// recomputed from their reference, Ignored becomes 0. Two passes so that the
/// independent values are set before the dependent ones read them.
fn get_parameters(params: &[f64], constraints: &[Constraint]) -> Vec<f64> {
    let mut out: Vec<f64> = params
        .iter()
        .zip(constraints)
        .map(|(&p, c)| match c {
            Constraint::Positive => p.abs(),
            _ => p,
        })
        .collect();
    for (i, c) in constraints.iter().enumerate() {
        match *c {
            Constraint::Factor { reference, factor } => out[i] = factor * out[reference],
            Constraint::Delta { reference, delta } => out[i] = delta + out[reference],
            Constraint::Sum { reference, sum } => out[i] = sum - out[reference],
            Constraint::Ignored => out[i] = 0.0,
            _ => {}
        }
    }
    out
}

/// Propagate free-parameter sigmas back onto the full parameter vector through
/// the constraints (silx `_get_sigma_parameters`). `sigma0` holds one entry per
/// free parameter, in free-parameter order.
fn get_sigma_parameters(
    parameters: &[f64],
    sigma0: &[f64],
    constraints: &[Constraint],
) -> Vec<f64> {
    let mut sigma_par = vec![0.0_f64; parameters.len()];
    let mut n_free = 0usize;
    for (i, c) in constraints.iter().enumerate() {
        match *c {
            Constraint::Free | Constraint::Positive => {
                sigma_par[i] = sigma0[n_free];
                n_free += 1;
            }
            Constraint::Quoted { min, max } => {
                let pmax = min.max(max);
                let pmin = min.min(max);
                let b = 0.5 * (pmax - pmin);
                if b > 0.0 && parameters[i] < pmax && parameters[i] > pmin {
                    sigma_par[i] = (b * parameters[i].cos() * sigma0[n_free]).abs();
                    n_free += 1;
                } else {
                    sigma_par[i] = parameters[i];
                }
            }
            Constraint::Fixed => sigma_par[i] = parameters[i],
            _ => {}
        }
    }
    for (i, c) in constraints.iter().enumerate() {
        match *c {
            // CFACTOR scales the reference sigma by the tie factor
            // (leastsq.py:875-876); only CDELTA/CSUM copy it unscaled.
            Constraint::Factor { reference, factor } => {
                sigma_par[i] = factor * sigma_par[reference]
            }
            Constraint::Delta { reference, .. } | Constraint::Sum { reference, .. } => {
                sigma_par[i] = sigma_par[reference]
            }
            _ => {}
        }
    }
    sigma_par
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

/// Default peak-search sensitivity used by the FitManager theory estimators
/// (silx `fittheories.DEFAULT_CONFIG["Sensitivity"] = 2.5`, fittheories.py:107;
/// `estimate_height_position_fwhm` passes
/// `search_sens = float(self.config["Sensitivity"])` into its peak search,
/// fittheories.py:338/:356).
///
/// Distinct from
/// [`DEFAULT_PEAK_SENSITIVITY`](crate::core::peaks::DEFAULT_PEAK_SENSITIVITY)
/// (3.5) — the standalone `silx.math.fit.peak_search` pyx default, which the
/// FitManager estimation path does not use.
pub const DEFAULT_FIT_SENSITIVITY: f64 = 2.5;

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

        // --- LM inner loop: pick a step that decreases chisq. Every λ attempt
        // (accepted or rejected) consumes one unit of the `max_iter` budget:
        // silx decrements `iiter` inside the damping loop (leastsq.py:470), so
        // rejected-λ retries are not free. Decrementing at the top of each pass
        // counts identically — the inner loop never tests `iiter`, and the
        // outer test is `<= 0` either way. ---
        loop {
            iiter -= 1;
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

    // silx: constraints is None => uncertainties = sigma0 = sqrt(abs(diag(cov0))).
    let uncertainties: Vec<f64> = (0..n_param)
        .map(|i| covariance[i][i].abs().sqrt())
        .collect();

    Ok(LeastSqResult {
        parameters: fittedpar,
        covariance,
        uncertainties,
        chisq: chisq_final,
        reduced_chisq,
        niter: iteration_counter,
        nfev,
    })
}

/// Gather elements of `v` at the given indices (numpy `take`).
fn take(v: &[f64], indices: &[usize]) -> Vec<f64> {
    indices.iter().map(|&i| v[i]).collect()
}

/// Output of one constrained `chisq_alpha_beta` evaluation.
struct CabOut {
    /// `sum(weight * (y - yfit)^2)` at the current parameters.
    chisq: f64,
    /// Curvature matrix over the free parameters (`n_free x n_free`).
    alpha: Vec<Vec<f64>>,
    /// Gradient vector over the free parameters (`n_free`).
    beta: Vec<f64>,
    /// Number of free parameters this evaluation fitted.
    n_free: usize,
    /// Full-parameter index of each free parameter (silx `free_index`).
    free_index: Vec<usize>,
    /// Full-parameter indices passed to the model (silx `noigno`).
    noigno: Vec<usize>,
    /// Current values of the free parameters (silx `fitparam`).
    fitparam: Vec<f64>,
}

/// One constrained curvature/gradient evaluation (silx `chisq_alpha_beta` with
/// a non-None `constraints`). Builds the free-parameter set, the numerical
/// forward-difference Jacobian scaled by each parameter's `derivfactor`, and the
/// normal-equation matrices restricted to the free parameters.
#[allow(clippy::too_many_arguments)]
fn chisq_alpha_beta_constrained<F>(
    model: &F,
    parameters: &[f64],
    xdata: &[f64],
    ydata: &[f64],
    weight0: &[f64],
    constraints: &[Constraint],
    sqrt_epsfcn: f64,
    last_evaluation: Option<&[f64]>,
    nfev: &mut usize,
) -> CabOut
where
    F: Fn(&[f64], &[f64]) -> Vec<f64>,
{
    let m = ydata.len();

    // Classify parameters into the free set (silx: CFREE/CPOSITIVE always free,
    // CQUOTED free only when in bounds; others held/derived/ignored).
    let mut fitparam: Vec<f64> = Vec::new();
    let mut free_index: Vec<usize> = Vec::new();
    let mut noigno: Vec<usize> = Vec::new();
    let mut derivfactor: Vec<f64> = Vec::new();
    for (i, c) in constraints.iter().enumerate() {
        if !matches!(c, Constraint::Ignored) {
            noigno.push(i);
        }
        match *c {
            Constraint::Free => {
                fitparam.push(parameters[i]);
                derivfactor.push(1.0);
                free_index.push(i);
            }
            Constraint::Positive => {
                fitparam.push(parameters[i].abs());
                derivfactor.push(1.0);
                free_index.push(i);
            }
            Constraint::Quoted { min, max } => {
                let pmax = min.max(max);
                let pmin = min.min(max);
                if (pmax - pmin) > 0.0 && parameters[i] <= pmax && parameters[i] >= pmin {
                    let a = 0.5 * (pmax + pmin);
                    let b = 0.5 * (pmax - pmin);
                    fitparam.push(parameters[i]);
                    derivfactor.push(b * ((parameters[i] - a) / b).asin().cos());
                    free_index.push(i);
                }
                // Out of bounds: kept at its start value (not fitted).
            }
            _ => {}
        }
    }
    let n_free = fitparam.len();

    // delta[i] = (fitparam[i] + (fitparam[i]==0)) * sqrt(epsfcn).
    let delta: Vec<f64> = fitparam
        .iter()
        .map(|&p| (p + if p == 0.0 { 1.0 } else { 0.0 }) * sqrt_epsfcn)
        .collect();

    // pwork is the full parameter vector with the free values substituted in.
    let mut pwork = parameters.to_vec();
    for (i, &fi) in free_index.iter().enumerate() {
        pwork[fi] = fitparam[i];
    }

    // Base evaluation (silx f2 / yfit). We use one constraint-expanded base for
    // both the derivative reference and the residual, so the forward difference
    // is self-consistent; this matches silx's `model(*parameters)` exactly when
    // nothing is ignored and the start is in-domain (the practical case).
    let yfit: Vec<f64> = match last_evaluation {
        Some(ev) => ev.to_vec(),
        None => {
            let base_in = take(&get_parameters(&pwork, constraints), &noigno);
            let ev = model(xdata, &base_in);
            *nfev += 1;
            ev
        }
    };

    // Numerical forward derivatives for each free parameter, scaled by
    // derivfactor (CQUOTED `B·cos`, else 1).
    let mut deriv: Vec<Vec<f64>> = Vec::with_capacity(n_free);
    for i in 0..n_free {
        let fi = free_index[i];
        pwork[fi] = fitparam[i] + delta[i];
        let newpar = take(&get_parameters(&pwork, constraints), &noigno);
        let f1 = model(xdata, &newpar);
        *nfev += 1;
        let di = delta[i];
        let df = derivfactor[i];
        let row: Vec<f64> = f1
            .iter()
            .zip(yfit.iter())
            .map(|(&a, &b)| (a - b) / di * df)
            .collect();
        deriv.push(row);
        pwork[fi] = fitparam[i]; // restore
    }

    let deltay: Vec<f64> = ydata
        .iter()
        .zip(yfit.iter())
        .map(|(&y, &f)| y - f)
        .collect();
    let help0: Vec<f64> = weight0
        .iter()
        .zip(deltay.iter())
        .map(|(&w, &d)| w * d)
        .collect();
    let mut beta = vec![0.0_f64; n_free];
    for (i, b) in beta.iter_mut().enumerate() {
        let mut s = 0.0;
        for j in 0..m {
            s += help0[j] * deriv[i][j];
        }
        *b = s;
    }
    let mut alpha = vec![vec![0.0_f64; n_free]; n_free];
    for i in 0..n_free {
        for k in 0..n_free {
            let mut s = 0.0;
            for j in 0..m {
                s += deriv[i][j] * weight0[j] * deriv[k][j];
            }
            alpha[i][k] = s;
        }
    }
    let chisq = help0.iter().zip(deltay.iter()).map(|(&h, &d)| h * d).sum();

    CabOut {
        chisq,
        alpha,
        beta,
        n_free,
        free_index,
        noigno,
        fitparam,
    }
}

/// Run a constrained Levenberg-Marquardt least-squares fit (silx `leastsq` with
/// a non-`None` `constraints`).
///
/// `constraints` has one entry per parameter in `p0`. Free / Positive /
/// in-bounds Quoted parameters are varied; Fixed parameters are held at their
/// start (and receive 100 % uncertainty); Factor / Delta / Sum parameters are
/// derived from another parameter each step; Ignored parameters are set to 0 and
/// dropped from the model call (the model must accept the reduced count). When
/// every entry is [`Constraint::Free`] this matches an unconstrained fit except
/// that the covariance is recomputed at the final parameters (silx's second
/// `chisq_alpha_beta` pass).
///
/// The non-finite check, weighting, LM damping and convergence test match
/// [`leastsq`]; only the per-step parameter handling differs.
#[allow(clippy::too_many_arguments)]
pub fn leastsq_constrained<F>(
    model: F,
    xdata: &[f64],
    ydata: &[f64],
    p0: &[f64],
    constraints: &[Constraint],
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
    if constraints.len() != n_param {
        return Err(FitError::BadConstraintReference);
    }
    let m = ydata.len();
    if m < 1 {
        return Err(FitError::NotEnoughData);
    }
    if xdata.iter().chain(ydata.iter()).any(|v| !v.is_finite()) {
        return Err(FitError::NonFinite);
    }
    // Validate Factor/Delta/Sum references and reject equal-bound Quoted.
    for c in constraints {
        match *c {
            Constraint::Factor { reference, .. }
            | Constraint::Delta { reference, .. }
            | Constraint::Sum { reference, .. } => {
                if reference >= n_param {
                    return Err(FitError::BadConstraintReference);
                }
            }
            Constraint::Quoted { min, max } => {
                if (min.max(max) - min.min(max)) == 0.0 {
                    return Err(FitError::InvalidConstraint);
                }
            }
            _ => {}
        }
    }

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

    // Count the initial free set so `alpha0` is sized even if no iteration runs
    // (max_iter == 0); also enforces silx's "No free parameters to fit".
    let n_free_initial = constraints
        .iter()
        .enumerate()
        .filter(|(i, c)| match **c {
            Constraint::Free | Constraint::Positive => true,
            Constraint::Quoted { min, max } => {
                let (pmax, pmin) = (min.max(max), min.min(max));
                (pmax - pmin) > 0.0 && p0[*i] <= pmax && p0[*i] >= pmin
            }
            _ => false,
        })
        .count();
    if n_free_initial == 0 {
        return Err(FitError::NoFreeParameters);
    }

    let mut fittedpar = p0.to_vec();
    let mut flambda = 0.001_f64;
    let mut iiter = max_iter as i64;
    let mut last_evaluation: Option<Vec<f64>> = None;
    let mut iteration_counter: usize = 0;
    let mut nfev: usize = 0;

    let mut alpha0: Vec<Vec<f64>> = vec![vec![0.0; n_free_initial]; n_free_initial];
    let mut n_free_final = n_free_initial;

    loop {
        if iiter <= 0 {
            break;
        }
        iteration_counter += 1;

        let cab = chisq_alpha_beta_constrained(
            &model,
            &fittedpar,
            xdata,
            ydata,
            &weight0,
            constraints,
            sqrt_epsfcn,
            last_evaluation.as_deref(),
            &mut nfev,
        );
        let chisq0 = cab.chisq;
        alpha0 = cab.alpha.clone();
        n_free_final = cab.n_free;
        let beta = &cab.beta;
        let free_index = &cab.free_index;
        let noigno = &cab.noigno;
        let fitparam = &cab.fitparam;
        if cab.n_free == 0 {
            return Err(FitError::NoFreeParameters);
        }

        // Every λ attempt consumes one unit of the `max_iter` budget — silx
        // decrements `iiter` inside the damping loop (leastsq.py:470); see the
        // unconstrained engine for the placement rationale.
        loop {
            iiter -= 1;
            let mut alpha_lm = alpha0.clone();
            for (d, row) in alpha_lm.iter_mut().enumerate() {
                row[d] *= 1.0 + flambda;
            }
            let inv_alpha = match invert_matrix(&alpha_lm) {
                Some(inv) => inv,
                None => {
                    flambda *= 10.0;
                    if flambda > 1000.0 {
                        iiter = 0;
                        break;
                    }
                    continue;
                }
            };
            // deltapar = beta · inv_alpha (free-parameter space).
            let mut deltapar = vec![0.0_f64; cab.n_free];
            for (k, dp) in deltapar.iter_mut().enumerate() {
                let mut s = 0.0;
                for (i, &b) in beta.iter().enumerate() {
                    s += b * inv_alpha[i][k];
                }
                *dp = s;
            }
            // Rebuild the full parameter vector: Fixed/derived entries come from
            // the original p0 template, free entries take the LM step (Quoted via
            // the sin reparametrisation), then get_parameters applies the
            // dependent relations.
            let mut newpar = p0.to_vec();
            for (i, &fi) in free_index.iter().enumerate() {
                let pv = match constraints[fi] {
                    Constraint::Quoted { min, max } => {
                        let pmax = min.max(max);
                        let pmin = min.min(max);
                        let a = 0.5 * (pmax + pmin);
                        let b = 0.5 * (pmax - pmin);
                        a + b * (((fitparam[i] - a) / b).asin() + deltapar[i]).sin()
                    }
                    // Free and Positive both step additively; Positive's abs is
                    // applied by get_parameters below.
                    _ => fitparam[i] + deltapar[i],
                };
                newpar[fi] = pv;
            }
            let newpar = get_parameters(&newpar, constraints);
            let workpar = take(&newpar, noigno);
            let yfit = model(xdata, &workpar);
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
                flambda *= 10.0;
                if flambda > 1000.0 {
                    iiter = 0;
                    break;
                }
            } else {
                fittedpar = newpar;
                let lastdeltachi =
                    100.0 * (absdeltachi / (chisq + if chisq == 0.0 { 1.0 } else { 0.0 }));
                if iteration_counter >= 2 && (lastdeltachi < deltachi || absdeltachi < sqrt_epsfcn)
                {
                    iiter = 0;
                }
                flambda /= 10.0;
                last_evaluation = Some(yfit);
                break;
            }
        }
    }

    // cov0 = inv(alpha0): the free-space covariance (silx).
    let cov0 = invert_matrix(&alpha0).ok_or(FitError::SingularMatrix)?;

    // Second pass: every non-fixed/ignored parameter becomes Free, recompute the
    // curvature at the final parameters and invert for the reported covariance;
    // fixed/ignored parameters get a zero row/col and a 100 % diagonal.
    let new_constraints: Vec<Constraint> = constraints
        .iter()
        .map(|c| match c {
            Constraint::Fixed | Constraint::Ignored => *c,
            _ => Constraint::Free,
        })
        .collect();
    let cab2 = chisq_alpha_beta_constrained(
        &model,
        &fittedpar,
        xdata,
        ydata,
        &weight0,
        &new_constraints,
        sqrt_epsfcn,
        last_evaluation.as_deref(),
        &mut nfev,
    );
    let mut covariance = vec![vec![0.0_f64; n_param]; n_param];
    if let Some(cov_free) = invert_matrix(&cab2.alpha) {
        for (r, &pr) in cab2.free_index.iter().enumerate() {
            for (cc, &pc) in cab2.free_index.iter().enumerate() {
                covariance[pr][pc] = cov_free[r][cc];
            }
        }
    }
    for (idx, c) in constraints.iter().enumerate() {
        if matches!(c, Constraint::Fixed | Constraint::Ignored) {
            covariance[idx][idx] = fittedpar[idx] * fittedpar[idx];
        }
    }

    // Uncertainties: sigma0 = sqrt(abs(diag(cov0))) over the free parameters,
    // propagated through the constraints (silx _get_sigma_parameters).
    let sigma0: Vec<f64> = (0..n_free_final).map(|i| cov0[i][i].abs().sqrt()).collect();
    let uncertainties = get_sigma_parameters(&fittedpar, &sigma0, constraints);

    // Final chi-square at the converged parameters.
    let workpar = take(&get_parameters(&fittedpar, constraints), &cab2.noigno);
    let yfit_final = model(xdata, &workpar);
    nfev += 1;
    let chisq_final: f64 = weight0
        .iter()
        .zip(ydata.iter().zip(yfit_final.iter()))
        .map(|(&w, (&y, &f))| {
            let r = y - f;
            w * r * r
        })
        .sum();
    let dof = m as i64 - n_free_final as i64;
    let reduced_chisq = if dof > 0 {
        Some(chisq_final / dof as f64)
    } else {
        None
    };

    Ok(LeastSqResult {
        parameters: fittedpar,
        covariance,
        uncertainties,
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

/// Evaluate a Lorentzian peak (area parameterisation) plus flat background.
///
/// `params = [area, centroid, fwhm, background]`. Mirrors C `sum_alorentz`:
/// `dhelp = (x-c)/(0.5*fwhm)`, `y = area/(0.5*pi*fwhm*(1 + dhelp^2))`.
pub fn lorentzian_area_model(x: &[f64], params: &[f64]) -> Vec<f64> {
    let (area, centroid, fwhm, bg) = (params[0], params[1], params[2], params[3]);
    x.iter()
        .map(|&xi| {
            let mut y = bg;
            if fwhm != 0.0 {
                let dhelp = (xi - centroid) / (0.5 * fwhm);
                y += area / (0.5 * std::f64::consts::PI * fwhm * (1.0 + dhelp * dhelp));
            }
            y
        })
        .collect()
}

/// Evaluate an asymmetric (split) Gaussian peak plus flat background.
///
/// `params = [height, centroid, fwhm1, fwhm2, background]`. Mirrors C
/// `sum_splitgauss`: `sigma_i = fwhm_i/(2*sqrt(2*LOG2))`; the low side
/// (`x <= centroid`) uses `fwhm1`, the high side (`x > centroid`) uses `fwhm2`;
/// C guard `(x-c)/sigma <= 20`.
pub fn split_gaussian_model(x: &[f64], params: &[f64]) -> Vec<f64> {
    let (height, centroid, fwhm1, fwhm2, bg) =
        (params[0], params[1], params[2], params[3], params[4]);
    let sigma1 = fwhm1 / fwhm_to_sigma_factor();
    let sigma2 = fwhm2 / fwhm_to_sigma_factor();
    x.iter()
        .map(|&xi| {
            let mut y = bg;
            let diff = xi - centroid;
            let sigma = if diff > 0.0 { sigma2 } else { sigma1 };
            if sigma != 0.0 {
                let dhelp = diff / sigma;
                if dhelp <= 20.0 {
                    y += height * (-0.5 * dhelp * dhelp).exp();
                }
            }
            y
        })
        .collect()
}

/// Evaluate an asymmetric (split) Lorentzian peak plus flat background.
///
/// `params = [height, centroid, fwhm1, fwhm2, background]`. Mirrors C
/// `sum_splitlorentz`: `dhelp = (x-c)/(0.5*fwhm)` with `fwhm1` for
/// `x <= centroid` and `fwhm2` for `x > centroid`; `y = height/(1 + dhelp^2)`.
pub fn split_lorentzian_model(x: &[f64], params: &[f64]) -> Vec<f64> {
    let (height, centroid, fwhm1, fwhm2, bg) =
        (params[0], params[1], params[2], params[3], params[4]);
    x.iter()
        .map(|&xi| {
            let mut y = bg;
            let diff = xi - centroid;
            let fwhm = if diff > 0.0 { fwhm2 } else { fwhm1 };
            if fwhm != 0.0 {
                let dhelp = diff / (0.5 * fwhm);
                y += height / (1.0 + dhelp * dhelp);
            }
            y
        })
        .collect()
}

/// Evaluate a pseudo-Voigt peak (area parameterisation) plus flat background.
///
/// `params = [area, centroid, fwhm, eta, background]`. Mirrors C `sum_apvoigt`:
/// `sigma = fwhm/(2*sqrt(2*LOG2))`, `height = area/(sigma*sqrt(2*pi))`; the
/// Lorentzian term is `eta * area/(0.5*pi*fwhm*(1+((x-c)/(0.5*fwhm))^2))` and the
/// Gaussian term `(1-eta)*height*exp(-0.5*((x-c)/sigma)^2)` (C guard
/// `(x-c)/sigma <= 35`).
pub fn pseudo_voigt_area_model(x: &[f64], params: &[f64]) -> Vec<f64> {
    let (area, centroid, fwhm, eta, bg) = (params[0], params[1], params[2], params[3], params[4]);
    let sigma = fwhm / fwhm_to_sigma_factor();
    let half_pi = 0.5 * std::f64::consts::PI;
    let sqrt2pi = (2.0 * std::f64::consts::PI).sqrt();
    x.iter()
        .map(|&xi| {
            let mut y = bg;
            if fwhm != 0.0 {
                // Lorentzian term (area-normalised).
                let dl = (xi - centroid) / (0.5 * fwhm);
                y += eta * (area / (half_pi * fwhm * (1.0 + dl * dl)));
            }
            if sigma != 0.0 {
                // Gaussian term (area-normalised height).
                let height = area / (sigma * sqrt2pi);
                let dg = (xi - centroid) / sigma;
                if dg <= 35.0 {
                    y += (1.0 - eta) * height * (-0.5 * dg * dg).exp();
                }
            }
            y
        })
        .collect()
}

/// Evaluate an asymmetric (split) pseudo-Voigt peak plus flat background.
///
/// `params = [height, centroid, fwhm1, fwhm2, eta, background]`. Mirrors C
/// `sum_splitpvoigt`: the low side (`x <= centroid`) uses `fwhm1`/`sigma1`, the
/// high side (`x > centroid`) `fwhm2`/`sigma2`; per side `PV = eta*L + (1-eta)*G`
/// with `L = height/(1+((x-c)/(0.5*fwhm))^2)` and the Gaussian C guard
/// `(x-c)/sigma <= 35`.
pub fn split_pseudo_voigt_model(x: &[f64], params: &[f64]) -> Vec<f64> {
    let (height, centroid, fwhm1, fwhm2, eta, bg) = (
        params[0], params[1], params[2], params[3], params[4], params[5],
    );
    let sigma1 = fwhm1 / fwhm_to_sigma_factor();
    let sigma2 = fwhm2 / fwhm_to_sigma_factor();
    x.iter()
        .map(|&xi| {
            let mut y = bg;
            let diff = xi - centroid;
            let (fwhm, sigma) = if diff > 0.0 {
                (fwhm2, sigma2)
            } else {
                (fwhm1, sigma1)
            };
            if fwhm != 0.0 {
                let dl = diff / (0.5 * fwhm);
                y += eta * height / (1.0 + dl * dl);
            }
            if sigma != 0.0 {
                let dg = diff / sigma;
                if dg <= 35.0 {
                    y += (1.0 - eta) * height * (-0.5 * dg * dg).exp();
                }
            }
            y
        })
        .collect()
}

/// Evaluate a split pseudo-Voigt peak with a per-side eta, plus flat background.
///
/// `params = [height, centroid, fwhm1, fwhm2, eta1, eta2, background]`. Mirrors C
/// `sum_splitpvoigt2`: the low side (`x <= centroid`) uses `fwhm1`/`eta1`, the
/// high side `fwhm2`/`eta2`. C writes the Lorentzian argument as
/// `2*(x-c)/fwhm` (identical to `(x-c)/(0.5*fwhm)`); the Gaussian C guard is
/// `(x-c)/sigma <= 35`.
pub fn split_pseudo_voigt2_model(x: &[f64], params: &[f64]) -> Vec<f64> {
    let (height, centroid, fwhm1, fwhm2, eta1, eta2, bg) = (
        params[0], params[1], params[2], params[3], params[4], params[5], params[6],
    );
    let sigma1 = fwhm1 / fwhm_to_sigma_factor();
    let sigma2 = fwhm2 / fwhm_to_sigma_factor();
    x.iter()
        .map(|&xi| {
            let mut y = bg;
            let diff = xi - centroid;
            let (fwhm, sigma, eta) = if diff > 0.0 {
                (fwhm2, sigma2, eta2)
            } else {
                (fwhm1, sigma1, eta1)
            };
            if fwhm != 0.0 {
                let dl = (2.0 * diff) / fwhm;
                y += eta * height / (1.0 + dl * dl);
            }
            if sigma != 0.0 {
                let dg = diff / sigma;
                if dg <= 35.0 {
                    y += (1.0 - eta) * height * (-0.5 * dg * dg).exp();
                }
            }
            y
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Step / slit models (non-peak fit theories).
//
// Ported from `silx/math/fit/functions/src/funs.c` (`sum_stepdown`,
// `sum_stepup`, `sum_slit`) and the pure-Python `atan_stepup`
// (`silx/math/fit/functions/functions.pyx`). As with the peak models above, a
// trailing constant `background` parameter is appended so each model is a
// complete `leastsq` target; silx keeps the baseline in a separate background
// theory.
//
// `erf`/`erfc` are not in Rust's std. silx calls the C library `erf`/`erfc`
// at full double precision (funs.c; the `_WIN32` `myerfc` fallback is the
// lower-precision NR form). The hypermet tails are `erfc(w)·exp(z)` products
// where `erfc(w)` can be 1e-16 and far below, so `erfc` needs RELATIVE
// accuracy into the deep tail — `1 - erf(x)` collapses to exactly 0 for
// `x >= ~5.9`, zeroing the tails and their LM gradients. The `sunpro_erf`
// module below vendors the fdlibm implementation (via rust-lang/libm 0.2.16,
// the musl port of FreeBSD msun `s_erf.c`) — the same code behind the libm
// `erf`/`erfc` silx links — accurate to <1 ulp over the full range, exact at
// `erf(0) = 0` / `erfc(0) = 1`, with `erfc` underflowing only past x ≈ 27.2
// exactly as libm does.
// ---------------------------------------------------------------------------

/// fdlibm `erf`/`erfc`, vendored verbatim from rust-lang/libm 0.2.16
/// `src/math/erf.rs` (the musl port of FreeBSD msun `s_erf.c`), with the
/// crate-internal float helpers inlined (`get_high_word`, `with_set_low_word`)
/// and `exp`/`fabs` mapped to `f64::exp`/`f64::abs`.
///
/// > ====================================================
/// > Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved.
/// >
/// > Developed at SunPro, a Sun Microsystems, Inc. business.
/// > Permission to use, copy, modify, and distribute this
/// > software is freely granted, provided that this notice
/// > is preserved.
/// > ====================================================
// The fdlibm decimal constants carry more digits than f64 resolves; they are
// kept verbatim from the reference (trimming them invites transcription
// error), so the excessive-precision lint is expected here.
#[allow(clippy::excessive_precision)]
mod sunpro_erf {
    const ERX: f64 = 8.45062911510467529297e-01;
    // Coefficients for approximation to erf on [0, 0.84375].
    const EFX8: f64 = 1.02703333676410069053e+00;
    const PP0: f64 = 1.28379167095512558561e-01;
    const PP1: f64 = -3.25042107247001499370e-01;
    const PP2: f64 = -2.84817495755985104766e-02;
    const PP3: f64 = -5.77027029648944159157e-03;
    const PP4: f64 = -2.37630166566501626084e-05;
    const QQ1: f64 = 3.97917223959155352819e-01;
    const QQ2: f64 = 6.50222499887672944485e-02;
    const QQ3: f64 = 5.08130628187576562776e-03;
    const QQ4: f64 = 1.32494738004321644526e-04;
    const QQ5: f64 = -3.96022827877536812320e-06;
    // Coefficients for approximation to erf in [0.84375, 1.25].
    const PA0: f64 = -2.36211856075265944077e-03;
    const PA1: f64 = 4.14856118683748331666e-01;
    const PA2: f64 = -3.72207876035701323847e-01;
    const PA3: f64 = 3.18346619901161753674e-01;
    const PA4: f64 = -1.10894694282396677476e-01;
    const PA5: f64 = 3.54783043256182359371e-02;
    const PA6: f64 = -2.16637559486879084300e-03;
    const QA1: f64 = 1.06420880400844228286e-01;
    const QA2: f64 = 5.40397917702171048937e-01;
    const QA3: f64 = 7.18286544141962662868e-02;
    const QA4: f64 = 1.26171219808761642112e-01;
    const QA5: f64 = 1.36370839120290507362e-02;
    const QA6: f64 = 1.19844998467991074170e-02;
    // Coefficients for approximation to erfc in [1.25, 1/0.35].
    const RA0: f64 = -9.86494403484714822705e-03;
    const RA1: f64 = -6.93858572707181764372e-01;
    const RA2: f64 = -1.05586262253232909814e+01;
    const RA3: f64 = -6.23753324503260060396e+01;
    const RA4: f64 = -1.62396669462573470355e+02;
    const RA5: f64 = -1.84605092906711035994e+02;
    const RA6: f64 = -8.12874355063065934246e+01;
    const RA7: f64 = -9.81432934416914548592e+00;
    const SA1: f64 = 1.96512716674392571292e+01;
    const SA2: f64 = 1.37657754143519042600e+02;
    const SA3: f64 = 4.34565877475229228821e+02;
    const SA4: f64 = 6.45387271733267880336e+02;
    const SA5: f64 = 4.29008140027567833386e+02;
    const SA6: f64 = 1.08635005541779435134e+02;
    const SA7: f64 = 6.57024977031928170135e+00;
    const SA8: f64 = -6.04244152148580987438e-02;
    // Coefficients for approximation to erfc in [1/0.35, 28].
    const RB0: f64 = -9.86494292470009928597e-03;
    const RB1: f64 = -7.99283237680523006574e-01;
    const RB2: f64 = -1.77579549177547519889e+01;
    const RB3: f64 = -1.60636384855821916062e+02;
    const RB4: f64 = -6.37566443368389627722e+02;
    const RB5: f64 = -1.02509513161107724954e+03;
    const RB6: f64 = -4.83519191608651397019e+02;
    const SB1: f64 = 3.03380607434824582924e+01;
    const SB2: f64 = 3.25792512996573918826e+02;
    const SB3: f64 = 1.53672958608443695994e+03;
    const SB4: f64 = 3.19985821950859553908e+03;
    const SB5: f64 = 2.55305040643316442583e+03;
    const SB6: f64 = 4.74528541206955367215e+02;
    const SB7: f64 = -2.24409524465858183362e+01;

    /// Top 32 bits of the IEEE-754 representation (libm `get_high_word`).
    fn get_high_word(x: f64) -> u32 {
        (x.to_bits() >> 32) as u32
    }

    /// `x` with its low 32 mantissa bits cleared (libm `with_set_low_word(x, 0)`).
    fn clear_low_word(x: f64) -> f64 {
        f64::from_bits(x.to_bits() & 0xffff_ffff_0000_0000)
    }

    fn erfc1(x: f64) -> f64 {
        let s = x.abs() - 1.0;
        let p = PA0 + s * (PA1 + s * (PA2 + s * (PA3 + s * (PA4 + s * (PA5 + s * PA6)))));
        let q = 1.0 + s * (QA1 + s * (QA2 + s * (QA3 + s * (QA4 + s * (QA5 + s * QA6)))));
        1.0 - ERX - p / q
    }

    fn erfc2(ix: u32, mut x: f64) -> f64 {
        if ix < 0x3ff40000 {
            /* |x| < 1.25 */
            return erfc1(x);
        }
        x = x.abs();
        let s = 1.0 / (x * x);
        let (r, big_s) = if ix < 0x4006db6d {
            /* |x| < 1/.35 ~ 2.85714 */
            (
                RA0 + s
                    * (RA1 + s * (RA2 + s * (RA3 + s * (RA4 + s * (RA5 + s * (RA6 + s * RA7)))))),
                1.0 + s
                    * (SA1
                        + s * (SA2
                            + s * (SA3 + s * (SA4 + s * (SA5 + s * (SA6 + s * (SA7 + s * SA8))))))),
            )
        } else {
            /* |x| > 1/.35 */
            (
                RB0 + s * (RB1 + s * (RB2 + s * (RB3 + s * (RB4 + s * (RB5 + s * RB6))))),
                1.0 + s
                    * (SB1 + s * (SB2 + s * (SB3 + s * (SB4 + s * (SB5 + s * (SB6 + s * SB7)))))),
            )
        };
        let z = clear_low_word(x);
        (-z * z - 0.5625).exp() * ((z - x) * (z + x) + r / big_s).exp() / x
    }

    /// Error function, `< 1` ulp over the full range.
    pub fn erf(x: f64) -> f64 {
        let mut ix = get_high_word(x);
        let sign = (ix >> 31) as usize;
        ix &= 0x7fffffff;
        if ix >= 0x7ff00000 {
            /* erf(nan)=nan, erf(+-inf)=+-1 */
            return 1.0 - 2.0 * (sign as f64) + 1.0 / x;
        }
        if ix < 0x3feb0000 {
            /* |x| < 0.84375 */
            if ix < 0x3e300000 {
                /* |x| < 2**-28: avoid underflow */
                return 0.125 * (8.0 * x + EFX8 * x);
            }
            let z = x * x;
            let r = PP0 + z * (PP1 + z * (PP2 + z * (PP3 + z * PP4)));
            let s = 1.0 + z * (QQ1 + z * (QQ2 + z * (QQ3 + z * (QQ4 + z * QQ5))));
            let y = r / s;
            return x + x * y;
        }
        let y = if ix < 0x40180000 {
            /* 0.84375 <= |x| < 6 */
            1.0 - erfc2(ix, x)
        } else {
            let x1p_1022 = f64::from_bits(0x0010000000000000);
            1.0 - x1p_1022
        };
        if sign != 0 { -y } else { y }
    }

    /// Complementary error function, computed directly (not `1 - erf(x)`) so
    /// large arguments keep relative precision instead of cancelling to 0.
    pub fn erfc(x: f64) -> f64 {
        let mut ix = get_high_word(x);
        let sign = (ix >> 31) as usize;
        ix &= 0x7fffffff;
        if ix >= 0x7ff00000 {
            /* erfc(nan)=nan, erfc(+-inf)=0,2 */
            return 2.0 * (sign as f64) + 1.0 / x;
        }
        if ix < 0x3feb0000 {
            /* |x| < 0.84375 */
            if ix < 0x3c700000 {
                /* |x| < 2**-56 */
                return 1.0 - x;
            }
            let z = x * x;
            let r = PP0 + z * (PP1 + z * (PP2 + z * (PP3 + z * PP4)));
            let s = 1.0 + z * (QQ1 + z * (QQ2 + z * (QQ3 + z * (QQ4 + z * QQ5))));
            let y = r / s;
            if sign != 0 || ix < 0x3fd00000 {
                /* x < 1/4 */
                return 1.0 - (x + x * y);
            }
            return 0.5 - (x - 0.5 + x * y);
        }
        if ix < 0x403c0000 {
            /* 0.84375 <= |x| < 28 */
            if sign != 0 {
                return 2.0 - erfc2(ix, x);
            } else {
                return erfc2(ix, x);
            }
        }
        let x1p_1022 = f64::from_bits(0x0010000000000000);
        if sign != 0 {
            2.0 - x1p_1022
        } else {
            x1p_1022 * x1p_1022
        }
    }
}

use sunpro_erf::{erf, erfc};

/// The C `sum_step*` edge scale: `denom = fwhm * sqrt(2) / (2*sqrt(2*LOG2))`,
/// i.e. `sigma * sqrt(2)`. The erf argument is `(x - centre) / denom`.
fn step_denom(fwhm: f64) -> f64 {
    fwhm * std::f64::consts::SQRT_2 / fwhm_to_sigma_factor()
}

/// Evaluate a step-down (descending error-function edge) plus flat background.
///
/// `params = [height, centroid, fwhm, background]`. Mirrors C `sum_stepdown`:
/// `y = background + height * 0.5 * erfc((x - centroid) / denom)` where `denom`
/// is `step_denom`. `fwhm` is the full-width at half maximum of the step's
/// derivative (its sharpness).
pub fn stepdown_model(x: &[f64], params: &[f64]) -> Vec<f64> {
    let (height, centroid, fwhm, bg) = (params[0], params[1], params[2], params[3]);
    let denom = step_denom(fwhm);
    x.iter()
        .map(|&xi| {
            let mut y = bg;
            if denom != 0.0 {
                y += height * 0.5 * erfc((xi - centroid) / denom);
            }
            y
        })
        .collect()
}

/// Evaluate a step-up (ascending error-function edge) plus flat background.
///
/// `params = [height, centroid, fwhm, background]`. Mirrors C `sum_stepup`:
/// `y = background + height * 0.5 * (1 + erf((x - centroid) / denom))`.
pub fn stepup_model(x: &[f64], params: &[f64]) -> Vec<f64> {
    let (height, centroid, fwhm, bg) = (params[0], params[1], params[2], params[3]);
    let denom = step_denom(fwhm);
    x.iter()
        .map(|&xi| {
            let mut y = bg;
            if denom != 0.0 {
                y += height * 0.5 * (1.0 + erf((xi - centroid) / denom));
            }
            y
        })
        .collect()
}

/// Evaluate a slit (a rising then falling pair of edges) plus flat background.
///
/// `params = [height, position, fwhm, beamfwhm, background]`. Mirrors C
/// `sum_slit`: with `c1 = position - 0.5*fwhm`, `c2 = position + 0.5*fwhm` and
/// `denom = step_denom(beamfwhm)`,
/// `y = background + height * 0.25 * (1 + erf((x-c1)/denom)) * erfc((x-c2)/denom)`.
/// `fwhm` is the slit width; `beamfwhm` is the sharpness of its two edges.
pub fn slit_model(x: &[f64], params: &[f64]) -> Vec<f64> {
    let (height, position, fwhm, beamfwhm, bg) =
        (params[0], params[1], params[2], params[3], params[4]);
    let denom = step_denom(beamfwhm);
    let c1 = position - 0.5 * fwhm;
    let c2 = position + 0.5 * fwhm;
    x.iter()
        .map(|&xi| {
            let mut y = bg;
            if denom != 0.0 {
                y += height * 0.25 * (1.0 + erf((xi - c1) / denom)) * erfc((xi - c2) / denom);
            }
            y
        })
        .collect()
}

/// Evaluate an arctan step-up plus flat background.
///
/// `params = [height, position, width, background]`. Mirrors Python
/// `atan_stepup`: `y = background + height * (0.5 + atan((x - position)/width)/pi)`.
/// A lower `width` yields a sharper step.
pub fn atan_stepup_model(x: &[f64], params: &[f64]) -> Vec<f64> {
    let (height, position, width, bg) = (params[0], params[1], params[2], params[3]);
    x.iter()
        .map(|&xi| {
            let mut y = bg;
            if width != 0.0 {
                y += height * (0.5 + ((xi - position) / width).atan() / std::f64::consts::PI);
            }
            y
        })
        .collect()
}

/// Evaluate a Hypermet peak (Gaussian + short tail + long tail + step) plus a
/// flat background.
///
/// `params = [area, position, fwhm, st_area_r, st_slope_r, lt_area_r,
/// lt_slope_r, step_height_r, background]`. Mirrors C `sum_ahypermet` with all
/// four terms active (`tail_flags = 15`, silx's default `HypermetTails`). With
/// `sigma = fwhm / (2*sqrt(2*LOG2))`, `height = area / (sigma*sqrt(2*pi))` and
/// `sigma_sqrt2 = sigma*sqrt(2)`, each sample is the sum of:
///
/// - Gaussian: `height * exp(-0.5*((x-pos)/sigma)^2)`;
/// - short tail (only when `|st_slope_r| > 1e-11`, the C `fabs(slope) > epsilon`
///   guard that also avoids the `1/slope` division):
///   `(area*c1/st_slope_r) * exp(0.5*(sigma/st_slope_r)^2 + (x-pos)/st_slope_r)`
///   with `c1 = st_area_r * 0.5 * erfc((x-pos)/sigma_sqrt2 + 0.5*sigma_sqrt2/st_slope_r)`;
/// - long tail: identical to the short tail with the `lt_*` ratios;
/// - step: `step_height_r * height * 0.5 * erfc((x-pos)/sigma_sqrt2)`.
///
/// The trailing constant `background` is siplot's per-model baseline (silx keeps
/// the baseline in a separate background theory). Returns a flat `background`
/// line when `sigma == 0` (matching siplot's other edge models, which contribute
/// nothing when their width collapses).
pub fn hypermet_model(x: &[f64], params: &[f64]) -> Vec<f64> {
    let area = params[0];
    let position = params[1];
    let fwhm = params[2];
    let st_area_r = params[3];
    let st_slope_r = params[4];
    let lt_area_r = params[5];
    let lt_slope_r = params[6];
    let step_height_r = params[7];
    let bg = params[8];

    let sigma = fwhm / fwhm_to_sigma_factor();
    if sigma == 0.0 {
        return vec![bg; x.len()];
    }
    let sqrt2pi = (2.0 * std::f64::consts::PI).sqrt();
    let height = area / (sigma * sqrt2pi);
    let sigma_sqrt2 = sigma * std::f64::consts::SQRT_2;
    const EPSILON: f64 = 1e-11;

    x.iter()
        .map(|&xi| {
            let dx = xi - position;
            let c2 = 0.5 * dx * dx / (sigma * sigma);
            let mut y = bg + (-c2).exp() * height;
            // Short-tail term.
            if st_slope_r.abs() > EPSILON {
                let c1 = st_area_r * 0.5 * erfc(dx / sigma_sqrt2 + 0.5 * sigma_sqrt2 / st_slope_r);
                y += (area * c1 / st_slope_r)
                    * (0.5 * (sigma / st_slope_r).powi(2) + dx / st_slope_r).exp();
            }
            // Long-tail term.
            if lt_slope_r.abs() > EPSILON {
                let c1 = lt_area_r * 0.5 * erfc(dx / sigma_sqrt2 + 0.5 * sigma_sqrt2 / lt_slope_r);
                y += (area * c1 / lt_slope_r)
                    * (0.5 * (sigma / lt_slope_r).powi(2) + dx / lt_slope_r).exp();
            }
            // Step term (`area / (sigma*sqrt2pi)` == `height`).
            y += step_height_r * height * 0.5 * erfc(dx / sigma_sqrt2);
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

/// Seed for [`lorentzian_area_model`]: `[area, centroid, fwhm, background]`.
///
/// Area conversion mirrors silx `estimate_alorentz`: `area = height * fwhm * 0.5 * pi`.
pub fn estimate_lorentzian_area(x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
    let (h, c, f, bg) = estimate_height_position_fwhm(x, y)?;
    let area = h * f * 0.5 * std::f64::consts::PI;
    Some(vec![area, c, f, bg])
}

/// Seed for [`split_gaussian_model`]: `[height, centroid, fwhm1, fwhm2, background]`.
///
/// Mirrors silx `estimate_splitgauss`: the second FWHM seeds equal to the first.
pub fn estimate_split_gaussian(x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
    let (h, c, f, bg) = estimate_height_position_fwhm(x, y)?;
    Some(vec![h, c, f, f, bg])
}

/// Seed for [`split_lorentzian_model`]: `[height, centroid, fwhm1, fwhm2, background]`.
///
/// silx's "Split Lorentz" theory reuses `estimate_splitgauss` (fittheories
/// `THEORY`), so the seed matches [`estimate_split_gaussian`]: `fwhm2 = fwhm1`.
pub fn estimate_split_lorentzian(x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
    let (h, c, f, bg) = estimate_height_position_fwhm(x, y)?;
    Some(vec![h, c, f, f, bg])
}

/// Seed for [`pseudo_voigt_area_model`]: `[area, centroid, fwhm, eta, background]`.
///
/// Mirrors silx `estimate_apvoigt`, which estimates the pseudo-Voigt height seed
/// then converts it to an area assuming the area is split half/half between the
/// Lorentzian and Gaussian contributions:
/// `area = 0.5*(h*fwhm*0.5*pi) + 0.5*(h*fwhm/(2*sqrt(2*ln2)))*sqrt(2*pi)`.
/// Eta seeds to 0.5.
pub fn estimate_pseudo_voigt_area(x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
    let (h, c, f, bg) = estimate_height_position_fwhm(x, y)?;
    let lorentz_area = h * f * 0.5 * std::f64::consts::PI;
    let gauss_area = (h * f / fwhm_to_sigma_factor()) * (2.0 * std::f64::consts::PI).sqrt();
    let area = 0.5 * lorentz_area + 0.5 * gauss_area;
    Some(vec![area, c, f, 0.5, bg])
}

/// Seed for [`split_pseudo_voigt_model`]:
/// `[height, centroid, fwhm1, fwhm2, eta, background]`.
///
/// Mirrors silx `estimate_splitpvoigt`: `fwhm2 = fwhm1` and `eta = 0.5`.
pub fn estimate_split_pseudo_voigt(x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
    let (h, c, f, bg) = estimate_height_position_fwhm(x, y)?;
    Some(vec![h, c, f, f, 0.5, bg])
}

/// Seed for [`split_pseudo_voigt2_model`]:
/// `[height, centroid, fwhm1, fwhm2, eta1, eta2, background]`.
///
/// Mirrors silx `estimate_splitpvoigt2`: `fwhm2 = fwhm1`, `eta1 = eta2 = 0.5`.
pub fn estimate_split_pseudo_voigt2(x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
    let (h, c, f, bg) = estimate_height_position_fwhm(x, y)?;
    Some(vec![h, c, f, f, 0.5, 0.5, bg])
}

/// `numpy.convolve(y, kernel, mode="valid")`: the kernel is applied reversed,
/// and the output length is `y.len() - kernel.len() + 1` (empty when `y` is
/// shorter than `kernel`).
fn convolve_valid(y: &[f64], kernel: &[f64]) -> Vec<f64> {
    let (n, m) = (y.len(), kernel.len());
    if n < m || m == 0 {
        return Vec::new();
    }
    (0..=n - m)
        .map(|k| (0..m).map(|j| y[k + j] * kernel[m - 1 - j]).sum())
        .collect()
}

/// Shared step-edge seed used by [`estimate_stepup`] / [`estimate_stepdown`].
///
/// silx convolves `y` with an edge-detecting kernel, rescales the derivative
/// so its peak has the data's amplitude (`y_deriv *= max(y) / max(y_deriv)`
/// when `max(y_deriv) > 0`, fittheories.py:1133-1134 / :1001-1002), then takes
/// the dominant peak of that derivative as the step centre and width. The
/// height rule differs per variant, and `use_deriv_height` selects it:
///
/// - `estimate_stepup` (fittheories.py:1150-1157) replaces the height with the
///   fitted derivative-peak height whenever it exceeds the data amplitude
///   `max(y) - min(y)` — after the rescale that fitted height is ≈ `max(y)`,
///   which wins whenever `min(y) > 0`.
/// - `estimate_stepdown` (fittheories.py:1019-1026) always keeps the data
///   amplitude (its largest-peak loop writes `data_amplitude` back even when
///   a larger derivative peak is found).
///
/// Multi-step search is out of scope: the single dominant edge is used,
/// matching the peak estimators (Rust's [`estimate_height_position_fwhm`] is
/// single-peak, so silx's largest-peak loop reduces to this one comparison).
/// The appended `background = min(y)`.
fn estimate_step(x: &[f64], y: &[f64], kernel: &[f64], use_deriv_height: bool) -> Option<Vec<f64>> {
    if x.len() != y.len() || x.len() < 3 {
        return None;
    }
    let bg = y.iter().copied().fold(f64::INFINITY, f64::min);
    let max_y = y.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let data_amplitude = max_y - bg;

    let cutoff = kernel.len() / 2;
    let mut y_deriv = convolve_valid(y, kernel);
    // silx: make the derivative's peak have the same amplitude as the step
    // (fittheories.py:1133-1134).
    let max_deriv = y_deriv.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if max_deriv > 0.0 {
        let scale = max_y / max_deriv;
        for v in &mut y_deriv {
            *v *= scale;
        }
    }
    let (height, center, fwhm) = if y_deriv.len() >= 3 && x.len() > 2 * cutoff {
        let x_slice = &x[cutoff..x.len() - cutoff];
        match estimate_height_position_fwhm(x_slice, &y_deriv) {
            Some((h, c, f, _b)) => {
                // Step-up only: the fitted derivative-peak height wins when it
                // exceeds the data amplitude (fittheories.py:1150-1157).
                let height = if use_deriv_height && h > data_amplitude {
                    h
                } else {
                    data_amplitude
                };
                (height, c, f)
            }
            None => {
                let (c, f) = step_fallback(x);
                (data_amplitude, c, f)
            }
        }
    } else {
        let (c, f) = step_fallback(x);
        (data_amplitude, c, f)
    };
    Some(vec![height, center, fwhm, bg])
}

/// silx no-peak fallback: centre at the middle of `x`, `fwhm = FwhmPoints * dx`
/// with the silx default `FwhmPoints = 8`.
fn step_fallback(x: &[f64]) -> (f64, f64) {
    let center = x[x.len() / 2];
    let dx = if x.len() > 1 { x[1] - x[0] } else { 1.0 };
    (center, 8.0 * dx)
}

/// Seed for [`stepup_model`]: `[height, centroid, fwhm, background]`.
///
/// Mirrors silx `estimate_stepup` (edge kernel `[0.25, 0.75, 0, -0.75, -0.25]`;
/// the seeded height is the fitted derivative-peak height — ≈ `max(y)` after
/// the rescale — whenever that exceeds `max(y) - min(y)`,
/// fittheories.py:1150-1157).
pub fn estimate_stepup(x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
    estimate_step(x, y, &[0.25, 0.75, 0.0, -0.75, -0.25], true)
}

/// Seed for [`stepdown_model`]: `[height, centroid, fwhm, background]`.
///
/// Mirrors silx `estimate_stepdown` (edge kernel `[-0.25, -0.75, 0, 0.75,
/// 0.25]`; the seeded height is always the data amplitude `max(y) - min(y)`,
/// fittheories.py:1019-1026).
pub fn estimate_stepdown(x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
    estimate_step(x, y, &[-0.25, -0.75, 0.0, 0.75, 0.25], false)
}

/// Seed for [`atan_stepup_model`]: `[height, position, width, background]`.
///
/// silx uses `estimate_stepup` for the arctan step (fittheories.py:1466 — the
/// same edge detection and height rule; the step fwhm seeds the arctan
/// `width`).
pub fn estimate_atan_stepup(x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
    estimate_stepup(x, y)
}

/// Seed for [`slit_model`]: `[height, position, fwhm, beamfwhm, background]`.
///
/// Mirrors silx `estimate_slit`: seed the up- and down-edges to size the beam
/// sharpness, then take the slit centre/width from the half-maximum crossings
/// of the background-subtracted data. silx subtracts a *strip* background; that
/// filter is a separate theory (not yet ported), so the baseline here is
/// `min(y)` — the same simplification the peak estimators use.
///
/// `beamfwhm` seeds as silx's exact expression `0.5 * (largestup[2] +
/// largestdown[1])` (fittheories.py:1076) — the up-step **FWHM** averaged
/// with the down-step **centre position**. silx's docstring says "average of
/// FWHM for up- and down-step" (fittheories.py:1065), but the code indexes
/// the down-step centre; the quirk is reproduced deliberately for parity.
/// It is then clamped in silx's order: `min` against `edge_distance / 10`,
/// then `max` against `range * 3 / len(x)` (fittheories.py:1077-1078).
pub fn estimate_slit(x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
    let up = estimate_stepup(x, y)?; // [h, center_up, fwhm_up, bg]
    let down = estimate_stepdown(x, y)?; // [h, center_down, fwhm_down, bg]
    let (center_up, fwhm_up) = (up[1], up[2]);
    let center_down = down[1];
    let edge_distance = (center_down - center_up).abs();

    let bg = y.iter().copied().fold(f64::INFINITY, f64::min);
    let y_minus_bg: Vec<f64> = y.iter().map(|&yi| yi - bg).collect();
    let height = y_minus_bg.iter().copied().fold(f64::NEG_INFINITY, f64::max);

    // Slit centre/width from the half-maximum crossings of (y - background).
    let threshold = 0.5 * height;
    let first = y_minus_bg.iter().position(|&v| v >= threshold)?;
    let last = y_minus_bg.iter().rposition(|&v| v >= threshold)?;
    let position = (x[first] + x[last]) / 2.0;
    let fwhm = x[last] - x[first];

    // Beam sharpness: silx's exact `0.5 * (largestup[2] + largestdown[1])` —
    // the up FWHM averaged with the down CENTRE (fittheories.py:1076, index
    // quirk reproduced), then clamped in silx's order (:1077-1078).
    let mut beamfwhm = 0.5 * (fwhm_up + center_down);
    beamfwhm = beamfwhm.min(edge_distance / 10.0);
    let xmin = x.iter().copied().fold(f64::INFINITY, f64::min);
    let xmax = x.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    beamfwhm = beamfwhm.max((xmax - xmin) * 3.0 / x.len() as f64);

    Some(vec![height, position, fwhm, beamfwhm, bg])
}

/// Seed for [`hypermet_model`]: `[area, position, fwhm, st_area_r, st_slope_r,
/// lt_area_r, lt_slope_r, step_height_r, background]`.
///
/// Area conversion mirrors silx `estimate_ahypermet`
/// (`area = height * fwhm / (2*sqrt(2*ln2)) * sqrt(2*pi)`). The five tail ratios
/// seed to silx's `Initial*Ratio` CONFIG defaults: `InitialShortTailAreaRatio
/// = 0.05`, `InitialShortTailSlopeRatio = 0.70`, `InitialLongTailAreaRatio
/// = 0.05`, `InitialLongTailSlopeRatio = 20.0`, `InitialStepTailHeightRatio
/// = 0.002`.
///
/// # Deviation from silx
///
/// silx's `estimate_ahypermet` zeroes *and* `CFIXED`s a tail when the Gaussian
/// area/height is below a counts threshold (`MinGaussArea4ShortTail = 50000`,
/// `MinGaussArea4LongTail = 1000`, `MinGaussHeight4StepTail = 5000`) and
/// otherwise `CQUOTED`s the ratios to their `[Min, Max]` CONFIG bounds — so its
/// tail activation is inseparable from that default constraint set. Per
/// siplot's project-wide fit invariant (estimators seed parameters only; the
/// fit defaults to all-[`Constraint::Free`]; per-parameter constraints are
/// applied through the `FitWidget`), the tails are instead seeded active and
/// free so the unconstrained fit is non-degenerate at the seed. Apply
/// `CFIXED`/`CQUOTED` through the constraint UI to reproduce silx's exact tail
/// handling.
pub fn estimate_ahypermet(x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
    let (h, c, f, bg) = estimate_height_position_fwhm(x, y)?;
    let area = h * f / fwhm_to_sigma_factor() * (2.0 * std::f64::consts::PI).sqrt();
    Some(vec![area, c, f, 0.05, 0.70, 0.05, 20.0, 0.002, bg])
}

// ---------------------------------------------------------------------------
// Iterative fit models exposed through the FitFunction trait, and fit range.
// ---------------------------------------------------------------------------

/// Which fit model (peak, step, or slit) an [`IterativeFit`] fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeakModel {
    /// Gaussian, height parameterisation: `[height, centroid, fwhm, bg]`.
    Gaussian,
    /// Gaussian, area parameterisation: `[area, centroid, fwhm, bg]`.
    GaussianArea,
    /// Asymmetric (split) Gaussian: `[height, centroid, fwhm1, fwhm2, bg]`.
    SplitGaussian,
    /// Lorentzian, height parameterisation: `[height, centroid, fwhm, bg]`.
    Lorentzian,
    /// Lorentzian, area parameterisation: `[area, centroid, fwhm, bg]`.
    LorentzianArea,
    /// Asymmetric (split) Lorentzian: `[height, centroid, fwhm1, fwhm2, bg]`.
    SplitLorentzian,
    /// Pseudo-Voigt: `[height, centroid, fwhm, eta, bg]`.
    PseudoVoigt,
    /// Pseudo-Voigt, area parameterisation: `[area, centroid, fwhm, eta, bg]`.
    AreaPseudoVoigt,
    /// Asymmetric (split) pseudo-Voigt: `[height, centroid, fwhm1, fwhm2, eta, bg]`.
    SplitPseudoVoigt,
    /// Asymmetric split pseudo-Voigt with per-side eta:
    /// `[height, centroid, fwhm1, fwhm2, eta1, eta2, bg]`.
    SplitPseudoVoigt2,
    /// Step down (descending erf edge): `[height, centroid, fwhm, bg]`.
    StepDown,
    /// Step up (ascending erf edge): `[height, centroid, fwhm, bg]`.
    StepUp,
    /// Slit (rising then falling edges): `[height, position, fwhm, beamfwhm, bg]`.
    Slit,
    /// Arctan step up: `[height, position, width, bg]`.
    AtanStepUp,
    /// Hypermet (Gaussian + short tail + long tail + step): `[area, position,
    /// fwhm, st_area_r, st_slope_r, lt_area_r, lt_slope_r, step_height_r, bg]`.
    Hypermet,
    /// Degree-2 polynomial: 3 coefficients highest-power-first (`a*x^2+b*x+c`).
    Polynomial2,
    /// Degree-3 polynomial: 4 coefficients highest-power-first.
    Polynomial3,
    /// Degree-4 polynomial: 5 coefficients highest-power-first.
    Polynomial4,
    /// Degree-5 polynomial: 6 coefficients highest-power-first.
    Polynomial5,
}

impl PeakModel {
    /// Polynomial degree for the `PolynomialN` variants, else `None`.
    fn poly_degree(self) -> Option<usize> {
        match self {
            PeakModel::Polynomial2 => Some(2),
            PeakModel::Polynomial3 => Some(3),
            PeakModel::Polynomial4 => Some(4),
            PeakModel::Polynomial5 => Some(5),
            _ => None,
        }
    }
    /// Display name for this model.
    pub fn name(self) -> &'static str {
        match self {
            PeakModel::Gaussian => "Gaussian",
            PeakModel::GaussianArea => "Gaussian (Area)",
            PeakModel::SplitGaussian => "Split Gaussian",
            PeakModel::Lorentzian => "Lorentzian",
            PeakModel::LorentzianArea => "Lorentzian (Area)",
            PeakModel::SplitLorentzian => "Split Lorentzian",
            PeakModel::PseudoVoigt => "Pseudo-Voigt",
            PeakModel::AreaPseudoVoigt => "Pseudo-Voigt (Area)",
            PeakModel::SplitPseudoVoigt => "Split Pseudo-Voigt",
            PeakModel::SplitPseudoVoigt2 => "Split Pseudo-Voigt 2",
            PeakModel::StepDown => "Step Down",
            PeakModel::StepUp => "Step Up",
            PeakModel::Slit => "Slit",
            PeakModel::AtanStepUp => "Arctan Step Up",
            PeakModel::Hypermet => "Hypermet",
            PeakModel::Polynomial2 => "Degree 2 Polynomial",
            PeakModel::Polynomial3 => "Degree 3 Polynomial",
            PeakModel::Polynomial4 => "Degree 4 Polynomial",
            PeakModel::Polynomial5 => "Degree 5 Polynomial",
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
            PeakModel::SplitGaussian | PeakModel::SplitLorentzian => vec![
                owned("Height"),
                owned("Center"),
                owned("FWHM1"),
                owned("FWHM2"),
                owned("Background"),
            ],
            PeakModel::Lorentzian => vec![
                owned("Height"),
                owned("Center"),
                owned("FWHM"),
                owned("Background"),
            ],
            PeakModel::LorentzianArea => vec![
                owned("Area"),
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
            PeakModel::AreaPseudoVoigt => vec![
                owned("Area"),
                owned("Center"),
                owned("FWHM"),
                owned("Eta"),
                owned("Background"),
            ],
            PeakModel::SplitPseudoVoigt => vec![
                owned("Height"),
                owned("Center"),
                owned("FWHM1"),
                owned("FWHM2"),
                owned("Eta"),
                owned("Background"),
            ],
            PeakModel::SplitPseudoVoigt2 => vec![
                owned("Height"),
                owned("Center"),
                owned("FWHM1"),
                owned("FWHM2"),
                owned("Eta1"),
                owned("Eta2"),
                owned("Background"),
            ],
            PeakModel::StepDown | PeakModel::StepUp => vec![
                owned("Height"),
                owned("Center"),
                owned("FWHM"),
                owned("Background"),
            ],
            PeakModel::Slit => vec![
                owned("Height"),
                owned("Center"),
                owned("FWHM"),
                owned("BeamFWHM"),
                owned("Background"),
            ],
            PeakModel::AtanStepUp => vec![
                owned("Height"),
                owned("Center"),
                owned("Width"),
                owned("Background"),
            ],
            PeakModel::Hypermet => vec![
                owned("Area"),
                owned("Center"),
                owned("FWHM"),
                owned("ST Area Ratio"),
                owned("ST Slope Ratio"),
                owned("LT Area Ratio"),
                owned("LT Slope Ratio"),
                owned("Step Height Ratio"),
                owned("Background"),
            ],
            PeakModel::Polynomial2
            | PeakModel::Polynomial3
            | PeakModel::Polynomial4
            | PeakModel::Polynomial5 => {
                // Highest-power-first coefficient labels a, b, c, … (numpy
                // `poly1d` order): `degree + 1` of them. The polynomial absorbs
                // the baseline, so there is no trailing background parameter.
                let degree = self.poly_degree().expect("polynomial variant");
                (0..=degree)
                    .map(|i| ((b'a' + i as u8) as char).to_string())
                    .collect()
            }
        }
    }

    /// Evaluate this model over `x` with the given parameter vector.
    pub fn eval(self, x: &[f64], params: &[f64]) -> Vec<f64> {
        match self {
            PeakModel::Gaussian => gaussian_model(x, params),
            PeakModel::GaussianArea => gaussian_area_model(x, params),
            PeakModel::SplitGaussian => split_gaussian_model(x, params),
            PeakModel::Lorentzian => lorentzian_model(x, params),
            PeakModel::LorentzianArea => lorentzian_area_model(x, params),
            PeakModel::SplitLorentzian => split_lorentzian_model(x, params),
            PeakModel::PseudoVoigt => pseudo_voigt_model(x, params),
            PeakModel::AreaPseudoVoigt => pseudo_voigt_area_model(x, params),
            PeakModel::SplitPseudoVoigt => split_pseudo_voigt_model(x, params),
            PeakModel::SplitPseudoVoigt2 => split_pseudo_voigt2_model(x, params),
            PeakModel::StepDown => stepdown_model(x, params),
            PeakModel::StepUp => stepup_model(x, params),
            PeakModel::Slit => slit_model(x, params),
            PeakModel::AtanStepUp => atan_stepup_model(x, params),
            PeakModel::Hypermet => hypermet_model(x, params),
            // silx `poly` theory: `numpy.poly1d(params)(x)`, coefficients
            // highest-power-first.
            PeakModel::Polynomial2
            | PeakModel::Polynomial3
            | PeakModel::Polynomial4
            | PeakModel::Polynomial5 => crate::core::background::poly_eval(params, x),
        }
    }

    /// Estimate an initial parameter vector for this model from the data.
    pub fn estimate(self, x: &[f64], y: &[f64]) -> Option<Vec<f64>> {
        match self {
            PeakModel::Gaussian => estimate_gaussian(x, y),
            PeakModel::GaussianArea => estimate_gaussian_area(x, y),
            PeakModel::SplitGaussian => estimate_split_gaussian(x, y),
            PeakModel::Lorentzian => estimate_lorentzian(x, y),
            PeakModel::LorentzianArea => estimate_lorentzian_area(x, y),
            PeakModel::SplitLorentzian => estimate_split_lorentzian(x, y),
            PeakModel::PseudoVoigt => estimate_pseudo_voigt(x, y),
            PeakModel::AreaPseudoVoigt => estimate_pseudo_voigt_area(x, y),
            PeakModel::SplitPseudoVoigt => estimate_split_pseudo_voigt(x, y),
            PeakModel::SplitPseudoVoigt2 => estimate_split_pseudo_voigt2(x, y),
            PeakModel::StepDown => estimate_stepdown(x, y),
            PeakModel::StepUp => estimate_stepup(x, y),
            PeakModel::Slit => estimate_slit(x, y),
            PeakModel::AtanStepUp => estimate_atan_stepup(x, y),
            PeakModel::Hypermet => estimate_ahypermet(x, y),
            // silx `estimate_poly`: `numpy.polyfit(x, y, degree)`, coefficients
            // highest-power-first (exact least-squares; the LM then confirms it).
            PeakModel::Polynomial2
            | PeakModel::Polynomial3
            | PeakModel::Polynomial4
            | PeakModel::Polynomial5 => {
                let degree = self.poly_degree().expect("polynomial variant");
                crate::core::background::polyfit(x, y, degree)
            }
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

    /// Per-parameter uncertainties propagated through the fit constraints —
    /// what silx's FitWidget shows in its sigma column (fitmanager.py:904-909,
    /// `infodict["uncertainties"]`). Identical to [`Self::std_errors`] for an
    /// unconstrained fit; diverges under QUOTED/FIXED/FACTOR/DELTA/SUM.
    pub fn uncertainties(&self) -> &[f64] {
        &self.solver.uncertainties
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

// ---------------------------------------------------------------------------
// Multi-peak (simultaneous) Gaussian fitting.
//
// Ports silx `fittheories.estimate_height_position_fwhm` + `functions.sum_gauss`
// (C `sum_gauss`): locate N peaks with `peak_search`, seed one (height, centre,
// FWHM) triple per peak, and fit them all at once with the constrained solver.
// This composes `peak_search` and `leastsq_constrained` into the multi-peak fit
// that `IterativeFit` (single peak) does not provide.
// ---------------------------------------------------------------------------

/// Sum of `N` Gaussians (silx `functions.sum_gauss` / C `sum_gauss`).
///
/// `params` is a flat vector of `(height, centroid, fwhm)` triples — one per
/// peak, with **no** background term. `y(x) = Σ_k height_k · exp(-0.5·((x −
/// centroid_k)/sigma_k)²)`, `sigma = fwhm / (2·√(2·ln2))`, with the C far-tail
/// guard skipping terms where `(x − centroid)/sigma > 20`. A trailing partial
/// triple (length not a multiple of 3) is ignored, as silx requires a multiple
/// of 3.
pub fn multi_gaussian_model(x: &[f64], params: &[f64]) -> Vec<f64> {
    let inv = 1.0 / fwhm_to_sigma_factor();
    let mut y = vec![0.0_f64; x.len()];
    for triple in params.chunks_exact(3) {
        let (height, centroid, fwhm) = (triple[0], triple[1], triple[2]);
        let sigma = fwhm * inv;
        if sigma == 0.0 {
            continue;
        }
        for (yi, &xi) in y.iter_mut().zip(x.iter()) {
            let dhelp = (xi - centroid) / sigma;
            if dhelp <= 20.0 {
                *yi += height * (-0.5 * dhelp * dhelp).exp();
            }
        }
    }
    y
}

/// Estimate initial parameters and fit constraints for a multi-peak Gaussian fit
/// (silx `estimate_height_position_fwhm`, default config).
///
/// Locates peaks with the edge-padded
/// [`padded_peak_search`](crate::core::peaks::padded_peak_search) — the search
/// silx's theory estimators use (`FitTheories.peak_search`,
/// fittheories.py:280-313; `search_fwhm` floored at 3, `sensitivity` floored
/// at 1; falls back to the global maximum when none are found, silx
/// `ForcePeakPresence`). Seeds each
/// peak as `(y[peak], x[peak], 5·|xspan|/n)`, refines the seeds with a quick
/// 4-iteration constrained fit (height/FWHM positive, centre quoted to ±½ of the
/// `search_fwhm`-sample x-width), and returns the refined seeds together with the
/// final per-parameter constraints (height & FWHM `Positive`, centre `Free` —
/// silx default `PositiveHeightAreaFlag`/`PositiveFwhmFlag` on, `QuotedPositionFlag`
/// off). Background removal (silx `StripBackgroundFlag`, off by default) is left
/// to the caller via [`crate::core::background`]. Returns `None` for empty or
/// length-mismatched input, or when no peak can be seeded.
pub fn estimate_multi_gaussian(
    x: &[f64],
    y: &[f64],
    search_fwhm: f64,
    sensitivity: f64,
) -> Option<(Vec<f64>, Vec<Constraint>)> {
    let npoints = y.len();
    if npoints == 0 || x.len() != npoints {
        return None;
    }
    let search_fwhm = search_fwhm.max(3.0);
    let search_sens = sensitivity.max(1.0);

    let found = crate::core::peaks::padded_peak_search(y, search_fwhm, search_sens);
    let peaks: Vec<usize> = if found.is_empty() {
        // silx ForcePeakPresence: use the (first) global maximum.
        let maxv = y.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        match y.iter().position(|&v| v == maxv) {
            Some(p) => vec![p],
            None => return None,
        }
    } else {
        found.iter().map(|p| p.index).collect()
    };
    if peaks.is_empty() {
        return None;
    }

    // silx seeds FWHM as 5 sampling intervals (in x units).
    let sig = 5.0 * (x[npoints - 1] - x[0]).abs() / npoints as f64;
    let mut param: Vec<f64> = Vec::with_capacity(peaks.len() * 3);
    let mut index_largest = 0usize;
    let mut height_largest = f64::NEG_INFINITY;
    for (k, &pi) in peaks.iter().enumerate() {
        let height = y[pi];
        // silx zeroes a near-zero position only for the first peak.
        let pos = if k == 0 && x[pi].abs() < 1.0e-16 {
            0.0
        } else {
            x[pi]
        };
        param.push(height);
        param.push(pos);
        param.push(sig);
        if height > height_largest {
            height_largest = height;
            index_largest = k;
        }
    }
    let _ = index_largest; // used by silx SameFwhmFlag (off by default).

    // Preliminary constraints for the quick refine: height & FWHM positive,
    // centre quoted around its seed.
    let sf = search_fwhm as usize;
    let (fwhmx, use_fwhmx) = if x.len() > sf {
        ((x[sf] - x[0]).abs(), true)
    } else {
        (0.0, false)
    };
    let xmin = x.iter().copied().fold(f64::INFINITY, f64::min);
    let xmax = x.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mut prelim: Vec<Constraint> = Vec::with_capacity(param.len());
    for k in 0..peaks.len() {
        let pos = param[3 * k + 1];
        prelim.push(Constraint::Positive);
        if use_fwhmx && fwhmx > 0.0 {
            prelim.push(Constraint::Quoted {
                min: pos - 0.5 * fwhmx,
                max: pos + 0.5 * fwhmx,
            });
        } else if xmax > xmin {
            prelim.push(Constraint::Quoted {
                min: xmin,
                max: xmax,
            });
        } else {
            prelim.push(Constraint::Free);
        }
        prelim.push(Constraint::Positive);
    }

    // Quick 4-iteration refine (silx max_iter=4); fall back to the raw seeds.
    let fittedpar = leastsq_constrained(
        multi_gaussian_model,
        x,
        y,
        &param,
        &prelim,
        None,
        4,
        DEFAULT_DELTACHI,
    )
    .map(|r| r.parameters)
    .unwrap_or(param);

    // Final constraints (default config): height & FWHM positive, centre free.
    let mut cons: Vec<Constraint> = Vec::with_capacity(fittedpar.len());
    for _ in 0..peaks.len() {
        cons.push(Constraint::Positive);
        cons.push(Constraint::Free);
        cons.push(Constraint::Positive);
    }

    Some((fittedpar, cons))
}

/// Fit a sum of Gaussians to `(x, y)`, discovering the peaks automatically
/// (silx `FitManager` multi-peak Gaussian fit).
///
/// Seeds the peaks with [`estimate_multi_gaussian`] then runs the full
/// constrained Levenberg-Marquardt fit ([`leastsq_constrained`]) over all peaks
/// simultaneously. The returned [`LeastSqResult::parameters`] is the flat
/// `(height, centroid, fwhm)` triple vector accepted by [`multi_gaussian_model`].
/// Returns `None` when no peak can be seeded or the solver fails. Background is
/// the caller's responsibility (see [`crate::core::background`]).
pub fn fit_multi_gaussian(
    x: &[f64],
    y: &[f64],
    search_fwhm: f64,
    sensitivity: f64,
    max_iter: usize,
    deltachi: f64,
) -> Option<LeastSqResult> {
    let (seeds, cons) = estimate_multi_gaussian(x, y, search_fwhm, sensitivity)?;
    leastsq_constrained(
        multi_gaussian_model,
        x,
        y,
        &seeds,
        &cons,
        None,
        max_iter,
        deltachi,
    )
    .ok()
}

/// Run the multi-peak Gaussian fit ([`fit_multi_gaussian`]) and package it as an
/// [`IterativeFitResult`] for display.
///
/// The fitted curve is [`multi_gaussian_model`] evaluated over the located
/// peaks; the parameter vector is the flat `(height, centre, fwhm)` triples;
/// the per-peak names are `Height i` / `Center i` / `FWHM i` (1-based); and the
/// solver covariance / chi-square come straight from the constrained solve.
/// Returns `None` when no peak is seeded or the solver fails.
pub fn fit_multi_gaussian_full(
    x: &[f64],
    y: &[f64],
    search_fwhm: f64,
    sensitivity: f64,
    max_iter: usize,
    deltachi: f64,
) -> Option<IterativeFitResult> {
    let solver = fit_multi_gaussian(x, y, search_fwhm, sensitivity, max_iter, deltachi)?;
    let y_fit = multi_gaussian_model(x, &solver.parameters);
    let mut param_names = Vec::with_capacity(solver.parameters.len());
    for peak in 0..solver.parameters.len() / 3 {
        let i = peak + 1;
        param_names.push(format!("Height {i}"));
        param_names.push(format!("Center {i}"));
        param_names.push(format!("FWHM {i}"));
    }
    let fit = FitResult {
        y_fit,
        parameters: solver.parameters.clone(),
        param_names,
    };
    Some(IterativeFitResult { fit, solver })
}

/// Fit a single [`PeakModel`] from explicit initial parameters `p0` under
/// per-parameter [`Constraint`]s (silx FitWidget parameter table → editable
/// values + constraint codes feeding `leastsq_constrained`).
///
/// `p0` and `constraints` must each have exactly one entry per model parameter
/// (the order of [`PeakModel::param_names`]). Returns `None` on a length
/// mismatch or solver failure. This is the single owner of the
/// constrained-fit-from-`p0` path; [`fit_peak_constrained`] estimates `p0` and
/// delegates here.
pub fn fit_peak_from(
    model: PeakModel,
    x: &[f64],
    y: &[f64],
    p0: &[f64],
    constraints: &[Constraint],
    max_iter: usize,
    deltachi: f64,
) -> Option<IterativeFitResult> {
    if constraints.len() != p0.len() {
        return None;
    }
    let solver = leastsq_constrained(
        |xx, pp| model.eval(xx, pp),
        x,
        y,
        p0,
        constraints,
        None,
        max_iter,
        deltachi,
    )
    .ok()?;
    let y_fit = model.eval(x, &solver.parameters);
    let fit = FitResult {
        y_fit,
        parameters: solver.parameters.clone(),
        param_names: model.param_names(),
    };
    Some(IterativeFitResult { fit, solver })
}

/// Fit a single [`PeakModel`] under per-parameter [`Constraint`]s, with the
/// initial parameters estimated from the data ([`PeakModel::estimate`]).
///
/// `constraints` must have exactly one entry per model parameter. Returns `None`
/// on estimate/solver failure or a constraint-count mismatch. Delegates to
/// [`fit_peak_from`] once the estimate is in hand.
pub fn fit_peak_constrained(
    model: PeakModel,
    x: &[f64],
    y: &[f64],
    constraints: &[Constraint],
    max_iter: usize,
    deltachi: f64,
) -> Option<IterativeFitResult> {
    let p0 = model.estimate(x, y)?;
    fit_peak_from(model, x, y, &p0, constraints, max_iter, deltachi)
}

/// Outcome of [`fit_peak_with_background`]: the peak fit on the
/// background-subtracted residual, the estimated background curve, and the
/// total displayed curve.
#[derive(Debug, Clone)]
pub struct BackgroundPeakFit {
    /// The peak fit on the background-subtracted residual. `peak.fit.parameters`
    /// are in the [`PeakModel`]'s own parameterisation; per-parameter errors come
    /// from [`IterativeFitResult::std_errors`].
    pub peak: IterativeFitResult,
    /// The background curve estimated from the data and sampled at the fit `x`
    /// (held fixed during the peak fit), length `x.len()`.
    pub background: Vec<f64>,
    /// The total curve to display, `background[i] + peak.fit.y_fit[i]`.
    pub total: Vec<f64>,
}

/// Fit `model` on top of an estimated `background`, mirroring silx `FitManager`'s
/// background-then-peak workflow.
///
/// The background is estimated from the data with [`Background`] (silx
/// `estimate_*`: the strip/snip filters, or a polynomial least-squares-fitted to
/// the strip background — silx `EstimatePolyOnStrip = True`), subtracted from
/// `y`, and the [`PeakModel`] is fitted to the residual. The background is then
/// added back to produce [`BackgroundPeakFit::total`].
///
/// Returns `None` on length mismatch, empty input, or when the peak fit fails.
///
/// # Deviation from silx
///
/// silx's background theories are `is_background=True` `FitTheory`s whose
/// parameters are concatenated with the peak parameters into ONE `leastsq`, so
/// the analytic-background coefficients (Constant / Linear / Polynomial) are
/// refined *simultaneously* with the peak. Here the background is estimated once
/// and held fixed while the peak is fitted on the residual. A simultaneous
/// refinement is blocked by siplot's single-peak [`PeakModel`]s baking in their
/// own trailing constant-background parameter: a free analytic-background
/// constant would be collinear with it (singular covariance). Removing that
/// baked-in constant is a [`PeakModel`] redesign tracked separately.
///
/// [`Background`]: crate::core::background::Background
pub fn fit_peak_with_background(
    model: PeakModel,
    background: crate::core::background::Background,
    x: &[f64],
    y: &[f64],
    max_iter: usize,
    deltachi: f64,
) -> Option<BackgroundPeakFit> {
    if x.is_empty() || x.len() != y.len() {
        return None;
    }
    let bg = background.compute(x, y);
    let residual: Vec<f64> = y.iter().zip(&bg).map(|(&yi, &bi)| yi - bi).collect();
    let fitter = IterativeFit {
        model,
        max_iter,
        deltachi,
    };
    let peak = fitter.fit_full(x, &residual)?;
    let total: Vec<f64> = bg
        .iter()
        .zip(&peak.fit.y_fit)
        .map(|(&bi, &fi)| bi + fi)
        .collect();
    Some(BackgroundPeakFit {
        peak,
        background: bg,
        total,
    })
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
    fn lm_budget_counts_lambda_attempts_like_silx() {
        // R2-28 goldens from silx leastsq itself (leastsq.py run directly with
        // numpy: model a*exp(b*x), y = exp(x), x = linspace(0, 3, 16),
        // p0 = [1, 3], no sigma, deltachi default): the overshooting seed makes
        // iteration 5 reject three λ steps, and silx charges every attempt to
        // the max_iter budget (iiter -= 1 inside the damping loop,
        // leastsq.py:470). max_iter=8 therefore stops after FIVE outer
        // iterations, not eight.
        let xs = linspace(0.0, 3.0, 16);
        let ys: Vec<f64> = xs.iter().map(|&x| x.exp()).collect();
        let expo = |x: &[f64], p: &[f64]| {
            x.iter()
                .map(|&xi| p[0] * (p[1] * xi).exp())
                .collect::<Vec<_>>()
        };

        let r8 = leastsq(expo, &xs, &ys, &[1.0, 3.0], None, 8, DEFAULT_DELTACHI).unwrap();
        assert_eq!(r8.niter, 5, "rejected-λ attempts must consume the budget");
        // Parameters match silx to summation-order noise (numpy accumulates
        // chisq/alpha/beta pairwise, Rust sequentially; ~1e-7 relative after
        // five iterations of this trajectory).
        assert!(
            (r8.parameters[0] - 0.156_263_258_615_617_27).abs() < 1e-6,
            "a {}",
            r8.parameters[0]
        );
        assert!(
            (r8.parameters[1] - 1.609_532_248_302_758).abs() < 1e-6,
            "b {}",
            r8.parameters[1]
        );

        // With budget to spare the same fit converges on silx's schedule.
        let r100 = leastsq(expo, &xs, &ys, &[1.0, 3.0], None, 100, DEFAULT_DELTACHI).unwrap();
        assert_eq!(
            r100.niter, 15,
            "converged trajectory length must match silx"
        );
        assert!((r100.parameters[0] - 1.0).abs() < 1e-6);
        assert!((r100.parameters[1] - 1.0).abs() < 1e-6);
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
    fn lorentzian_area_model_recovers_own_peak() {
        // Build data from the area model with a known area; fit recovers it.
        let xs = linspace(0.0, 20.0, 201);
        let area = 9.0;
        let ys = lorentzian_area_model(&xs, &[area, 11.0, 3.0, 0.5]);
        let fit = IterativeFit::new(PeakModel::LorentzianArea)
            .fit_full(&xs, &ys)
            .unwrap();
        let p = &fit.fit.parameters;
        assert!((p[0] - area).abs() < 5e-2, "area {}", p[0]);
        assert!((p[1] - 11.0).abs() < 1e-3, "center {}", p[1]);
        assert!((p[2] - 3.0).abs() < 1e-2, "fwhm {}", p[2]);
        assert!((p[3] - 0.5).abs() < 1e-2, "bg {}", p[3]);
        assert!(fit.reduced_chisq().unwrap() < 1e-6);
    }

    #[test]
    fn lorentzian_area_peak_value_matches_area_conversion() {
        // At the centroid, sum_alorentz reaches area / (0.5*pi*fwhm); silx's
        // estimate_alorentz converts a height seed via area = height*fwhm*0.5*pi,
        // so feeding that area back yields exactly the original height at the peak.
        let height = 4.0;
        let fwhm = 2.5;
        let area = height * fwhm * 0.5 * std::f64::consts::PI;
        let peak = lorentzian_area_model(&[7.0], &[area, 7.0, fwhm, 0.0])[0];
        assert!(
            (peak - height).abs() < 1e-12,
            "peak {peak} vs height {height}"
        );
    }

    #[test]
    fn split_gaussian_model_recovers_asymmetric_peak() {
        let xs = linspace(0.0, 20.0, 401);
        let ys = split_gaussian_model(&xs, &[5.0, 10.0, 2.0, 4.0, 0.3]);
        let fit = IterativeFit::new(PeakModel::SplitGaussian)
            .fit_full(&xs, &ys)
            .unwrap();
        let p = &fit.fit.parameters;
        assert!((p[0] - 5.0).abs() < 5e-2, "height {}", p[0]);
        assert!((p[1] - 10.0).abs() < 1e-2, "center {}", p[1]);
        assert!((p[2] - 2.0).abs() < 5e-2, "fwhm1 {}", p[2]);
        assert!((p[3] - 4.0).abs() < 5e-2, "fwhm2 {}", p[3]);
        assert!((p[4] - 0.3).abs() < 1e-2, "bg {}", p[4]);
        assert!(fit.reduced_chisq().unwrap() < 1e-4);
    }

    #[test]
    fn split_lorentzian_model_recovers_asymmetric_peak() {
        let xs = linspace(0.0, 20.0, 401);
        let ys = split_lorentzian_model(&xs, &[6.0, 9.0, 2.0, 5.0, 0.4]);
        let fit = IterativeFit::new(PeakModel::SplitLorentzian)
            .fit_full(&xs, &ys)
            .unwrap();
        let p = &fit.fit.parameters;
        assert!((p[0] - 6.0).abs() < 5e-2, "height {}", p[0]);
        assert!((p[1] - 9.0).abs() < 1e-2, "center {}", p[1]);
        assert!((p[2] - 2.0).abs() < 5e-2, "fwhm1 {}", p[2]);
        assert!((p[3] - 5.0).abs() < 5e-2, "fwhm2 {}", p[3]);
        assert!((p[4] - 0.4).abs() < 1e-2, "bg {}", p[4]);
        assert!(fit.reduced_chisq().unwrap() < 1e-4);
    }

    #[test]
    fn split_models_reduce_to_symmetric_when_fwhms_equal() {
        // fwhm1 == fwhm2 collapses the split models onto the symmetric ones.
        let xs = linspace(0.0, 20.0, 101);
        let sg = split_gaussian_model(&xs, &[5.0, 10.0, 3.0, 3.0, 0.0]);
        let g = gaussian_model(&xs, &[5.0, 10.0, 3.0, 0.0]);
        for (a, b) in sg.iter().zip(&g) {
            assert!((a - b).abs() < 1e-12, "split gauss {a} vs gauss {b}");
        }
        let sl = split_lorentzian_model(&xs, &[5.0, 10.0, 3.0, 3.0, 0.0]);
        let l = lorentzian_model(&xs, &[5.0, 10.0, 3.0, 0.0]);
        for (a, b) in sl.iter().zip(&l) {
            assert!((a - b).abs() < 1e-12, "split lorentz {a} vs lorentz {b}");
        }
    }

    #[test]
    fn pseudo_voigt_area_model_recovers_own_peak() {
        let xs = linspace(0.0, 20.0, 401);
        let area = 10.0;
        let ys = pseudo_voigt_area_model(&xs, &[area, 10.0, 2.5, 0.4, 0.3]);
        let fit = IterativeFit::new(PeakModel::AreaPseudoVoigt)
            .fit_full(&xs, &ys)
            .unwrap();
        let p = &fit.fit.parameters;
        assert!((p[0] - area).abs() < 1e-1, "area {}", p[0]);
        assert!((p[1] - 10.0).abs() < 1e-2, "center {}", p[1]);
        assert!((p[2] - 2.5).abs() < 5e-2, "fwhm {}", p[2]);
        assert!((p[3] - 0.4).abs() < 5e-2, "eta {}", p[3]);
        assert!((p[4] - 0.3).abs() < 1e-2, "bg {}", p[4]);
        assert!(fit.reduced_chisq().unwrap() < 1e-4);
    }

    #[test]
    fn split_pseudo_voigt_model_recovers_asymmetric_peak() {
        let xs = linspace(0.0, 20.0, 501);
        let ys = split_pseudo_voigt_model(&xs, &[5.0, 10.0, 2.0, 4.0, 0.4, 0.3]);
        let fit = IterativeFit::new(PeakModel::SplitPseudoVoigt)
            .fit_full(&xs, &ys)
            .unwrap();
        let p = &fit.fit.parameters;
        assert!((p[1] - 10.0).abs() < 2e-2, "center {}", p[1]);
        assert!((p[2] - 2.0).abs() < 1e-1, "fwhm1 {}", p[2]);
        assert!((p[3] - 4.0).abs() < 1e-1, "fwhm2 {}", p[3]);
        assert!((p[5] - 0.3).abs() < 2e-2, "bg {}", p[5]);
        assert!(fit.reduced_chisq().unwrap() < 1e-4);
    }

    #[test]
    fn split_pseudo_voigt2_model_recovers_per_side_eta() {
        let xs = linspace(0.0, 20.0, 501);
        let ys = split_pseudo_voigt2_model(&xs, &[5.0, 10.0, 2.5, 4.0, 0.2, 0.7, 0.3]);
        let fit = IterativeFit::new(PeakModel::SplitPseudoVoigt2)
            .fit_full(&xs, &ys)
            .unwrap();
        let p = &fit.fit.parameters;
        assert!((p[1] - 10.0).abs() < 2e-2, "center {}", p[1]);
        assert!((p[2] - 2.5).abs() < 1e-1, "fwhm1 {}", p[2]);
        assert!((p[3] - 4.0).abs() < 1e-1, "fwhm2 {}", p[3]);
        assert!((p[6] - 0.3).abs() < 2e-2, "bg {}", p[6]);
        assert!(fit.reduced_chisq().unwrap() < 1e-4);
    }

    #[test]
    fn split_pseudo_voigt_reduces_to_pseudo_voigt_when_symmetric() {
        let xs = linspace(0.0, 20.0, 101);
        let spv = split_pseudo_voigt_model(&xs, &[5.0, 10.0, 3.0, 3.0, 0.4, 0.0]);
        let pv = pseudo_voigt_model(&xs, &[5.0, 10.0, 3.0, 0.4, 0.0]);
        for (a, b) in spv.iter().zip(&pv) {
            assert!((a - b).abs() < 1e-12, "split pvoigt {a} vs pvoigt {b}");
        }
    }

    #[test]
    fn split_pseudo_voigt2_reduces_to_pseudo_voigt_when_symmetric() {
        // fwhm1==fwhm2 and eta1==eta2 collapses splitpvoigt2 onto the symmetric
        // pseudo-Voigt (the C arg 2*(x-c)/fwhm == (x-c)/(0.5*fwhm)).
        let xs = linspace(0.0, 20.0, 101);
        let spv2 = split_pseudo_voigt2_model(&xs, &[5.0, 10.0, 3.0, 3.0, 0.4, 0.4, 0.0]);
        let pv = pseudo_voigt_model(&xs, &[5.0, 10.0, 3.0, 0.4, 0.0]);
        for (a, b) in spv2.iter().zip(&pv) {
            assert!((a - b).abs() < 1e-12, "split pvoigt2 {a} vs pvoigt {b}");
        }
    }

    #[test]
    fn polynomial_models_recover_their_coefficients() {
        let xs = linspace(-3.0, 3.0, 81);
        // Degree 2 (well-conditioned): exact coefficient recovery.
        let q = [1.5, -2.0, 0.7];
        let ys = PeakModel::Polynomial2.eval(&xs, &q);
        let fit = IterativeFit::new(PeakModel::Polynomial2)
            .fit_full(&xs, &ys)
            .unwrap();
        for (got, want) in fit.fit.parameters.iter().zip(&q) {
            assert!((got - want).abs() < 1e-6, "deg2 coef {got} vs {want}");
        }
        assert!(fit.reduced_chisq().unwrap() < 1e-9);

        // Degree 3: exact coefficient recovery.
        let c3 = [0.4, -0.5, 0.6, -0.7];
        let ys3 = PeakModel::Polynomial3.eval(&xs, &c3);
        let fit3 = IterativeFit::new(PeakModel::Polynomial3)
            .fit_full(&xs, &ys3)
            .unwrap();
        for (got, want) in fit3.fit.parameters.iter().zip(&c3) {
            assert!((got - want).abs() < 1e-5, "deg3 coef {got} vs {want}");
        }

        // Degree 5: assert curve reproduction (the normal-equations polyfit
        // minimises the residual even where high-degree conditioning blurs the
        // individual coefficients).
        let c5 = [0.05, -0.1, 0.2, -0.3, 0.4, -0.5];
        let ys5 = PeakModel::Polynomial5.eval(&xs, &c5);
        let fit5 = IterativeFit::new(PeakModel::Polynomial5)
            .fit_full(&xs, &ys5)
            .unwrap();
        assert!(
            fit5.reduced_chisq().unwrap() < 1e-6,
            "deg5 chisq {}",
            fit5.reduced_chisq().unwrap()
        );
    }

    #[test]
    fn polynomial_param_names_match_degree() {
        assert_eq!(PeakModel::Polynomial2.param_names(), ["a", "b", "c"]);
        assert_eq!(PeakModel::Polynomial3.param_names(), ["a", "b", "c", "d"]);
        assert_eq!(
            PeakModel::Polynomial4.param_names(),
            ["a", "b", "c", "d", "e"]
        );
        assert_eq!(
            PeakModel::Polynomial5.param_names(),
            ["a", "b", "c", "d", "e", "f"]
        );
    }

    #[test]
    fn hypermet_model_reduces_to_area_gaussian_when_tails_zero() {
        // All tail ratios zero ⇒ Hypermet collapses onto the area Gaussian + bg.
        // The st/lt slope ratios are also zero, so the `|slope| > epsilon` guard
        // skips the short/long-tail terms entirely (no 1/slope division).
        let xs = linspace(0.0, 20.0, 201);
        let h = hypermet_model(&xs, &[12.0, 10.0, 2.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.4]);
        let g = gaussian_area_model(&xs, &[12.0, 10.0, 2.5, 0.4]);
        for (a, b) in h.iter().zip(&g) {
            assert!((a - b).abs() < 1e-12, "hypermet {a} vs area-gauss {b}");
        }
    }

    #[test]
    fn hypermet_tails_contribute_beyond_the_gaussian() {
        // With the silx Initial tail ratios active, the curve must differ
        // measurably from the tails-off (pure area-Gaussian) curve — proving the
        // short/long-tail and step terms are wired in.
        let xs = linspace(0.0, 20.0, 201);
        let area = 12.0;
        let tailed = hypermet_model(&xs, &[area, 10.0, 2.5, 0.05, 0.70, 0.05, 20.0, 0.002, 0.0]);
        let plain = hypermet_model(&xs, &[area, 10.0, 2.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let max_diff = tailed
            .iter()
            .zip(&plain)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f64, f64::max);
        let peak = plain.iter().cloned().fold(0.0_f64, f64::max);
        assert!(
            max_diff > 0.01 * peak,
            "tail contribution {max_diff} (peak {peak})"
        );
    }

    #[test]
    fn hypermet_model_recovers_own_curve() {
        // Generate from the model with the silx Initial tail ratios + a known
        // area/center/fwhm/bg, then fit via the estimator→LM path. Hypermet
        // parameters are strongly correlated (a broad long tail is nearly
        // collinear with the constant background), so the meaningful invariant is
        // curve reproduction (reduced chi-square → 0) plus the well-determined
        // centre, not individual parameter recovery.
        let xs = linspace(0.0, 20.0, 401);
        let truth = [12.0, 10.0, 2.5, 0.05, 0.70, 0.05, 20.0, 0.002, 0.3];
        let ys = hypermet_model(&xs, &truth);
        let fit = IterativeFit::new(PeakModel::Hypermet)
            .fit_full(&xs, &ys)
            .unwrap();
        let p = &fit.fit.parameters;
        assert!((p[1] - 10.0).abs() < 5e-2, "center {}", p[1]);
        assert!(
            fit.reduced_chisq().unwrap() < 1e-4,
            "chisq {}",
            fit.reduced_chisq().unwrap()
        );
    }

    #[test]
    fn estimate_ahypermet_seeds_silx_initial_tail_ratios() {
        let xs = linspace(0.0, 20.0, 201);
        let ys = hypermet_model(&xs, &[12.0, 10.0, 2.5, 0.05, 0.70, 0.05, 20.0, 0.002, 0.3]);
        let seed = estimate_ahypermet(&xs, &ys).unwrap();
        assert_eq!(seed.len(), 9, "9 seed params");
        assert!((seed[1] - 10.0).abs() < 0.2, "center seed {}", seed[1]);
        assert_eq!(seed[3], 0.05, "ST area ratio seed");
        assert_eq!(seed[4], 0.70, "ST slope ratio seed");
        assert_eq!(seed[5], 0.05, "LT area ratio seed");
        assert_eq!(seed[6], 20.0, "LT slope ratio seed");
        assert_eq!(seed[7], 0.002, "step height ratio seed");
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
            uncertainties: vec![2.0, 3.0, 4.0],
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
            uncertainties: vec![0.0],
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

    // --- Constrained leastsq (silx constraint codes) -----------------------

    // Straight line y = p[0]*x + p[1].
    fn line(x: &[f64], p: &[f64]) -> Vec<f64> {
        x.iter().map(|&xi| p[0] * xi + p[1]).collect()
    }
    // Constant y = p[0].
    fn constant(x: &[f64], p: &[f64]) -> Vec<f64> {
        vec![p[0]; x.len()]
    }

    #[test]
    fn constrained_all_free_matches_unconstrained() {
        let xs = linspace(-5.0, 5.0, 21);
        let ys: Vec<f64> = xs.iter().map(|&x| 2.5 * x - 1.0).collect();
        let free = leastsq_constrained(
            line,
            &xs,
            &ys,
            &[0.0, 0.0],
            &[Constraint::Free, Constraint::Free],
            None,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        let plain = leastsq(
            line,
            &xs,
            &ys,
            &[0.0, 0.0],
            None,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        assert!((free.parameters[0] - plain.parameters[0]).abs() < 1e-6);
        assert!((free.parameters[1] - plain.parameters[1]).abs() < 1e-6);
        assert!((free.parameters[0] - 2.5).abs() < 1e-6);
        assert!((free.parameters[1] + 1.0).abs() < 1e-6);
    }

    #[test]
    fn constrained_fixed_holds_parameter() {
        let xs = linspace(-5.0, 5.0, 21);
        let ys: Vec<f64> = xs.iter().map(|&x| 2.5 * x - 1.0).collect();
        // b fixed at its true value -1.0; only a is fitted and must recover 2.5.
        let res = leastsq_constrained(
            line,
            &xs,
            &ys,
            &[0.0, -1.0],
            &[Constraint::Free, Constraint::Fixed],
            None,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        assert_eq!(res.parameters[1], -1.0, "fixed b must not move");
        assert!(
            (res.parameters[0] - 2.5).abs() < 1e-6,
            "a {}",
            res.parameters[0]
        );
    }

    #[test]
    fn constrained_fixed_gets_full_uncertainty() {
        let xs = linspace(-5.0, 5.0, 21);
        let ys: Vec<f64> = xs.iter().map(|&x| 2.5 * x - 1.0).collect();
        let res = leastsq_constrained(
            line,
            &xs,
            &ys,
            &[0.0, -1.0],
            &[Constraint::Free, Constraint::Fixed],
            None,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        // silx: fixed parameter gets covariance diag = value^2 and uncertainty = value.
        assert_eq!(res.uncertainties[1], res.parameters[1]);
        assert!((res.covariance[1][1] - res.parameters[1] * res.parameters[1]).abs() < 1e-12);
    }

    #[test]
    fn constrained_positive_enforces_and_recovers() {
        let xs = linspace(0.0, 10.0, 21);
        // Negative target: the abs reparametrisation keeps the parameter
        // non-negative, so it can never reach -3 and the residual stays large.
        let neg: Vec<f64> = vec![-3.0; xs.len()];
        let r_neg = leastsq_constrained(
            constant,
            &xs,
            &neg,
            &[1.0],
            &[Constraint::Positive],
            None,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        assert!(
            r_neg.parameters[0] >= 0.0,
            "positive violated: {}",
            r_neg.parameters[0]
        );
        assert!(
            r_neg.chisq > 1.0,
            "constraint should prevent fitting the negative target, chisq {}",
            r_neg.chisq
        );
        // Positive target: behaves like a normal fit, recovering 4.
        let pos: Vec<f64> = vec![4.0; xs.len()];
        let r_pos = leastsq_constrained(
            constant,
            &xs,
            &pos,
            &[1.0],
            &[Constraint::Positive],
            None,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        assert!(
            (r_pos.parameters[0] - 4.0).abs() < 0.1,
            "recover {}",
            r_pos.parameters[0]
        );
    }

    #[test]
    fn constrained_quoted_clamps_to_bounds() {
        let xs = linspace(0.0, 10.0, 21);
        // Target 10 is above the [0,5] bound: the fit saturates near 5.
        let high: Vec<f64> = vec![10.0; xs.len()];
        let r_hi = leastsq_constrained(
            constant,
            &xs,
            &high,
            &[2.5],
            &[Constraint::Quoted { min: 0.0, max: 5.0 }],
            None,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        assert!(
            (0.0..=5.0).contains(&r_hi.parameters[0]),
            "out of bounds: {}",
            r_hi.parameters[0]
        );
        assert!(
            r_hi.parameters[0] > 4.5,
            "did not saturate near 5: {}",
            r_hi.parameters[0]
        );
        // Target 3 is inside the bound and is recovered.
        let mid: Vec<f64> = vec![3.0; xs.len()];
        let r_mid = leastsq_constrained(
            constant,
            &xs,
            &mid,
            &[2.5],
            &[Constraint::Quoted { min: 0.0, max: 5.0 }],
            None,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        assert!(
            (r_mid.parameters[0] - 3.0).abs() < 0.05,
            "recover {}",
            r_mid.parameters[0]
        );
    }

    #[test]
    fn constrained_factor_ties_parameters() {
        let xs = linspace(-5.0, 5.0, 21);
        // y = a*x + b with b = 2*a; true a = 3 => b = 6.
        let ys: Vec<f64> = xs.iter().map(|&x| 3.0 * x + 6.0).collect();
        let res = leastsq_constrained(
            line,
            &xs,
            &ys,
            &[1.0, 0.0],
            &[
                Constraint::Free,
                Constraint::Factor {
                    reference: 0,
                    factor: 2.0,
                },
            ],
            None,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        assert!(
            (res.parameters[0] - 3.0).abs() < 1e-4,
            "a {}",
            res.parameters[0]
        );
        assert!(
            (res.parameters[1] - 6.0).abs() < 1e-4,
            "b {}",
            res.parameters[1]
        );
        assert!(
            (res.parameters[1] - 2.0 * res.parameters[0]).abs() < 1e-9,
            "tie broken"
        );
    }

    #[test]
    fn factor_tied_sigma_scales_by_the_factor() {
        // R2-31: silx CFACTOR scales the reference sigma by the tie factor
        // (leastsq.py:875-876); CDELTA and CSUM copy it unscaled (:877-880).
        let sigmas = get_sigma_parameters(
            &[3.0, 0.0, 0.0, 0.0],
            &[0.5],
            &[
                Constraint::Free,
                Constraint::Factor {
                    reference: 0,
                    factor: 2.0,
                },
                Constraint::Delta {
                    reference: 0,
                    delta: 5.0,
                },
                Constraint::Sum {
                    reference: 0,
                    sum: 9.0,
                },
            ],
        );
        assert_eq!(sigmas[0], 0.5);
        assert_eq!(sigmas[1], 1.0, "FACTOR sigma must be factor * reference");
        assert_eq!(sigmas[2], 0.5, "DELTA sigma copies the reference unscaled");
        assert_eq!(sigmas[3], 0.5, "SUM sigma copies the reference unscaled");
    }

    #[test]
    fn constrained_delta_ties_parameters() {
        let xs = linspace(-5.0, 5.0, 21);
        // b = a + 5; true a = 2 => b = 7.
        let ys: Vec<f64> = xs.iter().map(|&x| 2.0 * x + 7.0).collect();
        let res = leastsq_constrained(
            line,
            &xs,
            &ys,
            &[0.0, 0.0],
            &[
                Constraint::Free,
                Constraint::Delta {
                    reference: 0,
                    delta: 5.0,
                },
            ],
            None,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        assert!(
            (res.parameters[0] - 2.0).abs() < 1e-4,
            "a {}",
            res.parameters[0]
        );
        assert!(
            (res.parameters[1] - res.parameters[0] - 5.0).abs() < 1e-9,
            "tie broken"
        );
    }

    #[test]
    fn constrained_sum_ties_parameters() {
        let xs = linspace(-5.0, 5.0, 21);
        // b = 10 - a; true a = 4 => b = 6.
        let ys: Vec<f64> = xs.iter().map(|&x| 4.0 * x + 6.0).collect();
        let res = leastsq_constrained(
            line,
            &xs,
            &ys,
            &[0.0, 0.0],
            &[
                Constraint::Free,
                Constraint::Sum {
                    reference: 0,
                    sum: 10.0,
                },
            ],
            None,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        assert!(
            (res.parameters[0] - 4.0).abs() < 1e-4,
            "a {}",
            res.parameters[0]
        );
        assert!(
            (res.parameters[0] + res.parameters[1] - 10.0).abs() < 1e-9,
            "tie broken"
        );
    }

    #[test]
    fn constrained_rejects_bad_spec() {
        let xs = linspace(0.0, 4.0, 5);
        let ys = vec![1.0; 5];
        // constraints length mismatch.
        assert_eq!(
            leastsq_constrained(constant, &xs, &ys, &[1.0], &[], None, 10, DEFAULT_DELTACHI)
                .unwrap_err(),
            FitError::BadConstraintReference
        );
        // equal Quoted bounds (B == 0).
        assert_eq!(
            leastsq_constrained(
                constant,
                &xs,
                &ys,
                &[1.0],
                &[Constraint::Quoted { min: 5.0, max: 5.0 }],
                None,
                10,
                DEFAULT_DELTACHI,
            )
            .unwrap_err(),
            FitError::InvalidConstraint
        );
        // Factor referencing a non-existent parameter.
        assert_eq!(
            leastsq_constrained(
                line,
                &xs,
                &ys,
                &[1.0, 0.0],
                &[
                    Constraint::Free,
                    Constraint::Factor {
                        reference: 9,
                        factor: 2.0
                    }
                ],
                None,
                10,
                DEFAULT_DELTACHI,
            )
            .unwrap_err(),
            FitError::BadConstraintReference
        );
        // No free parameters at all.
        assert_eq!(
            leastsq_constrained(
                line,
                &xs,
                &ys,
                &[1.0, 2.0],
                &[Constraint::Fixed, Constraint::Fixed],
                None,
                10,
                DEFAULT_DELTACHI,
            )
            .unwrap_err(),
            FitError::NoFreeParameters
        );
    }

    // --- Multi-peak (simultaneous) Gaussian fit ----------------------------

    fn grid(n: usize) -> Vec<f64> {
        (0..n).map(|i| i as f64).collect()
    }

    /// Find the fitted triple whose centre is nearest `target`.
    fn nearest_peak(params: &[f64], target: f64) -> [f64; 3] {
        params
            .chunks_exact(3)
            .min_by(|a, b| {
                (a[1] - target)
                    .abs()
                    .partial_cmp(&(b[1] - target).abs())
                    .unwrap()
            })
            .map(|t| [t[0], t[1], t[2]])
            .unwrap()
    }

    #[test]
    fn multi_gaussian_model_is_sum_of_single_gaussians() {
        let xs = grid(100);
        // Two peaks; compare against two single gaussian_model calls (bg = 0).
        let a = gaussian_model(&xs, &[100.0, 30.0, 8.0, 0.0]);
        let b = gaussian_model(&xs, &[60.0, 70.0, 5.0, 0.0]);
        let sum = multi_gaussian_model(&xs, &[100.0, 30.0, 8.0, 60.0, 70.0, 5.0]);
        for i in 0..xs.len() {
            assert!((sum[i] - (a[i] + b[i])).abs() < 1e-9, "mismatch at {i}");
        }
    }

    #[test]
    fn fit_multi_gaussian_recovers_two_peaks() {
        let xs = grid(100);
        let mut ys = gaussian_model(&xs, &[100.0, 30.0, 8.0, 0.0]);
        for (yi, g) in ys
            .iter_mut()
            .zip(gaussian_model(&xs, &[80.0, 70.0, 6.0, 0.0]))
        {
            *yi += g;
        }
        let res = fit_multi_gaussian(
            &xs,
            &ys,
            8.0,
            DEFAULT_FIT_SENSITIVITY,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .expect("multi-peak fit should succeed");
        assert!(res.parameters.len() >= 6, "expected >=2 peaks");
        let p1 = nearest_peak(&res.parameters, 30.0);
        let p2 = nearest_peak(&res.parameters, 70.0);
        assert!((p1[1] - 30.0).abs() < 1.0, "centre1 {}", p1[1]);
        assert!((p1[0] - 100.0).abs() < 5.0, "height1 {}", p1[0]);
        assert!((p1[2] - 8.0).abs() < 1.0, "fwhm1 {}", p1[2]);
        assert!((p2[1] - 70.0).abs() < 1.0, "centre2 {}", p2[1]);
        assert!((p2[0] - 80.0).abs() < 5.0, "height2 {}", p2[0]);
        assert!((p2[2] - 6.0).abs() < 1.0, "fwhm2 {}", p2[2]);
    }

    #[test]
    fn fit_multi_gaussian_full_packages_names_errors_and_curve() {
        let xs = grid(100);
        let mut ys = gaussian_model(&xs, &[100.0, 30.0, 8.0, 0.0]);
        for (yi, g) in ys
            .iter_mut()
            .zip(gaussian_model(&xs, &[80.0, 70.0, 6.0, 0.0]))
        {
            *yi += g;
        }
        let ir = fit_multi_gaussian_full(
            &xs,
            &ys,
            8.0,
            DEFAULT_FIT_SENSITIVITY,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .expect("multi-peak fit should succeed");
        let n = ir.fit.parameters.len();
        assert!(n >= 6 && n.is_multiple_of(3), "param count {n}");
        // Names, values, and errors all line up for the results table.
        assert_eq!(ir.fit.param_names.len(), n);
        assert_eq!(ir.std_errors().len(), n);
        assert_eq!(ir.fit.y_fit.len(), xs.len());
        // Per-peak naming is 1-based Height/Center/FWHM triples.
        assert_eq!(ir.fit.param_names[0], "Height 1");
        assert_eq!(ir.fit.param_names[1], "Center 1");
        assert_eq!(ir.fit.param_names[2], "FWHM 1");
        // The packaged curve equals the model evaluated at the fitted params.
        assert_eq!(ir.fit.y_fit, multi_gaussian_model(&xs, &ir.fit.parameters));
        // Both peaks recovered.
        assert!((nearest_peak(&ir.fit.parameters, 30.0)[1] - 30.0).abs() < 1.0);
        assert!((nearest_peak(&ir.fit.parameters, 70.0)[1] - 70.0).abs() < 1.0);
    }

    #[test]
    fn fit_peak_constrained_all_free_recovers_peak() {
        let xs = grid(100);
        let ys = gaussian_model(&xs, &[60.0, 45.0, 7.0, 5.0]);
        let cons = vec![Constraint::Free; 4];
        let ir = fit_peak_constrained(
            PeakModel::Gaussian,
            &xs,
            &ys,
            &cons,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        let p = &ir.fit.parameters;
        assert!((p[1] - 45.0).abs() < 1.0, "centre {}", p[1]);
        assert!((p[2] - 7.0).abs() < 1.0, "fwhm {}", p[2]);
        assert_eq!(ir.fit.param_names, PeakModel::Gaussian.param_names());
    }

    #[test]
    fn fit_peak_constrained_fixed_holds_param_at_estimate() {
        let xs = grid(100);
        let ys = gaussian_model(&xs, &[60.0, 45.0, 7.0, 5.0]);
        // The fixed parameter must stay bit-identical to its estimate.
        let p0 = PeakModel::Gaussian.estimate(&xs, &ys).unwrap();
        let mut cons = vec![Constraint::Free; 4];
        cons[1] = Constraint::Fixed; // hold the centre
        let ir = fit_peak_constrained(
            PeakModel::Gaussian,
            &xs,
            &ys,
            &cons,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        assert_eq!(ir.fit.parameters[1], p0[1]);
    }

    #[test]
    fn fit_peak_from_uses_explicit_p0_and_recovers() {
        let xs = grid(100);
        let ys = gaussian_model(&xs, &[60.0, 45.0, 7.0, 5.0]);
        // A deliberately-off initial guess; LM should still converge.
        let p0 = [20.0, 40.0, 12.0, 0.0];
        let cons = vec![Constraint::Free; 4];
        let ir = fit_peak_from(
            PeakModel::Gaussian,
            &xs,
            &ys,
            &p0,
            &cons,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        let p = &ir.fit.parameters;
        assert!((p[1] - 45.0).abs() < 1.0, "centre {}", p[1]);
        assert!((p[2] - 7.0).abs() < 1.0, "fwhm {}", p[2]);
        // fit_peak_constrained estimates p0 then delegates here, so the two
        // agree when started from the estimate.
        let est = PeakModel::Gaussian.estimate(&xs, &ys).unwrap();
        let via_estimate = fit_peak_from(
            PeakModel::Gaussian,
            &xs,
            &ys,
            &est,
            &cons,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        let direct = fit_peak_constrained(
            PeakModel::Gaussian,
            &xs,
            &ys,
            &cons,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        assert_eq!(via_estimate.fit.parameters, direct.fit.parameters);
    }

    #[test]
    fn fit_peak_constrained_rejects_count_mismatch() {
        let xs = grid(50);
        let ys = gaussian_model(&xs, &[60.0, 25.0, 7.0, 0.0]);
        // Gaussian has 4 parameters; a 3-entry constraint vector is rejected.
        assert!(
            fit_peak_constrained(
                PeakModel::Gaussian,
                &xs,
                &ys,
                &[Constraint::Free; 3],
                DEFAULT_MAX_ITER,
                DEFAULT_DELTACHI,
            )
            .is_none()
        );
    }

    #[test]
    fn fit_multi_gaussian_single_peak() {
        let xs = grid(100);
        let ys = gaussian_model(&xs, &[50.0, 45.0, 7.0, 0.0]);
        let res = fit_multi_gaussian(
            &xs,
            &ys,
            7.0,
            DEFAULT_FIT_SENSITIVITY,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        let p = nearest_peak(&res.parameters, 45.0);
        assert!((p[0] - 50.0).abs() < 2.0, "height {}", p[0]);
        assert!((p[1] - 45.0).abs() < 0.5, "centre {}", p[1]);
        assert!((p[2] - 7.0).abs() < 0.5, "fwhm {}", p[2]);
    }

    #[test]
    fn estimate_multi_gaussian_seeds_height_and_position() {
        let xs = grid(100);
        let ys = gaussian_model(&xs, &[50.0, 45.0, 7.0, 0.0]);
        let (seeds, cons) =
            estimate_multi_gaussian(&xs, &ys, 7.0, DEFAULT_FIT_SENSITIVITY).unwrap();
        assert_eq!(seeds.len() % 3, 0);
        assert_eq!(seeds.len(), cons.len());
        // Final constraints: height Positive, centre Free, FWHM Positive.
        assert_eq!(cons[0], Constraint::Positive);
        assert_eq!(cons[1], Constraint::Free);
        assert_eq!(cons[2], Constraint::Positive);
    }

    #[test]
    fn estimate_multi_gaussian_rejects_empty_and_mismatch() {
        assert!(estimate_multi_gaussian(&[], &[], 5.0, 2.5).is_none());
        assert!(estimate_multi_gaussian(&[0.0, 1.0], &[1.0], 5.0, 2.5).is_none());
    }

    #[test]
    fn estimate_multi_gaussian_seeds_an_edge_adjacent_peak() {
        // The estimator searches with silx's edge-padded FitTheories
        // peak_search (fittheories.py:280-313): a peak whose apex sits on the
        // last sample is seeded alongside the interior peak (the raw unpadded
        // search never sees it fall, so it seeded only 1 peak before).
        let xs = grid(100);
        let ys: Vec<f64> = xs
            .iter()
            .map(|&x| {
                gaussian_model(&[x], &[100.0, 40.0, 8.0, 0.0])[0]
                    + gaussian_model(&[x], &[80.0, 99.0, 8.0, 0.0])[0]
            })
            .collect();
        let (seeds, _) = estimate_multi_gaussian(&xs, &ys, 8.0, DEFAULT_FIT_SENSITIVITY).unwrap();
        assert_eq!(seeds.len(), 6, "both peaks seeded, got {seeds:?}");
        // One refined centre near 40, one near the right edge.
        let centers: Vec<f64> = seeds.chunks(3).map(|c| c[1]).collect();
        assert!(
            centers.iter().any(|&c| (c - 40.0).abs() < 2.0),
            "{centers:?}"
        );
        assert!(
            centers.iter().any(|&c| (c - 99.0).abs() < 2.0),
            "{centers:?}"
        );
    }

    #[test]
    fn fit_manager_sensitivity_seeds_marginal_peaks_the_pyx_default_misses() {
        // silx FitManager estimation searches with DEFAULT_CONFIG
        // ["Sensitivity"] = 2.5 (fittheories.py:107, passed as search_sens at
        // :338/:356), not the standalone peak_search pyx default of 3.5. On a
        // high baseline, a small peak's significance (~curvature / sqrt(level))
        // can land between the two: here the h=100 peak at x=140 on a 10000
        // baseline is found at 2.5 but dropped at 3.5, so the FitManager
        // default seeds 2 peaks (6 params) where 3.5 would seed only 1.
        assert_eq!(DEFAULT_FIT_SENSITIVITY, 2.5);
        let fwhm = 8.0;
        let xs = grid(200);
        let ys: Vec<f64> = xs
            .iter()
            .map(|&x| {
                10000.0
                    + gaussian_model(&[x], &[2000.0, 60.0, fwhm, 0.0])[0]
                    + gaussian_model(&[x], &[100.0, 140.0, fwhm, 0.0])[0]
            })
            .collect();
        let (seeds_fit, _) =
            estimate_multi_gaussian(&xs, &ys, fwhm, DEFAULT_FIT_SENSITIVITY).unwrap();
        assert_eq!(
            seeds_fit.len(),
            6,
            "FitManager sensitivity must seed both peaks"
        );
        let (seeds_pyx, _) =
            estimate_multi_gaussian(&xs, &ys, fwhm, crate::core::peaks::DEFAULT_PEAK_SENSITIVITY)
                .unwrap();
        assert_eq!(
            seeds_pyx.len(),
            3,
            "the pyx default 3.5 drops the marginal peak"
        );
    }

    // --- Non-peak theories: erf, step/slit/atan models, estimators ---------

    #[test]
    fn erf_matches_known_values() {
        // Reference values to full precision; A&S 7.1.26 is good to ~1.5e-7.
        assert_eq!(erf(0.0), 0.0);
        assert!((erf(0.5) - 0.520_499_877_813_046_5).abs() < 1e-6);
        assert!((erf(1.0) - 0.842_700_792_949_714_9).abs() < 1e-6);
        assert!((erf(2.0) - 0.995_322_265_018_952_7).abs() < 1e-6);
        // Odd symmetry.
        assert!((erf(-1.0) + erf(1.0)).abs() < 1e-12);
    }

    #[test]
    fn erfc_keeps_relative_precision_into_the_far_tail() {
        // R2-30: `1 - erf(x)` collapses to exactly 0 for x >= ~5.9; the
        // vendored fdlibm `erfc` computes the tail directly, keeping <1 ulp
        // relative accuracy like the libm `erfc` silx links. References are
        // libm values (Python `math.erfc`).
        for &(x, reference) in &[
            (0.5, 0.479_500_122_186_953_5),
            (2.0, 4.677_734_981_047_264_5e-3),
            (5.0, 1.537_459_794_428_035e-12),
            (6.0, 2.151_973_671_249_891_3e-17),
            (10.0, 2.088_487_583_762_544_6e-45),
            (15.0, 7.212_994_172_451_208e-100),
            (26.0, 5.663_192_408_856_143e-296),
        ] {
            let rel = (erfc(x) - reference) / reference;
            assert!(rel.abs() < 1e-13, "erfc({x}) rel err {rel:e}");
        }
        // Exact anchor points and the negative branch.
        assert_eq!(erfc(0.0), 1.0);
        assert!((erfc(-2.0) - (2.0 - erfc(2.0))).abs() < 1e-15);
        assert!((erfc(-10.0) - 2.0).abs() < 1e-12);
        // erfc = 1 - erf to rounding error where both are O(1).
        for &x in &[-2.0, -0.3, 0.0, 0.7, 1.5] {
            assert!((erfc(x) - (1.0 - erf(x))).abs() < 1e-15);
        }
    }

    #[test]
    fn hypermet_tail_survives_the_large_erfc_argument_regime() {
        // R2-30 end-to-end: sigma=5, st_slope=0.7, dx=+5 puts the short-tail
        // erfc argument at w = 5.7578… — where `1 - erf` was exactly 0, zeroing
        // the tail (and its LM gradient). Reference computed with libm erfc:
        // gauss 4.839414490382867 + tail 4.183776300786361.
        let fwhm = 11.774_100_225_154_747; // sigma = 5.0
        let params = [100.0, 50.0, fwhm, 1.0, 0.7, 0.0, 0.0, 0.0, 0.0];
        let y = hypermet_model(&[55.0], &params)[0];
        let reference = 9.023_190_791_169_228;
        assert!(
            ((y - reference) / reference).abs() < 1e-9,
            "hypermet tail regime: {y} vs {reference}"
        );
    }

    #[test]
    fn convolve_valid_matches_numpy_reversed_kernel() {
        // numpy reverses the kernel: out[k] = sum_j y[k+j] * kernel[m-1-j].
        // [1,0,0] picks y[k+2], so valid output is [y[2], y[3]].
        assert_eq!(
            convolve_valid(&[1.0, 2.0, 3.0, 4.0], &[1.0, 0.0, 0.0]),
            vec![3.0, 4.0]
        );
        // Step-up edge kernel on a ramp: single 'valid' output = 2.5.
        let out = convolve_valid(&[1.0, 2.0, 3.0, 4.0, 5.0], &[0.25, 0.75, 0.0, -0.75, -0.25]);
        assert_eq!(out.len(), 1);
        assert!((out[0] - 2.5).abs() < 1e-12);
        // Kernel longer than data → empty.
        assert!(convolve_valid(&[1.0], &[1.0, 2.0]).is_empty());
    }

    #[test]
    fn stepup_model_half_height_at_center_and_asymptotes() {
        let p = [10.0, 0.0, 4.0, 2.0]; // height, center, fwhm, bg
        // erf(0)=0 exactly → exactly bg + height/2 at the centre.
        assert_eq!(stepup_model(&[0.0], &p)[0], 7.0);
        // Asymptotes: bg on the far left, bg+height on the far right.
        assert!((stepup_model(&[-1000.0], &p)[0] - 2.0).abs() < 1e-9);
        assert!((stepup_model(&[1000.0], &p)[0] - 12.0).abs() < 1e-9);
        // Monotone increasing.
        let xs = linspace(-20.0, 20.0, 81);
        let ys = stepup_model(&xs, &p);
        assert!(ys.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn stepdown_model_half_height_at_center_and_asymptotes() {
        let p = [10.0, 0.0, 4.0, 2.0];
        assert_eq!(stepdown_model(&[0.0], &p)[0], 7.0); // erfc(0)=1
        assert!((stepdown_model(&[-1000.0], &p)[0] - 12.0).abs() < 1e-9);
        assert!((stepdown_model(&[1000.0], &p)[0] - 2.0).abs() < 1e-9);
        let xs = linspace(-20.0, 20.0, 81);
        let ys = stepdown_model(&xs, &p);
        assert!(ys.windows(2).all(|w| w[1] <= w[0]));
    }

    #[test]
    fn atan_stepup_model_half_height_at_center_and_monotonic() {
        let p = [10.0, 0.0, 4.0, 2.0]; // height, position, width, bg
        assert_eq!(atan_stepup_model(&[0.0], &p)[0], 7.0); // atan(0)=0
        assert!((atan_stepup_model(&[-1.0e8], &p)[0] - 2.0).abs() < 1e-6);
        assert!((atan_stepup_model(&[1.0e8], &p)[0] - 12.0).abs() < 1e-6);
        let xs = linspace(-50.0, 50.0, 101);
        let ys = atan_stepup_model(&xs, &p);
        assert!(ys.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn slit_model_is_symmetric_and_localised() {
        let p = [10.0, 0.0, 10.0, 2.0, 1.0]; // height, position, fwhm, beamfwhm, bg
        // Symmetric about the position.
        for &d in &[1.0, 3.0, 7.0, 12.0] {
            let a = slit_model(&[d], &p)[0];
            let b = slit_model(&[-d], &p)[0];
            assert!((a - b).abs() < 1e-12, "slit asymmetric at {d}: {a} vs {b}");
        }
        // Inside the slit it sits above the background; far outside it is bg.
        assert!(slit_model(&[0.0], &p)[0] > 5.0);
        assert!((slit_model(&[1000.0], &p)[0] - 1.0).abs() < 1e-9);
        assert!((slit_model(&[-1000.0], &p)[0] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn estimate_stepup_recovers_center_height_bg() {
        // silx estimate_stepup (fittheories.py:1150-1157): on a positive
        // baseline (min(y) = 3 > 0) the seeded height is the rescaled
        // derivative-peak height ≈ max(y) = 11, not the amplitude
        // max - min = 8 (which estimate_stepdown keeps).
        let xs = linspace(0.0, 100.0, 101);
        let ys = stepup_model(&xs, &[8.0, 40.0, 6.0, 3.0]);
        let s = estimate_stepup(&xs, &ys).unwrap();
        assert!((s[0] - 11.0).abs() < 0.5, "height seed {}", s[0]);
        assert!((s[1] - 40.0).abs() < 3.0, "center seed {}", s[1]);
        assert!((s[3] - 3.0).abs() < 0.5, "bg seed {}", s[3]);
    }

    #[test]
    fn estimate_stepup_zero_baseline_height_is_the_amplitude() {
        // Boundary: with min(y) = 0 the rescaled derivative peak ≈ max(y)
        // = amplitude, so the height seed stays ≈ 8 either way.
        let xs = linspace(0.0, 100.0, 101);
        let ys = stepup_model(&xs, &[8.0, 40.0, 6.0, 0.0]);
        let s = estimate_stepup(&xs, &ys).unwrap();
        assert!((s[0] - 8.0).abs() < 0.5, "height seed {}", s[0]);
    }

    #[test]
    fn estimate_atan_stepup_inherits_the_stepup_height_rule() {
        // silx's Atan theory estimates via estimate_stepup
        // (fittheories.py:1466), so it seeds ≈ max(y) on a positive baseline
        // too.
        let xs = linspace(0.0, 100.0, 101);
        let ys = stepup_model(&xs, &[8.0, 40.0, 6.0, 3.0]);
        let s = estimate_atan_stepup(&xs, &ys).unwrap();
        assert!((s[0] - 11.0).abs() < 0.5, "height seed {}", s[0]);
    }

    #[test]
    fn estimate_stepdown_recovers_center_height_bg() {
        let xs = linspace(0.0, 100.0, 101);
        let ys = stepdown_model(&xs, &[8.0, 40.0, 6.0, 3.0]);
        let s = estimate_stepdown(&xs, &ys).unwrap();
        assert!((s[0] - 8.0).abs() < 0.5, "height seed {}", s[0]);
        assert!((s[1] - 40.0).abs() < 3.0, "center seed {}", s[1]);
        assert!((s[3] - 3.0).abs() < 0.5, "bg seed {}", s[3]);
    }

    #[test]
    fn estimate_slit_centers_on_the_slit() {
        let xs = linspace(0.0, 100.0, 201);
        let ys = slit_model(&xs, &[10.0, 50.0, 20.0, 3.0, 2.0]);
        let s = estimate_slit(&xs, &ys).unwrap();
        assert_eq!(s.len(), 5);
        assert!((s[1] - 50.0).abs() < 2.0, "position seed {}", s[1]);
        // On a positive x domain the raw beamfwhm (>= 0.5 * centre_down) always
        // exceeds edge_distance / 10, so the min clamp binds: 20 / 10 = 2
        // (silx fittheories.py:1077).
        assert!((s[3] - 2.0).abs() < 1e-9, "beamfwhm seed {}", s[3]);
    }

    #[test]
    fn estimate_slit_beamfwhm_reproduces_the_silx_index_quirk() {
        // silx seeds beamfwhm = 0.5 * (largestup[2] + largestdown[1]) — the up
        // FWHM averaged with the down-step CENTRE, not the down FWHM
        // (fittheories.py:1076; the docstring at :1065 says "average of FWHM"
        // but the code indexes the centre). On a negative x domain the down
        // centre is negative, the raw value goes negative, the min clamp keeps
        // it, and the max clamp floors it at range * 3 / len(x)
        // (fittheories.py:1077-1078) — whereas averaging the two FWHMs (both
        // positive, ~22 here) would clamp at edge_distance / 10 = 2 instead.
        let xs = linspace(-100.0, 0.0, 201);
        let ys = slit_model(&xs, &[10.0, -50.0, 20.0, 3.0, 2.0]);
        let s = estimate_slit(&xs, &ys).unwrap();
        let floor = 100.0 * 3.0 / 201.0;
        assert!(
            (s[3] - floor).abs() < 1e-9,
            "beamfwhm {} must sit on the range*3/n floor {}",
            s[3],
            floor
        );
    }

    #[test]
    fn peak_model_step_variants_delegate() {
        assert_eq!(PeakModel::StepUp.name(), "Step Up");
        assert_eq!(PeakModel::StepDown.name(), "Step Down");
        assert_eq!(PeakModel::Slit.name(), "Slit");
        assert_eq!(PeakModel::AtanStepUp.name(), "Arctan Step Up");
        assert_eq!(
            PeakModel::StepUp.param_names(),
            vec!["Height", "Center", "FWHM", "Background"]
        );
        assert_eq!(PeakModel::Slit.param_names().len(), 5);
        assert_eq!(
            PeakModel::AtanStepUp.param_names(),
            vec!["Height", "Center", "Width", "Background"]
        );
        // eval delegates to the matching model function.
        let xs = linspace(-5.0, 5.0, 11);
        let p = [3.0, 0.0, 2.0, 1.0];
        assert_eq!(PeakModel::StepUp.eval(&xs, &p), stepup_model(&xs, &p));
        assert_eq!(PeakModel::StepDown.eval(&xs, &p), stepdown_model(&xs, &p));
        assert_eq!(
            PeakModel::AtanStepUp.eval(&xs, &p),
            atan_stepup_model(&xs, &p)
        );
    }

    #[test]
    fn iterative_fit_recovers_stepup() {
        let xs = linspace(0.0, 100.0, 101);
        let truth = [8.0, 40.0, 6.0, 3.0];
        let ys = stepup_model(&xs, &truth);
        let fit = IterativeFit::new(PeakModel::StepUp)
            .fit_full(&xs, &ys)
            .unwrap();
        let p = &fit.fit.parameters;
        assert!((p[0] - truth[0]).abs() < 1.0, "height {}", p[0]);
        assert!((p[1] - truth[1]).abs() < 1.0, "center {}", p[1]);
        assert!((p[3] - truth[3]).abs() < 1.0, "bg {}", p[3]);
    }

    // --- Peak-on-background fit (silx background-then-peak workflow) --------

    #[test]
    fn fit_peak_with_background_none_is_byte_identical_to_plain_fit() {
        use crate::core::background::Background;
        let xs = grid(100);
        let ys = gaussian_model(&xs, &[100.0, 50.0, 8.0, 0.0]);
        let bgfit = fit_peak_with_background(
            PeakModel::Gaussian,
            Background::None,
            &xs,
            &ys,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        let plain = IterativeFit::new(PeakModel::Gaussian)
            .fit_full(&xs, &ys)
            .unwrap();
        // No background: residual == y exactly, so the peak fit and the total
        // curve match the plain iterative fit bit-for-bit.
        assert!(bgfit.background.iter().all(|&b| b == 0.0));
        assert_eq!(bgfit.peak.fit.parameters, plain.fit.parameters);
        assert_eq!(bgfit.total, bgfit.peak.fit.y_fit);
        assert_eq!(bgfit.total, plain.fit.y_fit);
    }

    #[test]
    fn fit_peak_with_background_constant_offset_recovers_peak() {
        use crate::core::background::Background;
        let xs = grid(100);
        // Gaussian sitting on a +25 constant pedestal (the gaussian model's
        // trailing parameter is its own constant background).
        let ys = gaussian_model(&xs, &[100.0, 50.0, 8.0, 25.0]);
        let bgfit = fit_peak_with_background(
            PeakModel::Gaussian,
            Background::Constant,
            &xs,
            &ys,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        // Constant background = min(y) ≈ the 25 pedestal (tails approach it).
        assert!(
            (bgfit.background[0] - 25.0).abs() < 1.0,
            "background {}",
            bgfit.background[0]
        );
        assert!(bgfit.background.iter().all(|&b| b == bgfit.background[0]));
        // Centre recovered, and the reconstructed total tracks the data.
        let p = &bgfit.peak.fit.parameters;
        assert!((p[1] - 50.0).abs() < 2.0, "centre {}", p[1]);
        let max_err = ys
            .iter()
            .zip(&bgfit.total)
            .map(|(&d, &t)| (d - t).abs())
            .fold(0.0_f64, f64::max);
        assert!(max_err < 5.0, "max |data - total| = {max_err}");
    }

    #[test]
    fn fit_peak_with_background_linear_recovers_peak_on_slope() {
        use crate::core::background::Background;
        let xs = grid(120);
        // Gaussian on a rising line y = 0.5*x + 10.
        let base = line(&xs, &[0.5, 10.0]);
        let peak = gaussian_model(&xs, &[80.0, 60.0, 8.0, 0.0]);
        let ys: Vec<f64> = base.iter().zip(&peak).map(|(&b, &p)| b + p).collect();
        let bgfit = fit_peak_with_background(
            PeakModel::Gaussian,
            Background::Linear,
            &xs,
            &ys,
            DEFAULT_MAX_ITER,
            DEFAULT_DELTACHI,
        )
        .unwrap();
        // The fitted background follows the positive slope.
        assert!(
            *bgfit.background.last().unwrap() > bgfit.background[0],
            "background not rising: {} -> {}",
            bgfit.background[0],
            bgfit.background.last().unwrap()
        );
        // Peak centre and width recovered on the residual.
        let p = &bgfit.peak.fit.parameters;
        assert!((p[1] - 60.0).abs() < 3.0, "centre {}", p[1]);
        assert!((p[2] - 8.0).abs() < 4.0, "fwhm {}", p[2]);
        // Total tracks the data (peak height 80 → allow a modest residual).
        let max_err = ys
            .iter()
            .zip(&bgfit.total)
            .map(|(&d, &t)| (d - t).abs())
            .fold(0.0_f64, f64::max);
        assert!(max_err < 12.0, "max |data - total| = {max_err}");
    }

    #[test]
    fn fit_peak_with_background_rejects_bad_input() {
        use crate::core::background::Background;
        let xs = grid(10);
        let ys = constant(&xs, &[1.0]);
        // Length mismatch.
        assert!(
            fit_peak_with_background(
                PeakModel::Gaussian,
                Background::None,
                &xs,
                &ys[..5],
                DEFAULT_MAX_ITER,
                DEFAULT_DELTACHI,
            )
            .is_none()
        );
        // Empty input.
        assert!(
            fit_peak_with_background(
                PeakModel::Gaussian,
                Background::None,
                &[],
                &[],
                DEFAULT_MAX_ITER,
                DEFAULT_DELTACHI,
            )
            .is_none()
        );
    }
}
