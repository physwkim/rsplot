//! Axis synchronization across multiple plots (silx `SyncAxes`).
//!
//! In egui's immediate mode the sync is a per-frame call: detect which plot
//! changed its limits since the previous frame, then propagate those limits
//! to the other linked plots.

use crate::core::plot::Plot;

/// Synchronize one or more axes (X and/or Y) across a set of [`Plot`]
/// instances every frame. Mirrors silx `SyncAxes` from
/// `silx.gui.plot.utils.axis`.
///
/// Call [`SyncAxes::sync`] once per frame, **before** calling
/// [`crate::PlotView::show`] for each plot. The first plot whose limits
/// differ from the last-seen state is taken as the *source*; all other
/// plots are updated to match.
///
/// # Example
///
/// ```rust,ignore
/// let mut sync = SyncAxes::new(); // sync both axes by default
///
/// // In the frame loop:
/// sync.sync(&mut [&mut plot_a, &mut plot_b]);
/// view.show(ui, &mut plot_a);
/// view.show(ui, &mut plot_b);
/// ```
#[derive(Debug, Clone)]
pub struct SyncAxes {
    /// Synchronize the X axis limits. Default `true`.
    pub sync_x: bool,
    /// Synchronize the left Y axis limits. Default `true`.
    pub sync_y: bool,
    prev_x: Option<(f64, f64)>,
    prev_y: Option<(f64, f64)>,
}

impl Default for SyncAxes {
    fn default() -> Self {
        Self {
            sync_x: true,
            sync_y: true,
            prev_x: None,
            prev_y: None,
        }
    }
}

impl SyncAxes {
    /// Create a new `SyncAxes` that synchronizes both axes.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable or disable X-axis limit synchronization.
    pub fn with_sync_x(mut self, on: bool) -> Self {
        self.sync_x = on;
        self
    }

    /// Enable or disable Y-axis limit synchronization.
    pub fn with_sync_y(mut self, on: bool) -> Self {
        self.sync_y = on;
        self
    }

    /// Reset the remembered limits so the next [`sync`](Self::sync) call
    /// re-initializes from the first plot. Call this when replacing all plots.
    pub fn reset(&mut self) {
        self.prev_x = None;
        self.prev_y = None;
    }

    /// Synchronize limits across all `plots` for this frame.
    ///
    /// - If this is the first call (or after [`reset`](Self::reset)), limits
    ///   are copied from the first plot to all others.
    /// - Otherwise the first plot whose current limits differ from the
    ///   previously seen state is used as the source.
    /// - A no-op for empty slices or when both sync flags are `false`.
    pub fn sync(&mut self, plots: &mut [&mut Plot]) {
        if plots.is_empty() {
            return;
        }

        // -- X axis --
        if self.sync_x {
            let source_x = match self.prev_x {
                None => {
                    // First call: take limits from the first plot.
                    let (x0, x1, _, _) = plots[0].limits;
                    Some((x0, x1))
                }
                Some(prev) => {
                    // Find the first plot whose X limits changed.
                    plots.iter().find_map(|p| {
                        let (x0, x1, _, _) = p.limits;
                        if (x0, x1) != prev {
                            Some((x0, x1))
                        } else {
                            None
                        }
                    })
                }
            };
            if let Some((x0, x1)) = source_x {
                for p in plots.iter_mut() {
                    let (_, _, y0, y1) = p.limits;
                    p.limits = (x0, x1, y0, y1);
                }
                self.prev_x = Some((x0, x1));
            }
        }

        // -- Y axis --
        if self.sync_y {
            let source_y = match self.prev_y {
                None => {
                    let (_, _, y0, y1) = plots[0].limits;
                    Some((y0, y1))
                }
                Some(prev) => plots.iter().find_map(|p| {
                    let (_, _, y0, y1) = p.limits;
                    if (y0, y1) != prev {
                        Some((y0, y1))
                    } else {
                        None
                    }
                }),
            };
            if let Some((y0, y1)) = source_y {
                for p in plots.iter_mut() {
                    let (x0, x1, _, _) = p.limits;
                    p.limits = (x0, x1, y0, y1);
                }
                self.prev_y = Some((y0, y1));
            }
        }
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
}
