//! Shared "nice numbers" tick layout — a single port of silx
//! `silx.gui.plot._utils.ticklayout` (`numberOfDigits`, `niceNumGeneric`,
//! `niceNumbers`).
//!
//! Consolidates the copies previously carried privately by [`crate::widget`]
//! `chrome`/`colorbar` and [`crate::core`] `scene3d::axes`/`dtime_ticks`
//! (R2-38 structural follow-up: the duplication is what let the linear layout
//! drift out of parity). Each caller keeps its own *tick generation* (the
//! chrome tolerance loop, the axes `_frange` accumulation, the colorbar
//! `tick_layout`, the datetime element logic) but shares this exact-silx
//! nice-number core.

/// silx `ticklayout.numberOfDigits` (`ticklayout.py:36-46`): fractional digits
/// for a tick spacing, `max(0, -floor(log10(spacing)))`. Uses the dedicated
/// `log10` (silx `math.log10`), unlike [`nice_num_generic`] which uses the
/// generic log base.
pub fn number_of_digits(tick_spacing: f64) -> usize {
    let nfrac = -(tick_spacing.log10().floor());
    if nfrac < 0.0 { 0 } else { nfrac as usize }
}

/// silx `ticklayout.niceNumGeneric` (`ticklayout.py:78-108`): round `value` to a
/// nice multiple of a power of the last fraction.
///
/// `nice_fractions` mirrors silx's `niceFractions` argument and reproduces its
/// branch on `niceFractions is None`:
/// - `None` uses the default list `[1, 2, 5, 10]` with the **hardcoded** round
///   table `(1.5, 3.0, 7.0, 10.0)` (silx keeps this for backward-compat with the
///   original `_niceNum`, *not* the averaged table).
/// - `Some(list)` uses the caller's fractions with
///   `roundFractions[i] = (list[i] + list[i+1]) / 2` (the last stays), as silx
///   does for a non-default list (e.g. the datetime per-unit step lists).
///
/// The log base is the last fraction (`math.log(value, highest)` =
/// `ln(value)/ln(highest)`), so a custom `highest` (24 hours, 60 minutes, …)
/// works. `value == 0` returns `0` like silx.
pub fn nice_num_generic(value: f64, nice_fractions: Option<&[f64]>, is_round: bool) -> f64 {
    if value == 0.0 {
        return value;
    }
    match nice_fractions {
        None => {
            const NICE: [f64; 4] = [1.0, 2.0, 5.0, 10.0];
            let round: [f64; 4] = if is_round {
                [1.5, 3.0, 7.0, 10.0]
            } else {
                NICE
            };
            nice_num_inner(value, &NICE, &round)
        }
        Some(nice) => {
            let mut round = nice.to_vec();
            if is_round {
                // Average with the next element; the last remains the same.
                for i in 0..round.len().saturating_sub(1) {
                    round[i] = (nice[i] + nice[i + 1]) / 2.0;
                }
            }
            nice_num_inner(value, nice, &round)
        }
    }
}

/// Core of [`nice_num_generic`] once the round table is resolved: pick the first
/// nice fraction whose round threshold `value / highest^expvalue` does not
/// exceed. `nice` must be non-empty; its last element is the log base.
fn nice_num_inner(value: f64, nice: &[f64], round: &[f64]) -> f64 {
    let highest = *nice.last().expect("nice fractions non-empty");
    let expvalue = (value.ln() / highest.ln()).floor();
    let frac = value / highest.powf(expvalue);
    for (&nice_frac, &round_frac) in nice.iter().zip(round.iter()) {
        if frac <= round_frac {
            return nice_frac * highest.powf(expvalue);
        }
    }
    // silx asserts unreachable; frac <= highest always matches the last.
    highest * highest.powf(expvalue)
}

/// silx `ticklayout.niceNumGeneric` for the default `[1, 2, 5, 10]` fractions —
/// the only path used by the linear [`nice_numbers`] layout.
pub fn nice_num(value: f64, is_round: bool) -> f64 {
    nice_num_generic(value, None, is_round)
}

