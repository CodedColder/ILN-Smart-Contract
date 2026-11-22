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

// ----------------------------------------------------------------
// TWAP RECORDING & WINDOW-ROLLOVER INVARIANTS (Issue #1192)
//
// These declare the verified behavior of the sample buffer and the
// windowed average in this module. The rationale, entry-point map,
// verified findings and residual-risk notes live in
// `docs/twap-recording-rollover.md`.
//
// INV-TWAP1 (chronological append assumption): `push_sample` appends in
//   call order and never re-sorts. `twap_average` consumes samples in
//   storage order and assumes non-decreasing timestamps; an out-of-order
//   pair produces `seg_end < seg_start` and is skipped, so it
//   contributes *no* weight (silently ignored, not mis-ordered).
// INV-TWAP2 (bounded, count-based retention): the buffer keeps at most
//   `max_samples` entries (`MAX_TWAP_SAMPLES` = 100 when `max_samples`
//   is 0), evicting the oldest-inserted first. Retention is by COUNT,
//   never by sample age — a sample older than the read window stays in
//   storage until the cap forces it out.
// INV-TWAP3 (window clipping): only pairs overlapping
//   `[window_start, current_time]` contribute. A pair fully outside is
//   skipped; a partially overlapping pair is clipped to the window, so a
//   boundary sample older than `window_start` is carried forward and its
//   price participates at the window's left edge.
// INV-TWAP4 (no-opinion result): `twap_average` returns `None` when no
//   pair overlaps the window — empty input, a single sample, all samples
//   outside the window, or a degenerate window (`current_time <=
//   window_start`). Callers fall back to spot on `None`.
// INV-TWAP5 (saturating accumulation): every weight/accumulation update
//   (`total_weighted`, `total_weight`, `avg_price * duration`) saturates
//   at the `i128`/`u64` bound instead of wrapping. The ONE
//   non-saturating operation is the pairwise sum `s1.price + s2.price`,
//   whose operands the caller does not bound above (see RR-1).
// INV-TWAP6 (integer truncation): `(p1 + p2) / 2` and the final
//   `total_weighted / total_weight` truncate toward zero; nothing rounds
//   up.
// INV-TWAP7 (pure, stateless): both functions are pure over their
//   arguments — no storage, no authorization, no cross-contract calls.
//   All access control and persistence live in the caller
//   (`oracle_registry::{record_twap_sample, get_twap_price}`).
//
// Covered by the tests in this module. Residual risks (no price ceiling
// feeding the pair sum, count-not-age retention, global not-per-feed
// window) are enumerated in the doc, not pinned here.
// ----------------------------------------------------------------

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

    // ── Issue #1192: TWAP recording & window rollover ──────────────
    //
    // These pin the verified sample-ordering, duplicate-ledger, stale,
    // overflow and rollover behavior declared in the INV-TWAP* block at
    // the top of this module. They record what the code does today; they
    // do not assert the behavior is ideal (see RR-1..RR-4 in
    // docs/twap-recording-rollover.md).

    /// INV-TWAP1: `push_sample` appends in call order and never re-sorts,
    /// so an out-of-order timestamp stays exactly where it was pushed.
    #[test]
    fn test_push_sample_preserves_insertion_order_not_sorted() {
        let env = Env::default();
        let mut v = Vec::new(&env);
        push_sample(&mut v, sample(1000, 20_000), 10);
        push_sample(&mut v, sample(500, 30_000), 10);
        assert_eq!(v.len(), 2);
        assert_eq!(v.get(0).unwrap(), sample(1000, 20_000));
        assert_eq!(v.get(1).unwrap(), sample(500, 30_000));
    }

    /// INV-TWAP1: an out-of-order pair contributes *no* weight — its
    /// segment has `seg_end < seg_start` and is dropped, so the sample's
    /// price is silently excluded rather than mis-ordering the average.
    #[test]
    fn test_out_of_order_pair_contributes_no_weight() {
        let env = Env::default();
        // (1000, 20_000) is followed by (500, 30_000). The second pair is
        // out of order, so only the 10_000 -> 20_000 pair counts and the
        // 30_000 price is ignored entirely.
        let samples = vec_of(&env, &[(0, 10_000), (1000, 20_000), (500, 30_000)]);
        assert_eq!(twap_average(&samples, 0, 1000), Some(15_000));
    }

    /// INV-TWAP2: retention is by count, not age. A `max_samples` of 0
    /// falls back to the `MAX_TWAP_SAMPLES` default bound.
    #[test]
    fn test_push_sample_zero_cap_uses_default_bound() {
        let env = Env::default();
        let mut v = Vec::new(&env);
        for i in 0..(MAX_TWAP_SAMPLES + 2) {
            push_sample(&mut v, sample(i as u64, 100), 0);
        }
        assert_eq!(v.len(), MAX_TWAP_SAMPLES);
    }

    /// INV-TWAP2 + INV-TWAP6: two samples sharing a ledger timestamp form
    /// a zero-duration pair that contributes no weight but still consumes
    /// a buffer slot (no de-duplication).
    #[test]
    fn test_duplicate_ledger_timestamp_contributes_no_weight() {
        let env = Env::default();
        // The (100, 20_000)/(100, 30_000) pair has duration 0 and is
        // skipped, so only the two real segments are averaged:
        // (15_000*100 + 20_000*100) / 200 = 17_500.
        let samples = vec_of(
            &env,
            &[(0, 10_000), (100, 20_000), (100, 30_000), (200, 10_000)],
        );
        assert_eq!(twap_average(&samples, 0, 200), Some(17_500));
        // A buffer whose only pair is a duplicate has no overlap to weight.
        let dup_only = vec_of(&env, &[(100, 20_000), (100, 30_000)]);
        assert_eq!(twap_average(&dup_only, 0, 200), None);
    }

    /// INV-TWAP3: a pair lying fully before `window_start` is skipped.
    #[test]
    fn test_stale_pair_before_window_is_skipped() {
        let env = Env::default();
        let samples = vec_of(&env, &[(0, 10_000), (40, 20_000)]);
        assert_eq!(twap_average(&samples, 50, 100), None);
    }

    /// INV-TWAP3: a partially overlapping pair is clipped to the window,
    /// so a sample older than `window_start` is carried forward and its
    /// price participates at the window's left edge.
    #[test]
    fn test_stale_boundary_sample_is_carried_into_window() {
        let env = Env::default();
        let samples = vec_of(&env, &[(0, 10_000), (1000, 20_000)]);
        // Window [500, 1000] clips the pair to its left edge; the $100
        // sample at t=0 still contributes (carry-forward).
        assert_eq!(twap_average(&samples, 500, 1000), Some(15_000));
    }

    /// INV-TWAP5: the accumulator saturates instead of panicking when a
    /// segment's weighted price overflows the `i128` bound.
    #[test]
    fn test_accumulation_saturates_instead_of_panicking() {
        let env = Env::default();
        let p = i128::MAX / 4;
        let samples = vec_of(&env, &[(0, p), (1_000_000_000, p)]);
        // avg = (p + p) / 2 = p (the pairwise sum stays below i128::MAX);
        // avg * duration saturates at i128::MAX, so the final divide
        // yields i128::MAX / duration without a panic.
        assert_eq!(
            twap_average(&samples, 0, 1_000_000_000),
            Some(i128::MAX / 1_000_000_000)
        );
    }

    /// INV-TWAP5 exception / RR-1: the pairwise price sum `s1.price +
    /// s2.price` is the ONE non-saturating operation here, so two prices
    /// whose sum exceeds i128::MAX trap. `record_twap_sample` bounds price
    /// only below (`price <= 0` rejected), so this is reachable; the test
    /// pins current behavior so any future hardening is deliberate.
    #[test]
    #[should_panic]
    fn test_pairwise_price_sum_overflows_above_i128_max() {
        let env = Env::default();
        let samples = vec_of(&env, &[(0, i128::MAX), (1, i128::MAX)]);
        let _ = twap_average(&samples, 0, 1);
    }

    /// INV-TWAP3 + INV-TWAP4: advancing `current_time` rolls the window
    /// forward, dropping aged pairs and shifting the average.
    #[test]
    fn test_window_rollover_drops_aged_pairs() {
        let env = Env::default();
        let samples = vec_of(&env, &[(0, 10_000), (1000, 10_000), (2000, 50_000)]);
        // Window length 1500. At t=2000 the window [500, 2000] spans both
        // segments: (10_000*500 + 30_000*1000) / 1500 = 23_333.
        assert_eq!(twap_average(&samples, 500, 2000), Some(23_333));
        // Roll to t=3000, window [1500, 3000]: the [0, 1000] pair falls
        // out, leaving the clipped [1000, 2000] segment -> 30_000.
        assert_eq!(twap_average(&samples, 1500, 3000), Some(30_000));
    }
}
