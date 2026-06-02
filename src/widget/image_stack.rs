//! Browse an in-memory ordered list of 2D frames with a navigation slider,
//! a frame table, per-frame visibility, and a waiting overlay for empty slots.
//!
//! Ports silx `ImageStack.py`. The upstream widget loads images lazily from a
//! list of `DataUrl`s, prefetching neighbours on background threads; that
//! file-IO / threading machinery (`UrlLoader`, `_preFetch`, `silx.io`) is out
//! of scope here. This port accepts frames already resident in memory as a
//! [`Frame`] list, each carrying its own dimensions and label, and reproduces
//! the navigation behaviour:
//!
//! - a current-frame index driven by a slider with first/prev/next/last
//!   stepping (silx `_HorizontalSlider` / `FrameBrowser`, `ImageStack.py`
//!   :337-392 / `FrameBrowser.py` :129-220),
//! - a frame table listing each frame's label with selectable rows
//!   (silx `_ToggleableUrlSelectionTable`, `ImageStack.py` :65-128),
//! - a per-frame visibility toggle (the per-row checkbox the task adds on top
//!   of the silx selection table),
//! - display of the current frame through the same [`Plot2D`] backend path
//!   `ComplexImageView` uses to push an image,
//! - a centred "waiting" spinner overlay shown when the current frame slot is
//!   empty/`None` or hidden, mirroring silx `WaitingOverlay`
//!   (`ImageStack.py` :43, the `_plot.clear()` + `overlay.setVisible(True)`
//!   branch in `setCurrentUrl`, :516-527).
//!
//! Frame-navigation logic (index clamping, prev/next stepping, visibility
//! filtering) lives in the pure [`FrameNav`] state struct, so it is
//! unit-testable without a GPU; the GPU-backed [`ImageStack`] delegates to it.

use egui_wgpu::RenderState;

use crate::core::backend::ItemHandle;
use crate::core::colormap::Colormap;
use crate::core::plot::PlotId;
use crate::widget::high_level::Plot2D;

/// One 2D frame in an [`ImageStack`]: a row-major `f32` image with its
/// dimensions and an optional display label.
///
/// `data.len()` is expected to equal `width * height`; a frame whose data does
/// not match is treated as having no displayable image (it falls back to the
/// waiting overlay, like an empty slot).
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Row-major scalar samples, length `width * height`.
    pub data: Vec<f32>,
    /// Optional label shown in the frame table; falls back to `Frame N`.
    pub label: Option<String>,
}

impl Frame {
    /// Create a frame from explicit dimensions and row-major data.
    pub fn new(width: u32, height: u32, data: Vec<f32>, label: Option<String>) -> Self {
        Self {
            width,
            height,
            data,
            label,
        }
    }

    /// `true` when `data.len()` matches `width * height` and is non-empty, i.e.
    /// the frame holds a displayable image.
    pub fn is_displayable(&self) -> bool {
        let expected = (self.width as usize).saturating_mul(self.height as usize);
        expected != 0 && self.data.len() == expected
    }
}

/// Pure frame-navigation state: the ordered frame slots, the current index, the
/// per-frame visibility flags, and a `dirty` flag noting the displayed image
/// must be rebuilt.
///
/// All navigation, visibility, and label logic lives here as pure methods so it
/// can be unit-tested without constructing a GPU-backed [`Plot2D`].
/// [`ImageStack`] owns one of these and delegates to it.
#[derive(Clone, Debug, Default, PartialEq)]
struct FrameNav {
    /// Ordered frame slots; `None` is an empty/loading slot (waiting overlay).
    frames: Vec<Option<Frame>>,
    /// Current frame index, always in `[0, frames.len())` while non-empty.
    current: usize,
    /// Per-frame visibility; always the same length as `frames`.
    visible: Vec<bool>,
    /// Set when the current frame changed and the plot image needs rebuilding.
    dirty: bool,
}

