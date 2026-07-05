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
use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

/// How a scalar value is mapped to the `[0, 1]` LUT coordinate before the color
/// lookup (silx `Colormap.normalization`). Mirrors silx's `GLPlotImage`
/// normalizations; the numeric `Normalization::code` matches its `normID`.
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
    pub(crate) fn transform(self, v: f64) -> f64 {
        match self {
            Normalization::Linear | Normalization::Gamma => v,
            Normalization::Log => v.log10(),
            Normalization::Sqrt => v.sqrt(),
            Normalization::Arcsinh => v.asinh(),
        }
    }

    /// Inverse of [`transform`](Self::transform): map a value back from the
    /// transformed space to data space, so `inverse_transform(transform(v)) == v`
    /// for every `v` in the transform's domain (`v > 0` for log, `v >= 0` for
    /// sqrt; all `v` otherwise). Turns a dragged pixel position on the inline
    /// `HistogramColorBar` back into a data value under any normalization.
    /// [`Gamma`](Normalization::Gamma) is linear in this space (the exponent is
    /// applied to the ratio, not the value), so its inverse is the identity —
    /// matching [`transform`](Self::transform).
    pub(crate) fn inverse_transform(self, t: f64) -> f64 {
        match self {
            Normalization::Linear | Normalization::Gamma => t,
            Normalization::Log => 10f64.powf(t),
            Normalization::Sqrt => t * t,
            Normalization::Arcsinh => t.sinh(),
        }
    }

    /// The autoscale fallback `(vmin, vmax)` when there is no usable data
    /// (silx `_NormalizationMixIn.DEFAULT_RANGE`): `(1, 10)` for
    /// [`Log`](Normalization::Log) (`LogarithmicNormalization.DEFAULT_RANGE`,
    /// math/colormap.py:410), `(0, 1)` for every other normalization.
    pub fn default_autoscale_range(self) -> (f64, f64) {
        match self {
            Normalization::Log => (1.0, 10.0),
            _ => (0.0, 1.0),
        }
    }

    /// Whether `v` is in this normalization's valid autoscale domain (silx
    /// `_NormalizationMixIn.is_valid`): `v > 0` for
    /// [`Log`](Normalization::Log) (math/colormap.py:417-419), `v >= 0` for
    /// [`Sqrt`](Normalization::Sqrt) (math/colormap.py:434-436), everything
    /// (including non-finite values — silx's base `is_valid` is all-`True`;
    /// finiteness is filtered separately) otherwise.
    pub fn is_valid_autoscale_value(self, v: f64) -> bool {
        match self {
            Normalization::Log => v > 0.0,
            Normalization::Sqrt => v >= 0.0,
            _ => true,
        }
    }
}

/// A named colormap in the built-in catalog. The perceptual maps are backed by
/// `colorous` gradients; silx's analytic maps (`gray`, `red`, `green`, `blue`,
/// `temperature`) and the matplotlib-derived `jet`/`hsv` are built by
/// `ColormapName::build_lut`.
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

    // Additional `colorous` gradients (the d3-scale-chromatic / ColorBrewer set
    // that overlaps matplotlib's catalog). silx exposes all matplotlib maps
    // dynamically; rsplot cannot load matplotlib at runtime, so it ships the
    // colorous equivalents statically. Sequential single-hue:
    Blues,
    Greens,
    Oranges,
    Purples,
    Reds,
    // Sequential multi-hue:
    Warm,
    Cool,
    Cubehelix,
    BlueGreen,
    BluePurple,
    GreenBlue,
    OrangeRed,
    PurpleBlueGreen,
    PurpleBlue,
    PurpleRed,
    RedPurple,
    YellowGreenBlue,
    YellowGreen,
    YellowOrangeBrown,
    YellowOrangeRed,
    // Diverging:
    BrownGreen,
    PurpleGreen,
    PinkGreen,
    PurpleOrange,
    RedBlue,
    RedGrey,
    RedYellowBlue,
    RedYellowGreen,
    // Cyclical:
    Rainbow,
    Sinebow,
}

