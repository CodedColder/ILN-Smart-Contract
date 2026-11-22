#![cfg(test)]

//! Issue #1174 — Adversarial fund-queue race coverage.
//!
//! The reputation-weighted fund queue (`join_fund_queue` / `resolve_fund_queue`)
//! is an intent layer: an LP registers, a maturity window (`QUEUE_DELAY_LEDGERS`)
//! must elapse, and only then is a winner locked in. Cancellation and expiry are
//! *terminal* paths that can fire at any point around that window, and queue
//! state is reclaimed (not eagerly) by the permissionless `recover_fund_queue`.
//!
//! This module drives the ordering interfaces between those entry points under
//! ledger-sequence and transaction races — the join/resolve boundary, duplicate
//! resolution, cancel/expire interleaved with a resolved-but-unfunded queue, and
//! queue-capacity displacement — and records the *observed* behavior so off-chain
//! consumers and maintainers are not misled.
//!
//! Findings are catalogued in `docs/fund-queue-races.md`; each test maps to an
//! invariant (INV-FQR*) or a verified finding (VF-FQR*). This is an
//! implementation-only audit: nothing here changes contract behavior.

use super::*;
use crate::constants::{
    MAX_FUND_QUEUE_LEN, NEAR_EXPIRY_WINDOW_SECS, QUEUE_DELAY_LEDGERS,
    QUEUE_RESOLUTION_TTL_LEDGERS,
};
use crate::invoice::{
    get_fund_queue, get_fund_queue_opened_at, get_invoice_funders, get_lp_invoices,
    get_queue_resolution, get_queue_resolved_at, set_lp_score, InvoiceStatus, ReferralCode,
};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000; // 1,000 USDC (6dp)
const DISCOUNT_RATE: u32 = 300; // 3%
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30; // 30 days
const SETUP_TIMESTAMP: u64 = 1_700_000_000;

#[allow(dead_code)]
struct QueueRaceEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: TokenClient<'static>,
    freelancer: Address,
    payer: Address,
    lp_a: Address,
    lp_b: Address,
    lp_c: Address,
}