impl FrameNav {
    /// Replace the whole frame list, resetting the index to 0 and marking every
    /// frame visible (silx `setUrls` -> `reset` then re-add).
    fn set_frames(&mut self, frames: Vec<Option<Frame>>) {
        self.visible = vec![true; frames.len()];
        self.frames = frames;
        self.current = 0;
        self.dirty = true;
    }

    fn len(&self) -> usize {
        self.frames.len()
    }

    fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Jump to frame `index`, clamped to `[0, len)`. An empty stack is a no-op.
    ///
    /// Mirrors `FrameBrowser.setValue`, which clips the value to the valid
    /// range; silx raises on an out-of-range *programmatic* index, but the
    /// slider/browser path it is wired to always clamps, so this port clamps
    /// uniformly.
    fn set_current(&mut self, index: usize) {
        if self.frames.is_empty() {
            return;
        }
        let clamped = index.min(self.frames.len() - 1);
        if clamped != self.current {
            self.current = clamped;
            self.dirty = true;
        }
    }

    /// Step to the next frame, clamping at the last (silx `_nextClicked`).
    fn next_frame(&mut self) {
        if self.frames.is_empty() {
            return;
        }
        self.set_current(self.current + 1);
    }

    /// Step to the previous frame, clamping at the first (silx `_previousClicked`).
    fn prev_frame(&mut self) {
        if self.current > 0 {
            self.set_current(self.current - 1);
        }
    }

    /// Jump to the first frame (silx `_firstClicked`).
    fn first_frame(&mut self) {
        self.set_current(0);
    }

    /// Jump to the last frame (silx `_lastClicked`). No-op on an empty stack.
    fn last_frame(&mut self) {
        if !self.frames.is_empty() {
            self.set_current(self.frames.len() - 1);
        }
    }

    /// Whether frame `index` is currently visible. Out-of-range is `false`.
    fn is_visible(&self, index: usize) -> bool {
        self.visible.get(index).copied().unwrap_or(false)
    }

    /// Set frame `index`'s visibility. Out-of-range is a no-op. Marks dirty only
    /// when the current frame's visibility actually flips.
    fn set_visible(&mut self, index: usize, visible: bool) {
        if let Some(slot) = self.visible.get_mut(index)
            && *slot != visible
        {
            *slot = visible;
            if index == self.current {
                self.dirty = true;
            }
        }
    }

    /// The label shown for frame `index`: the frame's own label, or `Frame N`
    /// (1-based) when unset, or `Frame N (empty)` for an empty slot.
    /// Out-of-range returns an empty string.
    fn frame_label(&self, index: usize) -> String {
        match self.frames.get(index) {
            None => String::new(),
            Some(None) => format!("Frame {} (empty)", index + 1),
            Some(Some(frame)) => match &frame.label {
                Some(label) => label.clone(),
                None => format!("Frame {}", index + 1),
            },
        }
    }

    /// `true` when the current frame slot holds a displayable, visible image;
    /// `false` for an empty slot, a hidden frame, or a length-mismatched frame,
    /// in which cases the waiting overlay is shown.
    fn current_is_displayable(&self) -> bool {
        if !self.is_visible(self.current) {
            return false;
        }
        matches!(self.frames.get(self.current), Some(Some(frame)) if frame.is_displayable())
    }

    /// Borrow the current frame's displayable image, or `None` for an empty /
    /// hidden / invalid slot.
    fn current_frame(&self) -> Option<&Frame> {
        if !self.current_is_displayable() {
            return None;
        }
        self.frames.get(self.current).and_then(Option::as_ref)
    }
}

/// A navigation action selected from the toolbar this frame (silx
/// `FrameBrowser` first/prev/next/last buttons and the slider).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NavAction {
    First,
    Previous,
    Next,
    Last,
    /// Jump to an absolute index (the slider).
    Goto(usize),
}

