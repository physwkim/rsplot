//! `TimeSeriesBuffer` — a fixed-capacity FIFO of `(x, y)` samples.
//!
//! Ports the data buffer of `pydm/widgets/timeplot.py`'s `TimePlotCurveItem`:
//! a `(2, buffer_size)` array rolled left on every new value with the newest
//! sample written at the tail and `points_accumulated` capped at `buffer_size`
//! (`receiveNewValue` / `initialize_buffer`). PyDM's `np.roll` + the
//! `points_accumulated` cap is observably a capacity-bounded queue that keeps the
//! newest `buffer_size` samples in chronological order, so this is a
//! [`VecDeque`] with overwrite-oldest semantics.
//!
//! It is pure and headlessly testable; the time/scatter plot widgets feed it
//! samples and read them back ordered for rendering. The same `(x, y)` shape
//! serves a time series (`x = timestamp`) and a scatter trace (`x`, `y`).

use std::collections::VecDeque;

/// PyDM `MINIMUM_BUFFER_SIZE`: a buffer always holds at least two samples.
pub const MINIMUM_BUFFER_SIZE: usize = 2;
/// PyDM `timeplot.DEFAULT_BUFFER_SIZE`: the default capacity for a *time-plot*
/// curve.
pub const DEFAULT_BUFFER_SIZE: usize = 18000;
/// PyDM `scatterplot.DEFAULT_BUFFER_SIZE` / `eventplot.DEFAULT_BUFFER_SIZE`: the
/// default capacity for a scatter or event curve — 1200, not the 18000 the time
/// plot uses (`scatterplot.py:12`, `eventplot.py:11`).
pub const DEFAULT_SCATTER_EVENT_BUFFER_SIZE: usize = 1200;

/// A capacity-bounded FIFO of `(x, y)` samples that overwrites the oldest sample
/// when full (PyDM `TimePlotCurveItem` data buffer).
#[derive(Clone, Debug)]
pub struct TimeSeriesBuffer {
    samples: VecDeque<(f64, f64)>,
    capacity: usize,
}

impl TimeSeriesBuffer {
    /// Create a buffer holding the newest `capacity` samples (clamped up to
    /// [`MINIMUM_BUFFER_SIZE`], matching PyDM `setBufferSize`).
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(MINIMUM_BUFFER_SIZE);
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// The maximum number of samples retained.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The number of samples currently held (PyDM `points_accumulated`).
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Whether the buffer holds no samples.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Append a sample, evicting the oldest when at capacity (PyDM `np.roll` +
    /// tail-write).
    pub fn push(&mut self, x: f64, y: f64) {
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back((x, y));
    }

    /// Drop all samples (PyDM `initialize_buffer`).
    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// Resize the buffer, keeping the newest samples when shrinking (clamped up
    /// to [`MINIMUM_BUFFER_SIZE`]). PyDM's `setBufferSize` re-initializes (clears)
    /// the buffer; retaining the newest samples here is a deliberate deviation so
    /// a live plot does not blank on a buffer-size change.
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity.max(MINIMUM_BUFFER_SIZE);
        while self.samples.len() > self.capacity {
            self.samples.pop_front();
        }
    }

    /// The oldest retained sample, or `None` when empty (PyDM
    /// `data_buffer[:, -points_accumulated]`).
    pub fn oldest(&self) -> Option<(f64, f64)> {
        self.samples.front().copied()
    }

    /// The newest retained sample, or `None` when empty (PyDM
    /// `data_buffer[:, -1]`).
    pub fn newest(&self) -> Option<(f64, f64)> {
        self.samples.back().copied()
    }