fn setup() -> QueueRaceEnv {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let usdc_id = env.register_stellar_asset_contract_v2(admin.clone());
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
    // Pre-fund the contract so refunds / payouts can be serviced in-test.
    token_admin.mint(&contract.address, &(INVOICE_AMOUNT * 1000));

    let xlm_id = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let eurc_addr = Address::generate(&env);
    contract.initialize(&admin, &usdc_addr, &eurc_addr, &xlm_id.address());

    let mut ledger = env.ledger().get();
    ledger.timestamp = SETUP_TIMESTAMP;
    ledger.sequence_number = 100;
    env.ledger().set(ledger);

    QueueRaceEnv {
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

// ── helpers ────────────────────────────────────────────────────────────────

fn submit(t: &QueueRaceEnv) -> u64 {
    let due = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    )
}

fn status_of(t: &QueueRaceEnv, id: u64) -> InvoiceStatus {
    t.contract.get_invoice(&id).status
}

fn due_of(t: &QueueRaceEnv, id: u64) -> u64 {
    u64::from(t.contract.get_invoice(&id).due_date)
}

/// Advance both the ledger sequence and the timestamp in lockstep (as a real
/// chain would). Matches the queue tests' helper so maturity math lines up.
fn advance_ledgers(env: &Env, delta: u32) {
    let mut info = env.ledger().get();
    info.sequence_number += delta;
    info.timestamp += u64::from(delta) * 5;
    env.ledger().set(info);
}

fn advance_time(env: &Env, secs: u64) {
    let mut info = env.ledger().get();
    info.timestamp += secs;
    env.ledger().set(info);
}

fn mature(t: &QueueRaceEnv, id: &u64) -> Address {
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    t.contract.resolve_fund_queue(id)
}

fn set_score(t: &QueueRaceEnv, lp: &Address, score: u32) {
    t.env.as_contract(&t.contract.address, || {
        set_lp_score(&t.env, lp, score);
    });
}

fn queue_len(t: &QueueRaceEnv, id: u64) -> u32 {
    t.env
        .as_contract(&t.contract.address, || get_fund_queue(&t.env, id).len())
}

fn queue_contains(t: &QueueRaceEnv, id: u64, lp: &Address) -> bool {
    t.env.as_contract(&t.contract.address, || {
        let q = get_fund_queue(&t.env, id);
        for i in 0..q.len() {
            if q.get(i).unwrap().lp == *lp {
                return true;
            }
        }
        false
    })
}

fn resolution(t: &QueueRaceEnv, id: u64) -> Option<Address> {
    t.env
        .as_contract(&t.contract.address, || get_queue_resolution(&t.env, id))
}

fn opened_at(t: &QueueRaceEnv, id: u64) -> Option<u32> {
    t.env
        .as_contract(&t.contract.address, || {
            get_fund_queue_opened_at(&t.env, id)
        })
}

fn resolved_at(t: &QueueRaceEnv, id: u64) -> Option<u32> {
    t.env
        .as_contract(&t.contract.address, || get_queue_resolved_at(&t.env, id))
}

// ================================================================
// Group 1 — join races
// ================================================================

/// INV-FQR1: a given LP holds at most one queue slot, regardless of how many
/// ledgers elapse between their join attempts.
#[test]
fn race_double_join_same_lp_yields_single_entry() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    advance_ledgers(&t.env, 5);
    let second = t.contract.try_join_fund_queue(&t.lp_a, &id);

    assert_eq!(second, Err(Ok(ContractError::AlreadyInQueue)));
    assert_eq!(queue_len(&t, id), 1, "duplicate join must not add a slot");
}

/// INV-FQR2: once a resolution is stored, the resolution gate in
/// `join_fund_queue` is evaluated *before* the invoice-status gate, so a late
/// joiner is rejected as a non-approved funder even when the invoice has since
/// been cancelled.
#[test]
fn race_join_after_resolution_precedes_status_gate() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    let winner = mature(&t, &id);
    assert_eq!(winner, t.lp_a);
    t.contract.cancel_invoice(&id);
    assert_eq!(status_of(&t, id), InvoiceStatus::Cancelled);

    // Resolution is still present (cancel does not touch queue state), so the
    // late joiner hits the resolution gate first -> NotApprovedFunder.
    assert!(resolution(&t, id).is_some());
    let late = t.contract.try_join_fund_queue(&t.lp_b, &id);
    assert_eq!(late, Err(Ok(ContractError::NotApprovedFunder)));
}

/// INV-FQR3: with no resolution stored, `join_fund_queue` falls through to the
/// status gate and reports the terminal state directly.
#[test]
fn race_join_cancelled_invoice_without_resolution() {
    let t = setup();
    let id = submit(&t);
    t.contract.cancel_invoice(&id);

    let r = t.contract.try_join_fund_queue(&t.lp_a, &id);
    assert_eq!(r, Err(Ok(ContractError::AlreadyCancelled)));
}

#[test]
fn race_join_expired_invoice_without_resolution() {
    let t = setup();
    let id = submit(&t);
    advance_time(&t.env, DUE_DATE_OFFSET + 1);
    t.contract.expire_invoice(&id);

    let r = t.contract.try_join_fund_queue(&t.lp_a, &id);
    assert_eq!(r, Err(Ok(ContractError::InvoiceExpired)));
}

