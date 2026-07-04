use egui::Color32;
use egui_wgpu::RenderState;

use crate::core::backend::ItemHandle;
use crate::core::background::{
    Background, DEFAULT_SNIP_WIDTH, DEFAULT_STRIP_ITERATIONS, DEFAULT_STRIP_THRESHOLD_FACTOR,
    DEFAULT_STRIP_WIDTH,
};
use crate::core::fitting::{
    Constraint, DEFAULT_DELTACHI, DEFAULT_FIT_SENSITIVITY, DEFAULT_MAX_ITER, FitFunction,
    FitResult, GaussianEstimateFit, IterativeFit, IterativeFitResult, LinearFit, PeakModel,
    fit_multi_gaussian_full, fit_peak_constrained, fit_peak_from, fit_peak_with_background,
};
use crate::core::peaks::guess_fwhm;
use crate::core::plot::PlotId;
use crate::render::gpu_curve::CurveData;
use crate::widget::high_level::Plot1D;

/// Format a fitted parameter value together with its estimated error as
/// `value ± error`, mirroring the silx `FitWidget` results table which shows a
/// value and its sigma (the square root of the covariance diagonal).
///
/// A non-finite error is rendered without the `±` term (silx leaves the
/// uncertainty blank when it cannot be computed).
pub fn format_param_value_error(value: f64, error: f64) -> String {
    if error.is_finite() {
        format!("{value:.6} ± {error:.6}")
    } else {
        format!("{value:.6}")
    }
}

/// Format the reduced chi-square goodness-of-fit metric for the results table
/// (silx `FitWidget` shows `chisq` / reduced chi-square). `None` (non-positive
/// degrees of freedom) renders as `N/A`.
pub fn format_reduced_chisq(reduced_chisq: Option<f64>) -> String {
    match reduced_chisq {
        Some(rc) if rc.is_finite() => format!("{rc:.6}"),
        _ => "N/A".to_string(),
    }
}

/// The finite x extent of `x_data` as a `(min, max)` fit window, or `(0.0, 1.0)`
/// when there is no finite sample. Used to seed the FitWidget's xmin/xmax when
/// the user first enables range limiting (silx defaults them to the curve's x
/// range).
fn default_fit_range_of(x_data: &[f64]) -> (f64, f64) {
    let mut it = x_data.iter().copied().filter(|v| v.is_finite());
    match it.next() {
        Some(first) => {
            let (mut lo, mut hi) = (first, first);
            for v in it {
                lo = lo.min(v);
                hi = hi.max(v);
            }
            (lo, hi)
        }
        None => (0.0, 1.0),
    }
}

/// Select the fit-ready `(x, y)` pairs: drop any pair with a non-finite
/// member, and keep only in-range points when `range` is set (bounds
/// normalized, inclusive). silx FitManager builds
/// `_finite_mask = isfinite(x) & isfinite(y)` once (fitmanager.py:803-808)
/// and every estimation and fit runs on the masked arrays (:434-436,
/// :884-885) — a stray NaN sample never aborts the fit. The engines
/// themselves still reject non-finite input, as silx `leastsq`'s
/// `asarray_chkfinite` does; the filtering belongs to this manager layer.
fn fit_ready_data(
    x_data: &[f64],
    y_data: &[f64],
    range: Option<(f64, f64)>,
) -> (Vec<f64>, Vec<f64>) {
    let range = range.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for (&xi, &yi) in x_data.iter().zip(y_data.iter()) {
        if !(xi.is_finite() && yi.is_finite()) {
            continue;
        }
        if let Some((lo, hi)) = range
            && (xi < lo || xi > hi)
        {
            continue;
        }
        xs.push(xi);
        ys.push(yi);
    }
    (xs, ys)
}

/// The background theories offered by the [`FitWidget`] background combo, in
/// silx `bgtheories.THEORY` order, each paired with its silx display label.
const BACKGROUND_CHOICES: [(Background, &str); 9] = [
    (Background::None, "No Background"),
    (Background::Constant, "Constant"),
    (Background::Linear, "Linear"),
    (
        Background::Strip {
            width: DEFAULT_STRIP_WIDTH,
            niterations: DEFAULT_STRIP_ITERATIONS,
            factor: DEFAULT_STRIP_THRESHOLD_FACTOR,
        },
        "Strip",
    ),
    (
        Background::Snip {
            width: DEFAULT_SNIP_WIDTH,
        },
        "Snip",
    ),
    (Background::Polynomial { degree: 2 }, "Degree 2 Polynomial"),
    (Background::Polynomial { degree: 3 }, "Degree 3 Polynomial"),
    (Background::Polynomial { degree: 4 }, "Degree 4 Polynomial"),
    (Background::Polynomial { degree: 5 }, "Degree 5 Polynomial"),
];

/// The combo label for `background`: its [`BACKGROUND_CHOICES`] entry, or the
/// generic [`Background::name`] when it is a non-default parameterisation.
fn background_label(background: Background) -> &'static str {
    BACKGROUND_CHOICES
        .iter()
        .find(|(bg, _)| *bg == background)
        .map(|(_, label)| *label)
        .unwrap_or_else(|| background.name())
}

/// A constraint "code" the user picks in the parameter table, without its
/// payload — silx `Parameters.code_options` (`Parameters.py:205-215`). This is
/// what the combo selects; the payload (`QUOTED` min/max, `FACTOR`/`DELTA`/`SUM`
/// reference + value) is then edited in the adjacent fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConstraintKind {
    /// `FREE` — no restriction.
    Free,
    /// `POSITIVE` — kept positive.
    Positive,
    /// `QUOTED` — confined to a `[min, max]` interval.
    Quoted,
    /// `FIXED` — held at its starting value.
    Fixed,
    /// `FACTOR` — tied to another parameter by a multiplier.
    Factor,
    /// `DELTA` — tied to another parameter by an additive offset.
    Delta,
    /// `SUM` — the pair tied to a constant sum.
    Sum,
    /// `IGNORE` — held and stripped from the model call.
    Ignore,
}

/// silx `Parameters.code_options` display string for a [`ConstraintKind`].
fn constraint_kind_label(kind: ConstraintKind) -> &'static str {
    match kind {
        ConstraintKind::Free => "FREE",
        ConstraintKind::Positive => "POSITIVE",
        ConstraintKind::Quoted => "QUOTED",
        ConstraintKind::Fixed => "FIXED",
        ConstraintKind::Factor => "FACTOR",
        ConstraintKind::Delta => "DELTA",
        ConstraintKind::Sum => "SUM",
        ConstraintKind::Ignore => "IGNORE",
    }
}

