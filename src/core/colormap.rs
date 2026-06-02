//! Colormaps.
//!
//! A colormap is a 256-entry RGBA lookup table plus a value range (`vmin`,
//! `vmax`) and a [`Normalization`]. The image shader transforms each scalar to
//! `[0, 1]` against the range under the chosen normalization and indexes the
//! LUT (`doc/design.md` §5). A small catalog of perceptually-sensible maps is
//! provided via [`ColormapName`] (`doc/design.md` §13 E2).
//!
//! Scope: linear / log10 / sqrt / gamma / arcsinh normalization (mirrors silx
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
    /// `t = (asinh(v) - asinh(vmin)) / (asinh(vmax) - asinh(vmin))` (silx
    /// `ARCSINH`). `asinh` is finite and monotonic for every finite value, so
    /// unlike log/sqrt there is no invalid domain to guard.
    Arcsinh,
}

impl Normalization {
    /// Shader normalization code (must match the `if`-chain in `image.wgsl`,
    /// and silx `GLPlotImage` `normID`: linear 0, log 1, sqrt 2, gamma 3,
    /// arcsinh 4).
    pub(crate) fn code(self) -> u32 {
        match self {
            Normalization::Linear => 0,
            Normalization::Log => 1,
            Normalization::Sqrt => 2,
            Normalization::Gamma => 3,
            Normalization::Arcsinh => 4,
        }
    }

