use egui::Color32;
use egui_wgpu::RenderState;

use crate::core::backend::ItemHandle;
use crate::core::plot::PlotId;
use crate::core::roi::Roi;
use crate::core::scatter_viz::ProfileAxis;
use crate::core::transform::YAxis;
use crate::render::gpu_curve::CurveData;
use crate::widget::high_level::{
    Plot1D, ProfileMethod, aligned_profile_values, free_line_profile, rect_profile_values,
};

/// Python-style `%g` formatting of a profile-title coordinate — silx uses `%g`
/// throughout `_lineProfileTitle`/`createProfile`: 6 significant digits, with
/// trailing zeros and a bare trailing dot stripped, and scientific notation
/// outside `[1e-4, 1e6)`. Integer-valued coords render without a decimal point
/// (e.g. `3`, `-2`).
fn format_g(v: f64) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    if !v.is_finite() {
        return if v.is_nan() {
            "nan".to_string()
        } else if v > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
    }
    // Normalized decimal exponent via Rust's `{:e}` (reliable at powers of ten,
    // unlike `log10().floor()`).
    let exp: i32 = {
        let s = format!("{:e}", v.abs());
        s.split_once('e')
            .and_then(|(_, e)| e.parse().ok())
            .unwrap_or(0)
    };
    let p: i32 = 6;
    if exp < -4 || exp >= p {
        let s = format!("{:.*e}", (p - 1) as usize, v);
        let (mant, e) = s.split_once('e').unwrap_or((s.as_str(), "0"));
        let mant = strip_trailing_zeros(mant);
        let e_num: i32 = e.parse().unwrap_or(0);
        let sign = if e_num < 0 { '-' } else { '+' };
        format!("{mant}e{sign}{:02}", e_num.abs())
    } else {
        let prec = (p - 1 - exp).max(0) as usize;
        strip_trailing_zeros(&format!("{:.*}", prec, v))
    }
}

/// Drop trailing zeros (and a bare trailing dot) from a fixed-point string,
/// leaving integer-free-of-dot strings untouched.
fn strip_trailing_zeros(s: &str) -> String {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s.to_string()
    }
}

/// `%+g`: [`format_g`] with an explicit sign — silx `{b:+g}` for a diagonal
/// line's intercept.
fn format_g_signed(v: f64) -> String {
    let sign = if v < 0.0 { '-' } else { '+' };
    format!("{sign}{}", format_g(v.abs()))
}

/// Fill `{xlabel}`/`{ylabel}` tokens with the source plot's axis labels,
/// falling back to `"X"`/`"Y"` when a label is empty — silx `_relabelAxes`
/// (`tools/profile/rois.py:53-65`). The source plot supplies the real
/// fallbacks in practice (a [`Plot2D`](crate::widget::high_level::Plot2D)
/// image plot carries `"Columns"`/`"Rows"`).
fn relabel(template: &str, x_label: &str, y_label: &str) -> String {
    let xl = if x_label.is_empty() { "X" } else { x_label };
    let yl = if y_label.is_empty() { "Y" } else { y_label };
    template.replace("{xlabel}", xl).replace("{ylabel}", yl)
}

/// The Y-axis label of a profile plot: silx `str(method).capitalize()`
/// (`rois.py:315`), i.e. the reduction method's name.
fn method_label(method: ProfileMethod) -> &'static str {
    match method {
        ProfileMethod::Mean => "Mean",
        ProfileMethod::Sum => "Sum",
    }
}

/// silx `_alignedFullProfile` integration band for a whole-image aligned
/// profile (`core.py:222-235`), in pixel indices under rsplot's identity image
/// geometry: the profile line at `position` on the axis of length `size`,
/// integrated over `line_width` pixels. Returns `(lo, hi)` = the reported band
/// bounds `min(area)`, `max(area) − 1` (silx `core.py:380,398`).
fn aligned_band(position: f64, size: u32, line_width: u32) -> (i64, i64) {
    let roi_width = i64::from(line_width.max(1)).min(i64::from(size.max(1)));
    let img_pos = position.trunc() as i64;
    let start_f = img_pos as f64 + 0.5 - roi_width as f64 / 2.0;
    let start = (start_f.trunc() as i64).clamp(0, (i64::from(size) - roi_width).max(0));
    (start, start + roi_width - 1)
}

/// The silx `_lineProfileTitle` template (`tools/profile/rois.py:68-89`) for a
/// free line from `(x0, y0)` to `(x1, y1)`, with `{xlabel}`/`{ylabel}` tokens
/// unfilled: a vertical line (`x0 == x1`), a horizontal line (`y0 == y1`), or a
/// diagonal (`y = m·x + b`).
fn line_title_template(x0: f64, y0: f64, x1: f64, y1: f64) -> String {
    if x0 == x1 {
        format!(
            "{{xlabel}} = {}; {{ylabel}} = [{}, {}]",
            format_g(x0),
            format_g(y0),
            format_g(y1)
        )
    } else if y0 == y1 {
        format!(
            "{{ylabel}} = {}; {{xlabel}} = [{}, {}]",
            format_g(y0),
            format_g(x0),
            format_g(x1)
        )
    } else {
        let m = (y1 - y0) / (x1 - x0);
        let b = y0 - m * x0;
        format!(
            "{{ylabel}} = {} * {{xlabel}} {}",
            format_g(m),
            format_g_signed(b)
        )
    }
}

