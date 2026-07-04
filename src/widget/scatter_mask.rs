//! Scatter mask tools, porting silx `ScatterMaskToolsWidget`.
//!
//! Unlike the image mask ([`crate::widget::mask_tools`]) which masks pixels,
//! the scatter mask selects scatter *points by index*. The mask is a 1D
//! `uint8` buffer with one entry per point: `0` is unmasked, `1..=255` are
//! mask levels. The level / invert / clear / undo semantics mirror the image
//! mask (both descend from silx `BaseMask`); only the geometric selection
//! tests differ, operating on the point coordinate arrays instead of a pixel
//! grid.
//!
//! Faithful to `silx/gui/plot/ScatterMaskToolsWidget.py` (the `ScatterMask`
//! class) and the `Polygon.is_inside` point-in-polygon test in
//! `silx/image/shapes.pyx`.

use crate::widget::mask_tools::MaskHistory;

/// A multi-level mask over scatter points, selected by point index.
///
/// Mirrors silx `ScatterMask`: a 1D `uint8` array the same length as the
/// scatter data, where `0` means unmasked and `1..=255` are the (up to 254)
/// non-overlapping mask levels.
pub struct ScatterMaskWidget {
    /// Per-point mask level: `0` is unmasked, `1..=255` is a mask level. The
    /// length equals the number of scatter points.
    pub mask: Vec<u8>,

    /// Current mask level edited by the tools (silx `levelSpinBox`,
    /// range 1..=255).
    pub level: u8,

    history: MaskHistory,
}

impl ScatterMaskWidget {
    /// Create a scatter mask for `npoints` points, all initially unmasked.
    pub fn new(npoints: usize) -> Self {
        let mask = vec![0u8; npoints];
        let history = MaskHistory::new(&mask);
        Self {
            mask,
            level: 1,
            history,
        }
    }

    /// Number of points covered by the mask.
    pub fn len(&self) -> usize {
        self.mask.len()
    }

    /// Whether the mask covers zero points.
    pub fn is_empty(&self) -> bool {
        self.mask.is_empty()
    }

    /// Reset the mask to `npoints` points and clear it.
    ///
    /// Mirrors silx `reset(shape)`: a length change resets the undo history.
    pub fn reset_len(&mut self, npoints: usize) {
        self.mask = vec![0u8; npoints];
        self.history.reset(&self.mask);
    }

    // --- Whole-mask operations (silx BaseMask) ---

    /// Set all points of the current level back to `0`.
    ///
    /// Mirrors silx `BaseMask.clear(level)`.
    pub fn clear(&mut self) {
        let level = self.level;
        for cell in &mut self.mask {
            if *cell == level {
                *cell = 0;
            }
        }
    }

    /// Clear every mask level (reset the whole mask to `0`).
    ///
    /// Mirrors silx `resetSelectionMask`.
    pub fn clear_all(&mut self) {
        self.mask.fill(0);
    }

    /// Invert the current mask level over all points.
    ///
    /// `0` points become the current level and current-level points become
    /// `0`; points at other levels are untouched. Mirrors silx
    /// `BaseMask.invert(level)`.
    pub fn invert(&mut self) {
        let level = self.level;
        for cell in &mut self.mask {
            if *cell == 0 {
                *cell = level;
            } else if *cell == level {
                *cell = 0;
            }
        }
    }

    // --- Undo / redo (silx BaseMask history) ---

    /// Commit the current mask to the undo history.
    ///
    /// Mirrors silx `BaseMask.commit`.
    pub fn commit(&mut self) {
        self.history.commit(&self.mask);
    }

    /// Restore the previous mask snapshot, if any. Returns `true` if applied.
    ///
    /// Mirrors silx `BaseMask.undo`.
    pub fn undo(&mut self) -> bool {
        if let Some(snapshot) = self.history.undo() {
            self.mask = snapshot;
            true
        } else {
            false
        }
    }

