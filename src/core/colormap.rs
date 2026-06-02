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

/// A named colormap in the built-in catalog. The perceptual maps are backed by
/// `colorous` gradients; silx's analytic maps (`gray`, `red`, `green`, `blue`,
/// `temperature`) and the matplotlib-derived `jet`/`hsv` are built by
/// [`ColormapName::build_lut`].
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
    /// Single-hue grayscale (colorous greys; an alias of [`Gray`](Self::Gray)).
    Greys,
    /// Diverging blue–red (matplotlib's spectral).
    Spectral,
    /// Black-to-white linear ramp (silx `gray`).
    Gray,
    /// Black-to-red linear ramp (silx `red`).
    Red,
    /// Black-to-green linear ramp (silx `green`).
    Green,
    /// Black-to-blue linear ramp (silx `blue`).
    Blue,
    /// silx `temperature`: blue → cyan → green → red.
    Temperature,
    /// Classic blue-cyan-yellow-red rainbow (matplotlib's jet).
    Jet,
    /// Full-saturation hue wheel (matplotlib's hsv).
    Hsv,
}

impl ColormapName {
    /// All catalog entries, for building a picker. Ordered to match silx's
    /// preferred-colormap list (`colors.py:1086`) where the entries overlap.
    pub const ALL: [ColormapName; 15] = [
        ColormapName::Gray,
        ColormapName::Red,
        ColormapName::Green,
        ColormapName::Blue,
        ColormapName::Viridis,
        ColormapName::Cividis,
        ColormapName::Magma,
        ColormapName::Inferno,
        ColormapName::Plasma,
        ColormapName::Temperature,
        ColormapName::Jet,
        ColormapName::Hsv,
        ColormapName::Turbo,
        ColormapName::Greys,
        ColormapName::Spectral,
    ];

    /// The `colorous` gradient backing a perceptual name, or `None` for the
    /// analytic maps built by [`Self::build_lut`].
    fn gradient(self) -> Option<Gradient> {
        match self {
            ColormapName::Viridis => Some(colorous::VIRIDIS),
            ColormapName::Inferno => Some(colorous::INFERNO),
            ColormapName::Magma => Some(colorous::MAGMA),
            ColormapName::Plasma => Some(colorous::PLASMA),
            ColormapName::Cividis => Some(colorous::CIVIDIS),
            ColormapName::Turbo => Some(colorous::TURBO),
            ColormapName::Greys => Some(colorous::GREYS),
            ColormapName::Spectral => Some(colorous::SPECTRAL),
            ColormapName::Gray
            | ColormapName::Red
            | ColormapName::Green
            | ColormapName::Blue
            | ColormapName::Temperature
            | ColormapName::Jet
            | ColormapName::Hsv => None,
        }
    }

    /// Build the 256-entry sRGB LUT for this name. `colorous`-backed names are
    /// sampled regularly over `[0, 1]`; the analytic names mirror silx
    /// `_create_colormap_lut` (gray/red/green/blue/temperature) and the
    /// matplotlib segment data loaded by silx for `jet`/`hsv`.
    fn build_lut(self) -> [[u8; 4]; 256] {
        if let Some(gradient) = self.gradient() {
            let mut lut = [[0u8; 4]; 256];
            for (i, entry) in lut.iter_mut().enumerate() {
                let c = gradient.eval_continuous(i as f64 / 255.0);
                *entry = [c.r, c.g, c.b, 255];
            }
            return lut;
        }
        match self {
            ColormapName::Gray => single_channel_ramp(0b111),
            ColormapName::Red => single_channel_ramp(0b001),
            ColormapName::Green => single_channel_ramp(0b010),
            ColormapName::Blue => single_channel_ramp(0b100),
            ColormapName::Temperature => temperature_lut(),
            ColormapName::Jet => segmented_lut(&JET_SEGMENTS),
            ColormapName::Hsv => segmented_lut(&HSV_SEGMENTS),
            // colorous-backed names handled above.
            _ => unreachable!("colorous-backed name reaches analytic builder"),
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
            ColormapName::Gray => "Gray",
            ColormapName::Red => "Red",
            ColormapName::Green => "Green",
            ColormapName::Blue => "Blue",
            ColormapName::Temperature => "Temperature",
            ColormapName::Jet => "Jet",
            ColormapName::Hsv => "HSV",
        }
    }
}

/// silx single-channel ramp builder: each selected channel (bit 0 = red, bit 1
/// = green, bit 2 = blue) carries `arange(256)`, others stay 0. Bit mask `0b111`
/// yields `gray` (silx `_create_colormap_lut` `lut[:, :3] = arange(256)`).
fn single_channel_ramp(channels: u8) -> [[u8; 4]; 256] {
    let mut lut = [[0u8, 0, 0, 255]; 256];
    for (i, entry) in lut.iter_mut().enumerate() {
        let v = i as u8;
        if channels & 0b001 != 0 {
            entry[0] = v;
        }
        if channels & 0b010 != 0 {
            entry[1] = v;
        }
        if channels & 0b100 != 0 {
            entry[2] = v;
        }
    }
    lut
}

