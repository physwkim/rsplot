use std::io::{self, Read, Write};

use egui::Color32;

use crate::core::backend::ItemHandle;
use crate::widget::high_level::Plot2D;
use crate::widget::interaction::{
    DrawEvent, DrawMode, DrawParams, DrawState, FillMode, SelectionStyle,
};
use crate::widget::plot_widget::{PlotResponse, feed_draw_state, paint_draw_preview};

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

impl MaskTool {
    /// The on-plot [`DrawMode`] this tool draws as a shape, or `None` for the
    /// brush / disabled tools (which paint per-pointer, not as a closed shape).
    /// Mirrors silx `MaskToolsWidget._drawingMode` for the rectangle / ellipse /
    /// polygon draw shapes. This is the single source of truth for *which* tools
    /// are shape draws: both the on-plot wiring
    /// ([`MaskToolsWidget::handle_shape_draw`]) and the caller's gate read it,
    /// so a tool becomes a shape draw by adding exactly one arm here.
    pub(crate) fn draw_mode(self) -> Option<DrawMode> {
        match self {
            MaskTool::Rectangle => Some(DrawMode::Rectangle),
            MaskTool::Ellipse => Some(DrawMode::Ellipse),
            MaskTool::Polygon => Some(DrawMode::Polygon),
            // None / Pencil / Eraser are not shape draws (the brush paints
            // per-pointer; None disables masking).
            MaskTool::None | MaskTool::Pencil | MaskTool::Eraser => None,
        }
    }
}

/// Number of vertices on the pencil brush preview circle, mirroring silx
/// `DrawFreeHand._circle` (`PlotInteraction.py:996`, `numpy.arange(13.0)`).
pub(crate) const PENCIL_PREVIEW_SEGMENTS: usize = 13;

/// The data-space vertices of the pencil brush footprint preview: `segments`
/// points on a circle of `radius` around `center`. Mirrors silx
/// `DrawFreeHand`'s `_circle` (`PlotInteraction.py:996-998`): 13 points on a
/// circle of radius `pencil width * 0.5`, painted unfilled at the cursor. The
/// mask brush paints a disk of `brush_size / 2` cells, so a `radius` of
/// `brush_size / 2` (siplot masks in data==cell space) matches the
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
/// Which silx `DatasetDialog` mode the HDF5 dataset picker is in.
///
/// Mirrors `DatasetDialog.LoadMode`/`SaveMode`: load offers only the existing 2D
/// datasets to read; save lets the user name the dataset to write (a new name or
/// an existing one to overwrite).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum H5PickerMode {
    Load,
    Save,
}

/// Cross-frame state of the HDF5 "Select a 2D dataset" picker (silx
/// `DatasetDialog`).
///
/// The dataset-choice *logic* is a pure, headlessly-tested seam
/// ([`for_load`](Self::for_load)/[`for_save`](Self::for_save)/
/// [`resolved_path`](Self::resolved_path)); only the modal that drives it is an
/// egui UI shim (rendered by [`MaskToolsWidget::show_toolbar`]).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct H5DatasetPicker {
    /// The HDF5 file the dataset choice applies to.
    path: std::path::PathBuf,
    /// The existing 2D datasets in the file (silx tree filtered to datasets).
    /// Load offers these to pick; save shows them as overwrite shortcuts.
    datasets: Vec<String>,
    mode: H5PickerMode,
    /// The chosen (load) or typed (save) dataset path. Load seeds it to the
    /// first existing dataset; save seeds it to silx's default `mask`.
    name: String,
    /// The last load/save error, shown inline so the modal stays open for a
    /// retry instead of swallowing the failure (the rfd dialogs return
    /// `io::Result`; the modal completes across frames so it surfaces it here).
    error: Option<String>,
}

impl H5DatasetPicker {
    /// A load picker over the existing 2D datasets, seeded to the first one
    /// (silx `DatasetDialog.LoadMode`).
    fn for_load(path: std::path::PathBuf, datasets: Vec<String>) -> Self {
        let name = datasets.first().cloned().unwrap_or_default();
        Self {
            path,
            datasets,
            mode: H5PickerMode::Load,
            name,
            error: None,
        }
    }

    /// A save picker seeded to silx's default `mask` dataset name, showing the
    /// existing datasets as overwrite shortcuts (silx `DatasetDialog.SaveMode`).
    fn for_save(path: std::path::PathBuf, datasets: Vec<String>) -> Self {
        Self {
            path,
            datasets,
            mode: H5PickerMode::Save,
            name: "mask".to_string(),
            error: None,
        }
    }

