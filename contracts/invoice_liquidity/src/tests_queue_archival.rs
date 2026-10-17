//! Issue #1012 — Archival tests for fund-queue state
//!
//! Exercises the lifecycle introduced by Issues #1011/#1013/#1014/#1015 for
//! `FundQueue(id)`, `FundQueueOpenedAt(id)`, and `QueueResolution(id)`:
//! retention (every write extends TTL well past the default), the
//! fail-closed guard when `FundQueueOpenedAt` is missing despite a
//! non-empty queue, the TTL-margin instrumentation, and the ledger-boundary
//! behavior of all of the above.
//!
//! # Before / after baseline
//!
//! **Before (#1011):** `invoice::save_fund_queue` /
//! `try_set_fund_queue_opened_at` / `save_queue_resolution` — the functions
//! `lib.rs` actually calls from `join_fund_queue` / `resolve_fund_queue` —
//! wrote these three keys with Soroban's default persistent TTL and never
//! called `extend_ttl`. A TTL-safe copy existed in `storage.rs` (Issue
//! #1091) but was never imported or called, so it provided no protection.
//!
//! **Before (#1013):** `resolve_fund_queue`'s maturity gate was
//! `if let Some(opened_at) = get_fund_queue_opened_at(...) { ... }`. A
//! missing `FundQueueOpenedAt` (e.g. because it had been archived per the
//! gap above) silently skipped the whole MEV-mitigation delay check instead
//! of failing — fail-open.
//!
//! **After:** `save_fund_queue` / `try_set_fund_queue_opened_at` /
//! `save_queue_resolution` each extend their key's TTL to
//! `FUND_QUEUE_TTL_THRESHOLD` / `FUND_QUEUE_TTL_EXTEND_TO` (1,000,000 /
//! 2,000,000 ledgers) on every write (`test_join_fund_queue_extends_*`,
//! `test_resolve_fund_queue_extends_queue_resolution_ttl`,
//! `test_second_join_refreshes_fund_queue_opened_at_ttl`), and a missing
//! `FundQueueOpenedAt` on a *non-empty* queue now reverts with
//! `ContractError::QueueStateArchived` instead of resolving unprotected
//! (`test_resolve_fund_queue_fails_closed_when_opened_at_archived`,
//! `test_resolve_fund_queue_archived_does_not_silently_resolve`). The
//! `storage.rs` dead copy from Issue #1091 has been removed (#1014), so
//! there is exactly one implementation to test against. `get_fund_queue_ttl_margins`
//! (#1015) lets this be observed proactively, before either failure mode is
//! ever hit (`test_ttl_margin_*`).

#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env,
};

use crate::constants::{
    FUND_QUEUE_TTL_EXTEND_TO, FUND_QUEUE_TTL_THRESHOLD, TTL_MIN_LEDGERS_BEFORE_UPGRADE,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30; // 30 days

struct ArchivalTestEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: TokenClient<'static>,
    freelancer: Address,
    payer: Address,
    lp_a: Address,
    lp_b: Address,
}

fn setup_archival() -> ArchivalTestEnv {
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

    for lp in [&lp_a, &lp_b] {
        token_admin.mint(lp, &(INVOICE_AMOUNT * 10));
    }
    token_admin.mint(&payer, &(INVOICE_AMOUNT * 10));

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    token_admin.mint(&contract.address, &(INVOICE_AMOUNT * 100));

    let xlm_admin = Address::generate(&env);
    let xlm_id = env.register_stellar_asset_contract_v2(xlm_admin);
    let xlm_addr = xlm_id.address();
    let eurc_addr = Address::generate(&env);

    contract.initialize(&usdc_admin, &usdc_addr, &eurc_addr, &xlm_addr);

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    ledger.sequence_number = 100;
    env.ledger().set(ledger);

    ArchivalTestEnv {
        env,
        contract,
        token,
        freelancer,
        payer,
        lp_a,
        lp_b,
    }
}

fn submit_invoice_archival(t: &ArchivalTestEnv) -> u64 {
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

/// Simulate a `FundQueueOpenedAt` entry that has been archived (TTL lapsed
/// and the host evicted it). The mock test host doesn't model ledger-close
/// driven eviction directly, but from the contract's point of view archival
/// and an explicit removal are indistinguishable: both make
/// `get_fund_queue_opened_at` return `None`. This is exactly the state
/// `resolve_fund_queue`'s fail-closed guard (#1013) must detect.
fn simulate_fund_queue_opened_at_archived(t: &ArchivalTestEnv, invoice_id: u64) {
    t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .persistent()
            .remove(&StorageKey::FundQueueOpenedAt(invoice_id));
    });
}