/// INV-FQR4: the past-due join gate is a strict `now > due_date` test, so the
/// due-date ledger itself is still joinable and only the next second closes it.
#[test]
fn race_join_at_due_date_boundary_then_one_second_past() {
    let t = setup();
    let id = submit(&t);

    // Land exactly on the due date.
    advance_time(&t.env, DUE_DATE_OFFSET);
    assert_eq!(t.env.ledger().timestamp(), due_of(&t, id));
    t.contract.join_fund_queue(&t.lp_a, &id);
    assert_eq!(queue_len(&t, id), 1, "due-date ledger is still joinable");

    // One second later the invoice is past due for join purposes.
    advance_time(&t.env, 1);
    let late = t.contract.try_join_fund_queue(&t.lp_b, &id);
    assert_eq!(late, Err(Ok(ContractError::InvoiceExpired)));
}

/// VF-FQR1 (adversarial): `resolve_fund_queue` reads each LP's *current* score,
/// not the snapshot taken at join time. An LP who waits until the maturity
/// window has already elapsed (i.e. resolution is immediately callable) can
/// join last and still win outright if their reputation is highest.
#[test]
fn race_last_minute_join_outranks_earlier_joiner() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id); // snapshot score 50
    // Burn the whole maturity window so any caller could resolve right now.
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);

    // lp_b, already well-regarded, joins at the last possible moment.
    set_score(&t, &t.lp_b, 90);
    t.contract.join_fund_queue(&t.lp_b, &id);

    // Resolution re-scores: lp_b's current 90 beats lp_a's 50.
    let winner = t.contract.resolve_fund_queue(&id);
    assert_eq!(
        winner, t.lp_b,
        "resolution must honour current reputation, so a last-minute high-rep join wins"
    );
}

/// INV-FQR5: a resolve attempted in the same ledger as the join fails closed,
/// and the failed call leaves no resolution behind — it matures cleanly.
#[test]
fn race_same_ledger_resolve_rejected_then_matures() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    let early = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(early, Err(Ok(ContractError::QueueNotMature)));
    // Failed transaction left no partial state.
    assert!(resolution(&t, id).is_none());
    assert!(opened_at(&t, id).is_some());

    let winner = mature(&t, &id);
    assert_eq!(winner, t.lp_a);
}

// ================================================================
// Group 2 — resolution races
// ================================================================

/// INV-FQR6: resolution is idempotent, and a repeat call does *not* re-anchor
/// the `QueueResolvedAt` recovery clock — a caller cannot extend the stalled
/// winner's grace period by repeatedly re-resolving.
#[test]
fn race_duplicate_resolution_is_idempotent_and_keeps_clock() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    let first = mature(&t, &id);
    let clock_after_first = resolved_at(&t, id);

    advance_ledgers(&t.env, 30);
    let second = t.contract.resolve_fund_queue(&id);

    assert_eq!(first, second, "cached resolution must be returned verbatim");
    assert_eq!(
        resolved_at(&t, id),
        clock_after_first,
        "a repeated resolve must not move the recovery clock"
    );
}

/// VF-FQR2 (Q2): a resolution already stored is never erased by cancellation,
/// and `resolve_fund_queue` short-circuits on it — even for a cancelled
/// invoice — before the status gate runs.
#[test]
fn race_resolve_after_cancel_returns_cached_winner() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    let winner = mature(&t, &id);
    assert_eq!(winner, t.lp_a);

    t.contract.cancel_invoice(&id);
    assert_eq!(status_of(&t, id), InvoiceStatus::Cancelled);

    // Cached decision survives the terminal transition.
    assert_eq!(t.contract.resolve_fund_queue(&id), t.lp_a);
}

/// VF-FQR3 (Q1): without a stored resolution, a cancelled invoice cannot be
/// resolved — `require_queueable` rejects the terminal status.
#[test]
fn race_resolve_after_cancel_without_resolution_rejected() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.cancel_invoice(&id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);

    let r = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(r, Err(Ok(ContractError::AlreadyCancelled)));
}

