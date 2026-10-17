//! Issues #1070–#1073: token decimals, SAC authorization flags, frozen
//! accounts and clawback.
//!
//! Every failure-path test snapshots the invoice, funder list and token
//! balances before the call and asserts they are byte-for-byte unchanged
//! afterwards, so "typed error" also proves "no state corruption".
#![cfg(test)]

use super::*;
use crate::token_safety::MAX_TOKEN_DECIMALS;
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Events as _, IssuerFlags, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env,
};

const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;
const AMOUNT: i128 = 1_000_000_000;

fn discount(amount: i128) -> i128 {
    amount * DISCOUNT_RATE as i128 / 10_000
}

// ----------------------------------------------------------------
// Minimal SEP-41 tokens with a configurable surface
// ----------------------------------------------------------------

#[contracttype]
enum MockKey {
    Balance(Address),
    Decimals,
}

fn mock_balance(env: &Env, id: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&MockKey::Balance(id.clone()))
        .unwrap_or(0)
}

fn mock_move(env: &Env, from: &Address, to: &Address, amount: i128) {
    from.require_auth();
    let fb = mock_balance(env, from);
    assert!(fb >= amount, "insufficient balance");
    env.storage()
        .persistent()
        .set(&MockKey::Balance(from.clone()), &(fb - amount));
    let tb = mock_balance(env, to);
    env.storage()
        .persistent()
        .set(&MockKey::Balance(to.clone()), &(tb + amount));
}

fn mock_mint(env: &Env, to: &Address, amount: i128) {
    let b = mock_balance(env, to);
    env.storage()
        .persistent()
        .set(&MockKey::Balance(to.clone()), &(b + amount));
}

/// Reports a configurable `decimals()` and SAC-style `authorized()`.
#[contract]
pub struct DecimalsToken;

#[contractimpl]
impl DecimalsToken {
    pub fn set_decimals(env: Env, d: u32) {
        env.storage().instance().set(&MockKey::Decimals, &d);
    }
    pub fn decimals(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&MockKey::Decimals)
            .unwrap_or(7)
    }
    pub fn authorized(_env: Env, _id: Address) -> bool {
        true
    }
    pub fn balance(env: Env, id: Address) -> i128 {
        mock_balance(&env, &id)
    }
    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        mock_move(&env, &from, &to, amount)
    }
    pub fn mint(env: Env, to: Address, amount: i128) {
        mock_mint(&env, &to, amount)
    }
}

/// SEP-41-ish token that does not implement `decimals()`.
#[contract]
pub struct NoDecimalsToken;

#[contractimpl]
impl NoDecimalsToken {
    pub fn authorized(_env: Env, _id: Address) -> bool {
        true
    }
    pub fn balance(env: Env, id: Address) -> i128 {
        mock_balance(&env, &id)
    }
    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        mock_move(&env, &from, &to, amount)
    }
    pub fn mint(env: Env, to: Address, amount: i128) {
        mock_mint(&env, &to, amount)
    }
}

/// Token with `decimals()` but no SAC `authorized()` interface.
#[contract]
pub struct NoAuthToken;

#[contractimpl]
impl NoAuthToken {
    pub fn decimals(_env: Env) -> u32 {
        7
    }
    pub fn balance(env: Env, id: Address) -> i128 {
        mock_balance(&env, &id)
    }
    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        mock_move(&env, &from, &to, amount)
    }
    pub fn mint(env: Env, to: Address, amount: i128) {
        mock_mint(&env, &to, amount)
    }
}

// ----------------------------------------------------------------
// Fixture
// ----------------------------------------------------------------

struct T {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    admin: Address,
    freelancer: Address,
    payer: Address,
    lp: Address,
    lp2: Address,
    token: Address,
    sac: StellarAssetClient<'static>,
    tc: TokenClient<'static>,
    eurc: Address,
}