    /// Restore the most recently undone snapshot, if any. Returns `true` if
    /// applied.
    ///
    /// Mirrors silx `BaseMask.redo`.
    pub fn redo(&mut self) -> bool {
        if let Some(snapshot) = self.history.redo() {
            self.mask = snapshot;
            true
        } else {
            false
        }
    }

    /// Reset the undo history to the current mask as the only baseline.
    ///
    /// Mirrors silx `BaseMask.resetHistory`.
    pub fn reset_history(&mut self) {
        self.history.reset(&self.mask);
    }

    /// Whether an undo is currently possible (silx `sigUndoable`).
    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    /// Whether a redo is currently possible (silx `sigRedoable`).
    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    // --- Selection operations on point arrays ---

    /// Mask or unmask the points at the given `indices` at `level`.
    ///
    /// Mirrors silx `ScatterMask.updatePoints` (ScatterMaskToolsWidget.py:89):
    /// when `mask` is true the points are set to `level`; otherwise only
    /// points already at `level` and in `indices` are cleared. Indices outside
    /// the buffer are ignored.
    pub fn update_points(&mut self, level: u8, indices: &[usize], mask: bool) {
        for &idx in indices {
            if idx < self.mask.len() {
                if mask {
                    self.mask[idx] = level;
                } else if self.mask[idx] == level {
                    self.mask[idx] = 0;
                }
            }
        }
    }

    /// Mask or unmask points selected by a per-point boolean stencil at
    /// `level`.
    ///
    /// Mirrors silx `BaseMask.updateStencil` (_BaseMaskToolsWidget.py:249):
    /// when `mask` is true the stencil-true points are set to `level`,
    /// otherwise only points already at `level` and stencil-true are cleared.
    /// `stencil` must be the same length as the mask.
    pub fn update_stencil(&mut self, level: u8, stencil: &[bool], mask: bool) {
        for (idx, &selected) in stencil.iter().enumerate() {
            if selected && idx < self.mask.len() {
                if mask {
                    self.mask[idx] = level;
                } else if self.mask[idx] == level {
                    self.mask[idx] = 0;
                }
            }
        }
    }

    /// Mask points inside a disk of the given radius centered at
    /// `center = (cx, cy)`.
    ///
    /// Mirrors silx `ScatterMask.updateDisk` (ScatterMaskToolsWidget.py:137):
    /// the stencil is `(y - cy)^2 + (x - cx)^2 < radius^2` (strict). `x` and
    /// `y` are the point coordinate arrays (same length as the mask).
    pub fn update_disk(
        &mut self,
        level: u8,
        center: (f32, f32),
        radius: f32,
        x: &[f32],
        y: &[f32],
    ) {
        self.update_disk_with_mask(level, center, radius, x, y, true);
    }

    /// [`update_disk`] with an explicit mask/unmask flag. `center = (cx, cy)`.
    ///
    /// [`update_disk`]: Self::update_disk
    pub fn update_disk_with_mask(
        &mut self,
        level: u8,
        center: (f32, f32),
        radius: f32,
        x: &[f32],
        y: &[f32],
        mask: bool,
    ) {
        let (cx, cy) = center;
        let r2 = radius * radius;
        let stencil: Vec<bool> = x
            .iter()
            .zip(y.iter())
            .map(|(&px, &py)| {
                let dy = py - cy;
                let dx = px - cx;
                dy * dy + dx * dx < r2
            })
            .collect();
        self.update_stencil(level, &stencil, mask);
    }

    /// Mask or unmask points inside a polygon at `level`.
    ///
    /// Mirrors silx `ScatterMask.updatePolygon` (ScatterMaskToolsWidget.py:107):
    /// a `Polygon.is_inside(y, x)` test per point. `vertices` are `(y, x)`
    /// corners (matching the silx call site, which passes plot `(y, x)`); `x`
    /// and `y` are the point coordinate arrays.
    pub fn update_polygon(
        &mut self,
        level: u8,
        vertices: &[(f32, f32)],
        x: &[f32],
        y: &[f32],
        mask: bool,
    ) {
        let n = x.len().min(y.len());
        let indices: Vec<usize> = (0..n)
            .filter(|&idx| point_in_polygon(vertices, y[idx], x[idx]))
            .collect();
        self.update_points(level, &indices, mask);
    }

