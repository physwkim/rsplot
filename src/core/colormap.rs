//! Colormaps.
//!
//! A colormap is a 256-entry RGBA lookup table plus a value range (`vmin`,
//! `vmax`). The image shader normalizes each scalar to `[0, 1]` against the
//! range and indexes the LUT (`doc/design.md` §5). A small catalog of
//! perceptually-sensible maps is provided via [`ColormapName`]
//! (`doc/design.md` §13 E2).
//!
//! Scope: linear normalization only. Log/sqrt/gamma/arcsinh, NaN sentinel
//! handling, and autoscale (`vmin`/`vmax = None`) arrive in later steps.

use colorous::Gradient;

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

/// A 256-color lookup table with a linear value range.
///
/// `vmin`/`vmax` are the data values mapped to the first and last LUT entries.
/// Precondition: `vmax > vmin`.
#[derive(Clone, Debug, PartialEq)]
pub struct Colormap {
    /// 256 RGBA entries, sRGB-encoded (uploaded to an sRGB LUT texture).
    pub lut: [[u8; 4]; 256],
    pub vmin: f64,
    pub vmax: f64,
}

impl Colormap {
    /// Build a colormap from a catalog `name` over `[vmin, vmax]`.
    pub fn new(name: ColormapName, vmin: f64, vmax: f64) -> Self {
        let gradient = name.gradient();
        let mut lut = [[0u8; 4]; 256];
        for (i, entry) in lut.iter_mut().enumerate() {
            let c = gradient.eval_continuous(i as f64 / 255.0);
            *entry = [c.r, c.g, c.b, 255];
        }
        Self { lut, vmin, vmax }
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
}
