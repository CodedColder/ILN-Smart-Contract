#![cfg(test)]

//! Issue #1173 — Audit cancellation and expiry accounting.
//!
//! `cancel_invoice` and `expire_invoice` are the two terminal paths that fire
//! *without* a settlement or default. This module verifies that:
//!   * cancellation cannot discard a *fully funded* obligation (it must go
//!     through settlement / default / dispute instead),
//!   * cancellation of a *partially funded* invoice refunds each LP exactly the
//!     principal they paid (net of discount) — value-neutral,
//!   * expiry is restricted to `Pending` invoices (no funded obligation),
//!   * neither path corrupts reputation accounting,
//!   * the fund queue / queue-resolution state is *not* implicitly cleared by
//!     cancel/expire — it is reclaimed by the permissionless
//!     `recover_fund_queue`.
//!
//! Findings are catalogued in `docs/cancellation-expiry-accounting.md`; each
//! test maps to an invariant (INV-CX*) or verified finding (VF-CX*) there.
//!
//! Implementation-only audit: nothing here changes contract behavior.

use super::*;
use crate::constants::QUEUE_DELAY_LEDGERS;
use crate::invoice::{
    get_fund_queue, get_invoice_funders, get_lp_invoices, get_queue_resolution, InvoiceStatus,
    ReferralCode,
};
use soroban_sdk::{
    testutils::{Address as _, Ledger, MockAuth, MockAuthInvoke},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env, IntoVal,
};

const INVOICE_AMOUNT: i128 = 10_000_000; // 10 USDC
const DISCOUNT_RATE: u32 = 300; // 3%
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30; // 30 days
const SETUP_TIMESTAMP: u64 = 1_700_000_000;

#[allow(dead_code)]
struct FixedTestEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: TokenClient<'static>,
    admin: Address,
    freelancer: Address,
    payer: Address,
    funder: Address,
}

fn setup() -> FixedTestEnv {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let usdc_id = env.register_stellar_asset_contract_v2(admin.clone());
    let usdc_addr = usdc_id.address();

    let token = TokenClient::new(&env, &usdc_addr);
    let token_admin = StellarAssetClient::new(&env, &usdc_addr);

    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let funder = Address::generate(&env);

    token_admin.mint(&funder, &(INVOICE_AMOUNT * 20));
    token_admin.mint(&payer, &(INVOICE_AMOUNT * 20));

    let contract_id = env.register(InvoiceLiquidityContract, ());
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    // Fund the contract so it can service refunds / payouts.
    token_admin.mint(&contract.address, &(INVOICE_AMOUNT * 100));

    let eurc_id = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let xlm_id = env.register_stellar_asset_contract_v2(Address::generate(&env));

    contract.initialize(&admin, &usdc_addr, &eurc_id.address(), &xlm_id.address());

    let mut ledger = env.ledger().get();
    ledger.timestamp = SETUP_TIMESTAMP;
    env.ledger().set(ledger);

    FixedTestEnv {
        env,
        contract,
        token,
        admin,
        freelancer,
        payer,
        funder,
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

fn submit(t: &FixedTestEnv) -> u64 {
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

fn fund(t: &FixedTestEnv, id: u64, amount: i128) {
    t.contract.fund_invoice(&t.funder, &id, &amount, &false);
}

fn status_of(t: &FixedTestEnv, id: u64) -> InvoiceStatus {
    t.contract.get_invoice(&id).status
}

fn due_of(t: &FixedTestEnv, id: u64) -> u64 {
    u64::from(t.contract.get_invoice(&id).due_date)
}

fn advance_past_due(t: &FixedTestEnv, id: u64) {
    let mut ledger = t.env.ledger().get();
    ledger.timestamp = due_of(t, id) + 10;
    t.env.ledger().set(ledger);
}

fn advance_ledger(t: &FixedTestEnv, delta: u32) {
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number = ledger.sequence_number.saturating_add(delta);
    t.env.ledger().set(ledger);
}

fn funder_list(t: &FixedTestEnv, id: u64) -> soroban_sdk::Vec<(Address, i128)> {
    t.env
        .as_contract(&t.contract.address, || get_invoice_funders(&t.env, id))
}

fn lp_invoices(t: &FixedTestEnv, lp: &Address) -> soroban_sdk::Vec<u64> {
    t.env
        .as_contract(&t.contract.address, || get_lp_invoices(&t.env, lp))
}

fn queue_len(t: &FixedTestEnv, id: u64) -> u32 {
    t.env
        .as_contract(&t.contract.address, || get_fund_queue(&t.env, id).len())
}

fn queue_resolution(t: &FixedTestEnv, id: u64) -> Option<Address> {
    t.env
        .as_contract(&t.contract.address, || get_queue_resolution(&t.env, id))
}

fn contains_id(v: &soroban_sdk::Vec<u64>, id: u64) -> bool {
    for i in 0..v.len() {
        if v.get(i).unwrap() == id {
            return true;
        }
    }
    false
}

// ================================================================
// cancel_invoice authorization + state gates — INV-CX1 / INV-CX2
// ================================================================

#[test]
fn test_cancel_requires_freelancer_authorization() {
    let t = setup();
    let id = submit(&t);
    let imposter = Address::generate(&t.env);

    t.env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &t.contract.address,
            fn_name: "cancel_invoice",
            args: (id,).into_val(&t.env),
            sub_invokes: &[],
        },
    }]);

    let res = t.contract.try_cancel_invoice(&id);
    assert!(res.is_err(), "cancel_invoice must require the freelancer's auth");
    assert_eq!(status_of(&t, id), InvoiceStatus::Pending);
}