    /// The resolved dataset path, or `None` when nothing is selectable (silx
    /// leaves OK disabled then).
    ///
    /// Mirrors silx `DatasetDialog._updateUrl`'s new-name rule
    /// (`newDatasetName.lstrip("/")`): strip leading `/`, and treat an empty
    /// result as no selection.
    fn resolved_path(&self) -> Option<String> {
        let p = self.name.trim_start_matches('/');
        if p.is_empty() {
            None
        } else {
            Some(p.to_string())
        }
    }
}

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

    /// Mask vs. unmask direction for the shape draws (rectangle / ellipse /
    /// polygon), mirroring silx `maskStateGroup` (`_BaseMaskToolsWidget.py:802`):
    /// `true` masks the current level, `false` unmasks it. Holding **Ctrl**
    /// during a draw inverts it, matching silx `_isMasking()`
    /// (`_BaseMaskToolsWidget.py:1175-1177`). The pencil / eraser brush tools
    /// carry their own direction (pencil masks, eraser unmasks); Ctrl inverts
    /// them too.
    pub mask_state: bool,

    /// Selected threshold-masking mode for the threshold group box (silx
    /// `thresholdActionGroup`, `belowThresholdAction` checked by default).
    pub threshold_mode: ThresholdMode,
    /// Lower bound for the `Below` / `Between` threshold modes (silx
    /// `minLineEdit`, default `0`).
    pub threshold_min: f32,
    /// Upper bound for the `Between` / `Above` threshold modes (silx
    /// `maxLineEdit`, default `0`).
    pub threshold_max: f32,

    history: MaskHistory,
    mask_handle: Option<ItemHandle>,
    is_dirty: bool,
    /// Last array cell `(row, col)` painted in the current pencil/eraser
    /// stroke, or `None` between strokes. Mirrors silx `_lastPencilPos`: it
    /// anchors the interpolating line so a fast drag leaves no gap, and is
    /// cleared when the stroke finishes or the image geometry changes.
    last_pencil_pos: Option<(i64, i64)>,

    /// Mask vs. unmask direction captured at the start of the current
    /// pencil/eraser stroke, or `None` between strokes. silx captures the Ctrl
    /// modifier on the first draw event and holds it for the whole sequence
    /// ("First draw event, use current modifiers for all draw sequence",
    /// `_BaseMaskToolsWidget.py:1174`), so mid-stroke Ctrl toggling does not
    /// flip the direction. Reset alongside [`last_pencil_pos`] at stroke end.
    stroke_do_mask: Option<bool>,

    /// In-progress on-plot shape draw (rectangle / ellipse / polygon), or `None`
    /// when no shape tool is mid-draw. Mirrors the draw state silx keeps while a
    /// `MaskToolsWidget` draw mode is active; the finished shape masks the
    /// current level via [`Self::fill_from_draw`]. Cleared on a geometry change
    /// (the old draw refers to stale coordinates) and when leaving a shape tool.
    shape_draw: Option<DrawState>,

    /// Cross-frame state for the HDF5 "Select a 2D dataset" picker (silx
    /// `DatasetDialog`, opened by `_selectDataset`). The rfd file dialog resolves
    /// first (blocking, native); the dataset choice is then made in the egui
    /// modal rendered by [`show_toolbar`](Self::show_toolbar) across frames, so
    /// the request is parked here meanwhile. `None` when no HDF5 pick is pending.
    pending_h5: Option<H5DatasetPicker>,
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
            // silx `maskStateGroup` defaults to the "Mask" radio (checkedId 1).
            mask_state: true,
            // silx threshold group defaults: below-threshold action checked,
            // min/max line edits initialised to 0.
            threshold_mode: ThresholdMode::Below,
            threshold_min: 0.0,
            threshold_max: 0.0,
            history,
            mask_handle: None,
            is_dirty: true, // Force initial upload
            last_pencil_pos: None,
            stroke_do_mask: None,
            shape_draw: None,
            pending_h5: None,
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
        // next stroke does not interpolate from a stale cell, and release the
        // stroke's captured mask/unmask direction.
        self.last_pencil_pos = None;
        self.stroke_do_mask = None;
        // An in-progress shape draw refers to the old geometry too; drop it.
        self.shape_draw = None;
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

    /// The current overlay transparency in `[0, 1]` (silx
    /// `transparencySlider.value() / maximum()`). Symmetric with
    /// [`set_transparency`](Self::set_transparency); defaults to `0.8`
    /// (silx `transparencySlider` default `8/10`).
    pub fn transparency(&self) -> f32 {
        self.alpha
    }

    /// Adapt the base overlay color to the masked image's colormap — the port
    /// of silx `_setOverlayColorForImage` (MaskToolsWidget.py:449-458):
    /// `_defaultOverlayColor = rgba(cursorColorForColormap(colormap["name"]))`,
    /// so the mask stays visible on top of the LUT (pink on gray, green on
    /// inferno, …; the constructor's `rgba("gray")` is only a pre-first-image
    /// placeholder). Call on every image sync; per-level overrides
    /// ([`set_mask_colors`](Self::set_mask_colors)) are untouched, as in silx.
    /// silx's `rgba("black")` branch for non-colormapped (RGBA) images has no
    /// counterpart here — the mask editor only attaches to colormapped images.
    pub fn set_overlay_color_for_colormap(&mut self, colormap: &crate::core::colormap::Colormap) {
        let [r, g, b, _] = colormap.cursor_color;
        let color = Color32::from_rgb(r, g, b);
        if self.color != color {
            self.color = color;
            self.is_dirty = true;
        }
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

    /// The effective overlay color of the current mask [`level`](Self::level).
    ///
    /// Mirrors silx `getCurrentMaskColor`
    /// (gui/plot/_BaseMaskToolsWidget.py:973-982): the per-level override if
    /// one has been set ([`set_mask_colors`](Self::set_mask_colors)), else the
    /// base overlay [`color`](Self::color) (silx `_defaultOverlayColor`).
    pub fn current_mask_color(&self) -> Color32 {
        match self.overrides.get(self.level as usize).copied().flatten() {
            Some([r, g, b]) => Color32::from_rgb(r, g, b),
            None => self.color,
        }
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
        ui.horizontal_wrapped(|ui| {
            ui.label("Mask:");

            ui.selectable_value(&mut self.active_tool, MaskTool::None, "○")
                .on_hover_text("Disable masking");
            ui.selectable_value(&mut self.active_tool, MaskTool::Pencil, "Pencil")
                .on_hover_text("Draw mask (hold Ctrl to erase)");
            ui.selectable_value(&mut self.active_tool, MaskTool::Eraser, "Eraser")
                .on_hover_text("Erase mask (hold Ctrl to draw)");
            ui.selectable_value(&mut self.active_tool, MaskTool::Rectangle, "Rectangle")
                .on_hover_text("Draw a rectangular region with the Mask/Unmask state");
            ui.selectable_value(&mut self.active_tool, MaskTool::Polygon, "Polygon")
                .on_hover_text("Draw a polygonal region with the Mask/Unmask state");
            ui.selectable_value(&mut self.active_tool, MaskTool::Ellipse, "Ellipse")
                .on_hover_text("Draw an elliptical region with the Mask/Unmask state");

            // Mask vs. unmask direction for the shape draws (silx
            // `maskStateGroup`); Ctrl inverts it during a draw (silx
            // `_isMasking()`). The hover texts mirror silx's radio tooltips.
            ui.separator();
            ui.selectable_value(&mut self.mask_state, true, "Mask")
                .on_hover_text("Drawing masks with current level. Press Ctrl to unmask");
            ui.selectable_value(&mut self.mask_state, false, "Unmask")
                .on_hover_text("Drawing unmasks with current level. Press Ctrl to mask");

            ui.add(egui::Slider::new(&mut self.level, 1..=255).text("Mask level"));

            // Per-level overlay color override (silx `getCurrentMaskColor` /
            // `setMaskColors` / `resetMaskColors`,
            // _BaseMaskToolsWidget.py:973-1042): the swatch shows the current
            // level's effective color (its override if set, else the base
            // color) and editing it sets that level's override via the tested
            // `set_mask_colors`; "Reset color" clears the override so the level
            // falls back to the base color through `reset_mask_colors`.
            let mut level_color = self.current_mask_color();
            if ui
                .color_edit_button_srgba(&mut level_color)
                .on_hover_text("Overlay color for the current mask level")
                .changed()
            {
                let [r, g, b, _] = level_color.to_srgba_unmultiplied();
                self.set_mask_colors([r, g, b], Some(self.level));
            }
            if ui
                .button("Reset color")
                .on_hover_text("Reset the current level's color to the default")
                .clicked()
            {
                self.reset_mask_colors(Some(self.level));
            }

            // Overlay transparency (silx `transparencySlider` -> `_setMaskColors`,
            // _BaseMaskToolsWidget.py:554-577): the selected level renders at this
            // alpha, other masked levels at half. Routed through `set_transparency`
            // so the change clamps and re-renders the overlay LUT.
            let mut alpha = self.alpha;
            if ui
                .add(egui::Slider::new(&mut alpha, 0.0..=1.0).text("Transparency"))
                .changed()
            {
                self.set_transparency(alpha);
            }

            if self.active_tool != MaskTool::None {
                // silx pencil width: a 1-50 slider for quick adjust plus a 1-1024
                // spin box for precise/large widths (_BaseMaskToolsWidget.py
                // :822-846, synced via _pencilWidthChanged). Both edit the same
                // `brush_size`, so egui keeps them in lockstep.
                ui.add(egui::Slider::new(&mut self.brush_size, 1..=50).text("Brush size"));
                ui.add(
                    egui::DragValue::new(&mut self.brush_size)
                        .range(1..=1024)
                        .speed(1.0),
                )
                .on_hover_text("Brush width in pixels (1-1024)");
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
            // Invert the current level, either by button or the silx Ctrl+I
            // shortcut (`invertAction`, `_BaseMaskToolsWidget.py:649`). `COMMAND`
            // is Ctrl on Windows/Linux and Cmd on macOS, matching Qt's `Qt.CTRL`.
            let invert_shortcut =
                ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::I));
            if ui
                .button("Invert")
                .on_hover_text("Invert current mask (Ctrl+I)")
                .clicked()
                || invert_shortcut
            {
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

            // Load.../Save... file actions (silx `loadAction`/`saveAction`,
            // _BaseMaskToolsWidget.py:631-643). The native rfd picker resolves
            // the file; for HDF5 the dataset choice then runs through the modal
            // below. Errors from the native shims are swallowed like silx's
            // QMessageBox warnings (no in-app caller to surface them to here).
            ui.separator();
            if ui
                .button("Load...")
                .on_hover_text("Load mask from file")
                .clicked()
            {
                let _ = self.load_mask_dialog();
            }
            if ui
                .button("Save...")
                .on_hover_text("Save mask to file")
                .clicked()
            {
                let _ = self.save_mask_dialog();
            }
        });

        // The HDF5 "Select a 2D dataset" modal, when a load/save is pending.
        self.show_h5_picker(ui);
    }

    /// The effective mask/unmask direction for a draw, given the draw's base
    /// direction and whether Ctrl is held. Mirrors silx `_isMasking()`
    /// (`_BaseMaskToolsWidget.py:1175-1177`): `doMask = base`, inverted while the
    /// Control modifier is down.
    fn effective_do_mask(base: bool, ctrl: bool) -> bool {
        base ^ ctrl
    }

    /// Handle pointer interaction from the plot response to paint/erase the
    /// mask along a pencil stroke.
    ///
    /// Mirrors the silx pencil drag (`MaskToolsWidget.py:848-876`): while the
    /// primary button is held, the pointer is converted to an array cell and
    /// fed to `Self::paint_pencil_point`, which interpolates a thick line
    /// from the previous sample (so a fast drag leaves no gap) and stamps a
    /// disk at the point. Releasing the button ends the stroke
    /// (`Self::end_pencil_stroke`) so the next stroke starts fresh.
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
            // Pencil masks the current level; eraser unmasks it; holding Ctrl
            // inverts the direction (silx `_isMasking()`). The modifier is
            // captured once at the start of the stroke and held for its whole
            // duration (silx "use current modifiers for all draw sequence"), so
            // a mid-stroke Ctrl toggle does not flip direction.
            let do_mask = *self.stroke_do_mask.get_or_insert_with(|| {
                // `command` is Ctrl on Windows/Linux and Cmd on macOS, matching
                // Qt's platform remap of `Qt.ControlModifier`.
                let ctrl = plot_response.response.ctx.input(|i| i.modifiers.command);
                Self::effective_do_mask(self.active_tool == MaskTool::Pencil, ctrl)
            });
            let (row, col) = pencil_cell(data_x, data_y);
            self.paint_pencil_point(row, col, do_mask);
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
        self.stroke_do_mask = None;
    }

    /// Drop any in-progress on-plot shape draw, so re-entering a shape tool
    /// starts a fresh shape (silx resets the draw when the mode changes). Called
    /// by the caller when the active tool is not a shape draw.
    pub(crate) fn cancel_shape_draw(&mut self) {
        self.shape_draw = None;
    }

    /// The in-progress shape-draw state machine, for rubber-band preview
    /// rendering by the caller, or `None` when no shape draw is armed.
    pub(crate) fn shape_draw(&self) -> Option<&DrawState> {
        self.shape_draw.as_ref()
    }

    /// Drive the on-plot shape draw (rectangle / ellipse / polygon) from the
    /// plot pointer, mirroring silx `MaskToolsWidget._plotDrawEvent` for the
    /// shape draw modes: feed the draw state machine with this frame's pointer,
    /// and on `drawingFinished` convert the data-space shape to array cells and
    /// mask the current level ([`Self::fill_from_draw`]), then re-arm a fresh
    /// machine for the next shape (silx's continuous draw). Returns this frame's
    /// [`DrawEvent`] so the caller can paint the rubber-band preview. A no-op
    /// returning `None` when the active tool is not a shape draw
    /// ([`MaskTool::draw_mode`]).
    pub(crate) fn handle_shape_draw(&mut self, plot_response: &PlotResponse) -> Option<DrawEvent> {
        let Some(mode) = self.active_tool.draw_mode() else {
            self.shape_draw = None;
            return None;
        };
        // (Re)arm the machine for the active shape; reset if the tool changed
        // mode mid-draw so the preview/finish matches the current tool.
        if !matches!(&self.shape_draw, Some(d) if d.mode() == mode) {
            self.shape_draw = Some(DrawState::new(mode));
        }
        let draw = self.shape_draw.as_mut().expect("armed above");
        let event = feed_draw_state(draw, &plot_response.response, &plot_response.transform);
        if let Some(DrawEvent::Finished { params, .. }) = &event {
            // Shape draws follow the Mask/Unmask state, inverted while Ctrl is
            // held at the finish (silx `_isMasking()`). `command` is Ctrl on
            // Windows/Linux and Cmd on macOS, matching Qt's `Qt.ControlModifier`.
            let ctrl = plot_response.response.ctx.input(|i| i.modifiers.command);
            let do_mask = Self::effective_do_mask(self.mask_state, ctrl);
            self.fill_from_draw(params, do_mask);
            // Re-arm for the next shape (silx draws continuously until the mode
            // is left).
            self.shape_draw = Some(DrawState::new(mode));
        }
        event
    }

    /// Drive the active drawing tool from the plot pointer and paint its live
    /// cursor preview — the standalone-complete entry point for wiring this
    /// widget onto a bare [`Plot2D`], so a caller gets the full tool set without
    /// reaching for the crate-internal shape-draw / preview pieces.
    ///
    /// Dispatches on the active tool: pencil / eraser brush strokes
    /// ([`Self::handle_interaction`]), rectangle / ellipse / polygon shape draws
    /// (`Self::handle_shape_draw`), each with the matching cursor preview
    /// (silx `MaskToolsWidget._plotDrawEvent`). The caller still calls
    /// [`Self::apply`] afterwards to upload the overlay. Put the plot in
    /// [`crate::PlotInteractionMode::MaskDraw`] so the
    /// primary drag is reserved for drawing rather than pan / zoom / box-select.
    pub fn handle_draw(&mut self, ui: &egui::Ui, plot_response: &PlotResponse) {
        match self.active_tool {
            MaskTool::Pencil | MaskTool::Eraser => {
                // Leaving a shape tool: drop any in-progress shape so it does not
                // resume if the user switches back.
                self.cancel_shape_draw();
                self.handle_interaction(plot_response);
                self.paint_brush_preview(ui, plot_response);
            }
            MaskTool::Rectangle | MaskTool::Polygon | MaskTool::Ellipse => {
                let event = self.handle_shape_draw(plot_response);
                self.paint_shape_preview(ui, plot_response, event.as_ref());
            }
            MaskTool::None => {
                // No tool: end any in-progress brush stroke (handle_interaction
                // does this for a non-brush tool) and drop any shape draw.
                self.cancel_shape_draw();
                self.handle_interaction(plot_response);
            }
        }
    }

    /// Paint the pencil / eraser brush footprint — an unfilled dashed circle of
    /// radius `brush_size / 2` (data coords) — at the live cursor, mirroring silx
    /// `DrawFreeHand.updatePencilShape` (`PlotInteraction.py:1011-1017`,
    /// `fill="none"`), shown both while hovering and while painting (silx draws
    /// it from `Idle.onMove` and `Select.onMove`). A no-op when the active tool
    /// is not a brush or the cursor is outside the data area. Painted on a
    /// foreground layer clipped to the data area.
    ///
    /// The single owner of the brush-preview render, shared by the standalone
    /// [`Self::handle_draw`] and [`crate::widget::high_level::ImageView`].
    pub(crate) fn paint_brush_preview(&self, ui: &egui::Ui, plot_response: &PlotResponse) {
        if !matches!(self.active_tool, MaskTool::Pencil | MaskTool::Eraser) {
            return;
        }
        let area = plot_response.transform.area;
        // Current pointer: hover_pos while idle, interact_pointer_pos while
        // painting (the pointer is captured by the drag).
        let Some(cursor) = plot_response
            .response
            .hover_pos()
            .or_else(|| plot_response.response.interact_pointer_pos())
        else {
            return;
        };
        if !area.contains(cursor) {
            return;
        }
        let center = plot_response.transform.pixel_to_data(cursor);
        let radius = self.brush_size as f64 / 2.0;
        let circle = pencil_preview_circle(center, radius, PENCIL_PREVIEW_SEGMENTS);
        let mut outline: Vec<egui::Pos2> = circle
            .iter()
            .map(|&(x, y)| plot_response.transform.data_to_pixel(x, y))
            .collect();
        if let Some(&first) = outline.first() {
            outline.push(first); // close the ring (silx polygon, fill="none")
        }
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("mask-brush-preview"),
        ));
        painter.with_clip_rect(area).add(egui::Shape::dashed_line(
            &outline,
            egui::Stroke::new(1.5, self.color),
            6.0,
            4.0,
        ));
    }

    /// Paint the in-progress shape-draw rubber band (rectangle / ellipse /
    /// polygon) in the mask overlay color on a foreground layer clipped to the
    /// data area, mirroring silx drawing the mask shape in the overlay color
    /// (`MaskToolsWidget._plotDrawEvent`). `event` is this frame's draw event
    /// from [`Self::handle_shape_draw`]. A no-op when no shape draw is in
    /// progress.
    ///
    /// The single owner of the shape-preview render, shared by the standalone
    /// [`Self::handle_draw`] and [`crate::widget::high_level::ImageView`].
    pub(crate) fn paint_shape_preview(
        &self,
        ui: &egui::Ui,
        plot_response: &PlotResponse,
        event: Option<&DrawEvent>,
    ) {
        let Some(draw) = self.shape_draw() else {
            return;
        };
        let style = SelectionStyle::new(FillMode::Hatch, self.color);
        let painter = ui
            .ctx()
            .layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("mask-shape-preview"),
            ))
            .with_clip_rect(plot_response.transform.area);
        paint_draw_preview(&painter, &plot_response.transform, draw, event, style);
    }

    /// Apply a finished draw shape to the current level, converting its
    /// data-space parameters to array cells exactly as silx
    /// `MaskToolsWidget._plotDrawEvent` does with origin 0 / scale 1 (data ==
    /// cell; silx `int()` / `astype(int64)` truncate toward zero), then commit
    /// the result to the undo history. `do_mask` masks the current level when
    /// `true` and unmasks it when `false`, mirroring silx `_isMasking()` (the
    /// Mask/Unmask state inverted by Ctrl), resolved by the caller.
    fn fill_from_draw(&mut self, params: &DrawParams, do_mask: bool) {
        let level = self.level;
        match params {
            DrawParams::Rectangle {
                x,
                y,
                width,
                height,
            } => {
                let (row, col, h, w) = rect_params_to_cells(*x, *y, *width, *height);
                self.update_rectangle(level, row, col, h, w, do_mask);
            }
            DrawParams::Ellipse { center, semi_axes } => {
                let (crow, ccol, radius_r, radius_c) = ellipse_params_to_cells(*center, *semi_axes);
                self.update_ellipse(level, crow, ccol, radius_r, radius_c, do_mask);
            }
            DrawParams::Polygon { vertices } => {
                let cells = polygon_vertices_to_cells(vertices);
                self.update_polygon(level, &cells, do_mask);
            }
            // Other shapes (line / h-line / v-line / freehand / point) are not
            // mask draw modes (gated by `MaskTool::draw_mode`, so only the wired
            // shapes can reach here).
            _ => return,
        }
        self.commit();
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
        Ok(self.apply_loaded_mask(height, width, data))
    }

    /// Apply a freshly-loaded 2D `uint8` mask, cropping or padding it to the
    /// current image geometry and committing it to the undo history.
    ///
    /// Single owner of silx's crop/pad rule (`resizedMask[:h, :w] =
    /// mask[:h, :w]`, gui/plot/MaskToolsWidget.py:350-368) shared by every
    /// file-format loader ([`read_npy`](Self::read_npy),
    /// [`read_edf`](Self::read_edf), [`read_tiff`](Self::read_tiff)). Returns
    /// `true` when the loaded shape differed from the current image (silx raises
    /// `RuntimeWarning`).
    fn apply_loaded_mask(&mut self, height: u32, width: u32, data: Vec<u8>) -> bool {
        let resized = height != self.height || width != self.width;
        if resized {
            // silx crop/pad: zero buffer of current shape, copy the overlap.
            let mut buf = vec![0u8; (self.width as usize) * (self.height as usize)];
            let copy_h = self.height.min(height) as usize;
            let copy_w = self.width.min(width) as usize;
            for row in 0..copy_h {
                let dst = row * self.width as usize;
                let src = row * width as usize;
                buf[dst..dst + copy_w].copy_from_slice(&data[src..src + copy_w]);
            }
            self.mask = buf;
        } else {
            self.mask = data;
        }
        self.commit();
        self.is_dirty = true;
        resized
    }

    /// Write the current mask as an ESRF Data Format (EDF) stream.
    ///
    /// Mirrors silx `MaskToolsWidget.save(filename, "edf")` (the fabio
    /// `edfimage` writer) for the `uint8` mask; the byte format lives in the
    /// single-owner codec [`crate::render::save::encode_mask_edf`].
    pub fn write_edf(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(&crate::render::save::encode_mask_edf(
            self.height,
            self.width,
            &self.mask,
        ))
    }

    /// Read a 2D `uint8` EDF mask and apply it, cropping or padding to the
    /// current image geometry (silx `MaskToolsWidget.load`, EDF branch).
    ///
    /// Returns `Ok(true)` when the loaded shape differed from the current image
    /// (a resize occurred), `Ok(false)` when it matched. Decoded by the
    /// single-owner codec [`crate::render::save::decode_mask_edf`].
    pub fn read_edf(&mut self, mut r: impl Read) -> io::Result<bool> {
        let mut bytes = Vec::new();
        r.read_to_end(&mut bytes)?;
        let (height, width, data) = crate::render::save::decode_mask_edf(&bytes)?;
        Ok(self.apply_loaded_mask(height, width, data))
    }

    /// Write the current mask as a single-page grayscale TIFF stream.
    ///
    /// Mirrors silx `MaskToolsWidget.save(filename, "tif")` (fabio / `tifffile`)
    /// for the `uint8` mask; the byte format lives in the single-owner codec
    /// [`crate::render::save::encode_mask_tiff`].
    pub fn write_tiff(&self, w: &mut impl Write) -> io::Result<()> {
        let bytes = crate::render::save::encode_mask_tiff(self.height, self.width, &self.mask)?;
        w.write_all(&bytes)
    }

    /// Read a 2D `uint8` TIFF mask and apply it, cropping or padding to the
    /// current image geometry (silx `MaskToolsWidget.load`, TIFF branch).
    ///
    /// Returns `Ok(true)` when the loaded shape differed from the current image
    /// (a resize occurred), `Ok(false)` when it matched. Decoded by the
    /// single-owner codec [`crate::render::save::decode_mask_tiff`].
    pub fn read_tiff(&mut self, mut r: impl Read) -> io::Result<bool> {
        let mut bytes = Vec::new();
        r.read_to_end(&mut bytes)?;
        let (height, width, data) = crate::render::save::decode_mask_tiff(&bytes)?;
        Ok(self.apply_loaded_mask(height, width, data))
    }

    /// Write the current mask as an Andy Hammersley fit2d `.msk` stream.
    ///
    /// Mirrors silx `MaskToolsWidget.save(filename, "msk")` (via fabio
    /// `Fit2dMaskImage`); the byte format lives in the single-owner codec
    /// [`crate::render::save::encode_mask_msk`]. fit2d masks are **binary**: every
    /// non-zero mask level is written as a set bit, so multi-level information is
    /// collapsed to masked/unmasked (faithful to fabio writing `data != 0`).
    pub fn write_msk(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(&crate::render::save::encode_mask_msk(
            self.height,
            self.width,
            &self.mask,
        ))
    }

    /// Read a fit2d `.msk` mask and apply it, cropping or padding to the current
    /// image geometry (silx `MaskToolsWidget.load`, msk branch via fabio).
    ///
    /// The loaded mask is binary (`1` where masked). Returns `Ok(true)` when the
    /// loaded shape differed from the current image (a resize occurred), `Ok(false)`
    /// when it matched. Decoded by the single-owner codec
    /// [`crate::render::save::decode_mask_msk`].
    pub fn read_msk(&mut self, mut r: impl Read) -> io::Result<bool> {
        let mut bytes = Vec::new();
        r.read_to_end(&mut bytes)?;
        let (height, width, data) = crate::render::save::decode_mask_msk(&bytes)?;
        Ok(self.apply_loaded_mask(height, width, data))
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

    /// Save the current mask to an `.edf` file.
    ///
    /// File wrapper over [`write_edf`](Self::write_edf); see it for the format.
    pub fn save_edf(&self, path: impl AsRef<std::path::Path>) -> io::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut writer = io::BufWriter::new(file);
        self.write_edf(&mut writer)?;
        writer.flush()
    }

    /// Load a mask from an `.edf` file, cropping/padding to the current image.
    ///
    /// File wrapper over [`read_edf`](Self::read_edf); returns `Ok(true)` when
    /// the loaded shape differed from the current image (resize occurred).
    pub fn load_edf(&mut self, path: impl AsRef<std::path::Path>) -> io::Result<bool> {
        let file = std::fs::File::open(path)?;
        let reader = io::BufReader::new(file);
        self.read_edf(reader)
    }

    /// Save the current mask to an `.edf` file at the given in-app path string
    /// (silx `MaskToolsWidget.save(filename, "edf")`).
    ///
    /// Takes a plain `&str` path entered in-app rather than opening a native
    /// file dialog. The EDF bytes are produced by the single-owner codec
    /// [`crate::render::save::encode_mask_edf`].
    pub fn save_mask_edf(&self, path: &str) -> io::Result<()> {
        self.save_edf(path)
    }

    /// Load a mask from an `.edf` file at the given in-app path string, cropping
    /// or padding to the current image geometry (silx
    /// `MaskToolsWidget.load(filename)`, EDF branch).
    ///
    /// Takes a plain `&str` path entered in-app rather than opening a native
    /// file dialog. Returns `Ok(true)` when the loaded shape differed from the
    /// current image. The bytes are decoded by the single-owner codec
    /// [`crate::render::save::decode_mask_edf`].
    pub fn load_mask_edf(&mut self, path: &str) -> io::Result<bool> {
        self.load_edf(path)
    }

    /// Save the current mask to a `.tif`/`.tiff` file.
    ///
    /// File wrapper over [`write_tiff`](Self::write_tiff); see it for the format.
    pub fn save_tiff(&self, path: impl AsRef<std::path::Path>) -> io::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut writer = io::BufWriter::new(file);
        self.write_tiff(&mut writer)?;
        writer.flush()
    }

    /// Load a mask from a `.tif`/`.tiff` file, cropping/padding to the current
    /// image.
    ///
    /// File wrapper over [`read_tiff`](Self::read_tiff); returns `Ok(true)` when
    /// the loaded shape differed from the current image (resize occurred).
    pub fn load_tiff(&mut self, path: impl AsRef<std::path::Path>) -> io::Result<bool> {
        let file = std::fs::File::open(path)?;
        let reader = io::BufReader::new(file);
        self.read_tiff(reader)
    }

    /// Save the current mask to a TIFF file at the given in-app path string
    /// (silx `MaskToolsWidget.save(filename, "tif")`).
    ///
    /// Takes a plain `&str` path entered in-app rather than opening a native
    /// file dialog. The TIFF bytes are produced by the single-owner codec
    /// [`crate::render::save::encode_mask_tiff`].
    pub fn save_mask_tiff(&self, path: &str) -> io::Result<()> {
        self.save_tiff(path)
    }

    /// Load a mask from a TIFF file at the given in-app path string, cropping or
    /// padding to the current image geometry (silx `MaskToolsWidget.load`, TIFF
    /// branch).
    ///
    /// Takes a plain `&str` path entered in-app rather than opening a native
    /// file dialog. Returns `Ok(true)` when the loaded shape differed from the
    /// current image. The bytes are decoded by the single-owner codec
    /// [`crate::render::save::decode_mask_tiff`].
    pub fn load_mask_tiff(&mut self, path: &str) -> io::Result<bool> {
        self.load_tiff(path)
    }

    /// Save the current mask to a fit2d `.msk` file.
    ///
    /// File wrapper over [`write_msk`](Self::write_msk); see it for the (binary)
    /// format.
    pub fn save_msk(&self, path: impl AsRef<std::path::Path>) -> io::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut writer = io::BufWriter::new(file);
        self.write_msk(&mut writer)?;
        writer.flush()
    }

    /// Load a mask from a fit2d `.msk` file, cropping/padding to the current image.
    ///
    /// File wrapper over [`read_msk`](Self::read_msk); returns `Ok(true)` when the
    /// loaded shape differed from the current image (resize occurred).
    pub fn load_msk(&mut self, path: impl AsRef<std::path::Path>) -> io::Result<bool> {
        let file = std::fs::File::open(path)?;
        let reader = io::BufReader::new(file);
        self.read_msk(reader)
    }

    /// Save the current mask to a fit2d `.msk` file at the given in-app path string
    /// (silx `MaskToolsWidget.save(filename, "msk")`). Thin shim over
    /// [`save_msk`](Self::save_msk).
    pub fn save_mask_msk(&self, path: &str) -> io::Result<()> {
        self.save_msk(path)
    }

    /// Load a mask from a fit2d `.msk` file at the given in-app path string,
    /// cropping or padding to the current image geometry (silx
    /// `MaskToolsWidget.load`, msk branch). Thin shim over
    /// [`load_msk`](Self::load_msk).
    pub fn load_mask_msk(&mut self, path: &str) -> io::Result<bool> {
        self.load_msk(path)
    }

    /// Save the current mask to an HDF5 file, into a dataset named `mask`.
    ///
    /// Mirrors silx `MaskToolsWidget.save(filename, "h5")` →
    /// `_saveToHdf5` (gui/plot/MaskToolsWidget.py:128-174). silx prompts for the
    /// dataset path via its "Select a 2D dataset" dialog; this writes to the
    /// fixed default `mask` (the interactive dataset picker is the remaining
    /// Qt-specific gap — the selection *mechanism*, enumerating and reading a
    /// chosen dataset, is ported as [`mask_datasets`](Self::mask_datasets) /
    /// [`load_h5_dataset`](Self::load_h5_dataset)).
    ///
    /// HDF5 is a random-access container, not a byte stream, so this takes a path
    /// rather than `impl Write`. Backed by the pure-Rust `rust-hdf5` crate;
    /// faithful to silx's `h5py.File(filename, "a")` open mode — an existing file
    /// is appended to (its other datasets preserved, the `mask` dataset
    /// overwritten), a missing file is created.
    pub fn save_h5(&self, path: impl AsRef<std::path::Path>) -> io::Result<()> {
        crate::render::save::write_mask_hdf5(
            path.as_ref(),
            "mask",
            self.height,
            self.width,
            &self.mask,
        )
    }

    /// Load a mask from the first 2D dataset of an HDF5 file, cropping/padding to
    /// the current image geometry (silx `MaskToolsWidget.load`, HDF5 branch →
    /// `_loadFromHdf5`).
    ///
    /// When the file holds several 2D datasets, the first by sorted path is used;
    /// enumerate with [`mask_datasets`](Self::mask_datasets) and call
    /// [`load_h5_dataset`](Self::load_h5_dataset) to choose explicitly. Returns
    /// `Ok(true)` when the loaded shape differed from the current image (resize
    /// occurred). Backed by the pure-Rust `rust-hdf5` crate.
    pub fn load_h5(&mut self, path: impl AsRef<std::path::Path>) -> io::Result<bool> {
        let (height, width, data) = crate::render::save::read_mask_hdf5_auto(path.as_ref())?;
        Ok(self.apply_loaded_mask(height, width, data))
    }

    /// Load a mask from the named 2D dataset `data_path` of an HDF5 file
    /// (silx `_loadFromHdf5` with an explicit `_selectDataset` choice).
    ///
    /// The explicit-selection counterpart of [`load_h5`](Self::load_h5); pair with
    /// [`mask_datasets`](Self::mask_datasets) to present the available datasets.
    /// Returns `Ok(true)` when the loaded shape differed from the current image.
    pub fn load_h5_dataset(
        &mut self,
        path: impl AsRef<std::path::Path>,
        data_path: &str,
    ) -> io::Result<bool> {
        let (height, width, data) = crate::render::save::read_mask_hdf5(path.as_ref(), data_path)?;
        Ok(self.apply_loaded_mask(height, width, data))
    }

    /// List the full paths of every 2D dataset in an HDF5 file (the choices silx's
    /// "Select a 2D dataset" dialog would offer for load/save).
    pub fn mask_datasets(&self, path: impl AsRef<std::path::Path>) -> io::Result<Vec<String>> {
        crate::render::save::list_mask_datasets_hdf5(path.as_ref())
    }

    /// Save the current mask to a named 2D dataset of an HDF5 file (silx
    /// `_saveToHdf5` with an explicit `_selectDataset` choice).
    ///
    /// The explicit-name counterpart of [`save_h5`](Self::save_h5) (which always
    /// uses `mask`); used by the "Select a 2D dataset" save picker. Faithful to
    /// silx's "a" append mode via [`write_mask_hdf5`](crate::render::save::write_mask_hdf5).
    pub fn save_h5_dataset(
        &self,
        path: impl AsRef<std::path::Path>,
        data_path: &str,
    ) -> io::Result<()> {
        crate::render::save::write_mask_hdf5(
            path.as_ref(),
            data_path,
            self.height,
            self.width,
            &self.mask,
        )
    }

    /// Open the HDF5 "Select a 2D dataset" picker for a load (silx
    /// `_loadFromHdf5` → `_selectDataset(LoadMode)`).
    ///
    /// Enumerates the file's 2D datasets and parks a load picker; the actual read
    /// happens when the modal — rendered by [`show_toolbar`](Self::show_toolbar) —
    /// is confirmed. The [`load_mask_dialog`](Self::load_mask_dialog) HDF5 branch
    /// calls this after the native file picker resolves; a host can also call it
    /// directly to open the picker for a known file.
    pub fn begin_h5_load(&mut self, path: std::path::PathBuf) -> io::Result<()> {
        let datasets = crate::render::save::list_mask_datasets_hdf5(&path)?;
        self.pending_h5 = Some(H5DatasetPicker::for_load(path, datasets));
        Ok(())
    }

    /// Open the HDF5 "Select a 2D dataset" picker for a save (silx `_saveToHdf5`
    /// → `_selectDataset(SaveMode)`).
    ///
    /// Shows the existing datasets (if the file already exists) as overwrite
    /// shortcuts and seeds the name to silx's default `mask`; the write happens
    /// when the modal — rendered by [`show_toolbar`](Self::show_toolbar) — is
    /// confirmed. The [`save_mask_dialog`](Self::save_mask_dialog) HDF5 branch
    /// calls this after the native file picker resolves; a host can also call it
    /// directly to open the picker for a known file.
    pub fn begin_h5_save(&mut self, path: std::path::PathBuf) -> io::Result<()> {
        // A save target may not exist yet; only an existing file has datasets to
        // list (and to offer as overwrite shortcuts).
        let datasets = if path.exists() {
            crate::render::save::list_mask_datasets_hdf5(&path)?
        } else {
            Vec::new()
        };
        self.pending_h5 = Some(H5DatasetPicker::for_save(path, datasets));
        Ok(())
    }

    /// Apply a confirmed HDF5 dataset pick: load from / save to the resolved
    /// dataset path (the modal's OK action; also the testable seam).
    ///
    /// Returns `Ok(Some(resized))` for a load (whether the loaded shape differed
    /// from the image), `Ok(None)` for a save. An unselectable picker
    /// ([`resolved_path`](H5DatasetPicker::resolved_path) `None`) is a no-op
    /// returning `Ok(None)`.
    pub(crate) fn apply_h5_pick(&mut self, picker: &H5DatasetPicker) -> io::Result<Option<bool>> {
        let Some(data_path) = picker.resolved_path() else {
            return Ok(None);
        };
        match picker.mode {
            H5PickerMode::Load => Ok(Some(self.load_h5_dataset(&picker.path, &data_path)?)),
            H5PickerMode::Save => {
                self.save_h5_dataset(&picker.path, &data_path)?;
                Ok(None)
            }
        }
    }

    /// Render the HDF5 "Select a 2D dataset" modal when a pick is pending (silx
    /// `DatasetDialog`); a no-op otherwise.
    ///
    /// Load lists the existing datasets to choose; save adds a name field
    /// (default `mask`) with the existing datasets as overwrite shortcuts. OK
    /// applies the pick via [`apply_h5_pick`](Self::apply_h5_pick) (errors stay
    /// inline so the modal can be retried); Cancel drops it. UI shim — the
    /// choice logic and `apply_h5_pick` are headlessly tested.
    fn show_h5_picker(&mut self, ui: &mut egui::Ui) {
        let Some(mut picker) = self.pending_h5.take() else {
            return;
        };
        // None = still open, Some(true) = OK, Some(false) = Cancel.
        let mut action: Option<bool> = None;
        egui::Window::new("Select a 2D dataset")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label(match picker.mode {
                    H5PickerMode::Load => "Select a dataset",
                    H5PickerMode::Save => "Select a dataset or type a new dataset name",
                });
                if picker.datasets.is_empty() {
                    ui.weak("(no 2D datasets in this file)");
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(160.0)
                        .show(ui, |ui| {
                            for ds in &picker.datasets {
                                ui.selectable_value(&mut picker.name, ds.clone(), ds);
                            }
                        });
                }
                if picker.mode == H5PickerMode::Save {
                    ui.horizontal(|ui| {
                        ui.label("Dataset:");
                        ui.text_edit_singleline(&mut picker.name);
                    });
                }
                if let Some(err) = &picker.error {
                    ui.colored_label(egui::Color32::RED, err);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let can_ok = picker.resolved_path().is_some();
                    if ui.add_enabled(can_ok, egui::Button::new("OK")).clicked() {
                        action = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(false);
                    }
                });
            });
        match action {
            // OK: apply; on error keep the modal open with the message shown.
            Some(true) => match self.apply_h5_pick(&picker) {
                Ok(_) => {}
                Err(e) => {
                    picker.error = Some(e.to_string());
                    self.pending_h5 = Some(picker);
                }
            },
            // Cancel: drop the pending pick.
            Some(false) => {}
            // Still open: keep the (possibly edited) picker for the next frame.
            None => self.pending_h5 = Some(picker),
        }
    }

    /// Save the current mask to an HDF5 file at the given in-app path string
    /// (silx `MaskToolsWidget.save(filename, "h5")`).
    ///
    /// Takes a plain `&str` path entered in-app rather than opening a native file
    /// dialog. Thin shim over [`save_h5`](Self::save_h5).
    pub fn save_mask_h5(&self, path: &str) -> io::Result<()> {
        self.save_h5(path)
    }

    /// Load a mask from an HDF5 file at the given in-app path string, cropping or
    /// padding to the current image geometry (silx `MaskToolsWidget.load`, HDF5
    /// branch).
    ///
    /// Takes a plain `&str` path entered in-app rather than opening a native file
    /// dialog. Thin shim over [`load_h5`](Self::load_h5).
    pub fn load_mask_h5(&mut self, path: &str) -> io::Result<bool> {
        self.load_h5(path)
    }

    /// Open a native save-file dialog (silx `MaskToolsWidget._saveMask`) and
    /// write the current mask to the chosen path, dispatching to the `.npy`,
    /// `.edf`, `.tif`/`.tiff`, `.h5` (HDF5), or `.msk` (fit2d) codec by file
    /// extension. An extensionless path defaults to `.npy` (silx's default kind);
    /// an unsupported extension is rejected
    /// (faithful to silx `save` raising on an unknown kind). Returns `Ok(true)`
    /// when a file was written, `Ok(false)` on cancel or when an HDF5 dataset
    /// choice is pending (the modal completes the write later). The picker is a
    /// native shim; the codecs and the extension dispatch
    /// (`resolve_mask_save_format`) are unit-tested.
    pub fn save_mask_dialog(&mut self) -> io::Result<bool> {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("NumPy mask", &["npy"])
            .add_filter("EDF mask", &["edf"])
            .add_filter("TIFF mask", &["tif", "tiff"])
            .add_filter("HDF5 mask", &["h5", "nx5", "nxs", "hdf", "hdf5", "cxi"])
            .add_filter("Fit2D mask", &["msk"])
            .save_file()
        else {
            return Ok(false);
        };
        match resolve_mask_save_format(&path)? {
            MaskFileFormat::Npy => self.save_npy(&path)?,
            MaskFileFormat::Edf => self.save_edf(&path)?,
            MaskFileFormat::Tiff => self.save_tiff(&path)?,
            // HDF5 defers to the "Select a 2D dataset" modal (silx
            // `_selectDataset(SaveMode)`); the write runs on confirm.
            MaskFileFormat::H5 => {
                self.begin_h5_save(path)?;
                return Ok(false);
            }
            MaskFileFormat::Msk => self.save_msk(&path)?,
        }
        Ok(true)
    }

    /// Open a native open-file dialog (silx `MaskToolsWidget._loadMask`) and load
    /// a mask from the chosen path, dispatching by extension
    /// (`.npy`/`.edf`/`.tif`/`.tiff`/`.h5`/`.msk`) and cropping/padding to the
    /// current image.
    /// Returns `Ok(Some(resized))` — where `resized` is `true` when the loaded
    /// shape differed from the current image — `Ok(None)` on cancel or when an
    /// HDF5 dataset choice is pending (the modal completes the read later). An
    /// unknown extension is rejected (faithful to silx `load` raising on an
    /// unknown extension). Native shim; the codecs and the extension dispatch
    /// (`resolve_mask_load_format`) are unit-tested.
    pub fn load_mask_dialog(&mut self) -> io::Result<Option<bool>> {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("NumPy mask", &["npy"])
            .add_filter("EDF mask", &["edf"])
            .add_filter("TIFF mask", &["tif", "tiff"])
            .add_filter("HDF5 mask", &["h5", "nx5", "nxs", "hdf", "hdf5", "cxi"])
            .add_filter("Fit2D mask", &["msk"])
            .pick_file()
        else {
            return Ok(None);
        };
        let resized = match resolve_mask_load_format(&path)? {
            MaskFileFormat::Npy => self.load_npy(&path)?,
            MaskFileFormat::Edf => self.load_edf(&path)?,
            MaskFileFormat::Tiff => self.load_tiff(&path)?,
            // HDF5 defers to the "Select a 2D dataset" modal (silx
            // `_selectDataset(LoadMode)`); the read runs on confirm.
            MaskFileFormat::H5 => {
                self.begin_h5_load(path)?;
                return Ok(None);
            }
            MaskFileFormat::Msk => self.load_msk(&path)?,
        };
        Ok(Some(resized))
    }
}

