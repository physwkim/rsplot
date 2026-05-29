//! Shared item vocabulary used by both the GPU data layer and the egui overlay
//! layer: currently the line stroke style ([`LineStyle`]).
//!
//! These types live in `core` (not `render`) so the `core::Plot` model — which
//! stores overlay items (markers, shapes) carrying a [`LineStyle`] — can name
//! them without `core` depending on `render` (`doc/design.md` §9 `core/items.rs`).

/// Line stroke style (silx `linestyle`). Dash lengths for the predefined styles
/// scale with the line width (`max(width, 1)`) so they stay proportionate at any
/// thickness; a [`LineStyle::Custom`] pattern is taken verbatim. The dash unit is
/// physical pixels on the GPU curve path and logical points on the egui painter
/// overlay path.
#[derive(Clone, Debug, PartialEq)]
pub enum LineStyle {
    /// No line drawn (markers only, if any). silx `' '` / `''`.
    None,
    /// Continuous line. silx `'-'`.
    Solid,
    /// Dashed line. silx `'--'`.
    Dashed,
    /// Dash-dot line. silx `'-.'`.
    DashDot,
    /// Dotted line. silx `':'`.
    Dotted,
    /// Custom dash pattern: alternating on/off lengths (`on, off, on, off`), with
    /// `offset` the starting phase. silx `(offset, (dash pattern))`.
    Custom { offset: f32, pattern: Vec<f32> },
}

impl LineStyle {
    /// Whether this style draws a line at all (false only for [`LineStyle::None`]).
    pub(crate) fn draws_line(&self) -> bool {
        !matches!(self, LineStyle::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_line_false_only_for_none() {
        assert!(!LineStyle::None.draws_line());
        assert!(LineStyle::Solid.draws_line());
        assert!(LineStyle::Dashed.draws_line());
        assert!(LineStyle::DashDot.draws_line());
        assert!(LineStyle::Dotted.draws_line());
    }
}
