//! Issue #1017 — Prevent cleanup of unresolved economic state
//!
//! `resolve_fund_queue` picks and announces a reputation-weighted "winning"
//! LP (`FundQueueResolved`) — a real economic decision about who gets to
//! fund the invoice. `join_fund_queue` carefully gates every call on
//! `invoice.status` (an LP cannot even join once the invoice has left
//! `Pending`/`PartiallyFunded`), but `resolve_fund_queue` had no such gate
//! at all: it never loaded the invoice or looked at its status.
//!
//! That meant `cancel_invoice` (Submitter-only) or `expire_invoice`
//! (Anyone, once `due_date` has passed) — neither of which touch
//! `FundQueue`/`FundQueueOpenedAt`/`QueueResolution` — could "clean up" an
//! invoice while its fund queue was still non-empty and unresolved, and the
//! queue's still-unresolved economic state would survive that cleanup
//! completely untouched: anyone could still call `resolve_fund_queue`
//! afterward and have it pick a winner for an invoice that can never
//! actually be funded, stranding a misleading `QueueResolution` record and
//! `FundQueueResolved` event on dead state.
//!
//! # Before / after baseline
//!
//! **Before:** `resolve_fund_queue(invoice_id)` succeeded (or failed only on
//! `QueueNotMature`/`QueueStateArchived`/empty-queue grounds) regardless of
//! `invoice.status` — cancelling or expiring an invoice never stopped its
//! queue from later being resolved.
//!
//! **After:** `resolve_fund_queue` loads the invoice and mirrors
//! `join_fund_queue`'s exact status match. Once the invoice has left
//! `Pending`/`PartiallyFunded`, a *not-yet-resolved* queue can no longer be
//! resolved — the call fails with the same status-specific error
//! `join_fund_queue` would already give a new joiner
//! (`AlreadyCancelled`, `InvoiceExpired`, `AlreadyFunded`, `AlreadyPaid`,
//! `InvoiceDefaulted`, `InvoiceAppealed`, `InvoiceDisputed`). A queue that
//! was already resolved *before* cleanup is unaffected: the pre-existing
//! "already resolved" cache check runs first and keeps returning that
//! historical winner regardless of the invoice's current status — cleanup
//! cannot erase a decision that was already finalized.

#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30; // 30 days

struct GateTestEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: TokenClient<'static>,
    freelancer: Address,
    payer: Address,
    lp_a: Address,
}

fn setup_gate() -> GateTestEnv {
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

    token_admin.mint(&lp_a, &(INVOICE_AMOUNT * 10));
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

    GateTestEnv {
        env,
        contract,
        token,
        freelancer,
        payer,
        lp_a,
    }
}

fn submit_invoice_gate(t: &GateTestEnv) -> u64 {
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

/// Force an invoice directly into a given status, bypassing whatever normal
/// entry point would otherwise be required to reach it. Used only for the
/// statuses that are unreachable via the normal flow while a queue is still
/// unresolved (see the module doc comment on `test_resolve_fails_closed_for_...`
/// below for why), so the gate's defense-in-depth can still be exercised.
fn force_invoice_status(t: &GateTestEnv, id: u64, status: InvoiceStatus) {
    t.env.as_contract(&t.contract.address, || {
        let mut invoice = crate::invoice::load_invoice(&t.env, id);
        invoice.status = status;
        crate::invoice::save_invoice(&t.env, &invoice);
    });
}

// ─────────────────────────────────────────────────────────────────────────
// Reachable via normal flow: Cancelled, Expired
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_resolve_fails_after_cancel_invoice_with_unresolved_queue() {
    let t = setup_gate();
    let id = submit_invoice_gate(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    // Queue is non-empty but neither mature nor resolved yet.
    t.contract.cancel_invoice(&id);

    let result = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(
        result,
        Err(Ok(ContractError::AlreadyCancelled)),
        "resolving a cancelled invoice's queue must fail, not pick a winner for dead state"
    );
}

#[test]
fn test_resolve_fails_after_expire_invoice_with_unresolved_queue() {
    let t = setup_gate();
    let id = submit_invoice_gate(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);

    let invoice = t.contract.get_invoice(&id);
    let mut ledger = t.env.ledger().get();
    ledger.timestamp = u64::from(invoice.due_date) + 1;
    t.env.ledger().set(ledger);

    t.contract.expire_invoice(&id);

    let result = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(
        result,
        Err(Ok(ContractError::InvoiceExpired)),
        "resolving an expired invoice's queue must fail, not pick a winner for dead state"
    );
}

#[test]
fn test_resolve_still_succeeds_when_cancelled_after_resolution_already_happened() {
    // The gate must not retroactively invalidate a resolution that already
    // finalized while the invoice was still Pending — cleanup afterward
    // doesn't erase history, because cancel_invoice/expire_invoice never
    // touch QueueResolution.
    let t = setup_gate();
    let id = submit_invoice_gate(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    let approved = t.contract.resolve_fund_queue(&id);

    t.contract.cancel_invoice(&id);

    let approved_again = t.contract.resolve_fund_queue(&id);
    assert_eq!(
        approved, approved_again,
        "a pre-existing resolution must keep returning the same winner after cleanup"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Regression guard: the ordinary success path is unaffected
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_resolve_still_succeeds_for_pending_invoice() {
    let t = setup_gate();
    let id = submit_invoice_gate(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);

    let result = t.contract.try_resolve_fund_queue(&id);
    assert!(
        result.is_ok(),
        "an ordinary Pending invoice's mature queue must still resolve, got {:?}",
        result
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Defense in depth: Funded / Paid / Defaulted / Disputed / Appealed are not
// reachable via the normal flow while a queue is still unresolved for the
// SAME invoice — fund_invoice's QueueUnresolved gate means an invoice can
// only reach Funded (and therefore any status beyond it) *after*
// resolve_fund_queue has already succeeded, at which point the "already
// resolved" cache check above would short-circuit instead of reaching this
// gate. These statuses are tested here by forcing them directly, exactly
// mirroring join_fund_queue's own defensive completeness over every status
// variant rather than leaving a silent gap for however this situation might
// arise in the future (e.g. a new entry point, a migration, or a bug
// elsewhere).
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_resolve_fails_closed_for_every_non_fundable_status() {
    let statuses = [
        (InvoiceStatus::Funded, ContractError::AlreadyFunded),
        (InvoiceStatus::Paid, ContractError::AlreadyPaid),
        (InvoiceStatus::Defaulted, ContractError::InvoiceDefaulted),
        (InvoiceStatus::Appealed, ContractError::InvoiceAppealed),
        (InvoiceStatus::Disputed, ContractError::InvoiceDisputed),
    ];

    for (status, expected_err) in statuses {
        let t = setup_gate();
        let id = submit_invoice_gate(&t);
        t.contract.join_fund_queue(&t.lp_a, &id);

        force_invoice_status(&t, id, status.clone());

        let result = t.contract.try_resolve_fund_queue(&id);
        assert_eq!(
            result,
            Err(Ok(expected_err)),
            "resolve_fund_queue must reject status {:?}",
            status
        );
    }
}