/// SAC with AUTH_REVOCABLE + AUTH_CLAWBACK_ENABLED set before any balance
/// exists, so every balance (including the contract's) is freezable and
/// clawback-able — the worst case an issuer can configure.
fn setup() -> T {
    let env = Env::default();
    env.mock_all_auths();

    let issuer_admin = Address::generate(&env);
    let sac_contract = env.register_stellar_asset_contract_v2(issuer_admin);
    sac_contract.issuer().set_flag(IssuerFlags::RevocableFlag);
    sac_contract
        .issuer()
        .set_flag(IssuerFlags::ClawbackEnabledFlag);
    let token = sac_contract.address();
    let sac = StellarAssetClient::new(&env, &token);
    let tc = TokenClient::new(&env, &token);

    let admin = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let lp = Address::generate(&env);
    let lp2 = Address::generate(&env);
    for who in [&payer, &lp, &lp2] {
        sac.mint(who, &(AMOUNT * 10));
    }

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    let eurc = Address::generate(&env);
    let xlm = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
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
        lp2,
        token,
        sac,
        tc,
        eurc,
    }
}

fn submit(t: &T) -> u64 {
    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &AMOUNT,
        &(t.env.ledger().timestamp() + DUE_DATE_OFFSET),
        &DISCOUNT_RATE,
        &t.token,
        &ReferralCode::None,
    )
}

/// Invoice funded half by `lp`, half by `lp2`.
fn funded_by_two(t: &T) -> u64 {
    let id = submit(t);
    t.contract.fund_invoice(&t.lp, &id, &(AMOUNT / 2), &false);
    t.contract.fund_invoice(&t.lp2, &id, &(AMOUNT / 2), &false);
    id
}

#[derive(Debug, PartialEq)]
struct Snap {
    invoice: Invoice,
    funders: Vec<(Address, i128)>,
    balances: [i128; 6],
}

fn snap(t: &T, id: u64) -> Snap {
    let funders = t
        .env
        .as_contract(&t.contract.address, || get_invoice_funders(&t.env, id));
    Snap {
        invoice: t.contract.get_invoice(&id),
        funders,
        balances: [
            t.tc.balance(&t.contract.address),
            t.tc.balance(&t.freelancer),
            t.tc.balance(&t.payer),
            t.tc.balance(&t.lp),
            t.tc.balance(&t.lp2),
            t.tc.balance(&t.admin),
        ],
    }
}

fn freeze(t: &T, who: &Address) {
    t.sac.set_authorized(who, &false);
}

fn unfreeze(t: &T, who: &Address) {
    t.sac.set_authorized(who, &true);
}

// ================================================================
// Issue #1070 — reject token configuration without decimals
// ================================================================

#[test]
fn add_token_accepts_matching_sac_decimals() {
    let t = setup();
    let new = t
        .env
        .register_stellar_asset_contract_v2(Address::generate(&t.env))
        .address();
    StellarAssetClient::new(&t.env, &new).mint(&t.admin, &10_000_000);
    t.contract.add_token(&new, &7);
    assert_eq!(t.contract.get_token_decimals(&new), Some(7));
}

#[test]
fn add_token_rejects_declared_decimals_that_differ_from_token() {
    let t = setup();
    let new = t
        .env
        .register_stellar_asset_contract_v2(Address::generate(&t.env))
        .address();
    StellarAssetClient::new(&t.env, &new).mint(&t.admin, &10_000_000);
    for wrong in [0_u32, 6, 8] {
        assert_eq!(
            t.contract.try_add_token(&new, &wrong),
            Err(Ok(ContractError::InvalidTokenDecimals)),
            "declared {wrong} vs on-chain 7"
        );
    }
    assert_eq!(t.contract.get_token_decimals(&new), None);
}

#[test]
fn add_token_rejects_token_without_decimals_fn() {
    let t = setup();
    let id = t.env.register_contract(None, NoDecimalsToken);
    NoDecimalsTokenClient::new(&t.env, &id).mint(&t.admin, &10_000_000);
    assert_eq!(
        t.contract.try_add_token(&id, &7),
        Err(Ok(ContractError::TokenDecimalsUnavailable))
    );
    assert_eq!(t.contract.get_token_decimals(&id), None);
}