    /// Mask or unmask points inside a rectangle at `level`.
    ///
    /// Mirrors silx `ScatterMask.updateRectangle` (ScatterMaskToolsWidget.py:124):
    /// the rectangle is built as the 4-vertex polygon
    /// `[(y, x), (y+height, x), (y+height, x+width), (y, x+width)]` then the
    /// polygon test is applied. `anchor = (y, x)` is the bottom-left corner and
    /// `size = (height, width)`; `px`/`py` are the point coordinate arrays.
    pub fn update_rectangle(
        &mut self,
        level: u8,
        anchor: (f32, f32),
        size: (f32, f32),
        px: &[f32],
        py: &[f32],
        mask: bool,
    ) {
        let (y, x) = anchor;
        let (height, width) = size;
        let vertices = [
            (y, x),
            (y + height, x),
            (y + height, x + width),
            (y, x + width),
        ];
        self.update_polygon(level, &vertices, px, py, mask);
    }

    /// Mask or unmask points inside an ellipse at `level`.
    ///
    /// Mirrors silx `ScatterMask.updateEllipse`
    /// (ScatterMaskToolsWidget.py:150-168): a point is inside when
    /// `(px - ccol)²/radius_c² + (py - crow)²/radius_r² <= 1.0` — INCLUSIVE,
    /// unlike the disk's strict `<`. `(crow, ccol)` is the center in `(y, x)`
    /// order and `radius_r`/`radius_c` the per-axis radii; `x`/`y` are the
    /// point coordinate arrays.
    #[allow(clippy::too_many_arguments)] // positional mirror of the silx signature
    pub fn update_ellipse(
        &mut self,
        level: u8,
        crow: f32,
        ccol: f32,
        radius_r: f32,
        radius_c: f32,
        x: &[f32],
        y: &[f32],
        mask: bool,
    ) {
        let n = x.len().min(y.len());
        let indices: Vec<usize> = (0..n)
            .filter(|&idx| {
                let dx = x[idx] - ccol;
                let dy = y[idx] - crow;
                dx * dx / (radius_c * radius_c) + dy * dy / (radius_r * radius_r) <= 1.0
            })
            .collect();
        self.update_points(level, &indices, mask);
    }

    /// Mask or unmask points inside a rectangle defined by a line (two end
    /// points) and a width at `level`.
    ///
    /// Mirrors silx `ScatterMask.updateLine`
    /// (ScatterMaskToolsWidget.py:170-194): the band is the 4-vertex polygon
    /// spanned by offsetting both end points by `±width/2` perpendicular to
    /// the line, with `theta = atan((y1 - y0)/(x1 - x0))` — plain `atan` of
    /// the slope, and `theta = 0` for a vertical line (silx's own rule; the
    /// polygon then degenerates to zero width, so a vertical pencil stroke
    /// masks only through its disks — kept bug-for-bug). End points are
    /// `(y, x)`-ordered rows/columns; `x`/`y` are the point coordinate arrays.
    #[allow(clippy::too_many_arguments)] // positional mirror of the silx signature
    pub fn update_line(
        &mut self,
        level: u8,
        y0: f32,
        x0: f32,
        y1: f32,
        x1: f32,
        width: f32,
        x: &[f32],
        y: &[f32],
        mask: bool,
    ) {
        let theta = if x1 != x0 {
            ((y1 - y0) / (x1 - x0)).atan()
        } else {
            0.0
        };
        let w_sin = width / 2.0 * theta.sin();
        let w_cos = width / 2.0 * theta.cos();
        let vertices = [
            (y0 - w_cos, x0 + w_sin),
            (y0 + w_cos, x0 - w_sin),
            (y1 + w_cos, x1 - w_sin),
            (y1 - w_cos, x1 + w_sin),
        ];
        self.update_polygon(level, &vertices, x, y, mask);
    }