/// The silx `createProfile` title + X-label templates for a free-line image
/// profile (`core.py:405-563`), replicating [`free_line_profile`]'s
/// aligned/general ordering so the title matches the plotted coordinates.
/// Endpoints are `(col, row)` pixel coords (`start`/`end` of a [`Roi::Line`]).
fn line_profile_desc(start: (f64, f64), end: (f64, f64)) -> (String, String) {
    let (sc, sr) = start;
    let (ec, er) = end;
    let aligned =
        (sr.trunc() as i64) == (er.trunc() as i64) || (sc.trunc() as i64) == (ec.trunc() as i64);
    if !aligned {
        // Diagonal: order by column then row (silx `core.py:467-470`).
        let (mut a, mut b) = ((sc, sr), (ec, er));
        if a.0 > b.0 || (a.0 == b.0 && a.1 > b.1) {
            std::mem::swap(&mut a, &mut b);
        }
        (
            line_title_template(a.0, a.1, b.0, b.1),
            "{xlabel}".to_string(),
        )
    } else {
        // Aligned: integer pixel indices, ordered per component.
        let mut s = (sr.trunc() as i64, sc.trunc() as i64); // (row, col)
        let mut e = (er.trunc() as i64, ec.trunc() as i64);
        if s.0 > e.0 || s.1 > e.1 {
            std::mem::swap(&mut s, &mut e);
        }
        let (x0, y0, x1, y1) = (s.1, s.0, e.1, e.0); // (col, row)
        if s.1 == e.1 {
            // Column-aligned (vertical line): x constant, y ranges.
            (
                format!("{{xlabel}} = {x0}; {{ylabel}} = [{y0}, {y1}]"),
                "{ylabel}".to_string(),
            )
        } else {
            // Row-aligned (horizontal line): y constant, x ranges.
            (
                format!("{{ylabel}} = {y0}; {{xlabel}} = [{x0}, {x1}]"),
                "{xlabel}".to_string(),
            )
        }
    }
}

/// The silx `createProfile` title + X-label templates (`{xlabel}`/`{ylabel}`
/// tokens unfilled) for the image profile `roi` at band `line_width`, over a
/// `width`×`height` image under rsplot's identity geometry. Returns `None` for
/// a ROI kind that yields no profile (matching [`profiles_for_roi`]).
fn image_profile_desc(
    roi: &Roi,
    width: u32,
    height: u32,
    line_width: u32,
) -> Option<(String, String)> {
    match roi {
        Roi::HRange { y } => {
            let (lo, hi) = aligned_band((y.0 + y.1) / 2.0, height, line_width);
            let title = if line_width <= 1 {
                format!("{{ylabel}} = {lo}")
            } else {
                format!("{{ylabel}} = [{lo}, {hi}]")
            };
            Some((title, "{xlabel}".to_string()))
        }
        Roi::VRange { x } => {
            let (lo, hi) = aligned_band((x.0 + x.1) / 2.0, width, line_width);
            let title = if line_width <= 1 {
                format!("{{xlabel}} = {lo}")
            } else {
                format!("{{xlabel}} = [{lo}, {hi}]")
            };
            Some((title, "{ylabel}".to_string()))
        }
        Roi::Rect { y, .. } => {
            // Row band reduced along columns (silx "X" aligned width>1 form).
            let hi_row = (f64::from(height) - 1.0).max(0.0);
            let row_min = y.0.min(y.1).round().clamp(0.0, hi_row);
            let row_max = y.0.max(y.1).round().clamp(0.0, hi_row);
            Some((
                format!(
                    "{{ylabel}} = [{}, {}]",
                    format_g(row_min),
                    format_g(row_max)
                ),
                "{xlabel}".to_string(),
            ))
        }
        Roi::Line { start, end } => Some(line_profile_desc(*start, *end)),
        Roi::Cross { center } => {
            // silx renders a cross as two separate profile windows
            // (ProfileImageCrossROI: an hline + a vline sub-ROI). rsplot merges
            // both curves into one window, so the title names the crossing
            // pixel and the X label follows the horizontal (column) sub-profile.
            let (cx, cy) = *center;
            Some((
                format!(
                    "{{xlabel}} = {}; {{ylabel}} = {}",
                    format_g(cx.trunc()),
                    format_g(cy.trunc())
                ),
                "{xlabel}".to_string(),
            ))
        }
        _ => None,
    }
}

