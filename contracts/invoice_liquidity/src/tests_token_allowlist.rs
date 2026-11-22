//! Issue #1176: approved-token allowlist invariants.
//!
//! Regression tests that pin the *observed* behavior of the governance
//! approved-token allowlist (`add_token` / `remove_token` /
//! `is_approved_token`) and the per-token funded-volume cap across invoice
//! submission, token conversion, batch submission and funding.
//!
//! These tests document invariants; they do not assert that any current
//! behavior is a vulnerability. The rationale, gate table and residual-risk
//! notes live in `docs/token-allowlist-invariants.md`.
//!
//! Every rejection-path test asserts a *typed* `ContractError`; Soroban rolls
//! back all writes/events on `Err`, so a typed error also proves no partial
//! state was persisted (asserted where it matters).
#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, Env,
};

const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;
const AMOUNT: i128 = 1_000_000_000;

/// Classic-asset SACs (including `register_stellar_asset_contract_v2`) report
/// 7 decimals; `add_token` must be told exactly that (Issue #1070).
const SAC_DECIMALS: u32 = 7;

struct T {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    admin: Address,
    freelancer: Address,
    payer: Address,
    lp: Address,
    /// Bootstrap USDC-like token wired by `initialize` (approved, 6-dec entry).
    token: Address,
}

fn new_sac(env: &Env) -> Address {
    env.register_stellar_asset_contract_v2(Address::generate(env))
        .address()
}

fn setup() -> T {
    let env = Env::default();
    env.mock_all_auths();

    let token = new_sac(&env);
    let sac = StellarAssetClient::new(&env, &token);

    let admin = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let lp = Address::generate(&env);
    for who in [&payer, &lp] {
        sac.mint(who, &(AMOUNT * 10));
    }

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);

    // EURC's bootstrap address is deliberately not a contract in this fixture
    // (`initialize` never probes it); XLM is a second SAC.
    let eurc = Address::generate(&env);
    let xlm = new_sac(&env);
    contract.initialize(&admin, &token, &eurc, &xlm);

    let mut info = env.ledger().get();
    info.timestamp = 1_700_000_000;
    info.sequence_number += crate::constants::DEFAULT_RATE_LIMIT_LEDGERS as u32;
    env.ledger().set(info);

    T {
        env,
        contract,
        admin,
        freelancer,
        payer,
        lp,
        token,
    }
}

fn due(t: &T) -> u64 {
    t.env.ledger().timestamp() + DUE_DATE_OFFSET
}

fn submit(t: &T, token: &Address) -> u64 {
    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &AMOUNT,
        &due(t),
        &DISCOUNT_RATE,
        token,
        &ReferralCode::None,
    )
}

/// Assert a submission in `token` is rejected as unapproved.
fn assert_submit_unauthorized(t: &T, token: &Address) {
    assert_eq!(
        t.contract.try_submit_invoice(
            &t.freelancer,
            &t.payer,
            &AMOUNT,
            &due(t),
            &DISCOUNT_RATE,
            token,
            &ReferralCode::None,
        ),
        Err(Ok(ContractError::Unauthorized))
    );
}

/// Register and approve a fresh SAC token through the public `add_token` path.
fn add_new_token(t: &T) -> Address {
    let new = new_sac(&t.env);
    StellarAssetClient::new(&t.env, &new).mint(&t.admin, &10_000_000);
    t.contract.add_token(&new, &SAC_DECIMALS);
    new
}

/// Advance past the default rate-limit cooldown so a repeat call to the same
/// admin entry point is accepted.
fn advance(t: &T) {
    let mut info = t.env.ledger().get();
    info.sequence_number += crate::constants::DEFAULT_RATE_LIMIT_LEDGERS as u32;
    t.env.ledger().set(info);
}

fn volume(t: &T, token: &Address) -> i128 {
    t.env
        .as_contract(&t.contract.address, || get_token_volume(&t.env, token))
}

// ================================================================
// INV-AL1 / INV-AL2 — allowlist gating at submission
// ================================================================

/// A token that was never approved is rejected at submission with
/// `Unauthorized` — before any decimals/amount validation runs.
#[test]
fn unapproved_token_is_rejected_at_submission() {
    let t = setup();
    let rogue = Address::generate(&t.env);
    assert_submit_unauthorized(&t, &rogue);
}

/// The bootstrap token wired by `initialize` is usable; a never-wired token is
/// not.
#[test]
fn initialize_wires_and_approves_the_bootstrap_token() {
    let t = setup();
    let id = submit(&t, &t.token);
    assert_eq!(t.contract.get_invoice(&id).status, InvoiceStatus::Pending);

    let rogue = Address::generate(&t.env);
    assert_submit_unauthorized(&t, &rogue);
}

/// An explicit `ApprovedToken = false` is authoritative: removing a
/// Config-wired bootstrap token really disables it (no Config fall-through).
#[test]
fn explicit_false_wins_over_config_fallback() {
    let t = setup();
    // Sanity: usable while approved.
    assert_eq!(t.contract.get_invoice(&submit(&t, &t.token)).status, InvoiceStatus::Pending);

    t.contract.remove_token(&t.token);

    assert_submit_unauthorized(&t, &t.token);
}