    // --- Threshold / not-finite over the point value array ---

    /// Mask or unmask points whose value is below `threshold` at `level`.
    ///
    /// Mirrors silx `BaseMask.updateBelowThreshold` over the scatter value
    /// array (`values < threshold`).
    pub fn update_below_threshold(
        &mut self,
        level: u8,
        values: &[f32],
        threshold: f32,
        mask: bool,
    ) {
        let stencil: Vec<bool> = values.iter().map(|&v| v < threshold).collect();
        self.update_stencil(level, &stencil, mask);
    }

    /// Mask or unmask points whose value is within `[min, max]` at `level`.
    ///
    /// Mirrors silx `BaseMask.updateBetweenThresholds`
    /// (`min <= value <= max`, inclusive).
    pub fn update_between_thresholds(
        &mut self,
        level: u8,
        values: &[f32],
        min: f32,
        max: f32,
        mask: bool,
    ) {
        let stencil: Vec<bool> = values.iter().map(|&v| min <= v && v <= max).collect();
        self.update_stencil(level, &stencil, mask);
    }

    /// Mask or unmask points whose value is above `threshold` at `level`.
    ///
    /// Mirrors silx `BaseMask.updateAboveThreshold` (`value > threshold`).
    pub fn update_above_threshold(
        &mut self,
        level: u8,
        values: &[f32],
        threshold: f32,
        mask: bool,
    ) {
        let stencil: Vec<bool> = values.iter().map(|&v| v > threshold).collect();
        self.update_stencil(level, &stencil, mask);
    }

    /// Mask every point whose value is not finite (NaN or +/-infinity) at the
    /// current level.
    ///
    /// Mirrors silx `_BaseMaskToolsWidget.updateNotFinite` over the scatter
    /// value array (`~numpy.isfinite(values)`).
    pub fn mask_not_finite(&mut self, values: &[f32]) {
        let stencil: Vec<bool> = values.iter().map(|&v| !v.is_finite()).collect();
        self.update_stencil(self.level, &stencil, true);
    }
}

/// Point-in-polygon test, faithful to `silx.image.shapes.Polygon.is_inside`
/// (image/shapes.pyx:64-102).
///
/// `vertices` are `(row, col)` (equivalently `(y, x)`) corners stored as the
/// silx `Polygon` does, in `f32`. A horizontal ray crossing count (xor) gives
/// the inside test; the closing segment from the last vertex to the first is
/// included by seeding the previous point with the last vertex. Returns
/// `false` for fewer than 1 vertex.
pub fn point_in_polygon(vertices: &[(f32, f32)], row: f32, col: f32) -> bool {
    let nvert = vertices.len();
    if nvert == 0 {
        return false;
    }

    let mut is_inside = false;
    // Start the previous point at the last vertex so the closing segment is
    // visited (silx: pt1 = vertices[nvert - 1]).
    let (mut pt1y, mut pt1x) = vertices[nvert - 1];
    for &(pt2y, pt2x) in vertices {
        if ((pt1y <= row && row < pt2y) || (pt2y <= row && row < pt1y))
            // Extra (optional) condition matching silx to skip work.
            && (col <= pt1x || col <= pt2x)
        {
            let xinters = (row - pt1y) * (pt2x - pt1x) / (pt2y - pt1y) + pt1x;
            if col < xinters {
                is_inside = !is_inside;
            }
        }
        pt1y = pt2y;
        pt1x = pt2x;
    }
    is_inside
}