/// The self-describing labels of a profile plot — silx `*ProfileData`'s
/// `title`/`xLabel`/`yLabel`, already relabeled from the source plot. Passed to
/// [`ProfileWindow::set_profile_curve`] for a precomputed profile;
/// [`scatter_profile_labels`] builds the scatter-profile set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProfileLabels {
    /// Profile-plot title (silx `profileName`).
    pub title: String,
    /// Profile X-axis label.
    pub x_label: String,
    /// Profile Y-axis label.
    pub y_label: String,
}

/// The relabeled [`ProfileLabels`] for a scatter line profile — silx
/// `ScatterProfile*ROI.computeProfile` (`rois.py:797-821`): the title is
/// `_lineProfileTitle` over the sampled endpoints; the X label follows the
/// dominant [`ProfileAxis`]; the Y label is the fixed string `"Profile"`.
/// `src_x_label`/`src_y_label` are the source scatter plot's axis labels
/// (empty falls back to `"X"`/`"Y"`).
pub(crate) fn scatter_profile_labels(
    first: [f64; 2],
    last: [f64; 2],
    axis: ProfileAxis,
    src_x_label: &str,
    src_y_label: &str,
) -> ProfileLabels {
    let title_tpl = line_title_template(first[0], first[1], last[0], last[1]);
    let xlabel_tpl = match axis {
        ProfileAxis::X => "{xlabel}",
        ProfileAxis::Y => "{ylabel}",
    };
    ProfileLabels {
        title: relabel(&title_tpl, src_x_label, src_y_label),
        x_label: relabel(xlabel_tpl, src_x_label, src_y_label),
        y_label: "Profile".to_string(),
    }
}

/// A single named profile curve extracted from a profile ROI: a legend label, a
/// draw color, and the `(x, y)` samples. A line/range/rect ROI yields one of
/// these; a [`Roi::Cross`] yields two (silx `ProfileImageCrossROI`'s horizontal
/// and vertical sub-profiles), which is why extraction returns a `Vec`.
struct ProfileCurve {
    label: &'static str,
    color: Color32,
    x: Vec<f64>,
    y: Vec<f64>,
}

/// The horizontal full-row profile `(x, y)` through image `row` (silx
/// `ProfileImageHorizontalLineROI` / `_alignedFullProfile`): `x` is the column
/// index, `y` the band reduction over `line_width` rows centered on `row`. The
/// caller attaches a label/color to wrap it into a [`ProfileCurve`].
fn horizontal_profile_xy(
    width: u32,
    height: u32,
    data: &[f32],
    row: f64,
    line_width: u32,
    method: ProfileMethod,
) -> Option<(Vec<f64>, Vec<f64>)> {
    aligned_profile_values(width, height, data, row, line_width, true, method)
        .ok()
        .map(|y| {
            let x = (0..width as usize).map(|i| i as f64).collect();
            (x, y)
        })
}

/// The vertical full-column profile `(x, y)` through image `col` (silx
/// `ProfileImageVerticalLineROI`): `x` is the row index, `y` the band reduction
/// over `line_width` columns centered on `col`.
fn vertical_profile_xy(
    width: u32,
    height: u32,
    data: &[f32],
    col: f64,
    line_width: u32,
    method: ProfileMethod,
) -> Option<(Vec<f64>, Vec<f64>)> {
    aligned_profile_values(width, height, data, col, line_width, false, method)
        .ok()
        .map(|y| {
            let x = (0..height as usize).map(|i| i as f64).collect();
            (x, y)
        })
}