/// The code of an existing [`Constraint`] (drops its payload).
fn constraint_kind(constraint: Constraint) -> ConstraintKind {
    match constraint {
        Constraint::Free => ConstraintKind::Free,
        Constraint::Positive => ConstraintKind::Positive,
        Constraint::Quoted { .. } => ConstraintKind::Quoted,
        Constraint::Fixed => ConstraintKind::Fixed,
        Constraint::Factor { .. } => ConstraintKind::Factor,
        Constraint::Delta { .. } => ConstraintKind::Delta,
        Constraint::Sum { .. } => ConstraintKind::Sum,
        Constraint::Ignored => ConstraintKind::Ignore,
    }
}

/// The constraint codes offered in the editor combo — silx
/// `Parameters.code_options` minus the group-management `ADD`/`SHOW` pseudo-codes
/// and `IGNORE`. siplot fits a single model (one parameter group), so there is
/// no redundant grouped parameter for `IGNORE` to drop, and no second group for
/// `ADD`; the remaining seven are the per-parameter constraints
/// `core::fitting::leastsq_constrained` enforces.
const UI_CONSTRAINT_KINDS: [ConstraintKind; 7] = [
    ConstraintKind::Free,
    ConstraintKind::Positive,
    ConstraintKind::Quoted,
    ConstraintKind::Fixed,
    ConstraintKind::Factor,
    ConstraintKind::Delta,
    ConstraintKind::Sum,
];

/// Whether `constraint` ties a parameter to another (`FACTOR`/`DELTA`/`SUM`) or
/// drops it (`IGNORE`) — such a parameter cannot itself be the *reference* of a
/// tie (silx `getRelatedCandidates` excludes these, `Parameters.py:578-583`).
fn is_tied(constraint: Constraint) -> bool {
    matches!(
        constraint,
        Constraint::Factor { .. }
            | Constraint::Delta { .. }
            | Constraint::Sum { .. }
            | Constraint::Ignored
    )
}

/// The "best" related parameter for a `FACTOR`/`DELTA`/`SUM` tie on
/// `param_index`, mirroring silx `Parameters.getRelatedCandidates`
/// (`Parameters.py:565-600`): the first *other* parameter whose own constraint
/// is not itself a tie or `IGNORE` (you cannot chain ties). Returns `None` when
/// no candidate exists — silx `setCodeValue` rejects the change in that case
/// (`Parameters.py:477-479`).
///
/// silx additionally prefers the previous `relatedto` or a parameter sharing the
/// same base name; the single-peak models this editor serves have distinct
/// parameter names, so that refinement collapses to the first candidate.
fn default_related_reference(param_index: usize, constraints: &[Constraint]) -> Option<usize> {
    (0..constraints.len()).find(|&j| j != param_index && !is_tied(constraints[j]))
}

/// Build the [`Constraint`] for a newly-selected [`ConstraintKind`] on
/// parameter `param_index`, seeding silx defaults. `FACTOR`/`DELTA`/`SUM` need a
/// related parameter ([`default_related_reference`]); when none exists this
/// returns `None`, mirroring silx `setCodeValue` rejecting the selection. The
/// `QUOTED` seed `[0, 1]` is a placeholder the user edits in the min/max fields
/// (silx seeds from the fit theory's estimate, which this manual editor lacks).
fn make_constraint(
    kind: ConstraintKind,
    param_index: usize,
    constraints: &[Constraint],
) -> Option<Constraint> {
    Some(match kind {
        ConstraintKind::Free => Constraint::Free,
        ConstraintKind::Positive => Constraint::Positive,
        ConstraintKind::Quoted => Constraint::Quoted { min: 0.0, max: 1.0 },
        ConstraintKind::Fixed => Constraint::Fixed,
        ConstraintKind::Ignore => Constraint::Ignored,
        ConstraintKind::Factor => Constraint::Factor {
            reference: default_related_reference(param_index, constraints)?,
            factor: 1.0,
        },
        ConstraintKind::Delta => Constraint::Delta {
            reference: default_related_reference(param_index, constraints)?,
            delta: 0.0,
        },
        ConstraintKind::Sum => Constraint::Sum {
            reference: default_related_reference(param_index, constraints)?,
            sum: 0.0,
        },
    })
}

/// A combo that picks the *reference* parameter for a `FACTOR`/`DELTA`/`SUM`
/// tie on `param_index`, offering every other parameter not itself tied/ignored
/// (`tieable[j]`), shown by name (silx `relatedto` candidate list).
fn reference_param_combo(
    ui: &mut egui::Ui,
    param_index: usize,
    reference: &mut usize,
    names: &[String],
    tieable: &[bool],
) {
    let selected = names.get(*reference).map(String::as_str).unwrap_or("?");
    egui::ComboBox::from_id_salt(("fit_ref_combo", param_index))
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for (j, nm) in names.iter().enumerate() {
                if tieable.get(j).copied().unwrap_or(false) {
                    ui.selectable_value(reference, j, nm.as_str());
                }
            }
        });
}

/// The selectable fit model in [`FitWidget`].
///
/// The first two variants preserve the original analytical fits (Linear and
/// the analytical Gaussian estimate); the remaining variants drive the
/// iterative Levenberg-Marquardt path with a results table that includes
/// per-parameter errors and reduced chi-square.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitModelChoice {
    /// Analytical linear fit (`LinearFit`).
    Linear,
    /// Analytical Gaussian estimate (`GaussianEstimateFit`).
    GaussianEstimate,
    /// Iterative Gaussian (height parameterisation).
    IterativeGaussian,
    /// Iterative Gaussian (area parameterisation).
    IterativeGaussianArea,
    /// Iterative asymmetric (split) Gaussian.
    IterativeSplitGaussian,
    /// Iterative Lorentzian.
    IterativeLorentzian,
    /// Iterative Lorentzian (area parameterisation).
    IterativeLorentzianArea,
    /// Iterative asymmetric (split) Lorentzian.
    IterativeSplitLorentzian,
    /// Iterative pseudo-Voigt.
    IterativePseudoVoigt,
    /// Iterative pseudo-Voigt (area parameterisation).
    IterativeAreaPseudoVoigt,
    /// Iterative asymmetric (split) pseudo-Voigt.
    IterativeSplitPseudoVoigt,
    /// Iterative split pseudo-Voigt with per-side eta.
    IterativeSplitPseudoVoigt2,
    /// Iterative step down (descending erf edge).
    IterativeStepDown,
    /// Iterative step up (ascending erf edge).
    IterativeStepUp,
    /// Iterative slit (rising then falling edges).
    IterativeSlit,
    /// Iterative arctan step up.
    IterativeAtanStepUp,
    /// Iterative Hypermet (Gaussian + short tail + long tail + step).
    IterativeHypermet,
    /// Degree-2 polynomial fit.
    IterativePolynomial2,
    /// Degree-3 polynomial fit.
    IterativePolynomial3,
    /// Degree-4 polynomial fit.
    IterativePolynomial4,
    /// Degree-5 polynomial fit.
    IterativePolynomial5,
    /// Multi-peak Gaussian fit with automatic peak search (silx `sum_gauss`
    /// theory): locate N peaks and fit them simultaneously.
    MultiGaussian,
}

