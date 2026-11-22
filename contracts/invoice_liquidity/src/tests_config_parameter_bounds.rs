//! Configuration Parameter Bounds Validation Suite
//! ================================================
//!
//! Regression tests ensuring all governance-settable numeric parameters
//! have proper bounds enforcement to prevent economically-nonsensical or
//! unsafe configurations.
//!
//! Tests cover:
//! - Issue #914: decay_rate_bps upper-bound validation (MAX = 500 bps / 5%)
//! - Issue #915: high_rep_threshold 0-100 range validation + type audit
//! - Issue #916: min_discount_rate_bps bounds (0 < x < 10,000)
//! - Issue #918: governance-wide parameter bounds regression suite

#![cfg(test)]

use super::test::{setup, TestEnv};
use crate::constants::{ECONOMIC_PARAM_COOLDOWN_LEDGERS, MAX_DISCOUNT_RATE};
use crate::errors::ContractError;
use soroban_sdk::testutils::Ledger;

// ================================================================
// Issue #914: decay_rate_bps upper-bound validation
// ================================================================

#[test]
pub(crate) fn test_decay_rate_bps_boundary_accepted_at_max() {
    let t = setup();

    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &70,
        &200,
        &500,
        &500, // Exactly at MAX_DECAY_RATE_BPS
        &2000,
        &5000,
        &t.token.address,
        &t.token.address,
        &t.token.address,
    );

    assert!(
        result.is_ok(),
        "decay_rate_bps at boundary (500 bps) must be accepted"
    );
}

#[test]
pub(crate) fn test_decay_rate_bps_exceeding_max_rejected() {
    let t = setup();

    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &70,
        &200,
        &500,
        &501, // Exceeds MAX_DECAY_RATE_BPS (500)
        &2000,
        &5000,
        &t.token.address,
        &t.token.address,
        &t.token.address,
    );

    assert!(
        result.is_err(),
        "decay_rate_bps exceeding 500 must be rejected"
    );
}

#[test]
fn test_decay_rate_bps_zero_accepted() {
    let t = setup();

    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &70,
        &200,
        &500,
        &0, // Zero is valid (no decay)
        &2000,
        &5000,
        &t.token.address,
        &t.token.address,
        &t.token.address,
    );

    assert!(result.is_ok(), "decay_rate_bps = 0 must be accepted");
}

// ================================================================
// Issue #915: high_rep_threshold 0-100 range validation
// ================================================================

#[test]
pub(crate) fn test_high_rep_threshold_boundary_accepted_at_max() {
    let t = setup();

    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &100, // Exactly at MAX (100)
        &200,
        &500,
        &300,
        &2000,
        &5000,
        &t.token.address,
        &t.token.address,
        &t.token.address,
    );

    assert!(
        result.is_ok(),
        "high_rep_threshold at boundary (100) must be accepted"
    );
}

#[test]
pub(crate) fn test_high_rep_threshold_exceeding_max_rejected() {
    let t = setup();

    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &101, // Exceeds MAX_REP_THRESHOLD (100)
        &200,
        &500,
        &300,
        &2000,
        &5000,
        &t.token.address,
        &t.token.address,
        &t.token.address,
    );

    assert!(
        result.is_err(),
        "high_rep_threshold exceeding 100 must be rejected"
    );
}

#[test]
fn test_high_rep_threshold_zero_accepted() {
    let t = setup();

    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &0, // Zero is valid (no high-rep bonus path)
        &200,
        &500,
        &300,
        &2000,
        &5000,
        &t.token.address,
        &t.token.address,
        &t.token.address,
    );

    assert!(result.is_ok(), "high_rep_threshold = 0 must be accepted");
}

#[test]
fn test_high_rep_threshold_mid_range_accepted() {
    let t = setup();

    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &50, // Mid-range value
        &200,
        &500,
        &300,
        &2000,
        &5000,
        &t.token.address,
        &t.token.address,
        &t.token.address,
    );

    assert!(result.is_ok(), "high_rep_threshold = 50 must be accepted");
}

// ================================================================
// Issue #916: min_discount_rate_bps bounds validation
// ================================================================

#[test]
pub(crate) fn test_min_discount_rate_bps_zero_rejected() {
    let t = setup();

    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &70,
        &200,
        &0, // Zero is invalid (must be > 0)
        &300,
        &2000,
        &5000,
        &t.token.address,
        &t.token.address,
        &t.token.address,
    );

    assert!(
        result.is_err(),
        "min_discount_rate_bps = 0 must be rejected"
    );
}