/// Compute the named profile curve(s) for `roi` over a row-major image,
/// integrating a band of `line_width` pixels and reducing it with `method`
/// (silx `ProfileToolButtons` line-width + mean/sum). Returns an empty `Vec` for
/// ROI kinds that have no profile. Pure dispatch over the tested profile
/// extractors:
///
/// - [`Roi::Line`] -> [`line_profile_band`] (bilinear band, silx
///   `BilinearImage.profile_line`).
/// - [`Roi::Rect`] -> [`rect_profile_values`] reduced along the columns.
/// - [`Roi::HRange`] / [`Roi::VRange`] -> [`aligned_profile_values`] centered on
///   the range's midpoint with `line_width` as the integration band (silx
///   `_alignedFullProfile`; `int(position)` placement). `line_width == 1`,
///   `Mean` reproduces the single-row/column average.
/// - [`Roi::Cross`] -> **two** curves, the horizontal row-profile and the
///   vertical column-profile through the cross center, shown simultaneously
///   (silx `ProfileImageCrossROI`, which manages an `hline` + `vline` sub-ROI).
fn profiles_for_roi(
    width: u32,
    height: u32,
    data: &[f32],
    roi: &Roi,
    line_width: u32,
    method: ProfileMethod,
) -> Vec<ProfileCurve> {
    match roi {
        Roi::Line { start, end } => {
            free_line_profile(width, height, data, *start, *end, line_width, method)
                .ok()
                .map(|(x, y)| ProfileCurve {
                    label: "profile",
                    color: Color32::YELLOW,
                    x,
                    y,
                })
                .into_iter()
                .collect()
        }
        Roi::Rect { x, y } => {
            rect_profile_values(width, height, data, (x.0, x.1, y.0, y.1), true, method)
                .ok()
                .map(|(x, y)| ProfileCurve {
                    label: "profile",
                    color: Color32::YELLOW,
                    x,
                    y,
                })
                .into_iter()
                .collect()
        }
        Roi::HRange { y } => {
            let row = (y.0 + y.1) / 2.0;
            horizontal_profile_xy(width, height, data, row, line_width, method)
                .map(|(x, y)| ProfileCurve {
                    label: "profile",
                    color: Color32::YELLOW,
                    x,
                    y,
                })
                .into_iter()
                .collect()
        }
        Roi::VRange { x } => {
            let col = (x.0 + x.1) / 2.0;
            vertical_profile_xy(width, height, data, col, line_width, method)
                .map(|(x, y)| ProfileCurve {
                    label: "profile",
                    color: Color32::YELLOW,
                    x,
                    y,
                })
                .into_iter()
                .collect()
        }
        // Cross profile: extract both the horizontal (row through cy) and
        // vertical (column through cx) full profiles and show them together,
        // mirroring silx `ProfileImageCrossROI` (two sub-ROIs, one window).
        Roi::Cross { center } => {
            let (cx, cy) = *center;
            let h =
                horizontal_profile_xy(width, height, data, cy, line_width, method).map(|(x, y)| {
                    ProfileCurve {
                        label: "h profile",
                        color: Color32::YELLOW,
                        x,
                        y,
                    }
                });
            let v =
                vertical_profile_xy(width, height, data, cx, line_width, method).map(|(x, y)| {
                    ProfileCurve {
                        label: "v profile",
                        color: Color32::from_rgb(0, 200, 255),
                        x,
                        y,
                    }
                });
            [h, v].into_iter().flatten().collect()
        }
        _ => Vec::new(),
    }
}

/// The image + ROI the current profile was extracted from, retained so the
/// profile recomputes when the line width, reduction method, or image data
/// change — not only during a fresh drag. Mirrors silx `ProfileManager`, which
/// recomputes on item DATA/POSITION change and on `setProfileMethod`/
/// `setProfileLineWidth` (`manager.py:936-944`, `rois.py:238-257`). `None` until
/// the first image-ROI [`ProfileWindow::update_profile`]; cleared by the
/// precomputed-curve path so a later width/method edit never re-derives a stale
/// image ROI over a scatter/stack profile.
struct ProfileSource {
    width: u32,
    height: u32,
    data: Vec<f32>,
    roi: Roi,
}

/// A window widget to display the 1D profile of an image based on an ROI.
pub struct ProfileWindow {
    plot: Plot1D,
    /// The retained image + ROI the profile was last extracted from (see
    /// [`ProfileSource`]); drives width/method/data-change recompute.
    source: Option<ProfileSource>,
    /// Handles of the live profile curves. One for a line/range/rect ROI; two
    /// for a cross ROI (the horizontal and vertical sub-profiles). Rebuilt when
    /// the curve count changes between updates (silx `ProfileImageCrossROI`).
    curve_handles: Vec<ItemHandle>,
    window_id: egui::Id,
    open: bool,
    /// Band width in pixels for the profile integration (silx
    /// `ProfileToolButton` line width); `1` is a single-pixel line.
    line_width: u32,
    /// Band reduction: average (silx default) or sum (silx
    /// `ProfileOptionToolButton` method).
    method: ProfileMethod,
    /// Initial outer size of the profile viewport, in points. Reused for both
    /// the viewport builder and the "beside the main window" placement maths.
    size: egui::Vec2,
    /// Position chosen for the *current* open session. Computed once when the
    /// window opens and then left untouched so the user can freely drag it
    /// (re-passing an unchanged position never re-issues `OuterPosition`).
    placement: Option<egui::Pos2>,
    /// Last observed outer position of the profile viewport, restored as the
    /// initial placement on the next open — mirrors silx
    /// `ProfileManager._previousWindowGeometry`.
    remembered_pos: Option<egui::Pos2>,
    /// The source plot's X/Y axis labels, threaded in by [`update_profile`] so
    /// the computed title/axis labels relabel `{xlabel}`/`{ylabel}` from the
    /// originating image plot (silx `_relabelAxes`). Default `"Columns"`/
    /// `"Rows"` matches a [`Plot2D`](crate::widget::high_level::Plot2D).
    src_x_label: String,
    src_y_label: String,
}

