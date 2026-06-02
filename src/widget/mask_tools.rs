use egui::Color32;

use crate::core::backend::ItemHandle;
use crate::widget::high_level::Plot2D;
use crate::widget::plot_widget::PlotResponse;

/// Drawing tools mirroring silx `_BaseMaskToolsWidget` draw modes
/// (rectangle, ellipse, polygon, pencil/eraser).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskTool {
    None,
    Pencil,
    Eraser,
    Rectangle,
    Polygon,
    Ellipse,
}

/// A widget for interactively drawing a multi-level mask over a 2D image.
///
/// The mask mirrors silx `ImageMask`: a `uint8` array the same shape as the
/// image, where `0` means unmasked and `1..=255` are the (up to 254)
/// non-overlapping mask levels. Drawing writes the current [`level`], the
/// eraser clears it back to `0`.
///
/// [`level`]: Self::level
pub struct MaskToolsWidget {
    /// Per-pixel mask level in image (row, col) order: `0` is unmasked,
    /// `1..=255` is a mask level.
    pub mask: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub color: Color32,

    /// Current mask level edited by the drawing tools (silx `levelSpinBox`,
    /// range 1..=255).
    pub level: u8,

    pub active_tool: MaskTool,
    pub brush_size: u32,

    mask_handle: Option<ItemHandle>,
    is_dirty: bool,
}