#[test]
fn test_cancel_pending_invoice_succeeds() {
    let t = setup();
    let id = submit(&t);
    t.contract.cancel_invoice(&id);
    assert_eq!(status_of(&t, id), InvoiceStatus::Cancelled);
    assert!(
        funder_list(&t, id).is_empty(),
        "a never-funded invoice has no funder obligations to leave behind"
    );
}

#[test]
fn test_cancel_funded_invoice_rejected() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT);
    assert_eq!(status_of(&t, id), InvoiceStatus::Funded);

    let res = t.contract.try_cancel_invoice(&id);
    assert_eq!(
        res,
        Err(Ok(ContractError::AlreadyFunded)),
        "a fully funded obligation must not be cancellable away"
    );
    assert_eq!(status_of(&t, id), InvoiceStatus::Funded);
}

#[test]
fn test_cancel_paid_invoice_rejected() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    let res = t.contract.try_cancel_invoice(&id);
    assert_eq!(res, Err(Ok(ContractError::AlreadyPaid)));
}

#[test]
fn test_cancel_defaulted_invoice_rejected() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT);
    advance_past_due(&t, id);
    t.contract.claim_default(&t.funder, &id);

    let res = t.contract.try_cancel_invoice(&id);
    assert_eq!(res, Err(Ok(ContractError::InvoiceDefaulted)));
}

#[test]
fn test_cancel_appealed_invoice_rejected() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT);
    advance_past_due(&t, id);
    t.contract.claim_default(&t.funder, &id);
    t.contract.appeal_default(&id, &[9u8; 32].into());

    let res = t.contract.try_cancel_invoice(&id);
    assert_eq!(res, Err(Ok(ContractError::InvoiceAppealed)));
}

#[test]
fn test_cancel_disputed_invoice_rejected() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT / 2);
    t.contract.dispute_invoice(&id, &[3u8; 32].into());

    let res = t.contract.try_cancel_invoice(&id);
    assert_eq!(res, Err(Ok(ContractError::InvoiceDisputed)));
}

#[test]
fn test_cancel_expired_invoice_rejected() {
    let t = setup();
    let id = submit(&t);
    advance_past_due(&t, id);
    t.contract.expire_invoice(&id);

    let res = t.contract.try_cancel_invoice(&id);
    assert_eq!(res, Err(Ok(ContractError::InvoiceExpired)));
}

#[test]
fn test_cancel_twice_rejected() {
    let t = setup();
    let id = submit(&t);
    t.contract.cancel_invoice(&id);

    let res = t.contract.try_cancel_invoice(&id);
    assert_eq!(res, Err(Ok(ContractError::AlreadyCancelled)));
}

#[test]
fn test_cancel_missing_invoice_rejected() {
    let t = setup();
    let res = t.contract.try_cancel_invoice(&999);
    assert_eq!(res, Err(Ok(ContractError::InvoiceNotFound)));
}

#[test]
fn test_cancel_blocked_when_paused() {
    let t = setup();
    let id = submit(&t);
    t.contract.pause();

    let res = t.contract.try_cancel_invoice(&id);
    assert_eq!(res, Err(Ok(ContractError::ContractPaused)));
}

