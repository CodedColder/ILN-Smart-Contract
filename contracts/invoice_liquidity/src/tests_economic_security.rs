//! Economic Security Regression Suite
//! ===================================
//!
//! Consolidated regression surface for the protocol's economic-security
//! properties. Every test re-exported here guards an invariant that, if
//! broken silently by a refactor, would be exploitable for value extraction
//! rather than a mere functional bug.
//!
//! This module is declared in `lib.rs` and therefore runs as part of every
//! required CI invocation (`make test-rust` → `cargo test`, and
//! `cargo test -p invoice_liquidity`) — it cannot be omitted the way a loose
//! test file outside the crate graph could be.
//!
//! Threat-model coverage (see docs/threat-model.md):
//!
//! | Section | Threat                                        | Covered by |
//! |---------|-----------------------------------------------|------------|
//! | B0      | Ordinary `fund_invoice()` front-running       | `tests_mev_mitigation` (maturity delay) |
//! | B1      | LP queue position manipulation / griefing     | queue maturity, tie randomization, sorted-order integrity |
//! | D1      | Reputation score manipulation                 | highest-reputation LP wins; reputation-weighted ordering |
//!
//! Issue traceability:
//!  - #MEV-1: `resolve_fund_queue` maturity delay (griefing/front-run mitigation)
//!  - #708:   randomized tie-breaking so first-submitter no longer wins ties deterministically
//!  - #34:    reputation-weighted LP priority queue invariants
//!
//! Each wrapper below invokes the canonical test in its home module, keeping
//! a single source of truth while giving this suite its own obvious, named
//! failure points (`es_*`) in CI output.

#![cfg(test)]

// --- MEV / front-running / griefing mitigation (tests_mev_mitigation.rs) ---

#[test]
fn es_mev_queue_resolution_fails_immediately_after_join() {
    super::tests_mev_mitigation::test_resolve_queue_fails_immediately_after_join();
}

#[test]
fn es_mev_queue_resolution_fails_one_ledger_before_delay() {
    super::tests_mev_mitigation::test_resolve_queue_fails_one_ledger_before_delay();
}

#[test]
fn es_mev_queue_resolution_succeeds_after_delay() {
    super::tests_mev_mitigation::test_resolve_queue_succeeds_after_delay();
}

#[test]
fn es_mev_queue_resolution_succeeds_well_after_delay() {
    super::tests_mev_mitigation::test_resolve_queue_succeeds_well_after_delay();
}

#[test]
fn es_mev_second_lp_join_does_not_reset_maturity_timer() {
    super::tests_mev_mitigation::test_second_lp_join_does_not_reset_maturity_timer();
}

#[test]
fn es_mev_already_resolved_queue_returns_same_winner() {
    super::tests_mev_mitigation::test_resolve_already_resolved_queue_returns_same_winner();
}

#[test]
fn es_mev_rejected_resolution_emits_attempt_event_success_false() {
    super::tests_mev_mitigation::test_rejected_resolution_emits_attempt_event_with_success_false();
}

#[test]
fn es_mev_successful_resolution_emits_attempt_event_success_true() {
    super::tests_mev_mitigation::test_successful_resolution_emits_attempt_event_with_success_true();
}

/// Issue #708 — tie-breaking must not deterministically favour the first joiner.
#[test]
fn es_mev_tie_resolution_is_not_always_the_first_joiner() {
    super::tests_mev_mitigation::test_tie_resolution_is_not_always_the_first_joiner();
}

#[test]
fn es_mev_single_lp_queue_still_resolves_deterministically() {
    super::tests_mev_mitigation::test_resolve_queue_still_resolves_deterministically_for_a_single_lp();
}

#[test]
fn es_mev_queue_winner_can_transfer_position_to_loser() {
    super::tests_mev_mitigation::test_queue_winner_can_transfer_position_to_queue_loser();
}

// --- Reputation-weighted queue integrity (tests_lp_priority_queue.rs) ---