impl MaskToolsWidget {
    /// Create a new MaskToolsWidget for an image of the given dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            mask: vec![0; (width * height) as usize],
            width,
            height,
            color: Color32::from_rgba_unmultiplied(255, 0, 0, 128), // Default semi-transparent red
            level: 1,
            active_tool: MaskTool::None,
            brush_size: 1,
            mask_handle: None,
            is_dirty: true, // Force initial upload
        }
    }

    /// Reset the mask to the given dimensions and clear it.
    pub fn reset_geometry(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.mask = vec![0; (width * height) as usize];
        self.is_dirty = true;
    }

    /// Set all pixels of the current level back to `0`.
    ///
    /// Mirrors silx `BaseMask.clear(level)`.
    pub fn clear(&mut self) {
        let level = self.level;
        for cell in &mut self.mask {
            if *cell == level {
                *cell = 0;
            }
        }
        self.is_dirty = true;
    }

    /// Clear every mask level (reset the whole mask to `0`).
    ///
    /// Mirrors silx `resetSelectionMask` (`reset()` to zeros).
    pub fn clear_all(&mut self) {
        self.mask.fill(0);
        self.is_dirty = true;
    }

    /// Apply the mask onto a `Plot2D`.
    ///
    /// This should be called every frame after handling interaction,
    /// so the mask visual overlay stays up-to-date.
    pub fn apply(&mut self, plot: &mut Plot2D) {
        if !self.is_dirty {
            return;
        }

        // Build RGBA from the level buffer: any masked pixel (non-zero level)
        // is painted with the overlay color, unmasked pixels are transparent.
        let rgba: Vec<[u8; 4]> = self
            .mask
            .iter()
            .map(|&level| {
                if level != 0 {
                    [
                        self.color.r(),
                        self.color.g(),
                        self.color.b(),
                        self.color.a(),
                    ]
                } else {
                    [0, 0, 0, 0]
                }
            })
            .collect();

        if let Some(handle) = self.mask_handle {
            // Update existing mask image
            if plot
                .try_update_rgba_image(handle, self.width, self.height, &rgba)
                .is_err()
            {
                // If update fails (e.g. size mismatch), clear handle to force recreation
                plot.remove(handle);
                self.mask_handle = None;
            }
        }

        if self.mask_handle.is_none() {
            // Mark the overlay as a mask item: derive a boolean (masked /
            // unmasked) view from the level buffer for the existing API.
            let bool_view: Vec<bool> = self.mask.iter().map(|&level| level != 0).collect();
            if let Ok(handle) = plot.add_mask(self.width, self.height, &bool_view, self.color) {
                self.mask_handle = Some(handle);
            }
        }

        self.is_dirty = false;
    }

    /// Show the masking tools toolbar.
    pub fn show_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Mask:");

            ui.selectable_value(&mut self.active_tool, MaskTool::None, "○")
                .on_hover_text("Disable masking");
            ui.selectable_value(&mut self.active_tool, MaskTool::Pencil, "Pencil")
                .on_hover_text("Draw mask");
            ui.selectable_value(&mut self.active_tool, MaskTool::Eraser, "Eraser")
                .on_hover_text("Erase mask");
            ui.selectable_value(&mut self.active_tool, MaskTool::Rectangle, "Rectangle")
                .on_hover_text("Mask a rectangular region");
            ui.selectable_value(&mut self.active_tool, MaskTool::Polygon, "Polygon")
                .on_hover_text("Mask a polygonal region");
            ui.selectable_value(&mut self.active_tool, MaskTool::Ellipse, "Ellipse")
                .on_hover_text("Mask an elliptical region");

            ui.add(egui::Slider::new(&mut self.level, 1..=255).text("Mask level"));

            if self.active_tool != MaskTool::None {
                ui.add(egui::Slider::new(&mut self.brush_size, 1..=50).text("Brush size"));
            }

            if ui.button("Clear").clicked() {
                self.clear();
            }
            if ui.button("Clear All").clicked() {
                self.clear_all();
            }
        });
    }

    /// Handle pointer interaction from the plot response to paint/erase the mask.
    pub fn handle_interaction(&mut self, plot_response: &PlotResponse) {
        if !matches!(self.active_tool, MaskTool::Pencil | MaskTool::Eraser) {
            return;
        }

        // Only draw when the primary pointer button is held down
        if (plot_response
            .response
            .dragged_by(egui::PointerButton::Primary)
            || plot_response
                .response
                .clicked_by(egui::PointerButton::Primary))
            && let Some(pointer_pos) = plot_response.response.interact_pointer_pos()
        {
            let (data_x, data_y) = plot_response.transform.pixel_to_data(pointer_pos);
            let center_col = data_x.floor() as i64;
            let center_row = data_y.floor() as i64;

            let value = if self.active_tool == MaskTool::Pencil {
                self.level
            } else {
                0
            };
            let r = self.brush_size as i64;
            let r_squared = r * r;

            let w = self.width as i64;
            let h = self.height as i64;

            let mut changed = false;

            for dy in -r..=r {
                for dx in -r..=r {
                    // Circular brush
                    if dx * dx + dy * dy <= r_squared {
                        let col = center_col + dx;
                        let row = center_row + dy;

                        if col >= 0 && col < w && row >= 0 && row < h {
                            let idx = (row as usize) * (self.width as usize) + (col as usize);
                            if self.mask[idx] != value {
                                self.mask[idx] = value;
                                changed = true;
                            }
                        }
                    }
                }
            }

            if changed {
                self.is_dirty = true;
            }
        }
    }

    // Drawing operations on the level buffer, mirroring silx ImageMask.

    /// Mask (`mask = true`) or unmask a rectangle at the current level.
    ///
    /// Mirrors silx `ImageMask.updateRectangle` (gui/plot/MaskToolsWidget.py):
    /// the rectangle spans rows `row..=row + height` and columns
    /// `col..=col + width` (both endpoints inclusive). Pixels outside the
    /// image are clipped; when `mask` is false only pixels already at `level`
    /// are cleared.
    pub fn update_rectangle(
        &mut self,
        level: u8,
        row: i64,
        col: i64,
        height: i64,
        width: i64,
        mask: bool,
    ) {
        // Rectangle entirely above/left of the image: avoid negative indices.
        if row + height <= 0 || col + width <= 0 {
            return;
        }
        let img_w = self.width as i64;
        let img_h = self.height as i64;

        let row_start = row.max(0);
        let col_start = col.max(0);
        // silx slices [start : row + height + 1], clipped to the image bounds.
        let row_end = (row + height + 1).min(img_h);
        let col_end = (col + width + 1).min(img_w);

        for r in row_start..row_end {
            for c in col_start..col_end {
                let idx = (r as usize) * (self.width as usize) + (c as usize);
                self.set_or_clear(idx, level, mask);
            }
        }
        self.is_dirty = true;
    }

    /// Mask or unmask the interior of a polygon at the current level.
    ///
    /// Mirrors silx `ImageMask.updatePolygon`: the polygon interior is filled
    /// via [`polygon_fill_mask`], then masked/unmasked.
    pub fn update_polygon(&mut self, level: u8, vertices: &[(i64, i64)], mask: bool) {
        let fill = polygon_fill_mask(vertices, self.height, self.width);
        for (idx, &inside) in fill.iter().enumerate() {
            if inside {
                self.set_or_clear(idx, level, mask);
            }
        }
        self.is_dirty = true;
    }

    /// Mask or unmask the given `(row, col)` points at the current level.
    ///
    /// Mirrors silx `ImageMask.updatePoints`: out-of-bounds points are
    /// dropped; when `mask` is false only pixels already at `level` are
    /// cleared.
    pub fn update_points(&mut self, level: u8, rows: &[i64], cols: &[i64], mask: bool) {
        let img_w = self.width as i64;
        let img_h = self.height as i64;
        for (&r, &c) in rows.iter().zip(cols.iter()) {
            if r >= 0 && c >= 0 && r < img_h && c < img_w {
                let idx = (r as usize) * (self.width as usize) + (c as usize);
                self.set_or_clear(idx, level, mask);
            }
        }
        self.is_dirty = true;
    }

    /// Mask or unmask a disk of the given radius at the current level.
    ///
    /// Mirrors silx `ImageMask.updateDisk` (`circle_fill` then `updatePoints`).
    pub fn update_disk(&mut self, level: u8, crow: i64, ccol: i64, radius: f32, mask: bool) {
        let (rows, cols) = circle_fill(crow, ccol, radius);
        self.update_points(level, &rows, &cols, mask);
    }

    /// Mask or unmask an ellipse at the current level.
    ///
    /// Mirrors silx `ImageMask.updateEllipse` (`ellipse_fill` then
    /// `updatePoints`).
    pub fn update_ellipse(
        &mut self,
        level: u8,
        crow: i64,
        ccol: i64,
        radius_r: f32,
        radius_c: f32,
        mask: bool,
    ) {
        let (rows, cols) = ellipse_fill(crow, ccol, radius_r, radius_c);
        self.update_points(level, &rows, &cols, mask);
    }

    /// Set pixel `idx` to `level` (mask) or clear it to 0 if it currently
    /// holds `level` (unmask). Mirrors the silx mask/unmask branch shared by
    /// the update operations.
    fn set_or_clear(&mut self, idx: usize, level: u8, mask: bool) {
        if mask {
            self.mask[idx] = level;
        } else if self.mask[idx] == level {
            self.mask[idx] = 0;
        }
    }
}