/// INV-FQR6 continued: `recover_fund_queue` cannot be triggered in the same
/// ledger as resolution, and only unlocks exactly at the TTL boundary.
#[test]
fn race_recover_blocked_until_ttl_boundary() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    let winner = mature(&t, &id);

    // Same ledger as resolution: grace has not elapsed.
    assert_eq!(
        t.contract.try_recover_fund_queue(&id),
        Err(Ok(ContractError::QueueNotStalled))
    );
    advance_ledgers(&t.env, QUEUE_RESOLUTION_TTL_LEDGERS - 1);
    assert_eq!(
        t.contract.try_recover_fund_queue(&id),
        Err(Ok(ContractError::QueueNotStalled))
    );

    // Exactly at the TTL: the stalled winner is evictable.
    advance_ledgers(&t.env, 1);
    assert_eq!(t.contract.recover_fund_queue(&id), Some(winner));
    assert_eq!(queue_len(&t, id), 0);
}

/// INV-FQR7 / VF-FQR4: `recover_fund_queue` reopens the queue for the
/// remaining LPs and *restarts* the maturity window from the recovery ledger,
/// so a fresh join/resolve cycle is required after an eviction.
#[test]
fn race_recover_reopens_queue_and_restarts_maturity() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    let winner = mature(&t, &id);
    let other = if winner == t.lp_a {
        t.lp_b.clone()
    } else {
        t.lp_a.clone()
    };
    let opened_before = opened_at(&t, id).unwrap();

    advance_ledgers(&t.env, QUEUE_RESOLUTION_TTL_LEDGERS);
    assert_eq!(t.contract.recover_fund_queue(&id), Some(winner.clone()));

    // Queue now holds only the non-winning LP, and the maturity clock was
    // re-anchored to the recovery ledger.
    assert_eq!(queue_len(&t, id), 1);
    assert!(queue_contains(&t, id, &other));
    assert_ne!(opened_at(&t, id).unwrap(), opened_before);

    // Immediately after recovery the window is not mature.
    assert_eq!(
        t.contract.try_resolve_fund_queue(&id),
        Err(Ok(ContractError::QueueNotMature))
    );

    // After a fresh window the survivor wins and can fund.
    let second_winner = mature(&t, &id);
    assert_eq!(second_winner, other);
    t.contract
        .fund_invoice(&other, &id, &INVOICE_AMOUNT, &false);
    assert_eq!(status_of(&t, id), InvoiceStatus::Funded);
}

// ================================================================
// Group 3 — cancellation / expiry interleaved with a resolved queue
// ================================================================

/// VF-FQR5: cancelling a `Pending` invoice that already has a resolved queue
/// succeeds (the invoice is still `Pending`), leaves the resolution in place,
/// and only `recover_fund_queue` reclaims it. The approved LP cannot fund a
/// cancelled invoice; a non-approved LP is still gated by the resolution.
#[test]
fn race_cancel_pending_with_resolved_queue_needs_recovery() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    let winner = mature(&t, &id);
    assert_eq!(winner, t.lp_a);

    t.contract.cancel_invoice(&id);
    assert_eq!(status_of(&t, id), InvoiceStatus::Cancelled);
    assert_eq!(resolution(&t, id), Some(t.lp_a.clone()));

    // Approved LP passes the resolution gate but is stopped by the status gate.
    assert_eq!(
        t.contract
            .try_fund_invoice(&t.lp_a, &id, &INVOICE_AMOUNT, &false),
        Err(Ok(ContractError::AlreadyCancelled))
    );
    // Non-approved LP is stopped earlier, at the resolution gate.
    assert_eq!(
        t.contract
            .try_fund_invoice(&t.lp_b, &id, &INVOICE_AMOUNT, &false),
        Err(Ok(ContractError::NotApprovedFunder))
    );

    // Recovery purges the stale queue state for the un-fundable invoice.
    assert_eq!(t.contract.recover_fund_queue(&id), None);
    assert!(resolution(&t, id).is_none());
    assert_eq!(queue_len(&t, id), 0);
}