impl NavAction {
    /// Apply this action to `nav`, mirroring the silx `FrameBrowser` slots.
    fn apply(self, nav: &mut FrameNav) {
        match self {
            NavAction::First => nav.first_frame(),
            NavAction::Previous => nav.prev_frame(),
            NavAction::Next => nav.next_frame(),
            NavAction::Last => nav.last_frame(),
            NavAction::Goto(index) => nav.set_current(index),
        }
    }
}

/// Render the first/prev slider next/last navigation row with a `cur/len`
/// counter and return the action the user selected this frame, or `None` if no
/// control was activated. An empty stack (`len == 0`) renders nothing.
///
/// Pure over an [`egui::Ui`] plus the `current` index and `len` (no GPU /
/// [`Plot2D`]), so the toolbar's button/slider behaviour is unit-testable with
/// a headless egui context. [`ImageStack::navigation_ui`] applies the returned
/// [`NavAction`] to its [`FrameNav`].
fn navigation_toolbar_ui(ui: &mut egui::Ui, current: usize, len: usize) -> Option<NavAction> {
    if len == 0 {
        return None;
    }
    let mut action = None;
    ui.horizontal(|ui| {
        if ui.button("⏮").on_hover_text("First frame").clicked() {
            action = Some(NavAction::First);
        }
        if ui.button("◀").on_hover_text("Previous frame").clicked() {
            action = Some(NavAction::Previous);
        }
        let mut idx = current;
        if ui
            .add(egui::Slider::new(&mut idx, 0..=len - 1).text("frame"))
            .changed()
        {
            action = Some(NavAction::Goto(idx));
        }
        if ui.button("▶").on_hover_text("Next frame").clicked() {
            action = Some(NavAction::Next);
        }
        if ui.button("⏭").on_hover_text("Last frame").clicked() {
            action = Some(NavAction::Last);
        }
        ui.label(format!("{}/{}", current + 1, len));
    });
    action
}

/// Browse an in-memory ordered list of 2D frames.
///
/// Owns an internal [`Plot2D`] (like `ComplexImageView`/`StackView`) plus the
/// pure [`FrameNav`] navigation state. The displayed image is rebuilt in place
/// from the current slot, reusing the plot item handle so browsing does not
/// reset the zoom unless [`Self::set_auto_reset_zoom`] is enabled (silx
/// `_autoResetZoom`, default `true`).
///
/// ```ignore
/// let mut stack = ImageStack::new(render_state, 0);
/// stack.set_frames(vec![
///     Some(Frame::new(2, 2, vec![0.0, 1.0, 2.0, 3.0], Some("a".into()))),
///     None, // empty slot -> waiting overlay
/// ]);
///
/// // frame loop
/// stack.ui(ui);
/// ```
pub struct ImageStack {
    plot: Plot2D,
    nav: FrameNav,
    colormap: Colormap,
    image_handle: Option<ItemHandle>,
    auto_reset_zoom: bool,
    show_table: bool,
}

impl ImageStack {
    /// Create an empty image stack backed by wgpu plot id `id`.
    pub fn new(render_state: &RenderState, id: PlotId) -> Self {
        let mut plot = Plot2D::new(render_state, id);
        plot.set_keep_data_aspect_ratio(true);
        plot.set_graph_cursor(true);
        Self {
            plot,
            nav: FrameNav::default(),
            colormap: Colormap::viridis(0.0, 1.0),
            image_handle: None,
            // silx ImageStack defaults _autoResetZoom = True.
            auto_reset_zoom: true,
            show_table: true,
        }
    }

    /// Replace the whole frame list, resetting the current index to 0 and
    /// marking every frame visible (silx `setUrls` -> `reset` then re-add).
    pub fn set_frames(&mut self, frames: Vec<Option<Frame>>) {
        self.nav.set_frames(frames);
        self.image_handle = None;
        self.plot.clear_images();
    }

    /// Number of frame slots in the stack.
    pub fn len(&self) -> usize {
        self.nav.len()
    }