/// The scatter pencil width in data coordinates: the widget pencil size scaled
/// by 1% of the scatter's data extent — silx
/// `ScatterMaskToolsWidget._getPencilWidth`
/// (`width *= 0.01 * self._data_extent`, ScatterMaskToolsWidget.py:532-540)
/// with `_data_extent = max(xMax - xMin, yMax - yMin)` over the scatter's
/// coordinates (`_adjustColorAndBrushSize`, :318-327; silx `min_max` ignores
/// NaN). Returns `base_width` unscaled when the data is empty or has no finite
/// coordinate (silx `_data_extent = None` skips the scaling).
pub fn scatter_pencil_width(base_width: f32, x: &[f32], y: &[f32]) -> f32 {
    let finite_extent = |vals: &[f32]| -> Option<f32> {
        let mut range: Option<(f32, f32)> = None;
        for &v in vals {
            if v.is_finite() {
                let (lo, hi) = range.get_or_insert((v, v));
                *lo = lo.min(v);
                *hi = hi.max(v);
            }
        }
        range.map(|(lo, hi)| hi - lo)
    };
    match (finite_extent(x), finite_extent(y)) {
        (Some(dx), Some(dy)) => base_width * 0.01 * dx.max(dy),
        _ => base_width,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ellipse_test_is_inclusive_and_per_axis() {
        // silx updateEllipse (ScatterMaskToolsWidget.py:150-168):
        // (px-ccol)²/rc² + (py-crow)²/rr² <= 1.0 — INCLUSIVE, unlike the
        // disk's strict <. Center (row 0, col 0), radius_r = 1 (y), radius_c
        // = 2 (x): (±2, 0) and (0, ±1) lie exactly on the boundary.
        let x = [0.0_f32, 2.0, 0.0, 2.0, 2.1, 0.0];
        let y = [0.0_f32, 0.0, 1.0, 1.0, 0.0, 1.1];
        let mut m = ScatterMaskWidget::new(6);
        m.update_ellipse(1, 0.0, 0.0, 1.0, 2.0, &x, &y, true);
        // (0,0) in; (2,0) on boundary in; (0,1) on boundary in;
        // (2,1) sum=2 out; (2.1,0) out; (0,1.1) out.
        assert_eq!(m.mask, vec![1, 1, 1, 0, 0, 0]);
        // Unmask only clears the level's points inside.
        m.update_ellipse(1, 0.0, 0.0, 1.0, 2.0, &x, &y, false);
        assert_eq!(m.mask, vec![0; 6]);
    }

    #[test]
    fn line_masks_a_width_band_as_a_rotated_rectangle() {
        // silx updateLine (ScatterMaskToolsWidget.py:170-194): horizontal
        // line row 0, columns 0→4, width 2 → band |row| < 1 over x ∈ (0, 4).
        let x = [1.0_f32, 3.0, 1.0, 1.0, 5.0];
        let y = [0.0_f32, 0.5, -0.5, 1.5, 0.0];
        let mut m = ScatterMaskWidget::new(5);
        m.update_line(1, 0.0, 0.0, 0.0, 4.0, 2.0, &x, &y, true);
        assert_eq!(m.mask, vec![1, 1, 1, 0, 0], "in-band in, above/past out");
    }

    #[test]
    fn vertical_line_band_degenerates_like_silx() {
        // silx sets theta = 0 for x1 == x0, so the band polygon has zero
        // width — no point is inside (a vertical pencil stroke masks only
        // through its disks). Kept bug-for-bug.
        let x = [0.0_f32, 0.5];
        let y = [1.0_f32, 1.0];
        let mut m = ScatterMaskWidget::new(2);
        m.update_line(1, 0.0, 0.0, 2.0, 0.0, 2.0, &x, &y, true);
        assert_eq!(m.mask, vec![0, 0]);
    }

    #[test]
    fn pencil_width_scales_by_one_percent_of_data_extent() {
        // silx _getPencilWidth: width *= 0.01 * max(xMax-xMin, yMax-yMin)
        // (ScatterMaskToolsWidget.py:532-540, extent from :318-327).
        let x = [0.0_f32, 50.0, f32::NAN];
        let y = [0.0_f32, 10.0, 0.0];
        assert_eq!(scatter_pencil_width(4.0, &x, &y), 4.0 * 0.01 * 50.0);
        // Empty or non-finite data: unscaled (silx _data_extent = None).
        assert_eq!(scatter_pencil_width(4.0, &[], &[]), 4.0);
        assert_eq!(scatter_pencil_width(4.0, &[f32::NAN], &[f32::NAN]), 4.0);
    }

    #[test]
    fn disk_selects_points_within_radius() {
        // silx updateDisk stencil: (y-cy)^2 + (x-cx)^2 < r^2, strict.
        // Center (0,0), radius 2. Points at distance 0, 1, 2 (on boundary), 3.
        let x = [0.0_f32, 1.0, 2.0, 3.0];
        let y = [0.0_f32, 0.0, 0.0, 0.0];
        let mut m = ScatterMaskWidget::new(4);
        m.update_disk(1, (0.0, 0.0), 2.0, &x, &y);
        // dist^2 = 0, 1, 4, 9; r^2 = 4. Strict <: 0 and 1 selected, 2 (==4) not.
        assert_eq!(m.mask, vec![1, 1, 0, 0]);
    }

    #[test]
    fn disk_radius_boundary_is_strict() {
        // A point exactly on the disk boundary is excluded (strict <).
        let x = [2.0_f32];
        let y = [0.0_f32];
        let mut m = ScatterMaskWidget::new(1);
        m.update_disk(1, (0.0, 0.0), 2.0, &x, &y);
        assert_eq!(m.mask, vec![0]);
    }

    #[test]
    fn polygon_selects_interior_points() {
        // Unit square with corners (y, x): (0,0),(0,4),(4,4),(4,0).
        // Interior point (2,2) inside; (5,5) outside; (0,0) on a corner edge.
        let square = [(0.0_f32, 0.0), (0.0, 4.0), (4.0, 4.0), (4.0, 0.0)];
        // points: x, y arrays
        let x = [2.0_f32, 5.0, 1.0];
        let y = [2.0_f32, 5.0, 3.0];
        let mut m = ScatterMaskWidget::new(3);
        m.update_polygon(1, &square, &x, &y, true);
        // (2,2) inside, (5,5) outside, (3,1) inside.
        assert_eq!(m.mask, vec![1, 0, 1]);
    }

    #[test]
    fn point_in_polygon_triangle_inside_vs_outside() {
        // Triangle (y, x): (0,0), (0,4), (4,0).
        let tri = [(0.0_f32, 0.0), (0.0, 4.0), (4.0, 0.0)];
        // (1,1) is inside; (3,3) is outside (beyond the hypotenuse).
        assert!(point_in_polygon(&tri, 1.0, 1.0));
        assert!(!point_in_polygon(&tri, 3.0, 3.0));
    }

    #[test]
    fn rectangle_selects_points_inside() {
        // Rectangle anchored at (y=1, x=1) with height=2, width=2 -> covers
        // y in [1,3], x in [1,3]. Build via polygon corners as silx does.
        let x = [2.0_f32, 0.0, 5.0];
        let y = [2.0_f32, 0.0, 5.0];
        let mut m = ScatterMaskWidget::new(3);
        m.update_rectangle(1, (1.0, 1.0), (2.0, 2.0), &x, &y, true);
        // (2,2) inside; (0,0) and (5,5) outside.
        assert_eq!(m.mask, vec![1, 0, 0]);
    }

    #[test]
    fn update_points_masks_and_unmasks_by_index() {
        let mut m = ScatterMaskWidget::new(4);
        m.update_points(1, &[0, 2], true);
        assert_eq!(m.mask, vec![1, 0, 1, 0]);
        // Unmask only level-1 points among the given indices.
        m.mask[2] = 2; // change index 2 to a different level
        m.update_points(1, &[0, 2], false);
        // index 0 (level 1) cleared, index 2 (level 2) untouched.
        assert_eq!(m.mask, vec![0, 0, 2, 0]);
    }

    #[test]
    fn update_points_ignores_out_of_range_indices() {
        let mut m = ScatterMaskWidget::new(2);
        m.update_points(1, &[0, 5, 99], true);
        assert_eq!(m.mask, vec![1, 0]);
    }

    #[test]
    fn clear_only_affects_current_level() {
        // silx BaseMask.clear(level): only `level` points go to 0.
        let mut m = ScatterMaskWidget::new(4);
        m.mask = vec![1, 2, 1, 0];
        m.level = 1;
        m.clear();
        assert_eq!(m.mask, vec![0, 2, 0, 0]);
    }

    #[test]
    fn clear_all_resets_every_level() {
        let mut m = ScatterMaskWidget::new(4);
        m.mask = vec![1, 2, 255, 0];
        m.clear_all();
        assert_eq!(m.mask, vec![0, 0, 0, 0]);
    }

    #[test]
    fn invert_swaps_zero_and_current_level_only() {
        // silx BaseMask.invert(level): 0 <-> level, other levels untouched.
        let mut m = ScatterMaskWidget::new(4);
        m.mask = vec![0, 1, 2, 0];
        m.level = 1;
        m.invert();
        assert_eq!(m.mask, vec![1, 0, 2, 1]);
    }

    #[test]
    fn undo_then_redo_round_trips_one_change() {
        let mut m = ScatterMaskWidget::new(4);
        m.mask = vec![1, 0, 0, 0];
        m.commit();
        assert!(m.can_undo());

        assert!(m.undo());
        assert_eq!(m.mask, vec![0, 0, 0, 0]); // back to baseline
        assert!(!m.can_undo());
        assert!(m.can_redo());

        assert!(m.redo());
        assert_eq!(m.mask, vec![1, 0, 0, 0]);
        assert!(!m.can_redo());
    }

    #[test]
    fn undo_is_noop_with_only_baseline() {
        let mut m = ScatterMaskWidget::new(4);
        assert!(!m.can_undo());
        assert!(!m.undo());
    }

    #[test]
    fn threshold_below_is_strict() {
        let mut m = ScatterMaskWidget::new(4);
        let values = [0.0_f32, 1.0, 2.0, 3.0];
        m.update_below_threshold(1, &values, 2.0, true);
        assert_eq!(m.mask, vec![1, 1, 0, 0]);
    }

    #[test]
    fn threshold_between_is_inclusive() {
        let mut m = ScatterMaskWidget::new(5);
        let values = [0.0_f32, 1.0, 2.0, 3.0, 4.0];
        m.update_between_thresholds(1, &values, 1.0, 3.0, true);
        assert_eq!(m.mask, vec![0, 1, 1, 1, 0]);
    }

    #[test]
    fn threshold_above_is_strict() {
        let mut m = ScatterMaskWidget::new(4);
        let values = [0.0_f32, 1.0, 2.0, 3.0];
        m.update_above_threshold(1, &values, 2.0, true);
        assert_eq!(m.mask, vec![0, 0, 0, 1]);
    }

    #[test]
    fn mask_not_finite_masks_nan_and_infinities_only() {
        let mut m = ScatterMaskWidget::new(5);
        m.level = 1;
        let values = [0.0_f32, f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 7.0];
        m.mask_not_finite(&values);
        assert_eq!(m.mask, vec![0, 1, 1, 1, 0]);
    }

    #[test]
    fn empty_mask_selection_is_noop() {
        // Boundary: zero points. Every selection op leaves an empty buffer.
        let mut m = ScatterMaskWidget::new(0);
        assert!(m.is_empty());
        m.update_disk(1, (0.0, 0.0), 1.0, &[], &[]);
        m.update_polygon(1, &[(0.0, 0.0), (1.0, 1.0), (0.0, 1.0)], &[], &[], true);
        m.mask_not_finite(&[]);
        assert!(m.mask.is_empty());
    }

    #[test]
    fn point_in_polygon_empty_vertices_is_false() {
        assert!(!point_in_polygon(&[], 0.0, 0.0));
    }
}
