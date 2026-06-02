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

/// Threshold masking mode, mirroring the silx threshold action group
/// (`belowThresholdAction` / `betweenThresholdAction` / `aboveThresholdAction`
/// in `_BaseMaskToolsWidget._initThresholdGroupBox`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThresholdMode {
    /// Mask where `data < min`.
    Below,
    /// Mask where `min <= data <= max`.
    Between,
    /// Mask where `data > max`.
    Above,
}

/// Default maximum number of mask snapshots kept for undo, matching silx
/// `BaseMask.historyDepth`.
const DEFAULT_HISTORY_DEPTH: usize = 10;

/// Bounded undo/redo history of mask snapshots, mirroring the silx
/// `BaseMask` history machinery (`_history` / `_redo` lists, `historyDepth`,
/// `commit` / `undo` / `redo`).
///
/// The `history` stack always holds at least one baseline snapshot once
/// [`reset`](Self::reset) has run; `undo` is possible only when more than one
/// snapshot is stored.
struct MaskHistory {
    history: Vec<Vec<u8>>,
    redo: Vec<Vec<u8>>,
    depth: usize,
}

impl MaskHistory {
    /// Create a history seeded with `mask` as the single baseline snapshot.
    ///
    /// Mirrors silx `resetHistory` after construction: `_history = [mask]`,
    /// `_redo = []`.
    fn new(mask: &[u8]) -> Self {
        Self {
            history: vec![mask.to_vec()],
            redo: Vec::new(),
            depth: DEFAULT_HISTORY_DEPTH,
        }
    }

    /// Reset the history to a single baseline snapshot of `mask`.
    ///
    /// Mirrors silx `BaseMask.resetHistory`.
    fn reset(&mut self, mask: &[u8]) {
        self.history = vec![mask.to_vec()];
        self.redo.clear();
    }

    /// Append `mask` to the history if it represents a new state.
    ///
    /// Mirrors silx `BaseMask.commit`: commits when the redo stack is
    /// non-empty (a new action invalidates redo) or when `mask` differs from
    /// the last snapshot. The redo stack is cleared on commit, and the
    /// history is trimmed from the front to at most `depth` snapshots.
    fn commit(&mut self, mask: &[u8]) {
        let differs = self.history.last().map(|last| last != mask).unwrap_or(true);
        if self.history.is_empty() || !self.redo.is_empty() || differs {
            self.redo.clear();
            // silx pops from the front while len >= depth, then appends, so
            // the post-append length is at most `depth`.
            while self.history.len() >= self.depth {
                self.history.remove(0);
            }
            self.history.push(mask.to_vec());
        }
    }

    /// Restore the previous snapshot, returning it, if any.
    ///
    /// Mirrors silx `BaseMask.undo`: requires more than one snapshot; the
    /// popped state is pushed onto the redo stack and the new last snapshot
    /// is returned.
    fn undo(&mut self) -> Option<Vec<u8>> {
        if self.history.len() > 1 {
            let popped = self.history.pop().expect("len > 1");
            self.redo.push(popped);
            Some(self.history.last().expect("len >= 1").clone())
        } else {
            None
        }
    }

    /// Restore the most recently undone snapshot, returning it, if any.
    ///
    /// Mirrors silx `BaseMask.redo`: pops the redo stack, pushes it back onto
    /// the history and returns it.
    fn redo(&mut self) -> Option<Vec<u8>> {
        if let Some(snapshot) = self.redo.pop() {
            self.history.push(snapshot.clone());
            Some(snapshot)
        } else {
            None
        }
    }

    /// Whether an undo is currently possible (silx `sigUndoable`).
    fn can_undo(&self) -> bool {
        self.history.len() > 1
    }

    /// Whether a redo is currently possible (silx `sigRedoable`).
    fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
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

    history: MaskHistory,
    mask_handle: Option<ItemHandle>,
    is_dirty: bool,
}