#[test]
fn add_token_decimals_upper_bound_is_inclusive() {
    let t = setup();
    let id = t.env.register_contract(None, DecimalsToken);
    let c = DecimalsTokenClient::new(&t.env, &id);
    c.mint(&t.admin, &10_000_000);

    c.set_decimals(&(MAX_TOKEN_DECIMALS + 1));
    assert_eq!(
        t.contract.try_add_token(&id, &(MAX_TOKEN_DECIMALS + 1)),
        Err(Ok(ContractError::InvalidTokenDecimals))
    );
    assert_eq!(
        t.contract.try_add_token(&id, &u32::MAX),
        Err(Ok(ContractError::InvalidTokenDecimals))
    );

    c.set_decimals(&MAX_TOKEN_DECIMALS);
    t.contract.add_token(&id, &MAX_TOKEN_DECIMALS);
    assert_eq!(t.contract.get_token_decimals(&id), Some(MAX_TOKEN_DECIMALS));
}

#[test]
fn add_token_with_custom_decimals_scales_minimum_amount() {
    let t = setup();
    let id = t.env.register_contract(None, DecimalsToken);
    let c = DecimalsTokenClient::new(&t.env, &id);
    c.set_decimals(&8);
    c.mint(&t.admin, &10_000_000);
    t.contract.add_token(&id, &8);

    let due = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let r = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &99_999_999,
        &due,
        &DISCOUNT_RATE,
        &id,
        &ReferralCode::None,
    );
    assert_eq!(r, Err(Ok(ContractError::InvalidAmount)));
    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &100_000_000,
        &due,
        &DISCOUNT_RATE,
        &id,
        &ReferralCode::None,
    );
}

#[test]
fn add_token_requires_admin_auth() {
    let t = setup();
    let new = t
        .env
        .register_stellar_asset_contract_v2(Address::generate(&t.env))
        .address();
    StellarAssetClient::new(&t.env, &new).mint(&t.admin, &10_000_000);
    t.env.set_auths(&[]);
    assert!(t.contract.try_add_token(&new, &7).is_err());
    t.env.mock_all_auths();
    assert_eq!(t.contract.get_token_decimals(&new), None);
}

#[test]
fn initialize_registers_decimals_for_every_bootstrap_token() {
    let t = setup();
    assert_eq!(t.contract.get_token_decimals(&t.token), Some(6));
    assert_eq!(t.contract.get_token_decimals(&t.eurc), Some(6));
}

/// Legacy deployment: an approved token with no decimals entry must be
/// rejected (no silent fallback to 6) until `migrate` backfills it.
#[test]
fn missing_decimals_fail_closed_until_migrate_backfills() {
    let t = setup();
    t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .persistent()
            .remove(&DataKey::TokenDecimals(t.token.clone()));
        t.env
            .storage()
            .instance()
            .set(&DataKey::StorageVersion, &2_u32);
    });

    let due = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let r = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &AMOUNT,
        &due,
        &DISCOUNT_RATE,
        &t.token,
        &ReferralCode::None,
    );
    assert_eq!(r, Err(Ok(ContractError::TokenDecimalsNotConfigured)));

    assert_eq!(t.contract.migrate(), 3);
    assert_eq!(t.contract.get_token_decimals(&t.token), Some(6));
    submit(&t);
}

#[test]
fn migrate_never_overwrites_registered_decimals() {
    let t = setup();
    t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .persistent()
            .set(&DataKey::TokenDecimals(t.token.clone()), &9_u32);
    });
    t.contract.migrate();
    assert_eq!(t.contract.get_token_decimals(&t.token), Some(9));
}

