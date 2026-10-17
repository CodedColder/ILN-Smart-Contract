//! Tests for Issue #34 — Reputation-weighted LP priority queue
//!
//! Scenarios covered:
//!  - Single LP joins queue and resolves (happy path)
//!  - Highest-reputation LP wins when multiple LPs compete
//!  - Tie broken by first-come-first-served
//!  - Only the approved LP can fund after queue resolution
//!  - LP not in queue can still fund when no queue exists (backward compat)
//!  - Duplicate queue join rejected
//!  - join_fund_queue on non-existent invoice rejected
//!  - resolve_fund_queue on empty queue rejected
//!  - lp_score starts at neutral 50

#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{storage::Persistent as _, Address as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

struct QueueTestEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: TokenClient<'static>,
    freelancer: Address,
    payer: Address,
    lp_a: Address,
    lp_b: Address,
    lp_c: Address,
}

fn setup_queue() -> QueueTestEnv {
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

    for lp in [&lp_a, &lp_b, &lp_c] {
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
    env.ledger().set(ledger);

    QueueTestEnv {
        env,
        contract,
        token,
        freelancer,
        payer,
        lp_a,
        lp_b,
        lp_c,
    }
}

fn submit_invoice(t: &QueueTestEnv) -> u64 {
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

/// Issue #MEV-1 added a maturity delay to resolve_fund_queue after these
/// tests were written (while the file was orphaned); queue resolution in
/// tests must advance past QUEUE_DELAY_LEDGERS first.
fn advance_ledgers(env: &Env, delta: u32) {
    let mut info = env.ledger().get();
    info.sequence_number += delta;
    info.timestamp += u64::from(delta) * 5;
    env.ledger().set(info);
}

fn resolve_queue_mature(t: &QueueTestEnv, id: &u64) -> Address {
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    t.contract.resolve_fund_queue(id)
}

// ── lp_score ─────────────────────────────────────────────────────────────────

#[test]
fn test_lp_score_defaults_to_50() {
    let t = setup_queue();
    assert_eq!(t.contract.lp_score(&t.lp_a), 50);
}

// ── join_fund_queue ───────────────────────────────────────────────────────────

#[test]
fn test_single_lp_joins_queue_successfully() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    // No error means success; the LP is in the queue.
}

#[test]
pub(crate) fn test_duplicate_queue_join_rejected() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);

    let result = t.contract.try_join_fund_queue(&t.lp_a, &id);
    assert_eq!(result, Err(Ok(ContractError::AlreadyInQueue)));
}

#[test]
fn test_join_queue_nonexistent_invoice_fails() {
    let t = setup_queue();

    let result = t.contract.try_join_fund_queue(&t.lp_a, &999);
    assert_eq!(result, Err(Ok(ContractError::InvoiceNotFound)));
}

#[test]
fn test_join_queue_after_resolution_rejected() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    resolve_queue_mature(&t, &id);

    // Late arrival cannot join once queue is resolved.
    let result = t.contract.try_join_fund_queue(&t.lp_b, &id);
    assert_eq!(result, Err(Ok(ContractError::NotApprovedFunder)));
}

// ── resolve_fund_queue ────────────────────────────────────────────────────────

#[test]
fn test_resolve_queue_returns_only_lp_when_one_entry() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    let winner = resolve_queue_mature(&t, &id);

    assert_eq!(winner, t.lp_a);
}

#[test]
fn test_resolve_queue_empty_fails() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    let result = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(result, Err(Ok(ContractError::NotFunded)));
}

#[test]
pub(crate) fn test_resolve_queue_is_idempotent() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    let first = resolve_queue_mature(&t, &id);
    let second = resolve_queue_mature(&t, &id);

    assert_eq!(first, second);
}

// ── Reputation ordering ───────────────────────────────────────────────────────

#[test]
pub(crate) fn test_highest_reputation_lp_wins_queue() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    // Manually boost lp_b's score by funding + getting paid on several invoices.
    // We do this by directly calling fund_invoice + mark_paid on extra invoices
    // to drive up lp_b's lp_score.

    // Simulate lp_b having a higher score than default by funding 3 invoices.
    for _ in 0..3u32 {
        let extra_id = submit_invoice(&t);
        t.contract
            .fund_invoice(&t.lp_b, &extra_id, &INVOICE_AMOUNT, &false);
        // Each full fund adds 1 to lp_score → lp_b will be at 53.
    }

    // lp_a: score = 50 (default), lp_b: score = 53, lp_c: score = 50
    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    t.contract.join_fund_queue(&t.lp_c, &id);

    let winner = resolve_queue_mature(&t, &id);
    assert_eq!(winner, t.lp_b, "Highest-reputation LP should win");
}

