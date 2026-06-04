//! Small helpers for the egui [`Color32`] type, shared by the widget and render
//! layers.

use egui::Color32;

/// Replace `color`'s alpha with `a`, preserving its *straight* (un-premultiplied)
/// RGB.
///
/// [`Color32`] stores premultiplied RGBA: its `r()`/`g()`/`b()` accessors return
/// channels already multiplied by alpha. Reading those and rebuilding through
/// [`Color32::from_rgba_unmultiplied`] (which premultiplies again) would
/// *double-premultiply* the RGB whenever the source alpha is below 255 — the RGB
/// is darkened toward black. Reading the straight RGB via
/// [`Color32::to_srgba_unmultiplied`] keeps the rebuild exact (silx works on
/// straight RGBA throughout; cf. `compose_per_point_alpha` in the high-level
/// widget). For a fully opaque source the result is identical to a premultiplied
/// read, so existing opaque-color call sites are unchanged.
pub(crate) fn with_alpha(color: Color32, a: u8) -> Color32 {
    let [r, g, b, _] = color.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// Scale `color`'s straight alpha by `factor` (clamped to `[0, 1]`), preserving
/// its straight RGB.
///
/// Equivalent to [`with_alpha`] with `round(alpha · factor)`. The alpha channel
/// is unaffected by premultiplication, so scaling it directly is correct; the
/// RGB is taken straight to avoid the double-premultiply described on
/// [`with_alpha`].
pub(crate) fn scale_alpha(color: Color32, factor: f32) -> Color32 {
    let factor = factor.clamp(0.0, 1.0);
    let a = ((color.a() as f32) * factor).round() as u8;
    with_alpha(color, a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_alpha_uses_straight_rgb_for_opaque_source() {
        // Opaque source: straight RGB == premultiplied RGB (lossless), so the
        // result equals a straight-RGB rebuild at the new alpha exactly.
        let src = Color32::from_rgb(200, 100, 50);
        assert_eq!(
            with_alpha(src, 28),
            Color32::from_rgba_unmultiplied(200, 100, 50, 28)
        );
        assert_eq!(with_alpha(src, 28).a(), 28);
    }

    #[test]
    fn with_alpha_does_not_double_premultiply_translucent_source() {
        // Translucent source: the old premultiplied-accessor read
        // (`from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)`) darkens the RGB
        // toward black; `with_alpha` keeps the straight RGB, so the two differ
        // and `with_alpha` is no darker on any channel. (Premultiplied storage
        // is lossy, so this asserts the *direction* rather than exact straight
        // RGB.)
        let src = Color32::from_rgba_unmultiplied(200, 100, 50, 100);
        let buggy = Color32::from_rgba_unmultiplied(src.r(), src.g(), src.b(), 40);
        let fixed = with_alpha(src, 40);
        assert_ne!(
            fixed, buggy,
            "with_alpha must not reproduce the double-premultiply"
        );
        assert!(
            fixed.r() >= buggy.r() && fixed.g() >= buggy.g() && fixed.b() >= buggy.b(),
            "fixed {fixed:?} must be no darker than buggy {buggy:?}"
        );
        assert!(
            fixed.r() > buggy.r() || fixed.g() > buggy.g() || fixed.b() > buggy.b(),
            "the bug actually darkened at least one channel"
        );
        assert_eq!(fixed.a(), 40);
    }

    #[test]
    fn scale_alpha_scales_alpha_and_clamps() {
        // Opaque source so the straight RGB is lossless; only the alpha math is
        // under test.
        let src = Color32::from_rgb(10, 20, 30);
        // factor 0.5 -> alpha round(255·0.5) = 128, RGB preserved.
        assert_eq!(
            scale_alpha(src, 0.5),
            Color32::from_rgba_unmultiplied(10, 20, 30, 128)
        );
        // factor > 1 saturates at the source alpha; a negative factor clamps to 0.
        assert_eq!(scale_alpha(src, 2.0).a(), 255);
        assert_eq!(scale_alpha(src, -1.0).a(), 0);
    }
}
