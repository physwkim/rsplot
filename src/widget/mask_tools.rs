use std::io::{self, Read, Write};

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

/// Number of vertices on the pencil brush preview circle, mirroring silx
/// `DrawFreeHand._circle` (`PlotInteraction.py:996`, `numpy.arange(13.0)`).
pub(crate) const PENCIL_PREVIEW_SEGMENTS: usize = 13;

/// The data-space vertices of the pencil brush footprint preview: `segments`
/// points on a circle of `radius` around `center`. Mirrors silx
/// `DrawFreeHand`'s `_circle` (`PlotInteraction.py:996-998`): 13 points on a
/// circle of radius `pencil width * 0.5`, painted unfilled at the cursor. The
/// mask brush paints a disk of `brush_size / 2` cells, so a `radius` of
/// `brush_size / 2` (egui-silx masks in data==cell space) matches the
/// footprint.
pub(crate) fn pencil_preview_circle(
    center: (f64, f64),
    radius: f64,
    segments: usize,
) -> Vec<(f64, f64)> {
    (0..segments)
        .map(|i| {
            let a = i as f64 * std::f64::consts::TAU / segments as f64;
            (center.0 + radius * a.cos(), center.1 + radius * a.sin())
        })
        .collect()
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
///
/// Shared with the scatter mask (1D per-point buffer) via crate visibility;
/// the snapshot type is an opaque `Vec<u8>` so the same machinery serves both
/// the 2D image mask and the 1D scatter mask.
pub(crate) struct MaskHistory {
    history: Vec<Vec<u8>>,
    redo: Vec<Vec<u8>>,
    depth: usize,
}

impl MaskHistory {
    /// Create a history seeded with `mask` as the single baseline snapshot.
    ///
    /// Mirrors silx `resetHistory` after construction: `_history = [mask]`,
    /// `_redo = []`.
    pub(crate) fn new(mask: &[u8]) -> Self {
        Self {
            history: vec![mask.to_vec()],
            redo: Vec::new(),
            depth: DEFAULT_HISTORY_DEPTH,
        }
    }

    /// Reset the history to a single baseline snapshot of `mask`.
    ///
    /// Mirrors silx `BaseMask.resetHistory`.
    pub(crate) fn reset(&mut self, mask: &[u8]) {
        self.history = vec![mask.to_vec()];
        self.redo.clear();
    }

    /// Append `mask` to the history if it represents a new state.
    ///
    /// Mirrors silx `BaseMask.commit`: commits when the redo stack is
    /// non-empty (a new action invalidates redo) or when `mask` differs from
    /// the last snapshot. The redo stack is cleared on commit, and the
    /// history is trimmed from the front to at most `depth` snapshots.
    pub(crate) fn commit(&mut self, mask: &[u8]) {
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
    pub(crate) fn undo(&mut self) -> Option<Vec<u8>> {
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
    pub(crate) fn redo(&mut self) -> Option<Vec<u8>> {
        if let Some(snapshot) = self.redo.pop() {
            self.history.push(snapshot.clone());
            Some(snapshot)
        } else {
            None
        }
    }

    /// Whether an undo is currently possible (silx `sigUndoable`).
    pub(crate) fn can_undo(&self) -> bool {
        self.history.len() > 1
    }

    /// Whether a redo is currently possible (silx `sigRedoable`).
    pub(crate) fn can_redo(&self) -> bool {
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
    /// Base overlay RGB applied to every mask level without a per-level
    /// override (silx `_defaultOverlayColor`, default `rgba("gray")`). Only the
    /// RGB channels drive the overlay color; the per-level alpha is computed by
    /// the LUT rule ([`alpha`](Self::alpha) / the selected-level highlight), so
    /// the alpha byte of this color is ignored.
    pub color: Color32,

    /// Current mask level edited by the drawing tools (silx `levelSpinBox`,
    /// range 1..=255). Also the highlighted level: it gets full
    /// [`alpha`](Self::alpha) in the overlay, every other masked level gets
    /// `alpha / 2` (silx `_setMaskColors`).
    pub level: u8,

    /// Overlay transparency in `[0, 1]` (silx `transparencySlider.value() /
    /// maximum()`, default `0.8` = the silx slider's default `8/10`). The
    /// selected level renders at this alpha; other masked levels at half.
    pub alpha: f32,

    /// Per-level color overrides (silx `_overlayColors` gated by
    /// `_defaultColors`). `overrides[i] == Some(rgb)` gives level `i` a
    /// distinct color; `None` falls back to [`color`](Self::color). Length is
    /// always 256 (silx `_maxLevelNumber + 1`).
    overrides: Vec<Option<[u8; 3]>>,

    pub active_tool: MaskTool,
    pub brush_size: u32,

    history: MaskHistory,
    mask_handle: Option<ItemHandle>,
    is_dirty: bool,
    /// Last array cell `(row, col)` painted in the current pencil/eraser
    /// stroke, or `None` between strokes. Mirrors silx `_lastPencilPos`: it
    /// anchors the interpolating line so a fast drag leaves no gap, and is
    /// cleared when the stroke finishes or the image geometry changes.
    last_pencil_pos: Option<(i64, i64)>,
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
            // silx `_defaultOverlayColor = rgba("gray")`, which silx defines as
            // `#a0a0a4` = (160, 160, 164) (gui/colors.py:71; `#808080` is the
            // commented-out `darkGray`, NOT silx's "gray"). Only the RGB drives
            // the overlay color; the alpha is computed by the LUT rule.
            color: Color32::from_rgb(160, 160, 164),
            level: 1,
            // silx transparencySlider default 8/10 = 0.8.
            alpha: 0.8,
            // silx `_defaultColors` all True -> no per-level override yet.
            overrides: vec![None; 256],
            active_tool: MaskTool::None,
            brush_size: 1,
            history,
            mask_handle: None,
            is_dirty: true, // Force initial upload
            last_pencil_pos: None,
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
        // The previous stroke anchor refers to the old geometry; drop it so the
        // next stroke does not interpolate from a stale cell.
        self.last_pencil_pos = None;
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

    /// Set the overlay transparency (silx `transparencySlider` -> alpha).
    ///
    /// `alpha` is clamped to `[0, 1]`. Mirrors silx `_updateColors` passing
    /// `transparencySlider.value() / maximum()` to `_setMaskColors`: the
    /// selected level renders at this alpha, other masked levels at half.
    pub fn set_transparency(&mut self, alpha: f32) {
        self.alpha = alpha.clamp(0.0, 1.0);
        self.is_dirty = true;
    }

    /// Set the overlay color of one mask level, or of all levels.
    ///
    /// Mirrors silx `setMaskColors(rgb, level)`
    /// (gui/plot/_BaseMaskToolsWidget.py:1026-1042): `level = None` sets the
    /// override for every level (silx lines 1036-1037, `_overlayColors[:] = rgb`
    /// with `_defaultColors[:] = False`); `level = Some(l)` sets only level `l`
    /// (silx lines 1039-1040).
    pub fn set_mask_colors(&mut self, rgb: [u8; 3], level: Option<u8>) {
        match level {
            None => self.overrides.iter_mut().for_each(|c| *c = Some(rgb)),
            Some(l) => self.overrides[l as usize] = Some(rgb),
        }
        self.is_dirty = true;
    }

    /// Reset one mask level's color override, or all of them, back to the base
    /// overlay color.
    ///
    /// Mirrors silx `resetMaskColors(level=None)`
    /// (gui/plot/_BaseMaskToolsWidget.py:1012-1023): `level = None` clears every
    /// override (`_defaultColors[:] = True`); `level = Some(l)` clears only that
    /// level (`_defaultColors[l] = True`). Either way the affected level(s) fall
    /// back to [`color`](Self::color). Symmetric with
    /// [`set_mask_colors`](Self::set_mask_colors)'s `Option<u8>` level.
    pub fn reset_mask_colors(&mut self, level: Option<u8>) {
        match level {
            None => self.overrides.iter_mut().for_each(|c| *c = None),
            Some(l) => self.overrides[l as usize] = None,
        }
        self.is_dirty = true;
    }

    /// Apply the mask onto a `Plot2D`.
    ///
    /// This should be called every frame after handling interaction,
    /// so the mask visual overlay stays up-to-date.
    ///
    /// The overlay is rendered as direct per-pixel RGBA: each mask level is
    /// mapped through the silx 256-entry mask LUT
    /// ([`crate::core::colormap::mask_overlay_lut`], faithful to
    /// `_BaseMaskToolsWidget._setMaskColors`) and uploaded via the RGBA image
    /// path. Level 0 is transparent, the selected level gets full alpha, other
    /// masked levels half. The overlay z is set one above the active image
    /// (silx `MaskToolsWidget.py:482`, `z = activeImage.getZValue() + 1`).
    pub fn apply(&mut self, plot: &mut Plot2D) {
        if !self.is_dirty {
            return;
        }

        // sRGB byte -> silx float for the base overlay color (silx
        // `_defaultOverlayColor`, RGB only; alpha is set by the LUT rule).
        let srgba = self.color.to_srgba_unmultiplied();
        let base_rgb = [
            srgba[0] as f32 / 255.0,
            srgba[1] as f32 / 255.0,
            srgba[2] as f32 / 255.0,
        ];
        // Per-level overrides as silx-float RGB (silx `_overlayColors`).
        let overrides_f32: Vec<Option<[f32; 3]>> = self
            .overrides
            .iter()
            .map(|c| {
                c.map(|rgb| {
                    [
                        rgb[0] as f32 / 255.0,
                        rgb[1] as f32 / 255.0,
                        rgb[2] as f32 / 255.0,
                    ]
                })
            })
            .collect();

        let lut = crate::core::colormap::mask_overlay_lut(
            base_rgb,
            &overrides_f32,
            self.level,
            self.alpha,
        );
        let rgba = mask_overlay_rgba(&self.mask, &lut);

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
            // New handle: add the resolved per-level RGBA as a Mask-kind item.
            if let Ok(handle) = plot.add_rgba_mask(self.width, self.height, &rgba) {
                self.mask_handle = Some(handle);
            }
        }

        // silx MaskToolsWidget.py:482 `z = activeImage.getZValue() + 1`: draw
        // the overlay one layer above the active image (silx default _z = 1
        // when there is no active image).
        if let Some(handle) = self.mask_handle {
            let z = overlay_z_value(plot.active_image().map(|img| plot.item_z_value(img)));
            plot.set_item_z(handle, z);
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

    /// Handle pointer interaction from the plot response to paint/erase the
    /// mask along a pencil stroke.
    ///
    /// Mirrors the silx pencil drag (`MaskToolsWidget.py:848-876`): while the
    /// primary button is held, the pointer is converted to an array cell and
    /// fed to [`Self::paint_pencil_point`], which interpolates a thick line
    /// from the previous sample (so a fast drag leaves no gap) and stamps a
    /// disk at the point. Releasing the button ends the stroke
    /// ([`Self::end_pencil_stroke`]) so the next stroke starts fresh.
    pub fn handle_interaction(&mut self, plot_response: &PlotResponse) {
        if !matches!(self.active_tool, MaskTool::Pencil | MaskTool::Eraser) {
            // Not in a drawing tool: drop any in-progress stroke so re-entering
            // a drawing tool does not connect to a stale position.
            self.end_pencil_stroke();
            return;
        }

        let response = &plot_response.response;
        let primary = egui::PointerButton::Primary;
        let drawing = response.dragged_by(primary) || response.clicked_by(primary);
        // egui reports the release on its own frame (`drag_stopped_by`) with the
        // final pointer position still available; paint that last sample too,
        // matching silx drawing the point on the `drawingFinished` event.
        let finished = response.drag_stopped_by(primary) || response.clicked_by(primary);

        if (drawing || finished)
            && let Some(pointer_pos) = response.interact_pointer_pos()
        {
            let (data_x, data_y) = plot_response.transform.pixel_to_data(pointer_pos);
            // Pencil masks the current level; eraser unmasks it (silx
            // `_isMasking()`), both routed through the shared mask primitives.
            let do_mask = self.active_tool == MaskTool::Pencil;
            self.paint_pencil_point(data_y.floor() as i64, data_x.floor() as i64, do_mask);
        }

        if finished {
            self.end_pencil_stroke();
        }
    }

    /// Paint one pencil/eraser sample at array cell `(row, col)`, interpolating
    /// from the previous sample of the current stroke so a fast drag leaves no
    /// gap.
    ///
    /// Mirrors the silx pencil drag body (`MaskToolsWidget.py:856-870`): when
    /// the cell differs from the last one, draw a thick Bresenham line from the
    /// previous sample (silx `updateLine`, width = brush size) — skipped on the
    /// first sample — then a disk of radius `brush_size / 2` at the point (silx
    /// `updateDisk`). `do_mask` masks (pencil) or unmasks the current level
    /// (eraser). Both go through [`Self::update_line`] / [`Self::update_disk`],
    /// so on-plot painting shares one faithful implementation with the shape
    /// tools instead of an inline brush.
    fn paint_pencil_point(&mut self, row: i64, col: i64, do_mask: bool) {
        if self.last_pencil_pos == Some((row, col)) {
            return;
        }
        let level = self.level;
        if let Some((last_row, last_col)) = self.last_pencil_pos {
            self.update_line(
                level,
                (last_row, last_col),
                (row, col),
                self.brush_size as i64,
                do_mask,
            );
        }
        self.update_disk(level, row, col, self.brush_size as f32 / 2.0, do_mask);
        self.last_pencil_pos = Some((row, col));
    }

    /// End the current pencil stroke so the next painted sample starts a fresh
    /// line. Mirrors silx resetting `_lastPencilPos` to `None` on
    /// `drawingFinished`.
    fn end_pencil_stroke(&mut self) {
        self.last_pencil_pos = None;
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

    /// Mask every pixel whose `data` value is not finite (NaN or +/-infinity)
    /// at the current level.
    ///
    /// Mirrors silx `_BaseMaskToolsWidget.updateNotFinite`
    /// (gui/plot/_BaseMaskToolsWidget.py:296-304):
    /// `updateStencil(level, ~numpy.isfinite(values))`. Finite values are left
    /// untouched. `data` is row-major and must be the same length as the mask.
    pub fn mask_not_finite(&mut self, data: &[f32]) {
        let stencil: Vec<bool> = data.iter().map(|&v| !v.is_finite()).collect();
        self.update_stencil(self.level, &stencil, true);
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

    /// Write the current mask as a 2D `uint8` NumPy `.npy` array.
    ///
    /// Mirrors silx `MaskToolsWidget.save(filename, "npy")` (`numpy.save` of
    /// the `uint8` mask, gui/plot/MaskToolsWidget.py:122-126). The array shape
    /// is `(height, width)` in C (row-major) order; for `uint8` the byte order
    /// is irrelevant (`descr: '|u1'`).
    pub fn write_npy(&self, w: &mut impl Write) -> io::Result<()> {
        write_npy_u8(w, self.height, self.width, &self.mask)
    }

    /// Read a 2D `uint8` `.npy` mask and apply it, cropping or padding to the
    /// current image geometry.
    ///
    /// Mirrors silx `MaskToolsWidget.load(filename)` for the npy branch
    /// (gui/plot/MaskToolsWidget.py:600-628) feeding `setSelectionMask`
    /// (lines 350-368): if the loaded shape matches the current image it is
    /// used as-is, otherwise it is cropped/padded into a zero buffer of the
    /// current shape (`resizedMask[:h, :w] = mask[:h, :w]`). The mask is
    /// committed to the undo history. Returns `Ok(true)` when the loaded
    /// shape differed from the current image (silx raises `RuntimeWarning`),
    /// `Ok(false)` when it matched.
    pub fn read_npy(&mut self, r: impl Read) -> io::Result<bool> {
        let (height, width, data) = read_npy_u8(r)?;
        let resized = height != self.height || width != self.width;
        if resized {
            // silx crop/pad: zero buffer of current shape, copy the overlap.
            let mut buf = vec![0u8; (self.width as usize) * (self.height as usize)];
            let copy_h = self.height.min(height) as usize;
            let copy_w = self.width.min(width) as usize;
            for r in 0..copy_h {
                let dst = r * self.width as usize;
                let src = r * width as usize;
                buf[dst..dst + copy_w].copy_from_slice(&data[src..src + copy_w]);
            }
            self.mask = buf;
        } else {
            self.mask = data;
        }
        self.commit();
        self.is_dirty = true;
        Ok(resized)
    }

    /// Save the current mask to a `.npy` file.
    ///
    /// File wrapper over [`write_npy`](Self::write_npy); see it for the format.
    pub fn save_npy(&self, path: impl AsRef<std::path::Path>) -> io::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut writer = io::BufWriter::new(file);
        self.write_npy(&mut writer)?;
        writer.flush()
    }

    /// Load a mask from a `.npy` file, cropping/padding to the current image.
    ///
    /// File wrapper over [`read_npy`](Self::read_npy); returns `Ok(true)` when
    /// the loaded shape differed from the current image (resize occurred).
    pub fn load_npy(&mut self, path: impl AsRef<std::path::Path>) -> io::Result<bool> {
        let file = std::fs::File::open(path)?;
        let reader = io::BufReader::new(file);
        self.read_npy(reader)
    }

    /// Save the current mask to a `.npy` file at the given in-app path string
    /// (silx `MaskToolsWidget.save(filename, "npy")`).
    ///
    /// Takes a plain `&str` path entered in-app rather than opening a native
    /// file dialog. The `.npy` bytes are produced by the single-owner codec
    /// [`crate::render::save::encode_mask_npy`].
    pub fn save_mask_npy(&self, path: &str) -> io::Result<()> {
        self.save_npy(path)
    }

    /// Load a mask from a `.npy` file at the given in-app path string, cropping
    /// or padding to the current image geometry (silx
    /// `MaskToolsWidget.load(filename)`, npy branch).
    ///
    /// Takes a plain `&str` path entered in-app rather than opening a native
    /// file dialog. Returns `Ok(true)` when the loaded shape differed from the
    /// current image (a resize occurred). The bytes are decoded by the
    /// single-owner codec [`crate::render::save::decode_mask_npy`].
    pub fn load_mask_npy(&mut self, path: &str) -> io::Result<bool> {
        self.load_npy(path)
    }
}

/// Map each mask level through the 256-entry overlay LUT to per-pixel RGBA.
///
/// Pure index lookup `rgba[i] = lut[mask[i]]`, faithful to silx's discrete
/// mask colormap (each `uint8` level indexes the LUT exactly, with no
/// interpolation between neighbouring levels). Level 0 yields the LUT's
/// transparent entry; the selected level yields its full-alpha entry.
fn mask_overlay_rgba(mask: &[u8], lut: &[[u8; 4]; 256]) -> Vec<[u8; 4]> {
    mask.iter().map(|&level| lut[level as usize]).collect()
}

/// The z-value for the mask overlay: one layer above the active image, or the
/// silx default `1` when there is no active image.
///
/// Faithful to silx `MaskToolsWidget.py:482` (`z = activeImage.getZValue() +
/// 1`) with the no-active-image default `_z = 1` (`MaskToolsWidget.py:285`).
/// Factored out of [`MaskToolsWidget::apply`] so the rule is the single source
/// of truth and is unit-testable without a GPU (`apply` itself needs a
/// [`Plot2D`], hence a render device, so its `set_item_z` wiring is not).
fn overlay_z_value(active_image_z: Option<f32>) -> f32 {
    active_image_z.unwrap_or(0.0) + 1.0
}

/// Write a 2D `uint8` array `(height, width)` in NumPy `.npy` v1.0 format.
///
/// Thin adapter over the single-owner codec [`crate::render::save::encode_mask_npy`]
/// (which holds the byte-format details) for the streaming `impl Write` API.
fn write_npy_u8(w: &mut impl Write, height: u32, width: u32, data: &[u8]) -> io::Result<()> {
    w.write_all(&crate::render::save::encode_mask_npy(height, width, data))
}

/// Read a 2D `uint8` array from NumPy `.npy` format, returning
/// `(height, width, data)` in C (row-major) order.
///
/// Thin adapter over the single-owner codec
/// [`crate::render::save::decode_mask_npy`] for the streaming `impl Read` API:
/// the bytes are slurped into memory (mask files are small) and decoded there.
fn read_npy_u8(mut r: impl Read) -> io::Result<(u32, u32, Vec<u8>)> {
    let mut bytes = Vec::new();
    r.read_to_end(&mut bytes)?;
    crate::render::save::decode_mask_npy(&bytes)
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
    fn pencil_preview_circle_lies_on_radius_around_center() {
        // silx DrawFreeHand._circle: `segments` points on a circle of `radius`
        // around the cursor, first vertex at angle 0 (PlotInteraction.py:996-998).
        let c = (3.0, -2.0);
        let r = 2.5;
        let pts = pencil_preview_circle(c, r, PENCIL_PREVIEW_SEGMENTS);
        assert_eq!(pts.len(), PENCIL_PREVIEW_SEGMENTS);
        for (x, y) in &pts {
            let d = ((x - c.0).powi(2) + (y - c.1).powi(2)).sqrt();
            assert!((d - r).abs() < 1e-9, "point ({x},{y}) dist {d}");
        }
        // First point at angle 0 is (center.x + r, center.y).
        assert!((pts[0].0 - (c.0 + r)).abs() < 1e-9, "first.x {}", pts[0].0);
        assert!((pts[0].1 - c.1).abs() < 1e-9, "first.y {}", pts[0].1);
    }

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
    fn pencil_stroke_interpolates_between_fast_drag_samples() {
        // A pencil drag that jumps from cell (0,0) to (3,3) in one frame must
        // interpolate: paint_pencil_point draws update_line from the previous
        // sample so the diagonal is continuous, unlike a per-frame point brush
        // that would leave (1,1) and (2,2) unmasked. (Mirrors silx updateLine
        // when _lastPencilPos != current, MaskToolsWidget.py:856-869.)
        let mut w = MaskToolsWidget::new(4, 4);
        w.level = 1;
        w.active_tool = MaskTool::Pencil;
        // First sample: disk only (no previous anchor).
        w.paint_pencil_point(0, 0, true);
        // Second sample, far jump: line (0,0)->(3,3) fills the diagonal.
        w.paint_pencil_point(3, 3, true);
        let expected: Vec<u8> = vec![
            1, 0, 0, 0, //
            0, 1, 0, 0, //
            0, 0, 1, 0, //
            0, 0, 0, 1, //
        ];
        assert_eq!(w.mask, expected);
    }

    #[test]
    fn ending_a_stroke_prevents_connecting_to_the_next() {
        // After a stroke ends (button released / new click), the next sample
        // must not draw a line back to the previous stroke's last cell. Width-1
        // column: paint row 0, end the stroke, paint row 3 -> rows 1 and 2 stay
        // unmasked (a fresh stroke), whereas an un-reset anchor would fill them.
        // (Mirrors silx resetting _lastPencilPos to None on drawingFinished.)
        let mut w = MaskToolsWidget::new(1, 4); // width 1, height 4
        w.level = 1;
        w.active_tool = MaskTool::Pencil;
        w.paint_pencil_point(0, 0, true);
        w.end_pencil_stroke();
        w.paint_pencil_point(3, 0, true);
        assert_eq!(w.mask, vec![1, 0, 0, 1]);
    }

    #[test]
    fn mask_not_finite_masks_nan_and_infinities_only() {
        // silx updateNotFinite: ~isfinite masks NaN, +inf, -inf; finite values
        // (including 0.0 and very large/small finite values) are untouched.
        let mut w = MaskToolsWidget::new(6, 1);
        w.level = 1;
        let data = [
            0.0_f32,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::MAX,
            -1.5,
        ];
        w.mask_not_finite(&data);
        assert_eq!(w.mask, vec![0, 1, 1, 1, 0, 0]);
    }

    #[test]
    fn mask_not_finite_uses_current_level() {
        // The mask is written at the widget's current level, not always 1.
        let mut w = MaskToolsWidget::new(2, 1);
        w.level = 7;
        let data = [f32::NAN, 3.0];
        w.mask_not_finite(&data);
        assert_eq!(w.mask, vec![7, 0]);
    }

    #[test]
    fn npy_round_trips_through_memory() {
        // Save the mask to an in-memory buffer, then load it into a fresh
        // widget of the same shape: same-shape load returns false (no resize)
        // and the mask is bit-identical.
        let mut src = MaskToolsWidget::new(3, 2); // width 3, height 2
        src.mask = vec![0, 1, 2, 3, 4, 5];

        let mut buf = Vec::new();
        src.write_npy(&mut buf).unwrap();

        // The preamble (magic..newline) must be a multiple of 64 bytes, then 6
        // data bytes follow.
        assert_eq!(&buf[0..6], b"\x93NUMPY");
        assert_eq!(buf.len() % 64, 6);

        let mut dst = MaskToolsWidget::new(3, 2);
        let resized = dst.read_npy(buf.as_slice()).unwrap();
        assert!(!resized, "same shape must not report a resize");
        assert_eq!(dst.mask, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn npy_load_crops_larger_mask() {
        // Loaded mask 3x3 into a 2x2 widget: crop to top-left 2x2, report a
        // resize. silx setSelectionMask: resizedMask[:h, :w] = mask[:h, :w].
        let mut big = MaskToolsWidget::new(3, 3);
        big.mask = vec![
            1, 2, 3, //
            4, 5, 6, //
            7, 8, 9, //
        ];
        let mut buf = Vec::new();
        big.write_npy(&mut buf).unwrap();

        let mut small = MaskToolsWidget::new(2, 2);
        let resized = small.read_npy(buf.as_slice()).unwrap();
        assert!(resized, "shape mismatch must report a resize");
        // Top-left 2x2 of the 3x3 source.
        assert_eq!(small.mask, vec![1, 2, 4, 5]);
    }

    #[test]
    fn npy_load_pads_smaller_mask() {
        // Loaded mask 2x2 into a 3x3 widget: pad with zeros, report a resize.
        let mut small = MaskToolsWidget::new(2, 2);
        small.mask = vec![1, 2, 3, 4];
        let mut buf = Vec::new();
        small.write_npy(&mut buf).unwrap();

        let mut big = MaskToolsWidget::new(3, 3);
        let resized = big.read_npy(buf.as_slice()).unwrap();
        assert!(resized);
        assert_eq!(
            big.mask,
            vec![
                1, 2, 0, //
                3, 4, 0, //
                0, 0, 0, //
            ]
        );
    }

    #[test]
    fn npy_load_rejects_non_uint8_dtype() {
        // A header advertising float64 ('<f8') is rejected: silx masks are u8.
        let header = b"{'descr': '<f8', 'fortran_order': False, 'shape': (1, 1), }";
        let mut buf = Vec::new();
        buf.extend_from_slice(b"\x93NUMPY");
        buf.extend_from_slice(&[1u8, 0u8]);
        buf.extend_from_slice(&(header.len() as u16).to_le_bytes());
        buf.extend_from_slice(header);
        // 8 bytes of body (one f64), never reached because dtype check fails.
        buf.extend_from_slice(&[0u8; 8]);

        let mut w = MaskToolsWidget::new(1, 1);
        let err = w.read_npy(buf.as_slice()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn npy_load_commits_history() {
        // A successful load commits a snapshot so it can be undone.
        let mut src = MaskToolsWidget::new(2, 1);
        src.mask = vec![1, 1];
        let mut buf = Vec::new();
        src.write_npy(&mut buf).unwrap();

        let mut dst = MaskToolsWidget::new(2, 1);
        assert!(!dst.can_undo());
        dst.read_npy(buf.as_slice()).unwrap();
        assert_eq!(dst.mask, vec![1, 1]);
        assert!(dst.can_undo(), "load must commit to history");
    }

    #[test]
    fn save_mask_npy_then_load_mask_npy_round_trips_via_path_string() {
        // The in-app path-string API round-trips a small mask through a real
        // file: save_mask_npy writes the .npy, load_mask_npy reads it back into
        // a fresh same-shape widget bit-identically (no resize).
        let mut src = MaskToolsWidget::new(3, 2); // width 3, height 2
        src.mask = vec![0, 1, 2, 200, 254, 255];

        let mut path = std::env::temp_dir();
        path.push(format!(
            "egui_silx_mask_roundtrip_{}.npy",
            std::process::id()
        ));
        let path_str = path.to_str().expect("utf-8 temp path").to_string();

        src.save_mask_npy(&path_str).expect("save");
        let mut dst = MaskToolsWidget::new(3, 2);
        let resized = dst.load_mask_npy(&path_str).expect("load");
        assert!(!resized, "same shape must not report a resize");
        assert_eq!(dst.mask, vec![0, 1, 2, 200, 254, 255]);

        // The on-disk bytes are exactly what the single-owner encoder produces.
        let on_disk = std::fs::read(&path_str).expect("read back file");
        let expected = crate::render::save::encode_mask_npy(2, 3, &src.mask);
        assert_eq!(on_disk, expected);

        let _ = std::fs::remove_file(&path_str);
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

    #[test]
    fn default_overlay_color_is_silx_gray() {
        // silx `_defaultOverlayColor = rgba("gray")` = `#a0a0a4` = (160,160,164)
        // (gui/colors.py:71), opaque.
        let w = MaskToolsWidget::new(2, 2);
        assert_eq!(w.color, Color32::from_rgb(160, 160, 164));
        // silx transparencySlider default 8/10.
        assert_eq!(w.alpha, 0.8);
        // silx `_defaultColors` all True -> no per-level override.
        assert_eq!(w.overrides.len(), 256);
        assert!(w.overrides.iter().all(|c| c.is_none()));
    }

    #[test]
    fn mask_overlay_rgba_maps_each_level_through_lut() {
        // The overlay is a direct LUT index: level 0 -> transparent, the
        // selected level -> full alpha, other masked levels -> alpha / 2.
        // base gray = silx `rgba("gray")` = `#a0a0a4` (160,160,164), alpha 0.8,
        // selected level 1.
        let lut = crate::core::colormap::mask_overlay_lut(
            [160.0 / 255.0, 160.0 / 255.0, 164.0 / 255.0],
            &[],
            1,
            0.8,
        );
        let mask = vec![0u8, 1, 2, 5, 1, 0];
        let rgba = mask_overlay_rgba(&mask, &lut);
        assert_eq!(rgba.len(), mask.len());
        // level 0 -> transparent (silx line 1008).
        assert_eq!(rgba[0], [0, 0, 0, 0]);
        assert_eq!(rgba[5], [0, 0, 0, 0]);
        // selected level 1 -> full alpha (silx line 1005): 0.8 * 256 -> 204.
        assert_eq!(rgba[1], [160, 160, 164, 204]);
        assert_eq!(rgba[4], [160, 160, 164, 204]);
        // other masked levels -> alpha / 2 (silx line 1002): 0.4 * 256 -> 102.
        assert_eq!(rgba[2], [160, 160, 164, 102]);
        assert_eq!(rgba[3], [160, 160, 164, 102]);
        // Each pixel equals its LUT entry exactly (no interpolation).
        for (px, &level) in rgba.iter().zip(mask.iter()) {
            assert_eq!(*px, lut[level as usize]);
        }
    }

    #[test]
    fn set_mask_colors_and_transparency_feed_the_lut() {
        // The widget setters flow into the LUT the overlay is built from.
        let mut w = MaskToolsWidget::new(2, 2);
        // Per-level override at level 3 -> red (silx setMaskColors(rgb, 3)).
        w.set_mask_colors([255, 0, 0], Some(3));
        // All levels -> blue is overwritten below; first prove single-level set.
        let overrides_f32: Vec<Option<[f32; 3]>> = w
            .overrides
            .iter()
            .map(|c| {
                c.map(|rgb| {
                    [
                        rgb[0] as f32 / 255.0,
                        rgb[1] as f32 / 255.0,
                        rgb[2] as f32 / 255.0,
                    ]
                })
            })
            .collect();
        let lut = crate::core::colormap::mask_overlay_lut([0.5, 0.5, 0.5], &overrides_f32, 1, 0.8);
        assert_eq!(&lut[3][0..3], &[255, 0, 0]);

        // set_transparency clamps and marks the alpha used for the selected level.
        w.set_transparency(2.0);
        assert_eq!(w.alpha, 1.0);

        // set_mask_colors(None) sets every level (silx _overlayColors[:] = rgb).
        w.set_mask_colors([0, 0, 255], None);
        assert!(w.overrides.iter().all(|c| *c == Some([0, 0, 255])));

        // reset_mask_colors(Some(l)) clears only that level (silx
        // resetMaskColors(level)); the rest keep their override.
        w.reset_mask_colors(Some(7));
        assert_eq!(w.overrides[7], None);
        assert_eq!(w.overrides[6], Some([0, 0, 255]));

        // reset_mask_colors(None) clears every override (silx resetMaskColors()).
        w.reset_mask_colors(None);
        assert!(w.overrides.iter().all(|c| c.is_none()));
    }

    #[test]
    fn overlay_z_value_is_one_above_active_image() {
        // silx MaskToolsWidget.py:482 `z = activeImage.getZValue() + 1`: the
        // overlay sits one layer above the active image, whatever its z. This
        // exercises the actual helper `apply` calls (so a regression in the
        // `+1` or the no-active fallback is caught). The `apply` -> set_item_z
        // wiring is GPU-bound (Plot2D needs a RenderState/device) and so the
        // on-screen layering itself stays UNVERIFIED.
        assert_eq!(overlay_z_value(Some(3.0)), 4.0);
        assert_eq!(overlay_z_value(Some(-2.5)), -1.5);
        // No active image -> silx default _z = 1 (MaskToolsWidget.py:285).
        assert_eq!(overlay_z_value(None), 1.0);
    }
}
