//! `RsdmImageView` — a 1-D array channel rendered as a 2-D image.
//!
//! Ports `pydm/widgets/image.py` (`PyDMImageView`) onto a `rsplot`
//! [`ImageView`]. The image channel delivers a flat array; an optional width
//! channel (or a fixed [`RsdmImageView::with_width`]) gives the row length, and
//! the array is reshaped to `height × width` (`ImageUpdateThread.run`). The
//! colormap range is the manual `colorMapMin`/`colorMapMax` unless
//! `normalizeData` is set, in which case it is the data's min/max
//! (`process_image`).
//!
//! The reshape ([`reshape_image`]), array extraction ([`value_to_image`]), and
//! colour range ([`color_range`]) are pure and unit-tested; the GPU rendering is
//! exercised by a headless wgpu readback test.

use rsplot::egui_wgpu::RenderState;
use rsplot::{Colormap, ColormapName, ImageView, PlotId, egui};

use crate::channel::{Channel, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::middle_click_copy;

/// Reading order of the flat image array (PyDM `readingOrder`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ReadingOrder {
    /// Row-major (C order): element `(r, c)` is `data[r * width + c]` (PyDM
    /// `ReadingOrder.Clike`).
    CLike,
    /// Column-major (Fortran order): PyDM reshapes to `(width, -1)` with
    /// `order="F"` (image.py:108-109), so `width` becomes the ROW axis — the
    /// image is `width` rows × `len/width` columns and element `(r, c)` is
    /// `data[c * width + r]`, displayed row-major (image.py:210). This is
    /// PyDMImageView's constructor default (image.py:196).
    #[default]
    Fortran,
}

/// Extract a flat image array (`f32`) from a channel value: float/int arrays
/// convert element-wise. Non-array values yield `None`.
pub fn value_to_image(value: &PvValue) -> Option<Vec<f32>> {
    match value {
        PvValue::FloatArray(a) => Some(a.iter().map(|&v| v as f32).collect()),
        PvValue::IntArray(a) => Some(a.iter().map(|&v| v as f32).collect()),
        _ => None,
    }
}

/// Reshape a flat array into row-major pixels for [`ImageView::set_image`].
/// Returns `(width, height, pixels)` of the DISPLAYED image, or `None` when
/// the width is zero or there is not even one full row/column (PyDM aborts
/// the redraw when `width < 1`).
///
/// PyDM's two orders shape differently (image.py:106-109): `Clike` is
/// `reshape((-1, width), order="C")` — `len/width` rows × `width` columns —
/// while `Fortranlike` is `reshape((width, -1), order="F")` — `width` rows ×
/// `len/width` columns with `M[r][c] = data[c*width + r]`. numpy raises on a
/// non-divisible length; here the trailing partial row (C) / column (Fortran)
/// is dropped instead.
pub fn reshape_image(
    data: &[f32],
    width: usize,
    order: ReadingOrder,
) -> Option<(u32, u32, Vec<f32>)> {
    if width == 0 {
        return None;
    }
    // Second axis: rows for CLike, columns for Fortran.
    let other = data.len() / width;
    if other == 0 {
        return None;
    }
    let count = width * other;
    match order {
        ReadingOrder::CLike => Some((width as u32, other as u32, data[..count].to_vec())),
        ReadingOrder::Fortran => {
            // Column-major fill with `width` as the row axis: the displayed
            // image is `width` rows × `other` columns.
            let (rows, cols) = (width, other);
            let mut p = vec![0.0f32; count];
            for (j, col) in data[..count].chunks_exact(rows).enumerate() {
                for (i, &v) in col.iter().enumerate() {
                    p[i * cols + j] = v;
                }
            }
            Some((cols as u32, rows as u32, p))
        }
    }
}

/// Resolve the colormap value range: the data's min/max when `normalize`,
/// otherwise the manual `(min, max)` (PyDM `process_image` / `colorMapMin`/
/// `colorMapMax`). A degenerate range is widened so the colormap is never
/// zero-width.
pub fn color_range(normalize: bool, manual: (f32, f32), pixels: &[f32]) -> (f64, f64) {
    let (lo, hi) = if normalize {
        let mut mn = f32::INFINITY;
        let mut mx = f32::NEG_INFINITY;
        for &v in pixels {
            if v < mn {
                mn = v;
            }
            if v > mx {
                mx = v;
            }
        }
        if mn.is_finite() && mx.is_finite() {
            (mn, mx)
        } else {
            (0.0, 1.0)
        }
    } else {
        manual
    };
    let lo = f64::from(lo);
    let mut hi = f64::from(hi);
    if hi <= lo {
        hi = lo + 1.0;
    }
    (lo, hi)
}