impl MaskToolsWidget {
    /// Create a new MaskToolsWidget for an image of the given dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        let mask = vec![0; (width * height) as usize];
        let history = MaskHistory::new(&mask);
        Self {
            mask,
            width,
            height,
            color: Color32::from_rgba_unmultiplied(255, 0, 0, 128), // Default semi-transparent red
            level: 1,
            active_tool: MaskTool::None,
            brush_size: 1,
            history,
            mask_handle: None,
            is_dirty: true, // Force initial upload
        }
    }

    /// Reset the mask to the given dimensions and clear it.
    ///
    /// Mirrors silx `reset(shape)`: a shape change resets the undo history.
    pub fn reset_geometry(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.mask = vec![0; (width * height) as usize];
        self.history.reset(&self.mask);
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

    /// Invert the current mask level over the image.
    ///
    /// `0` pixels become the current level and current-level pixels become
    /// `0`; pixels at other levels are left untouched. Mirrors silx
    /// `BaseMask.invert(level)`: it captures the `level` pixels first, then
    /// turns unmasked pixels into `level`, then clears the captured ones.
    pub fn invert(&mut self) {
        let level = self.level;
        for cell in &mut self.mask {
            if *cell == 0 {
                *cell = level;
            } else if *cell == level {
                *cell = 0;
            }
        }
        self.is_dirty = true;
    }

    /// Commit the current mask to the undo history.
    ///
    /// Mirrors silx `BaseMask.commit`: call once per completed mask
    /// operation. A snapshot is stored only if the mask changed (or a redo
    /// was pending), and the history is bounded to the default depth (10).
    pub fn commit(&mut self) {
        self.history.commit(&self.mask);
    }

    /// Restore the previous mask snapshot, if any.
    ///
    /// Mirrors silx `BaseMask.undo`. Returns `true` if an undo was applied.
    pub fn undo(&mut self) -> bool {
        if let Some(snapshot) = self.history.undo() {
            self.mask = snapshot;
            self.is_dirty = true;
            true
        } else {
            false
        }
    }

    /// Restore the most recently undone mask snapshot, if any.
    ///
    /// Mirrors silx `BaseMask.redo`. Returns `true` if a redo was applied.
    pub fn redo(&mut self) -> bool {
        if let Some(snapshot) = self.history.redo() {
            self.mask = snapshot;
            self.is_dirty = true;
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

            if ui
                .add_enabled(self.can_undo(), egui::Button::new("Undo"))
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(self.can_redo(), egui::Button::new("Redo"))
                .clicked()
            {
                self.redo();
            }
            if ui.button("Invert").clicked() {
                self.invert();
                self.commit();
            }
            if ui.button("Clear").clicked() {
                self.clear();
                self.commit();
            }
            if ui.button("Clear All").clicked() {
                self.clear_all();
                self.commit();
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

    /// Mask or unmask a thickened Bresenham line at the given level.
    ///
    /// Mirrors silx `ImageMask.updateLine` (gui/plot/MaskToolsWidget.py:261):
    /// `shapes.draw_line` then `updatePoints`. `from`/`to` are `(row, col)`
    /// endpoints, both inclusive, and the line is thickened to `width` pixels.
    pub fn update_line(
        &mut self,
        level: u8,
        from: (i64, i64),
        to: (i64, i64),
        width: i64,
        mask: bool,
    ) {
        let (rows, cols) = line_coords(from.0, from.1, to.0, to.1, width);
        self.update_points(level, &rows, &cols, mask);
    }

    /// Draw a thickened pencil line between two `(col, row)` cells at the
    /// current level, filling every cell the line crosses so a fast drag
    /// leaves no gaps.
    ///
    /// Mirrors the silx pencil drag path (`MaskToolsWidget.py:849-876`
    /// `updateLine` when `lastPencilPos != current`): a Bresenham line at the
    /// current pencil width. Coordinates are `(x, y) = (col, row)` to match the
    /// plot's data-coordinate ordering; `width` is the brush width in pixels.
    /// Always masks (sets the current level); use [`update_line`] with
    /// `mask = false` for the eraser.
    ///
    /// [`update_line`]: Self::update_line
    pub fn draw_line(&mut self, from: (i32, i32), to: (i32, i32), width: u32) {
        let (col0, row0) = (from.0 as i64, from.1 as i64);
        let (col1, row1) = (to.0 as i64, to.1 as i64);
        self.update_line(
            self.level,
            (row0, col0),
            (row1, col1),
            width.max(1) as i64,
            true,
        );
    }

    /// Mask/unmask pixels selected by a boolean stencil at the current level.
    ///
    /// Mirrors silx `BaseMask.updateStencil`: when `mask` is true, every
    /// stencil pixel is set to `level`; otherwise only pixels already at
    /// `level` and inside the stencil are cleared. `stencil` is row-major and
    /// must be the same length as the mask.
    pub fn update_stencil(&mut self, level: u8, stencil: &[bool], mask: bool) {
        for (idx, &selected) in stencil.iter().enumerate() {
            if selected {
                self.set_or_clear(idx, level, mask);
            }
        }
        self.is_dirty = true;
    }

    /// Mask/unmask pixels whose `data` value is below `threshold`.
    ///
    /// Mirrors silx `BaseMask.updateBelowThreshold` (`data < threshold`).
    pub fn update_below_threshold(&mut self, level: u8, data: &[f32], threshold: f32, mask: bool) {
        let stencil: Vec<bool> = data.iter().map(|&v| v < threshold).collect();
        self.update_stencil(level, &stencil, mask);
    }

    /// Mask/unmask pixels whose `data` value is within `[min, max]`.
    ///
    /// Mirrors silx `BaseMask.updateBetweenThresholds`
    /// (`min <= data <= max`, both bounds inclusive).
    pub fn update_between_thresholds(
        &mut self,
        level: u8,
        data: &[f32],
        min: f32,
        max: f32,
        mask: bool,
    ) {
        let stencil: Vec<bool> = data.iter().map(|&v| min <= v && v <= max).collect();
        self.update_stencil(level, &stencil, mask);
    }

    /// Mask/unmask pixels whose `data` value is above `threshold`.
    ///
    /// Mirrors silx `BaseMask.updateAboveThreshold` (`data > threshold`).
    pub fn update_above_threshold(&mut self, level: u8, data: &[f32], threshold: f32, mask: bool) {
        let stencil: Vec<bool> = data.iter().map(|&v| v > threshold).collect();
        self.update_stencil(level, &stencil, mask);
    }

    /// Apply a threshold mask at the current level over `data` for the given
    /// `mode`.
    ///
    /// Mirrors silx `_maskBtnClicked`: `Below` uses `min`, `Above` uses `max`,
    /// `Between` uses both bounds.
    pub fn update_threshold(&mut self, data: &[f32], mode: ThresholdMode, min: f32, max: f32) {
        match mode {
            ThresholdMode::Below => self.update_below_threshold(self.level, data, min, true),
            ThresholdMode::Between => {
                self.update_between_thresholds(self.level, data, min, max, true)
            }
            ThresholdMode::Above => self.update_above_threshold(self.level, data, max, true),
        }
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

/// Generate the `(rows, cols)` image coordinates of a line between two end
/// points, both inclusive, thickened to `width` pixels.
///
/// Faithful port of `silx.image.shapes.draw_line` (image/shapes.pyx:195): a
/// Bresenham line where width is handled by drawing `width` parallel pixels
/// along the minor axis, offset back by `(width - 1) / 2`. The degenerate
/// case (`from == to`) returns the single end point regardless of width,
/// matching silx. Coordinates may be negative.
pub fn line_coords(row0: i64, col0: i64, row1: i64, col1: i64, width: i64) -> (Vec<i64>, Vec<i64>) {
    let dcol = (col1 - col0).abs();
    let drow = (row1 - row0).abs();
    let invert_coords = dcol < drow;

    // silx: single point when both deltas are zero (width ignored).
    if dcol == 0 && drow == 0 {
        return (vec![row0], vec![col0]);
    }

    let width = width.max(1);

    // Set the major axis `a` and minor axis `b` per the segment's octant.
    // `a` is the driving axis, `b` is thickened by `width`.
    let (da, db, step_a, step_b, a0, b0);
    if !invert_coords {
        da = dcol;
        db = drow;
        step_a = if col1 > col0 { 1 } else { -1 };
        step_b = if row1 > row0 { 1 } else { -1 };
        a0 = col0;
        b0 = row0;
    } else {
        da = drow;
        db = dcol;
        step_a = if row1 > row0 { 1 } else { -1 };
        step_b = if col1 > col0 { 1 } else { -1 };
        a0 = row0;
        b0 = col0;
    }

    let count = (da + 1) as usize;
    let wsize = width as usize;
    // Row-major (index, offset) buffers, matching silx's (da+1, width) arrays.
    let mut a_coords = Vec::with_capacity(count * wsize);
    let mut b_coords = Vec::with_capacity(count * wsize);

    let mut a = a0;
    let mut b = b0 - (width - 1) / 2;
    let mut delta = 2 * db - da;
    for _ in 0..count {
        for offset in 0..width {
            b_coords.push(b + offset);
            a_coords.push(a);
        }
        if delta >= 0 {
            b += step_b;
            delta -= 2 * da;
        }
        a += step_a;
        delta += 2 * db;
    }

    if !invert_coords {
        // a is the column axis, b is the row axis.
        (b_coords, a_coords)
    } else {
        // a is the row axis, b is the column axis.
        (a_coords, b_coords)
    }
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

    #[test]
    fn undo_is_noop_with_only_baseline() {
        // silx undo requires len(history) > 1: a fresh widget cannot undo.
        let mut w = MaskToolsWidget::new(2, 2);
        assert!(!w.can_undo());
        assert!(!w.undo());
    }

    #[test]
    fn commit_without_change_adds_no_snapshot() {
        // silx commit only stores when the mask differs from the last snapshot.
        let mut w = MaskToolsWidget::new(2, 2);
        w.commit(); // mask unchanged from baseline
        assert!(!w.can_undo());
    }

    #[test]
    fn undo_then_redo_round_trips_one_change() {
        let mut w = MaskToolsWidget::new(2, 2);
        w.mask = vec![1, 0, 0, 0];
        w.commit();
        assert!(w.can_undo());

        assert!(w.undo());
        assert_eq!(w.mask, vec![0, 0, 0, 0]); // back to baseline
        assert!(!w.can_undo());
        assert!(w.can_redo());

        assert!(w.redo());
        assert_eq!(w.mask, vec![1, 0, 0, 0]); // change restored
        assert!(!w.can_redo());
    }

    #[test]
    fn new_commit_after_undo_clears_redo() {
        // silx commit resets the redo stack when a new action is performed.
        let mut w = MaskToolsWidget::new(2, 2);
        w.mask = vec![1, 0, 0, 0];
        w.commit();
        assert!(w.undo());
        assert!(w.can_redo());

        // A different change committed after the undo invalidates redo.
        w.mask = vec![2, 0, 0, 0];
        w.commit();
        assert!(!w.can_redo());
        assert!(!w.redo());
    }

    #[test]
    fn history_is_bounded_to_depth() {
        // silx historyDepth=10: history holds at most `depth` snapshots, so
        // the oldest are trimmed and undo walks back depth-1 states.
        let depth = DEFAULT_HISTORY_DEPTH;
        let mut w = MaskToolsWidget::new(1, 1);

        // Commit `depth + 5` distinct states (level 1..=depth+5).
        for level in 1..=(depth + 5) as u8 {
            w.mask = vec![level];
            w.commit();
        }
        let last = (depth + 5) as u8;
        assert_eq!(w.mask, vec![last]);

        // Undo as far as possible: exactly depth-1 steps remain in history.
        let mut undos = 0;
        while w.undo() {
            undos += 1;
        }
        assert_eq!(undos, depth - 1);
        // The oldest retained snapshot is `last - (depth - 1)`, not the
        // original baseline (which was trimmed off the front).
        assert_eq!(w.mask, vec![last - (depth as u8 - 1)]);
    }

    #[test]
    fn invert_swaps_zero_and_current_level_only() {
        // silx BaseMask.invert(level): 0 <-> level, other levels untouched.
        let mut w = MaskToolsWidget::new(4, 1);
        w.mask = vec![0, 1, 2, 0];
        w.level = 1;
        w.invert();
        // 0 -> 1, original 1 -> 0, level 2 stays.
        assert_eq!(w.mask, vec![1, 0, 2, 1]);
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
    fn threshold_below_is_strict() {
        // silx updateBelowThreshold: data < threshold (boundary value excluded).
        let mut w = MaskToolsWidget::new(4, 1);
        let data = [0.0_f32, 1.0, 2.0, 3.0];
        w.update_below_threshold(1, &data, 2.0, true);
        assert_eq!(w.mask, vec![1, 1, 0, 0]);
    }

    #[test]
    fn threshold_between_is_inclusive() {
        // silx updateBetweenThresholds: min <= data <= max (bounds included).
        let mut w = MaskToolsWidget::new(5, 1);
        let data = [0.0_f32, 1.0, 2.0, 3.0, 4.0];
        w.update_between_thresholds(1, &data, 1.0, 3.0, true);
        assert_eq!(w.mask, vec![0, 1, 1, 1, 0]);
    }

    #[test]
    fn threshold_above_is_strict() {
        // silx updateAboveThreshold: data > threshold (boundary value excluded).
        let mut w = MaskToolsWidget::new(4, 1);
        let data = [0.0_f32, 1.0, 2.0, 3.0];
        w.update_above_threshold(1, &data, 2.0, true);
        assert_eq!(w.mask, vec![0, 0, 0, 1]);
    }

    #[test]
    fn threshold_dispatch_maps_bounds_per_mode() {
        // Below -> min, Above -> max, Between -> both (silx _maskBtnClicked).
        let data = [0.0_f32, 1.0, 2.0, 3.0];

        let mut below = MaskToolsWidget::new(4, 1);
        below.update_threshold(&data, ThresholdMode::Below, 2.0, 99.0);
        assert_eq!(below.mask, vec![1, 1, 0, 0]);

        let mut above = MaskToolsWidget::new(4, 1);
        above.update_threshold(&data, ThresholdMode::Above, -99.0, 2.0);
        assert_eq!(above.mask, vec![0, 0, 0, 1]);

        let mut between = MaskToolsWidget::new(4, 1);
        between.update_threshold(&data, ThresholdMode::Between, 1.0, 2.0);
        assert_eq!(between.mask, vec![0, 1, 1, 0]);
    }

    #[test]
    fn threshold_unmask_only_clears_current_level() {
        // silx updateStencil unmask branch: clears pixels at `level` & stencil.
        let mut w = MaskToolsWidget::new(4, 1);
        w.mask = vec![1, 2, 1, 2];
        let data = [0.0_f32, 1.0, 2.0, 3.0];
        // Unmask level 1 below threshold 3: covers idx 0,1,2; only level-1 clear.
        w.update_below_threshold(1, &data, 3.0, false);
        assert_eq!(w.mask, vec![0, 2, 0, 2]);
    }

    #[test]
    fn line_coords_diagonal_hits_every_cell() {
        // silx draw_line: Bresenham diagonal includes both endpoints with no
        // gaps. (row, col) order for a 45-degree line is the identity diagonal.
        let (rows, cols) = line_coords(0, 0, 3, 3, 1);
        assert_eq!(rows, vec![0, 1, 2, 3]);
        assert_eq!(cols, vec![0, 1, 2, 3]);
    }

    #[test]
    fn line_coords_single_point_is_degenerate() {
        // silx draw_line: dcol == 0 and drow == 0 returns the single point,
        // width ignored.
        let (rows, cols) = line_coords(2, 5, 2, 5, 7);
        assert_eq!(rows, vec![2]);
        assert_eq!(cols, vec![5]);
    }

    #[test]
    fn line_coords_width_thickens_minor_axis() {
        // silx draw_line: width draws `width` parallel pixels offset back by
        // (width-1)/2 on the minor axis. A horizontal width-2 line covers two
        // adjacent rows along the whole span.
        let (rows, cols) = line_coords(1, 0, 1, 3, 2);
        assert_eq!(rows, vec![1, 2, 1, 2, 1, 2, 1, 2]);
        assert_eq!(cols, vec![0, 0, 1, 1, 2, 2, 3, 3]);
    }

    #[test]
    fn draw_line_fills_gap_left_by_a_fast_drag() {
        // The pencil drag interpolates between sampled positions so a jump from
        // (0,0) to (3,3) leaves no unmasked cells along the diagonal.
        let mut w = MaskToolsWidget::new(4, 4);
        w.level = 1;
        // from/to are (col, row) = (x, y).
        w.draw_line((0, 0), (3, 3), 1);
        let expected: Vec<u8> = vec![
            1, 0, 0, 0, //
            0, 1, 0, 0, //
            0, 0, 1, 0, //
            0, 0, 0, 1, //
        ];
        assert_eq!(w.mask, expected);
    }

    #[test]
    fn update_line_eraser_clears_only_current_level() {
        // silx updateLine with mask=False unmasks only cells already at level.
        let mut w = MaskToolsWidget::new(4, 4);
        // Pre-fill the diagonal with mixed levels: (0,0)->1, (1,1)->2, etc.
        w.mask[0] = 1; // (0,0)
        w.mask[5] = 2; // (1,1)
        w.mask[10] = 1; // (2,2)
        w.mask[15] = 1; // (3,3)
        // Erase level 1 along the diagonal: only the level-1 cells clear.
        w.update_line(1, (0, 0), (3, 3), 1, false);
        let expected: Vec<u8> = vec![
            0, 0, 0, 0, //
            0, 2, 0, 0, //
            0, 0, 0, 0, //
            0, 0, 0, 0, //
        ];
        assert_eq!(w.mask, expected);
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
