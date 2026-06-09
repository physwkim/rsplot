//! Axis calibrations, mirroring `silx.math.calibration`.
//!
//! A calibration is a transformation applied to an axis (a 1-D index/channel
//! axis), `x ↦ f(x)`. silx defines an abstract base plus `NoCalibration`,
//! `LinearCalibration`, `ArrayCalibration` and `FunctionCalibration`. The only
//! consumer ported so far is [`StackView`](crate::StackView), which keeps one
//! calibration per volume dimension and uses them to place the displayed image
//! (origin + scale) and compute the per-frame Z value.
//!
//! StackView only ever uses *affine* calibrations for the graph axes — silx's
//! `getCalibrations` explicitly replaces any non-affine calibration with
//! `NoCalibration` before scaling the axes (`StackView.getCalibrations`). So the
//! affine pair below ([`Calibration::None`] and [`Calibration::Linear`]) is the
//! complete set StackView can act on; the silx "drop non-affine for graph axes"
//! filter is structurally a no-op here because every variant is affine. The
//! array/function calibrations are intentionally not ported (no affine slope to
//! feed the image scale, so StackView would ignore them anyway).

/// An affine axis calibration `x ↦ a + b·x`, mirroring the affine subset of
/// `silx.math.calibration` (`NoCalibration` and `LinearCalibration`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Calibration {
    /// Identity calibration `x ↦ x` — silx `NoCalibration` (slope `1.0`).
    #[default]
    None,
    /// Linear calibration `x ↦ constant + slope·x` — silx `LinearCalibration`
    /// (`constant` is the y-intercept, `slope` the slope).
    Linear {
        /// y-intercept `a` of `x ↦ a + b·x`.
        constant: f64,
        /// slope `b` of `x ↦ a + b·x`.
        slope: f64,
    },
}

impl Calibration {
    /// Build a linear calibration from a `(constant, slope)` pair — silx's
    /// 2-tuple `(a, b)` shorthand accepted by `StackView.setStack`.
    pub fn linear(constant: f64, slope: f64) -> Self {
        Calibration::Linear { constant, slope }
    }

    /// Apply the calibration to a value — silx `AbstractCalibration.__call__`.
    pub fn apply(self, x: f64) -> f64 {
        match self {
            Calibration::None => x,
            Calibration::Linear { constant, slope } => constant + slope * x,
        }
    }

    /// The slope `b` — silx `get_slope` (`NoCalibration` returns `1.0`).
    pub fn slope(self) -> f64 {
        match self {
            Calibration::None => 1.0,
            Calibration::Linear { slope, .. } => slope,
        }
    }

    /// Whether the calibration is affine (`x ↦ a + b·x`) — silx `is_affine`.
    /// Always `true` for this affine-only set; kept to mirror silx's API and to
    /// document why StackView's non-affine filter never fires here.
    pub fn is_affine(self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_identity_with_unit_slope() {
        let c = Calibration::None;
        assert_eq!(c.apply(0.0), 0.0);
        assert_eq!(c.apply(7.5), 7.5);
        assert_eq!(c.slope(), 1.0);
        assert!(c.is_affine());
    }

    #[test]
    fn linear_applies_intercept_and_slope() {
        // silx LinearCalibration(constant=2.0, slope=0.5): x -> 2 + 0.5x
        let c = Calibration::linear(2.0, 0.5);
        assert_eq!(c.apply(0.0), 2.0); // origin = c(0) = intercept
        assert_eq!(c.apply(4.0), 4.0); // 2 + 0.5*4
        assert_eq!(c.apply(-2.0), 1.0); // 2 + 0.5*-2
        assert_eq!(c.slope(), 0.5);
        assert!(c.is_affine());
    }

    #[test]
    fn default_is_no_calibration() {
        assert_eq!(Calibration::default(), Calibration::None);
    }
}
