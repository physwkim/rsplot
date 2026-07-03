//! LabelledAxes chrome — the headless math of silx `plot3d.scene.axes.LabelledAxes`
//! (axis name labels, dashed tick lines, tick value labels around the bounding
//! box) plus the tick layout it uses from `silx.gui.plot._utils.ticklayout`.
//!
//! The tick layout is a **fresh port** of `ticklayout.py` (`numberOfDigits`,
//! `niceNumGeneric`, `niceNumbers`, `ticks`): `crate::core::colorbar` holds a
//! private copy of the same nice-numbers algorithm, but that file is owned by
//! the 2D plot side, so the duplication here is deliberate.
//!
//! Everything in this module is pure geometry/label math, unit-tested headless;
//! the [`crate::SceneWidget`] draws the segments through the GPU line channel
//! and the labels as egui overlay text.

use crate::core::scene3d::mat4::Vec3;

/// Number of fractional digits for tick labels of `tick_spacing` — port of
/// `ticklayout.numberOfDigits` (`ticklayout.py:36-46`).
fn number_of_digits(tick_spacing: f64) -> usize {
    let nfrac = -tick_spacing.log10().floor();
    if nfrac < 0.0 { 0 } else { nfrac as usize }
}

/// Port of `ticklayout.niceNumGeneric` with the default fractions
/// (`ticklayout.py:78-110`): rounds `value` to a "nice" 1/2/5/10 × 10ᵏ.
fn nice_num(value: f64, is_round: bool) -> f64 {
    if value == 0.0 {
        return value;
    }
    const NICE_FRACTIONS: [f64; 4] = [1.0, 2.0, 5.0, 10.0];
    let round_fractions: [f64; 4] = if is_round {
        [1.5, 3.0, 7.0, 10.0]
    } else {
        NICE_FRACTIONS
    };
    // Python `math.log(value, 10)` is ln(value)/ln(10); mirror it exactly.
    let expvalue = (value.ln() / std::f64::consts::LN_10).floor();
    let frac = value / 10f64.powf(expvalue);
    for (nice, round) in NICE_FRACTIONS.into_iter().zip(round_fractions) {
        if frac <= round {
            return nice * 10f64.powf(expvalue);
        }
    }
    // Unreachable: frac ≤ 10 always (silx asserts the same).
    10f64.powf(expvalue + 1.0)
}

/// Port of `ticklayout.niceNumbers` (`ticklayout.py:112-132`, Heckbert's nice
/// numbers): returns `(graph_min, graph_max, spacing, n_frac)`.
fn nice_numbers(vmin: f64, vmax: f64, n_ticks: usize) -> (f64, f64, f64, usize) {
    let vrange = nice_num(vmax - vmin, false);
    let spacing = nice_num(vrange / n_ticks as f64, true);
    let graph_min = (vmin / spacing).floor() * spacing;
    let graph_max = (vmax / spacing).ceil() * spacing;
    let nfrac = number_of_digits(spacing);
    (graph_min, graph_max, spacing, nfrac)
}

/// One tick label. Python formats with `%g` when `nfrac == 0` and `%.{nfrac}f`
/// otherwise; Rust's plain `{}` matches `%g` for these values except that very
/// large/small magnitudes stay in decimal notation instead of switching to
/// exponent form (documented simplification).
fn format_tick(value: f64, nfrac: usize) -> String {
    if nfrac == 0 {
        format!("{value}")
    } else {
        format!("{value:.nfrac$}")
    }
}