#[test]
pub(crate) fn test_tie_broken_by_first_come_first_served() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    // lp_a and lp_b both have default score 50.
    t.contract.join_fund_queue(&t.lp_a, &id); // joins first
    t.contract.join_fund_queue(&t.lp_b, &id); // joins second

    let winner = resolve_queue_mature(&t, &id);
    // Issue #708: ties are now randomized, so join order no longer decides —
    // but the winner must still be one of the tied LPs.
    assert!(
        winner == t.lp_a || winner == t.lp_b,
        "winner on tie must be one of the tied LPs"
    );
}

// ── fund_invoice integration ──────────────────────────────────────────────────

#[test]
fn test_approved_lp_can_fund_after_queue_resolution() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    resolve_queue_mature(&t, &id);

    // lp_a is approved — should fund successfully.
    t.contract
        .fund_invoice(&t.lp_a, &id, &INVOICE_AMOUNT, &false);

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Funded);
}

#[test]
pub(crate) fn test_non_approved_lp_cannot_fund_after_queue_resolution() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    resolve_queue_mature(&t, &id);

    // lp_b is NOT the approved LP.
    let result = t
        .contract
        .try_fund_invoice(&t.lp_b, &id, &INVOICE_AMOUNT, &false);
    assert_eq!(result, Err(Ok(ContractError::NotApprovedFunder)));
}

#[test]
fn test_fund_invoice_without_queue_works_normally() {
    // Backward-compatibility: if no queue is used, fund_invoice is first-come-first-served.
    let t = setup_queue();
    let id = submit_invoice(&t);

    // No queue join, no resolution — lp_a funds directly.
    t.contract
        .fund_invoice(&t.lp_a, &id, &INVOICE_AMOUNT, &false);

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Funded);
}

#[test]
fn test_lp_score_increases_after_successful_fund() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    let score_before = t.contract.lp_score(&t.lp_a);
    t.contract
        .fund_invoice(&t.lp_a, &id, &INVOICE_AMOUNT, &false);
    let score_after = t.contract.lp_score(&t.lp_a);

    assert_eq!(score_after, score_before + 1);
}

#[test]
fn test_full_queue_lifecycle_with_payout() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);

    let winner = resolve_queue_mature(&t, &id);
    // Both at score 50, lp_a wins tie.
    assert_eq!(winner, t.lp_a);

    t.contract
        .fund_invoice(&t.lp_a, &id, &INVOICE_AMOUNT, &false);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Paid);
}

// ================================================================
// Tests for LP priority queue edge cases (Issue #486)
// ================================================================

#[test]
fn test_tie_breaking_three_lps_first_come_wins() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    // All three LPs have default score 50.
    t.contract.join_fund_queue(&t.lp_a, &id); // 1st
    t.contract.join_fund_queue(&t.lp_b, &id); // 2nd
    t.contract.join_fund_queue(&t.lp_c, &id); // 3rd

    let winner = resolve_queue_mature(&t, &id);
    // Issue #708 randomized tie-breaking: any tied LP may win, but the
    // winner can never be an outsider.
    assert!(
        winner == t.lp_a || winner == t.lp_b || winner == t.lp_c,
        "winner on three-way tie must be one of the queued LPs"
    );
}

#[test]
fn test_resolve_empty_queue_rejected() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    let result = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(result, Err(Ok(ContractError::NotFunded)));
}

#[test]
fn test_join_queue_after_invoice_resolved_rejected() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    resolve_queue_mature(&t, &id);

    let result = t.contract.try_join_fund_queue(&t.lp_c, &id);
    assert_eq!(result, Err(Ok(ContractError::NotApprovedFunder)));
}

#[test]
fn test_join_queue_for_funded_invoice_rejected() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    t.contract
        .fund_invoice(&t.lp_a, &id, &INVOICE_AMOUNT, &false);

    let result = t.contract.try_join_fund_queue(&t.lp_b, &id);
    assert_eq!(result, Err(Ok(ContractError::AlreadyFunded)));
}

#[test]
fn test_join_queue_for_paid_invoice_rejected() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    t.contract
        .fund_invoice(&t.lp_a, &id, &INVOICE_AMOUNT, &false);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    let result = t.contract.try_join_fund_queue(&t.lp_b, &id);
    assert_eq!(result, Err(Ok(ContractError::AlreadyPaid)));
}