fn fund_queue_opened_at_ttl(t: &ArchivalTestEnv, invoice_id: u64) -> u32 {
    t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .persistent()
            .get_ttl(&StorageKey::FundQueueOpenedAt(invoice_id))
    })
}

fn fund_queue_ttl(t: &ArchivalTestEnv, invoice_id: u64) -> u32 {
    t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .persistent()
            .get_ttl(&StorageKey::FundQueue(invoice_id))
    })
}

fn queue_resolution_ttl(t: &ArchivalTestEnv, invoice_id: u64) -> u32 {
    t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .persistent()
            .get_ttl(&StorageKey::QueueResolution(invoice_id))
    })
}

// ─────────────────────────────────────────────────────────────────────────
// Retention (Issue #1011): writes extend TTL well past the default
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_join_fund_queue_extends_fund_queue_opened_at_ttl() {
    let t = setup_archival();
    let id = submit_invoice_archival(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);

    let ttl = fund_queue_opened_at_ttl(&t, id);
    assert!(
        ttl >= FUND_QUEUE_TTL_THRESHOLD,
        "FundQueueOpenedAt TTL must be extended to at least {FUND_QUEUE_TTL_THRESHOLD} ledgers, got {ttl}"
    );
}

#[test]
fn test_join_fund_queue_extends_fund_queue_ttl() {
    let t = setup_archival();
    let id = submit_invoice_archival(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);

    let ttl = fund_queue_ttl(&t, id);
    assert!(
        ttl >= FUND_QUEUE_TTL_THRESHOLD,
        "FundQueue TTL must be extended to at least {FUND_QUEUE_TTL_THRESHOLD} ledgers, got {ttl}"
    );
}

