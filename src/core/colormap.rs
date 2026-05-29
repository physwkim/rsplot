//! Colormaps.
//!
//! A colormap is a 256-entry RGBA lookup table plus a value range (`vmin`,
//! `vmax`) and a [`Normalization`]. The image shader transforms each scalar to
//! `[0, 1]` against the range under the chosen normalization and indexes the
//! LUT (`doc/design.md` §5). A small catalog of perceptually-sensible maps is
//! provided via [`ColormapName`] (`doc/design.md` §13 E2).
//!
//! Scope: linear / log10 / sqrt / gamma normalization (mirrors silx
//! `GLPlotImage`). NaN sentinel handling and autoscale (`vmin`/`vmax = None`)
//! arrive in later steps.

use colorous::Gradient;

/// How a scalar value is mapped to the `[0, 1]` LUT coordinate before the color
/// lookup (silx `Colormap.normalization`). Mirrors silx's `GLPlotImage`
/// normalizations; the numeric [`Normalization::code`] matches its `normID`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Normalization {
    /// `t = (v - vmin) / (vmax - vmin)`.
    #[default]
    Linear,
    /// `t = (log10(v) - log10(vmin)) / (log10(vmax) - log10(vmin))`; values
    /// `v <= 0` map to the low color.
    Log,
    /// `t = (sqrt(v) - sqrt(vmin)) / (sqrt(vmax) - sqrt(vmin))`; values `v < 0`
    /// map to the low color.
    Sqrt,
    /// `t = ((v - vmin) / (vmax - vmin)) ^ gamma` (the linear ratio raised to
    /// the [`Colormap::gamma`] power; silx applies the exponent directly).
    Gamma,
}

impl Normalization {
    /// Shader normalization code (must match the `if`-chain in `image.wgsl`,
    /// and silx `GLPlotImage` `normID`: linear 0, log 1, sqrt 2, gamma 3).
    pub(crate) fn code(self) -> u32 {
        match self {
            Normalization::Linear => 0,
            Normalization::Log => 1,
            Normalization::Sqrt => 2,
            Normalization::Gamma => 3,
        }
    }

    /// The monotonic transform applied to a value before the linear `[0, 1]`
    /// scaling: `log10` for [`Log`](Normalization::Log), `sqrt` for
    /// [`Sqrt`](Normalization::Sqrt), identity otherwise. [`Gamma`] scales
    /// linearly here; its exponent is applied to the ratio afterwards, matching
    /// silx `GLPlotImage`.
    fn transform(self, v: f64) -> f64 {
        match self {
            Normalization::Linear | Normalization::Gamma => v,
            Normalization::Log => v.log10(),
            Normalization::Sqrt => v.sqrt(),
        }
    }
}

/// A named colormap in the built-in catalog. Backed by `colorous` gradients.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColormapName {
    /// Perceptually-uniform default (matplotlib's viridis).
    Viridis,
    Inferno,
    Magma,
    Plasma,
    Cividis,
    /// Modern rainbow-like, perceptually improved (Google's turbo).
    Turbo,
    /// Single-hue grayscale.
    Greys,
    /// Diverging blue–red (matplotlib's spectral).
    Spectral,
}

impl ColormapName {
    /// All catalog entries, for building a picker.
    pub const ALL: [ColormapName; 8] = [
        ColormapName::Viridis,
        ColormapName::Inferno,
        ColormapName::Magma,
        ColormapName::Plasma,
        ColormapName::Cividis,
        ColormapName::Turbo,
        ColormapName::Greys,
        ColormapName::Spectral,
    ];

    /// The `colorous` gradient backing this name.
    fn gradient(self) -> Gradient {
        match self {
            ColormapName::Viridis => colorous::VIRIDIS,
            ColormapName::Inferno => colorous::INFERNO,
            ColormapName::Magma => colorous::MAGMA,
            ColormapName::Plasma => colorous::PLASMA,
            ColormapName::Cividis => colorous::CIVIDIS,
            ColormapName::Turbo => colorous::TURBO,
            ColormapName::Greys => colorous::GREYS,
            ColormapName::Spectral => colorous::SPECTRAL,
        }
    }

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            ColormapName::Viridis => "Viridis",
            ColormapName::Inferno => "Inferno",
            ColormapName::Magma => "Magma",
            ColormapName::Plasma => "Plasma",
            ColormapName::Cividis => "Cividis",
            ColormapName::Turbo => "Turbo",
            ColormapName::Greys => "Greys",
            ColormapName::Spectral => "Spectral",
        }
    }
}

/// silx's default gamma-normalization exponent (`Colormap.__gamma`).
const DEFAULT_GAMMA: f32 = 2.0;