impl FitModelChoice {
    /// All choices, in display order.
    pub const ALL: [FitModelChoice; 22] = [
        FitModelChoice::Linear,
        FitModelChoice::GaussianEstimate,
        FitModelChoice::IterativeGaussian,
        FitModelChoice::IterativeGaussianArea,
        FitModelChoice::IterativeSplitGaussian,
        FitModelChoice::IterativeLorentzian,
        FitModelChoice::IterativeLorentzianArea,
        FitModelChoice::IterativeSplitLorentzian,
        FitModelChoice::IterativePseudoVoigt,
        FitModelChoice::IterativeAreaPseudoVoigt,
        FitModelChoice::IterativeSplitPseudoVoigt,
        FitModelChoice::IterativeSplitPseudoVoigt2,
        FitModelChoice::IterativeStepDown,
        FitModelChoice::IterativeStepUp,
        FitModelChoice::IterativeSlit,
        FitModelChoice::IterativeAtanStepUp,
        FitModelChoice::IterativeHypermet,
        FitModelChoice::IterativePolynomial2,
        FitModelChoice::IterativePolynomial3,
        FitModelChoice::IterativePolynomial4,
        FitModelChoice::IterativePolynomial5,
        FitModelChoice::MultiGaussian,
    ];

    /// Display name for the combo box.
    pub fn label(self) -> &'static str {
        match self {
            FitModelChoice::Linear => "Linear",
            FitModelChoice::GaussianEstimate => "Gaussian (Estimate)",
            FitModelChoice::IterativeGaussian => "Gaussian (Iterative)",
            FitModelChoice::IterativeGaussianArea => "Gaussian Area (Iterative)",
            FitModelChoice::IterativeSplitGaussian => "Split Gaussian (Iterative)",
            FitModelChoice::IterativeLorentzian => "Lorentzian (Iterative)",
            FitModelChoice::IterativeLorentzianArea => "Lorentzian Area (Iterative)",
            FitModelChoice::IterativeSplitLorentzian => "Split Lorentzian (Iterative)",
            FitModelChoice::IterativePseudoVoigt => "Pseudo-Voigt (Iterative)",
            FitModelChoice::IterativeAreaPseudoVoigt => "Pseudo-Voigt Area (Iterative)",
            FitModelChoice::IterativeSplitPseudoVoigt => "Split Pseudo-Voigt (Iterative)",
            FitModelChoice::IterativeSplitPseudoVoigt2 => "Split Pseudo-Voigt 2 (Iterative)",
            FitModelChoice::IterativeStepDown => "Step Down (Iterative)",
            FitModelChoice::IterativeStepUp => "Step Up (Iterative)",
            FitModelChoice::IterativeSlit => "Slit (Iterative)",
            FitModelChoice::IterativeAtanStepUp => "Arctan Step Up (Iterative)",
            FitModelChoice::IterativeHypermet => "Hypermet (Iterative)",
            FitModelChoice::IterativePolynomial2 => "Degree 2 Polynomial",
            FitModelChoice::IterativePolynomial3 => "Degree 3 Polynomial",
            FitModelChoice::IterativePolynomial4 => "Degree 4 Polynomial",
            FitModelChoice::IterativePolynomial5 => "Degree 5 Polynomial",
            FitModelChoice::MultiGaussian => "Gaussians (Multi-peak)",
        }
    }

    /// The [`PeakModel`] this choice maps to, if it is one of the iterative
    /// models.
    pub fn peak_model(self) -> Option<PeakModel> {
        match self {
            FitModelChoice::IterativeGaussian => Some(PeakModel::Gaussian),
            FitModelChoice::IterativeGaussianArea => Some(PeakModel::GaussianArea),
            FitModelChoice::IterativeSplitGaussian => Some(PeakModel::SplitGaussian),
            FitModelChoice::IterativeLorentzian => Some(PeakModel::Lorentzian),
            FitModelChoice::IterativeLorentzianArea => Some(PeakModel::LorentzianArea),
            FitModelChoice::IterativeSplitLorentzian => Some(PeakModel::SplitLorentzian),
            FitModelChoice::IterativePseudoVoigt => Some(PeakModel::PseudoVoigt),
            FitModelChoice::IterativeAreaPseudoVoigt => Some(PeakModel::AreaPseudoVoigt),
            FitModelChoice::IterativeSplitPseudoVoigt => Some(PeakModel::SplitPseudoVoigt),
            FitModelChoice::IterativeSplitPseudoVoigt2 => Some(PeakModel::SplitPseudoVoigt2),
            FitModelChoice::IterativeStepDown => Some(PeakModel::StepDown),
            FitModelChoice::IterativeStepUp => Some(PeakModel::StepUp),
            FitModelChoice::IterativeSlit => Some(PeakModel::Slit),
            FitModelChoice::IterativeAtanStepUp => Some(PeakModel::AtanStepUp),
            FitModelChoice::IterativeHypermet => Some(PeakModel::Hypermet),
            FitModelChoice::IterativePolynomial2 => Some(PeakModel::Polynomial2),
            FitModelChoice::IterativePolynomial3 => Some(PeakModel::Polynomial3),
            FitModelChoice::IterativePolynomial4 => Some(PeakModel::Polynomial4),
            FitModelChoice::IterativePolynomial5 => Some(PeakModel::Polynomial5),
            // Composite / analytical choices have no single peak model.
            FitModelChoice::Linear
            | FitModelChoice::GaussianEstimate
            | FitModelChoice::MultiGaussian => None,
        }
    }
}

/// A window widget to perform curve fitting on 1D data and display the result.
pub struct FitWidget {
    plot: Plot1D,
    data_handle: Option<ItemHandle>,
    fit_handle: Option<ItemHandle>,
    win: crate::widget::detached::DetachedWindow,
    open: bool,

    // Data
    x_data: Vec<f64>,
    y_data: Vec<f64>,

    // Fit state
    selected_function_idx: usize,
    fit_result: Option<FitResult>,

    // Iterative-fit state (Wave 5, additive).
    selected_choice: FitModelChoice,
    iterative_result: Option<IterativeFitResult>,
    /// Optional fit range `[xmin, xmax]`; `None` fits the whole curve
    /// (silx `FitWidget` xmin/xmax).
    fit_range: Option<(f64, f64)>,
    /// Background theory subtracted before an iterative peak fit (silx
    /// `FitWidget` background combo). `None` fits the raw data unchanged.
    background: Background,
    /// Per-parameter constraints for the current single-peak model (silx
    /// `FitWidget` parameter table). Resynced (cleared to all-`Free`) whenever
    /// the selected model's parameter count changes; empty until first synced.
    constraints: Vec<Constraint>,
    /// Editable initial parameters for the current single-peak model (silx
    /// `FitWidget` parameter table value column). `None` until the first fit
    /// populates it; the next fit then starts from these (possibly edited)
    /// values. Reset on data or model change.
    initial_params: Option<Vec<f64>>,
}