/// A camera/array image driven by an EPICS waveform (PyDM `PyDMImageView`).
pub struct RsdmImageView {
    view: ImageView,
    image_channel: Channel,
    width_channel: Option<Channel>,
    width: usize,
    reading_order: ReadingOrder,
    colormap: ColormapName,
    normalize: bool,
    cm_min: f32,
    cm_max: f32,
    last_image_stamp: u64,
    last_width_stamp: u64,
    latest: Option<Vec<f32>>,
    dirty: bool,
    has_image: bool,
}

impl RsdmImageView {
    /// Connect the image channel (and optional width channel) and create the
    /// view on the given GPU `render_state` and plot `id`.
    pub fn new(
        engine: &Engine,
        render_state: &RenderState,
        id: PlotId,
        image_address: &str,
        width_address: Option<&str>,
    ) -> Result<Self, EngineError> {
        let image_channel = engine.connect(image_address)?;
        let width_channel = match width_address {
            Some(addr) => Some(engine.connect(addr)?),
            None => None,
        };
        Ok(Self {
            view: ImageView::new(render_state, id),
            image_channel,
            width_channel,
            width: 0,
            reading_order: ReadingOrder::default(),
            // PyDMImageView defaults to Inferno (image.py:185).
            colormap: ColormapName::Inferno,
            normalize: false,
            cm_min: 0.0,
            cm_max: 255.0,
            last_image_stamp: 0,
            last_width_stamp: 0,
            latest: None,
            dirty: false,
            has_image: false,
        })
    }

    /// Set a fixed image width (builder style; PyDM `imageWidth`, overridden by a
    /// width channel).
    pub fn with_width(mut self, width: usize) -> Self {
        self.width = width;
        self
    }

    /// Set the array reading order (builder style; PyDM `readingOrder`).
    pub fn with_reading_order(mut self, order: ReadingOrder) -> Self {
        self.reading_order = order;
        self
    }

    /// Set the colormap (builder style).
    pub fn with_colormap(mut self, colormap: ColormapName) -> Self {
        self.colormap = colormap;
        self
    }

    /// Set the manual colormap range (builder style; PyDM `colorMapMin`/
    /// `colorMapMax`).
    pub fn with_color_map_range(mut self, min: f32, max: f32) -> Self {
        self.cm_min = min;
        self.cm_max = max;
        self
    }

    /// Use the data's min/max for the colormap range instead of the manual range
    /// (builder style; PyDM `normalizeData`).
    pub fn with_normalize(mut self, normalize: bool) -> Self {
        self.normalize = normalize;
        self
    }

    /// The image channel.
    pub fn channel(&self) -> &Channel {
        &self.image_channel
    }

    /// The underlying view, for styling.
    pub fn view_mut(&mut self) -> &mut ImageView {
        &mut self.view
    }

    /// Set the colormap range at runtime (PyDM `colorMapMin`/`colorMapMax`).
    pub fn set_color_map_range(&mut self, min: f32, max: f32) {
        self.cm_min = min;
        self.cm_max = max;
        self.dirty = true;
    }

    /// Toggle data normalization at runtime (PyDM `normalizeData`).
    pub fn set_normalize(&mut self, normalize: bool) {
        self.normalize = normalize;
        self.dirty = true;
    }

    /// Whether an image has been uploaded yet.
    pub fn has_image(&self) -> bool {
        self.has_image
    }

    /// Reshape the latest array with the current width and push it to the view.
    fn refresh_image(&mut self) {
        let Some(data) = &self.latest else {
            return;
        };
        let Some((w, h, pixels)) = reshape_image(data, self.width, self.reading_order) else {
            return;
        };
        let (lo, hi) = color_range(self.normalize, (self.cm_min, self.cm_max), &pixels);
        if self
            .view
            .set_image(w, h, &pixels, Colormap::new(self.colormap, lo, hi))
            .is_ok()
        {
            self.has_image = true;
        }
    }

