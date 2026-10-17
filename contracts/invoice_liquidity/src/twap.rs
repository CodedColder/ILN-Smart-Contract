//! TWAP accumulator ported from `contracts/examples/twap_oracle` (Issue #815).
//!
//! Production `invoice_liquidity` previously had TWAP support only as a
//! standalone example crate consulted by nothing. This module extracts the
//! cumulative-price-accumulator and windowed-average logic into a reusable,
//! opt-in library: pure functions over [`TwapSample`] slices with no storage
//! of their own. Callers (see `oracle_registry`'s per-feed opt-in) own sample
//! persistence and decide when to consult the TWAP path vs raw spot.
//!
//! The integration math mirrors the example crate exactly (trapezoidal rule:
//! each consecutive sample pair contributes
//! `(p1 + p2) / 2 * overlap_duration`), so the example's tests port over
//! directly. Default oracle behavior is unchanged — nothing here runs unless
//! a caller explicitly opts in.

use soroban_sdk::{contracttype, Vec};

/// A single price observation, mirroring the example crate's `PriceSample`
/// minus its `token` field (callers key samples per feed + token themselves).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TwapSample {
    /// Ledger timestamp (seconds) when the sample was recorded.
    pub timestamp: u64,
    /// Observed price (same units as the feed's spot price).
    pub price: i128,
}

/// Maximum samples retained per feed + token window. Bounds on-chain
/// storage while keeping enough points for the documented TWAP windows
/// (see `oracle_registry::{MIN,MAX}_TWAP_WINDOW_LEDGERS`).
pub const MAX_TWAP_SAMPLES: u32 = 100;

/// Compute the time-weighted average price over `[window_start,
/// current_time]` via trapezoidal integration over consecutive sample pairs.
///
/// Returns `None` when no sample pair overlaps the window (empty input,
/// single sample, or all samples outside the window) so callers can fall
/// back to spot. Never panics on empty input — the example crate returned
/// `0` / latest-price in those cases, which would silently masquerade as a
/// real price here.
pub fn twap_average(
    samples: &Vec<TwapSample>,
    window_start: u64,
    current_time: u64,
) -> Option<i128> {
    let n = samples.len();
    if n < 2 || current_time <= window_start {
        return None;
    }
    let mut total_weighted: i128 = 0;
    let mut total_weight: u64 = 0;
    for i in 0..(n - 1) {
        let s1 = samples.get(i).unwrap();
        let s2 = samples.get(i + 1).unwrap();
        // Skip pairs fully outside the window.
        if s2.timestamp < window_start || s1.timestamp > current_time {
            continue;
        }
        let seg_start = s1.timestamp.max(window_start);
        let seg_end = s2.timestamp.min(current_time);
        if seg_end > seg_start {
            let duration = seg_end - seg_start;
            let avg_price = (s1.price + s2.price) / 2;
            total_weighted =
                total_weighted.saturating_add(avg_price.saturating_mul(duration as i128));
            total_weight = total_weight.saturating_add(duration);
        }
    }
    if total_weight > 0 {
        Some(total_weighted / (total_weight as i128))
    } else {
        None
    }
}

/// Append `sample` to a chronological sample buffer, evicting the oldest
/// entry once `max_samples` is exceeded. Samples are assumed pushed in
/// non-decreasing timestamp order (same assumption as the example crate's
/// circular buffer, which never re-sorts).
pub fn push_sample(samples: &mut Vec<TwapSample>, sample: TwapSample, max_samples: u32) {
    let cap = if max_samples == 0 {
        MAX_TWAP_SAMPLES
    } else {
        max_samples
    };
    samples.push_back(sample);
    while samples.len() > cap {
        samples.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::Env;

    fn sample(timestamp: u64, price: i128) -> TwapSample {
        TwapSample { timestamp, price }
    }

    fn vec_of(env: &Env, points: &[(u64, i128)]) -> Vec<TwapSample> {
        let mut v = Vec::new(env);
        for (t, p) in points {
            v.push_back(sample(*t, *p));
        }
        v
    }

    // Ported from the example crate's `test_twap_calculation`: $20.00 at
    // t=0, $21.00 at t=1800, 30-minute window at t=1800 -> $20,500 average.
    #[test]
    fn test_twap_two_sample_average() {
        let env = Env::default();
        let samples = vec_of(&env, &[(0, 20_000), (1800, 21_000)]);
        assert_eq!(twap_average(&samples, 0, 1800), Some(20_500));
    }

    // Ported intent of the example's `test_window_validation`: degenerate
    // windows never produce a TWAP (callers validate bounds separately via
    // `oracle_registry::set_twap_window`).
    #[test]
    fn test_twap_degenerate_window_returns_none() {
        let env = Env::default();
        let samples = vec_of(&env, &[(0, 20_000), (1800, 21_000)]);
        assert_eq!(twap_average(&samples, 1800, 1800), None);
        assert_eq!(twap_average(&samples, 2000, 1800), None);
    }

    #[test]
    fn test_twap_empty_and_single_sample_return_none() {
        let env = Env::default();
        let empty = Vec::new(&env);
        assert_eq!(twap_average(&empty, 0, 3600), None);
        let single = vec_of(&env, &[(100, 20_000)]);
        assert_eq!(twap_average(&single, 0, 3600), None);
    }

    #[test]
    fn test_twap_ignores_samples_outside_window() {
        let env = Env::default();
        let samples = vec_of(&env, &[(0, 10_000), (100, 10_000)]);
        // Window [1000, 2000] has no overlapping pair.
        assert_eq!(twap_average(&samples, 1000, 2000), None);
    }

    #[test]
    fn test_twap_partial_overlap_clips_to_window() {
        let env = Env::default();
        // Constant $10.00 price; window clips the pair to [500, 1500].
        let samples = vec_of(&env, &[(0, 10_000), (2000, 10_000)]);
        assert_eq!(twap_average(&samples, 500, 1500), Some(10_000));
    }

    #[test]
    fn test_twap_dilutes_single_block_spike() {
        let env = Env::default();
        // Honest $10.00 for an hour, one manipulated $20.00 sample for 5s,
        // then back to $10.00: the TWAP stays near $10.00 while spot would
        // report $20.00 during the spike.
        let samples = vec_of(
            &env,
            &[(0, 10_000), (3600, 10_000), (3605, 20_000), (3610, 10_000)],
        );
        let avg = twap_average(&samples, 0, 3610).unwrap();
        assert!(
            avg < 11_000,
            "single-block spike must be diluted by the window, got {avg}"
        );
        assert!(avg >= 10_000);
    }

    #[test]
    fn test_push_sample_evicts_oldest_past_cap() {
        let env = Env::default();
        let mut v = Vec::new(&env);
        push_sample(&mut v, sample(1, 100), 2);
        push_sample(&mut v, sample(2, 200), 2);
        push_sample(&mut v, sample(3, 300), 2);
        assert_eq!(v.len(), 2);
        assert_eq!(v.get(0).unwrap(), sample(2, 200));
        assert_eq!(v.get(1).unwrap(), sample(3, 300));
    }
}
