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

// ── Issues #1009 / #1010: queue-entry TTL through resolution ─────────────────
//
// Baseline (before): `save_fund_queue`, `save_queue_resolution` and
// `try_set_fund_queue_opened_at` wrote with no `extend_ttl`, so each entry
// lived only for the network's default persistent TTL while the invoice it
// belongs to was extended to 2_000_000 ledgers — a resolution could expire
// before the deferred `fund_invoice`, silently re-opening funding to any LP.
// After: all three records are bumped together to QUEUE_TTL_EXTEND_TO_LEDGERS
// on every write and on every read.

use crate::constants::{
    QUEUE_DELAY_LEDGERS, QUEUE_TTL_EXTEND_TO_LEDGERS, QUEUE_TTL_THRESHOLD_LEDGERS,
};
use crate::storage::DataKey;

fn ttl(t: &QueueTestEnv, key: &DataKey) -> u32 {
    t.env.as_contract(&t.contract.address, || {
        t.env.storage().persistent().get_ttl(key)
    })
}

fn exists(t: &QueueTestEnv, key: &DataKey) -> bool {
    t.env
        .as_contract(&t.contract.address, || t.env.storage().persistent().has(key))
}

fn all_queue_keys(id: u64) -> [DataKey; 3] {
    [
        DataKey::FundQueue(id),
        DataKey::FundQueueOpenedAt(id),
        DataKey::QueueResolution(id),
    ]
}

#[test]
fn test_1009_join_extends_queue_and_opened_at_ttl() {
    let t = setup_queue();
    let id = submit_invoice(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);

    assert_eq!(ttl(&t, &DataKey::FundQueue(id)), QUEUE_TTL_EXTEND_TO_LEDGERS);
    assert_eq!(
        ttl(&t, &DataKey::FundQueueOpenedAt(id)),
        QUEUE_TTL_EXTEND_TO_LEDGERS
    );
}

#[test]
fn test_1009_resolution_extends_all_queue_records_together() {
    let t = setup_queue();
    let id = submit_invoice(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);
    resolve_queue_mature(&t, &id);

    // The resolution record is written at resolution time and the queue /
    // opened-at siblings were last bumped at join, QUEUE_DELAY_LEDGERS ago.
    // None of them may be left on the network's default TTL.
    assert_eq!(
        ttl(&t, &DataKey::QueueResolution(id)),
        QUEUE_TTL_EXTEND_TO_LEDGERS
    );
    for key in all_queue_keys(id) {
        assert!(ttl(&t, &key) >= QUEUE_TTL_EXTEND_TO_LEDGERS - QUEUE_DELAY_LEDGERS, "{key:?}");
    }
}

/// The deferred read in `fund_invoice` keeps the resolution (and its sibling
/// entries) alive well past the original write's expiry.
#[test]
fn test_1010_deferred_fund_read_keeps_resolution_alive_past_original_expiry() {
    let t = setup_queue();
    let id = submit_invoice(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);
    resolve_queue_mature(&t, &id);

    // Move past the threshold so a read must re-extend, but stay inside TTL.
    advance_ledgers(&t.env, QUEUE_TTL_EXTEND_TO_LEDGERS - QUEUE_TTL_THRESHOLD_LEDGERS + 1);
    assert!(ttl(&t, &DataKey::QueueResolution(id)) < QUEUE_TTL_THRESHOLD_LEDGERS);

    // `get_fund_queue_opened_at` / resolution reads are what resolve/join/fund do.
    assert_eq!(t.contract.resolve_fund_queue(&id), t.lp_a);
    for key in all_queue_keys(id) {
        assert_eq!(ttl(&t, &key), QUEUE_TTL_EXTEND_TO_LEDGERS, "{key:?}");
    }

    // Cross the *original* expiry: without the bump the resolution is gone.
    advance_ledgers(&t.env, QUEUE_TTL_THRESHOLD_LEDGERS);
    for key in all_queue_keys(id) {
        assert!(exists(&t, &key), "{key:?} expired despite being read");
    }
}

/// Ledger boundary: `extend_ttl` bumps when the remaining TTL is at or below
/// the threshold. One ledger above it a read is a no-op (no wasted write);
/// at the threshold the read bumps back to the full window.
#[test]
fn test_1009_extension_threshold_boundary() {
    let t = setup_queue();
    let id = submit_invoice(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);
    resolve_queue_mature(&t, &id);
    let key = DataKey::QueueResolution(id);

    // remaining TTL == threshold + 1: not extended.
    advance_ledgers(
        &t.env,
        QUEUE_TTL_EXTEND_TO_LEDGERS - QUEUE_TTL_THRESHOLD_LEDGERS - 1,
    );
    assert_eq!(ttl(&t, &key), QUEUE_TTL_THRESHOLD_LEDGERS + 1);
    t.contract.resolve_fund_queue(&id);
    assert_eq!(ttl(&t, &key), QUEUE_TTL_THRESHOLD_LEDGERS + 1);

    // remaining TTL == threshold: extended back to the full window.
    advance_ledgers(&t.env, 1);
    assert_eq!(ttl(&t, &key), QUEUE_TTL_THRESHOLD_LEDGERS);
    t.contract.resolve_fund_queue(&id);
    assert_eq!(ttl(&t, &key), QUEUE_TTL_EXTEND_TO_LEDGERS);
}

/// Adversarial: an unapproved LP probing a resolved queue is rejected and
/// cannot change who is approved; the approved LP can still fund, and a queue
/// that was never opened creates no queue records (no state growth).
#[test]
fn test_1010_unapproved_probe_rejected_and_approved_lp_still_funds() {
    let t = setup_queue();
    let id = submit_invoice(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);
    resolve_queue_mature(&t, &id);

    advance_ledgers(&t.env, QUEUE_TTL_EXTEND_TO_LEDGERS - QUEUE_TTL_THRESHOLD_LEDGERS + 1);
    assert_eq!(
        t.contract.try_fund_invoice(&t.lp_b, &id, &INVOICE_AMOUNT, &false),
        Err(Ok(ContractError::NotApprovedFunder))
    );
    assert_eq!(
        t.contract.try_join_fund_queue(&t.lp_b, &id),
        Err(Ok(ContractError::NotApprovedFunder))
    );
    assert_eq!(t.contract.resolve_fund_queue(&id), t.lp_a);

    let untouched = submit_invoice(&t);
    for key in all_queue_keys(untouched) {
        assert!(!exists(&t, &key), "{key:?} must not exist for an unused queue");
    }
}