#[test]
fn convert_invoice_token_rejects_target_without_decimals() {
    let t = setup();
    let id = submit(&t);
    let other = t
        .env
        .register_stellar_asset_contract_v2(Address::generate(&t.env))
        .address();
    t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .persistent()
            .set(&DataKey::ApprovedToken(other.clone()), &true);
    });
    assert_eq!(
        t.contract
            .try_convert_invoice_token(&t.freelancer, &id, &other),
        Err(Ok(ContractError::TokenDecimalsNotConfigured))
    );
    assert_eq!(t.contract.get_invoice(&id).token, t.token);
}

// ================================================================
// Issue #1073 — validate SAC authorization flags
// ================================================================

#[test]
fn add_token_rejects_token_without_authorization_interface() {
    let t = setup();
    let id = t.env.register_contract(None, NoAuthToken);
    NoAuthTokenClient::new(&t.env, &id).mint(&t.admin, &10_000_000);
    assert_eq!(
        t.contract.try_add_token(&id, &7),
        Err(Ok(ContractError::TokenAuthorizationUnavailable))
    );
}

#[test]
fn add_token_rejects_when_contract_balance_is_deauthorized() {
    let t = setup();
    let c = t
        .env
        .register_stellar_asset_contract_v2(Address::generate(&t.env));
    c.issuer().set_flag(IssuerFlags::RevocableFlag);
    let sac = StellarAssetClient::new(&t.env, &c.address());
    sac.mint(&t.admin, &10_000_000);
    sac.set_authorized(&t.contract.address, &false);
    assert_eq!(
        t.contract.try_add_token(&c.address(), &7),
        Err(Ok(ContractError::AccountFrozen))
    );
    assert_eq!(t.contract.get_token_decimals(&c.address()), None);
}

#[test]
fn add_token_rejects_when_admin_is_deauthorized() {
    let t = setup();
    let c = t
        .env
        .register_stellar_asset_contract_v2(Address::generate(&t.env));
    c.issuer().set_flag(IssuerFlags::RevocableFlag);
    let sac = StellarAssetClient::new(&t.env, &c.address());
    sac.mint(&t.admin, &10_000_000);
    sac.set_authorized(&t.admin, &false);
    assert_eq!(
        t.contract.try_add_token(&c.address(), &7),
        Err(Ok(ContractError::AccountFrozen))
    );
}

/// AUTH_REQUIRED asset: the contract holds nothing yet, so the SAC reports
/// it unauthorized until the issuer explicitly authorizes it.
#[test]
fn add_token_auth_required_asset_needs_explicit_contract_authorization() {
    let t = setup();
    let c = t
        .env
        .register_stellar_asset_contract_v2(Address::generate(&t.env));
    let sac = StellarAssetClient::new(&t.env, &c.address());
    sac.mint(&t.admin, &10_000_000);
    c.issuer().set_flag(IssuerFlags::RequiredFlag);
    sac.set_authorized(&t.admin, &true);

    assert_eq!(
        t.contract.try_add_token(&c.address(), &7),
        Err(Ok(ContractError::AccountFrozen))
    );
    sac.set_authorized(&t.contract.address, &true);
    t.contract.add_token(&c.address(), &7);
    assert_eq!(t.contract.get_token_decimals(&c.address()), Some(7));
}

// ================================================================
// Issue #1072 — frozen accounts without state corruption
// ================================================================

#[test]
fn frozen_lp_cannot_fund_and_state_is_untouched() {
    let t = setup();
    let id = submit(&t);
    freeze(&t, &t.lp);
    let before = snap(&t, id);
    assert_eq!(
        t.contract.try_fund_invoice(&t.lp, &id, &AMOUNT, &false),
        Err(Ok(ContractError::AccountFrozen))
    );
    assert_eq!(snap(&t, id), before);
}

#[test]
fn completing_fund_to_frozen_freelancer_is_rejected_before_lp_pays() {
    let t = setup();
    let id = submit(&t);
    t.contract.fund_invoice(&t.lp, &id, &(AMOUNT / 2), &false);
    freeze(&t, &t.freelancer);
    let before = snap(&t, id);
    assert_eq!(
        t.contract
            .try_fund_invoice(&t.lp2, &id, &(AMOUNT / 2), &false),
        Err(Ok(ContractError::AccountFrozen))
    );
    assert_eq!(snap(&t, id), before);
    assert_eq!(before.invoice.status, InvoiceStatus::PartiallyFunded);

    // Partial funding does not pay the freelancer, so it is still allowed.
    let id2 = submit(&t);
    t.contract.fund_invoice(&t.lp, &id2, &(AMOUNT / 2), &false);
}

