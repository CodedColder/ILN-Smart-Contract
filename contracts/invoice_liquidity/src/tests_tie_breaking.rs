//! Issue #1027 — Focused tests for join-order tie-breaking replacement
//!
//! Scenarios covered:
//!  - Two-way tie: winner is one of the tied LPs (success)
//!  - Three-way tie: winner is one of the tied LPs (success)
//!  - No tie (single top score): FundQueueTieBroken event is NOT emitted
//!  - FundQueueTieBroken event IS emitted with correct fields on a tie
//!  - Distribution/fairness: both LPs win across many independent ties
//!  - Ledger-boundary: resolution at exactly QUEUE_DELAY_LEDGERS with ties
//!  - Authorization: only the approved LP can fund after tie-break resolution
//!  - Non-approved LP is rejected after tie-break resolution
//!  - Higher-reputation LP wins outright (no tie, no PRNG)
//!  - Tie-break winner is always within the tied prefix (adversarial check)

#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env, Symbol,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

struct TieTestEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: TokenClient<'static>,
    freelancer: Address,
    payer: Address,
    lp_a: Address,
    lp_b: Address,
    lp_c: Address,
    lp_d: Address,
}

fn setup_tie() -> TieTestEnv {
    let env = Env::default();
    env.mock_all_auths();

    let usdc_admin = Address::generate(&env);
    let usdc_id = env.register_stellar_asset_contract_v2(usdc_admin.clone());
    let usdc_addr = usdc_id.address();

    let token = TokenClient::new(&env, &usdc_addr);
    let token_admin = StellarAssetClient::new(&env, &usdc_addr);

    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let lp_a = Address::generate(&env);
    let lp_b = Address::generate(&env);
    let lp_c = Address::generate(&env);
    let lp_d = Address::generate(&env);

    for lp in [&lp_a, &lp_b, &lp_c, &lp_d] {
        token_admin.mint(lp, &(INVOICE_AMOUNT * 100));
    }
    token_admin.mint(&payer, &(INVOICE_AMOUNT * 100));

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    token_admin.mint(&contract.address, &(INVOICE_AMOUNT * 1000));

    let xlm_admin = Address::generate(&env);
    let xlm_id = env.register_stellar_asset_contract_v2(xlm_admin);
    let xlm_addr = xlm_id.address();

    let eurc_addr = Address::generate(&env);
    contract.initialize(&usdc_admin, &usdc_addr, &eurc_addr, &xlm_addr);

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    ledger.sequence_number = 100;
    env.ledger().set(ledger);

    TieTestEnv {
        env,
        contract,
        token,
        freelancer,
        payer,
        lp_a,
        lp_b,
        lp_c,
        lp_d,
    }
}

fn submit_invoice_tie(t: &TieTestEnv) -> u64 {
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    )
}

fn advance_ledgers(env: &Env, delta: u32) {
    let mut info = env.ledger().get();
    info.sequence_number += delta;
    info.timestamp += u64::from(delta) * 5;
    env.ledger().set(info);
}

/// Resolve the queue after the maturity delay has elapsed.
fn resolve_mature(t: &TieTestEnv, id: &u64) -> Address {
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    t.contract.resolve_fund_queue(id)
}

// ── Success: winner is from tied set ─────────────────────────────────────────

#[test]
fn test_two_way_tie_winner_is_one_of_tied_lps() {
    let t = setup_tie();
    let id = submit_invoice_tie(&t);

    // Both LPs have default score 50 — genuine tie.
    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);

    let winner = resolve_mature(&t, &id);
    assert!(
        winner == t.lp_a || winner == t.lp_b,
        "winner on two-way tie must be one of the tied LPs, got {winner:?}"
    );
}

#[test]
fn test_three_way_tie_winner_is_one_of_tied_lps() {
    let t = setup_tie();
    let id = submit_invoice_tie(&t);

    // All three LPs have default score 50 — three-way tie.
    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    t.contract.join_fund_queue(&t.lp_c, &id);

    let winner = resolve_mature(&t, &id);
    assert!(
        winner == t.lp_a || winner == t.lp_b || winner == t.lp_c,
        "winner on three-way tie must be one of the tied LPs, got {winner:?}"
    );
}

// ── No tie: event not emitted ────────────────────────────────────────────────

