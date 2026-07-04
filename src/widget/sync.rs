//! Axis synchronization across multiple plots (silx `SyncAxes`).
//!
//! In egui's immediate mode the sync is a per-frame call: detect which plot
//! changed an axis aspect since the previous frame, then propagate that
//! aspect to the other linked plots.

use crate::core::plot::Plot;
use crate::core::transform::Scale;

/// Synchronize one or more axes (X and/or Y) across a set of [`Plot`]
/// instances every frame. Mirrors silx `SyncAxes` from
/// `silx.gui.plot.utils.axis`: by default limits, scale, AND direction are
/// all synchronized (`SyncAxes(..., syncLimits=True, syncScale=True,
/// syncDirection=True)`, "By default everything is synchronized",
/// axis.py:57-66; the scale/direction callbacks are `sigScaleChanged →
/// __axisScaleChanged` and `sigInvertedChanged → __axisInvertedChanged`,
/// :158-171, and `synchronize()` pushes scale and inverted state too,
/// :238-241).
///
/// [`Self::sync_x`]/[`Self::sync_y`] select which axes are linked (silx
/// builds one `SyncAxes` over a list of axes of one kind); the aspect flags
/// [`Self::sync_limits`]/[`Self::sync_scale`]/[`Self::sync_direction`]
/// mirror the silx constructor arguments and choose what is propagated
/// along the linked axes.
///
/// Call [`SyncAxes::sync`] once per frame, **before** calling
/// [`crate::PlotView::show`] for each plot. Per aspect, the first plot whose
/// state differs from the last-seen value is taken as the *source*; all
/// other plots are updated to match.
///
/// # Example
///
/// ```rust,ignore
/// let mut sync = SyncAxes::new(); // limits + scale + direction, both axes
///
/// // In the frame loop:
/// sync.sync(&mut [&mut plot_a, &mut plot_b]);
/// view.show(ui, &mut plot_a);
/// view.show(ui, &mut plot_b);
/// ```
#[derive(Debug, Clone)]
pub struct SyncAxes {
    /// Link the X axes. Default `true`.
    pub sync_x: bool,
    /// Link the left Y axes. Default `true`.
    pub sync_y: bool,
    /// Synchronize limits along the linked axes (silx `syncLimits`).
    /// Default `true`.
    pub sync_limits: bool,
    /// Synchronize the axis scale along the linked axes (silx `syncScale`).
    /// Default `true`.
    pub sync_scale: bool,
    /// Synchronize the axis direction along the linked axes (silx
    /// `syncDirection`). Default `true`.
    pub sync_direction: bool,
    prev_x: Option<(f64, f64)>,
    prev_y: Option<(f64, f64)>,
    prev_x_scale: Option<Scale>,
    prev_y_scale: Option<Scale>,
    prev_x_inverted: Option<bool>,
    prev_y_inverted: Option<bool>,
}

impl Default for SyncAxes {
    fn default() -> Self {
        Self {
            sync_x: true,
            sync_y: true,
            sync_limits: true,
            sync_scale: true,
            sync_direction: true,
            prev_x: None,
            prev_y: None,
            prev_x_scale: None,
            prev_y_scale: None,
            prev_x_inverted: None,
            prev_y_inverted: None,
        }
    }
}

impl SyncAxes {
    /// Create a new `SyncAxes` that synchronizes everything on both axes
    /// (the silx default).
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable or disable X-axis linking.
    pub fn with_sync_x(mut self, on: bool) -> Self {
        self.sync_x = on;
        self
    }

    /// Enable or disable Y-axis linking.
    pub fn with_sync_y(mut self, on: bool) -> Self {
        self.sync_y = on;
        self
    }

    /// Enable or disable limit synchronization (silx `syncLimits`).
    pub fn with_sync_limits(mut self, on: bool) -> Self {
        self.sync_limits = on;
        self
    }

    /// Enable or disable scale synchronization (silx `syncScale`).
    pub fn with_sync_scale(mut self, on: bool) -> Self {
        self.sync_scale = on;
        self
    }

    /// Enable or disable direction synchronization (silx `syncDirection`).
    pub fn with_sync_direction(mut self, on: bool) -> Self {
        self.sync_direction = on;
        self
    }

    /// Reset the remembered state so the next [`sync`](Self::sync) call
    /// re-initializes from the first plot. Call this when replacing all plots.
    pub fn reset(&mut self) {
        self.prev_x = None;
        self.prev_y = None;
        self.prev_x_scale = None;
        self.prev_y_scale = None;
        self.prev_x_inverted = None;
        self.prev_y_inverted = None;
    }