impl ProfileWindow {
    /// Create a new ProfileWindow with a backing Plot1D.
    pub fn new(render_state: &RenderState, plot_id: PlotId) -> Self {
        let mut plot = Plot1D::new(render_state, plot_id);
        plot.set_graph_title("Profile");

        Self {
            plot,
            source: None,
            curve_handles: Vec::new(),
            window_id: egui::Id::new(plot_id).with("profile_window"),
            open: false,
            line_width: 1,
            method: ProfileMethod::Mean,
            size: egui::vec2(420.0, 320.0),
            placement: None,
            remembered_pos: None,
            src_x_label: "Columns".to_string(),
            src_y_label: "Rows".to_string(),
        }
    }

    /// The current profile band width in pixels (silx `ProfileToolButton`).
    pub fn line_width(&self) -> u32 {
        self.line_width
    }

    /// Set the profile band width in pixels (clamped to at least 1) and
    /// recompute the profile from the retained source (silx
    /// `setProfileLineWidth` -> `invalidateProfile`). A no-op recompute when no
    /// image-ROI source is retained.
    pub fn set_line_width(&mut self, width: u32) {
        self.line_width = width.max(1);
        self.recompute();
    }

    /// The current band reduction method (silx `ProfileOptionToolButton`).
    pub fn method(&self) -> ProfileMethod {
        self.method
    }

    /// Set the band reduction method (mean vs sum) and recompute the profile
    /// from the retained source (silx `setProfileMethod` -> `invalidateProfile`).
    /// A no-op recompute when no image-ROI source is retained.
    pub fn set_method(&mut self, method: ProfileMethod) {
        self.method = method;
        self.recompute();
    }

    /// The profile curve `y`-values the retained image-ROI source currently
    /// produces at the active line width and method — one entry per curve
    /// ([`Roi::Cross`] yields two). Empty when no image-ROI source is retained
    /// (before the first drag, or after a precomputed-curve profile). Lets a
    /// caller/test confirm that a width/method/data change flows into the
    /// profile without a fresh drag (the R2-4 recompute contract).
    pub fn active_profile_values(&self) -> Vec<Vec<f64>> {
        match &self.source {
            Some(src) => profiles_for_roi(
                src.width,
                src.height,
                &src.data,
                &src.roi,
                self.line_width,
                self.method,
            )
            .into_iter()
            .map(|c| c.y)
            .collect(),
            None => Vec::new(),
        }
    }

    /// Is the window currently open?
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open or close the window.
    pub fn set_open(&mut self, open: bool) {
        // Closing forgets the current placement so the next open re-runs the
        // beside-the-main-window logic against the latest window position.
        if !open {
            self.placement = None;
        }
        self.open = open;
    }

    /// Re-calculate and update the profile curve based on the given ROI, using
    /// the current line width and reduction method. Retains `(data, roi)` as the
    /// active [`ProfileSource`] so subsequent width/method edits and
    /// [`refresh_image`](Self::refresh_image) calls recompute from it.
    /// `x_label`/`y_label` are the source image plot's axis labels, used to
    /// relabel the computed profile title/axes (silx `_relabelAxes`); an empty
    /// label falls back to `"X"`/`"Y"`.
    pub fn update_profile(
        &mut self,
        width: u32,
        height: u32,
        data: &[f32],
        roi: &Roi,
        x_label: &str,
        y_label: &str,
    ) {
        self.src_x_label = x_label.to_string();
        self.src_y_label = y_label.to_string();
        self.source = Some(ProfileSource {
            width,
            height,
            data: data.to_vec(),
            roi: roi.clone(),
        });
        self.recompute();
    }

    /// Replace the retained image data (keeping the active ROI) and recompute —
    /// the host calls this when its image changes while a profile is open (silx
    /// recompute on item DATA change, `manager.py:936-944`). A no-op when no
    /// profile ROI has been drawn yet, so hosts may call it unconditionally on
    /// every image update.
    pub fn refresh_image(&mut self, width: u32, height: u32, data: &[f32]) {
        let Some(src) = self.source.as_mut() else {
            return;
        };
        src.width = width;
        src.height = height;
        src.data = data.to_vec();
        self.recompute();
    }

    /// Re-extract the profile curve(s) from the retained [`ProfileSource`] with
    /// the current line width and method, and push them to the plot. The single
    /// recompute path for the image-ROI profile: shared by
    /// [`update_profile`](Self::update_profile),
    /// [`refresh_image`](Self::refresh_image), and the in-window width/method
    /// edits. No-op when no source is retained.
    fn recompute(&mut self) {
        let Some(src) = self.source.as_ref() else {
            return;
        };
        let (w, h, roi) = (src.width, src.height, src.roi.clone());
        let curves = profiles_for_roi(w, h, &src.data, &roi, self.line_width, self.method);
        // Self-describing title + axis labels (silx `createProfile` +
        // `computeProfile`): relabel `{xlabel}`/`{ylabel}` from the source plot,
        // append silx's `; width = %d`, and set the Y label to the method name.
        if let Some((title_tpl, xlabel_tpl)) = image_profile_desc(&roi, w, h, self.line_width) {
            let title = format!(
                "{}; width = {}",
                relabel(&title_tpl, &self.src_x_label, &self.src_y_label),
                self.line_width
            );
            let xlabel = relabel(&xlabel_tpl, &self.src_x_label, &self.src_y_label);
            self.plot.set_graph_title(title);
            self.plot.set_graph_x_label(xlabel);
            self.plot
                .set_graph_y_label(method_label(self.method), YAxis::Left);
        }
        self.set_curves(curves);
    }