impl ColormapName {
    /// All catalog entries, for building a picker. Ordered to match silx's
    /// preferred-colormap list (`colors.py:1086`) where the entries overlap.
    pub const ALL: [ColormapName; 45] = [
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
        ColormapName::Blues,
        ColormapName::Greens,
        ColormapName::Oranges,
        ColormapName::Purples,
        ColormapName::Reds,
        ColormapName::Warm,
        ColormapName::Cool,
        ColormapName::Cubehelix,
        ColormapName::BlueGreen,
        ColormapName::BluePurple,
        ColormapName::GreenBlue,
        ColormapName::OrangeRed,
        ColormapName::PurpleBlueGreen,
        ColormapName::PurpleBlue,
        ColormapName::PurpleRed,
        ColormapName::RedPurple,
        ColormapName::YellowGreenBlue,
        ColormapName::YellowGreen,
        ColormapName::YellowOrangeBrown,
        ColormapName::YellowOrangeRed,
        ColormapName::BrownGreen,
        ColormapName::PurpleGreen,
        ColormapName::PinkGreen,
        ColormapName::PurpleOrange,
        ColormapName::RedBlue,
        ColormapName::RedGrey,
        ColormapName::RedYellowBlue,
        ColormapName::RedYellowGreen,
        ColormapName::Rainbow,
        ColormapName::Sinebow,
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
            ColormapName::Blues => Some(colorous::BLUES),
            ColormapName::Greens => Some(colorous::GREENS),
            ColormapName::Oranges => Some(colorous::ORANGES),
            ColormapName::Purples => Some(colorous::PURPLES),
            ColormapName::Reds => Some(colorous::REDS),
            ColormapName::Warm => Some(colorous::WARM),
            ColormapName::Cool => Some(colorous::COOL),
            ColormapName::Cubehelix => Some(colorous::CUBEHELIX),
            ColormapName::BlueGreen => Some(colorous::BLUE_GREEN),
            ColormapName::BluePurple => Some(colorous::BLUE_PURPLE),
            ColormapName::GreenBlue => Some(colorous::GREEN_BLUE),
            ColormapName::OrangeRed => Some(colorous::ORANGE_RED),
            ColormapName::PurpleBlueGreen => Some(colorous::PURPLE_BLUE_GREEN),
            ColormapName::PurpleBlue => Some(colorous::PURPLE_BLUE),
            ColormapName::PurpleRed => Some(colorous::PURPLE_RED),
            ColormapName::RedPurple => Some(colorous::RED_PURPLE),
            ColormapName::YellowGreenBlue => Some(colorous::YELLOW_GREEN_BLUE),
            ColormapName::YellowGreen => Some(colorous::YELLOW_GREEN),
            ColormapName::YellowOrangeBrown => Some(colorous::YELLOW_ORANGE_BROWN),
            ColormapName::YellowOrangeRed => Some(colorous::YELLOW_ORANGE_RED),
            ColormapName::BrownGreen => Some(colorous::BROWN_GREEN),
            ColormapName::PurpleGreen => Some(colorous::PURPLE_GREEN),
            ColormapName::PinkGreen => Some(colorous::PINK_GREEN),
            ColormapName::PurpleOrange => Some(colorous::PURPLE_ORANGE),
            ColormapName::RedBlue => Some(colorous::RED_BLUE),
            ColormapName::RedGrey => Some(colorous::RED_GREY),
            ColormapName::RedYellowBlue => Some(colorous::RED_YELLOW_BLUE),
            ColormapName::RedYellowGreen => Some(colorous::RED_YELLOW_GREEN),
            ColormapName::Rainbow => Some(colorous::RAINBOW),
            ColormapName::Sinebow => Some(colorous::SINEBOW),
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

    /// silx's overlay/cursor color for this colormap — a color that stays
    /// visible on top of it (`cursorColorForColormap`, gui/colors.py:210 →
    /// `get_colormap_cursor_color` over `_AVAILABLE_LUTS`,
    /// math/colormap.py:52-66 and :185-196): pink `#ff66ff` for the light-tone
    /// builtin LUTs, green `#00ff00` for red/magma/inferno/plasma, yellow
    /// `#ffff00` for blue, and black for every other name — silx loads those
    /// from matplotlib with the `registerLUT` default `cursor_color="black"`
    /// (colors.py:244).
    pub fn cursor_color(self) -> [u8; 4] {
        match self {
            ColormapName::Gray
            | ColormapName::Green
            | ColormapName::Viridis
            | ColormapName::Cividis
            | ColormapName::Temperature => [255, 102, 255, 255], // #ff66ff
            ColormapName::Red
            | ColormapName::Magma
            | ColormapName::Inferno
            | ColormapName::Plasma => [0, 255, 0, 255], // #00ff00
            ColormapName::Blue => [255, 255, 0, 255], // #ffff00
            _ => DEFAULT_CURSOR_COLOR,
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
            ColormapName::Blues => "Blues",
            ColormapName::Greens => "Greens",
            ColormapName::Oranges => "Oranges",
            ColormapName::Purples => "Purples",
            ColormapName::Reds => "Reds",
            ColormapName::Warm => "Warm",
            ColormapName::Cool => "Cool",
            ColormapName::Cubehelix => "Cubehelix",
            ColormapName::BlueGreen => "Blue-Green",
            ColormapName::BluePurple => "Blue-Purple",
            ColormapName::GreenBlue => "Green-Blue",
            ColormapName::OrangeRed => "Orange-Red",
            ColormapName::PurpleBlueGreen => "Purple-Blue-Green",
            ColormapName::PurpleBlue => "Purple-Blue",
            ColormapName::PurpleRed => "Purple-Red",
            ColormapName::RedPurple => "Red-Purple",
            ColormapName::YellowGreenBlue => "Yellow-Green-Blue",
            ColormapName::YellowGreen => "Yellow-Green",
            ColormapName::YellowOrangeBrown => "Yellow-Orange-Brown",
            ColormapName::YellowOrangeRed => "Yellow-Orange-Red",
            ColormapName::BrownGreen => "Brown-Green",
            ColormapName::PurpleGreen => "Purple-Green",
            ColormapName::PinkGreen => "Pink-Green",
            ColormapName::PurpleOrange => "Purple-Orange",
            ColormapName::RedBlue => "Red-Blue",
            ColormapName::RedGrey => "Red-Grey",
            ColormapName::RedYellowBlue => "Red-Yellow-Blue",
            ColormapName::RedYellowGreen => "Red-Yellow-Green",
            ColormapName::Rainbow => "Rainbow",
            ColormapName::Sinebow => "Sinebow",
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

/// silx's default overlay/cursor color for a registered LUT
/// (`registerLUT(..., cursor_color="black")`) and for any nameless colormap
/// (`get_colormap_cursor_color` fallback, math/colormap.py:196): opaque black.
pub(crate) const DEFAULT_CURSOR_COLOR: [u8; 4] = [0, 0, 0, 255];

/// A custom colormap LUT registered by name (silx `register_colormap`): the
/// resolved 256-entry table plus the overlay cursor color silx associates with
/// it (used when drawing an overlay over an image colormapped with this LUT).
#[derive(Clone, Debug, PartialEq)]
struct RegisteredColormap {
    lut: [[u8; 4]; 256],
    cursor_color: [u8; 4],
}

/// Process-global registry of custom named colormap LUTs — the rsplot analogue
/// of silx's module-level `_AVAILABLE_LUTS` / `_COLORMAP_CACHE`. A `BTreeMap`
/// keeps [`registered_colormaps`] deterministic; the `RwLock` allows concurrent
/// name resolution. Lazily initialised on first use.
fn registry() -> &'static RwLock<BTreeMap<String, RegisteredColormap>> {
    static REGISTRY: OnceLock<RwLock<BTreeMap<String, RegisteredColormap>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// Register a custom colormap LUT under `name` so it can be resolved by name
/// through [`Colormap::from_registered`] (silx `silx.gui.colors.registerLUT` /
/// `silx.math.colormap.register_colormap`).
///
/// `colors` is an arbitrary-length (`N >= 1`) array of RGBA `u8` rows, resampled
/// to the 256-entry LUT exactly as [`Colormap::from_colors`] does. `cursor_color`
/// is the overlay color silx stores alongside the LUT (`None` → opaque black,
/// silx's `cursor_color="black"` default). Registering an already-registered
/// `name` overrides it (silx explicitly allows overriding).
///
/// Returns `false` and registers nothing when `colors` is empty, mirroring
/// silx's non-empty-LUT assertion.
pub fn register_colormap(name: &str, colors: &[[u8; 4]], cursor_color: Option<[u8; 4]>) -> bool {
    let Some(lut) = resample_lut(colors) else {
        return false;
    };
    let entry = RegisteredColormap {
        lut,
        cursor_color: cursor_color.unwrap_or(DEFAULT_CURSOR_COLOR),
    };
    registry().write().unwrap().insert(name.to_owned(), entry);
    true
}

/// The names of all currently registered custom colormaps, in sorted order
/// (silx `get_registered_colormaps`).
pub fn registered_colormaps() -> Vec<String> {
    registry().read().unwrap().keys().cloned().collect()
}

/// The overlay cursor color of a registered colormap (silx
/// `get_colormap_cursor_color`), or `None` if `name` is not registered.
pub fn registered_colormap_cursor_color(name: &str) -> Option<[u8; 4]> {
    registry().read().unwrap().get(name).map(|c| c.cursor_color)
}

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
    /// Whether `vmin` tracks the data instead of being pinned — silx
    /// `Colormap.vmin is None` (autoscale that bound, `math/colormap.py`). The
    /// concrete `vmin` above is still the range *in effect* for rendering (all
    /// readers use it); this flag only tells [`Self::resolved`] to refresh it
    /// from data on each update. Both flags default `false` (a `new`/`viridis`/
    /// `from_*` colormap has explicit, pinned bounds).
    pub vmin_auto: bool,
    /// Whether `vmax` tracks the data — silx `Colormap.vmax is None`. See
    /// [`Self::vmin_auto`].
    pub vmax_auto: bool,
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
    /// The overlay/cursor color silx associates with this colormap's
    /// originating name (`cursorColorForColormap(colormap["name"])`) — what
    /// overlays (mask, crosshair) draw in to stay visible on top of the LUT.
    /// [`ColormapName::cursor_color`] for catalog names, the registry's color
    /// for [`Self::from_registered`], and black for a raw LUT
    /// ([`Self::from_colors`], [`Self::set_lut`], [`Self::with_lut`]) — silx's
    /// `setColormapLUT` clears the name, and a nameless colormap resolves to
    /// `"black"` (math/colormap.py:185-196). [`Self::reversed`] keeps it, as
    /// silx's `"reversed gray"` table entry keeps `"gray"`'s color.
    pub cursor_color: [u8; 4],
}

impl Colormap {
    /// Build a colormap from a catalog `name` over `[vmin, vmax]` with linear
    /// normalization and the default gamma.
    pub fn new(name: ColormapName, vmin: f64, vmax: f64) -> Self {
        Self {
            lut: name.build_lut(),
            vmin,
            vmax,
            vmin_auto: false,
            vmax_auto: false,
            normalization: Normalization::Linear,
            gamma: DEFAULT_GAMMA,
            nan_color: DEFAULT_NAN_COLOR,
            autoscale_percentiles: DEFAULT_PERCENTILES,
            editable: true,
            cursor_color: name.cursor_color(),
        }
    }

    /// A colormap over `name` whose range autoscales to the data — silx's
    /// default `Colormap(name, vmin=None, vmax=None)` (`ColormapMixIn` items,
    /// `ScalarFieldView` cut plane). Both bounds are `auto`; the concrete
    /// `[0, 1]` placeholder is only what renders before any data arrives and is
    /// replaced by [`Self::resolved`] on the first update.
    pub fn autoscale(name: ColormapName) -> Self {
        Self {
            vmin_auto: true,
            vmax_auto: true,
            ..Self::new(name, 0.0, 1.0)
        }
    }

    /// The perceptually-uniform "viridis" colormap over `[vmin, vmax]`.
    pub fn viridis(vmin: f64, vmax: f64) -> Self {
        Self::new(ColormapName::Viridis, vmin, vmax)
    }

    /// Rebuild the LUT from a catalog `name`, keeping the value range,
    /// normalization, gamma, and the other settings (silx `Colormap.setName`).
    /// rsplot stores the LUT rather than the name, so this replaces the 256
    /// entries in place, and re-derives
    /// [`cursor_color`](Self::cursor_color) from the new name as silx's
    /// name-keyed `cursorColorForColormap` lookup would.
    pub fn set_name(&mut self, name: ColormapName) {
        self.lut = name.build_lut();
        self.cursor_color = name.cursor_color();
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
    /// with [`Self::set_lut`] for the editable-guarded setter. Resets
    /// [`cursor_color`](Self::cursor_color) to black: silx's `setColormapLUT`
    /// clears the name, and a nameless colormap's cursor color is `"black"`
    /// (math/colormap.py:185-196).
    pub fn with_lut(mut self, lut: [[u8; 4]; 256]) -> Self {
        self.lut = lut;
        self.cursor_color = DEFAULT_CURSOR_COLOR;
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
            vmin_auto: false,
            vmax_auto: false,
            normalization: Normalization::Linear,
            gamma: DEFAULT_GAMMA,
            nan_color: DEFAULT_NAN_COLOR,
            autoscale_percentiles: DEFAULT_PERCENTILES,
            editable: true,
            // A raw-LUT colormap has no name; silx resolves that to "black"
            // (math/colormap.py:185-196).
            cursor_color: DEFAULT_CURSOR_COLOR,
        })
    }

    /// Build a colormap from a custom LUT previously registered under `name`
    /// (silx `Colormap(name=...)` resolving a `registerLUT` name), over
    /// `[vmin, vmax]` with linear normalization and the default gamma.
    ///
    /// Returns `None` when no colormap is registered under `name` (register one
    /// first with [`register_colormap`]).
    pub fn from_registered(name: &str, vmin: f64, vmax: f64) -> Option<Self> {
        let (lut, cursor_color) = {
            let registry = registry().read().unwrap();
            let entry = registry.get(name)?;
            (entry.lut, entry.cursor_color)
        };
        Some(Self {
            lut,
            vmin,
            vmax,
            vmin_auto: false,
            vmax_auto: false,
            normalization: Normalization::Linear,
            gamma: DEFAULT_GAMMA,
            nan_color: DEFAULT_NAN_COLOR,
            autoscale_percentiles: DEFAULT_PERCENTILES,
            editable: true,
            // The color registered alongside the LUT (silx `registerLUT`'s
            // `cursor_color`, black by default).
            cursor_color,
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
    /// Resets [`cursor_color`](Self::cursor_color) to black: silx's
    /// `setColormapLUT` clears the name, and a nameless colormap's cursor
    /// color is `"black"` (math/colormap.py:185-196).
    pub fn set_lut(&mut self, lut: [[u8; 4]; 256]) -> bool {
        if !self.editable {
            return false;
        }
        self.lut = lut;
        self.cursor_color = DEFAULT_CURSOR_COLOR;
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

    /// The 256-entry LUT index for `v` under this colormap's normalization — silx
    /// `_colormap.pyx:345-376`: `int(ratio · nb_colors)` capped at `nb_colors − 1`
    /// (`nb_colors == 256`), NOT `ratio · 255`. [`normalize`](Self::normalize)
    /// clamps the ratio to `[0, 1]`, so `ratio == 1.0` (and the `+inf`-clamped
    /// top) yields `256`, which the cap pulls back to the last entry; a `NaN`
    /// ratio (a degenerate range mapped over `±inf`) casts to `0` under Rust's
    /// saturating float→int, landing on the low color like silx's degenerate
    /// fallback. The single owner of the value→index quantization shared by
    /// [`color_at`](Self::color_at) and the CPU image/scatter colorizers, kept
    /// identical to the GL path's NEAREST LUT sampler (`GLPlotImage.py:338-347`);
    /// `ratio · 255` (the old rule) put roughly half of all values one entry away
    /// from silx.
    pub(crate) fn lut_index(&self, v: f64) -> usize {
        ((self.normalize(v) * 256.0) as usize).min(self.lut.len() - 1)
    }

    /// Map a data value to its straight-alpha `[r, g, b, a]` color under this
    /// colormap — the CPU mirror of the image shader's LUT lookup, the single
    /// value→color entry point for items that colour their geometry on the CPU
    /// (e.g. 3D scatter). Only `NaN` yields [`nan_color`](Self::nan_color) (silx
    /// `_colormap.pyx:362-376` / `GLPlotImage.py:202-206`: `nancolor` is used
    /// solely for `isnan`); `±inf` survive normalization and clamp into the LUT
    /// ends (`+inf` → top color, `-inf` → bottom), matching the GL path. The index
    /// is [`lut_index`](Self::lut_index) (silx's `int(ratio · 256)` binning).
    pub fn color_at(&self, v: f64) -> [u8; 4] {
        if v.is_nan() {
            return self.nan_color;
        }
        self.lut[self.lut_index(v)]
    }

    /// The `(vmin, vmax)` autoscale range over `data` for `mode` under *this
    /// colormap's* normalization and percentile pair — silx
    /// `Colormap._computeAutoscaleRange` (colors.py:682-692), which dispatches
    /// to the normalizer's `autoscale`. This is the single entry point every
    /// raw-data autoscale should use; see [`AutoscaleMode::range`] for the
    /// per-normalization semantics.
    pub fn autoscale_range(&self, mode: AutoscaleMode, data: &[f64]) -> (f64, f64) {
        mode.range(self.normalization, data, self.autoscale_percentiles)
    }

    /// Clone `self` with its value range replaced by the [`AutoscaleMode`] range
    /// over `data` (via [`Self::autoscale_range`], so the range honors this
    /// colormap's normalization and percentile pair). The LUT, normalization,
    /// gamma, NaN color, and percentiles are preserved — only `vmin`/`vmax`
    /// change.
    ///
    /// This is the shared primitive for silx's "colormap with `vmin`/`vmax =
    /// None`, autoscaled to the item's own data" contract (`ColormapMixIn`
    /// items, `ImageStack`, `ImageComplexData`, the 3D colormapped items): a
    /// base colormap carries the name/normalization, the item re-derives the
    /// range from its data on each update.
    pub fn autoscaled(&self, mode: AutoscaleMode, data: &[f64]) -> Colormap {
        let (vmin, vmax) = self.autoscale_range(mode, data);
        let mut cm = self.clone();
        cm.vmin = vmin;
        cm.vmax = vmax;
        cm
    }

    /// Whether either bound autoscales to data ([`Self::vmin_auto`] /
    /// [`Self::vmax_auto`]) — silx `Colormap.vmin is None or vmax is None`.
    pub fn is_autoscale(&self) -> bool {
        self.vmin_auto || self.vmax_auto
    }

    /// Clone `self` with only its *auto* bounds refreshed from `data`, keeping
    /// pinned bounds and the auto flags — silx `getColormapRange(data)`, which
    /// fills a `None` bound from the data and keeps a set one
    /// (`math/colormap.py`). This is the per-bound counterpart of
    /// [`Self::autoscaled`] (which forces *both* bounds): "pin `vmax`, let
    /// `vmin` track the data" resolves to `(autoscale_min, vmax)`. A fully
    /// pinned colormap resolves to an unchanged clone.
    pub fn resolved(&self, mode: AutoscaleMode, data: &[f64]) -> Colormap {
        let mut cm = self.clone();
        if !self.is_autoscale() {
            return cm;
        }
        let (amin, amax) = self.autoscale_range(mode, data);
        // silx `_getColormapRange` ordering-clamp tail (colors.py:739-748): the
        // auto side is clamped against the *pinned* opposite so the resolved
        // range always satisfies `vmin <= vmax`. Filling only the auto side with
        // the raw data bound leaves an inverted range when a pinned bound sits on
        // the wrong side of the data — e.g. pin `vmax = 2` over data `[3, 90]`
        // with `vmin` auto gives `(3, 2)`, which violates `norm_bounds`' `vmax >
        // vmin` precondition and collapses the whole item to the low color. silx
        // returns the degenerate-but-ordered `(2, 2)` instead.
        let vmin = if self.vmin_auto {
            if self.vmax_auto {
                amin
            } else {
                amin.min(self.vmax)
            }
        } else {
            self.vmin
        };
        let vmax = if self.vmax_auto {
            amax.max(vmin)
        } else {
            self.vmax
        };
        cm.vmin = vmin;
        cm.vmax = vmax;
        cm
    }
}

/// silx's default `(low, high)` percentiles for [`AutoscaleMode::Percentile`]
/// (`Colormap._DEFAULT_PERCENTILES`).
pub const DEFAULT_PERCENTILES: (f64, f64) = (1.0, 99.0);

/// How autoscale derives `(vmin, vmax)` from data (silx `Colormap.AUTOSCALE_MODES`).
///
/// Autoscale is normalization-dependent (silx
/// `Colormap._computeAutoscaleRange` dispatches to the *normalizer*'s
/// `autoscale`, colors.py:682-692): [`AutoscaleMode::range`] therefore
/// requires the [`Normalization`], making a normalization-blind autoscale
/// unrepresentable. Use [`Colormap::autoscale_range`] to autoscale with a
/// colormap's own normalization and percentiles.
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

    /// Compute the `(vmin, vmax)` autoscale range over `data` for this mode
    /// under `normalization` — silx's normalizer `autoscale`
    /// (`_NormalizationMixIn.autoscale`, math/colormap.py:238-297), reached
    /// through `Colormap._computeAutoscaleRange` (colors.py:682-692). The
    /// normalization is a required parameter because every part of the
    /// computation depends on it:
    ///
    /// - **minmax**: valid-and-finite data min/max; under
    ///   [`Normalization::Log`] it is `(min_positive, finite max)` instead
    ///   (`LogarithmicNormalization.autoscale_minmax` →
    ///   `_min_max(min_positive=True, finite=True)`, math/colormap.py:421-424,
    ///   with **no** validity pre-filter — the max spans all finite values).
    /// - **stddev3**: `mean ± 3·std` intersected with the minmax bounds
    ///   (`vmin = max(dmin, stdmin)`, `vmax = min(dmax, stdmax)`, each falling
    ///   back to the other side when one is absent, math/colormap.py:266-283).
    ///   For [`Linear`](Normalization::Linear)/[`Gamma`](Normalization::Gamma)
    ///   the mean/std run in *data* space (`_LinearNormalizationMixIn`,
    ///   math/colormap.py:376-395); for log/sqrt/arcsinh they run in
    ///   *normalized* space — `transform` → mean/std over the finite
    ///   transformed values → `inverse_transform` back
    ///   (`_NormalizationMixIn.autoscale_mean3std`, math/colormap.py:313-340).
    /// - **percentile**: valid then finite filtering, then the `(low, high)`
    ///   `nanpercentile` pair (math/colormap.py:355-370); `percentiles` is
    ///   ignored by the other modes — pass [`DEFAULT_PERCENTILES`] for silx's
    ///   default.
    /// - **fallbacks**: empty data and `None`/non-finite bounds collapse
    ///   per-side to [`Normalization::default_autoscale_range`] (`(1, 10)`
    ///   under log), and an inverted range is clamped so `vmax >= vmin`
    ///   (math/colormap.py:290-297).
    ///
    /// The standard deviation is the population (ddof = 0) std, matching
    /// numpy `nanstd`.
    pub fn range(
        self,
        normalization: Normalization,
        data: &[f64],
        percentiles: (f64, f64),
    ) -> (f64, f64) {
        let default_range = normalization.default_autoscale_range();
        // silx autoscale head: no data at all -> DEFAULT_RANGE
        // (math/colormap.py:251-253).
        if data.is_empty() {
            return default_range;
        }

        let (raw_min, raw_max) = match self {
            AutoscaleMode::MinMax => autoscale_minmax(normalization, data),
            AutoscaleMode::Stddev3 => {
                let (dmin, dmax) = autoscale_minmax(normalization, data);
                let (stdmin, stdmax) = autoscale_mean3std(normalization, data);
                // silx: vmin = max(dmin, stdmin), vmax = min(dmax, stdmax),
                // each falling back to the other when one side is absent
                // (math/colormap.py:266-283).
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
                // silx autoscale_percentiles: is_valid filter, then strip
                // non-finite, then nanpercentile (math/colormap.py:355-370).
                let valid: Vec<f64> = data
                    .iter()
                    .copied()
                    .filter(|&v| v.is_finite() && normalization.is_valid_autoscale_value(v))
                    .collect();
                let lo = nanpercentile(&valid, percentiles.0);
                let hi = nanpercentile(&valid, percentiles.1);
                (lo, hi)
            }
        };

        // silx fallback handling (_NormalizationMixIn.autoscale tail,
        // math/colormap.py:290-297).
        let vmin = raw_min.filter(|v| v.is_finite()).unwrap_or(default_range.0);
        let mut vmax = raw_max.filter(|v| v.is_finite()).unwrap_or(default_range.1);
        if vmax < vmin {
            vmax = vmin;
        }
        (vmin, vmax)
    }
}

/// Normalization-aware autoscale min/max (silx `autoscale_minmax`), each side
/// `None` when absent.
///
/// [`Normalization::Log`] uses silx's override (math/colormap.py:421-424):
/// `_min_max(data, min_positive=True, finite=True)` → the smallest strictly
/// positive finite value and the largest finite value, with **no** validity
/// pre-filter (so the max spans all finite values, including non-positive
/// ones). Every other normalization uses the base implementation
/// (math/colormap.py:299-310): filter to valid (`is_valid`) finite values,
/// then plain min/max.
fn autoscale_minmax(normalization: Normalization, data: &[f64]) -> (Option<f64>, Option<f64>) {
    if normalization == Normalization::Log {
        let mut min_positive: Option<f64> = None;
        let mut max: Option<f64> = None;
        for &v in data {
            if !v.is_finite() {
                continue;
            }
            if v > 0.0 {
                min_positive = Some(min_positive.map_or(v, |m| m.min(v)));
            }
            max = Some(max.map_or(v, |m| m.max(v)));
        }
        return (min_positive, max);
    }
    let mut min: Option<f64> = None;
    let mut max: Option<f64> = None;
    for &v in data {
        if !v.is_finite() || !normalization.is_valid_autoscale_value(v) {
            continue;
        }
        min = Some(min.map_or(v, |m| m.min(v)));
        max = Some(max.map_or(v, |m| m.max(v)));
    }
    (min, max)
}

/// Normalization-aware `mean ± 3·std` (silx `autoscale_mean3std`), or
/// `(None, None)` when no finite sample survives.
///
/// [`Normalization::Linear`]/[`Normalization::Gamma`] compute in *data* space
/// (`_LinearNormalizationMixIn.autoscale_mean3std`, math/colormap.py:376-395).
/// Log/sqrt/arcsinh compute in *normalized* space: apply
/// [`Normalization::transform`], drop non-finite results (silx replaces them
/// with NaN before `nanmean`/`nanstd` — a non-positive value under log and a
/// negative one under sqrt transform to NaN/-inf and are excluded), take
/// `mean ± 3·std` there, and map back through
/// [`Normalization::inverse_transform`] (`_NormalizationMixIn
/// .autoscale_mean3std`, math/colormap.py:313-340). The std is the population
/// (ddof = 0) std, matching numpy `nanstd`.
fn autoscale_mean3std(normalization: Normalization, data: &[f64]) -> (Option<f64>, Option<f64>) {
    let data_space = matches!(normalization, Normalization::Linear | Normalization::Gamma);
    let samples: Vec<f64> = if data_space {
        data.iter().copied().filter(|v| v.is_finite()).collect()
    } else {
        data.iter()
            .map(|&v| normalization.transform(v))
            .filter(|t| t.is_finite())
            .collect()
    };
    if samples.is_empty() {
        return (None, None);
    }
    let n = samples.len() as f64;
    let mean = samples.iter().sum::<f64>() / n;
    let variance = samples.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n;
    let std = variance.sqrt();
    let (lo, hi) = (mean - 3.0 * std, mean + 3.0 * std);
    if data_space {
        (Some(lo), Some(hi))
    } else {
        (
            Some(normalization.inverse_transform(lo)),
            Some(normalization.inverse_transform(hi)),
        )
    }
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
    fn new_colormap_has_pinned_bounds() {
        // An explicit-range colormap does not autoscale (silx `Colormap(name,
        // vmin=.., vmax=..)` has both bounds set, `vmin/vmax is not None`).
        let cm = Colormap::new(ColormapName::Viridis, 2.0, 8.0);
        assert!(!cm.vmin_auto);
        assert!(!cm.vmax_auto);
        assert!(!cm.is_autoscale());
    }

    #[test]
    fn autoscale_colormap_has_both_bounds_auto() {
        // silx default `Colormap(name)` leaves both bounds None → autoscale.
        let cm = Colormap::autoscale(ColormapName::Gray);
        assert!(cm.vmin_auto);
        assert!(cm.vmax_auto);
        assert!(cm.is_autoscale());
    }

    #[test]
    fn resolved_fills_both_auto_bounds_from_data() {
        // Both bounds auto → both come from the data's min/max (silx
        // getColormapRange over MINMAX).
        let cm = Colormap::autoscale(ColormapName::Gray);
        let out = cm.resolved(AutoscaleMode::MinMax, &[3.0, 7.0, 5.0]);
        assert_eq!((out.vmin, out.vmax), (3.0, 7.0));
        // Flags are preserved: a later data change re-resolves.
        assert!(out.vmin_auto && out.vmax_auto);
    }

    #[test]
    fn resolved_keeps_a_pinned_bound_and_fills_the_auto_one() {
        // silx per-bound: vmax set, vmin None → (data_min, vmax_pinned).
        let mut cm = Colormap::autoscale(ColormapName::Gray);
        cm.vmax = 100.0;
        cm.vmax_auto = false; // pin the upper bound
        let out = cm.resolved(AutoscaleMode::MinMax, &[3.0, 7.0, 5.0]);
        assert_eq!(out.vmin, 3.0, "auto lower bound follows the data");
        assert_eq!(out.vmax, 100.0, "pinned upper bound is kept");
        assert!(out.vmin_auto && !out.vmax_auto);
    }

    #[test]
    fn resolved_is_a_noop_for_a_fully_pinned_colormap() {
        // Neither bound auto → data is ignored, range unchanged.
        let cm = Colormap::new(ColormapName::Viridis, 2.0, 8.0);
        let out = cm.resolved(AutoscaleMode::MinMax, &[100.0, 200.0]);
        assert_eq!((out.vmin, out.vmax), (2.0, 8.0));
    }

    #[test]
    fn resolved_clamps_auto_bound_against_a_pinned_wrong_side_bound() {
        // R3-6: silx `_getColormapRange`'s ordering clamp (colors.py:740-746).
        // Pin vmax=2 below the data [3,90] with vmin auto: the auto vmin is
        // clamped to the pinned vmax, giving the degenerate-but-ordered (2, 2),
        // not the inverted (3, 2) that violates `norm_bounds`' `vmax > vmin`
        // precondition and collapses the item to the low color.
        let mut cm = Colormap::autoscale(ColormapName::Gray);
        cm.vmax = 2.0;
        cm.vmax_auto = false;
        let out = cm.resolved(AutoscaleMode::MinMax, &[3.0, 90.0]);
        assert_eq!((out.vmin, out.vmax), (2.0, 2.0));
        assert!(out.vmin <= out.vmax, "resolved range must be ordered");

        // Symmetric: pin vmin=100 above the data, vmax auto → vmax2 = max(90,
        // 100) = 100, so (100, 100), not the inverted (100, 90).
        let mut cm = Colormap::autoscale(ColormapName::Gray);
        cm.vmin = 100.0;
        cm.vmin_auto = false;
        let out = cm.resolved(AutoscaleMode::MinMax, &[3.0, 90.0]);
        assert_eq!((out.vmin, out.vmax), (100.0, 100.0));
    }

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
            // Cyclic maps wrap, so their endpoints coincide by design; assert
            // distinct endpoints only for the non-cyclic maps.
            let cyclic = matches!(name, ColormapName::Rainbow | ColormapName::Sinebow);
            if !cyclic {
                assert_ne!(
                    cm.lut[0],
                    cm.lut[255],
                    "{} has equal endpoints",
                    name.label()
                );
            }
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

    // --- inverse_transform round-trips -----------------------------------

    #[test]
    fn inverse_transform_round_trips_each_normalization() {
        // inverse_transform(transform(v)) == v over each variant's domain.
        for norm in [Normalization::Linear, Normalization::Gamma] {
            for &v in &[-3.0, 0.0, 2.5, 100.0] {
                let back = norm.inverse_transform(norm.transform(v));
                assert!((back - v).abs() < 1e-9, "{norm:?} at v={v}: {back}");
            }
        }
        // Arcsinh is finite/monotonic for all reals (sinh(asinh(v)) == v).
        for &v in &[-50.0, -1.0, 0.0, 1.0, 50.0] {
            let n = Normalization::Arcsinh;
            let back = n.inverse_transform(n.transform(v));
            assert!((back - v).abs() < 1e-6, "arcsinh at v={v}: {back}");
        }
        // Log is defined for v > 0; sqrt for v >= 0.
        for &v in &[1e-3, 1.0, 10.0, 1000.0] {
            let n = Normalization::Log;
            let back = n.inverse_transform(n.transform(v));
            assert!((back - v).abs() < 1e-6 * v.max(1.0), "log at v={v}: {back}");
        }
        for &v in &[0.0, 0.25, 4.0, 81.0] {
            let n = Normalization::Sqrt;
            let back = n.inverse_transform(n.transform(v));
            assert!((back - v).abs() < 1e-9, "sqrt at v={v}: {back}");
        }
    }

    #[test]
    fn inverse_transform_maps_transformed_space_to_value() {
        // Spot-check the closed forms (not via round-trip).
        assert!((Normalization::Log.inverse_transform(2.0) - 100.0).abs() < 1e-9);
        assert!((Normalization::Sqrt.inverse_transform(3.0) - 9.0).abs() < 1e-9);
        assert!((Normalization::Arcsinh.inverse_transform(0.0)).abs() < 1e-12);
        assert_eq!(Normalization::Linear.inverse_transform(7.5), 7.5);
        assert_eq!(Normalization::Gamma.inverse_transform(7.5), 7.5);
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
    fn catalog_has_45_entries_with_unique_labels_and_usable_luts() {
        // The catalog ships silx's analytic maps (7) plus the matplotlib-
        // overlapping colorous gradients (38) = 45. Every entry must build a
        // usable, opaque, non-degenerate LUT, and labels must be unique so the
        // picker has no collisions.
        assert_eq!(ColormapName::ALL.len(), 45);
        let mut labels = std::collections::HashSet::new();
        for name in ColormapName::ALL {
            assert!(
                labels.insert(name.label()),
                "duplicate label {}",
                name.label()
            );
            let cm = Colormap::new(name, 0.0, 1.0);
            assert!(
                cm.lut.iter().all(|c| c[3] == 255),
                "{} not opaque",
                name.label()
            );
            // Cyclic maps (Rainbow/Sinebow) can share endpoints, so assert a
            // rich palette rather than distinct endpoints.
            let distinct = cm
                .lut
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len();
            assert!(
                distinct > 8,
                "{} has too few colors ({distinct})",
                name.label()
            );
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
            AutoscaleMode::MinMax.range(Normalization::Linear, &data, DEFAULT_PERCENTILES),
            (-1.0, 5.0)
        );
    }

    #[test]
    fn autoscale_stddev3_is_mean_plus_minus_3std_clamped_to_data() {
        // [0, 0, 0, 0, 10]: mean = 2, population std = 4, so mean±3·std =
        // [-10, 14]; clamped into the data range [0, 10] -> [0, 10].
        let data = [0.0, 0.0, 0.0, 0.0, 10.0];
        let (vmin, vmax) =
            AutoscaleMode::Stddev3.range(Normalization::Linear, &data, DEFAULT_PERCENTILES);
        assert!((vmin - 0.0).abs() < 1e-9, "vmin {vmin}");
        assert!((vmax - 10.0).abs() < 1e-9, "vmax {vmax}");

        // A tight cluster keeps mean±3·std inside the data range: data
        // [1, 2, 3, 4, 5] has mean 3, std sqrt(2); mean±3·std = 3 ± 3·sqrt(2)
        // = [-1.2426, 7.2426] -> clamped to data range [1, 5].
        let data2 = [1.0, 2.0, 3.0, 4.0, 5.0];
        let (vmin2, vmax2) =
            AutoscaleMode::Stddev3.range(Normalization::Linear, &data2, DEFAULT_PERCENTILES);
        assert!((vmin2 - 1.0).abs() < 1e-9, "vmin2 {vmin2}");
        assert!((vmax2 - 5.0).abs() < 1e-9, "vmax2 {vmax2}");
    }

    #[test]
    fn autoscale_percentile_default_1_99_bounds() {
        // 0..=100 (101 samples). numpy linear interpolation:
        // rank(1%)  = 0.01 * 100 = 1.0  -> data[1]  = 1.0
        // rank(99%) = 0.99 * 100 = 99.0 -> data[99] = 99.0
        let data: Vec<f64> = (0..=100).map(|i| i as f64).collect();
        let (vmin, vmax) =
            AutoscaleMode::Percentile.range(Normalization::Linear, &data, DEFAULT_PERCENTILES);
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
            AutoscaleMode::MinMax.range(Normalization::Linear, &data, DEFAULT_PERCENTILES),
            (2.0, 4.0)
        );
        // No finite samples -> silx DEFAULT_RANGE (0, 1).
        let empty: [f64; 0] = [];
        assert_eq!(
            AutoscaleMode::MinMax.range(Normalization::Linear, &empty, DEFAULT_PERCENTILES),
            (0.0, 1.0)
        );
        let all_nan = [f64::NAN, f64::NAN];
        assert_eq!(
            AutoscaleMode::Stddev3.range(Normalization::Linear, &all_nan, DEFAULT_PERCENTILES),
            (0.0, 1.0)
        );
    }

    #[test]
    fn log_autoscale_minmax_is_min_positive_to_finite_max() {
        // silx LogarithmicNormalization.autoscale_minmax
        // (math/colormap.py:421-424): (min_positive, finite max) — counting
        // data with zeros/negatives must NOT collapse vmin to <= 0.
        let data = [-5.0, 0.0, 0.1, 100.0, f64::NAN];
        assert_eq!(
            AutoscaleMode::MinMax.range(Normalization::Log, &data, DEFAULT_PERCENTILES),
            (0.1, 100.0)
        );
        // The same data under Linear keeps the raw min.
        assert_eq!(
            AutoscaleMode::MinMax.range(Normalization::Linear, &data, DEFAULT_PERCENTILES),
            (-5.0, 100.0)
        );
    }

    #[test]
    fn log_autoscaled_colormap_does_not_collapse_norm_bounds() {
        // End to end through Colormap::autoscale_range (silx
        // _computeAutoscaleRange, colors.py:682-692): a log colormap
        // autoscaled over data containing zeros gets vmin = min positive, so
        // norm_bounds() stays usable instead of the (0, 0) low-color collapse.
        let mut cm = Colormap::viridis(0.0, 1.0);
        cm.normalization = Normalization::Log;
        let data = [0.0, 0.0, 0.5, 20.0];
        let (vmin, vmax) = cm.autoscale_range(AutoscaleMode::MinMax, &data);
        assert_eq!((vmin, vmax), (0.5, 20.0));
        cm.vmin = vmin;
        cm.vmax = vmax;
        assert_ne!(cm.norm_bounds(), (0.0, 0.0), "log bounds must not collapse");
    }

    #[test]
    fn log_autoscale_fallbacks_use_the_1_10_default_range() {
        // Empty data -> the log DEFAULT_RANGE (1, 10)
        // (LogarithmicNormalization.DEFAULT_RANGE, math/colormap.py:410).
        let empty: [f64; 0] = [];
        assert_eq!(
            AutoscaleMode::MinMax.range(Normalization::Log, &empty, DEFAULT_PERCENTILES),
            (1.0, 10.0)
        );
        let all_nan = [f64::NAN, f64::NAN];
        assert_eq!(
            AutoscaleMode::MinMax.range(Normalization::Log, &all_nan, DEFAULT_PERCENTILES),
            (1.0, 10.0)
        );
        // No positive value: min_positive is None -> vmin falls back to 1;
        // the finite max (-1) is below it, so vmax clamps to vmin
        // (math/colormap.py:290-297).
        let negative = [-5.0, -1.0];
        assert_eq!(
            AutoscaleMode::MinMax.range(Normalization::Log, &negative, DEFAULT_PERCENTILES),
            (1.0, 1.0)
        );
    }

    #[test]
    fn log_percentile_filters_to_positive_values() {
        // silx autoscale_percentiles applies is_valid (v > 0 for log) before
        // the percentile (math/colormap.py:355-370): with (0, 100) the range
        // spans only the positive values.
        let data = [-100.0, 1.0, 2.0, 3.0, 4.0];
        assert_eq!(
            AutoscaleMode::Percentile.range(Normalization::Log, &data, (0.0, 100.0)),
            (1.0, 4.0)
        );
        assert_eq!(
            AutoscaleMode::Percentile.range(Normalization::Linear, &data, (0.0, 100.0)),
            (-100.0, 4.0)
        );
    }

    #[test]
    fn log_stddev3_runs_in_normalized_space() {
        // silx _NormalizationMixIn.autoscale_mean3std for non-linear
        // normalizations (math/colormap.py:313-340): mean ± 3·std over
        // log10(data) (non-finite transforms dropped), reverted through 10^x,
        // then intersected with minmax. Twenty 1.0 samples and one 1e6:
        // transformed = twenty 0.0 and one 6.0.
        let mut data = vec![1.0; 20];
        data.push(1e6);
        let n = 21.0f64;
        let mean = 6.0 / n;
        let variance: f64 = (20.0 * mean * mean + (6.0 - mean) * (6.0 - mean)) / n;
        let hi = 10f64.powf(mean + 3.0 * variance.sqrt()); // ~1.3e4 < dmax 1e6
        let (vmin, vmax) =
            AutoscaleMode::Stddev3.range(Normalization::Log, &data, DEFAULT_PERCENTILES);
        // vmin: revert(mean - 3·std) ~ 2.8e-4 is below min_positive = 1, so
        // the minmax intersection keeps 1 (vmin = max(dmin, stdmin)).
        assert!((vmin - 1.0).abs() < 1e-12, "vmin {vmin}");
        assert!((vmax - hi).abs() / hi < 1e-12, "vmax {vmax} != {hi}");
        // Data-space stddev3 (the old behavior) is wildly different here:
        // mean_d = 1e6/21 + 20/21, std_d from the same two-point split ->
        // vmax = mean_d + 3·std_d ~ 6.9e5, ~50x the normalized-space bound.
        let mean_d = (20.0 * 1.0 + 1e6) / n;
        let var_d: f64 =
            (20.0 * (1.0 - mean_d) * (1.0 - mean_d) + (1e6 - mean_d) * (1e6 - mean_d)) / n;
        let hi_d = mean_d + 3.0 * var_d.sqrt();
        let (_, linear_vmax) =
            AutoscaleMode::Stddev3.range(Normalization::Linear, &data, DEFAULT_PERCENTILES);
        assert!(
            (linear_vmax - hi_d).abs() / hi_d < 1e-12,
            "linear vmax {linear_vmax} != {hi_d}"
        );
        assert!(linear_vmax > 50.0 * hi, "spaces must differ materially");
    }

    #[test]
    fn sqrt_autoscale_filters_negative_values() {
        // silx SqrtNormalization.is_valid = v >= 0 (math/colormap.py:434-436):
        // minmax and percentile exclude negatives; zero stays valid.
        let data = [-4.0, 0.0, 1.0, 4.0];
        assert_eq!(
            AutoscaleMode::MinMax.range(Normalization::Sqrt, &data, DEFAULT_PERCENTILES),
            (0.0, 4.0)
        );
        assert_eq!(
            AutoscaleMode::Percentile.range(Normalization::Sqrt, &data, (0.0, 100.0)),
            (0.0, 4.0)
        );
    }

    #[test]
    fn default_autoscale_range_and_validity_per_normalization() {
        assert_eq!(Normalization::Log.default_autoscale_range(), (1.0, 10.0));
        for norm in [
            Normalization::Linear,
            Normalization::Sqrt,
            Normalization::Gamma,
            Normalization::Arcsinh,
        ] {
            assert_eq!(norm.default_autoscale_range(), (0.0, 1.0), "{norm:?}");
        }
        assert!(!Normalization::Log.is_valid_autoscale_value(0.0));
        assert!(Normalization::Log.is_valid_autoscale_value(0.5));
        assert!(Normalization::Sqrt.is_valid_autoscale_value(0.0));
        assert!(!Normalization::Sqrt.is_valid_autoscale_value(-0.5));
        assert!(Normalization::Arcsinh.is_valid_autoscale_value(-0.5));
        assert!(Normalization::Linear.is_valid_autoscale_value(-0.5));
    }

    #[test]
    fn color_at_looks_up_lut_and_uses_nan_color_only_for_nan() {
        let cmap = Colormap::new(ColormapName::Viridis, 0.0, 4.0).with_nan_color([1, 2, 3, 4]);
        // Endpoints hit the first/last LUT entries; the midpoint the middle.
        assert_eq!(cmap.color_at(0.0), cmap.lut[0]);
        assert_eq!(cmap.color_at(4.0), cmap.lut[255]);
        // Midpoint: normalize(2.0)=0.5 → int(0.5*256)=128 (R2-42 silx binning).
        assert_eq!(cmap.color_at(2.0), cmap.lut[128]);
        // Out-of-range values clamp to the endpoints (normalize clamps to [0, 1]).
        assert_eq!(cmap.color_at(-10.0), cmap.lut[0]);
        assert_eq!(cmap.color_at(10.0), cmap.lut[255]);
        // R2-40: only NaN takes the NaN color. ±inf clamp into the LUT ends
        // (+inf → top, -inf → bottom), matching silx GLPlotImage / _colormap.pyx.
        assert_eq!(cmap.color_at(f64::NAN), [1, 2, 3, 4]);
        assert_eq!(cmap.color_at(f64::INFINITY), cmap.lut[255]);
        assert_eq!(cmap.color_at(f64::NEG_INFINITY), cmap.lut[0]);
    }

    #[test]
    fn lut_index_uses_silx_256_binning_capped_at_255() {
        // silx _colormap.pyx: int(ratio * nb_colors) capped at nb_colors-1,
        // nb_colors=256. Over [0, 4]: ratio 0 → 0, 0.5 → int(128)=128 (NOT the
        // old ×255's 127), 1.0 → int(256)=256 capped to 255.
        let cmap = Colormap::new(ColormapName::Viridis, 0.0, 4.0);
        assert_eq!(cmap.lut_index(0.0), 0);
        assert_eq!(cmap.lut_index(2.0), 128);
        assert_eq!(cmap.lut_index(4.0), 255);
        // Out-of-range clamps: below → 0, above → 255 (normalize clamps ratio).
        assert_eq!(cmap.lut_index(-1.0), 0);
        assert_eq!(cmap.lut_index(10.0), 255);
        // A value just under vmax stays within [0, 255].
        assert!(cmap.lut_index(3.999) <= 255);
    }

    #[test]
    fn color_at_degenerate_range_maps_infinity_to_low_color() {
        // A degenerate range (vmin == vmax) makes norm_bounds return one_over_range
        // 0; a +inf sample then hits the low LUT entry (not nan_color), matching
        // silx's degenerate-range fallback (everything → low color).
        let cmap = Colormap::new(ColormapName::Viridis, 2.0, 2.0).with_nan_color([1, 2, 3, 4]);
        assert_eq!(cmap.color_at(f64::INFINITY), cmap.lut[0]);
        assert_eq!(cmap.color_at(f64::NEG_INFINITY), cmap.lut[0]);
        // NaN still uses the nan color.
        assert_eq!(cmap.color_at(f64::NAN), [1, 2, 3, 4]);
    }

    #[test]
    fn autoscaled_replaces_range_and_preserves_lut() {
        // `autoscaled` re-derives vmin/vmax from data (minmax here) while
        // keeping the LUT, normalization, gamma, and nan color — the shared
        // primitive for silx's autoscale-to-item-data contract.
        let base = Colormap::new(ColormapName::Gray, 0.0, 1.0).with_nan_color([9, 8, 7, 6]);
        let cm = base.autoscaled(AutoscaleMode::MinMax, &[10.0, 20.0, 30.0]);
        assert_eq!((cm.vmin, cm.vmax), (10.0, 30.0));
        assert_eq!(cm.lut, base.lut);
        assert_eq!(cm.nan_color, [9, 8, 7, 6]);
        assert_eq!(cm.normalization, base.normalization);
    }

    #[test]
    fn autoscaled_honors_normalization_for_the_range() {
        // Under log normalization, minmax autoscale uses the smallest strictly
        // positive value as vmin (silx LogarithmicNormalization.autoscale),
        // proving the range derivation is normalization-aware, not plain
        // min/max.
        let base = Colormap::viridis(1.0, 10.0).with_normalization(Normalization::Log);
        let cm = base.autoscaled(AutoscaleMode::MinMax, &[-5.0, 0.0, 2.0, 100.0]);
        assert_eq!((cm.vmin, cm.vmax), (2.0, 100.0));
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

    #[test]
    fn register_then_resolve_round_trips_the_lut() {
        // Distinct name keeps the process-global registry test-isolated.
        let colors = [[0, 0, 0, 255], [255, 255, 255, 255]];
        assert!(register_colormap("test-reg-roundtrip", &colors, None));

        let cm = Colormap::from_registered("test-reg-roundtrip", 0.0, 1.0)
            .expect("registered name resolves");
        // The resolved LUT is exactly the resampled input.
        assert_eq!(cm.lut, resample_lut(&colors).unwrap());
        // Resolved colormaps default to a linear range with the default gamma.
        assert_eq!((cm.vmin, cm.vmax), (0.0, 1.0));
        assert_eq!(cm.normalization, Normalization::Linear);
        assert_eq!(cm.gamma, DEFAULT_GAMMA);
        assert!(cm.editable);
    }

    #[test]
    fn register_empty_colors_registers_nothing() {
        assert!(!register_colormap("test-reg-empty", &[], None));
        assert!(Colormap::from_registered("test-reg-empty", 0.0, 1.0).is_none());
    }

    #[test]
    fn register_overrides_existing_name() {
        assert!(register_colormap(
            "test-reg-override",
            &[[10, 20, 30, 255]],
            None
        ));
        // Re-registering the same name wins (silx allows overriding).
        assert!(register_colormap(
            "test-reg-override",
            &[[200, 100, 50, 255]],
            None
        ));
        let cm = Colormap::from_registered("test-reg-override", 0.0, 1.0).unwrap();
        assert_eq!(cm.lut[0], [200, 100, 50, 255]);
    }

    #[test]
    fn registered_colormaps_lists_names_sorted() {
        register_colormap("test-reg-list-b", &[[1, 1, 1, 255]], None);
        register_colormap("test-reg-list-a", &[[2, 2, 2, 255]], None);
        let names = registered_colormaps();
        let a = names.iter().position(|n| n == "test-reg-list-a");
        let b = names.iter().position(|n| n == "test-reg-list-b");
        assert!(a.is_some() && b.is_some());
        assert!(a < b, "BTreeMap keeps names sorted: a before b");
    }

    #[test]
    fn cursor_color_defaults_to_black_or_uses_given() {
        register_colormap("test-reg-cursor-default", &[[0, 0, 0, 255]], None);
        assert_eq!(
            registered_colormap_cursor_color("test-reg-cursor-default"),
            Some(DEFAULT_CURSOR_COLOR)
        );
        register_colormap(
            "test-reg-cursor-set",
            &[[0, 0, 0, 255]],
            Some([9, 8, 7, 255]),
        );
        assert_eq!(
            registered_colormap_cursor_color("test-reg-cursor-set"),
            Some([9, 8, 7, 255])
        );
        assert_eq!(registered_colormap_cursor_color("test-reg-not-there"), None);
    }

    #[test]
    fn cursor_color_matches_the_silx_builtin_table() {
        // silx `_AVAILABLE_LUTS` (math/colormap.py:52-66): pink for the
        // light-tone builtins, green for red/magma/inferno/plasma, yellow for
        // blue — and BLACK for every matplotlib-loaded name (colors.py:244).
        assert_eq!(ColormapName::Gray.cursor_color(), [255, 102, 255, 255]);
        assert_eq!(ColormapName::Viridis.cursor_color(), [255, 102, 255, 255]);
        assert_eq!(
            ColormapName::Temperature.cursor_color(),
            [255, 102, 255, 255]
        );
        assert_eq!(ColormapName::Red.cursor_color(), [0, 255, 0, 255]);
        assert_eq!(ColormapName::Inferno.cursor_color(), [0, 255, 0, 255]);
        assert_eq!(ColormapName::Blue.cursor_color(), [255, 255, 0, 255]);
        assert_eq!(ColormapName::Jet.cursor_color(), DEFAULT_CURSOR_COLOR);
        assert_eq!(ColormapName::Turbo.cursor_color(), DEFAULT_CURSOR_COLOR);
    }

    #[test]
    fn colormap_carries_cursor_color_and_a_raw_lut_resets_it() {
        // A named colormap carries its silx cursor color…
        let cm = Colormap::new(ColormapName::Gray, 0.0, 1.0);
        assert_eq!(cm.cursor_color, [255, 102, 255, 255]);
        // …a raw LUT clears the name in silx, so the color falls to black
        // (setColormapLUT → name=None → "black", math/colormap.py:185-196).
        let lut = cm.lut;
        assert_eq!(cm.with_lut(lut).cursor_color, DEFAULT_CURSOR_COLOR);
        let mut cm = Colormap::new(ColormapName::Inferno, 0.0, 1.0);
        assert!(cm.set_lut(lut));
        assert_eq!(cm.cursor_color, DEFAULT_CURSOR_COLOR);
        // from_colors is a raw LUT from the start.
        let cm = Colormap::from_colors(&[[1, 2, 3, 255]], 0.0, 1.0).expect("non-empty");
        assert_eq!(cm.cursor_color, DEFAULT_CURSOR_COLOR);
        // A registered LUT resolves the color registered alongside it.
        register_colormap(
            "test-cmap-cursor-carry",
            &[[0, 0, 0, 255]],
            Some([10, 20, 30, 255]),
        );
        let cm = Colormap::from_registered("test-cmap-cursor-carry", 0.0, 1.0).expect("registered");
        assert_eq!(cm.cursor_color, [10, 20, 30, 255]);
    }
}