/// A 256-color lookup table with a value range and a [`Normalization`].
///
/// `vmin`/`vmax` are the data values mapped to the first and last LUT entries.
/// Precondition: `vmax > vmin` (and for [`Normalization::Log`], `vmin > 0`).
#[derive(Clone, Debug, PartialEq)]
pub struct Colormap {
    /// 256 RGBA entries, sRGB-encoded (uploaded to an sRGB LUT texture).
    pub lut: [[u8; 4]; 256],
    pub vmin: f64,
    pub vmax: f64,
    /// How a value is mapped to the LUT coordinate (linear by default).
    pub normalization: Normalization,
    /// Exponent for [`Normalization::Gamma`] (ignored otherwise); `2.0` by
    /// default, matching silx.
    pub gamma: f32,
}

impl Colormap {
    /// Build a colormap from a catalog `name` over `[vmin, vmax]` with linear
    /// normalization and the default gamma.
    pub fn new(name: ColormapName, vmin: f64, vmax: f64) -> Self {
        let gradient = name.gradient();
        let mut lut = [[0u8; 4]; 256];
        for (i, entry) in lut.iter_mut().enumerate() {
            let c = gradient.eval_continuous(i as f64 / 255.0);
            *entry = [c.r, c.g, c.b, 255];
        }
        Self {
            lut,
            vmin,
            vmax,
            normalization: Normalization::Linear,
            gamma: DEFAULT_GAMMA,
        }
    }

    /// The perceptually-uniform "viridis" colormap over `[vmin, vmax]`.
    pub fn viridis(vmin: f64, vmax: f64) -> Self {
        Self::new(ColormapName::Viridis, vmin, vmax)
    }

    /// Reverse the LUT (low and high colors swap) while keeping the value range.
    pub fn reversed(mut self) -> Self {
        self.lut.reverse();
        self
    }

    /// Set the value-to-LUT normalization (silx `Colormap.normalization`).
    pub fn with_normalization(mut self, normalization: Normalization) -> Self {
        self.normalization = normalization;
        self
    }

    /// Set the [`Normalization::Gamma`] exponent (clamped to ≥ 0); only used
    /// under gamma normalization.
    pub fn with_gamma(mut self, gamma: f32) -> Self {
        self.gamma = gamma.max(0.0);
        self
    }

    /// The `(cmap_min, one_over_range)` the image shader needs: the
    /// normalization transform applied to the bounds. `one_over_range` is `0`
    /// for a degenerate or invalid (e.g. non-positive log) range, which maps
    /// every value to the low color — the silx `GLPlotImage` fallback.
    pub(crate) fn norm_bounds(&self) -> (f32, f32) {
        let lo = self.normalization.transform(self.vmin);
        let hi = self.normalization.transform(self.vmax);
        if lo.is_finite() && hi.is_finite() && hi > lo {
            (lo as f32, (1.0 / (hi - lo)) as f32)
        } else {
            (0.0, 0.0)
        }
    }