// ================================================================
// Cancellation accounting — INV-CX3 (value-neutral refund)
// ================================================================

#[test]
fn test_cancel_partially_funded_refunds_principal_value_neutral() {
    let t = setup();
    let id = submit(&t);

    let funder_before = t.token.balance(&t.funder);
    let contract_before = t.token.balance(&t.contract.address);

    fund(&t, id, INVOICE_AMOUNT / 2);
    assert_eq!(status_of(&t, id), InvoiceStatus::PartiallyFunded);
    assert!(t.token.balance(&t.funder) < funder_before, "LP paid in");

    t.contract.cancel_invoice(&id);

    assert_eq!(status_of(&t, id), InvoiceStatus::Cancelled);
    assert_eq!(
        t.token.balance(&t.funder),
        funder_before,
        "the LP must be refunded exactly the principal it paid"
    );
    assert_eq!(
        t.token.balance(&t.contract.address),
        contract_before,
        "cancellation must not retain any LP funds"
    );
}

#[test]
fn test_cancel_partially_funded_refunds_every_funder() {
    let t = setup();
    let id = submit(&t);

    let second_lp = Address::generate(&t.env);
    StellarAssetClient::new(&t.env, &t.token.address).mint(&second_lp, &(INVOICE_AMOUNT * 5));

    let first_before = t.token.balance(&t.funder);
    let second_before = t.token.balance(&second_lp);

    fund(&t, id, INVOICE_AMOUNT / 2);
    t.contract
        .fund_invoice(&second_lp, &id, &(INVOICE_AMOUNT / 4), &false);
    assert_eq!(funder_list(&t, id).len(), 2);

    t.contract.cancel_invoice(&id);

    assert_eq!(t.token.balance(&t.funder), first_before);
    assert_eq!(t.token.balance(&second_lp), second_before);
}

/// VF-CX1: cancellation refunds the principal but leaves the invoice's funded
/// metadata (`amount_funded`, the funder list) and each LP's invoice index
/// entry intact. The invoice is terminal, so this is stale-but-inert state, not
/// a double-refund vector.
#[test]
fn test_cancel_partially_funded_leaves_stale_funded_metadata() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT / 2);

    t.contract.cancel_invoice(&id);

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Cancelled);
    assert_eq!(
        invoice.amount_funded,
        INVOICE_AMOUNT / 2,
        "amount_funded is not reset on cancellation"
    );
    assert_eq!(
        funder_list(&t, id).len(),
        1,
        "the funder list is not cleared on cancellation"
    );
    assert!(
        contains_id(&lp_invoices(&t, &t.funder), id),
        "the LP's invoice index still references the cancelled invoice"
    );
}

// ================================================================
// expire_invoice authorization + state gates — INV-CX4
// ================================================================

#[test]
fn test_expire_is_permissionless() {
    let t = setup();
    let id = submit(&t);
    advance_past_due(&t, id);

    t.env.mock_auths(&[]);
    let res = t.contract.try_expire_invoice(&id);
    assert!(res.is_ok(), "expire_invoice must be callable by anyone");
    assert_eq!(status_of(&t, id), InvoiceStatus::Expired);
}

#[test]
fn test_expire_requires_past_due_date() {
    let t = setup();
    let id = submit(&t);

    // Exactly at the due date is not yet expired (strict `>`).
    let mut ledger = t.env.ledger().get();
    ledger.timestamp = due_of(&t, id);
    t.env.ledger().set(ledger);

    let res = t.contract.try_expire_invoice(&id);
    assert_eq!(res, Err(Ok(ContractError::NotYetDefaulted)));
    assert_eq!(status_of(&t, id), InvoiceStatus::Pending);
}

#[test]
fn test_expire_partially_funded_rejected() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT / 2);
    advance_past_due(&t, id);

    let res = t.contract.try_expire_invoice(&id);
    assert_eq!(
        res,
        Err(Ok(ContractError::AlreadyFunded)),
        "a partially funded invoice must not be expirable"
    );
}

#[test]
fn test_expire_funded_rejected() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT);
    advance_past_due(&t, id);

    let res = t.contract.try_expire_invoice(&id);
    assert_eq!(res, Err(Ok(ContractError::AlreadyFunded)));
}

#[test]
fn test_expire_paid_rejected() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    let res = t.contract.try_expire_invoice(&id);
    assert_eq!(res, Err(Ok(ContractError::AlreadyPaid)));
}