/// Return a boolean fill mask (row-major, `height * width`) that is `true`
/// for pixels inside the polygon defined by `vertices`.
///
/// Faithful port of `silx.image.shapes.polygon_fill_mask` /
/// `Polygon.make_mask` (silx image/shapes.pyx): a per-row scanline xor fill
/// adapted from <http://alienryderflex.com/polygon_fill/>. `vertices` are
/// `(row, col)` corners; the computation runs in `f32` to match silx, which
/// stores the vertices as `float32`.
pub fn polygon_fill_mask(vertices: &[(i64, i64)], height: u32, width: u32) -> Vec<bool> {
    let height = height as i32;
    let width = width as i32;
    let mut mask = vec![false; (height.max(0) as usize) * (width.max(0) as usize)];

    let nvert = vertices.len();
    if nvert == 0 || height <= 0 || width <= 0 {
        return mask;
    }

    // Vertices in f32, matching silx Polygon's float32 storage.
    let verts: Vec<(f32, f32)> = vertices
        .iter()
        .map(|&(r, c)| (r as f32, c as f32))
        .collect();

    let mut row_f_min = verts[0].0;
    let mut row_f_max = verts[0].0;
    for &(r, _) in &verts {
        if r < row_f_min {
            row_f_min = r;
        }
        if r > row_f_max {
            row_f_max = r;
        }
    }
    // silx: row_min = max(int(min(rows)), 0); row_max = min(int(max(rows)) + 1, height)
    let row_min = (row_f_min as i32).max(0);
    let row_max = ((row_f_max as i32) + 1).min(height);

    for row in row_min..row_max {
        let row_f = row as f32;
        // Start from the last vertex so all segments (including the closing
        // one) are visited.
        let (mut pt1y, mut pt1x) = verts[nvert - 1];
        let mut col_min = width - 1;
        let mut col_max = 0;
        let mut is_inside: i32 = 0;

        for &(pt2y, pt2x) in &verts {
            if (pt1y <= row_f && row_f < pt2y) || (pt2y <= row_f && row_f < pt1y) {
                // Intersection cast to int so that ]x, x+1] => x.
                let xinters =
                    (pt1x + (row_f - pt1y) * (pt2x - pt1x) / (pt2y - pt1y)).ceil() as i32 - 1;

                if xinters < col_min {
                    col_min = xinters;
                }
                if xinters > col_max {
                    col_max = xinters;
                }

                if xinters < 0 {
                    // Intersection left of the image seeds the xor scan.
                    is_inside ^= 1;
                } else if xinters < width {
                    let idx = (row as usize) * (width as usize) + (xinters as usize);
                    mask[idx] = !mask[idx];
                }
                // else: intersection on the right is ignored.
            }
            pt1y = pt2y;
            pt1x = pt2x;
        }

        if col_min < col_max {
            let col_min = col_min.max(0);
            let col_max = col_max.min(width - 1);

            // xor exclusive scan to fill the interior between intersections.
            for col in col_min..=col_max {
                let idx = (row as usize) * (width as usize) + (col as usize);
                let current = mask[idx] as i32;
                mask[idx] = is_inside != 0;
                is_inside ^= current;
            }
        }
    }

    mask
}