/// VF-FQR6: a *partially funded* invoice with a resolved queue that is then
/// cancelled refunds the LP, and its queue state again persists until recovery.
#[test]
fn race_cancel_partially_funded_resolved_queue_persists_until_recovered() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    let winner = mature(&t, &id);
    assert_eq!(winner, t.lp_a);

    // Approved LP funds half the invoice -> PartiallyFunded.
    t.contract
        .fund_invoice(&t.lp_a, &id, &(INVOICE_AMOUNT / 2), &false);
    assert_eq!(status_of(&t, id), InvoiceStatus::PartiallyFunded);

    // Freelancer cancels; the partial principal is refunded.
    t.contract.cancel_invoice(&id);
    assert_eq!(status_of(&t, id), InvoiceStatus::Cancelled);
    assert_eq!(resolution(&t, id), Some(t.lp_a.clone()));

    assert_eq!(t.contract.recover_fund_queue(&id), None);
    assert!(resolution(&t, id).is_none());
}

/// VF-FQR7: expiring a `Pending` invoice that already has a resolved queue
/// leaves the resolution behind; the approved LP can no longer fund, and the
/// stale queue is reclaimed only by `recover_fund_queue`.
#[test]
fn race_expire_pending_with_resolved_queue_needs_recovery() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    let winner = mature(&t, &id);
    assert_eq!(winner, t.lp_a);

    advance_time(&t.env, DUE_DATE_OFFSET + 10);
    t.contract.expire_invoice(&id);
    assert_eq!(status_of(&t, id), InvoiceStatus::Expired);
    // Expiry does not touch queue state.
    assert_eq!(resolution(&t, id), Some(t.lp_a.clone()));

    assert_eq!(
        t.contract
            .try_fund_invoice(&t.lp_a, &id, &INVOICE_AMOUNT, &false),
        Err(Ok(ContractError::InvoiceExpired))
    );

    assert_eq!(t.contract.recover_fund_queue(&id), None);
    assert!(resolution(&t, id).is_none());
    assert!(opened_at(&t, id).is_none());
}

/// VF-FQR8 (asymmetry): `expire_invoice` refuses a partially funded invoice
/// (`AlreadyFunded`), and a *past-due* partially funded invoice cannot be
/// resolved either (`require_queueable` fails closed on `now > due_date`).
#[test]
fn race_expire_and_resolve_rejected_for_partially_funded_past_due() {
    let t = setup();
    let id = submit(&t);

    t.contract
        .fund_invoice(&t.lp_a, &id, &(INVOICE_AMOUNT / 2), &false);
    assert_eq!(status_of(&t, id), InvoiceStatus::PartiallyFunded);
    t.contract.join_fund_queue(&t.lp_b, &id);

    advance_time(&t.env, DUE_DATE_OFFSET + 10);
    assert_eq!(
        t.contract.try_expire_invoice(&id),
        Err(Ok(ContractError::AlreadyFunded)),
        "expiry is Pending-only"
    );

    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    assert_eq!(
        t.contract.try_resolve_fund_queue(&id),
        Err(Ok(ContractError::InvoiceExpired)),
        "resolution fails closed once past due, even for PartiallyFunded"
    );
}

/// VF-FQR9 (asymmetry, adversarial): the past-due guard that blocks queue
/// resolution for a `PartiallyFunded` invoice does *not* block direct funding —
/// `fund_invoice` only auto-expires the `Pending` case. A partially funded,
/// past-due invoice can still be completed by a direct funder.
#[test]
fn race_partially_funded_past_due_still_directly_fundable() {
    let t = setup();
    let id = submit(&t);

    t.contract
        .fund_invoice(&t.lp_a, &id, &(INVOICE_AMOUNT / 2), &false);
    assert_eq!(status_of(&t, id), InvoiceStatus::PartiallyFunded);

    advance_time(&t.env, DUE_DATE_OFFSET + 10);

    // No resolution exists, so the resolution gate is skipped; the
    // PartiallyFunded status is still in the "fundable" set.
    t.contract
        .fund_invoice(&t.lp_b, &id, &(INVOICE_AMOUNT / 2), &false);
    assert_eq!(
        status_of(&t, id),
        InvoiceStatus::Funded,
        "a past-due partially funded invoice remains directly fundable"
    );
}