#[test]
fn test_expire_twice_rejected() {
    let t = setup();
    let id = submit(&t);
    advance_past_due(&t, id);
    t.contract.expire_invoice(&id);

    let res = t.contract.try_expire_invoice(&id);
    assert_eq!(res, Err(Ok(ContractError::InvoiceExpired)));
}

#[test]
fn test_expire_missing_invoice_rejected() {
    let t = setup();
    let res = t.contract.try_expire_invoice(&999);
    assert_eq!(res, Err(Ok(ContractError::InvoiceNotFound)));
}

// ================================================================
// Reputation accounting — INV-CX5
// ================================================================

#[test]
fn test_expire_does_not_penalize_reputation() {
    let t = setup();
    let id = submit(&t);
    let before = t.contract.get_reputation(&t.freelancer);
    assert_eq!(before.invoices_submitted, 1);

    advance_past_due(&t, id);
    t.contract.expire_invoice(&id);

    let after = t.contract.get_reputation(&t.freelancer);
    assert_eq!(after.invoices_submitted, 1, "submission is still counted");
    assert_eq!(
        after.invoices_defaulted, 0,
        "an unfunded expiry is no-fault and must not be a default"
    );
    assert_eq!(after.invoices_paid, 0);
}

#[test]
fn test_cancel_does_not_penalize_reputation() {
    let t = setup();
    let id = submit(&t);
    let before = t.contract.get_reputation(&t.freelancer);

    t.contract.cancel_invoice(&id);

    let after = t.contract.get_reputation(&t.freelancer);
    assert_eq!(after.invoices_defaulted, 0);
    assert_eq!(after.invoices_paid, 0);
    assert_eq!(after.invoices_submitted, before.invoices_submitted);
}

// ================================================================
// Queue state — INV-CX6 (not implicitly cleared)
// ================================================================

/// VF-CX2: cancel/expire leave the fund queue and queue resolution in place.
/// They are reclaimed only by the permissionless `recover_fund_queue`.
#[test]
fn test_cancel_does_not_clear_fund_queue_resolution() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.funder, &id);
    assert_eq!(queue_len(&t, id), 1);

    advance_ledger(&t, QUEUE_DELAY_LEDGERS);
    let approved = t.contract.resolve_fund_queue(&id);
    assert_eq!(queue_resolution(&t, id), Some(approved.clone()));

    t.contract.cancel_invoice(&id);

    assert_eq!(
        queue_resolution(&t, id),
        Some(approved),
        "cancellation does not clear the queue resolution"
    );
    assert_eq!(queue_len(&t, id), 1, "the fund queue survives cancellation");

    // The permissionless recovery path is what reclaims the storage.
    let recovered = t.contract.recover_fund_queue(&id);
    assert_eq!(recovered, Ok(None));
    assert_eq!(queue_len(&t, id), 0);
    assert_eq!(queue_resolution(&t, id), None);
}

#[test]
fn test_expire_leaves_fund_queue_until_recovered() {
    let t = setup();
    let id = submit(&t);

    t.contract.join_fund_queue(&t.funder, &id);
    assert_eq!(queue_len(&t, id), 1);

    advance_past_due(&t, id);
    t.contract.expire_invoice(&id);
    assert_eq!(status_of(&t, id), InvoiceStatus::Expired);

    assert_eq!(
        queue_len(&t, id),
        1,
        "expiry does not clear the fund queue"
    );

    let recovered = t.contract.recover_fund_queue(&id);
    assert!(recovered.is_none(), "an expired invoice's queue is purged");
    assert_eq!(queue_len(&t, id), 0);
}

// ================================================================
// Residual risk — RR-CX1 (no expiry for partial funding)
// ================================================================

/// RR-CX1: a `PartiallyFunded` invoice is neither expirable nor deadline-bound
/// — it remains fundable past its due date until it fills or the freelancer
/// cancels it.
#[test]
fn test_partially_funded_past_due_remains_fundable() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT / 2);
    advance_past_due(&t, id);

    let expire = t.contract.try_expire_invoice(&id);
    assert_eq!(expire, Err(Ok(ContractError::AlreadyFunded)));

    // Still accepts more funding past the due date (not blocked by the Pending
    // expiry check, which only applies to `Pending`).
    t.contract
        .fund_invoice(&t.funder, &id, &(INVOICE_AMOUNT / 4), &false);
    assert_eq!(status_of(&t, id), InvoiceStatus::PartiallyFunded);
}