/// `convert_invoice_token` only accepts an allowlisted *target* token; a
/// rejected conversion leaves the invoice's token unchanged.
#[test]
fn convert_to_unapproved_token_is_rejected() {
    let t = setup();
    let id = submit(&t, &t.token);
    let rogue = Address::generate(&t.env);

    assert_eq!(
        t.contract
            .try_convert_invoice_token(&t.freelancer, &id, &rogue),
        Err(Ok(ContractError::Unauthorized))
    );
    assert_eq!(t.contract.get_invoice(&id).token, t.token);
}

/// `submit_invoices_batch` rejects an unapproved token per item.
#[test]
fn batch_submission_rejects_unapproved_token() {
    let t = setup();
    let rogue = Address::generate(&t.env);

    let mut batch = Vec::new(&t.env);
    batch.push_back(InvoiceParams {
        freelancer: t.freelancer.clone(),
        payer: t.payer.clone(),
        amount: AMOUNT,
        due_date: due(&t),
        discount_rate: DISCOUNT_RATE,
        token: rogue,
        referral_code: ReferralCode::None,
    });

    assert_eq!(
        t.contract.try_submit_invoices_batch(&batch),
        Err(Ok(ContractError::Unauthorized))
    );
}

// ================================================================
// INV-AL3 — remove_token gates new money in, not settlement
// ================================================================

/// Removing a token after submission blocks *further funding* of the invoice
/// (Issue #19), and leaves the invoice untouched.
#[test]
fn removed_token_blocks_further_funding_of_existing_invoice() {
    let t = setup();
    let id = submit(&t, &t.token);

    t.contract.remove_token(&t.token);

    assert_eq!(
        t.contract.try_fund_invoice(&t.lp, &id, &AMOUNT, &false),
        Err(Ok(ContractError::Unauthorized))
    );
    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Pending);
    assert_eq!(invoice.amount_funded, 0);
}

/// Removal does not block settlement of an invoice already fully funded: the
/// committed principal can still be repaid (`mark_paid` is not gated).
#[test]
fn removed_token_does_not_block_settlement_of_funded_invoice() {
    let t = setup();
    let id = submit(&t, &t.token);
    t.contract.fund_invoice(&t.lp, &id, &AMOUNT, &false);
    assert_eq!(t.contract.get_invoice(&id).status, InvoiceStatus::Funded);

    t.contract.remove_token(&t.token);

    t.contract.mark_paid(&id, &AMOUNT);
    assert_eq!(t.contract.get_invoice(&id).status, InvoiceStatus::Paid);
}

// ================================================================
// INV-AL4 — re-approval round trip
// ================================================================

/// `add_token` → `remove_token` → `add_token` restores usability, and
/// registered decimals survive removal.
#[test]
fn reapproval_after_removal_restores_usability() {
    let t = setup();
    let new = add_new_token(&t);
    assert_eq!(t.contract.get_invoice(&submit(&t, &new)).status, InvoiceStatus::Pending);

    t.contract.remove_token(&new);
    assert_submit_unauthorized(&t, &new);
    // Decimals are not cleared by removal.
    assert_eq!(t.contract.get_token_decimals(&new), Some(SAC_DECIMALS));

    advance(&t);
    t.contract.add_token(&new, &SAC_DECIMALS);
    assert_eq!(t.contract.get_invoice(&submit(&t, &new)).status, InvoiceStatus::Pending);
}

// ================================================================
// INV-VC1..INV-VC4 — per-token funded-volume caps
// ================================================================

/// An unset cap is `0` and therefore uncapped.
#[test]
fn unset_volume_cap_is_uncapped() {
    let t = setup();
    assert_eq!(t.contract.token_volume_cap(&t.token), 0);

    let id = submit(&t, &t.token);
    t.contract.fund_invoice(&t.lp, &id, &AMOUNT, &false);
    assert_eq!(t.contract.get_invoice(&id).status, InvoiceStatus::Funded);
    assert_eq!(volume(&t, &t.token), AMOUNT);
}

/// The cap is enforced at the funding boundary: reaching it exactly is
/// allowed, exceeding it is rejected atomically with
/// `GlobalVolumeCapExceeded`, and the counter is not advanced on rejection.
#[test]
fn volume_cap_is_enforced_at_funding_boundary() {
    let t = setup();
    t.contract.set_token_volume_cap(&t.token, &AMOUNT);
    assert_eq!(t.contract.token_volume_cap(&t.token), AMOUNT);

    // Exactly at the cap: allowed.
    let a = submit(&t, &t.token);
    t.contract.fund_invoice(&t.lp, &a, &AMOUNT, &false);
    assert_eq!(volume(&t, &t.token), AMOUNT);

    // Would exceed the cap: rejected, nothing changes.
    let b = submit(&t, &t.token);
    assert_eq!(
        t.contract.try_fund_invoice(&t.lp, &b, &AMOUNT, &false),
        Err(Ok(ContractError::GlobalVolumeCapExceeded))
    );
    assert_eq!(volume(&t, &t.token), AMOUNT);
    let invoice_b = t.contract.get_invoice(&b);
    assert_eq!(invoice_b.status, InvoiceStatus::Pending);
    assert_eq!(invoice_b.amount_funded, 0);
}

/// `set_token_volume_cap` is admin-only.
#[test]
fn set_token_volume_cap_requires_admin() {
    let t = setup();
    t.env.set_auths(&[]);
    assert!(t.contract.try_set_token_volume_cap(&t.token, &AMOUNT).is_err());

    t.env.mock_all_auths();
    assert_eq!(t.contract.token_volume_cap(&t.token), 0);
}