#[test]
fn test_resolve_fund_queue_extends_queue_resolution_ttl() {
    let t = setup_archival();
    let id = submit_invoice_archival(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    t.contract.resolve_fund_queue(&id);

    let ttl = queue_resolution_ttl(&t, id);
    assert!(
        ttl >= FUND_QUEUE_TTL_THRESHOLD,
        "QueueResolution TTL must be extended to at least {FUND_QUEUE_TTL_THRESHOLD} ledgers, got {ttl}"
    );
}

#[test]
fn test_second_join_refreshes_fund_queue_opened_at_ttl() {
    let t = setup_archival();
    let id = submit_invoice_archival(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);

    // Advance far enough that the remaining TTL drops below
    // FUND_QUEUE_TTL_THRESHOLD (so the next write is guaranteed to trigger
    // an actual bump, not a no-op) without exhausting it entirely.
    let advance_below_threshold = FUND_QUEUE_TTL_EXTEND_TO - FUND_QUEUE_TTL_THRESHOLD + 100_000;
    advance_ledgers(&t.env, advance_below_threshold);
    let ttl_before_second_join = fund_queue_opened_at_ttl(&t, id);
    assert!(
        ttl_before_second_join < FUND_QUEUE_TTL_THRESHOLD,
        "test setup must leave TTL below the threshold before the second join"
    );

    // A second LP joining must refresh the TTL back up, even though
    // `FundQueueOpenedAt`'s *value* is untouched (still anchored to lp_a's
    // join) — this is the mechanism that keeps a long-lived, still-active
    // queue from ever drifting toward expiry.
    t.contract.join_fund_queue(&t.lp_b, &id);
    let ttl_after_second_join = fund_queue_opened_at_ttl(&t, id);
    assert!(
        ttl_after_second_join >= FUND_QUEUE_TTL_THRESHOLD,
        "second join must refresh FundQueueOpenedAt TTL back to at least {FUND_QUEUE_TTL_THRESHOLD}, got {ttl_after_second_join}"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Fail-closed on archival (Issue #1013)
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_resolve_fund_queue_fails_closed_when_opened_at_archived() {
    let t = setup_archival();
    let id = submit_invoice_archival(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS); // would otherwise be mature
    simulate_fund_queue_opened_at_archived(&t, id);

    let result = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(result, Err(Ok(ContractError::QueueStateArchived)));
}

#[test]
fn test_resolve_fund_queue_archived_does_not_silently_resolve() {
    let t = setup_archival();
    let id = submit_invoice_archival(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    simulate_fund_queue_opened_at_archived(&t, id);

    let _ = t.contract.try_resolve_fund_queue(&id);

    // The queue must remain unresolved — the pre-#1013 bug would have
    // silently picked a winner here instead of reverting.
    let resolution = t
        .env
        .as_contract(&t.contract.address, || crate::invoice::get_queue_resolution(&t.env, id));
    assert!(
        resolution.is_none(),
        "an archived queue must not resolve a winner"
    );
}

#[test]
fn test_resolve_fund_queue_distinguishes_archived_from_merely_immature() {
    // Regression guard: a *present* FundQueueOpenedAt that simply hasn't
    // matured yet must still return QueueNotMature, not QueueStateArchived —
    // the fail-closed path must not fire for the ordinary, expected case.
    let t = setup_archival();
    let id = submit_invoice_archival(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    // No archival simulated, no ledger advance — queue is immature only.
    let result = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(result, Err(Ok(ContractError::QueueNotMature)));
}

#[test]
fn test_resolve_fund_queue_empty_queue_is_not_funded_not_archived() {
    // Regression guard: an invoice that never had any LP join must still
    // get NotFunded — the archived-state fail-closed error is only
    // reachable once the queue is confirmed non-empty.
    let t = setup_archival();
    let id = submit_invoice_archival(&t);

    let result = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(result, Err(Ok(ContractError::NotFunded)));
}

// ─────────────────────────────────────────────────────────────────────────
// TTL margin instrumentation (Issue #1015) + ledger-boundary cases
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_ttl_margins_are_none_when_queue_never_opened() {
    let t = setup_archival();
    let id = submit_invoice_archival(&t);

    let margins = t.contract.get_fund_queue_ttl_margins(&id);
    assert!(margins.fund_queue_ttl.is_none());
    assert!(margins.fund_queue_margin.is_none());
    assert!(margins.fund_queue_opened_at_ttl.is_none());
    assert!(margins.fund_queue_opened_at_margin.is_none());
    assert!(margins.queue_resolution_ttl.is_none());
    assert!(margins.queue_resolution_margin.is_none());
}

#[test]
fn test_ttl_margin_is_positive_right_after_join() {
    let t = setup_archival();
    let id = submit_invoice_archival(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);

    let margins = t.contract.get_fund_queue_ttl_margins(&id);
    let margin = margins
        .fund_queue_opened_at_margin
        .expect("margin must be present once the queue has opened");
    assert_eq!(
        margin,
        (FUND_QUEUE_TTL_EXTEND_TO as i64) - (TTL_MIN_LEDGERS_BEFORE_UPGRADE as i64)
    );
}

#[test]
fn test_ttl_margin_crosses_zero_boundary_as_ledgers_advance_without_refresh() {
    let t = setup_archival();
    let id = submit_invoice_archival(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);

    // Advance exactly far enough that the remaining TTL equals
    // TTL_MIN_LEDGERS_BEFORE_UPGRADE — margin must land exactly on 0.
    let advance_to_zero_margin = FUND_QUEUE_TTL_EXTEND_TO - TTL_MIN_LEDGERS_BEFORE_UPGRADE;
    advance_ledgers(&t.env, advance_to_zero_margin);
    let margins_at_zero = t.contract.get_fund_queue_ttl_margins(&id);
    assert_eq!(margins_at_zero.fund_queue_opened_at_margin, Some(0));

    // One more ledger — margin must go negative, signalling the queue has
    // drifted past the safety threshold the pre-upgrade guard (#1082)
    // enforces, before it ever reaches actual archival.
    advance_ledgers(&t.env, 1);
    let margins_past_zero = t.contract.get_fund_queue_ttl_margins(&id);
    assert_eq!(margins_past_zero.fund_queue_opened_at_margin, Some(-1));
}

// ─────────────────────────────────────────────────────────────────────────
// Authorization: get_fund_queue_ttl_margins is Access: Anyone — confirm it
// doesn't gate on a specific signer identity. (resolve_fund_queue is also
// Access: Anyone, but takes no caller argument at all, so there is no
// identity to vary there — the fail-closed tests above already cover its
// only behavior dimension.)
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_get_fund_queue_ttl_margins_is_callable_by_any_observer() {
    // An off-chain monitor with no stake in the invoice (not LP, payer, or
    // freelancer) must be able to read TTL margins — the whole point of
    // Issue #1015 is proactive, permissionless observability.
    let t = setup_archival();
    let id = submit_invoice_archival(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);

    let margins = t.contract.get_fund_queue_ttl_margins(&id);
    assert!(
        margins.fund_queue_opened_at_ttl.is_some(),
        "margins must be readable with no special authorization"
    );
}