#[test]
pub(crate) fn test_min_discount_rate_bps_boundary_accepted_below_max() {
    let t = setup();

    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &70,
        &200,
        &9_999, // Just below MAX (10,000)
        &300,
        &2000,
        &5000,
        &t.token.address,
        &t.token.address,
        &t.token.address,
    );

    assert!(
        result.is_ok(),
        "min_discount_rate_bps at boundary (9,999) must be accepted"
    );
}

#[test]
pub(crate) fn test_min_discount_rate_bps_exceeding_max_rejected() {
    let t = setup();

    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &70,
        &200,
        &10_000, // At or above MAX (10,000 = 100%)
        &300,
        &2000,
        &5000,
        &t.token.address,
        &t.token.address,
        &t.token.address,
    );

    assert!(
        result.is_err(),
        "min_discount_rate_bps >= 10,000 must be rejected"
    );
}

#[test]
fn test_min_discount_rate_bps_above_max_rejected() {
    let t = setup();

    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &70,
        &200,
        &15_000, // Well above MAX
        &300,
        &2000,
        &5000,
        &t.token.address,
        &t.token.address,
        &t.token.address,
    );

    assert!(
        result.is_err(),
        "min_discount_rate_bps >> 10,000 must be rejected"
    );
}

#[test]
fn test_min_discount_rate_bps_low_value_accepted() {
    let t = setup();

    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &70,
        &200,
        &1, // Minimum valid value
        &300,
        &2000,
        &5000,
        &t.token.address,
        &t.token.address,
        &t.token.address,
    );

    assert!(result.is_ok(), "min_discount_rate_bps = 1 must be accepted");
}

// ================================================================
// Issue #918: Governance-Wide Parameter Bounds Regression Suite
// ================================================================

/// Composite test: all parameters at their maximum valid bounds.
/// If any parameter's upper bound is silently removed in a refactor,
/// this test will fail.
#[test]
pub(crate) fn test_all_parameters_at_max_bounds() {
    let t = setup();

    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &100,            // high_rep_threshold: MAX = 100
        &500,            // bonus_bps: MAX = 500
        &9_999,          // min_discount_rate_bps: MAX = 9,999
        &500,            // decay_rate_bps: MAX = 500
        &u64::MAX,       // decay_period_ledgers: no upper bound
        &u64::MAX,       // dispute_timeout_ledgers: no upper bound
        &t.token.address,
        &t.token.address,
        &t.token.address,
    );

    assert!(
        result.is_ok(),
        "all parameters at their maximum valid bounds must be accepted"
    );
}

/// Composite test: at least one parameter exceeds its bound.
/// Validates that bounds are actually enforced in combination.
#[test]
pub(crate) fn test_one_parameter_exceeds_bound_rejected() {
    let t = setup();

    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &101,            // high_rep_threshold: EXCEEDS MAX (100)
        &500,            // bonus_bps: valid
        &5_000,          // min_discount_rate_bps: valid
        &500,            // decay_rate_bps: valid
        &2_000,
        &5_000,
        &t.token.address,
        &t.token.address,
        &t.token.address,
    );

    assert!(
        result.is_err(),
        "if any parameter exceeds its bound, update must fail"
    );
}

/// Test: minimal valid parameter set (all at lower bounds where applicable).
#[test]
pub(crate) fn test_minimal_valid_parameters() {
    let t = setup();

    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &0,     // high_rep_threshold: MIN = 0
        &0,     // bonus_bps: MIN = 0 (no bonus)
        &1,     // min_discount_rate_bps: MIN = 1 (can't be 0)
        &0,     // decay_rate_bps: MIN = 0 (no decay)
        &1,     // decay_period_ledgers: MIN = 1
        &1,     // dispute_timeout_ledgers: MIN = 1
        &t.token.address,
        &t.token.address,
        &t.token.address,
    );

    assert!(
        result.is_ok(),
        "minimal valid parameter set must be accepted"
    );
}

/// Test: typical production parameters (mid-range, sensible governance defaults).
#[test]
pub(crate) fn test_typical_production_parameters() {
    let t = setup();

    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &70,            // high_rep_threshold: typical
        &200,           // bonus_bps: 2% typical high-rep bonus
        &300,           // min_discount_rate_bps: 3% minimum discount
        &50,            // decay_rate_bps: 0.5% typical decay
        &2_000,         // decay_period_ledgers: ~10 days
        &5_000,         // dispute_timeout_ledgers: ~25 days
        &t.token.address,
        &t.token.address,
        &t.token.address,
    );

    assert!(
        result.is_ok(),
        "typical production parameters must be accepted"
    );
}