/// Tick positions and labels for a `[vmin, vmax]` axis — port of
/// `ticklayout.ticks` (`ticklayout.py:142-172`, `nbTicks=5`). Ticks are
/// clamped into `[vmin, vmax]`; at least one tick is returned (a single one
/// when `vmin == vmax`), and fewer than two surviving ticks fall back to the
/// range ends. The float accumulation of Python's `_frange` is reproduced so
/// tick sets match silx bit-for-bit.
pub fn ticks(vmin: f64, vmax: f64) -> (Vec<f64>, Vec<String>) {
    debug_assert!(vmin <= vmax);
    let (mut positions, nfrac);
    if vmin == vmax {
        positions = vec![vmin];
        nfrac = 0;
    } else {
        let (start, end, step, n) = nice_numbers(vmin, vmax, 5);
        // Python `_frange(start, stop, step)`: accumulate, inclusive stop.
        let mut t = start;
        positions = Vec::new();
        while t <= end {
            if t >= vmin && t <= vmax {
                positions.push(t);
            }
            t += step;
        }
        if positions.len() < 2 {
            positions = vec![vmin, vmax];
            nfrac = number_of_digits(vmax - vmin);
        } else {
            nfrac = n;
        }
    }
    let labels = positions.iter().map(|&t| format_tick(t, nfrac)).collect();
    (positions, labels)
}

/// One billboard text label of the axes chrome: a scene-space anchor and its
/// string. silx draws these as `Text2D` (align/valign center); the widget
/// draws them as egui overlay text at the projected anchor.
#[derive(Clone, Debug, PartialEq)]
pub struct AxisLabel {
    /// Scene-space anchor of the label centre.
    pub position: Vec3,
    /// Label text.
    pub text: String,
}

/// The headless `LabelledAxes` chrome for a bounding box: axis name labels,
/// tick line segments (to be dashed by the caller), and tick value labels.
#[derive(Clone, Debug, Default)]
pub struct AxesChrome {
    /// Axis name labels at the box face-edge midpoints
    /// (`scene/axes.py:57-67`: `Translate(bounds.min) · Translate(size/2)` on
    /// one axis each). Axes with an empty name are omitted (the silx `Text2D`
    /// default text is empty).
    pub axis_labels: Vec<AxisLabel>,
    /// Solid tick segments on the box planes (`scene/axes.py:190-211`): per
    /// tick, one segment across each of the two box planes containing the
    /// axis. Drawn dashed (silx `DashedLines`, dash 5,10 — `axes.py:71-73`).
    pub tick_segments: Vec<(Vec3, Vec3)>,
    /// One value label per tick, anchored at `bounds.min − extent/20` on the
    /// two other axes (`scene/axes.py:216-245`).
    pub tick_labels: Vec<AxisLabel>,
}