    /// Poll the channels, reshape and re-upload the image when it (or the width)
    /// changed, and render the view this frame. The returned response covers
    /// the view's footprint and carries the MEDM Btn2 middle-click PV copy
    /// (image channel first, then the width channel — PyDM `channels()` order).
    pub fn show(&mut self, ui: &mut egui::Ui) -> egui::Response {
        if let Some(wc) = &self.width_channel {
            let ws = wc.state();
            if ws.connected && ws.stamp != self.last_width_stamp {
                self.last_width_stamp = ws.stamp;
                if let Some(w) = ws.value.as_ref().and_then(PvValue::as_i64)
                    && w >= 1
                {
                    self.width = w as usize;
                    self.dirty = true;
                }
            }
        }

        let is = self.image_channel.state();
        if is.connected && is.stamp != self.last_image_stamp {
            self.last_image_stamp = is.stamp;
            if let Some(data) = is.value.as_ref().and_then(value_to_image) {
                self.latest = Some(data);
                self.dirty = true;
            }
        }

        if self.dirty {
            self.refresh_image();
            self.dirty = false;
        }
        ui.ctx().request_repaint();
        // `ImageView::show` returns no response; the scope registers the drawn
        // footprint as a widget rect, which is what MEDM's Btn2 touches anyway
        // (the whole image-monitor element, side panels included).
        let response = ui.scope(|ui| self.view.show(ui, None, None)).response;
        middle_click_copy(
            ui,
            &response,
            std::iter::once(self.image_channel.address().raw())
                .chain(self.width_channel.as_ref().map(|wc| wc.address().raw())),
        );
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_order_defaults_to_fortran_like_pydm() {
        // PyDMImageView constructs with ReadingOrder.Fortranlike (image.py:196),
        // not Clike.
        assert_eq!(ReadingOrder::default(), ReadingOrder::Fortran);
    }

    #[test]
    fn reshape_clike_is_row_major() {
        // 6 values, width 3 → 2 rows, row-major as-is.
        let data = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let (w, h, px) = reshape_image(&data, 3, ReadingOrder::CLike).expect("reshape");
        assert_eq!((w, h), (3, 2));
        assert_eq!(px, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
    }

    #[test]
    fn reshape_fortran_makes_width_the_row_axis_like_pydm() {
        // R2-56 golden from PyDM image.py:108-109 — reshape((width, -1),
        // order="F") with len=6, width=3 is a 3-row × 2-column image
        // [[d0,d3],[d1,d4],[d2,d5]], NOT a 2×3 transpose.
        let data = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let (w, h, px) = reshape_image(&data, 3, ReadingOrder::Fortran).expect("reshape");
        assert_eq!((w, h), (2, 3), "width is the ROW axis in Fortran order");
        assert_eq!(px, vec![0.0, 3.0, 1.0, 4.0, 2.0, 5.0]);
    }

    #[test]
    fn reshape_fortran_drops_a_trailing_partial_column() {
        // 7 values, width 3 → two full 3-tall columns; the 7th value would
        // start a third column and is dropped (numpy would raise instead).
        let data = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let (w, h, px) = reshape_image(&data, 3, ReadingOrder::Fortran).expect("reshape");
        assert_eq!((w, h), (2, 3));
        assert_eq!(px, vec![0.0, 3.0, 1.0, 4.0, 2.0, 5.0]);
    }

    #[test]
    fn reshape_drops_trailing_partial_row_and_rejects_bad_width() {
        // 7 values, width 3 → 2 full rows (6 values), last dropped.
        let data = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let (w, h, px) = reshape_image(&data, 3, ReadingOrder::CLike).expect("reshape");
        assert_eq!((w, h), (3, 2));
        assert_eq!(px.len(), 6);
        // Width 0 and a width wider than the data yield no image.
        assert!(reshape_image(&data, 0, ReadingOrder::CLike).is_none());
        assert!(reshape_image(&data, 8, ReadingOrder::CLike).is_none());
    }

    #[test]
    fn color_range_manual_vs_normalize() {
        let pixels = [1.0_f32, 2.0, 9.0, 4.0];
        // Manual: returns the configured range.
        assert_eq!(color_range(false, (0.0, 255.0), &pixels), (0.0, 255.0));
        // Normalize: returns the data extremes.
        assert_eq!(color_range(true, (0.0, 255.0), &pixels), (1.0, 9.0));
    }

    #[test]
    fn color_range_widens_degenerate() {
        // Manual min == max → widened so the colormap is never zero-width.
        assert_eq!(color_range(false, (5.0, 5.0), &[]), (5.0, 6.0));
        // All-equal data under normalize → widened.
        assert_eq!(color_range(true, (0.0, 1.0), &[3.0, 3.0, 3.0]), (3.0, 4.0));
    }

    #[test]
    fn value_to_image_converts_numeric_arrays_only() {
        use std::sync::Arc;
        assert_eq!(
            value_to_image(&PvValue::FloatArray(Arc::from([1.0, 2.0]))),
            Some(vec![1.0_f32, 2.0])
        );
        assert_eq!(
            value_to_image(&PvValue::IntArray(Arc::from([3_i64, 4]))),
            Some(vec![3.0_f32, 4.0])
        );
        assert_eq!(value_to_image(&PvValue::Float(1.0)), None);
        assert_eq!(value_to_image(&PvValue::Str("x".into())), None);
    }
}