    /// The monotonic transform applied to a value before the linear `[0, 1]`
    /// scaling: `log10` for [`Log`](Normalization::Log), `sqrt` for
    /// [`Sqrt`](Normalization::Sqrt), `asinh` for
    /// [`Arcsinh`](Normalization::Arcsinh), identity otherwise. [`Gamma`] scales
    /// linearly here; its exponent is applied to the ratio afterwards, matching
    /// silx `GLPlotImage`.
    fn transform(self, v: f64) -> f64 {
        match self {
            Normalization::Linear | Normalization::Gamma => v,
            Normalization::Log => v.log10(),
            Normalization::Sqrt => v.sqrt(),
            Normalization::Arcsinh => v.asinh(),
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

/// Resample an arbitrary-length RGBA color list to a 256-entry LUT by
/// nearest-neighbour over `[0, 1]`: LUT index `i` reads source row
/// `round(i / 255 * (N - 1))`. `N == 256` is an exact copy; `N == 1` fills the
/// whole LUT with the single color. Returns `None` for an empty input (silx
/// `setColormapLUT` asserts `len(colors) != 0`).
fn resample_lut(colors: &[[u8; 4]]) -> Option<[[u8; 4]; 256]> {
    let n = colors.len();
    if n == 0 {
        return None;
    }
    let mut lut = [[0u8; 4]; 256];
    for (i, entry) in lut.iter_mut().enumerate() {
        let src = if n == 1 {
            0
        } else {
            // round(i / 255 * (N - 1)); i in 0..=255 keeps src in 0..=N-1.
            (i as f64 / 255.0 * (n - 1) as f64).round() as usize
        };
        *entry = colors[src.min(n - 1)];
    }
    Some(lut)
}

/// silx's default gamma-normalization exponent (`Colormap.__gamma`).
const DEFAULT_GAMMA: f32 = 2.0;

/// silx's default Not-A-Number color (`Colormap._DEFAULT_NAN_COLOR`): fully
/// transparent white.
const DEFAULT_NAN_COLOR: [u8; 4] = [255, 255, 255, 0];

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
    /// RGBA color used for Not-A-Number values (silx `Colormap.setNaNColor`);
    /// fully transparent white by default.
    pub nan_color: [u8; 4],
    /// `(low, high)` percentiles for [`AutoscaleMode::Percentile`] (silx
    /// `Colormap._percentiles`); defaults to [`DEFAULT_PERCENTILES`]. Both are
    /// in `[0, 100]`.
    pub autoscale_percentiles: (f64, f64),
    /// Whether the editable-aware setters may change the colormap (silx
    /// `Colormap._editable`, a plain instance attribute; default `true`).
    ///
    /// silx enforces editability inside the mutating *methods*
    /// (`setColormapLUT`, `setVMin`, … raise `NotEditableError`), not by hiding
    /// the attribute — this field is public for the same reason and the guard
    /// lives in [`Self::set_lut`], [`Self::set_autoscale_percentiles`], and
    /// [`Self::set_from`]. Read/write it via [`Self::is_editable`] /
    /// [`Self::set_editable`].
    pub editable: bool,
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
            nan_color: DEFAULT_NAN_COLOR,
            autoscale_percentiles: DEFAULT_PERCENTILES,
            editable: true,
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

    /// Set the RGBA color used for Not-A-Number values (silx
    /// `Colormap.setNaNColor`).
    pub fn with_nan_color(mut self, nan_color: [u8; 4]) -> Self {
        self.nan_color = nan_color;
        self
    }

    /// Replace the LUT with an explicit 256-entry RGBA table (silx
    /// `Colormap.setColormapLUT` for a length-256 array). Builder form; pairs
    /// with [`Self::set_lut`] for the editable-guarded setter.
    pub fn with_lut(mut self, lut: [[u8; 4]; 256]) -> Self {
        self.lut = lut;
        self
    }

    /// Build a colormap from an arbitrary `colors` array resampled to 256 LUT
    /// entries (silx `Colormap(colors=...)` / `setColormapLUT`).
    ///
    /// `colors` is a list of RGB (`[r, g, b]`) or RGBA (`[r, g, b, a]`) `u8`
    /// rows of any length `N >= 1`; RGB rows gain an opaque alpha. The N rows
    /// are resampled to 256 by nearest-neighbour over `[0, 1]` (LUT index `i`
    /// reads source row `round(i / 255 * (N - 1))`), mirroring how silx samples
    /// a stored LUT regularly. `N == 256` is an identity copy. Returns a linear
    /// colormap over `[vmin, vmax]` with the default gamma.
    ///
    /// Returns `None` when `colors` is empty (silx asserts a non-empty array).
    pub fn from_colors(colors: &[[u8; 4]], vmin: f64, vmax: f64) -> Option<Self> {
        let lut = resample_lut(colors)?;
        Some(Self {
            lut,
            vmin,
            vmax,
            normalization: Normalization::Linear,
            gamma: DEFAULT_GAMMA,
            nan_color: DEFAULT_NAN_COLOR,
            autoscale_percentiles: DEFAULT_PERCENTILES,
            editable: true,
        })
    }

    /// Whether the editable-guarded setters may mutate this colormap (silx
    /// `Colormap.isEditable`).
    pub fn is_editable(&self) -> bool {
        self.editable
    }

    /// Set the editable flag (silx `Colormap.setEditable`). When `false` the
    /// editable-guarded setters ([`Self::set_lut`],
    /// [`Self::set_autoscale_percentiles`], [`Self::set_from`]) become no-ops
    /// returning `false`. Builder/`with_*` constructors are unaffected, matching
    /// silx where `copy()` and the constructor bypass the guard.
    pub fn set_editable(&mut self, editable: bool) {
        self.editable = editable;
    }

    /// Replace the LUT with an explicit 256-entry table if editable (silx
    /// `Colormap.setColormapLUT`, which raises `NotEditableError` otherwise).
    /// Returns `true` when applied, `false` when blocked by the editable guard.
    pub fn set_lut(&mut self, lut: [[u8; 4]; 256]) -> bool {
        if !self.editable {
            return false;
        }
        self.lut = lut;
        true
    }

    /// Set the `(low, high)` autoscale percentiles if editable (silx
    /// `Colormap.setAutoscalePercentiles`). Each value is clamped into `[0, 100]`
    /// and the pair is ordered so `low <= high`. Returns `true` when applied,
    /// `false` when blocked by the editable guard.
    pub fn set_autoscale_percentiles(&mut self, low: f64, high: f64) -> bool {
        if !self.editable {
            return false;
        }
        let lo = low.clamp(0.0, 100.0);
        let hi = high.clamp(0.0, 100.0);
        self.autoscale_percentiles = if lo <= hi { (lo, hi) } else { (hi, lo) };
        true
    }

    /// Copy `self` (silx `Colormap.copy`): a value clone that always carries
    /// over every field, including the editable flag, regardless of its value.
    pub fn copy(&self) -> Self {
        self.clone()
    }

    /// Set every field of `self` from `other` if editable (silx
    /// `Colormap.setFromColormap`, which raises `NotEditableError` otherwise).
    /// Mirrors silx: the editable flag is itself overwritten from `other` on
    /// success. Returns `true` when applied, `false` when blocked by the
    /// editable guard.
    pub fn set_from(&mut self, other: &Self) -> bool {
        if !self.editable {
            return false;
        }
        *self = other.clone();
        true
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

/// silx's default `(low, high)` percentiles for [`AutoscaleMode::Percentile`]
/// (`Colormap._DEFAULT_PERCENTILES`).
pub const DEFAULT_PERCENTILES: (f64, f64) = (1.0, 99.0);

/// silx's default `(vmin, vmax)` when autoscale has no finite data to work with
/// (the linear-normalization `_NormalizationMixIn.DEFAULT_RANGE`).
const DEFAULT_RANGE: (f64, f64) = (0.0, 1.0);

/// How autoscale derives `(vmin, vmax)` from data (silx `Colormap.AUTOSCALE_MODES`).
///
/// These mirror the *linear-normalization* autoscale of `silx.math.colormap`
/// (`_LinearNormalizationMixIn`): the data range is used directly, not the
/// normalized data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AutoscaleMode {
    /// Finite data min/max (silx `MINMAX`).
    #[default]
    MinMax,
    /// `mean ± 3·stddev`, each bound clamped into the data min/max range
    /// (silx `STDDEV3`). The standard deviation is the population (ddof = 0)
    /// std, matching numpy `nanstd`.
    Stddev3,
    /// The `(low, high)` percentiles of the finite data (silx `PERCENTILE`),
    /// defaulting to [`DEFAULT_PERCENTILES`].
    Percentile,
}

impl AutoscaleMode {
    /// All modes, for building a picker.
    pub const ALL: [AutoscaleMode; 3] = [
        AutoscaleMode::MinMax,
        AutoscaleMode::Stddev3,
        AutoscaleMode::Percentile,
    ];

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            AutoscaleMode::MinMax => "Min/Max",
            AutoscaleMode::Stddev3 => "Mean ± 3·std",
            AutoscaleMode::Percentile => "Percentile",
        }
    }

    /// Compute the `(vmin, vmax)` autoscale range over `data` for this mode.
    ///
    /// `percentiles` is the `(low, high)` pair used by
    /// [`Percentile`](Self::Percentile) (ignored by the other modes); pass
    /// [`DEFAULT_PERCENTILES`] for silx's default. Non-finite samples are
    /// dropped first. Mirrors `silx.math.colormap` linear-normalization
    /// autoscale, including its fallbacks: empty / non-finite results collapse
    /// to [`DEFAULT_RANGE`], and an inverted range is clamped so `vmax >= vmin`.
    pub fn range(self, data: &[f64], percentiles: (f64, f64)) -> (f64, f64) {
        let finite: Vec<f64> = data.iter().copied().filter(|v| v.is_finite()).collect();

        let (raw_min, raw_max) = match self {
            AutoscaleMode::MinMax => minmax(&finite),
            AutoscaleMode::Stddev3 => {
                let (dmin, dmax) = minmax(&finite);
                let (stdmin, stdmax) = mean3std(&finite);
                // silx: vmin = max(dmin, stdmin), vmax = min(dmax, stdmax),
                // each falling back to the other when one side is absent.
                let vmin = match (dmin, stdmin) {
                    (Some(d), Some(s)) => Some(d.max(s)),
                    (d, s) => d.or(s),
                };
                let vmax = match (dmax, stdmax) {
                    (Some(d), Some(s)) => Some(d.min(s)),
                    (d, s) => d.or(s),
                };
                (vmin, vmax)
            }
            AutoscaleMode::Percentile => {
                let lo = nanpercentile(&finite, percentiles.0);
                let hi = nanpercentile(&finite, percentiles.1);
                (lo, hi)
            }
        };

        // silx fallback handling (_NormalizationMixIn.autoscale tail).
        let vmin = raw_min.filter(|v| v.is_finite()).unwrap_or(DEFAULT_RANGE.0);
        let mut vmax = raw_max.filter(|v| v.is_finite()).unwrap_or(DEFAULT_RANGE.1);
        if vmax < vmin {
            vmax = vmin;
        }
        (vmin, vmax)
    }
}

/// Finite min/max of `data`, or `(None, None)` when empty.
fn minmax(data: &[f64]) -> (Option<f64>, Option<f64>) {
    if data.is_empty() {
        return (None, None);
    }
    let mut min = data[0];
    let mut max = data[0];
    for &v in &data[1..] {
        min = min.min(v);
        max = max.max(v);
    }
    (Some(min), Some(max))
}

/// `(mean - 3·std, mean + 3·std)` of `data`, or `(None, None)` when empty.
/// The standard deviation is the population (ddof = 0) std, matching numpy
/// `nanstd` as used by silx's linear-normalization `autoscale_mean3std`.
fn mean3std(data: &[f64]) -> (Option<f64>, Option<f64>) {
    if data.is_empty() {
        return (None, None);
    }
    let n = data.len() as f64;
    let mean = data.iter().sum::<f64>() / n;
    let variance = data.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n;
    let std = variance.sqrt();
    (Some(mean - 3.0 * std), Some(mean + 3.0 * std))
}

/// The `percentile`-th percentile of `data` (a value in `[0, 100]`), using
/// numpy's default linear interpolation between ranks. Returns `None` for empty
/// input.
fn nanpercentile(data: &[f64], percentile: f64) -> Option<f64> {
    if data.is_empty() {
        return None;
    }
    let mut sorted = data.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("finite values are total-ordered"));
    if sorted.len() == 1 {
        return Some(sorted[0]);
    }
    // numpy 'linear': rank = q/100 * (n - 1), interpolate between floor/ceil.
    let rank = (percentile / 100.0) * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    let frac = rank - lo as f64;
    Some(sorted[lo] + frac * (sorted[hi] - sorted[lo]))
}