    /// `true` when the stack holds no frame slots.
    pub fn is_empty(&self) -> bool {
        self.nav.is_empty()
    }

    /// Index of the current frame.
    pub fn current(&self) -> usize {
        self.nav.current
    }

    /// Jump to frame `index`, clamped to `[0, len)`. An empty stack is a no-op.
    pub fn set_current(&mut self, index: usize) {
        self.nav.set_current(index);
    }

    /// Step to the next frame, clamping at the last. No-op at the last frame.
    pub fn next_frame(&mut self) {
        self.nav.next_frame();
    }

    /// Step to the previous frame, clamping at the first. No-op at the first.
    pub fn prev_frame(&mut self) {
        self.nav.prev_frame();
    }

    /// Jump to the first frame (silx `_firstClicked`).
    pub fn first_frame(&mut self) {
        self.nav.first_frame();
    }

    /// Jump to the last frame (silx `_lastClicked`). No-op on an empty stack.
    pub fn last_frame(&mut self) {
        self.nav.last_frame();
    }

    /// Whether frame `index` is currently visible. Out-of-range is `false`.
    pub fn is_visible(&self, index: usize) -> bool {
        self.nav.is_visible(index)
    }

    /// Set frame `index`'s visibility. Out-of-range is a no-op.
    pub fn set_visible(&mut self, index: usize, visible: bool) {
        self.nav.set_visible(index, visible);
    }

    /// Toggle frame `index`'s visibility. Out-of-range is a no-op.
    pub fn toggle_visible(&mut self, index: usize) {
        let now = self.nav.is_visible(index);
        self.nav.set_visible(index, !now);
    }

    /// The label shown for frame `index` in the table: the frame's own label,
    /// `Frame N` (1-based) when unset, or `Frame N (empty)` for an empty slot.
    /// Out-of-range returns an empty string.
    pub fn frame_label(&self, index: usize) -> String {
        self.nav.frame_label(index)
    }

    /// `true` when the current frame slot holds a displayable, visible image;
    /// `false` for an empty slot, a hidden frame, or a length-mismatched frame,
    /// in which cases the waiting overlay is shown (silx: empty-slot branch of
    /// `setCurrentUrl` clears the plot and shows `WaitingOverlay`).
    pub fn current_is_displayable(&self) -> bool {
        self.nav.current_is_displayable()
    }

    /// Set the colormap applied to every frame (silx frames use the plot's
    /// active colormap; this exposes it directly).
    pub fn set_colormap(&mut self, colormap: Colormap) {
        self.colormap = colormap;
        self.image_handle = None;
        self.plot.clear_images();
        self.nav.dirty = true;
    }

    /// Whether to reset the zoom when the displayed frame changes
    /// (silx `setAutoResetZoom`, default `true`).
    pub fn set_auto_reset_zoom(&mut self, reset: bool) {
        self.auto_reset_zoom = reset;
    }

    /// `true` when the displayed frame change resets the zoom
    /// (silx `isAutoResetZoom`).
    pub fn is_auto_reset_zoom(&self) -> bool {
        self.auto_reset_zoom
    }

    /// Toggle whether the frame table is shown (silx toggle button,
    /// `_ToggleableUrlSelectionTable.toggleUrlSelectionTable`).
    pub fn set_table_visible(&mut self, visible: bool) {
        self.show_table = visible;
    }

    /// Access the underlying [`Plot2D`].
    pub fn plot(&self) -> &Plot2D {
        &self.plot
    }

    /// Mutably access the underlying [`Plot2D`].
    pub fn plot_mut(&mut self) -> &mut Plot2D {
        &mut self.plot
    }