    /// Iterate the samples oldest → newest.
    pub fn iter(&self) -> impl Iterator<Item = (f64, f64)> + '_ {
        self.samples.iter().copied()
    }

    /// Copy the samples (oldest → newest) into the two output vectors, clearing
    /// them first. This is the render feed: `x` into `xs`, `y` into `ys`.
    pub fn ordered_into(&self, xs: &mut Vec<f64>, ys: &mut Vec<f64>) {
        xs.clear();
        ys.clear();
        xs.reserve(self.samples.len());
        ys.reserve(self.samples.len());
        for &(x, y) in &self.samples {
            xs.push(x);
            ys.push(y);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ordered(buf: &TimeSeriesBuffer) -> (Vec<f64>, Vec<f64>) {
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        buf.ordered_into(&mut xs, &mut ys);
        (xs, ys)
    }

    #[test]
    fn scatter_event_default_buffer_matches_pydm_not_timeplot() {
        // PyDM's scatter/event curves default to 1200 samples
        // (`scatterplot.py:12`, `eventplot.py:11`), a 15×-smaller window than the
        // time plot's 18000 (`timeplot.py`). The two must stay distinct.
        assert_eq!(DEFAULT_SCATTER_EVENT_BUFFER_SIZE, 1200);
        assert_eq!(DEFAULT_BUFFER_SIZE, 18000);
        assert_ne!(DEFAULT_SCATTER_EVENT_BUFFER_SIZE, DEFAULT_BUFFER_SIZE);
    }

    #[test]
    fn new_clamps_capacity_to_minimum() {
        assert_eq!(TimeSeriesBuffer::new(0).capacity(), MINIMUM_BUFFER_SIZE);
        assert_eq!(TimeSeriesBuffer::new(1).capacity(), MINIMUM_BUFFER_SIZE);
        assert_eq!(TimeSeriesBuffer::new(50).capacity(), 50);
    }

    #[test]
    fn push_yields_samples_oldest_to_newest() {
        let mut buf = TimeSeriesBuffer::new(8);
        buf.push(1.0, 10.0);
        buf.push(2.0, 20.0);
        buf.push(3.0, 30.0);
        assert_eq!(buf.len(), 3);
        let (xs, ys) = ordered(&buf);
        assert_eq!(xs, vec![1.0, 2.0, 3.0]);
        assert_eq!(ys, vec![10.0, 20.0, 30.0]);
        assert_eq!(buf.oldest(), Some((1.0, 10.0)));
        assert_eq!(buf.newest(), Some((3.0, 30.0)));
    }

    #[test]
    fn push_past_capacity_evicts_oldest() {
        let mut buf = TimeSeriesBuffer::new(3);
        for i in 0..5 {
            buf.push(f64::from(i), f64::from(i) * 10.0);
        }
        // Only the newest 3 survive, still chronological.
        assert_eq!(buf.len(), 3);
        let (xs, ys) = ordered(&buf);
        assert_eq!(xs, vec![2.0, 3.0, 4.0]);
        assert_eq!(ys, vec![20.0, 30.0, 40.0]);
        assert_eq!(buf.oldest(), Some((2.0, 20.0)));
        assert_eq!(buf.newest(), Some((4.0, 40.0)));
    }

    #[test]
    fn ordered_into_clears_prior_output() {
        let mut buf = TimeSeriesBuffer::new(4);
        buf.push(1.0, 1.0);
        let mut xs = vec![99.0, 98.0];
        let mut ys = vec![97.0];
        buf.ordered_into(&mut xs, &mut ys);
        assert_eq!(xs, vec![1.0]);
        assert_eq!(ys, vec![1.0]);
    }

    #[test]
    fn set_capacity_shrink_keeps_newest_grow_keeps_all() {
        let mut buf = TimeSeriesBuffer::new(5);
        for i in 0..5 {
            buf.push(f64::from(i), f64::from(i));
        }
        buf.set_capacity(2);
        assert_eq!(buf.capacity(), 2);
        let (xs, _) = ordered(&buf);
        assert_eq!(xs, vec![3.0, 4.0]);
        // Growing keeps what is there and admits more.
        buf.set_capacity(4);
        buf.push(5.0, 5.0);
        buf.push(6.0, 6.0);
        let (xs, _) = ordered(&buf);
        assert_eq!(xs, vec![3.0, 4.0, 5.0, 6.0]);
        // The minimum floor still applies.
        buf.set_capacity(0);
        assert_eq!(buf.capacity(), MINIMUM_BUFFER_SIZE);
    }

    #[test]
    fn empty_buffer_has_no_endpoints() {
        let buf = TimeSeriesBuffer::new(4);
        assert!(buf.is_empty());
        assert_eq!(buf.oldest(), None);
        assert_eq!(buf.newest(), None);
    }
}