/// Generate the `(rows, cols)` image coordinates lying inside a disk.
///
/// Faithful port of `silx.image.shapes.circle_fill` (image/shapes.pyx): a
/// point at offset `(dr, dc)` from the center is inside when
/// `dr^2 + dc^2 < radius^2` (strict), scanned over
/// `-floor(|radius|) ..= ceil(|radius|)`. Coordinates may be negative.
pub fn circle_fill(crow: i64, ccol: i64, radius: f32) -> (Vec<i64>, Vec<i64>) {
    let radius = radius.abs();
    let i_radius = radius as i64;
    let r2 = radius * radius;

    // offsets: -i_radius ..= ceil(radius)
    let lo = -i_radius;
    let hi = radius.ceil() as i64;

    let mut rows = Vec::new();
    let mut cols = Vec::new();
    // silx iterates the squared offset grid in (row, col) order.
    for dr in lo..=hi {
        for dc in lo..=hi {
            let dr_f = dr as f32;
            let dc_f = dc as f32;
            if dr_f * dr_f + dc_f * dc_f < r2 {
                rows.push(crow + dr);
                cols.push(ccol + dc);
            }
        }
    }
    (rows, cols)
}

/// Generate the `(rows, cols)` image coordinates lying inside an ellipse.
///
/// Faithful port of `silx.image.shapes.ellipse_fill` (image/shapes.pyx): a
/// point at offset `(dr, dc)` is inside when
/// `dr^2 / radius_r^2 + dc^2 / radius_c^2 < 1` (strict). The row axis uses
/// `radius_r`, the column axis `radius_c`. Coordinates may be negative.
pub fn ellipse_fill(crow: i64, ccol: i64, radius_r: f32, radius_c: f32) -> (Vec<i64>, Vec<i64>) {
    let i_radius_r = radius_r.abs() as i64;
    let i_radius_c = radius_c.abs() as i64;
    let rr2 = radius_r * radius_r;
    let rc2 = radius_c * radius_c;

    let r_lo = -i_radius_r;
    let r_hi = radius_r.ceil() as i64;
    let c_lo = -i_radius_c;
    let c_hi = radius_c.ceil() as i64;

    let mut rows = Vec::new();
    let mut cols = Vec::new();
    for dr in r_lo..=r_hi {
        for dc in c_lo..=c_hi {
            let dr_f = dr as f32;
            let dc_f = dc as f32;
            if dr_f * dr_f / rr2 + dc_f * dc_f / rc2 < 1.0 {
                rows.push(crow + dr);
                cols.push(ccol + dc);
            }
        }
    }
    (rows, cols)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_only_affects_current_level() {
        // Mirrors silx BaseMask.clear(level): only `level` pixels go to 0.
        let mut w = MaskToolsWidget::new(2, 2);
        w.mask = vec![1, 2, 1, 0];
        w.level = 1;
        w.clear();
        assert_eq!(w.mask, vec![0, 2, 0, 0]);
    }

    #[test]
    fn clear_all_resets_every_level() {
        // Mirrors silx resetSelectionMask: whole buffer back to 0.
        let mut w = MaskToolsWidget::new(2, 2);
        w.mask = vec![1, 2, 255, 0];
        w.clear_all();
        assert_eq!(w.mask, vec![0, 0, 0, 0]);
    }

    /// Render a mask buffer as a `height * width` grid of 0/1 for comparison.
    fn grid(mask: &[bool], width: usize) -> Vec<Vec<u8>> {
        mask.chunks(width)
            .map(|row| row.iter().map(|&b| b as u8).collect())
            .collect()
    }

    #[test]
    fn rectangle_fill_is_inclusive_of_both_edges() {
        // silx updateRectangle slices [row : row+height+1, col : col+width+1],
        // so a height/width of 1 covers 2 rows and 2 columns.
        let mut w = MaskToolsWidget::new(4, 4);
        w.update_rectangle(3, 1, 1, 1, 1, true);
        let expected: Vec<u8> = vec![
            0, 0, 0, 0, //
            0, 3, 3, 0, //
            0, 3, 3, 0, //
            0, 0, 0, 0, //
        ];
        assert_eq!(w.mask, expected);
    }

    #[test]
    fn rectangle_fill_clips_to_image_and_skips_fully_outside() {
        // Top-left corner outside image: clipped to the in-image part.
        // row=-1, height=2 -> slice [0 : -1+2+1] = [0:2] (rows 0,1).
        let mut w = MaskToolsWidget::new(3, 3);
        w.update_rectangle(2, -1, -1, 2, 2, true);
        let expected: Vec<u8> = vec![
            2, 2, 0, //
            2, 2, 0, //
            0, 0, 0, //
        ];
        assert_eq!(w.mask, expected);

        // Fully above/left (row + height <= 0): no-op (silx early return).
        let mut w2 = MaskToolsWidget::new(3, 3);
        w2.update_rectangle(2, -5, 0, 1, 1, true);
        assert!(w2.mask.iter().all(|&v| v == 0));
    }

    #[test]
    fn rectangle_unmask_only_clears_current_level() {
        let mut w = MaskToolsWidget::new(2, 2);
        w.mask = vec![1, 2, 1, 2];
        // Unmask level 1 over the whole image: only level-1 pixels clear.
        w.update_rectangle(1, 0, 0, 1, 1, false);
        assert_eq!(w.mask, vec![0, 2, 0, 2]);
    }

    #[test]
    fn polygon_fills_triangle_interior() {
        // silx test_shapes "concave polygon" reference on a 6x8 mask.
        let vertices = [(1, 1), (4, 3), (1, 5), (2, 3)];
        let mask = polygon_fill_mask(&vertices, 6, 8);
        let expected = vec![
            vec![0, 0, 0, 0, 0, 0, 0, 0],
            vec![0, 0, 0, 0, 0, 0, 0, 0],
            vec![0, 0, 1, 1, 1, 0, 0, 0],
            vec![0, 0, 0, 1, 0, 0, 0, 0],
            vec![0, 0, 0, 0, 0, 0, 0, 0],
            vec![0, 0, 0, 0, 0, 0, 0, 0],
        ];
        assert_eq!(grid(&mask, 8), expected);
    }

    #[test]
    fn polygon_clips_when_partly_outside_image() {
        // silx test_shapes "concave polygon partly outside mask" on a 8x6 mask.
        let vertices = [(-1, -1), (4, 3), (1, 5), (2, 3)];
        let mask = polygon_fill_mask(&vertices, 8, 6);
        let expected = vec![
            vec![1, 0, 0, 0, 0, 0],
            vec![0, 1, 0, 0, 0, 0],
            vec![0, 0, 1, 1, 1, 0],
            vec![0, 0, 0, 1, 0, 0],
            vec![0, 0, 0, 0, 0, 0],
            vec![0, 0, 0, 0, 0, 0],
            vec![0, 0, 0, 0, 0, 0],
            vec![0, 0, 0, 0, 0, 0],
        ];
        assert_eq!(grid(&mask, 6), expected);
    }

    #[test]
    fn polygon_surrounding_mask_fills_nothing_on_xor_balance() {
        // silx test_shapes "polygon surrounding mask": self-intersecting
        // bounding strip leaves the 6x6 mask empty under the xor scan.
        let vertices = [
            (-1, -1),
            (-1, 7),
            (7, 7),
            (7, -1),
            (0, -1),
            (8, -2),
            (8, 8),
            (-2, 8),
        ];
        let mask = polygon_fill_mask(&vertices, 6, 6);
        assert!(
            mask.iter().all(|&b| !b),
            "surrounding polygon must be empty"
        );
    }

    #[test]
    fn circle_fill_radius_boundary_is_strict() {
        // silx circle_fill: radius 1 yields only the center (strict `<`).
        let (rows, cols) = circle_fill(0, 0, 1.0);
        assert_eq!((rows, cols), (vec![0], vec![0]));

        // radius 1.5 yields the full 3x3 neighbourhood (silx square3x3).
        let (rows, cols) = circle_fill(0, 0, 1.5);
        let expected_rows = vec![-1, -1, -1, 0, 0, 0, 1, 1, 1];
        let expected_cols = vec![-1, 0, 1, -1, 0, 1, -1, 0, 1];
        assert_eq!(rows, expected_rows);
        assert_eq!(cols, expected_cols);
    }

    #[test]
    fn ellipse_fill_point_and_extent() {
        // silx ellipse_fill testPoint: radii (1,1) at center yields the center.
        let (rows, cols) = ellipse_fill(1, 1, 1.0, 1.0);
        assert_eq!((rows, cols), (vec![1], vec![1]));

        // silx ellipse_fill testEllipse: (0,0,20,10) has 617 interior points
        // with the row extent wider than the column extent.
        let (rows, cols) = ellipse_fill(0, 0, 20.0, 10.0);
        assert_eq!(rows.len(), 617);
        assert_eq!(cols.len(), 617);
        let row_extent = rows.iter().max().unwrap() - rows.iter().min().unwrap();
        let col_extent = cols.iter().max().unwrap() - cols.iter().min().unwrap();
        assert!(
            row_extent > col_extent,
            "row radius 20 must span wider than col radius 10"
        );
    }
}