#[test]
fn test_join_queue_for_cancelled_invoice_rejected() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    t.contract.cancel_invoice(&id);

    let result = t.contract.try_join_fund_queue(&t.lp_a, &id);
    assert_eq!(result, Err(Ok(ContractError::AlreadyCancelled)));
}

#[test]
fn test_join_queue_for_expired_invoice_rejected() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    // Advance time past due date to make it expirable.
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += DUE_DATE_OFFSET + 1;
    t.env.ledger().set(ledger);

    t.contract.expire_invoice(&id);

    let result = t.contract.try_join_fund_queue(&t.lp_a, &id);
    assert_eq!(result, Err(Ok(ContractError::InvoiceExpired)));
}

#[test]
fn test_non_approved_lp_cannot_fund_with_4_arg_signature() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    resolve_queue_mature(&t, &id);

    // lp_b is not the approved LP — should fail with 4-arg fund_invoice.
    let result = t
        .contract
        .try_fund_invoice(&t.lp_b, &id, &INVOICE_AMOUNT, &false);
    assert_eq!(result, Err(Ok(ContractError::NotApprovedFunder)));
}

#[test]
fn test_queue_lifecycle_with_different_scores() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    // Boost lp_c's score by having it fund several invoices.
    for _ in 0..5u32 {
        let extra_id = submit_invoice(&t);
        t.contract
            .fund_invoice(&t.lp_c, &extra_id, &INVOICE_AMOUNT, &false);
    }
    // lp_c score is now 55, lp_a=50, lp_b=50.

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    t.contract.join_fund_queue(&t.lp_c, &id);

    let winner = resolve_queue_mature(&t, &id);
    assert_eq!(
        winner, t.lp_c,
        "highest score should win regardless of join order"
    );

    t.contract
        .fund_invoice(&t.lp_c, &id, &INVOICE_AMOUNT, &false);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Paid);
}

#[test]
fn test_resolve_queue_only_once_stored() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    let first_winner = resolve_queue_mature(&t, &id);

    // Adding a new LP with higher score after resolution doesn't change result.
    let boosted_id = submit_invoice(&t);
    for _ in 0..10u32 {
        let extra = submit_invoice(&t);
        t.contract
            .fund_invoice(&t.lp_b, &extra, &INVOICE_AMOUNT, &false);
    }
    t.contract
        .fund_invoice(&t.lp_b, &boosted_id, &INVOICE_AMOUNT, &false);

    // Can't join after resolution anyway, but resolve is idempotent.
    let second_winner = resolve_queue_mature(&t, &id);
    assert_eq!(first_winner, second_winner);
}

// ── Sorted Queue Optimization Tests ────────────────────────────────────────

/// Verify that queue is maintained in sorted order (highest score first).
/// This is the key optimization: resolve_fund_queue can return immediately.
#[test]
pub(crate) fn test_queue_maintains_sorted_order_after_joins() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    // Set different scores for each LP to ensure sorting.
    // lp_a: score 50 (initial)
    // lp_b: score 50 (initial)
    // lp_c: score 50 (initial)

    // Boost lp_c to 55
    for _ in 0..5u32 {
        let extra_id = submit_invoice(&t);
        t.contract
            .fund_invoice(&t.lp_c, &extra_id, &INVOICE_AMOUNT, &false);
    }

    // Join in order: A (50), B (50), C (55)
    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    t.contract.join_fund_queue(&t.lp_c, &id);

    // Resolve should return C (highest score) regardless of join order
    let winner = resolve_queue_mature(&t, &id);
    assert_eq!(winner, t.lp_c, "Highest score should be selected");
}

/// Test that LPs joining in reverse score order are still sorted correctly.
#[test]
pub(crate) fn test_queue_sorted_even_when_joining_in_reverse_order() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    // Create different scores:
    // Boost lp_c to 60
    for _ in 0..10u32 {
        let extra_id = submit_invoice(&t);
        t.contract
            .fund_invoice(&t.lp_c, &extra_id, &INVOICE_AMOUNT, &false);
    }

    // Boost lp_b to 55
    for _ in 0..5u32 {
        let extra_id = submit_invoice(&t);
        t.contract
            .fund_invoice(&t.lp_b, &extra_id, &INVOICE_AMOUNT, &false);
    }
    // lp_a stays at 50

    // Join in reverse score order: C (60), B (55), A (50)
    t.contract.join_fund_queue(&t.lp_c, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    t.contract.join_fund_queue(&t.lp_a, &id);

    // Even though they joined in descending order, resolve should still work
    let winner = resolve_queue_mature(&t, &id);
    assert_eq!(winner, t.lp_c, "Highest score (lp_c) should win");
}