/// silx `temperature` LUT, transcribed channel-by-channel from
/// `silx.math.colormap._create_colormap_lut` (the `numpy.arange` slice fills).
fn temperature_lut() -> [[u8; 4]; 256] {
    let mut lut = [[0u8, 0, 0, 255]; 256];

    // Red: lut[128:192, 0] = arange(2, 255, 4); lut[192:, 0] = 255.
    for (k, i) in (128..192).enumerate() {
        lut[i][0] = (2 + 4 * k) as u8;
    }
    for entry in lut.iter_mut().take(256).skip(192) {
        entry[0] = 255;
    }

    // Green: lut[:64, 1] = arange(0, 255, 4); lut[64:192, 1] = 255;
    //        lut[192:, 1] = arange(252, -1, -4).
    for (k, entry) in lut.iter_mut().take(64).enumerate() {
        entry[1] = (4 * k) as u8;
    }
    for entry in lut.iter_mut().take(192).skip(64) {
        entry[1] = 255;
    }
    for (k, i) in (192..256).enumerate() {
        lut[i][1] = (252 - 4 * k) as u8;
    }

    // Blue: lut[:64, 2] = 255; lut[64:128, 2] = arange(254, 0, -4).
    for entry in lut.iter_mut().take(64) {
        entry[2] = 255;
    }
    for (k, i) in (64..128).enumerate() {
        lut[i][2] = (254 - 4 * k) as u8;
    }

    lut
}

/// A piecewise-linear colormap segment: at LUT coordinate `x` in `[0, 1]` the
/// channel value is `y` in `[0, 1]`. Matches the per-channel anchor lists of a
/// matplotlib `LinearSegmentedColormap` (left/right discontinuity values are
/// equal for these maps, so a single `y` per anchor suffices).
struct Segment {
    x: f64,
    y: f64,
}

const fn seg(x: f64, y: f64) -> Segment {
    Segment { x, y }
}

/// Per-channel anchor lists for one colormap (red, green, blue).
struct Segments {
    red: &'static [Segment],
    green: &'static [Segment],
    blue: &'static [Segment],
}

/// matplotlib `jet` segment data (`matplotlib._cm._jet_data`).
static JET_SEGMENTS: Segments = Segments {
    red: &[
        seg(0.00, 0.0),
        seg(0.35, 0.0),
        seg(0.66, 1.0),
        seg(0.89, 1.0),
        seg(1.00, 0.5),
    ],
    green: &[
        seg(0.000, 0.0),
        seg(0.125, 0.0),
        seg(0.375, 1.0),
        seg(0.640, 1.0),
        seg(0.910, 0.0),
        seg(1.000, 0.0),
    ],
    blue: &[
        seg(0.00, 0.5),
        seg(0.11, 1.0),
        seg(0.34, 1.0),
        seg(0.65, 0.0),
        seg(1.00, 0.0),
    ],
};

/// matplotlib `hsv` segment data (`matplotlib._cm._hsv_data`): the full-saturation
/// hue wheel, red → yellow → green → cyan → blue → magenta → red.
static HSV_SEGMENTS: Segments = Segments {
    red: &[
        seg(0.0, 1.0),
        seg(0.158730, 1.0),
        seg(0.174603, 0.968750),
        seg(0.333333, 0.031250),
        seg(0.349206, 0.0),
        seg(0.666667, 0.0),
        seg(0.682540, 0.031250),
        seg(0.841270, 0.968750),
        seg(0.857143, 1.0),
        seg(1.0, 1.0),
    ],
    green: &[
        seg(0.0, 0.0),
        seg(0.158730, 0.937500),
        seg(0.174603, 1.0),
        seg(0.682540, 1.0),
        seg(0.698413, 0.937500),
        seg(0.841270, 0.031250),
        seg(0.857143, 0.0),
        seg(1.0, 0.0),
    ],
    blue: &[
        seg(0.0, 0.0),
        seg(0.333333, 0.0),
        seg(0.349206, 0.031250),
        seg(0.507937, 0.968750),
        seg(0.523810, 1.0),
        seg(0.841270, 1.0),
        seg(0.857143, 0.968750),
        seg(1.0, 0.062500),
    ],
};

/// Interpolate a single channel's segment list at coordinate `x` in `[0, 1]`,
/// returning the value in `[0, 1]` (matplotlib `LinearSegmentedColormap` lookup,
/// clamped at the endpoints).
fn interp_segment(segments: &[Segment], x: f64) -> f64 {
    if x <= segments[0].x {
        return segments[0].y;
    }
    let last = &segments[segments.len() - 1];
    if x >= last.x {
        return last.y;
    }
    for pair in segments.windows(2) {
        let (lo, hi) = (&pair[0], &pair[1]);
        if x <= hi.x {
            // Coincident anchors (a discontinuity) take the right value.
            if hi.x == lo.x {
                return hi.y;
            }
            let t = (x - lo.x) / (hi.x - lo.x);
            return lo.y + t * (hi.y - lo.y);
        }
    }
    last.y
}