/// Build the `LabelledAxes` chrome for `bounds` — the pure-math half of silx
/// `LabelledAxes._updateTicks` (`scene/axes.py:172-247`) plus the three axis
/// name anchors (`axes.py:57-67`, updated in `_updateBoxAndAxes` :88-101).
pub fn labelled_axes_chrome(bounds: (Vec3, Vec3), axis_names: [&str; 3]) -> AxesChrome {
    let (lo, hi) = bounds;
    let size = hi - lo;

    // Axis name labels: box origin + half the size along the named axis.
    let anchors = [
        Vec3::new(lo.x + size.x * 0.5, lo.y, lo.z),
        Vec3::new(lo.x, lo.y + size.y * 0.5, lo.z),
        Vec3::new(lo.x, lo.y, lo.z + size.z * 0.5),
    ];
    let axis_labels = anchors
        .into_iter()
        .zip(axis_names)
        .filter(|(_, name)| !name.is_empty())
        .map(|(position, name)| AxisLabel {
            position,
            text: name.to_string(),
        })
        .collect();

    // ticklength = |bounds[1] - bounds[0]| (axes.py:186).
    let len = Vec3::new(size.x.abs(), size.y.abs(), size.z.abs());
    let (xticks, xlabels) = ticks(lo.x as f64, hi.x as f64);
    let (yticks, ylabels) = ticks(lo.y as f64, hi.y as f64);
    let (zticks, zlabels) = ticks(lo.z as f64, hi.z as f64);

    // Tick lines: 4 points per tick starting at bounds.min — point 1 extends
    // along one plane, point 3 along the other (axes.py:190-211); as a line
    // *set* that is two segments (0→1, 2→3) sharing the base point.
    let mut tick_segments = Vec::new();
    for &t in &xticks {
        let base = Vec3::new(t as f32, lo.y, lo.z);
        tick_segments.push((base, Vec3::new(t as f32, lo.y + len.y, lo.z))); // XY plane
        tick_segments.push((base, Vec3::new(t as f32, lo.y, lo.z + len.z))); // XZ plane
    }
    for &t in &yticks {
        let base = Vec3::new(lo.x, t as f32, lo.z);
        tick_segments.push((base, Vec3::new(lo.x + len.x, t as f32, lo.z))); // XY plane
        tick_segments.push((base, Vec3::new(lo.x, t as f32, lo.z + len.z))); // YZ plane
    }
    for &t in &zticks {
        let base = Vec3::new(lo.x, lo.y, t as f32);
        tick_segments.push((base, Vec3::new(lo.x + len.x, lo.y, t as f32))); // XZ plane
        tick_segments.push((base, Vec3::new(lo.x, lo.y + len.y, t as f32))); // YZ plane
    }

    // Tick value labels: offsets = bounds.min − ticklength/20 (axes.py:217).
    let off = lo - len * (1.0 / 20.0);
    let mut tick_labels = Vec::new();
    for (&t, label) in xticks.iter().zip(xlabels) {
        tick_labels.push(AxisLabel {
            position: Vec3::new(t as f32, off.y, off.z),
            text: label,
        });
    }
    for (&t, label) in yticks.iter().zip(ylabels) {
        tick_labels.push(AxisLabel {
            position: Vec3::new(off.x, t as f32, off.z),
            text: label,
        });
    }
    for (&t, label) in zticks.iter().zip(zlabels) {
        tick_labels.push(AxisLabel {
            position: Vec3::new(off.x, off.y, t as f32),
            text: label,
        });
    }

    AxesChrome {
        axis_labels,
        tick_segments,
        tick_labels,
    }
}

