//! Browse an ordered list of 2D frames with a navigation slider, a frame
//! table, per-frame visibility, and a waiting overlay for empty/loading slots.
//!
//! Ports silx `ImageStack.py`, with both upstream modes:
//!
//! - **In-memory** ([`ImageStack::set_frames`]): frames already resident as a
//!   [`Frame`] list, each carrying its own dimensions and label.
//! - **Lazy** ([`ImageStack::set_sources`] + [`ImageStack::set_loader`]): a list
//!   of opaque source strings whose frames are loaded on demand on background
//!   threads through a pluggable [`FrameLoader`] (silx `UrlLoader` /
//!   `setUrlLoaderClass`); the current slot — and a configurable prefetch radius
//!   of neighbours on each side ([`ImageStack::set_n_prefetch`], silx
//!   `_preFetch` / `N_PRELOAD`) — is loaded as you browse and the results are
//!   drained back on the UI thread (silx `_urlLoaded`).
//!
//! Either mode reproduces the navigation behaviour:
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
//! filtering) lives in the pure `FrameNav` state struct, so it is
//! unit-testable without a GPU; the GPU-backed [`ImageStack`] delegates to it.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use egui_wgpu::RenderState;

use crate::core::backend::ItemHandle;
use crate::core::colormap::{AutoscaleMode, Colormap, ColormapName};
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

/// Loads a [`Frame`] for a source string on demand, off the UI thread.
///
/// This is the pluggable seam silx exposes through `ImageStack.setUrlLoaderClass`
/// (`ImageStack.py` :207-223): the stack does not know how to turn a `DataUrl`
/// into pixels — it delegates to a loader. [`ImageStack::set_sources`] hands the
/// loader one opaque `source` string (an HDF5/image URL, a file path — whatever
/// the concrete loader understands) per slot; [`load`](Self::load) is then
/// called on a background thread when that slot is browsed to or prefetched.
///
/// Implementors MUST be `Send + Sync` (the load runs on a spawned thread) and
/// SHOULD return `None` rather than panicking when a source cannot be read,
/// mirroring silx `UrlLoader.run` returning `None` on `OSError` (`ImageStack.py`
/// :141-145). A `None` result marks the slot failed and is not retried.
pub trait FrameLoader: Send + Sync {
    /// Load the frame for `source`, or `None` if it cannot be read. Runs on a
    /// background thread; keep it self-contained (no shared mutable UI state).
    fn load(&self, source: &str) -> Option<Frame>;
}

/// A [`FrameLoader`] that reads frames from HDF5 datasets — the common
/// `ImageStack` source (silx resolves a `DataUrl` through `silx.io.get_data`).
///
/// Each source string is one of:
///
/// - `"<file>::<dataset>"` — a 2D dataset, read as one frame
///   ([`read_image_hdf5`](crate::render::save::read_image_hdf5)).
/// - `"<file>::<dataset>::<index>"` — frame `index` of a 3D `[N, H, W]` stack
///   dataset; only that slice is read off disk
///   ([`read_image_hdf5_slice`](crate::render::save::read_image_hdf5_slice)),
///   which is the point of the lazy path.
///
/// Float datasets only — see
/// [`read_image_hdf5`](crate::render::save::read_image_hdf5) for the dtype
/// caveat (the dependency exposes only the element byte size). Any failure
/// (missing file/dataset, wrong rank, unsupported dtype, out-of-range slice, or
/// a malformed source string) yields `None`, matching silx `UrlLoader.run`
/// returning `None` on `OSError`. Each `load` opens its own file handle, so it
/// is safe to call from several prefetch threads at once.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hdf5FrameLoader;

impl FrameLoader for Hdf5FrameLoader {
    fn load(&self, source: &str) -> Option<Frame> {
        let parts: Vec<&str> = source.split("::").collect();
        let (path, data_path, slice) = match parts.as_slice() {
            [path, data_path] => (*path, *data_path, None),
            [path, data_path, index] => (*path, *data_path, Some(index.parse::<usize>().ok()?)),
            _ => return None,
        };
        let p = std::path::Path::new(path);
        // read_image_hdf5* return (height, width, row-major data); Frame is
        // (width, height, data).
        let (height, width, data) = match slice {
            None => crate::render::save::read_image_hdf5(p, data_path).ok()?,
            Some(index) => crate::render::save::read_image_hdf5_slice(p, data_path, index).ok()?,
        };
        Some(Frame::new(width, height, data, Some(source.to_string())))
    }
}