    /// Rebuild the displayed image from the current slot, reusing the existing
    /// item handle when possible so the zoom is preserved (silx
    /// `addImage(resetzoom=self._autoResetZoom)`; when auto-reset is on we drop
    /// the handle so the plot re-fits the new frame).
    fn rebuild_image(&mut self) {
        let Some(frame) = self.nav.current_frame() else {
            // Empty / hidden / invalid slot: clear the plot like silx's
            // `_plot.clear()` branch; the waiting overlay is painted in `ui`.
            self.plot.clear_images();
            self.image_handle = None;
            return;
        };
        let (width, height) = (frame.width, frame.height);
        // Clone the row data out so the immutable borrow of `self.nav` ends
        // before the mutable plot borrow begins.
        let data = frame.data.clone();
        let colormap = self.colormap.clone();

        if self.auto_reset_zoom {
            // Re-add to refit the view to the new frame.
            self.plot.clear_images();
            self.image_handle = None;
        }
        if let Some(handle) = self.image_handle
            && self
                .plot
                .try_update_image(handle, width, height, &data, colormap.clone())
                .unwrap_or(false)
        {
            return;
        }
        if let Ok(handle) = self.plot.try_add_image(width, height, &data, colormap) {
            self.image_handle = Some(handle);
        }
    }

    /// Render the composite widget: an optional frame table, a navigation row
    /// (first/prev slider next/last with a counter), and the plot with the
    /// waiting overlay drawn on top when the current slot has no image.
    ///
    /// Returns the egui [`Response`](egui::Response) of the plot area.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> egui::Response {
        if self.show_table {
            self.frame_table_ui(ui);
        }
        self.navigation_ui(ui);

        if self.nav.dirty {
            self.rebuild_image();
            self.nav.dirty = false;
        }