/// Build the 256-entry mask-overlay LUT, faithful to silx
/// `_BaseMaskToolsWidget._setMaskColors` (gui/plot/_BaseMaskToolsWidget.py
/// lines 984-1010).
///
/// The mask is a `uint8` per-pixel level (`0` unmasked, `1..=255` mask levels);
/// the overlay is rendered by indexing this LUT directly: `rgba = lut[level]`.
///
/// Arguments:
/// - `base`: default overlay RGB in `[0, 1]` (silx `_defaultOverlayColor`,
///   default `rgba("gray")`). Applied to every level whose override is `None`.
/// - `overrides`: per-level color override; `overrides[i] == Some(rgb in [0,1])`
///   gives level `i` a distinct color. Mirrors silx `_overlayColors[i]` where
///   `_defaultColors[i] == False` (silx lines 997-999). Indices beyond
///   `overrides.len()` fall back to `base`.
/// - `selected_level`: the silx `levelSpinBox` value (`1..=255`); this level
///   gets the full `alpha` (silx line 1005). Every other masked level gets
///   `alpha / 2` (silx line 1002).
/// - `alpha`: overlay alpha in `[0, 1]` (silx slider yields `[0.3, 1.0]`). The
///   value is clamped to `[0, 1]` to defend the contract.
///
/// Level `0` is always fully transparent `[0, 0, 0, 0]` (silx line 1008, set
/// last so it overrides both RGB and alpha for the unmasked level).
///
/// Float channels are mapped to `u8` exactly as silx does — silx uploads the
/// float LUT and the GL backend applies `numpy.clip(colors * 256, 0, 255)
/// .astype(uint8)`, which TRUNCATES (does not round, does not scale by 255).
pub fn mask_overlay_lut(
    base: [f32; 3],
    overrides: &[Option<[f32; 3]>],
    selected_level: u8,
    alpha: f32,
) -> [[u8; 4]; 256] {
    // silx slider produces alpha in [0.3, 1.0]; clamp to defend the contract.
    let alpha = alpha.clamp(0.0, 1.0);
    // silx maps float -> uint8 via numpy.clip(c * 256, 0, 255).astype(uint8),
    // which truncates toward zero.
    let to_u8 = |x: f32| (x * 256.0).clamp(0.0, 255.0) as u8;

    let half_alpha = alpha / 2.0;
    let mut lut = [[0u8; 4]; 256];
    for (i, entry) in lut.iter_mut().enumerate() {
        // silx lines 995/999: default overlay RGB, replaced by a per-level
        // override where one was set by the user.
        let rgb = overrides.get(i).copied().flatten().unwrap_or(base);
        // silx line 1002: every level starts at alpha / 2.
        *entry = [
            to_u8(rgb[0]),
            to_u8(rgb[1]),
            to_u8(rgb[2]),
            to_u8(half_alpha),
        ];
    }
    // silx line 1005: highlighted level gets the full alpha (overwrites
    // alpha / 2 written above).
    lut[selected_level as usize][3] = to_u8(alpha);
    // silx line 1008 (set LAST): the no-mask level is fully transparent,
    // overriding both its RGB and alpha.
    lut[0] = [0, 0, 0, 0];
    lut
}

