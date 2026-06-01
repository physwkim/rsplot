//! Basic curve fitting utilities.
//!
//! Provides traits and simple implementations for curve fitting (Linear, Gaussian estimation).
//! For rigorous non-linear least squares fitting, a dedicated optimization crate should be used.

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