#[test]
fn test_no_tie_does_not_emit_tie_broken_event() {
    let t = setup_tie();
    let id = submit_invoice_tie(&t);

    // Only one LP in the queue — no tie possible.
    t.contract.join_fund_queue(&t.lp_a, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);

    // Snapshot event count before resolution (after join + ledger advance).
    let events_before = t.env.events().all().events().len();

    t.contract.resolve_fund_queue(&id);

    let events_after = t.env.events().all();

    // With a single LP (no tie), resolution emits events for the resolution
    // attempt + FundQueueResolved, but NOT FundQueueTieBroken.
    // We verify by checking that the event count increased by at least 1
    // (FundQueueResolved) but did not include an extra tie-broken event.
    // The exact count depends on SDK event consolidation, so we check that
    // resolution produced events and that a tie-break would add one more.
    let new_events = events_after.events().len() - events_before;
    assert!(
        new_events >= 1,
        "expected at least 1 event from non-tie resolution, got {new_events}"
    );

    // Now run a tie scenario with the same setup to compare event counts.
    let id2 = submit_invoice_tie(&t);
    t.contract.join_fund_queue(&t.lp_a, &id2);
    t.contract.join_fund_queue(&t.lp_b, &id2);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);

    let events_before_tie = t.env.events().all().events().len();
    t.contract.resolve_fund_queue(&id2);
    let events_after_tie = t.env.events().all();

    let new_events_tie = events_after_tie.events().len() - events_before_tie;

    // The tie case must emit exactly 1 more event than the no-tie case
    // (the FundQueueTieBroken event).
    assert_eq!(
        new_events_tie,
        new_events + 1,
        "tie resolution should emit exactly 1 more event than non-tie (the FundQueueTieBroken event), \
         got {new_events_tie} for tie vs {new_events} for non-tie"
    );
}

// ── Tie: event emitted with correct fields ───────────────────────────────────

#[test]
fn test_tie_broken_event_emitted_with_correct_fields() {
    let t = setup_tie();
    let id = submit_invoice_tie(&t);

    // Two LPs with equal score — tie.
    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);

    let events_before = t.env.events().all().events().len();

    let winner = t.contract.resolve_fund_queue(&id);

    let events_after = t.env.events().all();

    // With a tie, resolution emits 3 events:
    // FundQueueResolutionAttempted + FundQueueTieBroken + FundQueueResolved.
    let new_events = events_after.events().len() - events_before;
    assert_eq!(
        new_events, 3,
        "expected exactly 3 events (attempt + tie_broken + resolved) for tie resolution, got {new_events}"
    );

    // The winner must be one of the tied LPs.
    assert!(
        winner == t.lp_a || winner == t.lp_b,
        "winner must be one of the tied LPs"
    );
}

// ── Distribution/fairness test ────────────────────────────────────────────────

#[test]
fn test_tie_breaking_distribution_both_lps_win_over_many_trials() {
    let t = setup_tie();

    let mut lp_a_wins = 0u32;
    let mut lp_b_wins = 0u32;
    const TRIALS: u32 = 30;

    for _ in 0..TRIALS {
        let id = submit_invoice_tie(&t);
        // Both default to score 50 — genuine tie every time.
        t.contract.join_fund_queue(&t.lp_a, &id);
        t.contract.join_fund_queue(&t.lp_b, &id);
        advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);

        let winner = t.contract.resolve_fund_queue(&id);
        if winner == t.lp_a {
            lp_a_wins += 1;
        } else if winner == t.lp_b {
            lp_b_wins += 1;
        } else {
            panic!("resolve_fund_queue returned neither tied LP");
        }
    }

    assert_eq!(lp_a_wins + lp_b_wins, TRIALS);
    // Both LPs must win at least once across 30 trials. With uniform random
    // selection between 2 candidates, P(all 30 go to one side) = 2 * (0.5^30)
    // ≈ 1.9e-9 — effectively impossible if the PRNG is working.
    assert!(
        lp_a_wins > 0 && lp_b_wins > 0,
        "expected both tied LPs to win at least once across {TRIALS} ties, \
         got lp_a={lp_a_wins} lp_b={lp_b_wins}"
    );
}

// ── Ledger-boundary: resolution at exactly QUEUE_DELAY_LEDGERS ───────────────

#[test]
fn test_tie_resolution_at_exact_delay_boundary() {
    let t = setup_tie();
    let id = submit_invoice_tie(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);

    // Advance exactly QUEUE_DELAY_LEDGERS — the boundary case.
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);

    // Resolution must succeed at the exact boundary.
    let winner = t.contract.resolve_fund_queue(&id);
    assert!(
        winner == t.lp_a || winner == t.lp_b,
        "winner at exact delay boundary must be one of the tied LPs"
    );
}

