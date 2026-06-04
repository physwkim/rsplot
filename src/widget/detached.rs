//! Shared helper for showing widget panels as detachable native OS windows.
//!
//! egui's [`egui::Window`] is an *area* clamped to the host window's screen
//! rect, so it can never be dragged outside the application window. To let a
//! tool panel live anywhere on the desktop — like silx's separate
//! `QWidget`/dock tool windows — it must be its own egui *viewport* (a real OS
//! window on native multi-viewport backends). This module centralises that
//! machinery so every detachable tool window (ROI manager, colormap dialog,
//! limits, fit, rename, analysis-result dialogs) behaves identically, reusing
//! the placement maths the original [`super::profile_window::ProfileWindow`]
//! pioneered.
//!
//! On backends without multi-viewport support (Wayland, Android, web) egui
//! transparently falls back to an embedded in-app window (see
//! [`egui::Context::embed_viewports`]); the placement maths is then skipped
//! because the window position is not exposed.

use egui::{Pos2, Vec2};

/// Signals read back from a viewport after its frame, so the caller can react
/// to the user closing or moving the window.
#[derive(Default)]
pub struct ViewportSignals {
    /// The OS window's close button (or a platform close request) was hit.
    pub close_requested: bool,
    /// Current outer top-left of the window, in monitor points. `None` on
    /// backends that do not report it (e.g. the embedded fallback).
    pub live_pos: Option<Pos2>,
}

impl ViewportSignals {
    fn read(ctx: &egui::Context) -> Self {
        ctx.input(|i| {
            let vp = i.viewport();
            Self {
                close_requested: vp.close_requested(),
                live_pos: vp.outer_rect.map(|r| r.min),
            }
        })
    }
}

/// Show `add_contents` in its own native OS window (a separate egui viewport)
/// titled `title`, sized `size`, optionally placed at `position`. Returns the
/// [`ViewportSignals`] observed this frame.
///
/// The viewport callback hands `add_contents` a ready `&mut Ui` (egui has
/// already opened the window's root area), so callers render straight into it —
/// no [`egui::CentralPanel`] wrapper is needed. Use this directly for transient
/// dialogs that keep no persistent placement state; persistent tool windows
/// should drive it through [`DetachedWindow`] to also remember their position.
pub fn show_detached(
    ctx: &egui::Context,
    id: egui::Id,
    title: &str,
    size: Vec2,
    position: Option<Pos2>,
    mut add_contents: impl FnMut(&mut egui::Ui),
) -> ViewportSignals {
    let viewport_id = egui::ViewportId::from_hash_of(id);
    let mut builder = egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size(size);
    if let Some(pos) = position {
        builder = builder.with_position(pos);
    }

    let mut signals = ViewportSignals::default();
    ctx.show_viewport_immediate(viewport_id, builder, |ui, _class| {
        add_contents(ui);
        signals = ViewportSignals::read(ui.ctx());
    });
    signals
}

/// A detachable native OS window with remembered placement, for persistent
/// tool panels (ROI manager, colormap, limits, fit). Owns only the windowing
/// state; the panel widget keeps its own `open` flag and renders its own
/// content via [`show_detached`].
pub struct DetachedWindow {
    id: egui::Id,
    size: Vec2,
    /// Position chosen for the *current* open session, computed once when the
    /// window opens and then left untouched so the user can freely drag it
    /// (re-passing an unchanged position never re-issues `OuterPosition`).
    placement: Option<Pos2>,
    /// Last observed outer position, restored as the initial placement on the
    /// next open — mirrors silx `*Manager._previousWindowGeometry`.
    remembered: Option<Pos2>,
}

impl DetachedWindow {
    /// Create a detachable window with a stable viewport id and initial size.
    pub fn new(id: egui::Id, size: Vec2) -> Self {
        Self {
            id,
            size,
            placement: None,
            remembered: None,
        }
    }

    /// The stable viewport-seed id.
    pub fn id(&self) -> egui::Id {
        self.id
    }

    /// The initial outer size, in points.
    pub fn size(&self) -> Vec2 {
        self.size
    }

    /// Position for this open session, computing it once: restore the last
    /// place the user left it, else sit beside the main window. Returns `None`
    /// when the host cannot report geometry (no explicit position is then set,
    /// and the OS/egui chooses the placement).
    pub fn position(&mut self, ctx: &egui::Context) -> Option<Pos2> {
        if self.placement.is_none() {
            self.placement = self
                .remembered
                .or_else(|| beside_main_window(ctx, self.size));
        }
        self.placement
    }