impl FitWidget {
    /// Create a new FitWidget with a backing Plot1D.
    pub fn new(render_state: &RenderState, plot_id: PlotId) -> Self {
        let mut plot = Plot1D::new(render_state, plot_id);
        plot.set_graph_title("Fit Result");

        Self {
            plot,
            data_handle: None,
            fit_handle: None,
            win: crate::widget::detached::DetachedWindow::new(
                egui::Id::new(plot_id).with("fit_widget"),
                egui::vec2(600.0, 400.0),
            ),
            open: false,
            x_data: Vec::new(),
            y_data: Vec::new(),
            selected_function_idx: 0,
            fit_result: None,
            selected_choice: FitModelChoice::Linear,
            iterative_result: None,
            fit_range: None,
            background: Background::None,
            constraints: Vec::new(),
            initial_params: None,
        }
    }

    /// The default fit window when the user first enables range limiting: the
    /// data's finite x extent (silx initialises xmin/xmax from the active
    /// curve's x range).
    fn default_fit_range(&self) -> (f64, f64) {
        default_fit_range_of(&self.x_data)
    }

    /// Set the fit range `[xmin, xmax]`; only points inside it are fitted
    /// (silx `FitWidget` xmin/xmax). Pass `None` to fit the whole curve.
    pub fn set_fit_range(&mut self, range: Option<(f64, f64)>) {
        self.fit_range = range;
    }

    /// The currently selected fit model choice.
    pub fn selected_choice(&self) -> FitModelChoice {
        self.selected_choice
    }

    /// Set the selected fit model choice.
    pub fn set_selected_choice(&mut self, choice: FitModelChoice) {
        self.selected_choice = choice;
    }

    /// The background theory subtracted before an iterative peak fit (silx
    /// `FitWidget` background combo).
    pub fn fit_background(&self) -> Background {
        self.background
    }

    /// Set the background theory subtracted before an iterative peak fit. The
    /// analytical Linear / Gaussian-estimate choices ignore it; iterative peak
    /// models fit the background-subtracted residual and display the
    /// reconstructed total curve.
    pub fn set_fit_background(&mut self, background: Background) {
        self.background = background;
    }

    /// The per-parameter constraints applied to the current single-peak model
    /// (silx `FitWidget` parameter table). Empty until first synced.
    pub fn param_constraints(&self) -> &[Constraint] {
        &self.constraints
    }

    /// Set the per-parameter constraints for the current single-peak model. The
    /// vector is resynced to all-`Free` if its length stops matching the
    /// selected model's parameter count.
    pub fn set_param_constraints(&mut self, constraints: Vec<Constraint>) {
        self.constraints = constraints;
    }

    /// The editable initial parameters for the current single-peak model, once a
    /// fit has populated them (silx `FitWidget` parameter table value column).
    pub fn initial_params(&self) -> Option<&[f64]> {
        self.initial_params.as_deref()
    }

    /// Set the initial parameters the next single-peak fit starts from. Dropped
    /// if the length stops matching the selected model's parameter count.
    pub fn set_initial_params(&mut self, params: Option<Vec<f64>>) {
        self.initial_params = params;
    }

    /// Ensure the per-parameter state matches a model with `n` parameters:
    /// `constraints` resets to all-`Free` when its length differs, and a stale
    /// `initial_params` (wrong length) is dropped (silx clears the parameter
    /// table on theory change). Returns `true` when every constraint is `Free`
    /// (the unconstrained default, so the fit can take the byte-identical path).
    fn ensure_constraints_len(&mut self, n: usize) -> bool {
        if self.constraints.len() != n {
            self.constraints = vec![Constraint::Free; n];
        }
        if self.initial_params.as_ref().is_some_and(|p| p.len() != n) {
            self.initial_params = None;
        }
        self.constraints.iter().all(|c| *c == Constraint::Free)
    }

    /// The most recent iterative-fit result (covariance / chi-square), if the
    /// last successful fit used an iterative peak model.
    pub fn iterative_result(&self) -> Option<&IterativeFitResult> {
        self.iterative_result.as_ref()
    }

    /// Is the window currently open?
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open or close the window.
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    /// Set the data to fit.
    pub fn set_data(&mut self, x: &[f64], y: &[f64]) {
        self.x_data = x.to_vec();
        self.y_data = y.to_vec();

        let curve = CurveData::new(self.x_data.clone(), self.y_data.clone(), Color32::BLUE);
        if let Some(handle) = self.data_handle {
            self.plot.update_curve_data(handle, &curve);
        } else {
            self.data_handle = Some(self.plot.add_curve_with_legend(
                &self.x_data,
                &self.y_data,
                Color32::BLUE,
                "Data",
            ));
        }

        // Clear previous fit
        if let Some(handle) = self.fit_handle {
            self.plot.remove(handle);
            self.fit_handle = None;
        }
        self.fit_result = None;
        self.iterative_result = None;
        self.initial_params = None;
        self.plot.reset_zoom_to_data();
    }

    /// Restrict the data to the configured fit range, if any, keeping only
    /// finite samples. Returns owned `(xs, ys)` of the fit-ready points (silx
    /// `FitWidget` xmin/xmax plus FitManager's `_finite_mask`).
    fn ranged_data(&self) -> (Vec<f64>, Vec<f64>) {
        fit_ready_data(&self.x_data, &self.y_data, self.fit_range)
    }