    /// Synchronize the enabled aspects across all `plots` for this frame.
    ///
    /// - If this is the first call (or after [`reset`](Self::reset)), each
    ///   aspect is copied from the first plot to all others (silx
    ///   `synchronize()` with the first axis as `mainAxis`, axis.py:238-241).
    /// - Otherwise, per aspect, the first plot whose current state differs
    ///   from the previously seen value is used as the source.
    /// - A no-op for empty slices or when nothing is enabled.
    pub fn sync(&mut self, plots: &mut [&mut Plot]) {
        if plots.is_empty() {
            return;
        }

        if self.sync_x {
            if self.sync_limits {
                sync_aspect(
                    plots,
                    &mut self.prev_x,
                    |p| (p.limits.0, p.limits.1),
                    |p, (x0, x1)| p.limits = (x0, x1, p.limits.2, p.limits.3),
                );
            }
            if self.sync_scale {
                sync_aspect(
                    plots,
                    &mut self.prev_x_scale,
                    |p| p.x_scale,
                    |p, s| {
                        p.x_scale = s;
                    },
                );
            }
            if self.sync_direction {
                sync_aspect(
                    plots,
                    &mut self.prev_x_inverted,
                    |p| p.x_inverted,
                    |p, v| p.x_inverted = v,
                );
            }
        }

        if self.sync_y {
            if self.sync_limits {
                sync_aspect(
                    plots,
                    &mut self.prev_y,
                    |p| (p.limits.2, p.limits.3),
                    |p, (y0, y1)| p.limits = (p.limits.0, p.limits.1, y0, y1),
                );
            }
            if self.sync_scale {
                sync_aspect(
                    plots,
                    &mut self.prev_y_scale,
                    |p| p.y_scale,
                    |p, s| {
                        p.y_scale = s;
                    },
                );
            }
            if self.sync_direction {
                sync_aspect(
                    plots,
                    &mut self.prev_y_inverted,
                    |p| p.y_inverted,
                    |p, v| p.y_inverted = v,
                );
            }
        }
    }
}