#[cfg(test)]
mod tests {
    use super::*;

    /// silx `rgba("gray")` = `#a0a0a4` = (160, 160, 164) (gui/colors.py:71;
    /// `#808080`/128 is the commented-out `darkGray`, not silx's "gray"). In
    /// float: R=G=160/255, B=164/255, which truncate to bytes 160/160/164 via
    /// silx's `(c * 256).astype(uint8)` (160/255 * 256 = 160.6 -> 160;
    /// 164/255 * 256 = 164.6 -> 164).
    const GRAY: [f32; 3] = [160.0 / 255.0, 160.0 / 255.0, 164.0 / 255.0];

    #[test]
    fn mask_overlay_lut_matches_silx_set_mask_colors() {
        // silx _setMaskColors(level=1, alpha=0.8) with no per-level overrides
        // (_BaseMaskToolsWidget.py:984-1010).
        let lut = mask_overlay_lut(GRAY, &[], 1, 0.8);

        // silx line 1008: no-mask level is fully transparent.
        assert_eq!(lut[0], [0, 0, 0, 0]);
        // silx line 1005: selected level 1 gets full alpha.
        // rgb 160/255 * 256 = 160.6 -> trunc 160 (B 164); alpha 0.8 * 256 = 204.8 -> 204.
        assert_eq!(lut[1], [160, 160, 164, 204]);
        // silx line 1002: other masked levels get alpha / 2.
        // alpha/2 = 0.4; 0.4 * 256 = 102.4 -> trunc 102.
        assert_eq!(lut[2], [160, 160, 164, 102]);
        assert_eq!(lut[5], [160, 160, 164, 102]);
    }