    /// Perform the fit using the currently selected [`FitModelChoice`].
    ///
    /// Iterative peak models are refined with Levenberg-Marquardt and populate
    /// the results table (per-parameter error + reduced chi-square); the
    /// analytical Linear / Gaussian-estimate choices keep their original
    /// behaviour. Honors the configured fit range.
    pub fn perform_fit_choice(&mut self) {
        if self.x_data.is_empty() || self.y_data.is_empty() {
            return;
        }
        let (xs, ys) = self.ranged_data();
        // The fit curve is drawn over the in-range points so the displayed fit
        // matches what was fitted.
        let result: Option<FitResult> = match self.selected_choice {
            FitModelChoice::Linear => {
                self.iterative_result = None;
                LinearFit.fit(&xs, &ys)
            }
            FitModelChoice::GaussianEstimate => {
                self.iterative_result = None;
                GaussianEstimateFit.fit(&xs, &ys)
            }
            FitModelChoice::MultiGaussian => {
                // Auto peak-search multi-Gaussian (silx `sum_gauss` theory):
                // seed the search width from the data (`guess_fwhm`) and fit all
                // located peaks simultaneously with the FitManager default
                // sensitivity (DEFAULT_CONFIG["Sensitivity"] = 2.5,
                // fittheories.py:107, passed as search_sens at :338/:356). The
                // background combo does not apply — the multi-gaussian model
                // carries no per-peak constant and silx's `StripBackgroundFlag`
                // is off by default.
                match fit_multi_gaussian_full(
                    &xs,
                    &ys,
                    guess_fwhm(&ys),
                    DEFAULT_FIT_SENSITIVITY,
                    DEFAULT_MAX_ITER,
                    DEFAULT_DELTACHI,
                ) {
                    Some(ir) => {
                        let fit = ir.fit.clone();
                        self.iterative_result = Some(ir);
                        Some(fit)
                    }
                    None => {
                        self.iterative_result = None;
                        None
                    }
                }
            }
            choice => {
                // One of the iterative peak models.
                let peak_model = choice
                    .peak_model()
                    .expect("non-analytical choice has a peak model");
                match self.background {
                    // No background: start from edited initial parameters and/or
                    // apply per-parameter constraints when set, else the original
                    // unconstrained estimate→fit path (byte-identical).
                    Background::None => {
                        let all_free = self.ensure_constraints_len(peak_model.param_names().len());
                        let fitted = match (&self.initial_params, all_free) {
                            // Default: no edited start, no constraints.
                            (None, true) => IterativeFit::new(peak_model).fit_full(&xs, &ys),
                            // Edited initial parameters → start the fit from them.
                            (Some(p0), _) => fit_peak_from(
                                peak_model,
                                &xs,
                                &ys,
                                p0,
                                &self.constraints,
                                DEFAULT_MAX_ITER,
                                DEFAULT_DELTACHI,
                            ),
                            // Constraints only → estimate then constrained fit.
                            (None, false) => fit_peak_constrained(
                                peak_model,
                                &xs,
                                &ys,
                                &self.constraints,
                                DEFAULT_MAX_ITER,
                                DEFAULT_DELTACHI,
                            ),
                        };
                        match fitted {
                            Some(ir) => {
                                let fit = ir.fit.clone();
                                // Populate the editable value column with the
                                // fitted parameters (silx: the table shows the
                                // last fit; a re-fit starts from these).
                                self.initial_params = Some(fit.parameters.clone());
                                self.iterative_result = Some(ir);
                                Some(fit)
                            }
                            None => {
                                self.iterative_result = None;
                                None
                            }
                        }
                    }
                    // Background theory selected: fit the peak on the
                    // background-subtracted residual and draw the reconstructed
                    // total curve, keeping the peak's solver diagnostics for the
                    // results table (silx background-then-peak workflow).
                    bg => match fit_peak_with_background(
                        peak_model,
                        bg,
                        &xs,
                        &ys,
                        DEFAULT_MAX_ITER,
                        DEFAULT_DELTACHI,
                    ) {
                        Some(bp) => {
                            let mut fit = bp.peak.fit.clone();
                            fit.y_fit = bp.total;
                            self.iterative_result = Some(bp.peak);
                            Some(fit)
                        }
                        None => {
                            self.iterative_result = None;
                            None
                        }
                    },
                }
            }
        };

        match result {
            Some(result) => {
                let curve = CurveData::new(xs.clone(), result.y_fit.clone(), Color32::RED);
                if let Some(handle) = self.fit_handle {
                    self.plot.update_curve_data(handle, &curve);
                } else {
                    self.fit_handle = Some(self.plot.add_curve_with_legend(
                        &xs,
                        &result.y_fit,
                        Color32::RED,
                        "Fit",
                    ));
                }
                self.fit_result = Some(result);
            }
            None => {
                self.fit_result = None;
                self.iterative_result = None;
                if let Some(handle) = self.fit_handle {
                    self.plot.remove(handle);
                    self.fit_handle = None;
                }
            }
        }
    }

    /// Perform the fit using the currently selected function.
    pub fn perform_fit(&mut self) {
        if self.x_data.is_empty() || self.y_data.is_empty() {
            return;
        }

        let functions: [&dyn FitFunction; 2] = [&LinearFit, &GaussianEstimateFit];
        let func = functions[self.selected_function_idx];

        // Fit and draw over the finite samples only (silx `_finite_mask`;
        // this entry point has no range control, so `range` is None).
        let (xs, ys) = fit_ready_data(&self.x_data, &self.y_data, None);
        if let Some(result) = func.fit(&xs, &ys) {
            let curve = CurveData::new(xs.clone(), result.y_fit.clone(), Color32::RED);
            if let Some(handle) = self.fit_handle {
                self.plot.update_curve_data(handle, &curve);
            } else {
                self.fit_handle =
                    Some(
                        self.plot
                            .add_curve_with_legend(&xs, &result.y_fit, Color32::RED, "Fit"),
                    );
            }
            self.fit_result = Some(result);
        } else {
            // Fit failed
            self.fit_result = None;
            if let Some(handle) = self.fit_handle {
                self.plot.remove(handle);
                self.fit_handle = None;
            }
        }
    }