/// Sample a segmented colormap into a 256-entry sRGB LUT. Coordinate `i / 255`
/// (matplotlib's regular 256-sample grid) is evaluated per channel, then the
/// float `[0, 1]` value is quantized exactly as silx's `array_to_rgba8888`:
/// `clip(value * 256, 0, 255)` truncated to `u8` (each bin `[N, N+1)`, with the
/// top bin `[255, 256]`). This matches how silx loads these maps from `.npy`.
fn segmented_lut(segments: &Segments) -> [[u8; 4]; 256] {
    let mut lut = [[0u8, 0, 0, 255]; 256];
    for (i, entry) in lut.iter_mut().enumerate() {
        let x = i as f64 / 255.0;
        entry[0] = quantize_float_channel(interp_segment(segments.red, x));
        entry[1] = quantize_float_channel(interp_segment(segments.green, x));
        entry[2] = quantize_float_channel(interp_segment(segments.blue, x));
    }
    lut
}

/// Convert a float channel value in `[0, 1]` to a `u8`, mirroring silx
/// `array_to_rgba8888`: `clip(value * 256, 0, 255)` truncated toward zero.
fn quantize_float_channel(value: f64) -> u8 {
    (value * 256.0).clamp(0.0, 255.0) as u8
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
        Self {
            lut: name.build_lut(),
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

    // --- Catalog LUTs ----------------------------------------------------

    #[test]
    fn every_name_yields_a_256_entry_lut() {
        for name in ColormapName::ALL {
            let lut = name.build_lut();
            assert_eq!(lut.len(), 256, "{} LUT length", name.label());
            assert!(
                lut.iter().all(|c| c[3] == 255),
                "{} should be fully opaque",
                name.label()
            );
        }
    }

    #[test]
    fn gray_red_green_blue_are_silx_linear_ramps() {
        // silx _create_colormap_lut: gray -> arange(256) in all RGB channels,
        // single-channel maps -> arange(256) in their channel only.
        let gray = Colormap::new(ColormapName::Gray, 0.0, 1.0).lut;
        assert_eq!(gray[0], [0, 0, 0, 255]);
        assert_eq!(gray[128], [128, 128, 128, 255]);
        assert_eq!(gray[255], [255, 255, 255, 255]);

        let red = Colormap::new(ColormapName::Red, 0.0, 1.0).lut;
        assert_eq!(red[200], [200, 0, 0, 255]);
        let green = Colormap::new(ColormapName::Green, 0.0, 1.0).lut;
        assert_eq!(green[200], [0, 200, 0, 255]);
        let blue = Colormap::new(ColormapName::Blue, 0.0, 1.0).lut;
        assert_eq!(blue[200], [0, 0, 200, 255]);
    }

    #[test]
    fn temperature_matches_silx_channel_stops() {
        // Boundary samples of silx's _create_colormap_lut "temperature" fills.
        let lut = Colormap::new(ColormapName::Temperature, 0.0, 1.0).lut;
        // Blue: [:64] = 255; index 64 starts arange(254, 0, -4).
        assert_eq!(lut[0][2], 255);
        assert_eq!(lut[63][2], 255);
        assert_eq!(lut[64][2], 254);
        // Green: [:64] = arange(0, 255, 4); [64:192] = 255.
        assert_eq!(lut[0][1], 0);
        assert_eq!(lut[63][1], 252); // 4 * 63
        assert_eq!(lut[64][1], 255);
        // Red: [128:192] = arange(2, 255, 4); [192:] = 255.
        assert_eq!(lut[127][0], 0);
        assert_eq!(lut[128][0], 2);
        assert_eq!(lut[192][0], 255);
        assert_eq!(lut[255][0], 255);
    }

    #[test]
    fn jet_and_hsv_endpoints_match_matplotlib_segments() {
        // matplotlib jet: red 0->0.5 (->128) blue, ends at red 0.5 (->128).
        let jet = Colormap::new(ColormapName::Jet, 0.0, 1.0).lut;
        assert_eq!(jet[0], [0, 0, 128, 255]); // low: blue 0.5
        assert_eq!(jet[255], [128, 0, 0, 255]); // high: red 0.5
        // matplotlib hsv starts pure red, ends near pure red (wraps the wheel).
        let hsv = Colormap::new(ColormapName::Hsv, 0.0, 1.0).lut;
        assert_eq!(hsv[0], [255, 0, 0, 255]);
        assert_eq!(hsv[255], [255, 0, 16, 255]); // blue 0.0625 -> 16
    }
}