    #[test]
    fn mask_overlay_lut_applies_per_level_override() {
        // silx lines 997-999: a per-level override replaces the base RGB at
        // that level only; its alpha still follows the level/alpha rule.
        let mut overrides = vec![None; 256];
        overrides[3] = Some([1.0, 0.0, 0.0]); // red override at level 3

        // selected_level = 1, so level 3 keeps alpha / 2 (silx line 1002).
        let lut = mask_overlay_lut(GRAY, &overrides, 1, 0.8);
        assert_eq!(lut[3], [255, 0, 0, 102]);

        // selected_level = 3, so level 3 now gets full alpha (silx line 1005).
        let lut = mask_overlay_lut(GRAY, &overrides, 3, 0.8);
        assert_eq!(lut[3][3], 204);
        assert_eq!(&lut[3][0..3], &[255, 0, 0]);
    }

    #[test]
    fn mask_overlay_lut_clamps_alpha_to_unit_range() {
        // The contract clamps alpha to [0, 1] before silx's float->u8 map.
        // alpha = 2.0 -> clamped 1.0; selected alpha 1.0 * 256 = 256 -> clip 255.
        // others alpha/2 = 0.5; 0.5 * 256 = 128 -> trunc 128.
        let lut = mask_overlay_lut(GRAY, &[], 1, 2.0);
        assert_eq!(lut[1][3], 255);
        assert_eq!(lut[2][3], 128);
    }

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
        assert_eq!(Normalization::Arcsinh.code(), 4);
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
    fn normalize_arcsinh_matches_asinh_ratio_with_no_domain_guard() {
        // asinh is defined for all reals, so there is no low-color guard (unlike
        // log/sqrt). bounds: asinh(0) = 0, asinh(sinh(1)) = 1.
        let vmax = 1.0_f64.sinh();
        let cm = Colormap::viridis(0.0, vmax).with_normalization(Normalization::Arcsinh);
        assert_eq!(cm.normalize(0.0), 0.0); // asinh(0) = 0 -> vmin
        assert!((cm.normalize(vmax) - 1.0).abs() < 1e-6); // asinh(vmax) -> 1
        // A negative value below vmin clamps to the low color rather than being
        // rejected: asinh(-x) is finite, the clamp does the flooring.
        assert_eq!(cm.normalize(-5.0), 0.0);
    }