    /// Fold the per-frame viewport signals back into this window's state and
    /// the owning panel's `open` flag: remember the moved position; on close,
    /// clear `open` and forget the placement so the next open re-runs the
    /// beside-the-main-window logic against the latest main-window position.
    pub fn apply_signals(&mut self, signals: &ViewportSignals, open: &mut bool) {
        if let Some(pos) = signals.live_pos {
            self.remembered = Some(pos);
        }
        if signals.close_requested {
            *open = false;
            self.placement = None;
        }
    }
}

/// Compute where to first place a detached window so it sits beside the main
/// window instead of covering it, mirroring silx
/// `ProfileManager.initProfileWindow` (tools/profile/manager.py:1030-1055):
/// prefer the right of the main window, else the left, else whichever side of
/// the screen has more room; vertically centre on the main window.
///
/// Returns `None` when the host cannot report the window/monitor geometry
/// (e.g. Wayland/Android), in which case no explicit position is set.
pub(crate) fn beside_main_window(ctx: &egui::Context, size: Vec2) -> Option<Pos2> {
    let (win, monitor) = ctx.input(|i| {
        let vp = i.viewport();
        (vp.outer_rect, vp.monitor_size)
    });
    // Both are in egui points, in monitor space (monitor origin at 0,0).
    Some(placement_beside(win?, monitor?, size, 5.0))
}

/// Pure placement maths behind [`beside_main_window`], split out so the
/// silx-parity boundaries can be unit-tested without an egui context.
///
/// `win` is the main window's outer rect and `monitor_size` the monitor size,
/// both in egui points / monitor space (origin at 0,0). Returns the top-left
/// the detached window should take so it sits beside `win` without covering it.
pub(crate) fn placement_beside(
    win: egui::Rect,
    monitor_size: Vec2,
    size: Vec2,
    margin: f32,
) -> Pos2 {
    let profile_w = size.x;

    let space_left = win.left(); // screen-left .. window-left
    let space_right = monitor_size.x - win.right(); // window-right .. screen-right

    // Vertically centre the detached window on the main window.
    let top = win.top() + (win.height() - size.y) / 2.0;

    let left = if profile_w < space_right {
        // Place it to the right of the main window.
        win.right() + margin
    } else if profile_w < space_left {
        // Place it to the left of the main window.
        (win.left() - profile_w - margin).max(0.0)
    } else if space_left > space_right {
        // Not enough room either side: push it to whichever has more.
        0.0
    } else {
        monitor_size.x - profile_w
    };

    egui::pos2(left, top)
}

#[cfg(test)]
mod tests {
    use super::placement_beside;
    use egui::{Rect, pos2, vec2};

    const MARGIN: f32 = 5.0;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-4, "expected {b}, got {a}");
    }

    #[test]
    fn places_to_the_right_when_it_fits() {
        // Main window 100..600 on a 1200-wide monitor: 600 pt free on the right.
        let win = Rect::from_min_max(pos2(100.0, 0.0), pos2(600.0, 400.0));
        let p = placement_beside(win, vec2(1200.0, 800.0), vec2(420.0, 320.0), MARGIN);
        approx(p.x, 605.0); // win.right() + margin
        approx(p.y, 40.0); // (400 - 320) / 2
    }

    #[test]
    fn places_to_the_left_when_right_is_too_tight() {
        // Right edge near the monitor edge (20 pt free); 600 pt free on the left.
        let win = Rect::from_min_max(pos2(600.0, 0.0), pos2(1180.0, 400.0));
        let p = placement_beside(win, vec2(1200.0, 800.0), vec2(420.0, 320.0), MARGIN);
        approx(p.x, 175.0); // win.left() - profile_w - margin
    }

    #[test]
    fn left_placement_clamps_to_screen_edge() {
        // Just enough left space to choose the left branch but it would go < 0.
        let win = Rect::from_min_max(pos2(422.0, 0.0), pos2(900.0, 400.0));
        let p = placement_beside(win, vec2(1000.0, 500.0), vec2(420.0, 320.0), MARGIN);
        approx(p.x, 0.0); // max(0, 422 - 420 - 5)
    }

    #[test]
    fn overflow_both_sides_picks_the_roomier_edge() {
        // Detached window wider than either gap. More room on the left -> hug left edge.
        let win = Rect::from_min_max(pos2(300.0, 0.0), pos2(900.0, 400.0));
        let p = placement_beside(win, vec2(1000.0, 500.0), vec2(420.0, 320.0), MARGIN);
        approx(p.x, 0.0);

        // More room on the right -> hug the right edge (monitor.x - profile_w).
        let win = Rect::from_min_max(pos2(80.0, 0.0), pos2(400.0, 400.0));
        let p = placement_beside(win, vec2(1000.0, 500.0), vec2(700.0, 320.0), MARGIN);
        approx(p.x, 300.0); // 1000 - 700
    }
}