    /// Display a single precomputed `(x, y)` profile curve, for tool bars whose
    /// profile is sampled directly rather than re-derived from an image + ROI
    /// (silx `ScatterProfileToolBar` / `Profile3DToolBar`, whose profiles come
    /// from [`crate::core::scatter_viz::scatter_line_profile`] / a stack
    /// reduction). `label` names the curve in the legend; `color` is its stroke.
    /// `labels` are the already-computed, already-relabeled profile-plot
    /// descriptions (silx `CurveProfileData.title`/`xLabel`/`yLabel`) — see
    /// [`scatter_profile_labels`]. An empty `x` is ignored (the previous profile
    /// stays shown).
    pub fn set_profile_curve(
        &mut self,
        label: &'static str,
        color: Color32,
        x: Vec<f64>,
        y: Vec<f64>,
        labels: &ProfileLabels,
    ) {
        if x.is_empty() {
            return;
        }
        // This profile is sampled upstream, not from a retained image + ROI, so
        // drop any retained source: a later width/method edit must not re-derive
        // a stale image profile over this precomputed curve.
        self.source = None;
        self.plot.set_graph_title(labels.title.clone());
        self.plot.set_graph_x_label(labels.x_label.clone());
        self.plot
            .set_graph_y_label(labels.y_label.clone(), YAxis::Left);
        self.set_curves(vec![ProfileCurve { label, color, x, y }]);
    }

    /// Push `curves` into the backing [`Plot1D`] and auto-scale, the shared body
    /// of [`Self::update_profile`] and [`Self::set_profile_curve`]. An empty list
    /// leaves the current profile untouched (so a no-op extraction does not blank
    /// the window).
    fn set_curves(&mut self, curves: Vec<ProfileCurve>) {
        if curves.is_empty() {
            return;
        }

        // When the curve count changes (line/range/rect ↔ cross), drop the old
        // handles so stale curves do not linger; otherwise update in place.
        if self.curve_handles.len() != curves.len() {
            for handle in self.curve_handles.drain(..) {
                self.plot.remove(handle);
            }
        }

        for (i, c) in curves.into_iter().enumerate() {
            if let Some(&handle) = self.curve_handles.get(i) {
                let curve = CurveData::new(c.x, c.y, c.color);
                self.plot.update_curve_data(handle, &curve);
            } else {
                let handle = self
                    .plot
                    .add_curve_with_legend(&c.x, &c.y, c.color, c.label);
                self.curve_handles.push(handle);
            }
        }
        // Auto-scale limits based on data.
        self.plot.reset_zoom_to_data();
    }