/// Per-source load state for the lazy/threaded loading path.
///
/// A slot is `NotRequested` until its background load is dispatched
/// (`InFlight`), then `Loaded` (its frame is resident in the nav) or `Failed`
/// (the loader returned `None`). `Failed` is terminal and never re-dispatched,
/// mirroring silx storing the `None` result in `_urlData` so it is not retried.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum LoadState {
    #[default]
    NotRequested,
    InFlight,
    Loaded,
    Failed,
}

/// Pure per-slot load bookkeeping for the threaded loader: one [`LoadState`] per
/// source slot.
///
/// Decides which slots still need dispatching ([`needs_load`](Self::needs_load)
/// / [`to_dispatch`](Self::to_dispatch)) and records each outcome, with no GPU
/// or threads, so the dispatch/dedup logic is unit-testable in isolation.
/// [`ImageStack`] owns one and consults it before spawning a load thread.
#[derive(Clone, Debug, Default, PartialEq)]
struct LoadSchedule {
    state: Vec<LoadState>,
}

impl LoadSchedule {
    /// Reset to `len` slots, all `NotRequested` (silx `reset` clears `_urlData`).
    fn reset(&mut self, len: usize) {
        self.state = vec![LoadState::NotRequested; len];
    }

    /// Drop the state for slot `index`, keeping the lane aligned with the
    /// shrunken source/frame lists (silx `removeUrl`). Out-of-range is a no-op.
    fn remove(&mut self, index: usize) {
        if index < self.state.len() {
            self.state.remove(index);
        }
    }

    /// `true` when slot `i` has not yet been dispatched, so it is eligible to
    /// load. `InFlight`/`Loaded`/`Failed` slots return `false` (no re-dispatch).
    fn needs_load(&self, i: usize) -> bool {
        matches!(self.state.get(i), Some(LoadState::NotRequested))
    }

    fn mark_in_flight(&mut self, i: usize) {
        if let Some(s) = self.state.get_mut(i) {
            *s = LoadState::InFlight;
        }
    }

    fn mark_loaded(&mut self, i: usize) {
        if let Some(s) = self.state.get_mut(i) {
            *s = LoadState::Loaded;
        }
    }

    fn mark_failed(&mut self, i: usize) {
        if let Some(s) = self.state.get_mut(i) {
            *s = LoadState::Failed;
        }
    }

    /// Filter `candidates` to the slots that still need loading, deduped in
    /// first-seen order — the set of loads to dispatch this frame.
    fn to_dispatch(&self, candidates: impl IntoIterator<Item = usize>) -> Vec<usize> {
        let mut out = Vec::new();
        for i in candidates {
            if self.needs_load(i) && !out.contains(&i) {
                out.push(i);
            }
        }
        out
    }
}