#[test]
fn frozen_payer_cannot_mark_paid_and_state_is_untouched() {
    let t = setup();
    let id = funded_by_two(&t);
    freeze(&t, &t.payer);
    let before = snap(&t, id);
    assert_eq!(
        t.contract.try_mark_paid(&id, &AMOUNT),
        Err(Ok(ContractError::AccountFrozen))
    );
    assert_eq!(snap(&t, id), before);
}

/// Before #1072 one frozen funder made `mark_paid` trap for everyone. Now
/// the payer settles, the healthy funder is paid, and the frozen funder's
/// share is escrowed and claimable after unfreezing.
#[test]
fn frozen_funder_share_is_escrowed_without_blocking_settlement() {
    let t = setup();
    let id = funded_by_two(&t);
    freeze(&t, &t.lp2);

    let lp_before = t.tc.balance(&t.lp);
    let lp2_before = t.tc.balance(&t.lp2);
    t.contract.mark_paid(&id, &AMOUNT);
    // Check before any other call replaces the "last invocation" event set.
    assert!(
        t.env
            .events()
            .all()
            .events()
            .iter()
            .any(|e| std::format!("{:?}", e).contains("payout_escrowed")),
        "PayoutEscrowed event must be emitted"
    );

    let share = AMOUNT / 2;
    assert_eq!(t.contract.get_invoice(&id).status, InvoiceStatus::Paid);
    assert_eq!(t.tc.balance(&t.lp), lp_before + share);
    assert_eq!(t.tc.balance(&t.lp2), lp2_before);
    assert_eq!(t.contract.get_pending_payout(&t.lp2, &t.token), share);
    assert_eq!(t.tc.balance(&t.contract.address), share);

    // Still frozen: claim fails and the escrow entry survives.
    assert_eq!(
        t.contract.try_claim_pending_payout(&t.lp2, &t.token),
        Err(Ok(ContractError::AccountFrozen))
    );
    assert_eq!(t.contract.get_pending_payout(&t.lp2, &t.token), share);

    unfreeze(&t, &t.lp2);
    assert_eq!(t.contract.claim_pending_payout(&t.lp2, &t.token), share);
    assert_eq!(t.tc.balance(&t.lp2), lp2_before + share);
    assert_eq!(t.contract.get_pending_payout(&t.lp2, &t.token), 0);
    assert_eq!(t.tc.balance(&t.contract.address), 0);

    assert_eq!(
        t.contract.try_claim_pending_payout(&t.lp2, &t.token),
        Err(Ok(ContractError::NothingToClaim))
    );
}

#[test]
fn claim_pending_payout_requires_recipient_auth() {
    let t = setup();
    let id = funded_by_two(&t);
    freeze(&t, &t.lp2);
    t.contract.mark_paid(&id, &AMOUNT);
    unfreeze(&t, &t.lp2);

    t.env.set_auths(&[]);
    assert!(t
        .contract
        .try_claim_pending_payout(&t.lp2, &t.token)
        .is_err());
    t.env.mock_all_auths();
    assert_eq!(t.contract.get_pending_payout(&t.lp2, &t.token), AMOUNT / 2);

    // Another address has nothing to claim for that token.
    assert_eq!(
        t.contract.try_claim_pending_payout(&t.lp, &t.token),
        Err(Ok(ContractError::NothingToClaim))
    );
}

#[test]
fn claim_pending_payout_blocked_while_paused() {
    let t = setup();
    let id = funded_by_two(&t);
    freeze(&t, &t.lp2);
    t.contract.mark_paid(&id, &AMOUNT);
    unfreeze(&t, &t.lp2);
    t.contract.pause();
    assert_eq!(
        t.contract.try_claim_pending_payout(&t.lp2, &t.token),
        Err(Ok(ContractError::ContractPaused))
    );
}