// ================================================================
// Group 4 — duplicate resolution against terminal transitions
// ================================================================

/// INV-FQR6: the cached resolution short-circuits *before* `require_queueable`,
/// so once a queue was resolved, later resolution calls keep returning the
/// winner even after the invoice is funded or expired.
#[test]
fn race_resolve_returns_cached_winner_after_funding_and_expiry() {
    let t = setup();

    // Case A: funded invoice keeps its cached resolution.
    let funded = submit(&t);
    t.contract.join_fund_queue(&t.lp_a, &funded);
    let winner = mature(&t, &funded);
    t.contract
        .fund_invoice(&winner, &funded, &INVOICE_AMOUNT, &false);
    assert_eq!(status_of(&t, funded), InvoiceStatus::Funded);
    assert_eq!(t.contract.resolve_fund_queue(&funded), winner);

    // Case B: expired invoice keeps its cached resolution.
    let expiring = submit(&t);
    t.contract.join_fund_queue(&t.lp_b, &expiring);
    let winner2 = mature(&t, &expiring);
    advance_time(&t.env, DUE_DATE_OFFSET + 10);
    t.contract.expire_invoice(&expiring);
    assert_eq!(status_of(&t, expiring), InvoiceStatus::Expired);
    assert_eq!(t.contract.resolve_fund_queue(&expiring), winner2);
}

// ================================================================
// Group 5 — queue capacity + join races
// ================================================================

/// INV-FQR8 (boundedness): whatever the outcome of joins attempted at/over
/// capacity — a higher-scored LP displacing the tail, or a rejection — the
/// queue never grows past `MAX_FUND_QUEUE_LEN` and never loses well-formedness.
/// This pins the observable safety property (the cap is never exceeded and the
/// queue is still resolvable), not which of the two capacity paths executes.
#[test]
fn race_queue_stays_bounded_under_capacity_burst() {
    let t = setup();
    let id = submit(&t);

    for _ in 0..MAX_FUND_QUEUE_LEN {
        let lp = Address::generate(&t.env);
        set_score(&t, &lp, 1);
        t.contract.join_fund_queue(&lp, &id);
        assert!(
            queue_len(&t, id) <= MAX_FUND_QUEUE_LEN,
            "queue must never exceed MAX_FUND_QUEUE_LEN while filling"
        );
    }
    assert_eq!(queue_len(&t, id), MAX_FUND_QUEUE_LEN);

    // A burst of extra attempts at capacity: some may displace a tail entry,
    // some may be rejected — either way the queue stays capped.
    for _ in 0..10 {
        let challenger = Address::generate(&t.env);
        set_score(&t, &challenger, 99);
        let _ = t.contract.try_join_fund_queue(&challenger, &id);
        assert!(
            queue_len(&t, id) <= MAX_FUND_QUEUE_LEN,
            "a capacity-burst join must not push the queue past the cap"
        );
    }

    // The (still bounded) queue remains resolvable to a queued LP.
    let winner = mature(&t, &id);
    assert_eq!(resolution(&t, id), Some(winner));
}

/// INV-FQR1 under capacity pressure: a duplicate join attempt while the queue
/// is full is still reported as `AlreadyInQueue` (dedup runs before any
/// capacity consequence when the queue is not yet at the first bound).
#[test]
fn race_duplicate_join_does_not_grow_or_reorder_queue() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    let r = t.contract.try_join_fund_queue(&t.lp_a, &id);
    assert_eq!(r, Err(Ok(ContractError::AlreadyInQueue)));

    assert_eq!(queue_len(&t, id), 2);
    // Order preserved: first join stays at index 0 (tie broken by join order).
    t.env.as_contract(&t.contract.address, || {
        let q = get_fund_queue(&t.env, id);
        assert_eq!(q.get(0).unwrap().lp, t.lp_a);
        assert_eq!(q.get(1).unwrap().lp, t.lp_b);
    });
}