/// The mask file formats siplot can encode/decode — the full silx
/// `MaskToolsWidget` set: NumPy `.npy`, ESRF `.edf`, `.tif`/`.tiff`, HDF5 (via
/// the pure-Rust `rust-hdf5` crate), and Andy Hammersley fit2d `.msk` (binary).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MaskFileFormat {
    Npy,
    Edf,
    Tiff,
    H5,
    Msk,
}

/// Map a (case-insensitive) file extension to its [`MaskFileFormat`], or `None`
/// for an extension siplot cannot handle. Single owner of the extension→format
/// mapping shared by the save and load dialogs (silx dispatches the same way via
/// `os.path.splitext(filename).lower()`).
fn mask_format_for_ext(ext: &str) -> Option<MaskFileFormat> {
    match ext.to_ascii_lowercase().as_str() {
        "npy" => Some(MaskFileFormat::Npy),
        "edf" => Some(MaskFileFormat::Edf),
        "tif" | "tiff" => Some(MaskFileFormat::Tiff),
        // silx NEXUS_HDF5_EXT (silx/io/utils.py): .h5 .nx5 .nxs .hdf .hdf5 .cxi
        "h5" | "nx5" | "nxs" | "hdf" | "hdf5" | "cxi" => Some(MaskFileFormat::H5),
        "msk" => Some(MaskFileFormat::Msk),
        _ => None,
    }
}