/// Split the segment `(a, b)` into its on-dash pieces with world-space dash
/// lengths `on`/`off`. CPU stand-in for silx `DashedLines`' screen-space
/// fragment dash (`scene/primitives.py:589-593`: a fragment survives while
/// `mod(dist, on + off) ≤ on`, measured in pixels from the segment origin);
/// here the dash period is fixed in world units chosen by the caller —
/// documented deviation, the dash no longer has a constant on-screen size.
pub fn dash_segments(a: Vec3, b: Vec3, on: f32, off: f32) -> Vec<(Vec3, Vec3)> {
    let dir = b - a;
    let length = dir.length();
    if length == 0.0 || on <= 0.0 {
        return Vec::new();
    }
    if off <= 0.0 {
        return vec![(a, b)];
    }
    let period = on + off;
    let mut out = Vec::new();
    let mut s = 0.0f32;
    while s < length {
        let e = (s + on).min(length);
        out.push((a + dir * (s / length), a + dir * (e / length)));
        s += period;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_match_silx_nice_numbers() {
        // ticks(0, 10): vrange = 10, spacing = niceNum(2, round) = 2 →
        // 0, 2, …, 10 with %g labels (silx ticklayout doctest behaviour).
        let (pos, labels) = ticks(0.0, 10.0);
        assert_eq!(pos, vec![0.0, 2.0, 4.0, 6.0, 8.0, 10.0]);
        assert_eq!(labels, vec!["0", "2", "4", "6", "8", "10"]);

        // ticks(0.5, 10.5): grid 0..12 step 2, clamped into [0.5, 10.5].
        let (pos, labels) = ticks(0.5, 10.5);
        assert_eq!(pos, vec![2.0, 4.0, 6.0, 8.0, 10.0]);
        assert_eq!(labels, vec!["2", "4", "6", "8", "10"]);

        // ticks(0, 1): spacing 0.2 → one fractional digit. Python's _frange
        // accumulates floats (0.6 lands at 0.6000000000000001) — the port
        // reproduces the accumulation, and the labels, exactly.
        let (pos, labels) = ticks(0.0, 1.0);
        assert_eq!(labels, vec!["0.0", "0.2", "0.4", "0.6", "0.8", "1.0"]);
        assert_eq!(pos[3], 0.6000000000000001);

        // vMin == vMax → a single tick (ticklayout.py:153-155).
        let (pos, labels) = ticks(2.5, 2.5);
        assert_eq!(pos, vec![2.5]);
        assert_eq!(labels, vec!["2.5"]);
    }

    #[test]
    fn chrome_places_names_ticks_and_value_labels_like_silx() {
        let bounds = (Vec3::ZERO, Vec3::new(2.0, 4.0, 6.0));
        let chrome = labelled_axes_chrome(bounds, ["X", "", "Depth"]);

        // Axis names at origin + size/2 on their own axis; empty Y omitted.
        assert_eq!(chrome.axis_labels.len(), 2);
        assert_eq!(chrome.axis_labels[0].position, Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(chrome.axis_labels[0].text, "X");
        assert_eq!(chrome.axis_labels[1].position, Vec3::new(0.0, 0.0, 3.0));
        assert_eq!(chrome.axis_labels[1].text, "Depth");

        // Two segments per tick, and a label per tick.
        let n_ticks = ticks(0.0, 2.0).0.len() + ticks(0.0, 4.0).0.len() + ticks(0.0, 6.0).0.len();
        assert_eq!(chrome.tick_segments.len(), 2 * n_ticks);
        assert_eq!(chrome.tick_labels.len(), n_ticks);

        // First x tick (t = 0): base at the origin, one segment spanning the
        // XY plane (y + 4), one the XZ plane (z + 6) — axes.py:195-198.
        assert_eq!(
            chrome.tick_segments[0],
            (Vec3::ZERO, Vec3::new(0.0, 4.0, 0.0))
        );
        assert_eq!(
            chrome.tick_segments[1],
            (Vec3::ZERO, Vec3::new(0.0, 0.0, 6.0))
        );

        // X tick labels anchor at (t, −len.y/20, −len.z/20) — axes.py:217-227.
        // ticks(0, 2) uses spacing 0.5 → "%.1f" labels, so "0.0" (matches
        // silx ticklayout.ticks(0, 2) exactly).
        assert_eq!(chrome.tick_labels[0].position, Vec3::new(0.0, -0.2, -0.3));
        assert_eq!(chrome.tick_labels[0].text, "0.0");

        // A y tick segment runs along +x on the XY plane: find t = 4.
        assert!(
            chrome
                .tick_segments
                .iter()
                .any(|&(a, b)| a == Vec3::new(0.0, 4.0, 0.0) && b == Vec3::new(2.0, 4.0, 0.0)),
            "y tick at 4 must span the XY plane"
        );
    }

    #[test]
    fn dash_segments_cover_on_lengths_only() {
        // Length 3 with on = 1, off = 0.5 → dashes [0,1] and [1.5,2.5]; the
        // third period would start exactly at the segment end and is dropped.
        let out = dash_segments(Vec3::ZERO, Vec3::new(3.0, 0.0, 0.0), 1.0, 0.5);
        assert_eq!(out.len(), 2);
        assert!((out[0].0.x - 0.0).abs() < 1e-6 && (out[0].1.x - 1.0).abs() < 1e-6);
        assert!((out[1].0.x - 1.5).abs() < 1e-6 && (out[1].1.x - 2.5).abs() < 1e-6);

        // A degenerate segment or non-positive on-length draws nothing.
        assert!(dash_segments(Vec3::ZERO, Vec3::ZERO, 1.0, 1.0).is_empty());
        assert!(dash_segments(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), 0.0, 1.0).is_empty());

        // No gap → the whole segment.
        let solid = dash_segments(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), 5.0, 0.0);
        assert_eq!(solid, vec![(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0))]);
    }
}