    /// Show the profile in its own native OS window (a separate egui viewport).
    ///
    /// Using a viewport instead of an [`egui::Window`] lets the profile be
    /// moved anywhere on the desktop, including outside the parent application
    /// window. When it first opens it is positioned *beside* the main window
    /// (preferring the right side, then the left, then the roomier screen
    /// edge) and vertically centred on it, so it does not cover the image —
    /// mirroring silx `ProfileManager.initProfileWindow`. After that the user
    /// can drag it anywhere, and the position is restored on the next open.
    ///
    /// On backends without multi-viewport support (Wayland, Android, web) egui
    /// transparently falls back to an embedded in-app window and the placement
    /// maths is skipped because the window position is not exposed.
    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }

        // Choose the initial position once per open session: restore the last
        // place the user left it, else sit beside the main window.
        if self.placement.is_none() {
            self.placement = self
                .remembered_pos
                .or_else(|| crate::widget::detached::beside_main_window(ctx, self.size));
        }

        let viewport_id = egui::ViewportId::from_hash_of(self.window_id);
        let mut builder = egui::ViewportBuilder::default()
            .with_title("Profile")
            .with_inner_size(self.size);
        if let Some(pos) = self.placement {
            builder = builder.with_position(pos);
        }

        let mut close_requested = false;
        let mut live_pos = None;
        ctx.show_viewport_immediate(viewport_id, builder, |ui, _class| {
            // Line-width + method controls (silx ProfileToolButton / method
            // option). Each edit routes through `set_line_width`/`set_method`,
            // which recompute the profile immediately from the retained source
            // (silx `setProfileLineWidth`/`setProfileMethod` ->
            // `invalidateProfile`), so it no longer waits for the next drag.
            ui.horizontal(|ui| {
                ui.label("Width:");
                let mut width = self.line_width;
                if ui
                    .add(
                        egui::DragValue::new(&mut width)
                            .speed(1.0)
                            .range(1..=u32::MAX),
                    )
                    .on_hover_text("Profile band width in pixels")
                    .changed()
                {
                    self.set_line_width(width);
                }
                ui.separator();
                ui.label("Method:");
                let mut method = self.method;
                egui::ComboBox::from_id_salt("profile_method")
                    .selected_text(match method {
                        ProfileMethod::Mean => "Mean",
                        ProfileMethod::Sum => "Sum",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut method, ProfileMethod::Mean, "Mean");
                        ui.selectable_value(&mut method, ProfileMethod::Sum, "Sum");
                    });
                if method != self.method {
                    self.set_method(method);
                }
            });
            ui.separator();
            self.plot.show(ui);
            ui.ctx().input(|i| {
                let vp = i.viewport();
                if vp.close_requested() {
                    close_requested = true;
                }
                // Track where the user has moved the window so the next open
                // restores it (silx `_previousWindowGeometry`).
                live_pos = vp.outer_rect.map(|r| r.min);
            });
        });

        if let Some(pos) = live_pos {
            self.remembered_pos = Some(pos);
        }
        if close_requested {
            self.open = false;
            self.placement = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 3×3 ramp where value == row*10 + col, so band reductions are easy to
    // verify by hand.
    fn ramp_3x3() -> Vec<f32> {
        let mut v = Vec::with_capacity(9);
        for row in 0..3 {
            for col in 0..3 {
                v.push((row * 10 + col) as f32);
            }
        }
        v
    }

    #[test]
    fn profile_for_roi_hrange_width_and_method() {
        let data = ramp_3x3();
        // HRange centred on row 1: width 1, Mean -> just row 1 = [10, 11, 12].
        let curves = profiles_for_roi(
            3,
            3,
            &data,
            &Roi::HRange { y: (1.0, 1.0) },
            1,
            ProfileMethod::Mean,
        );
        assert_eq!(curves.len(), 1);
        assert_eq!(curves[0].y, vec![10.0, 11.0, 12.0]);

        // Width 3, Sum -> every column summed over all three rows:
        // col c -> (0+10+20) + c*3 = 30 + 3c = [30, 33, 36].
        let curves = profiles_for_roi(
            3,
            3,
            &data,
            &Roi::HRange { y: (1.0, 1.0) },
            3,
            ProfileMethod::Sum,
        );
        assert_eq!(curves.len(), 1);
        assert_eq!(curves[0].y, vec![30.0, 33.0, 36.0]);
    }

    #[test]
    fn profile_for_roi_cross_yields_horizontal_and_vertical_curves() {
        // A cross at (col=1, row=1) extracts BOTH the row-1 horizontal profile
        // and the col-1 vertical profile simultaneously (silx
        // ProfileImageCrossROI), width 1 / Mean = the raw line.
        let data = ramp_3x3();
        let curves = profiles_for_roi(
            3,
            3,
            &data,
            &Roi::Cross { center: (1.0, 1.0) },
            1,
            ProfileMethod::Mean,
        );
        assert_eq!(curves.len(), 2);
        // Horizontal profile = row 1 across columns: value == 10 + col.
        assert_eq!(curves[0].label, "h profile");
        assert_eq!(curves[0].y, vec![10.0, 11.0, 12.0]);
        // Vertical profile = column 1 across rows: value == row*10 + 1.
        assert_eq!(curves[1].label, "v profile");
        assert_eq!(curves[1].y, vec![1.0, 11.0, 21.0]);
    }

    #[test]
    fn profile_for_roi_returns_empty_for_unsupported_kind() {
        let data = ramp_3x3();
        assert!(
            profiles_for_roi(
                3,
                3,
                &data,
                &Roi::Point { x: 1.0, y: 1.0 },
                1,
                ProfileMethod::Mean,
            )
            .is_empty()
        );
    }

    // --- R2-6 title/label computation ---------------------------------------

    #[test]
    fn format_g_matches_python_percent_g() {
        assert_eq!(format_g(0.0), "0");
        assert_eq!(format_g(3.0), "3");
        assert_eq!(format_g(-2.0), "-2");
        assert_eq!(format_g(1.5), "1.5");
        assert_eq!(format_g(1.0 / 3.0), "0.333333");
        // Scientific outside [1e-4, 1e6): Python '%g' % 1234567 == '1.23457e+06'.
        assert_eq!(format_g(1_234_567.0), "1.23457e+06");
        // '%g' % 0.00001234 == '1.234e-05'.
        assert_eq!(format_g(0.00001234), "1.234e-05");
    }

    #[test]
    fn format_g_signed_always_carries_a_sign() {
        assert_eq!(format_g_signed(3.0), "+3");
        assert_eq!(format_g_signed(-2.5), "-2.5");
        assert_eq!(format_g_signed(0.0), "+0");
    }

    #[test]
    fn relabel_fills_tokens_and_falls_back_to_x_y() {
        assert_eq!(
            relabel("{ylabel} = 1; {xlabel} = [0, 5]", "Columns", "Rows"),
            "Rows = 1; Columns = [0, 5]"
        );
        // Empty source labels fall back to X/Y (silx `_relabelAxes`).
        assert_eq!(relabel("{xlabel} vs {ylabel}", "", ""), "X vs Y");
    }

    #[test]
    fn hrange_title_is_the_band_row_and_widens_with_line_width() {
        // Width 1 over a 3-row image at row 1: single reported row.
        assert_eq!(
            image_profile_desc(&Roi::HRange { y: (1.0, 1.0) }, 3, 3, 1),
            Some(("{ylabel} = 1".to_string(), "{xlabel}".to_string()))
        );
        // Width 3 clamps the band to rows [0, 2] (silx `_alignedFullProfile`).
        assert_eq!(
            image_profile_desc(&Roi::HRange { y: (1.0, 1.0) }, 3, 3, 3),
            Some(("{ylabel} = [0, 2]".to_string(), "{xlabel}".to_string()))
        );
    }

    #[test]
    fn vrange_title_is_the_band_column() {
        assert_eq!(
            image_profile_desc(&Roi::VRange { x: (1.0, 1.0) }, 3, 3, 1),
            Some(("{xlabel} = 1".to_string(), "{ylabel}".to_string()))
        );
    }

    #[test]
    fn rect_title_is_the_row_range_reduced_over_columns() {
        assert_eq!(
            image_profile_desc(
                &Roi::Rect {
                    x: (0.0, 2.0),
                    y: (1.0, 3.0)
                },
                5,
                5,
                1
            ),
            Some(("{ylabel} = [1, 3]".to_string(), "{xlabel}".to_string()))
        );
    }

    #[test]
    fn cross_title_names_the_crossing_pixel() {
        assert_eq!(
            image_profile_desc(&Roi::Cross { center: (2.0, 1.0) }, 5, 5, 1),
            Some((
                "{xlabel} = 2; {ylabel} = 1".to_string(),
                "{xlabel}".to_string()
            ))
        );
    }

    #[test]
    fn line_title_horizontal_vertical_and_diagonal() {
        // Row-aligned (horizontal) line at row 2 spanning columns 0..5.
        assert_eq!(
            image_profile_desc(
                &Roi::Line {
                    start: (0.0, 2.0),
                    end: (5.0, 2.0)
                },
                8,
                8,
                1
            ),
            Some((
                "{ylabel} = 2; {xlabel} = [0, 5]".to_string(),
                "{xlabel}".to_string()
            ))
        );
        // Column-aligned (vertical) line at column 3 spanning rows 1..6.
        assert_eq!(
            image_profile_desc(
                &Roi::Line {
                    start: (3.0, 1.0),
                    end: (3.0, 6.0)
                },
                8,
                8,
                1
            ),
            Some((
                "{xlabel} = 3; {ylabel} = [1, 6]".to_string(),
                "{ylabel}".to_string()
            ))
        );
        // Diagonal line (0,0)->(3,4): slope 4/3, intercept 0.
        assert_eq!(
            image_profile_desc(
                &Roi::Line {
                    start: (0.0, 0.0),
                    end: (3.0, 4.0)
                },
                8,
                8,
                1
            ),
            Some((
                "{ylabel} = 1.33333 * {xlabel} +0".to_string(),
                "{xlabel}".to_string()
            ))
        );
    }

    #[test]
    fn line_title_is_independent_of_drag_direction() {
        // Silx orders endpoints (column then row) before titling, so dragging a
        // diagonal either way yields the same title.
        let forward = image_profile_desc(
            &Roi::Line {
                start: (0.0, 0.0),
                end: (3.0, 4.0),
            },
            8,
            8,
            1,
        );
        let reversed = image_profile_desc(
            &Roi::Line {
                start: (3.0, 4.0),
                end: (0.0, 0.0),
            },
            8,
            8,
            1,
        );
        assert_eq!(forward, reversed);
    }

    #[test]
    fn scatter_labels_pick_the_dominant_axis_and_relabel() {
        // Y span (8) > X span (6): X label follows Y ("Rows"); Y label fixed.
        let labels = scatter_profile_labels([0.0, 0.0], [6.0, 8.0], ProfileAxis::Y, "Cols", "Rows");
        assert_eq!(labels.title, "Rows = 1.33333 * Cols +0");
        assert_eq!(labels.x_label, "Rows");
        assert_eq!(labels.y_label, "Profile");
    }
}