    /// Map a data value to its `[0, 1]` LUT coordinate under this colormap's
    /// normalization — the CPU mirror of the `image.wgsl` fragment math, used
    /// to place colorbar ticks at the same position the image colors them.
    pub fn normalize(&self, v: f64) -> f32 {
        // Match the shader's domain guards for log/sqrt.
        match self.normalization {
            Normalization::Log if v <= 0.0 => return 0.0,
            Normalization::Sqrt if v < 0.0 => return 0.0,
            _ => {}
        }
        let (cmap_min, one_over_range) = self.norm_bounds();
        let t = self.normalization.transform(v) as f32;
        let ratio = (one_over_range * (t - cmap_min)).clamp(0.0, 1.0);
        match self.normalization {
            Normalization::Gamma => ratio.powf(self.gamma),
            _ => ratio,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_viridis_matches_convenience_ctor() {
        assert_eq!(
            Colormap::new(ColormapName::Viridis, 0.0, 1.0),
            Colormap::viridis(0.0, 1.0)
        );
    }

    #[test]
    fn reversed_swaps_endpoints_and_is_an_involution() {
        let cm = Colormap::new(ColormapName::Viridis, 0.0, 2.0);
        let rev = cm.clone().reversed();
        assert_eq!(cm.lut[0], rev.lut[255]);
        assert_eq!(cm.lut[255], rev.lut[0]);
        // Range is unaffected; reversing twice restores the original.
        assert_eq!(rev.vmin, 0.0);
        assert_eq!(rev.vmax, 2.0);
        assert_eq!(cm, rev.reversed());
    }

    #[test]
    fn catalog_entries_build_with_distinct_endpoints() {
        for name in ColormapName::ALL {
            let cm = Colormap::new(name, 0.0, 1.0);
            assert_ne!(
                cm.lut[0],
                cm.lut[255],
                "{} has equal endpoints",
                name.label()
            );
            assert_eq!(cm.lut[0][3], 255, "{} alpha", name.label());
        }
    }

    #[test]
    fn defaults_to_linear_with_silx_gamma() {
        let cm = Colormap::viridis(0.0, 1.0);
        assert_eq!(cm.normalization, Normalization::Linear);
        assert_eq!(cm.gamma, 2.0);
        assert_eq!(Normalization::default(), Normalization::Linear);
    }

    #[test]
    fn normalization_codes_match_shader() {
        // These must stay in sync with the `if`-chain in image.wgsl / silx normID.
        assert_eq!(Normalization::Linear.code(), 0);
        assert_eq!(Normalization::Log.code(), 1);
        assert_eq!(Normalization::Sqrt.code(), 2);
        assert_eq!(Normalization::Gamma.code(), 3);
    }

    #[test]
    fn with_gamma_clamps_negative() {
        assert_eq!(Colormap::viridis(0.0, 1.0).with_gamma(-1.0).gamma, 0.0);
    }

    #[test]
    fn normalize_linear_is_clamped_ratio() {
        let cm = Colormap::viridis(2.0, 6.0);
        assert_eq!(cm.normalize(2.0), 0.0); // vmin
        assert_eq!(cm.normalize(6.0), 1.0); // vmax
        assert_eq!(cm.normalize(4.0), 0.5); // midpoint
        assert_eq!(cm.normalize(0.0), 0.0); // below clamps
        assert_eq!(cm.normalize(10.0), 1.0); // above clamps
    }

    #[test]
    fn normalize_log_matches_log_ratio_and_guards_nonpositive() {
        let cm = Colormap::viridis(1.0, 100.0).with_normalization(Normalization::Log);
        assert_eq!(cm.normalize(1.0), 0.0); // log10(1) = 0 -> vmin
        assert_eq!(cm.normalize(100.0), 1.0); // log10(100) = 2 -> vmax
        assert!((cm.normalize(10.0) - 0.5).abs() < 1e-6); // log10(10) = 1 -> mid
        assert_eq!(cm.normalize(0.0), 0.0); // non-positive -> low color
        assert_eq!(cm.normalize(-5.0), 0.0);
    }

    #[test]
    fn normalize_sqrt_matches_sqrt_ratio_and_guards_negative() {
        let cm = Colormap::viridis(0.0, 4.0).with_normalization(Normalization::Sqrt);
        assert_eq!(cm.normalize(0.0), 0.0); // sqrt(0) = 0
        assert_eq!(cm.normalize(4.0), 1.0); // sqrt(4) = 2 -> vmax
        assert_eq!(cm.normalize(1.0), 0.5); // sqrt(1) = 1 -> mid
        assert_eq!(cm.normalize(-1.0), 0.0); // negative -> low color
    }

    #[test]
    fn normalize_gamma_raises_ratio_to_the_power() {
        let cm = Colormap::viridis(0.0, 1.0)
            .with_normalization(Normalization::Gamma)
            .with_gamma(2.0);
        // ratio at v=0.5 is 0.5; gamma 2.0 -> 0.25.
        assert!((cm.normalize(0.5) - 0.25).abs() < 1e-6);
        assert_eq!(cm.normalize(0.0), 0.0);
        assert_eq!(cm.normalize(1.0), 1.0);
    }

    #[test]
    fn norm_bounds_degenerate_or_invalid_range_collapses() {
        // vmax == vmin -> one_over_range 0 (maps everything to the low color).
        assert_eq!(Colormap::viridis(3.0, 3.0).norm_bounds(), (0.0, 0.0));
        // Log of a non-positive vmin is non-finite -> degenerate fallback.
        let log = Colormap::viridis(-1.0, 100.0).with_normalization(Normalization::Log);
        assert_eq!(log.norm_bounds(), (0.0, 0.0));
    }

    #[test]
    fn norm_bounds_transform_log_and_sqrt_bounds() {
        let log = Colormap::viridis(1.0, 100.0).with_normalization(Normalization::Log);
        let (cmin, oor) = log.norm_bounds();
        assert_eq!(cmin, 0.0); // log10(1)
        assert!((oor - 0.5).abs() < 1e-6); // 1 / (log10(100) - log10(1)) = 1/2

        let sqrt = Colormap::viridis(0.0, 4.0).with_normalization(Normalization::Sqrt);
        let (cmin, oor) = sqrt.norm_bounds();
        assert_eq!(cmin, 0.0); // sqrt(0)
        assert!((oor - 0.5).abs() < 1e-6); // 1 / (sqrt(4) - sqrt(0)) = 1/2
    }
}