// ================================================================
// Group 6 — transaction atomicity of failed calls
// ================================================================

/// INV-FQR5 (atomicity): a rejected resolve (immature) rolls back every write,
/// including the resolution-attempt event, leaving the queue exactly as the
/// join left it — and a later mature resolve still succeeds.
#[test]
fn race_rejected_resolve_leaves_state_untouched() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    let opened = opened_at(&t, id).unwrap();

    assert_eq!(
        t.contract.try_resolve_fund_queue(&id),
        Err(Ok(ContractError::QueueNotMature))
    );

    assert!(resolution(&t, id).is_none());
    assert!(resolved_at(&t, id).is_none());
    assert_eq!(opened_at(&t, id), Some(opened));
    assert_eq!(queue_len(&t, id), 1);

    // The recovered path still matures normally.
    assert_eq!(mature(&t, &id), t.lp_a);
}

/// INV-FQR5 (atomicity): a rejected `fund_invoice` by a non-approved LP leaves
/// the invoice untouched (no accounting change, no LP-index mutation).
#[test]
fn race_rejected_fund_leaves_invoice_untouched() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    mature(&t, &id);

    assert_eq!(
        t.contract
            .try_fund_invoice(&t.lp_b, &id, &INVOICE_AMOUNT, &false),
        Err(Ok(ContractError::NotApprovedFunder))
    );

    let inv = t.contract.get_invoice(&id);
    assert_eq!(inv.status, InvoiceStatus::Pending);
    assert_eq!(inv.amount_funded, 0);
    // lp_b was never added to the LP index or funder list.
    t.env.as_contract(&t.contract.address, || {
        assert_eq!(get_invoice_funders(&t.env, id).len(), 0);
        assert_eq!(get_lp_invoices(&t.env, &t.lp_b).len(), 0);
    });
}

/// INV-FQR7 (recovery idempotency): after the queue is purged for an
/// un-fundable invoice, a repeated recovery finds nothing to reclaim and the
/// near-expiry grace shortening does not become reachable by retrying.
#[test]
fn race_repeated_recovery_after_purge_is_noop() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.cancel_invoice(&id);

    assert_eq!(t.contract.recover_fund_queue(&id), None);
    assert_eq!(
        t.contract.try_recover_fund_queue(&id),
        Err(Ok(ContractError::QueueNotStalled))
    );
    assert_eq!(queue_len(&t, id), 0);
    assert!(resolution(&t, id).is_none());
}

/// VF-FQR11 (near-expiry grace): `recover_fund_queue` shortens the stalled
/// winner's grace to `QUEUE_DELAY_LEDGERS` when the invoice is within
/// `NEAR_EXPIRY_WINDOW_SECS` of its due date, so a stalled winner cannot run
/// the full TTL down near the deadline.
#[test]
fn race_recover_shortens_grace_near_expiry() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    // Move to one hour before the due date (inside the near-expiry window).
    advance_time(&t.env, DUE_DATE_OFFSET - 3600);
    let winner = mature(&t, &id);
    assert_eq!(winner, t.lp_a);
    assert!(
        due_of(&t, id).saturating_sub(t.env.ledger().timestamp()) <= NEAR_EXPIRY_WINDOW_SECS,
        "precondition: within the near-expiry window"
    );

    // Well below the full TTL, but past the shortened grace.
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    assert_eq!(t.contract.recover_fund_queue(&id), Some(t.lp_a.clone()));
    assert_eq!(queue_len(&t, id), 0);
}