    #[test]
    fn norm_bounds_transform_arcsinh_bounds() {
        // 1 / (asinh(vmax) - asinh(vmin)) with vmin = 0, vmax = sinh(2) -> 1/2.
        let vmax = 2.0_f64.sinh();
        let cm = Colormap::viridis(0.0, vmax).with_normalization(Normalization::Arcsinh);
        let (cmin, oor) = cm.norm_bounds();
        assert_eq!(cmin, 0.0); // asinh(0)
        assert!((oor - 0.5).abs() < 1e-6);
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

    // --- Arcsinh normalization -------------------------------------------

    #[test]
    fn normalize_arcsinh_endpoints_and_monotonic() {
        // asinh is monotonic over all reals, so vmin/vmax pin the [0, 1] ends
        // and the mapping is strictly increasing in between.
        let cm = Colormap::viridis(-10.0, 10.0).with_normalization(Normalization::Arcsinh);
        assert_eq!(cm.normalize(-10.0), 0.0); // vmin -> low
        assert_eq!(cm.normalize(10.0), 1.0); // vmax -> high

        // asinh(0) = 0 is the midpoint of asinh(-10)..asinh(10) (odd function).
        assert!((cm.normalize(0.0) - 0.5).abs() < 1e-6);

        // Strictly increasing across a swept range.
        let mut prev = cm.normalize(-10.0);
        for i in 1..=40 {
            let v = -10.0 + (i as f64) * 0.5;
            let cur = cm.normalize(v);
            assert!(cur >= prev, "arcsinh not monotonic at v={v}");
            prev = cur;
        }
    }

    #[test]
    fn norm_bounds_arcsinh_transforms_with_asinh() {
        let cm = Colormap::viridis(-10.0, 10.0).with_normalization(Normalization::Arcsinh);
        let (cmin, oor) = cm.norm_bounds();
        assert!((cmin as f64 - (-10.0f64).asinh()).abs() < 1e-6);
        let expected_oor = 1.0 / (10.0f64.asinh() - (-10.0f64).asinh());
        assert!((oor as f64 - expected_oor).abs() < 1e-6);
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

    // --- Reversed LUT ----------------------------------------------------

    #[test]
    fn reversed_lut_equals_base_lut_reversed() {
        // The reversed builder yields the base LUT in reverse order (silx
        // "reversed gray" / "_r"), one entry-for-entry mirror.
        for name in ColormapName::ALL {
            let base = Colormap::new(name, 0.0, 1.0);
            let rev = base.clone().reversed();
            for i in 0..256 {
                assert_eq!(rev.lut[i], base.lut[255 - i], "{} entry {i}", name.label());
            }
        }
    }

    #[test]
    fn reversed_gray_matches_silx_descending_ramp() {
        // silx "reversed gray" = arange(255, -1, -1) in all RGB channels.
        let rev = Colormap::new(ColormapName::Gray, 0.0, 1.0).reversed();
        assert_eq!(rev.lut[0], [255, 255, 255, 255]);
        assert_eq!(rev.lut[255], [0, 0, 0, 255]);
        assert_eq!(rev.lut[1], [254, 254, 254, 255]);
    }

    // --- NaN color -------------------------------------------------------

    #[test]
    fn nan_color_defaults_to_transparent_white_and_is_settable() {
        // silx _DEFAULT_NAN_COLOR = (255, 255, 255, 0).
        let cm = Colormap::viridis(0.0, 1.0);
        assert_eq!(cm.nan_color, [255, 255, 255, 0]);
        let recolored = cm.with_nan_color([10, 20, 30, 255]);
        assert_eq!(recolored.nan_color, [10, 20, 30, 255]);
    }

    // --- Autoscale modes -------------------------------------------------

    #[test]
    fn autoscale_minmax_is_exact_finite_range() {
        let data = [3.0, -1.0, 5.0, 2.0];
        assert_eq!(
            AutoscaleMode::MinMax.range(&data, DEFAULT_PERCENTILES),
            (-1.0, 5.0)
        );
    }

    #[test]
    fn autoscale_stddev3_is_mean_plus_minus_3std_clamped_to_data() {
        // [0, 0, 0, 0, 10]: mean = 2, population std = 4, so mean±3·std =
        // [-10, 14]; clamped into the data range [0, 10] -> [0, 10].
        let data = [0.0, 0.0, 0.0, 0.0, 10.0];
        let (vmin, vmax) = AutoscaleMode::Stddev3.range(&data, DEFAULT_PERCENTILES);
        assert!((vmin - 0.0).abs() < 1e-9, "vmin {vmin}");
        assert!((vmax - 10.0).abs() < 1e-9, "vmax {vmax}");

        // A tight cluster keeps mean±3·std inside the data range: data
        // [1, 2, 3, 4, 5] has mean 3, std sqrt(2); mean±3·std = 3 ± 3·sqrt(2)
        // = [-1.2426, 7.2426] -> clamped to data range [1, 5].
        let data2 = [1.0, 2.0, 3.0, 4.0, 5.0];
        let (vmin2, vmax2) = AutoscaleMode::Stddev3.range(&data2, DEFAULT_PERCENTILES);
        assert!((vmin2 - 1.0).abs() < 1e-9, "vmin2 {vmin2}");
        assert!((vmax2 - 5.0).abs() < 1e-9, "vmax2 {vmax2}");
    }

    #[test]
    fn autoscale_percentile_default_1_99_bounds() {
        // 0..=100 (101 samples). numpy linear interpolation:
        // rank(1%)  = 0.01 * 100 = 1.0  -> data[1]  = 1.0
        // rank(99%) = 0.99 * 100 = 99.0 -> data[99] = 99.0
        let data: Vec<f64> = (0..=100).map(|i| i as f64).collect();
        let (vmin, vmax) = AutoscaleMode::Percentile.range(&data, DEFAULT_PERCENTILES);
        assert!((vmin - 1.0).abs() < 1e-9, "vmin {vmin}");
        assert!((vmax - 99.0).abs() < 1e-9, "vmax {vmax}");
    }

    #[test]
    fn autoscale_percentile_interpolates_between_ranks() {
        // [10, 20, 30, 40]: rank(50%) = 0.5 * 3 = 1.5 -> halfway between
        // data[1]=20 and data[2]=30 -> 25.
        let data = [10.0, 20.0, 30.0, 40.0];
        assert_eq!(nanpercentile(&data, 50.0), Some(25.0));
    }

    #[test]
    fn autoscale_drops_nonfinite_and_falls_back_when_empty() {
        // NaN/inf are stripped before computing the range.
        let data = [f64::NAN, 4.0, f64::INFINITY, 2.0];
        assert_eq!(
            AutoscaleMode::MinMax.range(&data, DEFAULT_PERCENTILES),
            (2.0, 4.0)
        );
        // No finite samples -> silx DEFAULT_RANGE (0, 1).
        let empty: [f64; 0] = [];
        assert_eq!(
            AutoscaleMode::MinMax.range(&empty, DEFAULT_PERCENTILES),
            (0.0, 1.0)
        );
        let all_nan = [f64::NAN, f64::NAN];
        assert_eq!(
            AutoscaleMode::Stddev3.range(&all_nan, DEFAULT_PERCENTILES),
            (0.0, 1.0)
        );
    }

    // --- Custom LUT registration -----------------------------------------

    #[test]
    fn with_lut_replaces_the_table_exactly() {
        let mut table = [[1u8, 2, 3, 4]; 256];
        table[0] = [9, 8, 7, 6];
        table[255] = [10, 11, 12, 13];
        let cm = Colormap::viridis(0.0, 1.0).with_lut(table);
        assert_eq!(cm.lut, table);
        // Range and other fields are untouched.
        assert_eq!((cm.vmin, cm.vmax), (0.0, 1.0));
        assert_eq!(cm.normalization, Normalization::Linear);
    }

    #[test]
    fn from_colors_resamples_to_256_entries() {
        // Two endpoints -> a LUT whose first half is the low color and second
        // half the high color (nearest-neighbour rounds at the midpoint).
        let cm = Colormap::from_colors(&[[0, 0, 0, 255], [255, 255, 255, 255]], 0.0, 1.0)
            .expect("non-empty");
        assert_eq!(cm.lut.len(), 256);
        assert_eq!(cm.lut[0], [0, 0, 0, 255]);
        assert_eq!(cm.lut[255], [255, 255, 255, 255]);
        // round(127/255 * 1) = 0 -> low; round(128/255 * 1) = 1 -> high.
        assert_eq!(cm.lut[127], [0, 0, 0, 255]);
        assert_eq!(cm.lut[128], [255, 255, 255, 255]);
    }

    #[test]
    fn from_colors_identity_for_length_256() {
        let mut src = vec![[0u8; 4]; 256];
        for (i, c) in src.iter_mut().enumerate() {
            *c = [i as u8, 0, 0, 255];
        }
        let cm = Colormap::from_colors(&src, 0.0, 1.0).expect("non-empty");
        for (i, (&out, &want)) in cm.lut.iter().zip(src.iter()).enumerate() {
            assert_eq!(out, want, "entry {i}");
        }
    }

    #[test]
    fn from_colors_single_color_fills_whole_lut() {
        let cm = Colormap::from_colors(&[[7, 8, 9, 255]], 0.0, 1.0).expect("non-empty");
        assert!(cm.lut.iter().all(|&c| c == [7, 8, 9, 255]));
    }

    #[test]
    fn from_colors_empty_is_none() {
        assert!(Colormap::from_colors(&[], 0.0, 1.0).is_none());
    }

    #[test]
    fn resample_lut_endpoints_and_length() {
        // N = 3: index 0 -> row 0, 255 -> row 2, midpoint 128 -> row 1.
        let lut =
            resample_lut(&[[0, 0, 0, 255], [128, 0, 0, 255], [255, 0, 0, 255]]).expect("non-empty");
        assert_eq!(lut.len(), 256);
        assert_eq!(lut[0], [0, 0, 0, 255]);
        assert_eq!(lut[255], [255, 0, 0, 255]);
        // round(128/255 * 2) = round(1.004) = 1 -> middle row.
        assert_eq!(lut[128], [128, 0, 0, 255]);
    }

    #[test]
    fn resample_lut_empty_is_none() {
        assert!(resample_lut(&[]).is_none());
    }

    // --- Autoscale percentiles field -------------------------------------

    #[test]
    fn autoscale_percentiles_default_and_clamp() {
        let mut cm = Colormap::viridis(0.0, 1.0);
        assert_eq!(cm.autoscale_percentiles, DEFAULT_PERCENTILES);
        // In-range values are kept verbatim.
        assert!(cm.set_autoscale_percentiles(5.0, 95.0));
        assert_eq!(cm.autoscale_percentiles, (5.0, 95.0));
        // Out-of-range values clamp into [0, 100].
        assert!(cm.set_autoscale_percentiles(-10.0, 150.0));
        assert_eq!(cm.autoscale_percentiles, (0.0, 100.0));
    }

    #[test]
    fn autoscale_percentiles_orders_low_below_high() {
        let mut cm = Colormap::viridis(0.0, 1.0);
        // Inverted pair is reordered after clamping.
        assert!(cm.set_autoscale_percentiles(90.0, 10.0));
        assert_eq!(cm.autoscale_percentiles, (10.0, 90.0));
    }

    // --- Editable guard --------------------------------------------------

    #[test]
    fn editable_defaults_true_and_guards_mutating_setters() {
        let mut cm = Colormap::viridis(0.0, 1.0);
        assert!(cm.is_editable());

        cm.set_editable(false);
        assert!(!cm.is_editable());

        // Each editable-guarded setter is a no-op returning false.
        let before = cm.clone();
        assert!(!cm.set_lut([[1, 2, 3, 4]; 256]));
        assert!(!cm.set_autoscale_percentiles(5.0, 95.0));
        let other = Colormap::new(ColormapName::Jet, 2.0, 3.0);
        assert!(!cm.set_from(&other));
        assert_eq!(cm, before);

        // Re-enabling lets the same setters through.
        cm.set_editable(true);
        assert!(cm.set_autoscale_percentiles(5.0, 95.0));
        assert_eq!(cm.autoscale_percentiles, (5.0, 95.0));
    }

    #[test]
    fn set_from_copies_all_fields_including_editable_flag() {
        let source = Colormap::new(ColormapName::Jet, 2.0, 8.0)
            .with_normalization(Normalization::Log)
            .with_gamma(3.0)
            .with_nan_color([1, 2, 3, 4]);
        let mut source = source;
        assert!(source.set_autoscale_percentiles(2.0, 98.0));
        source.set_editable(false); // source is no longer editable

        let mut dst = Colormap::viridis(0.0, 1.0);
        assert!(dst.set_from(&source)); // dst is still editable here
        assert_eq!(dst, source);
        // The editable flag was overwritten from source (now false).
        assert!(!dst.is_editable());
    }

    #[test]
    fn copy_carries_editable_flag_unguarded() {
        // copy() bypasses the editable guard (mirrors silx Colormap.copy).
        let mut cm = Colormap::new(ColormapName::Gray, 0.0, 1.0);
        cm.set_editable(false);
        let dup = cm.copy();
        assert_eq!(dup, cm);
        assert!(!dup.is_editable());
    }
}