#[test]
fn frozen_fee_recipient_does_not_block_settlement() {
    let t = setup();
    t.contract.update_fee_rate(&100);
    let id = funded_by_two(&t);
    freeze(&t, &t.admin);
    t.contract.mark_paid(&id, &AMOUNT);
    let fee = AMOUNT / 100;
    assert_eq!(t.contract.get_invoice(&id).status, InvoiceStatus::Paid);
    assert_eq!(t.contract.get_pending_payout(&t.admin, &t.token), fee);
    assert_eq!(t.tc.balance(&t.admin), 0);
}

#[test]
fn cancel_with_frozen_partial_funder_escrows_refund() {
    let t = setup();
    let id = submit(&t);
    t.contract.fund_invoice(&t.lp, &id, &(AMOUNT / 4), &false);
    t.contract.fund_invoice(&t.lp2, &id, &(AMOUNT / 4), &false);
    freeze(&t, &t.lp);
    let lp2_before = t.tc.balance(&t.lp2);

    t.contract.cancel_invoice(&id);

    let refund = AMOUNT / 4 - discount(AMOUNT / 4);
    assert_eq!(t.contract.get_invoice(&id).status, InvoiceStatus::Cancelled);
    assert_eq!(t.tc.balance(&t.lp2), lp2_before + refund);
    assert_eq!(t.contract.get_pending_payout(&t.lp, &t.token), refund);
    assert_eq!(t.tc.balance(&t.contract.address), refund);
}

/// A recipient frozen across two payouts accumulates one bounded entry per
/// (recipient, token), not one per invoice.
#[test]
fn escrow_accumulates_per_recipient_and_token() {
    let t = setup();
    let a = funded_by_two(&t);
    let b = funded_by_two(&t);
    freeze(&t, &t.lp2);
    t.contract.mark_paid(&a, &AMOUNT);
    t.contract.mark_paid(&b, &AMOUNT);
    assert_eq!(t.contract.get_pending_payout(&t.lp2, &t.token), AMOUNT);
    unfreeze(&t, &t.lp2);
    assert_eq!(t.contract.claim_pending_payout(&t.lp2, &t.token), AMOUNT);
}

// ================================================================
// Issue #1071 — trustline authorization and clawback
// ================================================================

#[test]
fn clawback_from_escrow_yields_typed_error_and_preserves_state() {
    let t = setup();
    let id = submit(&t);
    t.contract.fund_invoice(&t.lp, &id, &(AMOUNT / 2), &false);
    let held = t.tc.balance(&t.contract.address);
    assert_eq!(held, AMOUNT / 2 - discount(AMOUNT / 2));

    // Issuer claws back all but one stroop of the escrowed LP funds.
    t.sac.clawback(&t.contract.address, &(held - 1));
    let before = snap(&t, id);
    assert_eq!(
        t.contract.try_cancel_invoice(&id),
        Err(Ok(ContractError::InsufficientContractBalance))
    );
    assert_eq!(snap(&t, id), before);
    assert_eq!(before.invoice.status, InvoiceStatus::PartiallyFunded);

    // Recovery: once the shortfall is restored the cancel goes through.
    t.sac.mint(&t.contract.address, &(held - 1));
    t.contract.cancel_invoice(&id);
    assert_eq!(t.contract.get_invoice(&id).status, InvoiceStatus::Cancelled);
}