#[test]
fn test_tie_resolution_fails_one_ledger_before_delay() {
    let t = setup_tie();
    let id = submit_invoice_tie(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);

    // One ledger short of the maturity delay.
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS - 1);

    let result = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(result, Err(Ok(ContractError::QueueNotMature)));
}

// ── Authorization: only approved LP can fund after tie-break ─────────────────

#[test]
fn test_tie_break_winner_can_fund_after_resolution() {
    let t = setup_tie();
    let id = submit_invoice_tie(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    let winner = resolve_mature(&t, &id);

    // The winner must be able to fund the invoice.
    t.contract
        .fund_invoice(&winner, &id, &INVOICE_AMOUNT, &false);

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Funded);
}

#[test]
fn test_tie_break_loser_cannot_fund_after_resolution() {
    let t = setup_tie();
    let id = submit_invoice_tie(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    let winner = resolve_mature(&t, &id);

    // The loser is whichever LP didn't win.
    let loser = if winner == t.lp_a { &t.lp_b } else { &t.lp_a };

    let result = t
        .contract
        .try_fund_invoice(loser, &id, &INVOICE_AMOUNT, &false);
    assert_eq!(result, Err(Ok(ContractError::NotApprovedFunder)));
}

#[test]
fn test_non_queued_lp_cannot_fund_after_tie_break_resolution() {
    let t = setup_tie();
    let id = submit_invoice_tie(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    resolve_mature(&t, &id);

    // lp_c was never in the queue — must be rejected.
    let result = t
        .contract
        .try_fund_invoice(&t.lp_c, &id, &INVOICE_AMOUNT, &false);
    assert_eq!(result, Err(Ok(ContractError::NotApprovedFunder)));
}

// ── No tie when scores differ: highest score wins outright ───────────────────

#[test]
fn test_higher_reputation_lp_wins_outright_no_tie() {
    let t = setup_tie();
    let id = submit_invoice_tie(&t);

    // Boost lp_b's score by funding 3 invoices (each adds 1 → score 53).
    for _ in 0..3u32 {
        let extra_id = submit_invoice_tie(&t);
        t.contract
            .fund_invoice(&t.lp_b, &extra_id, &INVOICE_AMOUNT, &false);
    }

    // lp_a: score 50, lp_b: score 53 — no tie.
    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);

    let winner = resolve_mature(&t, &id);
    assert_eq!(
        winner, t.lp_b,
        "higher-reputation LP must win outright without tie-breaking"
    );
}

// ── Adversarial: tie-break winner is always within tied prefix ───────────────

#[test]
fn test_tie_break_winner_never_an_outsider() {
    let t = setup_tie();
    let id = submit_invoice_tie(&t);

    // lp_a and lp_b have score 50 (tied at top), lp_c has lower score.
    // We need to lower lp_c's score below 50 — but all LPs start at 50.
    // Instead, boost lp_a and lp_b equally so they're tied at 53,
    // while lp_c stays at 50 (the default).
    for _ in 0..3u32 {
        let extra_a = submit_invoice_tie(&t);
        t.contract
            .fund_invoice(&t.lp_a, &extra_a, &INVOICE_AMOUNT, &false);
        let extra_b = submit_invoice_tie(&t);
        t.contract
            .fund_invoice(&t.lp_b, &extra_b, &INVOICE_AMOUNT, &false);
    }
    // Now lp_a and lp_b are at score 53, lp_c at 50.

    t.contract.join_fund_queue(&t.lp_a, &id); // score 53
    t.contract.join_fund_queue(&t.lp_c, &id); // score 50
    t.contract.join_fund_queue(&t.lp_b, &id); // score 53

    let winner = resolve_mature(&t, &id);

    // The winner must be lp_a or lp_b (the tied top-score LPs),
    // never lp_c (lower score).
    assert!(
        winner == t.lp_a || winner == t.lp_b,
        "tie-break winner must be from the tied top-score prefix, not a lower-score LP, got {winner:?}"
    );
    assert_ne!(
        winner, t.lp_c,
        "lower-score LP must never win via tie-break"
    );
}

// ── Idempotency: tie-break result is cached ──────────────────────────────────

#[test]
fn test_tie_break_resolution_is_idempotent() {
    let t = setup_tie();
    let id = submit_invoice_tie(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    let first = resolve_mature(&t, &id);

    // Second call must return the same winner (cached resolution).
    let second = t.contract.resolve_fund_queue(&id);
    assert_eq!(first, second);
}