// ================================================================
// Issue #1180: fee & discount bounds + ledger-boundary behaviour
// ================================================================
//
// Scope: the two economic setters named in the issue — `update_fee_rate`
// and `update_max_discount` — plus the shared ledger-based cooldown in
// `access::check_rate_limit` that sequences them.
//
// Data sources / assumptions:
//   * Rates are basis points out of 10,000 (1 bp = 0.01%).
//   * `initialize` seeds `DataKey::FeeRate = 0` and
//     `DataKey::MaxDiscountRate = constants::MAX_DISCOUNT_RATE` (5,000).
//   * `validate_invoice_terms_for_min` reads the *stored* `MaxDiscountRate`
//     (default 5,000) as the per-invoice discount ceiling.
//   * `check_rate_limit` uses `ECONOMIC_PARAM_COOLDOWN_LEDGERS` (360).
//
// Authorization: admin-only; the exhaustive imposter matrix lives in
// `tests_access_control` (Issues #540/#1185).
//
// Failure behaviour: a rejected call reverts atomically (Soroban), so it
// neither mutates the stored parameter nor consumes the cooldown, and it
// appends nothing to the admin-action audit log.

// -- Ledger-boundary behaviour of the shared cooldown ----------------

/// A second `update_fee_rate` exactly one ledger before the cooldown has
/// elapsed must be rejected — the boundary is exclusive.
#[test]
fn test_fee_rate_cooldown_boundary_off_by_one_rejected() {
    let t = setup();
    let start = t.env.ledger().sequence();

    assert!(t.contract.try_update_fee_rate(&100).is_ok());

    let mut info = t.env.ledger().get();
    info.sequence_number = start + (ECONOMIC_PARAM_COOLDOWN_LEDGERS as u32) - 1;
    t.env.ledger().set(info);

    assert_eq!(
        t.contract.try_update_fee_rate(&200),
        Err(Ok(ContractError::RateLimited)),
        "one ledger before the cooldown boundary must still be rate-limited"
    );
}

/// A second `update_fee_rate` exactly at `last + cooldown` must be accepted.
#[test]
fn test_fee_rate_cooldown_boundary_at_exact_threshold_accepted() {
    let t = setup();
    let start = t.env.ledger().sequence();

    assert!(t.contract.try_update_fee_rate(&100).is_ok());

    let mut info = t.env.ledger().get();
    info.sequence_number = start + (ECONOMIC_PARAM_COOLDOWN_LEDGERS as u32);
    t.env.ledger().set(info);

    assert!(
        t.contract.try_update_fee_rate(&200).is_ok(),
        "at exactly last + cooldown the call must succeed"
    );
}

/// A second `update_max_discount` one ledger before the cooldown must fail.
#[test]
fn test_max_discount_cooldown_boundary_off_by_one_rejected() {
    let t = setup();
    let start = t.env.ledger().sequence();

    assert!(t.contract.try_update_max_discount(&3_000).is_ok());

    let mut info = t.env.ledger().get();
    info.sequence_number = start + (ECONOMIC_PARAM_COOLDOWN_LEDGERS as u32) - 1;
    t.env.ledger().set(info);

    assert_eq!(
        t.contract.try_update_max_discount(&4_000),
        Err(Ok(ContractError::RateLimited)),
        "one ledger before the cooldown boundary must still be rate-limited"
    );
}

/// A second `update_max_discount` exactly at `last + cooldown` must succeed.
#[test]
fn test_max_discount_cooldown_boundary_at_exact_threshold_accepted() {
    let t = setup();
    let start = t.env.ledger().sequence();

    assert!(t.contract.try_update_max_discount(&3_000).is_ok());

    let mut info = t.env.ledger().get();
    info.sequence_number = start + (ECONOMIC_PARAM_COOLDOWN_LEDGERS as u32);
    t.env.ledger().set(info);

    assert!(
        t.contract.try_update_max_discount(&4_000).is_ok(),
        "at exactly last + cooldown the call must succeed"
    );
}

/// A rejected (rate-limited) call must not consume or advance the cooldown:
/// an immediate retry is still limited, and the original window still governs.
#[test]
fn test_rejected_fee_rate_call_does_not_consume_cooldown() {
    let t = setup();
    let start = t.env.ledger().sequence();

    assert!(t.contract.try_update_fee_rate(&100).is_ok());

    // Still inside the cooldown: rejected.
    assert_eq!(
        t.contract.try_update_fee_rate(&200),
        Err(Ok(ContractError::RateLimited))
    );
    // A further retry at the same ledger is likewise rejected — the failed
    // attempt did not move the recorded last-call ledger.
    assert_eq!(
        t.contract.try_update_fee_rate(&300),
        Err(Ok(ContractError::RateLimited))
    );

    // The window is still measured from the single successful call.
    let mut info = t.env.ledger().get();
    info.sequence_number = start + (ECONOMIC_PARAM_COOLDOWN_LEDGERS as u32);
    t.env.ledger().set(info);
    assert!(t.contract.try_update_fee_rate(&300).is_ok());
}