#[test]
fn clawback_exactly_at_balance_boundary() {
    let t = setup();
    let id = submit(&t);
    t.contract.fund_invoice(&t.lp, &id, &(AMOUNT / 2), &false);
    let held = t.tc.balance(&t.contract.address);

    // Exact balance: refund == balance, must succeed.
    t.contract.cancel_invoice(&id);
    assert_eq!(t.tc.balance(&t.contract.address), 0);

    // One stroop short on a second invoice: must fail.
    let id2 = submit(&t);
    t.contract.fund_invoice(&t.lp, &id2, &(AMOUNT / 2), &false);
    t.sac.clawback(&t.contract.address, &1);
    assert_eq!(t.tc.balance(&t.contract.address), held - 1);
    assert_eq!(
        t.contract.try_cancel_invoice(&id2),
        Err(Ok(ContractError::InsufficientContractBalance))
    );
}

#[test]
fn clawback_of_escrowed_payout_blocks_claim_until_restored() {
    let t = setup();
    let id = funded_by_two(&t);
    freeze(&t, &t.lp2);
    t.contract.mark_paid(&id, &AMOUNT);
    unfreeze(&t, &t.lp2);
    let owed = AMOUNT / 2;

    t.sac.clawback(&t.contract.address, &1);
    assert_eq!(
        t.contract.try_claim_pending_payout(&t.lp2, &t.token),
        Err(Ok(ContractError::InsufficientContractBalance))
    );
    assert_eq!(t.contract.get_pending_payout(&t.lp2, &t.token), owed);

    t.sac.mint(&t.contract.address, &1);
    assert_eq!(t.contract.claim_pending_payout(&t.lp2, &t.token), owed);
}

/// Escrowed funds are reserved: a later payout may not spend them even
/// though they sit in the contract's balance.
#[test]
fn escrowed_funds_are_not_spendable_by_other_payouts() {
    let t = setup();
    let paid = funded_by_two(&t);
    freeze(&t, &t.lp2);
    t.contract.mark_paid(&paid, &AMOUNT);
    unfreeze(&t, &t.lp2);
    let reserved = AMOUNT / 2;

    let id = submit(&t);
    t.contract.fund_invoice(&t.lp, &id, &(AMOUNT / 2), &false);
    let refund = AMOUNT / 2 - discount(AMOUNT / 2);
    assert_eq!(t.tc.balance(&t.contract.address), reserved + refund);

    // Claw back the unreserved refund money: balance now equals escrow.
    t.sac.clawback(&t.contract.address, &refund);
    assert_eq!(
        t.contract.try_cancel_invoice(&id),
        Err(Ok(ContractError::InsufficientContractBalance))
    );
    assert_eq!(t.contract.claim_pending_payout(&t.lp2, &t.token), reserved);
}

#[test]
fn clawback_from_lp_after_funding_does_not_affect_contract_accounting() {
    let t = setup();
    let id = funded_by_two(&t);
    let lp_bal = t.tc.balance(&t.lp);
    t.sac.clawback(&t.lp, &lp_bal);
    t.contract.mark_paid(&id, &AMOUNT);
    assert_eq!(t.tc.balance(&t.lp), AMOUNT / 2);
    assert_eq!(t.tc.balance(&t.contract.address), 0);
}

#[test]
fn deauthorized_contract_balance_blocks_inbound_funding_atomically() {
    let t = setup();
    let id = submit(&t);
    freeze(&t, &t.contract.address);
    let before = snap(&t, id);
    assert!(t
        .contract
        .try_fund_invoice(&t.lp, &id, &AMOUNT, &false)
        .is_err());
    assert_eq!(snap(&t, id), before);
}

/// Resource measurement for the PR: CPU instructions for a full
/// fund + settle cycle. Run with `--nocapture` to print.
#[test]
fn measure_fund_and_settle_cost() {
    let t = setup();
    let id = submit(&t);
    t.env.cost_estimate().budget().reset_default();
    t.contract.fund_invoice(&t.lp, &id, &AMOUNT, &false);
    let fund_cpu = t.env.cost_estimate().budget().cpu_instruction_cost();
    t.env.cost_estimate().budget().reset_default();
    t.contract.mark_paid(&id, &AMOUNT);
    let paid_cpu = t.env.cost_estimate().budget().cpu_instruction_cost();
    std::println!("fund_invoice cpu={fund_cpu} mark_paid cpu={paid_cpu}");
}