#[test]
fn es_queue_duplicate_join_rejected() {
    super::tests_lp_priority_queue::test_duplicate_queue_join_rejected();
}

#[test]
fn es_queue_highest_reputation_lp_wins() {
    super::tests_lp_priority_queue::test_highest_reputation_lp_wins_queue();
}

#[test]
fn es_queue_tie_broken_first_come_first_served_within_randomization() {
    super::tests_lp_priority_queue::test_tie_broken_by_first_come_first_served();
}

#[test]
fn es_queue_non_approved_lp_cannot_fund_after_resolution() {
    super::tests_lp_priority_queue::test_non_approved_lp_cannot_fund_after_queue_resolution();
}

#[test]
fn es_queue_resolution_is_idempotent() {
    super::tests_lp_priority_queue::test_resolve_queue_is_idempotent();
}

#[test]
fn es_queue_maintains_sorted_order_after_joins() {
    super::tests_lp_priority_queue::test_queue_maintains_sorted_order_after_joins();
}

#[test]
fn es_queue_sorted_when_joining_in_reverse_order() {
    super::tests_lp_priority_queue::test_queue_sorted_even_when_joining_in_reverse_order();
}

#[test]
fn es_queue_sorted_when_inserting_mid_range_scores() {
    super::tests_lp_priority_queue::test_queue_sorted_when_inserting_mid_range_scores();
}

#[test]
fn es_queue_duplicate_prevention_with_sorted_queue() {
    super::tests_lp_priority_queue::test_duplicate_prevention_with_sorted_queue();
}

// --- Configuration parameter bounds enforcement (tests_config_parameter_bounds.rs) ---

/// Issue #914: decay_rate_bps must not exceed 500 bps (5% per period).
#[test]
fn es_config_decay_rate_bps_boundary_accepted_at_max() {
    super::tests_config_parameter_bounds::test_decay_rate_bps_boundary_accepted_at_max();
}

#[test]
fn es_config_decay_rate_bps_exceeding_max_rejected() {
    super::tests_config_parameter_bounds::test_decay_rate_bps_exceeding_max_rejected();
}

/// Issue #915: high_rep_threshold must stay within 0-100 (reputation score range).
#[test]
fn es_config_high_rep_threshold_boundary_accepted_at_max() {
    super::tests_config_parameter_bounds::test_high_rep_threshold_boundary_accepted_at_max();
}

#[test]
fn es_config_high_rep_threshold_exceeding_max_rejected() {
    super::tests_config_parameter_bounds::test_high_rep_threshold_exceeding_max_rejected();
}

/// Issue #916: min_discount_rate_bps must be in (0, 10_000), i.e., 0% < x < 100%.
#[test]
fn es_config_min_discount_rate_bps_zero_rejected() {
    super::tests_config_parameter_bounds::test_min_discount_rate_bps_zero_rejected();
}

#[test]
fn es_config_min_discount_rate_bps_boundary_accepted_below_max() {
    super::tests_config_parameter_bounds::test_min_discount_rate_bps_boundary_accepted_below_max();
}

#[test]
fn es_config_min_discount_rate_bps_exceeding_max_rejected() {
    super::tests_config_parameter_bounds::test_min_discount_rate_bps_exceeding_max_rejected();
}

/// Issue #918: Governance-wide regression suite ensuring no parameter bound
/// can be silently removed without causing test failures.
#[test]
fn es_config_all_parameters_at_max_bounds() {
    super::tests_config_parameter_bounds::test_all_parameters_at_max_bounds();
}

#[test]
fn es_config_one_parameter_exceeds_bound_rejected() {
    super::tests_config_parameter_bounds::test_one_parameter_exceeds_bound_rejected();
}

#[test]
fn es_config_minimal_valid_parameters() {
    super::tests_config_parameter_bounds::test_minimal_valid_parameters();
}

#[test]
fn es_config_typical_production_parameters() {
    super::tests_config_parameter_bounds::test_typical_production_parameters();
}