// -- Atomicity / no partial update -----------------------------------

/// A rate-limited rejection must leave the admin-action audit log unchanged:
/// no partial write survives the reverted transaction.
#[test]
fn test_rate_limited_fee_rate_does_not_append_admin_action() {
    let t = setup();

    assert!(t.contract.try_update_fee_rate(&100).is_ok());
    let after_success = t.contract.get_recent_admin_actions(&50).len();

    assert_eq!(
        t.contract.try_update_fee_rate(&200),
        Err(Ok(ContractError::RateLimited))
    );
    let after_rejection = t.contract.get_recent_admin_actions(&50).len();

    assert_eq!(
        after_success, after_rejection,
        "a rejected update_fee_rate must not append an admin-action record"
    );
}

/// Same atomicity guarantee for `update_max_discount`.
#[test]
fn test_rate_limited_max_discount_does_not_append_admin_action() {
    let t = setup();

    assert!(t.contract.try_update_max_discount(&3_000).is_ok());
    let after_success = t.contract.get_recent_admin_actions(&50).len();

    assert_eq!(
        t.contract.try_update_max_discount(&4_000),
        Err(Ok(ContractError::RateLimited))
    );
    let after_rejection = t.contract.get_recent_admin_actions(&50).len();

    assert_eq!(
        after_success, after_rejection,
        "a rejected update_max_discount must not append an admin-action record"
    );
}

// -- In-range acceptance + observed bounds ---------------------------

/// Well-formed, in-range fee rates are accepted (advancing past the cooldown
/// between calls so the rate limiter does not mask the bound result).
#[test]
fn test_fee_rate_in_range_values_accepted() {
    let t = setup();

    for value in [0u32, 1, 300, 4_000] {
        let mut info = t.env.ledger().get();
        info.sequence_number += ECONOMIC_PARAM_COOLDOWN_LEDGERS as u32 + 1;
        t.env.ledger().set(info);

        assert!(
            t.contract.try_update_fee_rate(&value).is_ok(),
            "in-range fee_rate {value} must be accepted"
        );
    }
}

/// A `max_discount` ceiling equal to the protocol constant is accepted.
#[test]
fn test_max_discount_at_configured_ceiling_accepted() {
    let t = setup();

    assert!(
        t.contract.try_update_max_discount(&MAX_DISCOUNT_RATE).is_ok(),
        "max_discount at MAX_DISCOUNT_RATE must be accepted"
    );
}

/// VF-1180-1 (verified finding — characterization, NOT an endorsement):
///
/// `update_max_discount` applies **no upper bound**, so governance can raise
/// the discount ceiling above `constants::MAX_DISCOUNT_RATE` (5,000 bps = 50%).
/// Because `validate_invoice_terms_for_min` trusts the *stored* value as the
/// per-invoice discount ceiling, a ceiling above 5,000 admits invoices carrying
/// discounts beyond the documented cap. This test pins the *current* behaviour
/// so the gap is visible; once VF-1180-1 is remediated, the assertion below
/// must flip to `result.is_err()` (expected `InvalidDiscountRate`).
#[test]
fn test_max_discount_above_ceiling_observed_behaviour_vf1180_1() {
    let t = setup();
    let above = MAX_DISCOUNT_RATE + 1;

    let result = t.contract.try_update_max_discount(&above);

    assert!(
        result.is_ok(),
        "VF-1180-1: update_max_discount currently accepts a ceiling above MAX_DISCOUNT_RATE"
    );
}

/// RR-1180-1 (residual risk — characterization): `update_fee_rate` applies no
/// declared ceiling either. A compromise of the admin/multisig key could set
/// the protocol fee at or above the bps denominator (10,000 = 100%). The value
/// is not re-validated at settlement (`effective_fee_rate`), so the risk is
/// bounded only by the admin gate and the economic-parameter cooldown.
/// Pinned here so a future cap is a deliberate, reviewed change.
#[test]
fn test_fee_rate_at_and_above_bps_denominator_observed_behaviour_rr1180_1() {
    let t = setup();

    assert!(
        t.contract.try_update_fee_rate(&10_000).is_ok(),
        "RR-1180-1: update_fee_rate currently accepts the full 100% (10,000 bps) boundary"
    );

    let mut info = t.env.ledger().get();
    info.sequence_number += ECONOMIC_PARAM_COOLDOWN_LEDGERS as u32;
    t.env.ledger().set(info);

    assert!(
        t.contract.try_update_fee_rate(&u32::MAX).is_ok(),
        "RR-1180-1: update_fee_rate currently accepts arbitrarily large values"
    );
}
