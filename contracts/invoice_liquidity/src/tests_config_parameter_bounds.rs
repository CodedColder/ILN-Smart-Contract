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