        let response = self.plot.show(ui).response;
        if !self.nav.current_is_displayable() && !self.nav.is_empty() {
            Self::paint_waiting_overlay(ui, response.rect);
        }
        response
    }

    /// The frame/URL table: one selectable row per frame slot with a per-frame
    /// visibility checkbox (silx `_ToggleableUrlSelectionTable` + `UrlList`,
    /// extended with the visibility toggle the task requires).
    fn frame_table_ui(&mut self, ui: &mut egui::Ui) {
        let len = self.nav.len();
        if len == 0 {
            ui.label("No frames");
            return;
        }
        let current = self.nav.current;
        egui::ScrollArea::vertical()
            .max_height(120.0)
            .show(ui, |ui| {
                for index in 0..len {
                    ui.horizontal(|ui| {
                        let mut vis = self.nav.is_visible(index);
                        if ui
                            .checkbox(&mut vis, "")
                            .on_hover_text("Show this frame")
                            .changed()
                        {
                            self.nav.set_visible(index, vis);
                        }
                        let label = self.nav.frame_label(index);
                        if ui.selectable_label(index == current, label).clicked() {
                            self.nav.set_current(index);
                        }
                    });
                }
            });
    }

    /// Show only the navigation toolbar (first/prev slider next/last with a
    /// `cur/len` counter), without the frame table or plot. A standalone entry
    /// point over the same row [`Self::ui`] draws, for callers that lay the
    /// plot out themselves (silx `FrameBrowser` / `HorizontalSliderWithBrowser`).
    pub fn show_toolbar(&mut self, ui: &mut egui::Ui) {
        self.navigation_ui(ui);
    }

    /// The first/prev slider next/last navigation row with a `cur/len` counter,
    /// mirroring silx `FrameBrowser` + `HorizontalSliderWithBrowser`. Renders
    /// via the pure [`navigation_toolbar_ui`] and applies the returned action to
    /// the frame nav.
    fn navigation_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(action) = navigation_toolbar_ui(ui, self.nav.current, self.nav.len()) {
            action.apply(&mut self.nav);
        }
    }

    /// Paint a centred spinner with a "Loading…" caption over `rect`,
    /// reproducing silx `WaitingOverlay` (a processing wheel centred on the
    /// plot). No real threading is involved.
    fn paint_waiting_overlay(ui: &egui::Ui, rect: egui::Rect) {
        if !ui.is_rect_visible(rect) {
            return;
        }
        let painter = ui.painter_at(rect);
        // Translucent grey backing, matching silx's rgba(150,150,150,40) plate.
        let spinner_size = 30.0;
        let plate = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(spinner_size + 24.0, spinner_size + 36.0),
        );
        painter.rect_filled(
            plate,
            10.0,
            egui::Color32::from_rgba_unmultiplied(150, 150, 150, 40),
        );
        let spinner_rect = egui::Rect::from_center_size(
            rect.center() - egui::vec2(0.0, 8.0),
            egui::vec2(spinner_size, spinner_size),
        );
        egui::Spinner::new()
            .size(spinner_size)
            .paint_at(ui, spinner_rect);
        painter.text(
            rect.center() + egui::vec2(0.0, spinner_size / 2.0),
            egui::Align2::CENTER_CENTER,
            "Loading…",
            egui::FontId::proportional(12.0),
            ui.visuals().strong_text_color(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(label: &str) -> Option<Frame> {
        Some(Frame::new(
            2,
            2,
            vec![0.0, 1.0, 2.0, 3.0],
            Some(label.to_string()),
        ))
    }

    fn nav(frames: Vec<Option<Frame>>) -> FrameNav {
        let mut n = FrameNav::default();
        n.set_frames(frames);
        n
    }

    // ── Frame::is_displayable boundaries ────────────────────────────────────

    #[test]
    fn displayable_matches_dims() {
        assert!(Frame::new(2, 2, vec![0.0; 4], None).is_displayable());
    }

    #[test]
    fn displayable_false_on_length_mismatch() {
        // 3 != 2*2.
        assert!(!Frame::new(2, 2, vec![0.0; 3], None).is_displayable());
    }

    #[test]
    fn displayable_false_on_zero_dims() {
        // width*height == 0 -> not displayable even with empty matching data.
        assert!(!Frame::new(0, 0, vec![], None).is_displayable());
        assert!(!Frame::new(2, 0, vec![], None).is_displayable());
    }

    // ── set_current clamping ────────────────────────────────────────────────

    #[test]
    fn set_current_clamps_to_last() {
        let mut n = nav(vec![frame("a"), frame("b"), frame("c")]);
        n.set_current(99);
        assert_eq!(n.current, 2); // clamped to len-1.
    }

    #[test]
    fn set_current_in_range_is_exact() {
        let mut n = nav(vec![frame("a"), frame("b"), frame("c")]);
        n.set_current(1);
        assert_eq!(n.current, 1);
    }

    #[test]
    fn empty_stack_set_current_is_noop() {
        let mut n = nav(vec![]);
        n.set_current(5);
        assert_eq!(n.current, 0);
        assert_eq!(n.len(), 0);
    }

    // ── prev/next at boundaries ─────────────────────────────────────────────

    #[test]
    fn next_at_last_stays() {
        let mut n = nav(vec![frame("a"), frame("b")]);
        n.set_current(1);
        n.next_frame();
        assert_eq!(n.current, 1); // clamped at last.
    }

    #[test]
    fn prev_at_first_stays() {
        let mut n = nav(vec![frame("a"), frame("b")]);
        assert_eq!(n.current, 0);
        n.prev_frame();
        assert_eq!(n.current, 0); // clamped at first.
    }

    #[test]
    fn next_then_prev_round_trips() {
        let mut n = nav(vec![frame("a"), frame("b"), frame("c")]);
        n.next_frame();
        assert_eq!(n.current, 1);
        n.next_frame();
        assert_eq!(n.current, 2);
        n.prev_frame();
        assert_eq!(n.current, 1);
    }

    #[test]
    fn empty_stack_next_prev_are_noops() {
        let mut n = nav(vec![]);
        n.next_frame();
        n.prev_frame();
        assert_eq!(n.current, 0);
    }

    // ── first/last ──────────────────────────────────────────────────────────

    #[test]
    fn first_and_last_jump_to_ends() {
        let mut n = nav(vec![frame("a"), frame("b"), frame("c"), frame("d")]);
        n.last_frame();
        assert_eq!(n.current, 3);
        n.first_frame();
        assert_eq!(n.current, 0);
    }

    #[test]
    fn last_on_empty_is_noop() {
        let mut n = nav(vec![]);
        n.last_frame();
        assert_eq!(n.current, 0);
    }

    // ── visibility filtering ────────────────────────────────────────────────

    #[test]
    fn visibility_defaults_true_and_toggles() {
        let mut n = nav(vec![frame("a"), frame("b")]);
        assert!(n.is_visible(0));
        assert!(n.is_visible(1));
        n.set_visible(0, false);
        assert!(!n.is_visible(0));
        assert!(n.is_visible(1));
    }

    #[test]
    fn visibility_out_of_range_is_false_and_noop() {
        let mut n = nav(vec![frame("a")]);
        assert!(!n.is_visible(7));
        n.set_visible(7, true); // no panic, no effect.
        assert_eq!(n.visible, vec![true]);
    }

    #[test]
    fn hidden_current_frame_is_not_displayable() {
        let mut n = nav(vec![frame("a")]);
        assert!(n.current_is_displayable());
        n.set_visible(0, false);
        assert!(!n.current_is_displayable());
        assert!(n.current_frame().is_none());
    }

    #[test]
    fn empty_slot_is_not_displayable() {
        // None slot -> waiting overlay path, never displayable even if visible.
        let n = nav(vec![None]);
        assert!(n.is_visible(0));
        assert!(!n.current_is_displayable());
        assert!(n.current_frame().is_none());
    }

    #[test]
    fn invalid_length_frame_is_not_displayable() {
        let n = nav(vec![Some(Frame::new(2, 2, vec![0.0; 3], None))]);
        assert!(!n.current_is_displayable());
        assert!(n.current_frame().is_none());
    }

    #[test]
    fn current_frame_returns_visible_displayable_slot() {
        let n = nav(vec![frame("a")]);
        let f = n.current_frame().expect("displayable");
        assert_eq!(f.label.as_deref(), Some("a"));
    }

    // ── dirty flag transitions ──────────────────────────────────────────────

    #[test]
    fn set_frames_marks_dirty() {
        let n = nav(vec![frame("a")]);
        assert!(n.dirty);
    }

    #[test]
    fn redundant_set_current_does_not_mark_dirty() {
        let mut n = nav(vec![frame("a"), frame("b")]);
        n.dirty = false;
        n.set_current(0); // already 0.
        assert!(!n.dirty);
        n.set_current(1); // changes -> dirty.
        assert!(n.dirty);
    }

    #[test]
    fn hiding_non_current_frame_does_not_mark_dirty() {
        let mut n = nav(vec![frame("a"), frame("b")]);
        n.dirty = false;
        n.set_visible(1, false); // current is 0, so no rebuild needed.
        assert!(!n.dirty);
        n.set_visible(0, false); // current -> dirty.
        assert!(n.dirty);
    }

    // ── frame_label fallbacks ───────────────────────────────────────────────

    #[test]
    fn frame_label_uses_label_then_index_then_empty_marker() {
        let n = nav(vec![
            frame("custom"),
            Some(Frame::new(1, 1, vec![0.0], None)),
            None,
        ]);
        assert_eq!(n.frame_label(0), "custom");
        assert_eq!(n.frame_label(1), "Frame 2"); // 1-based, no label.
        assert_eq!(n.frame_label(2), "Frame 3 (empty)");
        assert_eq!(n.frame_label(99), ""); // out of range.
    }

    // ── Navigation toolbar (Item 4) ─────────────────────────────────────────

    /// Capture the toolbar widget rects by running one headless layout frame
    /// with the same sequence `navigation_toolbar_ui` emits. The geometry is
    /// deterministic, so these rects line up with the real call.
    fn capture_toolbar_rects(
        ctx: &egui::Context,
        current: usize,
        len: usize,
    ) -> (egui::Rect, egui::Rect, egui::Rect, egui::Rect) {
        let mut first = egui::Rect::NOTHING;
        let mut prev = egui::Rect::NOTHING;
        let mut next = egui::Rect::NOTHING;
        let mut last = egui::Rect::NOTHING;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.horizontal(|ui| {
                first = ui.button("⏮").rect;
                prev = ui.button("◀").rect;
                let mut idx = current;
                let _ = ui.add(egui::Slider::new(&mut idx, 0..=len - 1).text("frame"));
                next = ui.button("▶").rect;
                last = ui.button("⏭").rect;
                let _ = ui.label(format!("{}/{}", current + 1, len));
            });
        });
        (first, prev, next, last)
    }

    fn run_toolbar(
        ctx: &egui::Context,
        current: usize,
        len: usize,
        raw: egui::RawInput,
    ) -> Option<NavAction> {
        let mut action = None;
        let _ = ctx.run_ui(raw, |ui| {
            action = navigation_toolbar_ui(ui, current, len);
        });
        action
    }

    fn click_at(point: egui::Pos2) -> egui::RawInput {
        egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn empty_toolbar_renders_nothing_and_returns_none() {
        let ctx = egui::Context::default();
        assert_eq!(run_toolbar(&ctx, 0, 0, egui::RawInput::default()), None);
    }

    #[test]
    fn next_button_click_yields_next_action() {
        let ctx = egui::Context::default();
        let (_first, _prev, next, _last) = capture_toolbar_rects(&ctx, 0, 3);
        let _ = run_toolbar(&ctx, 0, 3, egui::RawInput::default());
        let action = run_toolbar(&ctx, 0, 3, click_at(next.center()));
        assert_eq!(action, Some(NavAction::Next));
    }

    #[test]
    fn prev_first_last_button_clicks_yield_their_actions() {
        let ctx = egui::Context::default();
        let (first, prev, _next, last) = capture_toolbar_rects(&ctx, 1, 3);

        let _ = run_toolbar(&ctx, 1, 3, egui::RawInput::default());
        assert_eq!(
            run_toolbar(&ctx, 1, 3, click_at(prev.center())),
            Some(NavAction::Previous)
        );
        let _ = run_toolbar(&ctx, 1, 3, egui::RawInput::default());
        assert_eq!(
            run_toolbar(&ctx, 1, 3, click_at(first.center())),
            Some(NavAction::First)
        );
        let _ = run_toolbar(&ctx, 1, 3, egui::RawInput::default());
        assert_eq!(
            run_toolbar(&ctx, 1, 3, click_at(last.center())),
            Some(NavAction::Last)
        );
    }

    #[test]
    fn toolbar_actions_keep_index_in_bounds_when_applied() {
        // Apply each toolbar action to a FrameNav and confirm the index stays
        // within [0, len): Next clamps at the last, Previous at the first.
        let mut n = nav(vec![frame("a"), frame("b"), frame("c")]);
        NavAction::Next.apply(&mut n);
        assert_eq!(n.current, 1);
        NavAction::Last.apply(&mut n);
        assert_eq!(n.current, 2);
        NavAction::Next.apply(&mut n); // already last -> clamp.
        assert_eq!(n.current, 2);
        NavAction::Goto(99).apply(&mut n); // slider clamps to len-1.
        assert_eq!(n.current, 2);
        NavAction::First.apply(&mut n);
        assert_eq!(n.current, 0);
        NavAction::Previous.apply(&mut n); // already first -> clamp.
        assert_eq!(n.current, 0);
        NavAction::Goto(1).apply(&mut n);
        assert_eq!(n.current, 1);
    }
}