    /// Show the fit widget using the given egui context.
    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let pos = self.win.position(ctx);
        let id = self.win.id();
        let size = self.win.size();
        let signals =
            crate::widget::detached::show_detached(ctx, id, "Fit Widget", size, pos, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Fit Function:");
                    egui::ComboBox::from_id_salt("fit_function_combo")
                        .selected_text(self.selected_choice.label())
                        .show_ui(ui, |ui| {
                            for choice in FitModelChoice::ALL {
                                ui.selectable_value(
                                    &mut self.selected_choice,
                                    choice,
                                    choice.label(),
                                );
                            }
                        });

                    if ui.button("Fit").clicked() {
                        self.perform_fit_choice();
                    }
                });

                // Background theory (silx `FitWidget` background combo). Applies
                // to the iterative peak models; the analytical Linear /
                // Gaussian-estimate choices ignore it.
                ui.horizontal(|ui| {
                    ui.label("Background:");
                    egui::ComboBox::from_id_salt("fit_background_combo")
                        .selected_text(background_label(self.background))
                        .show_ui(ui, |ui| {
                            for (bg, label) in BACKGROUND_CHOICES {
                                ui.selectable_value(&mut self.background, bg, label);
                            }
                        });
                });

                // Fit-range selection (silx `FitWidget` xmin/xmax): the checkbox
                // toggles whole-curve vs a restricted `[xmin, xmax]` window, and
                // the two DragValues edit the bounds (consumed by
                // `in_range_points` on the next fit). Enabling defaults the window
                // to the data's x extent.
                ui.horizontal(|ui| {
                    let mut limited = self.fit_range.is_some();
                    if ui
                        .checkbox(&mut limited, "Fit range")
                        .on_hover_text("Restrict the fit to an x window (silx xmin/xmax)")
                        .changed()
                    {
                        self.fit_range = limited.then(|| self.default_fit_range());
                    }
                    if let Some((xmin, xmax)) = self.fit_range.as_mut() {
                        ui.label("min");
                        ui.add(egui::DragValue::new(xmin).speed(0.1));
                        ui.label("max");
                        ui.add(egui::DragValue::new(xmax).speed(0.1));
                    }
                });

                // Per-parameter table (silx `FitWidget`), shown for the
                // single-peak iterative models: an editable initial-value column
                // (populated after the first fit; the next fit starts from it)
                // plus a constraint cell — a code combo (`UI_CONSTRAINT_KINDS`)
                // and the payload editor for the selected code (QUOTED min/max,
                // FACTOR/DELTA/SUM reference picker + value), all driving
                // `core::fitting::leastsq_constrained` (silx `Parameters`
                // Constraints column + cons1/cons2).
                if let Some(peak_model) = self.selected_choice.peak_model() {
                    let names = peak_model.param_names();
                    self.ensure_constraints_len(names.len());
                    ui.collapsing("Parameters", |ui| {
                        egui::Grid::new("fit_params_input_grid")
                            .num_columns(3)
                            .show(ui, |ui| {
                                ui.label("Parameter");
                                ui.label("Initial");
                                ui.label("Constraint");
                                ui.end_row();
                                for (i, name) in names.iter().enumerate() {
                                    ui.label(name);
                                    match self.initial_params.as_mut() {
                                        Some(p0) => {
                                            ui.add(egui::DragValue::new(&mut p0[i]).speed(0.1));
                                        }
                                        None => {
                                            ui.label("—");
                                        }
                                    }
                                    ui.horizontal(|ui| {
                                        let current = constraint_kind(self.constraints[i]);
                                        let mut kind = current;
                                        egui::ComboBox::from_id_salt(("fit_constraint_combo", i))
                                            .selected_text(constraint_kind_label(kind))
                                            .show_ui(ui, |ui| {
                                                for choice in UI_CONSTRAINT_KINDS {
                                                    ui.selectable_value(
                                                        &mut kind,
                                                        choice,
                                                        constraint_kind_label(choice),
                                                    );
                                                }
                                            });
                                        if kind != current {
                                            // silx rejects FACTOR/DELTA/SUM when no
                                            // related parameter exists; leave it.
                                            if let Some(c) =
                                                make_constraint(kind, i, &self.constraints)
                                            {
                                                self.constraints[i] = c;
                                            }
                                        }
                                        // Reference-picker candidates (snapshot
                                        // before the &mut borrow below).
                                        let tieable: Vec<bool> = self
                                            .constraints
                                            .iter()
                                            .enumerate()
                                            .map(|(j, c)| j != i && !is_tied(*c))
                                            .collect();
                                        match &mut self.constraints[i] {
                                            Constraint::Quoted { min, max } => {
                                                ui.label("min");
                                                ui.add(egui::DragValue::new(min).speed(0.1));
                                                ui.label("max");
                                                ui.add(egui::DragValue::new(max).speed(0.1));
                                            }
                                            Constraint::Factor { reference, factor } => {
                                                reference_param_combo(
                                                    ui, i, reference, &names, &tieable,
                                                );
                                                ui.label("×");
                                                ui.add(egui::DragValue::new(factor).speed(0.1));
                                            }
                                            Constraint::Delta { reference, delta } => {
                                                reference_param_combo(
                                                    ui, i, reference, &names, &tieable,
                                                );
                                                ui.label("+");
                                                ui.add(egui::DragValue::new(delta).speed(0.1));
                                            }
                                            Constraint::Sum { reference, sum } => {
                                                reference_param_combo(
                                                    ui, i, reference, &names, &tieable,
                                                );
                                                ui.label("Σ−");
                                                ui.add(egui::DragValue::new(sum).speed(0.1));
                                            }
                                            _ => {}
                                        }
                                    });
                                    ui.end_row();
                                }
                            });
                    });
                }

                ui.separator();

                // Show fit parameters if available. Iterative fits add a per
                // parameter estimated error column and a reduced chi-square row
                // (silx FitWidget results table). The column shows the
                // constraint-propagated uncertainties — silx displays
                // `infodict["uncertainties"]` (fitmanager.py:904-909), not the
                // raw covariance-diagonal errors.
                if let Some(result) = &self.fit_result {
                    let errors: Option<Vec<f64>> = self
                        .iterative_result
                        .as_ref()
                        .map(|ir| ir.uncertainties().to_vec());
                    ui.group(|ui| {
                        ui.heading("Fit Parameters");
                        egui::Grid::new("fit_params_grid")
                            .num_columns(3)
                            .show(ui, |ui| {
                                ui.label("Parameter");
                                ui.label("Value");
                                ui.label("Error");
                                ui.end_row();
                                for (i, (name, val)) in result
                                    .param_names
                                    .iter()
                                    .zip(result.parameters.iter())
                                    .enumerate()
                                {
                                    ui.label(name);
                                    ui.label(format!("{val:.6}"));
                                    match errors.as_ref().and_then(|e| e.get(i)) {
                                        Some(&err) if err.is_finite() => {
                                            ui.label(format!("{err:.6}"));
                                        }
                                        _ => {
                                            ui.label("");
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                        if let Some(ir) = &self.iterative_result {
                            ui.separator();
                            ui.horizontal(|ui| {
                                ui.label("Reduced chi-square:");
                                ui.label(format_reduced_chisq(ir.reduced_chisq()));
                            });
                        }
                    });
                    ui.separator();
                }

                // Show the plot
                self.plot.show(ui);
            });
        self.win.apply_signals(&signals, &mut self.open);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::fitting::{IterativeFit, LeastSqResult, PeakModel};

    #[test]
    fn format_value_error_with_finite_error() {
        assert_eq!(
            format_param_value_error(1.234_567_8, 0.012_345_6),
            "1.234568 ± 0.012346"
        );
    }

    #[test]
    fn format_value_error_with_nonfinite_error_drops_pm() {
        let s = format_param_value_error(2.5, f64::NAN);
        assert_eq!(s, "2.500000");
        assert!(!s.contains('±'));
    }

    #[test]
    fn format_reduced_chisq_some_and_none() {
        assert_eq!(format_reduced_chisq(Some(0.5)), "0.500000");
        assert_eq!(format_reduced_chisq(None), "N/A");
        assert_eq!(format_reduced_chisq(Some(f64::INFINITY)), "N/A");
    }

    #[test]
    fn default_fit_range_uses_finite_x_extent() {
        // The seed window is the finite min/max of the data, skipping NaN/inf.
        assert_eq!(
            default_fit_range_of(&[3.0, 1.0, f64::NAN, 5.0, f64::INFINITY]),
            (1.0, 5.0)
        );
        // No finite sample -> the (0, 1) fallback.
        assert_eq!(default_fit_range_of(&[]), (0.0, 1.0));
        assert_eq!(default_fit_range_of(&[f64::NAN]), (0.0, 1.0));
    }

    #[test]
    fn fit_ready_data_drops_each_non_finite_member() {
        // R2-33: silx fits `data[_finite_mask]` (fitmanager.py:803-808) — a
        // pair is dropped when EITHER member is non-finite, and every finite
        // pair survives.
        let xs = [1.0, 2.0, f64::NAN, 4.0, f64::INFINITY, 6.0];
        let ys = [10.0, f64::NAN, 30.0, 40.0, 50.0, f64::NEG_INFINITY];
        let (fx, fy) = fit_ready_data(&xs, &ys, None);
        assert_eq!(fx, vec![1.0, 4.0]);
        assert_eq!(fy, vec![10.0, 40.0]);
    }

    #[test]
    fn fit_ready_data_all_non_finite_yields_empty() {
        let (fx, fy) = fit_ready_data(&[f64::NAN, f64::INFINITY], &[1.0, 2.0], None);
        assert!(fx.is_empty());
        assert!(fy.is_empty());
    }

    #[test]
    fn fit_ready_data_range_is_inclusive_normalized_and_composes_with_mask() {
        let xs = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let ys = [0.0, 10.0, f64::NAN, 30.0, 40.0, 50.0];
        // Reversed bounds normalize; both endpoints inclusive; the in-range
        // NaN pair (x=2) is still dropped.
        let (fx, fy) = fit_ready_data(&xs, &ys, Some((4.0, 1.0)));
        assert_eq!(fx, vec![1.0, 3.0, 4.0]);
        assert_eq!(fy, vec![10.0, 30.0, 40.0]);
    }

    #[test]
    fn error_extraction_from_covariance_diagonal() {
        // std_errors() is sqrt(diag(covariance)) — the unconstrained sigma.
        let res = LeastSqResult {
            parameters: vec![1.0, 2.0],
            covariance: vec![vec![9.0, 0.0], vec![0.0, 25.0]],
            uncertainties: vec![3.0, 5.0],
            chisq: 0.0,
            reduced_chisq: Some(0.0),
            niter: 1,
            nfev: 1,
        };
        let errs = res.std_errors();
        assert!((errs[0] - 3.0).abs() < 1e-12);
        assert!((errs[1] - 5.0).abs() < 1e-12);
        // And formatting them.
        assert_eq!(
            format_param_value_error(res.parameters[0], errs[0]),
            "1.000000 ± 3.000000"
        );
    }

    #[test]
    fn results_table_errors_use_constraint_propagated_uncertainties() {
        // R2-32: silx's FitWidget sigma column shows infodict["uncertainties"]
        // (fitmanager.py:904-909) — constraint-propagated — not the raw
        // covariance-diagonal std_errors(). The two diverge for any constrained
        // fit (e.g. a FIXED parameter shows its value, a FACTOR tie scales).
        let ir = IterativeFitResult {
            fit: FitResult {
                y_fit: Vec::new(),
                parameters: vec![1.0, 2.0],
                param_names: vec!["a".to_string(), "b".to_string()],
            },
            solver: LeastSqResult {
                parameters: vec![1.0, 2.0],
                covariance: vec![vec![9.0, 0.0], vec![0.0, 25.0]],
                // Diverges from sqrt(diag) = [3, 5]: b is FIXED, so its
                // propagated uncertainty is the parameter value itself.
                uncertainties: vec![3.0, 2.0],
                chisq: 0.0,
                reduced_chisq: Some(0.0),
                niter: 1,
                nfev: 1,
            },
        };
        assert_eq!(ir.uncertainties(), &[3.0, 2.0]);
        assert_ne!(ir.uncertainties(), ir.std_errors().as_slice());
    }

    #[test]
    fn peak_model_mapping_for_iterative_choices() {
        assert_eq!(
            FitModelChoice::IterativeGaussian.peak_model(),
            Some(PeakModel::Gaussian)
        );
        assert_eq!(
            FitModelChoice::IterativeGaussianArea.peak_model(),
            Some(PeakModel::GaussianArea)
        );
        assert_eq!(
            FitModelChoice::IterativeSplitGaussian.peak_model(),
            Some(PeakModel::SplitGaussian)
        );
        assert_eq!(
            FitModelChoice::IterativeLorentzian.peak_model(),
            Some(PeakModel::Lorentzian)
        );
        assert_eq!(
            FitModelChoice::IterativeLorentzianArea.peak_model(),
            Some(PeakModel::LorentzianArea)
        );
        assert_eq!(
            FitModelChoice::IterativeSplitLorentzian.peak_model(),
            Some(PeakModel::SplitLorentzian)
        );
        assert_eq!(
            FitModelChoice::IterativePseudoVoigt.peak_model(),
            Some(PeakModel::PseudoVoigt)
        );
        assert_eq!(
            FitModelChoice::IterativeAreaPseudoVoigt.peak_model(),
            Some(PeakModel::AreaPseudoVoigt)
        );
        assert_eq!(
            FitModelChoice::IterativeSplitPseudoVoigt.peak_model(),
            Some(PeakModel::SplitPseudoVoigt)
        );
        assert_eq!(
            FitModelChoice::IterativeSplitPseudoVoigt2.peak_model(),
            Some(PeakModel::SplitPseudoVoigt2)
        );
        assert_eq!(
            FitModelChoice::IterativeHypermet.peak_model(),
            Some(PeakModel::Hypermet)
        );
        assert_eq!(
            FitModelChoice::IterativePolynomial2.peak_model(),
            Some(PeakModel::Polynomial2)
        );
        assert_eq!(
            FitModelChoice::IterativePolynomial5.peak_model(),
            Some(PeakModel::Polynomial5)
        );
        assert_eq!(FitModelChoice::Linear.peak_model(), None);
        assert_eq!(FitModelChoice::GaussianEstimate.peak_model(), None);
    }

    #[test]
    fn all_choices_listed_once_in_order() {
        assert_eq!(FitModelChoice::ALL.len(), 22);
        assert_eq!(FitModelChoice::ALL[0], FitModelChoice::Linear);
        assert_eq!(FitModelChoice::ALL[8], FitModelChoice::IterativePseudoVoigt);
        assert_eq!(FitModelChoice::ALL[15], FitModelChoice::IterativeAtanStepUp);
        assert_eq!(FitModelChoice::ALL[16], FitModelChoice::IterativeHypermet);
        assert_eq!(
            FitModelChoice::ALL[17],
            FitModelChoice::IterativePolynomial2
        );
        assert_eq!(FitModelChoice::ALL[21], FitModelChoice::MultiGaussian);
        // Only the single-peak iterative choices map to one `PeakModel`; the
        // analytical (Linear / Gaussian-estimate) and composite (multi-peak)
        // choices have none.
        for choice in FitModelChoice::ALL {
            let single_peak = !matches!(
                choice,
                FitModelChoice::Linear
                    | FitModelChoice::GaussianEstimate
                    | FitModelChoice::MultiGaussian
            );
            assert_eq!(choice.peak_model().is_some(), single_peak);
        }
    }

    #[test]
    fn background_choices_match_silx_theory_order() {
        // silx `bgtheories.THEORY` insertion order.
        let labels: Vec<&str> = BACKGROUND_CHOICES.iter().map(|(_, l)| *l).collect();
        assert_eq!(
            labels,
            vec![
                "No Background",
                "Constant",
                "Linear",
                "Strip",
                "Snip",
                "Degree 2 Polynomial",
                "Degree 3 Polynomial",
                "Degree 4 Polynomial",
                "Degree 5 Polynomial",
            ]
        );
        // The first entry is the no-background default.
        assert_eq!(BACKGROUND_CHOICES[0].0, Background::None);
    }

    #[test]
    fn background_label_resolves_choices_and_falls_back() {
        // Each canonical choice round-trips to its silx label.
        for (bg, label) in BACKGROUND_CHOICES {
            assert_eq!(background_label(bg), label);
        }
        // A non-default parameterisation falls back to the generic name.
        let custom = Background::Polynomial { degree: 9 };
        assert_eq!(background_label(custom), custom.name());
    }

    #[test]
    fn constraint_labels_match_silx_code_options() {
        // silx `Parameters.code_options` display strings.
        assert_eq!(constraint_kind_label(ConstraintKind::Free), "FREE");
        assert_eq!(constraint_kind_label(ConstraintKind::Positive), "POSITIVE");
        assert_eq!(constraint_kind_label(ConstraintKind::Quoted), "QUOTED");
        assert_eq!(constraint_kind_label(ConstraintKind::Fixed), "FIXED");
        assert_eq!(constraint_kind_label(ConstraintKind::Factor), "FACTOR");
        assert_eq!(constraint_kind_label(ConstraintKind::Delta), "DELTA");
        assert_eq!(constraint_kind_label(ConstraintKind::Sum), "SUM");
        assert_eq!(constraint_kind_label(ConstraintKind::Ignore), "IGNORE");
        // The combo exposes silx `code_options` minus the group-management
        // `ADD`/`SHOW` and the group-only `IGNORE`.
        assert_eq!(
            UI_CONSTRAINT_KINDS,
            [
                ConstraintKind::Free,
                ConstraintKind::Positive,
                ConstraintKind::Quoted,
                ConstraintKind::Fixed,
                ConstraintKind::Factor,
                ConstraintKind::Delta,
                ConstraintKind::Sum,
            ]
        );
    }

    #[test]
    fn constraint_kind_drops_payload() {
        assert_eq!(
            constraint_kind(Constraint::Quoted { min: 2.0, max: 9.0 }),
            ConstraintKind::Quoted
        );
        assert_eq!(
            constraint_kind(Constraint::Factor {
                reference: 3,
                factor: 0.5
            }),
            ConstraintKind::Factor
        );
        assert_eq!(constraint_kind(Constraint::Ignored), ConstraintKind::Ignore);
    }

    #[test]
    fn make_constraint_seeds_silx_defaults_for_payload_codes() {
        // FREE/POSITIVE/FIXED need no related parameter and no reference.
        let solo = [Constraint::Free];
        assert_eq!(
            make_constraint(ConstraintKind::Positive, 0, &solo),
            Some(Constraint::Positive)
        );
        assert_eq!(
            make_constraint(ConstraintKind::Fixed, 0, &solo),
            Some(Constraint::Fixed)
        );
        // QUOTED seeds the [0, 1] placeholder interval.
        assert_eq!(
            make_constraint(ConstraintKind::Quoted, 0, &solo),
            Some(Constraint::Quoted { min: 0.0, max: 1.0 })
        );
        // FACTOR/DELTA/SUM tie to the first other free parameter, seeded 1/0/0.
        let three = [Constraint::Free, Constraint::Free, Constraint::Free];
        assert_eq!(
            make_constraint(ConstraintKind::Factor, 1, &three),
            Some(Constraint::Factor {
                reference: 0,
                factor: 1.0
            })
        );
        assert_eq!(
            make_constraint(ConstraintKind::Delta, 0, &three),
            Some(Constraint::Delta {
                reference: 1,
                delta: 0.0
            })
        );
        assert_eq!(
            make_constraint(ConstraintKind::Sum, 0, &three),
            Some(Constraint::Sum {
                reference: 1,
                sum: 0.0
            })
        );
    }

    #[test]
    fn make_constraint_rejects_tie_with_no_candidate() {
        // A single parameter has no other to tie to (silx returns False).
        let solo = [Constraint::Free];
        assert_eq!(make_constraint(ConstraintKind::Factor, 0, &solo), None);
        assert_eq!(make_constraint(ConstraintKind::Delta, 0, &solo), None);
        assert_eq!(make_constraint(ConstraintKind::Sum, 0, &solo), None);
    }

    #[test]
    fn related_reference_skips_self_and_tied_parameters() {
        // param 0: only candidate is param 2 (param 1 is itself a tie, excluded).
        let constraints = [
            Constraint::Free,
            Constraint::Factor {
                reference: 2,
                factor: 1.0,
            },
            Constraint::Positive,
        ];
        assert_eq!(default_related_reference(0, &constraints), Some(2));
        // No untied other parameter -> None (matches make_constraint rejection).
        let all_tied = [
            Constraint::Free,
            Constraint::Ignored,
            Constraint::Sum {
                reference: 0,
                sum: 1.0,
            },
        ];
        assert_eq!(default_related_reference(1, &all_tied), Some(0));
        assert_eq!(default_related_reference(0, &[Constraint::Free]), None);
    }

    #[test]
    fn iterative_fit_result_table_has_one_error_per_param() {
        // A clean gaussian; the per-parameter error vector must line up with
        // the parameter vector so the results table renders one error per row.
        let xs: Vec<f64> = (0..201).map(|i| i as f64 * 0.1).collect();
        let ys = crate::core::fitting::gaussian_model(&xs, &[5.0, 10.0, 2.0, 0.5]);
        let ir = IterativeFit::new(PeakModel::Gaussian)
            .fit_full(&xs, &ys)
            .unwrap();
        assert_eq!(ir.fit.parameters.len(), ir.std_errors().len());
        assert_eq!(ir.fit.param_names.len(), ir.fit.parameters.len());
    }
}