/// Test that inserting an LP with mid-range score places it correctly.
#[test]
pub(crate) fn test_queue_sorted_when_inserting_mid_range_scores() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    // Set up scores: lp_a=50, lp_b=60, lp_c=55
    for _ in 0..10u32 {
        let extra_id = submit_invoice(&t);
        t.contract
            .fund_invoice(&t.lp_b, &extra_id, &INVOICE_AMOUNT, &false);
    }
    for _ in 0..5u32 {
        let extra_id = submit_invoice(&t);
        t.contract
            .fund_invoice(&t.lp_c, &extra_id, &INVOICE_AMOUNT, &false);
    }

    // Join in order: A (50), B (60), C (55)
    // Queue should be sorted as: B (60), C (55), A (50)
    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    t.contract.join_fund_queue(&t.lp_c, &id);

    let winner = resolve_queue_mature(&t, &id);
    assert_eq!(winner, t.lp_b, "Highest score (lp_b=60) should win");
}

/// Test that duplicate prevention still works after sorting optimization.
#[test]
pub(crate) fn test_duplicate_prevention_with_sorted_queue() {
    let t = setup_queue();
    let id = submit_invoice(&t);

    // First join should succeed
    t.contract.join_fund_queue(&t.lp_a, &id);

    // Duplicate join should fail
    let res = t.contract.try_join_fund_queue(&t.lp_a, &id);
    assert_eq!(res, Err(Ok(ContractError::AlreadyInQueue)));

    // Other LPs can still join
    t.contract.join_fund_queue(&t.lp_b, &id);
    let winner = resolve_queue_mature(&t, &id);
    assert!(winner == t.lp_a || winner == t.lp_b);
}

// ── Issues #1019–#1022: delayed resolution, recovery, bounds ─────────────────

fn set_score(t: &QueueTestEnv, lp: &Address, score: u32) {
    t.env.as_contract(&t.contract.address, || {
        crate::storage::set_lp_score(&t.env, lp, score);
    });
}

fn queue_len(t: &QueueTestEnv, id: u64) -> u32 {
    t.env.as_contract(&t.contract.address, || {
        crate::storage::get_fund_queue(&t.env, id).len()
    })
}

fn advance_time(env: &Env, secs: u64) {
    let mut info = env.ledger().get();
    info.timestamp += secs;
    env.ledger().set(info);
}

#[test]
fn queue_full_rejects_join_and_stays_bounded() {
    let t = setup_queue();
    let id = submit_invoice(&t);
    for _ in 0..crate::constants::MAX_FUND_QUEUE_LEN {
        t.contract.join_fund_queue(&Address::generate(&t.env), &id);
    }
    let r = t.contract.try_join_fund_queue(&t.lp_a, &id);
    assert_eq!(r, Err(Ok(ContractError::QueueFull)));
    assert_eq!(queue_len(&t, id), crate::constants::MAX_FUND_QUEUE_LEN);
}

#[test]
fn join_rejected_for_pending_invoice_past_due_date() {
    let t = setup_queue();
    let id = submit_invoice(&t);
    advance_time(&t.env, DUE_DATE_OFFSET + 1);
    let r = t.contract.try_join_fund_queue(&t.lp_a, &id);
    assert_eq!(r, Err(Ok(ContractError::InvoiceExpired)));
}