/// One aspect's per-frame sync: pick the source (the first plot whose state
/// differs from `prev`, or the first plot when `prev` is unset), write it to
/// every plot, and remember it.
fn sync_aspect<T: Copy + PartialEq>(
    plots: &mut [&mut Plot],
    prev: &mut Option<T>,
    get: impl Fn(&Plot) -> T,
    mut set: impl FnMut(&mut Plot, T),
) {
    let source = match *prev {
        None => Some(get(plots[0])),
        Some(p) => plots.iter().map(|plot| get(plot)).find(|&v| v != p),
    };
    if let Some(value) = source {
        for plot in plots.iter_mut() {
            set(plot, value);
        }
        *prev = Some(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plot_with_limits(x0: f64, x1: f64, y0: f64, y1: f64) -> Plot {
        let mut p = Plot::new(0);
        p.limits = (x0, x1, y0, y1);
        p
    }

    #[test]
    fn first_call_copies_first_plot_to_all() {
        let mut sync = SyncAxes::new();
        let mut a = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        let mut b = plot_with_limits(0.0, 1.0, 0.0, 10.0); // different limits
        sync.sync(&mut [&mut a, &mut b]);
        // b should now match a's limits
        assert_eq!(b.limits, (0.0, 5.0, -1.0, 1.0));
        assert_eq!(a.limits, (0.0, 5.0, -1.0, 1.0));
    }

    #[test]
    fn changed_plot_propagates_to_others() {
        let mut sync = SyncAxes::new();
        let mut a = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        let mut b = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        // Frame 1: initialize
        sync.sync(&mut [&mut a, &mut b]);

        // Frame 2: a pans to [1, 6]
        a.limits = (1.0, 6.0, -1.0, 1.0);
        sync.sync(&mut [&mut a, &mut b]);
        assert_eq!(b.limits.0, 1.0);
        assert_eq!(b.limits.1, 6.0);
    }

    #[test]
    fn sync_x_only_leaves_y_independent() {
        let mut sync = SyncAxes::new().with_sync_y(false);
        let mut a = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        let mut b = plot_with_limits(0.0, 1.0, -2.0, 2.0);
        sync.sync(&mut [&mut a, &mut b]);
        // X synced; Y not changed
        assert_eq!(b.limits.0, 0.0); // b x → 0.0 (from a)
        assert_eq!(b.limits.1, 5.0);
        assert_eq!(b.limits.2, -2.0); // b y unchanged
        assert_eq!(b.limits.3, 2.0);
    }

    #[test]
    fn sync_y_only_leaves_x_independent() {
        let mut sync = SyncAxes::new().with_sync_x(false);
        let mut a = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        let mut b = plot_with_limits(10.0, 20.0, -1.0, 1.0);
        sync.sync(&mut [&mut a, &mut b]);
        // Y synced; X not changed
        assert_eq!(b.limits.0, 10.0); // b x unchanged
        assert_eq!(b.limits.2, -1.0); // b y → from a (already same)
    }

    #[test]
    fn reset_reinitializes_from_first_plot() {
        let mut sync = SyncAxes::new();
        let mut a = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        let mut b = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        sync.sync(&mut [&mut a, &mut b]);

        // Reset and replace with new limits
        sync.reset();
        a.limits = (100.0, 200.0, 50.0, 60.0);
        b.limits = (0.0, 1.0, 0.0, 1.0);
        sync.sync(&mut [&mut a, &mut b]);
        assert_eq!(b.limits, (100.0, 200.0, 50.0, 60.0));
    }

    #[test]
    fn empty_slice_is_noop() {
        let mut sync = SyncAxes::new();
        sync.sync(&mut []); // must not panic
    }

    #[test]
    fn scale_syncs_by_default_like_silx() {
        // silx SyncAxes(..., syncScale=True) by default (axis.py:57-66):
        // toggling one plot's axis scale propagates to the others.
        let mut sync = SyncAxes::new();
        let mut a = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        let mut b = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        sync.sync(&mut [&mut a, &mut b]); // initialize

        a.x_scale = Scale::Log10;
        a.y_scale = Scale::Log10;
        sync.sync(&mut [&mut a, &mut b]);
        assert_eq!(b.x_scale, Scale::Log10);
        assert_eq!(b.y_scale, Scale::Log10);
    }

    #[test]
    fn direction_syncs_by_default_like_silx() {
        // silx syncDirection=True by default: sigInvertedChanged propagates
        // (axis.py:165-171).
        let mut sync = SyncAxes::new();
        let mut a = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        let mut b = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        sync.sync(&mut [&mut a, &mut b]); // initialize

        a.y_inverted = true;
        sync.sync(&mut [&mut a, &mut b]);
        assert!(b.y_inverted);
        assert!(!b.x_inverted, "the untouched direction stays put");
    }

    #[test]
    fn first_call_pushes_scale_and_direction_from_first_plot() {
        // silx synchronize() pushes scale and inverted state from the main
        // axis too (axis.py:238-241).
        let mut sync = SyncAxes::new();
        let mut a = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        a.x_scale = Scale::Log10;
        a.x_inverted = true;
        let mut b = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        sync.sync(&mut [&mut a, &mut b]);
        assert_eq!(b.x_scale, Scale::Log10);
        assert!(b.x_inverted);
    }

    #[test]
    fn aspect_flags_gate_scale_and_direction_independently() {
        // syncScale=False leaves scales independent while limits still sync;
        // syncDirection=False likewise (the silx constructor flags).
        let mut sync = SyncAxes::new()
            .with_sync_scale(false)
            .with_sync_direction(false);
        let mut a = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        let mut b = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        sync.sync(&mut [&mut a, &mut b]); // initialize

        a.x_scale = Scale::Log10;
        a.x_inverted = true;
        a.limits = (1.0, 6.0, -1.0, 1.0);
        sync.sync(&mut [&mut a, &mut b]);
        assert_eq!(b.x_scale, Scale::Linear, "scale must stay independent");
        assert!(!b.x_inverted, "direction must stay independent");
        assert_eq!(b.limits.0, 1.0, "limits still sync");
    }

    #[test]
    fn unlinked_axis_syncs_no_aspect() {
        // sync_x=false unlinks the X axes entirely: neither X limits nor X
        // scale/direction propagate, while Y aspects still do.
        let mut sync = SyncAxes::new().with_sync_x(false);
        let mut a = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        let mut b = plot_with_limits(0.0, 5.0, -1.0, 1.0);
        sync.sync(&mut [&mut a, &mut b]); // initialize

        a.x_scale = Scale::Log10;
        a.x_inverted = true;
        a.y_scale = Scale::Log10;
        sync.sync(&mut [&mut a, &mut b]);
        assert_eq!(b.x_scale, Scale::Linear);
        assert!(!b.x_inverted);
        assert_eq!(b.y_scale, Scale::Log10);
    }
}