/// The slots to (consider) loading for a current position with prefetch radius
/// `n`: the current slot first, then the next `n` slots, then the previous `n`
/// slots, each clamped to `[0, len)` (silx `setCurrentUrl` loads the current url
/// then `_preFetch(_getNNextUrls(n))` and `_preFetch(_getNPreviousUrls(n))`).
///
/// Nearer neighbours come first on each side; `n == 0` yields just the current
/// slot, and an empty stack yields nothing. Pure, so the prefetch window is
/// unit-testable without threads.
fn prefetch_candidates(current: usize, len: usize, n: usize) -> Vec<usize> {
    if len == 0 {
        return Vec::new();
    }
    let mut out = vec![current];
    for k in 1..=n {
        if current + k < len {
            out.push(current + k);
        }
    }
    for k in 1..=n {
        if let Some(prev) = current.checked_sub(k) {
            out.push(prev);
        }
    }
    out
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

    /// Remove frame slot `index`, keeping `frames`/`visible` aligned and
    /// adjusting the current index so the displayed frame is preserved where
    /// possible (silx `removeUrl` re-indexes then keeps the current url):
    /// removing a slot *before* the current one shifts the current index down by
    /// one (same frame stays shown); removing the current slot leaves the index
    /// pointing at the next frame (clamped to the new last), which is a content
    /// change (`dirty`); removing a slot *after* the current one is index-neutral.
    /// Out-of-range is a no-op.
    fn remove(&mut self, index: usize) {
        if index >= self.frames.len() {
            return;
        }
        self.frames.remove(index);
        self.visible.remove(index);
        if self.frames.is_empty() {
            self.current = 0;
        } else if index < self.current {
            self.current -= 1; // same frame, lower index; no content change.
        } else if index == self.current {
            self.current = self.current.min(self.frames.len() - 1);
            self.dirty = true; // a different frame now occupies this index.
        }
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
/// pure `FrameNav` navigation state. The displayed image is rebuilt in place
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
    /// Per-slot source strings for the lazy path (silx `_urls`); empty in the
    /// in-memory [`Self::set_frames`] mode.
    sources: Vec<String>,
    /// Prefetch radius: how many neighbours on each side of the current slot to
    /// preload (silx `__n_prefetch` / `N_PRELOAD`, default 10). `0` disables
    /// prefetch (only the current slot loads).
    n_prefetch: usize,
    /// Loader used to turn a source into a [`Frame`] off-thread (silx
    /// `_url_loader`); `None` until [`Self::set_loader`] is called.
    loader: Option<Arc<dyn FrameLoader>>,
    /// Pure per-slot load bookkeeping (silx `_urlData` keys + in-flight set).
    schedule: LoadSchedule,
    /// Sends `(slot index, loaded frame or None)` from a load thread back to the
    /// UI thread (silx `UrlLoader.finished` → `_urlLoaded`, queued connection).
    load_tx: Sender<(usize, Option<Frame>)>,
    /// Receives load results, drained each frame in [`Self::ui`].
    load_rx: Receiver<(usize, Option<Frame>)>,
}

/// Clone `base` with its value range autoscaled to `data` (minmax, silx's
/// default autoscale mode), honoring the base colormap's normalization — the
/// LUT / normalization / gamma / NaN color are preserved, only `vmin`/`vmax`
/// change. silx frames carry `vmin`/`vmax = None`, so each frame's range tracks
/// its own data rather than a fixed span. Split out so the per-frame range
/// derivation is unit-testable without a GPU backend.
fn frame_colormap(base: &Colormap, data: &[f32]) -> Colormap {
    let data_f64: Vec<f64> = data.iter().map(|&v| v as f64).collect();
    let (vmin, vmax) = base.autoscale_range(AutoscaleMode::MinMax, &data_f64);
    let mut cm = base.clone();
    cm.vmin = vmin;
    cm.vmax = vmax;
    cm
}

impl ImageStack {
    /// Create an empty image stack backed by wgpu plot id `id`.
    pub fn new(render_state: &RenderState, id: PlotId) -> Self {
        let mut plot = Plot2D::new(render_state, id);
        plot.set_keep_data_aspect_ratio(true);
        plot.set_graph_cursor(true);
        let (load_tx, load_rx) = std::sync::mpsc::channel();
        Self {
            plot,
            nav: FrameNav::default(),
            // silx frames use the plot's default colormap — gray, autoscaled
            // per frame (`addImage` with no colormap → vmin/vmax = None). The
            // stored range here is a placeholder; `rebuild_image` re-derives it
            // from each frame's own data (see `set_colormap`).
            colormap: Colormap::new(ColormapName::Gray, 0.0, 1.0),
            image_handle: None,
            // silx ImageStack defaults _autoResetZoom = True.
            auto_reset_zoom: true,
            show_table: true,
            sources: Vec::new(),
            // silx ImageStack defaults N_PRELOAD = 10 (each side).
            n_prefetch: 10,
            loader: None,
            schedule: LoadSchedule::default(),
            load_tx,
            load_rx,
        }
    }

    /// Replace the whole frame list with in-memory frames, resetting the current
    /// index to 0 and marking every frame visible (silx `setUrls` -> `reset`
    /// then re-add). Leaves the lazy-loading mode: any source list and pending
    /// load state are cleared, so these resident frames are shown as-is.
    pub fn set_frames(&mut self, frames: Vec<Option<Frame>>) {
        self.nav.set_frames(frames);
        self.sources.clear();
        self.schedule.reset(0);
        self.image_handle = None;
        self.plot.clear_images();
    }

    /// Set the loader used to turn a source string into a [`Frame`] off the UI
    /// thread (silx `setUrlLoaderClass`, `ImageStack.py` :207-215). Must be set
    /// before (or together with) [`Self::set_sources`] for lazy loading to run;
    /// sources browsed to while no loader is set stay empty (waiting overlay).
    pub fn set_loader(&mut self, loader: Arc<dyn FrameLoader>) {
        self.loader = Some(loader);
    }

    /// Switch to lazy loading from `sources` (silx `setUrls`, `ImageStack.py`
    /// :337-361): each source is an opaque string the [`FrameLoader`] resolves
    /// to a frame on demand. Every slot starts empty (waiting overlay) and is
    /// loaded on a background thread when browsed to (and, with a prefetch
    /// radius set, when it falls within it). Resets the current index to 0 and
    /// drops any in-memory frames previously set.
    pub fn set_sources(&mut self, sources: Vec<String>) {
        let len = sources.len();
        // Every slot empty -> the existing None-slot waiting-overlay path covers
        // "not yet loaded"; a completed load fills the slot in place.
        self.nav.set_frames(vec![None; len]);
        self.sources = sources;
        self.schedule.reset(len);
        self.image_handle = None;
        self.plot.clear_images();
    }

    /// The source strings backing the lazy path, in slot order (empty in the
    /// in-memory [`Self::set_frames`] mode). Mirrors silx `getUrls`.
    pub fn sources(&self) -> &[String] {
        &self.sources
    }

    /// Remove slot `index` from a lazy-mode stack (silx `removeUrl` /
    /// `sigUrlRemoved`): drops its source, its frame/visibility, and its load
    /// state together so the four parallel lanes stay aligned, then keeps the
    /// displayed frame where possible (the nav adjusts its current index: a
    /// removal before the current shifts it down, removing the current points at
    /// the next frame clamped to the new last, removing after is index-neutral).
    /// Out-of-range,
    /// or a call in the in-memory [`Self::set_frames`] mode, is a no-op.
    ///
    /// This is the single owner of slot removal: every lane (`sources`,
    /// `schedule`, the nav's `frames`/`visible`) is mutated here in lockstep, so
    /// `sources.len() == frames.len()` holds by construction.
    pub fn remove_source(&mut self, index: usize) {
        if index >= self.sources.len() {
            return;
        }
        self.sources.remove(index);
        self.schedule.remove(index);
        self.nav.remove(index);
        // The displayed image's slot may have shifted/changed; drop the handle so
        // the next render rebuilds from the (possibly new) current slot.
        self.image_handle = None;
    }

    /// The label shown for slot `index` in the table. In lazy mode the row shows
    /// the source string (the URL, silx `UrlList`); otherwise it falls back to
    /// the frame's own label / `Frame N` (silx in-memory rows).
    fn row_label(&self, index: usize) -> String {
        match self.sources.get(index) {
            Some(source) => source.clone(),
            None => self.nav.frame_label(index),
        }
    }

    /// Set the prefetch radius: the number of neighbouring slots on *each* side
    /// of the current slot to preload in the background (silx `setNPrefetch`,
    /// `ImageStack.py` :294-304 — "in total 2*n DataUrls"). `0` disables
    /// prefetch. The new radius takes effect on the next render.
    pub fn set_n_prefetch(&mut self, n: usize) {
        self.n_prefetch = n;
    }

    /// The prefetch radius (slots preloaded on each side; silx `getNPrefetch`).
    pub fn n_prefetch(&self) -> usize {
        self.n_prefetch
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
    /// active colormap; this exposes it directly). Only the name /
    /// normalization / gamma are honored — the value range is re-derived from
    /// each frame's own data by [`Self::rebuild_image`], matching silx's
    /// default autoscale (`vmin`/`vmax = None`). Pinning an explicit range is
    /// the per-bound-autoscale surface silx exposes but siplot does not yet.
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

    /// The slot indices that should be loaded given the current position: the
    /// current slot first, then its prefetch neighbours (silx
    /// `setCurrentUrl` loads the current url, then `_preFetch`es the next and
    /// previous `n_prefetch`).
    fn load_candidates(&self) -> Vec<usize> {
        prefetch_candidates(self.nav.current, self.nav.len(), self.n_prefetch)
    }

    /// Move every completed background load into its slot and update its load
    /// state (silx `_urlLoaded`): a `Some` frame becomes resident and, if it is
    /// the current slot, marks the display dirty; a `None` marks the slot failed
    /// (terminal, not retried).
    fn drain_loaded(&mut self) {
        while let Ok((index, frame)) = self.load_rx.try_recv() {
            match frame {
                Some(frame) => {
                    if let Some(slot) = self.nav.frames.get_mut(index) {
                        *slot = Some(frame);
                        self.schedule.mark_loaded(index);
                        if index == self.nav.current {
                            self.nav.dirty = true;
                        }
                    }
                }
                None => self.schedule.mark_failed(index),
            }
        }
    }

    /// Spawn a background load for each candidate slot that still needs one
    /// (silx `_load` / `_preFetch`): clones the loader, source, result sender,
    /// and `ctx` into a thread that loads off the UI thread and requests a
    /// repaint when done. A no-op when no loader is set.
    fn dispatch_loads(&mut self, ctx: &egui::Context) {
        let Some(loader) = self.loader.clone() else {
            return;
        };
        for index in self.schedule.to_dispatch(self.load_candidates()) {
            let Some(source) = self.sources.get(index).cloned() else {
                continue;
            };
            self.schedule.mark_in_flight(index);
            let loader = loader.clone();
            let tx = self.load_tx.clone();
            let ctx = ctx.clone();
            // Detached: the worker only sends on `tx` and pokes `ctx`, both of
            // which stay valid (Arc-backed) even if the stack is dropped, in
            // which case the send fails harmlessly. Mirrors silx's per-url
            // QThread (`_loadingThreads`), freed implicitly on drop here.
            std::thread::spawn(move || {
                let frame = loader.load(&source);
                let _ = tx.send((index, frame));
                ctx.request_repaint();
            });
        }
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
        // silx autoscales each frame to its own data range (the frame's
        // colormap carries `vmin`/`vmax = None`), honoring the base colormap's
        // normalization. Re-derive the range here rather than reusing a fixed
        // span.
        let colormap = frame_colormap(&self.colormap, &data);

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

        // Lazy path: pull in any finished loads, then dispatch loads for the
        // (possibly just-changed) current slot and its prefetch neighbours.
        let ctx = ui.ctx().clone();
        self.drain_loaded();
        self.dispatch_loads(&ctx);

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
    /// visibility checkbox and, in lazy mode, a remove button (silx
    /// `_ToggleableUrlSelectionTable` + `UrlList`, extended with the visibility
    /// toggle the task requires). Lazy rows show the source/URL text (silx
    /// `UrlList`); in-memory rows show the frame label.
    fn frame_table_ui(&mut self, ui: &mut egui::Ui) {
        let len = self.nav.len();
        if len == 0 {
            ui.label("No frames");
            return;
        }
        let current = self.nav.current;
        let lazy = !self.sources.is_empty();
        let mut to_remove = None;
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
                        // silx UrlList row delete -> sigUrlRemoved (lazy mode only).
                        if lazy
                            && ui
                                .small_button("✕")
                                .on_hover_text("Remove this source")
                                .clicked()
                        {
                            to_remove = Some(index);
                        }
                        let label = self.row_label(index);
                        if ui.selectable_label(index == current, label).clicked() {
                            self.nav.set_current(index);
                        }
                    });
                }
            });
        if let Some(index) = to_remove {
            self.remove_source(index);
        }
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

    // ── Per-frame autoscale (R2-1) ──────────────────────────────────────────

    #[test]
    fn frame_colormap_autoscales_to_frame_data() {
        // silx frames carry no explicit range; the per-frame colormap must
        // track the frame's own min/max, not a fixed [0, 1] span, and must
        // keep the base colormap's LUT (gray by default).
        let base = Colormap::new(ColormapName::Gray, 0.0, 1.0);
        let cm = frame_colormap(&base, &[10.0, 20.0, 30.0, 40.0]);
        assert_eq!((cm.vmin, cm.vmax), (10.0, 40.0));
        assert_eq!(cm.lut, base.lut);
    }

    #[test]
    fn frame_colormap_tracks_data_not_stored_range() {
        // Even when the base carries a stale span, the per-frame range comes
        // from the frame data (silx re-autoscales each frame), so the stored
        // (0, 1) does not leak through. A uniform frame minmax-autoscales to
        // (v, v), matching silx `getColormapRange` for constant data.
        let base = Colormap::new(ColormapName::Gray, 0.0, 1.0);
        let cm = frame_colormap(&base, &[7.0, 7.0, 7.0, 7.0]);
        assert_eq!((cm.vmin, cm.vmax), (7.0, 7.0));
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

    // ── FrameNav::remove index adjustment (silx removeUrl) ───────────────────

    #[test]
    fn remove_before_current_shifts_index_down() {
        let mut n = nav(vec![frame("a"), frame("b"), frame("c")]);
        n.set_current(2); // showing "c".
        n.dirty = false;
        n.remove(0); // drop "a".
        assert_eq!(n.len(), 2);
        assert_eq!(n.current, 1); // still showing "c", now at index 1.
        assert!(!n.dirty); // same content -> no rebuild.
        assert_eq!(n.frame_label(1), "c");
    }

    #[test]
    fn remove_current_points_at_next_then_clamps() {
        let mut n = nav(vec![frame("a"), frame("b"), frame("c")]);
        n.set_current(1); // showing "b".
        n.dirty = false;
        n.remove(1); // drop the current "b".
        assert_eq!(n.current, 1); // now showing "c" (the next frame).
        assert_eq!(n.frame_label(1), "c");
        assert!(n.dirty); // content changed.
        // Removing the (new) last while it is current clamps to the new last.
        n.dirty = false;
        n.remove(1); // drop "c" (current, last) -> current clamps to 0 ("a").
        assert_eq!(n.current, 0);
        assert_eq!(n.frame_label(0), "a");
        assert!(n.dirty);
    }

    #[test]
    fn remove_after_current_is_index_neutral() {
        let mut n = nav(vec![frame("a"), frame("b"), frame("c")]);
        n.set_current(0);
        n.dirty = false;
        n.remove(2); // drop "c", after current.
        assert_eq!(n.current, 0);
        assert!(!n.dirty);
        assert_eq!(n.len(), 2);
    }

    #[test]
    fn remove_last_remaining_resets() {
        let mut n = nav(vec![frame("a")]);
        n.remove(0);
        assert!(n.is_empty());
        assert_eq!(n.current, 0);
    }

    #[test]
    fn remove_out_of_range_is_noop() {
        let mut n = nav(vec![frame("a"), frame("b")]);
        n.remove(9);
        assert_eq!(n.len(), 2);
    }

    #[test]
    fn schedule_remove_drops_the_slot() {
        let mut s = LoadSchedule::default();
        s.reset(3);
        s.mark_loaded(0);
        s.mark_failed(2);
        s.remove(0); // drop slot 0; slots 1,2 shift down to 0,1.
        assert_eq!(s.state.len(), 2);
        assert!(s.needs_load(0)); // was slot 1 (NotRequested).
        assert!(!s.needs_load(1)); // was slot 2 (Failed).
        s.remove(9); // out of range: no-op.
        assert_eq!(s.state.len(), 2);
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

    // ── LoadSchedule dispatch/dedup (lazy path) ─────────────────────────────

    #[test]
    fn schedule_reset_marks_all_not_requested() {
        let mut s = LoadSchedule::default();
        s.reset(3);
        assert!(s.needs_load(0));
        assert!(s.needs_load(1));
        assert!(s.needs_load(2));
        assert!(!s.needs_load(3)); // out of range.
    }

    #[test]
    fn schedule_in_flight_and_loaded_are_not_redispatched() {
        let mut s = LoadSchedule::default();
        s.reset(2);
        s.mark_in_flight(0);
        s.mark_loaded(1);
        assert!(!s.needs_load(0)); // in flight.
        assert!(!s.needs_load(1)); // loaded.
        assert_eq!(s.to_dispatch([0, 1]), Vec::<usize>::new());
    }

    #[test]
    fn schedule_failed_is_terminal_not_retried() {
        let mut s = LoadSchedule::default();
        s.reset(1);
        s.mark_in_flight(0);
        s.mark_failed(0);
        assert!(!s.needs_load(0));
        assert_eq!(s.to_dispatch([0]), Vec::<usize>::new());
    }

    #[test]
    fn schedule_to_dispatch_filters_and_dedups() {
        let mut s = LoadSchedule::default();
        s.reset(4);
        s.mark_loaded(1);
        // 1 is loaded (dropped); 5 is out of range (dropped); 2 repeats (deduped).
        assert_eq!(s.to_dispatch([0, 1, 2, 2, 5]), vec![0, 2]);
    }

    // ── prefetch_candidates window (silx _getNNextUrls/_getNPreviousUrls) ────

    #[test]
    fn prefetch_window_in_the_middle_is_current_then_next_then_prev() {
        // current first, then next n (nearest first), then prev n (nearest first).
        assert_eq!(prefetch_candidates(5, 10, 2), vec![5, 6, 7, 4, 3]);
    }

    #[test]
    fn prefetch_window_clamps_at_both_ends() {
        // At the start: no previous slots.
        assert_eq!(prefetch_candidates(0, 4, 2), vec![0, 1, 2]);
        // At the end: no next slots.
        assert_eq!(prefetch_candidates(3, 4, 2), vec![3, 2, 1]);
    }

    #[test]
    fn prefetch_window_zero_radius_is_just_current() {
        assert_eq!(prefetch_candidates(2, 5, 0), vec![2]);
    }

    #[test]
    fn prefetch_window_empty_stack_is_empty() {
        assert_eq!(prefetch_candidates(0, 0, 3), Vec::<usize>::new());
    }

    #[test]
    fn prefetch_window_radius_exceeds_length() {
        // n larger than the stack: every other slot, no out-of-range indices.
        assert_eq!(prefetch_candidates(1, 3, 10), vec![1, 2, 0]);
    }

    // ── Hdf5FrameLoader source parsing + read ───────────────────────────────

    fn temp_h5(tag: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "siplot_image_stack_h5_{}_{}.h5",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn seed_dataset<T: rust_hdf5::types::H5Type>(
        path: &std::path::Path,
        name: &str,
        shape: &[usize],
        data: &[T],
    ) {
        use rust_hdf5::H5File;
        let file = if path.exists() {
            H5File::open_rw(path).unwrap()
        } else {
            H5File::create(path).unwrap()
        };
        let ds = file.new_dataset::<T>().shape(shape).create(name).unwrap();
        ds.write_raw(data).unwrap();
        file.close().unwrap();
    }

    #[test]
    fn hdf5_loader_reads_a_2d_dataset() {
        let path = temp_h5("loader_2d");
        seed_dataset(&path, "/img", &[2, 3], &[0.0f32, 1.0, 2.0, 3.0, 4.0, 5.0]);
        let source = format!("{}::/img", path.display());
        let frame = Hdf5FrameLoader.load(&source).expect("2D frame loads");
        assert_eq!((frame.width, frame.height), (3, 2)); // (width, height).
        assert_eq!(frame.data, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        assert!(frame.is_displayable());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hdf5_loader_reads_a_3d_slice() {
        let path = temp_h5("loader_3d");
        seed_dataset(&path, "/stack", &[2, 1, 2], &[0.0f32, 1.0, 10.0, 11.0]);
        let frame = Hdf5FrameLoader
            .load(&format!("{}::/stack::1", path.display()))
            .expect("3D slice loads");
        assert_eq!((frame.width, frame.height), (2, 1));
        assert_eq!(frame.data, vec![10.0, 11.0]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hdf5_loader_returns_none_on_bad_source() {
        // No "::" separator -> malformed.
        assert!(Hdf5FrameLoader.load("no-separator").is_none());
        // Slice index does not parse.
        assert!(
            Hdf5FrameLoader
                .load("/tmp/x.h5::/stack::notanumber")
                .is_none()
        );
        // Well-formed but missing file -> read fails -> None.
        assert!(
            Hdf5FrameLoader
                .load("/nonexistent/siplot_missing.h5::/img")
                .is_none()
        );
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
