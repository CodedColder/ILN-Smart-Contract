#![cfg(test)]

//! ADR-004 — lazy reputation decay multi-year idle coverage.
//!
//! Tests that simulate long idle periods (years of ledger time) to verify
//! no overflow, correct floor convergence, and proper decay behavior.

use super::*;
use crate::constants::MAX_REPUTATION_DECAY_PERIODS;
use crate::test::setup;
use soroban_sdk::testutils::{Address as _, Ledger};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

/// Helper: submit, fund, and mark an invoice paid to establish a non-zero reputation.
fn establish_reputation(t: &crate::test::TestEnv, payer: &soroban_sdk::Address) {
    let now = t.env.ledger().timestamp();
    let due_date = now + DUE_DATE_OFFSET;
    let id = t.contract.submit_invoice(
        &t.freelancer,
        payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    );
    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);
}

/// Helper: advance the ledger by `ledgers` sequences.
fn advance_ledgers(t: &crate::test::TestEnv, ledgers: u32) {
    let mut ledger_info = t.env.ledger().get();
    ledger_info.sequence_number += ledgers;
    t.env.ledger().set(ledger_info);
}

/// 1 year idle: ~63 periods (365 days * 24h * 12ledgers/h ≈ 105,120 ledgers
/// at 5s/ledger; decay_period = 259,200 ledgers ≈ 15 days).
/// 1 year ≈ 365/15 ≈ 24.3 periods.
/// Score should decay noticeably but not reach floor.
#[test]
fn test_one_year_idle_partial_decay() {
    let t = setup();
    let payer = t.payer.clone();

    establish_reputation(&t, &payer);
    let score_before = t.contract.payer_score(&payer);
    assert!(
        score_before > 50,
        "score should have increased after payment"
    );

    // Advance 1 year: ~24 decay periods
    let one_year_ledgers = (365 * 24 * 60 * 60 / 5) as u32; // ~6,307,200
    advance_ledgers(&t, one_year_ledgers);

    let score_after = t.contract.payer_score(&payer);
    assert!(
        score_after < score_before,
        "score should decay after 1 year idle"
    );
    assert!(
        score_after > 0,
        "score should not reach floor after just 1 year"
    );
}

/// 3 years idle: ~73 periods. Score should be significantly reduced.
#[test]
fn test_three_year_idle_significant_decay() {
    let t = setup();
    let payer = t.payer.clone();

    establish_reputation(&t, &payer);
    let score_before = t.contract.payer_score(&payer);

    // Advance 3 years: ~73 decay periods
    let three_year_ledgers = (3 * 365 * 24 * 60 * 60 / 5) as u32;
    advance_ledgers(&t, three_year_ledgers);

    let score_after = t.contract.payer_score(&payer);
    assert!(
        score_after < score_before,
        "score should decay after 3 years"
    );
    // With 0.5% decay per period, after 73 periods: 0.995^73 ≈ 0.69
    // So score should be roughly 69% of original.
    assert!(
        score_after > 0,
        "score should still be positive after 3 years"
    );
}

/// 10 years idle: ~244 periods. Score should be very low but still non-negative.
#[test]
fn test_ten_year_idle_deep_decay() {
    let t = setup();
    let payer = t.payer.clone();

    establish_reputation(&t, &payer);
    let score_before = t.contract.payer_score(&payer);

    // Advance 10 years: ~244 decay periods
    let ten_year_ledgers = (10 * 365 * 24 * 60 * 60 / 5) as u32;
    advance_ledgers(&t, ten_year_ledgers);

    let score_after = t.contract.payer_score(&payer);
    assert!(
        score_after < score_before,
        "score should decay heavily after 10 years"
    );
    // 0.995^244 ≈ 0.29 — score should be around 29% of original
    assert!(
        score_after < score_before / 2,
        "score should be less than half after 10 years"
    );
}

/// 100 years idle: ~2443 periods. Should hit the MAX_REPUTATION_DECAY_PERIODS
/// cap (1000) and short-circuit to 0.
#[test]
fn test_hundred_year_idle_floor() {
    let t = setup();
    let payer = t.payer.clone();

    establish_reputation(&t, &payer);

    // Advance 100 years: ~2,443,200 ledgers — way beyond MAX_REPUTATION_DECAY_PERIODS
    let hundred_year_ledgers = (100u64 * 365 * 24 * 60 * 60 / 5) as u32;
    advance_ledgers(&t, hundred_year_ledgers);

    let score_after = t.contract.payer_score(&payer);
    assert_eq!(
        score_after, 0,
        "score must converge to 0 after extreme idle (100 years)"
    );
}

/// Verify no overflow: even with maximum initial score (100) and the most
/// aggressive decay, the arithmetic never panics.
#[test]
fn test_decay_no_overflow_at_max_score() {
    let t = setup();
    let payer = t.payer.clone();

    establish_reputation(&t, &payer);

    // Manually set score to 100 (max)
    set_payer_score(&t.env, &payer, 100);
    assert_eq!(t.contract.payer_score(&payer), 100);

    // Advance 10 years — should not panic
    let ledgers = (10 * 365 * 24 * 60 * 60 / 5) as u32;
    advance_ledgers(&t, ledgers);

    let score = t.contract.payer_score(&payer);
    assert!(score <= 100, "score must never exceed 100");
    // score: u32 — cannot go negative (clippy absurd_extreme_comparisons)
}

/// Verify decay converges to 0 correctly (not stuck at 1).
/// With 0.5% decay per period, the score approaches 0 asymptotically.
/// The minimum-decay-of-1 rule ensures it eventually reaches 0.
#[test]
fn test_decay_converges_to_zero_not_stuck_at_one() {
    let t = setup();
    let payer = t.payer.clone();

    // Set score to 1 (the minimum non-zero score)
    set_payer_score(&t.env, &payer, 1);
    assert_eq!(t.contract.payer_score(&payer), 1);

    // Advance 1 period (decay_period_ledgers = 259,200)
    advance_ledgers(&t, 259_200);

    let score = t.contract.payer_score(&payer);
    // With score=1, decay_amount = 1 * 50 / 10000 = 0, but the minimum
    // decay of 1 kicks in, so score goes to 0.
    assert_eq!(
        score, 0,
        "score of 1 must decay to 0 in one period (min decay = 1)"
    );
}

/// Verify that a second activity resets the decay clock.
#[test]
fn test_activity_resets_decay_clock() {
    let t = setup();
    let payer = t.payer.clone();

    establish_reputation(&t, &payer);
    let score_after_first = t.contract.payer_score(&payer);

    // Advance 1 year — score decays
    let one_year = (365 * 24 * 60 * 60 / 5) as u32;
    advance_ledgers(&t, one_year);
    let score_after_idle = t.contract.payer_score(&payer);
    assert!(
        score_after_idle < score_after_first,
        "score should decay after idle"
    );

    // Perform another payment — score should increase and decay clock resets
    establish_reputation(&t, &payer);
    let score_after_second = t.contract.payer_score(&payer);
    assert!(
        score_after_second >= score_after_idle,
        "new activity should restore or increase score"
    );

    // Advance another year — should decay from the new baseline
    advance_ledgers(&t, one_year);
    let score_after_second_idle = t.contract.payer_score(&payer);
    assert!(
        score_after_second_idle < score_after_second,
        "score should decay again after second idle period"
    );
}