/// Resolve the save format for `path`: a known extension picks the codec, a
/// missing extension defaults to `.npy` (silx's default kind), and an
/// unsupported extension is rejected rather than silently coerced.
fn resolve_mask_save_format(path: &std::path::Path) -> io::Result<MaskFileFormat> {
    match path.extension().and_then(|e| e.to_str()) {
        None => Ok(MaskFileFormat::Npy),
        Some(ext) => mask_format_for_ext(ext).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported mask format: .{ext} (siplot writes .npy/.edf/.tif/.h5/.msk)"),
            )
        }),
    }
}

/// Resolve the load format for `path` by extension; a missing or unsupported
/// extension is rejected (a file's format cannot be guessed, faithful to silx
/// `load` raising on an unknown extension).
fn resolve_mask_load_format(path: &std::path::Path) -> io::Result<MaskFileFormat> {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(mask_format_for_ext)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported mask format (siplot reads .npy/.edf/.tif/.h5/.msk)",
            )
        })
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

/// Convert a finished rectangle draw to mask array cells `(row, col, height,
/// width)`, mirroring silx `MaskToolsWidget._plotDrawEvent`'s rectangle branch
/// (`MaskToolsWidget.py:805-825`) with origin 0 / scale 1 (data == cell):
/// `row = int(y)`, `col = int(x)`, `height = int(|height|)`,
/// `width = int(|width|)`, where silx `int()` truncates toward zero. The draw's
/// `(x, y)` is the min corner and `width`/`height` are non-negative extents
/// (`DrawParams::Rectangle`). The returned cells feed
/// [`MaskToolsWidget::update_rectangle`], which masks rows `row..=row+height`
/// and columns `col..=col+width` (silx slice `[row : row + height + 1]`).
pub(crate) fn rect_params_to_cells(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> (i64, i64, i64, i64) {
    (y as i64, x as i64, height.abs() as i64, width.abs() as i64)
}

/// Convert a pencil sample's data coordinates to the mask array cell
/// `(row, col)`, mirroring silx `MaskToolsWidget._plotDrawEvent`'s pencil
/// branch (`MaskToolsWidget.py:857-858`) with origin 0 / scale 1
/// (data == cell): `col, row = int(col), int(row)` — truncation toward zero,
/// like [`rect_params_to_cells`], NOT `floor()`. The two differ on negative
/// fractional coordinates: a stroke within one pixel outside the top/left
/// image edge anchors at 0 (edge cell) in silx, not at −1.
pub(crate) fn pencil_cell(data_x: f64, data_y: f64) -> (i64, i64) {
    (data_y as i64, data_x as i64)
}

/// Convert a finished ellipse draw to mask array parameters
/// `(crow, ccol, radius_r, radius_c)`, mirroring silx
/// `MaskToolsWidget._plotDrawEvent`'s ellipse branch
/// (`MaskToolsWidget.py:828-838`) with origin 0 / scale 1 (data == cell):
/// the `center` is cast to `int64` (`crow = int(cy)`, `ccol = int(cx)`,
/// truncating toward zero) while the radii stay floating-point (silx does *not*
/// cast `size`). `DrawParams::Ellipse` stores `semi_axes` as `(x_semi, y_semi)`
/// (`ellipse_semi_axes` returns `(x, y)`); silx maps `size[1]` (the y/row
/// semi-axis) to `radius_r` and `size[0]` (the x/col semi-axis) to `radius_c`.
/// The result feeds [`MaskToolsWidget::update_ellipse`].
pub(crate) fn ellipse_params_to_cells(
    center: (f64, f64),
    semi_axes: (f64, f64),
) -> (i64, i64, f32, f32) {
    (
        center.1 as i64,    // crow = int(center_y)
        center.0 as i64,    // ccol = int(center_x)
        semi_axes.1 as f32, // radius_r = y/row semi-axis
        semi_axes.0 as f32, // radius_c = x/col semi-axis
    )
}

/// Convert a finished polygon draw's data-space `(x, y)` vertices to mask array
/// `(row, col)` vertices, mirroring silx `MaskToolsWidget._plotDrawEvent`'s
/// polygon branch (`MaskToolsWidget.py:840-847`) with origin 0 / scale 1
/// (data == cell): `vertices.astype(int64)[:, (1, 0)]` casts each `(x, y)` to
/// `int64` (truncate toward zero) and swaps to `(row = int(y), col = int(x))`.
/// The result feeds [`MaskToolsWidget::update_polygon`] (via
/// [`polygon_fill_mask`], whose vertices are `(row, col)`).
pub(crate) fn polygon_vertices_to_cells(vertices: &[(f64, f64)]) -> Vec<(i64, i64)> {
    vertices
        .iter()
        .map(|&(x, y)| (y as i64, x as i64))
        .collect()
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
    fn pencil_cell_truncates_toward_zero_like_silx_int() {
        // R2-22: silx `int(col), int(row)` (MaskToolsWidget.py:858). A stroke
        // just outside the top/left edge anchors at the edge cell 0, not −1.
        assert_eq!(pencil_cell(2.7, 3.9), (3, 2));
        assert_eq!(pencil_cell(-0.5, -0.5), (0, 0));
        assert_eq!(pencil_cell(-1.5, 0.5), (0, -1));
        // Consistent with the port's own rectangle converter.
        assert_eq!(rect_params_to_cells(-0.5, -0.5, 1.0, 1.0).0, 0);
    }

    #[test]
    fn overlay_color_adapts_to_the_image_colormap() {
        use crate::core::colormap::{Colormap, ColormapName};

        // R2-10: silx `_setOverlayColorForImage` (MaskToolsWidget.py:449-458)
        // re-derives the base overlay color from the image's colormap on every
        // sync; the constructor gray is only a pre-first-image placeholder.
        let mut mask = MaskToolsWidget::new(4, 4);
        assert_eq!(mask.color, Color32::from_rgb(160, 160, 164), "placeholder");

        mask.set_overlay_color_for_colormap(&Colormap::new(ColormapName::Gray, 0.0, 1.0));
        assert_eq!(mask.color, Color32::from_rgb(255, 102, 255), "pink on gray");

        mask.set_overlay_color_for_colormap(&Colormap::new(ColormapName::Inferno, 0.0, 1.0));
        assert_eq!(mask.color, Color32::from_rgb(0, 255, 0), "green on inferno");

        // Per-level overrides survive the adaptation (silx only replaces
        // `_defaultOverlayColor`; `_overlayColors` overrides stay).
        mask.level = 1;
        mask.set_mask_colors([9, 8, 7], Some(1));
        mask.set_overlay_color_for_colormap(&Colormap::new(ColormapName::Blue, 0.0, 1.0));
        assert_eq!(mask.current_mask_color(), Color32::from_rgb(9, 8, 7));
        assert_eq!(mask.color, Color32::from_rgb(255, 255, 0), "yellow on blue");
    }

    #[test]
    fn mask_save_format_dispatches_by_extension_defaulting_to_npy() {
        use std::path::Path;
        assert_eq!(
            resolve_mask_save_format(Path::new("m.npy")).unwrap(),
            MaskFileFormat::Npy
        );
        assert_eq!(
            resolve_mask_save_format(Path::new("m.edf")).unwrap(),
            MaskFileFormat::Edf
        );
        // Case-insensitive (silx lowercases the extension).
        assert_eq!(
            resolve_mask_save_format(Path::new("M.EDF")).unwrap(),
            MaskFileFormat::Edf
        );
        // A missing extension defaults to npy (silx's default kind).
        assert_eq!(
            resolve_mask_save_format(Path::new("mask")).unwrap(),
            MaskFileFormat::Npy
        );
        // TIFF (.tif/.tiff, case-insensitive) is now a supported format.
        assert_eq!(
            resolve_mask_save_format(Path::new("m.tif")).unwrap(),
            MaskFileFormat::Tiff
        );
        assert_eq!(
            resolve_mask_save_format(Path::new("M.TIFF")).unwrap(),
            MaskFileFormat::Tiff
        );
        // HDF5 extensions (silx NEXUS_HDF5_EXT) resolve to the H5 format.
        assert_eq!(
            resolve_mask_save_format(Path::new("m.h5")).unwrap(),
            MaskFileFormat::H5
        );
        assert_eq!(
            resolve_mask_save_format(Path::new("m.NXS")).unwrap(),
            MaskFileFormat::H5
        );
        // fit2d .msk (case-insensitive) now resolves too (ported from fabio).
        assert_eq!(
            resolve_mask_save_format(Path::new("m.msk")).unwrap(),
            MaskFileFormat::Msk
        );
        assert_eq!(
            resolve_mask_save_format(Path::new("M.MSK")).unwrap(),
            MaskFileFormat::Msk
        );
        // A genuinely unknown extension is still rejected, not silently npy.
        assert!(resolve_mask_save_format(Path::new("m.bogus")).is_err());
    }

    #[test]
    fn mask_load_format_requires_a_known_extension() {
        use std::path::Path;
        assert_eq!(
            resolve_mask_load_format(Path::new("m.npy")).unwrap(),
            MaskFileFormat::Npy
        );
        assert_eq!(
            resolve_mask_load_format(Path::new("m.edf")).unwrap(),
            MaskFileFormat::Edf
        );
        assert_eq!(
            resolve_mask_load_format(Path::new("m.Npy")).unwrap(),
            MaskFileFormat::Npy
        );
        // TIFF (.tif/.tiff) is now a supported load format.
        assert_eq!(
            resolve_mask_load_format(Path::new("m.tiff")).unwrap(),
            MaskFileFormat::Tiff
        );
        // HDF5 extensions resolve to the H5 format.
        assert_eq!(
            resolve_mask_load_format(Path::new("m.h5")).unwrap(),
            MaskFileFormat::H5
        );
        assert_eq!(
            resolve_mask_load_format(Path::new("m.HDF5")).unwrap(),
            MaskFileFormat::H5
        );
        // fit2d .msk now resolves too.
        assert_eq!(
            resolve_mask_load_format(Path::new("m.msk")).unwrap(),
            MaskFileFormat::Msk
        );
        // A file's format cannot be guessed: a missing or unknown extension
        // errors (faithful to silx `load` raising on an unknown extension).
        assert!(resolve_mask_load_format(Path::new("mask")).is_err());
        assert!(resolve_mask_load_format(Path::new("m.bogus")).is_err());
    }

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
    fn rect_params_to_cells_matches_silx_truncation() {
        // silx _plotDrawEvent rectangle (origin 0, scale 1): row=int(y),
        // col=int(x), height=int(|height|), width=int(|width|); int() truncates
        // toward zero. (x, y) is the min corner.
        // (2.7, 3.2) corner, width 4.9, height 1.1 ->
        // row=int(3.2)=3, col=int(2.7)=2, height=int(1.1)=1, width=int(4.9)=4.
        assert_eq!(rect_params_to_cells(2.7, 3.2, 4.9, 1.1), (3, 2, 1, 4));
        // Negative corner truncates toward zero (silx int(), not floor):
        // int(-0.5) == 0.
        assert_eq!(rect_params_to_cells(-0.5, -0.5, 2.0, 2.0), (0, 0, 2, 2));
    }

    #[test]
    fn fill_from_draw_rectangle_masks_and_commits() {
        // A finished rectangle draw masks the current level over the converted
        // cells and commits to the undo history (silx _plotDrawEvent ->
        // updateRectangle -> commit). Rectangle min corner (2, 3), width 4,
        // height 1 -> rows 3..=4, cols 2..=6.
        let mut w = MaskToolsWidget::new(10, 10);
        w.level = 1;
        w.fill_from_draw(
            &DrawParams::Rectangle {
                x: 2.0,
                y: 3.0,
                width: 4.0,
                height: 1.0,
            },
            true,
        );
        for r in 3..=4 {
            for c in 2..=6 {
                assert_eq!(
                    w.mask[(r * 10 + c) as usize],
                    1,
                    "cell ({r}, {c}) should be masked"
                );
            }
        }
        // Cells just outside the rectangle stay unmasked.
        assert_eq!(w.mask[(2 * 10 + 2) as usize], 0, "row above must be clear");
        assert_eq!(w.mask[(5 * 10 + 2) as usize], 0, "row below must be clear");
        assert_eq!(w.mask[(3 * 10 + 7) as usize], 0, "col right must be clear");
        // The fill is committed (undo available).
        assert!(w.can_undo(), "fill_from_draw must commit to undo history");
    }

    #[test]
    fn effective_do_mask_xors_base_with_ctrl() {
        // silx `_isMasking()`: doMask = base, inverted while Ctrl is held.
        assert!(
            MaskToolsWidget::effective_do_mask(true, false),
            "mask, no ctrl"
        );
        assert!(
            !MaskToolsWidget::effective_do_mask(true, true),
            "mask + ctrl -> unmask"
        );
        assert!(
            !MaskToolsWidget::effective_do_mask(false, false),
            "unmask, no ctrl"
        );
        assert!(
            MaskToolsWidget::effective_do_mask(false, true),
            "unmask + ctrl -> mask"
        );
    }

    #[test]
    fn fill_from_draw_unmasks_when_do_mask_false() {
        // A shape draw with do_mask=false clears the current level over its
        // cells (silx Unmask state / Ctrl-inverted Mask state). Mask a
        // rectangle first, then unmask a sub-rectangle and confirm the overlap
        // is cleared while the rest stays masked.
        let mut w = MaskToolsWidget::new(10, 10);
        w.level = 1;
        w.fill_from_draw(
            &DrawParams::Rectangle {
                x: 2.0,
                y: 3.0,
                width: 4.0,
                height: 1.0,
            },
            true,
        );
        // Unmask a 1x1 cell inside the masked band (col 4, row 3).
        w.fill_from_draw(
            &DrawParams::Rectangle {
                x: 4.0,
                y: 3.0,
                width: 0.0,
                height: 0.0,
            },
            false,
        );
        assert_eq!(w.mask[(3 * 10 + 4) as usize], 0, "unmasked cell is cleared");
        assert_eq!(
            w.mask[(3 * 10 + 2) as usize],
            1,
            "neighbouring masked cell is untouched"
        );
    }

    #[test]
    fn fill_from_draw_ignores_unwired_shapes() {
        // Only wired shapes (gated by MaskTool::draw_mode) reach fill_from_draw;
        // an unwired param kind is a no-op and does not commit.
        let mut w = MaskToolsWidget::new(4, 4);
        w.fill_from_draw(&DrawParams::Point { x: 1.0, y: 1.0 }, true);
        assert!(w.mask.iter().all(|&v| v == 0));
        assert!(!w.can_undo(), "a no-op fill must not commit");
    }

    #[test]
    fn ellipse_params_to_cells_maps_axes_to_row_col() {
        // DrawParams::Ellipse semi_axes = (x_semi, y_semi); silx maps the y/row
        // semi-axis to radius_r and the x/col semi-axis to radius_c, and casts
        // the center to int64 (truncate toward zero); radii stay float.
        // center (5.7, 4.2), semi_axes (x=3.0, y=2.0) ->
        // crow=int(4.2)=4, ccol=int(5.7)=5, radius_r=2.0, radius_c=3.0.
        let (crow, ccol, rr, rc) = ellipse_params_to_cells((5.7, 4.2), (3.0, 2.0));
        assert_eq!((crow, ccol), (4, 5));
        assert_eq!((rr, rc), (2.0_f32, 3.0_f32));
    }

    #[test]
    fn fill_from_draw_ellipse_masks_and_commits() {
        // A finished ellipse draw masks an ellipse wider in columns than rows
        // (semi_axes x=3 > y=2) and commits to the undo history.
        let mut w = MaskToolsWidget::new(12, 12);
        w.level = 1;
        w.fill_from_draw(
            &DrawParams::Ellipse {
                center: (5.0, 5.0),
                semi_axes: (3.0, 2.0),
            },
            true,
        );
        let at = |r: i64, c: i64| w.mask[(r * 12 + c) as usize];
        assert_eq!(at(5, 5), 1, "center is masked");
        // radius_c = 3 (col) > radius_r = 2 (row): a col offset of 2 is inside,
        // the same row offset lands on the (strictly excluded) boundary.
        assert_eq!(at(5, 7), 1, "col offset 2 (< col radius 3) is masked");
        assert_eq!(
            at(7, 5),
            0,
            "row offset 2 (== row radius 2) is excluded (strict <)"
        );
        assert!(w.can_undo(), "fill_from_draw must commit");
    }

    #[test]
    fn polygon_vertices_to_cells_swaps_xy_to_row_col() {
        // silx: vertices (x, y) -> astype(int64)[:, (1, 0)] =
        // (row = int(y), col = int(x)); int() truncates toward zero.
        let cells = polygon_vertices_to_cells(&[(1.7, 2.3), (4.9, 0.1), (-0.5, 3.8)]);
        assert_eq!(cells, vec![(2, 1), (0, 4), (3, 0)]);
    }

    #[test]
    fn fill_from_draw_polygon_masks_interior_and_commits() {
        // A square polygon given in data (x, y) corners; the converter swaps to
        // (row, col) and the scanline fill masks the interior. Verifying an
        // interior cell is masked and exterior corners are not also confirms
        // the x/y -> row/col swap (a wrong swap would shift the square).
        let mut w = MaskToolsWidget::new(6, 6);
        w.level = 1;
        w.fill_from_draw(
            &DrawParams::Polygon {
                vertices: vec![(1.0, 1.0), (4.0, 1.0), (4.0, 4.0), (1.0, 4.0)],
            },
            true,
        );
        let at = |r: i64, c: i64| w.mask[(r * 6 + c) as usize];
        assert_eq!(at(2, 2), 1, "interior cell is masked");
        assert_eq!(at(0, 0), 0, "exterior corner stays unmasked");
        assert_eq!(at(5, 5), 0, "exterior corner stays unmasked");
        assert!(w.can_undo(), "fill_from_draw must commit");
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
    fn threshold_state_defaults_match_silx_group_box() {
        // silx _initThresholdGroupBox: belowThresholdAction is checked by
        // default and both line edits start at 0.
        let w = MaskToolsWidget::new(4, 1);
        assert_eq!(w.threshold_mode, ThresholdMode::Below);
        assert_eq!(w.threshold_min, 0.0);
        assert_eq!(w.threshold_max, 0.0);
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
        path.push(format!("siplot_mask_roundtrip_{}.npy", std::process::id()));
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
    fn edf_round_trips_via_read_write() {
        // A same-shape EDF write -> read is bit-identical with no resize.
        let mut src = MaskToolsWidget::new(3, 2); // width 3, height 2
        src.mask = vec![0, 1, 2, 200, 254, 255];
        let mut buf = Vec::new();
        src.write_edf(&mut buf).unwrap();

        let mut dst = MaskToolsWidget::new(3, 2);
        let resized = dst.read_edf(buf.as_slice()).unwrap();
        assert!(!resized, "same shape must not report a resize");
        assert_eq!(dst.mask, vec![0, 1, 2, 200, 254, 255]);
        assert!(dst.can_undo(), "load must commit to history");
    }

    #[test]
    fn edf_load_crops_larger_mask() {
        // Loaded 3x3 EDF mask into a 2x2 widget: crop to the top-left 2x2,
        // report a resize (the shared apply_loaded_mask crop/pad owner).
        let mut big = MaskToolsWidget::new(3, 3);
        big.mask = vec![
            1, 2, 3, //
            4, 5, 6, //
            7, 8, 9, //
        ];
        let mut buf = Vec::new();
        big.write_edf(&mut buf).unwrap();

        let mut small = MaskToolsWidget::new(2, 2);
        let resized = small.read_edf(buf.as_slice()).unwrap();
        assert!(resized, "shape mismatch must report a resize");
        assert_eq!(small.mask, vec![1, 2, 4, 5]);
    }

    #[test]
    fn tiff_round_trips_via_read_write() {
        // A same-shape TIFF write -> read is bit-identical with no resize.
        let mut src = MaskToolsWidget::new(3, 2); // width 3, height 2
        src.mask = vec![0, 1, 2, 200, 254, 255];
        let mut buf = Vec::new();
        src.write_tiff(&mut buf).unwrap();

        let mut dst = MaskToolsWidget::new(3, 2);
        let resized = dst.read_tiff(buf.as_slice()).unwrap();
        assert!(!resized, "same shape must not report a resize");
        assert_eq!(dst.mask, vec![0, 1, 2, 200, 254, 255]);
        assert!(dst.can_undo(), "load must commit to history");
    }

    #[test]
    fn tiff_load_crops_larger_mask() {
        // Loaded 3x3 TIFF mask into a 2x2 widget: crop to the top-left 2x2 and
        // report a resize (the shared apply_loaded_mask crop/pad owner).
        let mut big = MaskToolsWidget::new(3, 3);
        big.mask = vec![
            1, 2, 3, //
            4, 5, 6, //
            7, 8, 9, //
        ];
        let mut buf = Vec::new();
        big.write_tiff(&mut buf).unwrap();

        let mut small = MaskToolsWidget::new(2, 2);
        let resized = small.read_tiff(buf.as_slice()).unwrap();
        assert!(resized, "shape mismatch must report a resize");
        assert_eq!(small.mask, vec![1, 2, 4, 5]);
    }

    #[test]
    fn msk_round_trips_as_binary_via_read_write() {
        // fit2d .msk is a binary format: multi-level mask values collapse to 1
        // on write and read back as 1. Same shape -> no resize.
        let mut src = MaskToolsWidget::new(5, 2); // width 5, height 2
        src.mask = vec![0, 3, 0, 250, 1, /**/ 0, 0, 7, 0, 0];
        let mut buf = Vec::new();
        src.write_msk(&mut buf).unwrap();

        let mut dst = MaskToolsWidget::new(5, 2);
        let resized = dst.read_msk(buf.as_slice()).unwrap();
        assert!(!resized, "same shape must not report a resize");
        assert_eq!(dst.mask, vec![0, 1, 0, 1, 1, /**/ 0, 0, 1, 0, 0]);
        assert!(dst.can_undo(), "load must commit to history");
    }

    #[test]
    fn msk_load_crops_larger_mask() {
        // A 3x3 .msk loaded into a 2x2 widget crops to the top-left 2x2 (binary)
        // and reports a resize (the shared apply_loaded_mask crop/pad owner).
        let mut big = MaskToolsWidget::new(3, 3);
        big.mask = vec![
            1, 0, 9, //
            0, 5, 0, //
            7, 0, 2, //
        ];
        let mut buf = Vec::new();
        big.write_msk(&mut buf).unwrap();

        let mut small = MaskToolsWidget::new(2, 2);
        let resized = small.read_msk(buf.as_slice()).unwrap();
        assert!(resized, "shape mismatch must report a resize");
        assert_eq!(small.mask, vec![1, 0, 0, 1]);
    }

    fn h5_temp_path(tag: &str) -> String {
        let mut path = std::env::temp_dir();
        path.push(format!("siplot_mask_h5_{}_{}.h5", tag, std::process::id()));
        // save_h5 opens in append mode (silx "a"); start from a clean file so a
        // stale dataset from a previous run cannot shadow this test.
        let _ = std::fs::remove_file(&path);
        path.to_str().expect("utf-8 temp path").to_string()
    }

    #[test]
    fn h5_round_trips_via_save_load() {
        // A same-shape HDF5 write -> auto-load is bit-identical with no resize.
        let path = h5_temp_path("roundtrip");
        let mut src = MaskToolsWidget::new(3, 2); // width 3, height 2
        src.mask = vec![0, 1, 2, 200, 254, 255];
        src.save_h5(&path).expect("save h5");

        let mut dst = MaskToolsWidget::new(3, 2);
        let resized = dst.load_h5(&path).expect("load h5");
        assert!(!resized, "same shape must not report a resize");
        assert_eq!(dst.mask, vec![0, 1, 2, 200, 254, 255]);
        assert!(dst.can_undo(), "load must commit to history");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn h5_load_crops_larger_mask() {
        // A 3x3 HDF5 mask loaded into a 2x2 widget crops to the top-left 2x2 and
        // reports a resize (the shared apply_loaded_mask crop/pad owner).
        let path = h5_temp_path("crop");
        let mut big = MaskToolsWidget::new(3, 3);
        big.mask = vec![
            1, 2, 3, //
            4, 5, 6, //
            7, 8, 9, //
        ];
        big.save_h5(&path).expect("save h5");

        let mut small = MaskToolsWidget::new(2, 2);
        let resized = small.load_h5(&path).expect("load h5");
        assert!(resized, "shape mismatch must report a resize");
        assert_eq!(small.mask, vec![1, 2, 4, 5]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn h5_enumerates_then_loads_named_dataset() {
        // The dataset-selection mechanism: a saved mask is enumerated by full path
        // and loaded back by that explicit name (silx _selectDataset + _loadFromHdf5).
        let path = h5_temp_path("select");
        let mut src = MaskToolsWidget::new(2, 2);
        src.mask = vec![10, 20, 30, 40];
        src.save_h5(&path).expect("save h5");

        let datasets = src.mask_datasets(&path).expect("enumerate datasets");
        assert_eq!(datasets, vec!["mask".to_string()]);

        let mut dst = MaskToolsWidget::new(2, 2);
        let resized = dst
            .load_h5_dataset(&path, &datasets[0])
            .expect("load named dataset");
        assert!(!resized);
        assert_eq!(dst.mask, vec![10, 20, 30, 40]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn h5_save_append_preserves_existing_dataset() {
        // silx "a" mode: saving the mask into an existing HDF5 file keeps the
        // other datasets already in it (open_rw, not truncate). Seed an unrelated
        // 2D dataset, save the mask, then both must survive and read back intact.
        use crate::render::save::{read_mask_hdf5, write_mask_hdf5};
        let path = h5_temp_path("append");
        let pathbuf = std::path::Path::new(&path);
        write_mask_hdf5(pathbuf, "keep", 2, 2, &[5, 6, 7, 8]).expect("seed keep dataset");

        let mut src = MaskToolsWidget::new(2, 2);
        src.mask = vec![10, 20, 30, 40];
        src.save_h5(&path).expect("append mask into existing file");

        let mut datasets = src.mask_datasets(&path).expect("enumerate datasets");
        datasets.sort();
        assert_eq!(
            datasets,
            vec!["keep".to_string(), "mask".to_string()],
            "append must preserve the pre-existing dataset alongside the mask"
        );
        let (_, _, keep) = read_mask_hdf5(pathbuf, "keep").expect("read preserved dataset");
        assert_eq!(
            keep,
            vec![5, 6, 7, 8],
            "pre-existing data must be untouched"
        );
        let (_, _, mask) = read_mask_hdf5(pathbuf, "mask").expect("read appended mask");
        assert_eq!(mask, vec![10, 20, 30, 40]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn h5_resave_overwrites_existing_mask_dataset() {
        // silx deletes an existing dataset at the path before recreating it: a
        // second save to the same file must replace the mask, not error on a
        // duplicate name or leave the stale data.
        let path = h5_temp_path("resave");
        let mut first = MaskToolsWidget::new(2, 2);
        first.mask = vec![1, 1, 1, 1];
        first.save_h5(&path).expect("first save");

        let mut second = MaskToolsWidget::new(2, 2);
        second.mask = vec![9, 8, 7, 6];
        second.save_h5(&path).expect("re-save overwrites mask");

        let mut dst = MaskToolsWidget::new(2, 2);
        dst.load_h5(&path).expect("load h5");
        assert_eq!(dst.mask, vec![9, 8, 7, 6], "re-save must replace the mask");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn h5_picker_load_lists_datasets_seeds_first_and_loads_choice() {
        // The load picker (silx _selectDataset LoadMode): begin_h5_load parks a
        // Load picker listing every 2D dataset seeded to the first (sorted);
        // apply_h5_pick reads the chosen one.
        let path = h5_temp_path("pickload");
        let pathbuf = std::path::Path::new(&path).to_path_buf();
        let mut src = MaskToolsWidget::new(2, 2);
        src.mask = vec![1, 2, 3, 4];
        src.save_h5(&path).expect("write mask dataset");
        crate::render::save::write_mask_hdf5(&pathbuf, "other", 2, 2, &[9, 9, 9, 9])
            .expect("write other dataset");

        let mut w = MaskToolsWidget::new(2, 2);
        w.begin_h5_load(pathbuf.clone()).expect("begin load");
        let picker = w.pending_h5.clone().expect("a load pick is pending");
        assert_eq!(picker.mode, H5PickerMode::Load);
        assert_eq!(
            picker.datasets,
            vec!["mask".to_string(), "other".to_string()]
        );
        assert_eq!(
            picker.name, "mask",
            "load seeds to the first dataset (sorted)"
        );

        // Choose the second dataset and confirm.
        let mut choose = picker;
        choose.name = "other".to_string();
        w.apply_h5_pick(&choose).expect("apply load");
        assert_eq!(w.mask, vec![9, 9, 9, 9]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn h5_picker_save_defaults_to_mask_and_writes_named_dataset() {
        // The save picker (silx _selectDataset SaveMode): begin_h5_save on a new
        // file parks a Save picker (no existing datasets) seeded to silx's
        // default `mask`; apply_h5_pick writes the typed name (leading slash
        // stripped, silx lstrip('/')).
        let path = h5_temp_path("picksave");
        let pathbuf = std::path::Path::new(&path).to_path_buf();
        let mut w = MaskToolsWidget::new(2, 2);
        w.mask = vec![7, 7, 7, 7];
        w.begin_h5_save(pathbuf.clone()).expect("begin save");
        let mut picker = w.pending_h5.clone().expect("a save pick is pending");
        assert_eq!(picker.mode, H5PickerMode::Save);
        assert!(picker.datasets.is_empty(), "a new file has no datasets yet");
        assert_eq!(picker.name, "mask", "save seeds to silx default 'mask'");

        picker.name = "/custom".to_string();
        w.apply_h5_pick(&picker).expect("apply save");

        let (_, _, data) =
            crate::render::save::read_mask_hdf5(&pathbuf, "custom").expect("read named dataset");
        assert_eq!(data, vec![7, 7, 7, 7], "slash-stripped name written");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn h5_picker_resolved_path_strips_slashes_and_rejects_empty() {
        // silx DatasetDialog._updateUrl new-name rule: lstrip('/'), empty → no
        // selection (OK disabled).
        let p = std::path::PathBuf::from("/tmp/x.h5");
        let mut picker = H5DatasetPicker::for_save(p, Vec::new());
        assert_eq!(picker.resolved_path().as_deref(), Some("mask"));
        picker.name = "/leading/slash".to_string();
        assert_eq!(picker.resolved_path().as_deref(), Some("leading/slash"));
        picker.name = String::new();
        assert_eq!(picker.resolved_path(), None);
        picker.name = "///".to_string();
        assert_eq!(
            picker.resolved_path(),
            None,
            "all-slashes resolves to empty"
        );
    }

    #[test]
    fn save_mask_edf_then_load_mask_edf_round_trips_via_path_string() {
        // The in-app path-string EDF API round-trips through a real file, and
        // the on-disk bytes are exactly what the single-owner encoder produces.
        let mut src = MaskToolsWidget::new(3, 2); // width 3, height 2
        src.mask = vec![0, 1, 2, 200, 254, 255];

        let mut path = std::env::temp_dir();
        path.push(format!("siplot_mask_roundtrip_{}.edf", std::process::id()));
        let path_str = path.to_str().expect("utf-8 temp path").to_string();

        src.save_mask_edf(&path_str).expect("save");
        let mut dst = MaskToolsWidget::new(3, 2);
        let resized = dst.load_mask_edf(&path_str).expect("load");
        assert!(!resized, "same shape must not report a resize");
        assert_eq!(dst.mask, vec![0, 1, 2, 200, 254, 255]);

        let on_disk = std::fs::read(&path_str).expect("read back file");
        let expected = crate::render::save::encode_mask_edf(2, 3, &src.mask);
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
    fn current_mask_color_reports_override_then_falls_back_to_base() {
        // silx getCurrentMaskColor (_BaseMaskToolsWidget.py:973-982): the
        // current level's override if set, else the base overlay color. This is
        // what the toolbar swatch reads/writes for the per-level color UI.
        let mut w = MaskToolsWidget::new(2, 2);
        w.level = 4;
        // No override yet -> base color (the default gray).
        assert_eq!(w.current_mask_color(), w.color);

        // Set this level's override -> the swatch reflects it.
        w.set_mask_colors([255, 0, 0], Some(4));
        assert_eq!(w.current_mask_color(), Color32::from_rgb(255, 0, 0));

        // A different level is unaffected and still shows the base color.
        w.level = 5;
        assert_eq!(w.current_mask_color(), w.color);

        // Clearing the override drops level 4 back to the base color.
        w.level = 4;
        w.reset_mask_colors(Some(4));
        assert_eq!(w.current_mask_color(), w.color);
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