#[test]
fn resolve_rejected_if_invoice_cancelled_after_join() {
    let t = setup_queue();
    let id = submit_invoice(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.cancel_invoice(&id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    let r = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(r, Err(Ok(ContractError::AlreadyCancelled)));
    assert!(t.contract.try_recover_fund_queue(&id).is_ok());
}

#[test]
fn resolve_rejected_once_invoice_is_past_due() {
    let t = setup_queue();
    let id = submit_invoice(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);
    advance_time(&t.env, DUE_DATE_OFFSET + 1);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    let r = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(r, Err(Ok(ContractError::InvoiceExpired)));
}

#[test]
fn delayed_resolution_uses_current_score_not_stale_snapshot() {
    let t = setup_queue();
    let id = submit_invoice(&t);
    t.contract.join_fund_queue(&t.lp_a, &id); // snapshot 50, first in line
    t.contract.join_fund_queue(&t.lp_b, &id); // snapshot 50
    set_score(&t, &t.lp_a, 5); // A's reputation collapses before resolution
    let winner = resolve_queue_mature(&t, &id);
    assert_eq!(winner, t.lp_b);
}

#[test]
fn recover_with_nothing_queued_fails() {
    let t = setup_queue();
    let id = submit_invoice(&t);
    let r = t.contract.try_recover_fund_queue(&id);
    assert_eq!(r, Err(Ok(ContractError::QueueNotStalled)));
    let r = t.contract.try_recover_fund_queue(&999);
    assert_eq!(r, Err(Ok(ContractError::InvoiceNotFound)));
}

#[test]
fn recover_before_resolution_fails() {
    let t = setup_queue();
    let id = submit_invoice(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);
    let r = t.contract.try_recover_fund_queue(&id);
    assert_eq!(r, Err(Ok(ContractError::QueueNotStalled)));
    assert_eq!(queue_len(&t, id), 1);
}

#[test]
fn recover_stalled_winner_at_ttl_boundary_and_reopen() {
    let t = setup_queue();
    let id = submit_invoice(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    let winner = resolve_queue_mature(&t, &id);
    let other = if winner == t.lp_a { t.lp_b.clone() } else { t.lp_a.clone() };

    // One ledger short of the TTL: still locked.
    advance_ledgers(&t.env, crate::constants::QUEUE_RESOLUTION_TTL_LEDGERS - 1);
    assert_eq!(
        t.contract.try_recover_fund_queue(&id),
        Err(Ok(ContractError::QueueNotStalled))
    );
    assert_eq!(
        t.contract.try_fund_invoice(&other, &id, &INVOICE_AMOUNT, &false),
        Err(Ok(ContractError::NotApprovedFunder))
    );

    // Exactly at the TTL: anyone may evict the stalled winner.
    advance_ledgers(&t.env, 1);
    assert_eq!(t.contract.recover_fund_queue(&id), Some(winner.clone()));
    assert_eq!(queue_len(&t, id), 1);

    // Maturity restarts for the remaining LP.
    assert_eq!(
        t.contract.try_resolve_fund_queue(&id),
        Err(Ok(ContractError::QueueNotMature))
    );
    assert_eq!(resolve_queue_mature(&t, &id), other);
    t.contract.fund_invoice(&other, &id, &INVOICE_AMOUNT, &false);
}

#[test]
fn recover_shortens_grace_when_near_expiry() {
    let t = setup_queue();
    let id = submit_invoice(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);
    // Move to 1 hour before the due date.
    advance_time(&t.env, DUE_DATE_OFFSET - 3600);
    let winner = resolve_queue_mature(&t, &id);
    assert_eq!(winner, t.lp_a);
    // Far below the full TTL but past QUEUE_DELAY_LEDGERS.
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    assert_eq!(t.contract.recover_fund_queue(&id), Some(t.lp_a.clone()));
    assert_eq!(queue_len(&t, id), 0);
}

#[test]
fn recover_purges_state_for_cancelled_and_expired_invoices() {
    let t = setup_queue();
    let cancelled = submit_invoice(&t);
    let expiring = submit_invoice(&t);
    for id in [cancelled, expiring] {
        t.contract.join_fund_queue(&t.lp_a, &id);
    }
    resolve_queue_mature(&t, &cancelled);
    t.contract.cancel_invoice(&cancelled);
    assert_eq!(t.contract.recover_fund_queue(&cancelled), None);

    advance_time(&t.env, DUE_DATE_OFFSET + 1);
    t.contract.expire_invoice(&expiring);
    assert_eq!(t.contract.recover_fund_queue(&expiring), None);

    for id in [cancelled, expiring] {
        assert_eq!(queue_len(&t, id), 0);
        t.env.as_contract(&t.contract.address, || {
            assert!(crate::storage::get_queue_resolution(&t.env, id).is_none());
            assert!(crate::storage::get_fund_queue_opened_at(&t.env, id).is_none());
            assert!(crate::storage::get_queue_resolved_at(&t.env, id).is_none());
        });
        // Idempotent: a second purge finds nothing to do.
        assert_eq!(
            t.contract.try_recover_fund_queue(&id),
            Err(Ok(ContractError::QueueNotStalled))
        );
    }
}