/// silx `ticklayout.niceNumbers` (`ticklayout.py:112-132`, Heckbert's nice
/// numbers): `(graph_min, graph_max, spacing, nfrac)` for a `[vmin, vmax]`
/// linear axis targeting `n_ticks` ticks. The span is divided by `n_ticks`
/// (silx `vrange / nTicks`), not `n_ticks − 1`.
pub fn nice_numbers(vmin: f64, vmax: f64, n_ticks: usize) -> (f64, f64, f64, usize) {
    let vrange = nice_num(vmax - vmin, false);
    let spacing = nice_num(vrange / n_ticks as f64, true);
    let graph_min = (vmin / spacing).floor() * spacing;
    let graph_max = (vmax / spacing).ceil() * spacing;
    let nfrac = number_of_digits(spacing);
    (graph_min, graph_max, spacing, nfrac)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_num_zero_is_zero() {
        // silx `if value == 0: return value`.
        assert_eq!(nice_num(0.0, false), 0.0);
        assert_eq!(nice_num(0.0, true), 0.0);
        assert_eq!(
            nice_num_generic(0.0, Some(&[1.0, 2.0, 3.0, 12.0]), true),
            0.0
        );
    }

    #[test]
    fn nice_num_default_round_thresholds_are_inclusive() {
        // Default fractions: hardcoded round table (1.5, 3.0, 7.0, 10.0) with
        // `frac <= roundFrac`, so a frac exactly on a boundary rounds down.
        assert_eq!(nice_num(1.5, true), 1.0); // 1.5 <= 1.5 -> 1
        assert_eq!(nice_num(3.0, true), 2.0); // 3.0 <= 3.0 -> 2
        assert_eq!(nice_num(7.0, true), 5.0); // 7.0 <= 7.0 -> 5
        assert_eq!(nice_num(8.0, true), 10.0); // > 7 -> 10
    }

    #[test]
    fn nice_num_default_floor_thresholds() {
        // is_round = false uses the nice fractions themselves as thresholds.
        assert_eq!(nice_num(1.0, false), 1.0);
        assert_eq!(nice_num(2.0, false), 2.0);
        assert_eq!(nice_num(5.0, false), 5.0);
        assert_eq!(nice_num(6.0, false), 10.0);
    }

    #[test]
    fn nice_num_custom_fractions_average_the_round_table() {
        // Non-default list: roundFractions[i] = (nice[i] + nice[i+1]) / 2.
        // For [1, 2, 5, 10] passed EXPLICITLY, the averaged table is
        // (1.5, 3.5, 7.5, 10.0) — differs from the hardcoded default at frac 3.2.
        let custom = [1.0, 2.0, 5.0, 10.0];
        assert_eq!(nice_num_generic(3.2, Some(&custom), true), 2.0); // 3.2 <= 3.5
        // The default (None) path rounds the same frac to 5 (3.2 > 3.0).
        assert_eq!(nice_num(3.2, true), 5.0);
    }

    #[test]
    fn nice_num_custom_base_from_last_fraction() {
        // Datetime hours use [1, 2, 3, 4, 6, 12] with base 12: a value of 6
        // hours is exactly the 6-fraction at expvalue 0.
        let hours = [1.0, 2.0, 3.0, 4.0, 6.0, 12.0];
        assert_eq!(nice_num_generic(6.0, Some(&hours), false), 6.0);
        assert_eq!(nice_num_generic(5.0, Some(&hours), false), 6.0); // 5 -> next nice up
    }

    #[test]
    fn number_of_digits_matches_silx() {
        assert_eq!(number_of_digits(1.0), 0); // -floor(0) = 0
        assert_eq!(number_of_digits(0.1), 1); // -floor(-1) = 1
        assert_eq!(number_of_digits(0.01), 2);
        assert_eq!(number_of_digits(0.5), 1); // -floor(-0.30) = 1
        assert_eq!(number_of_digits(10.0), 0); // -floor(1) = -1 -> 0
    }

    #[test]
    fn nice_numbers_simple_decade() {
        // [0, 10] with 5 ticks -> spacing 2 (silx vrange/nTicks = 10/5 = 2).
        let (gmin, gmax, spacing, nfrac) = nice_numbers(0.0, 10.0, 5);
        assert_eq!(gmin, 0.0);
        assert_eq!(gmax, 10.0);
        assert_eq!(spacing, 2.0);
        assert_eq!(nfrac, 0);
    }

    #[test]
    fn nice_numbers_divides_by_n_ticks_not_n_minus_one() {
        // [0, 100] / 5 ticks -> step 20 (silx), NOT nice_num(100/4)=25.
        let (_gmin, _gmax, spacing, _nfrac) = nice_numbers(0.0, 100.0, 5);
        assert_eq!(spacing, 20.0);
    }

    #[test]
    fn nice_numbers_fractional_spacing_sets_nfrac() {
        // [0, 1] / 5 ticks -> spacing 0.2, one fractional digit.
        let (gmin, gmax, spacing, nfrac) = nice_numbers(0.0, 1.0, 5);
        assert_eq!(gmin, 0.0);
        assert_eq!(gmax, 1.0);
        assert!((spacing - 0.2).abs() < 1e-12);
        assert_eq!(nfrac, 1);
    }
}
