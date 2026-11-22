//! Issue #1177: token decimal normalization in financial entry points.
//!
//! Regression tests that pin the *observed* precision handling of token
//! amounts across submission, funding and settlement. They do not assert that
//! any current behavior is a vulnerability; the verified findings and residual
//! risks live in `docs/token-decimal-normalization.md`, and the normative
//! invariants are declared in `token_safety.rs` (INV-DN1..INV-DN6).
//!
//! The central invariant is INV-DN3: the contract carries amounts in the
//! token's **native precision** and its `normalize_*` helpers are identity
//! functions, so base units transferred equal base units accounted.
#![cfg(test)]

use super::*;
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Ledger},
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
// A minimal SEP-41 token with a configurable `decimals()`, used to model a
// custom (non-USDC/EURC/XLM) asset — the `else` branch of the normalizer
// ladder. `register_stellar_asset_contract_v2` always reports 7, so a mock is
// the only way to exercise a custom token through the real `add_token` gate.
// ----------------------------------------------------------------

#[contracttype]
enum TokenKey {
    Balance(Address),
    Decimals,
}

fn mock_balance(env: &Env, id: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&TokenKey::Balance(id.clone()))
        .unwrap_or(0)
}

fn mock_mint(env: &Env, to: &Address, amount: i128) {
    let b = mock_balance(env, to);
    env.storage()
        .persistent()
        .set(&TokenKey::Balance(to.clone()), &(b + amount));
}

fn mock_move(env: &Env, from: &Address, to: &Address, amount: i128) {
    from.require_auth();
    let fb = mock_balance(env, from);
    assert!(fb >= amount, "insufficient balance");
    env.storage()
        .persistent()
        .set(&TokenKey::Balance(from.clone()), &(fb - amount));
    let tb = mock_balance(env, to);
    env.storage()
        .persistent()
        .set(&TokenKey::Balance(to.clone()), &(tb + amount));
}

#[contract]
pub struct NormalizeToken;

#[contractimpl]
impl NormalizeToken {
    pub fn set_decimals(env: Env, d: u32) {
        env.storage().instance().set(&TokenKey::Decimals, &d);
    }
    pub fn decimals(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&TokenKey::Decimals)
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
        mock_mint(&env, &to, amount);
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
    /// Bootstrap USDC-like SAC, registered with 6 decimals by `initialize`.
    usdc: Address,
    /// Bootstrap XLM-like SAC, registered with 7 decimals by `initialize`.
    xlm: Address,
    /// Placeholder EURC address (`initialize` never probes it).
    eurc: Address,
}

fn new_sac(env: &Env) -> Address {
    env.register_stellar_asset_contract_v2(Address::generate(env))
        .address()
}

fn setup() -> T {
    let env = Env::default();
    env.mock_all_auths();

    let usdc = new_sac(&env);
    let xlm = new_sac(&env);
    let eurc = Address::generate(&env);

    let admin = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let lp = Address::generate(&env);
    let usdc_sac = StellarAssetClient::new(&env, &usdc);
    for who in [&payer, &lp] {
        usdc_sac.mint(who, &(AMOUNT * 10));
    }
    usdc_sac.mint(&admin, &(AMOUNT * 10));

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    contract.initialize(&admin, &usdc, &eurc, &xlm);

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
        usdc,
        xlm,
        eurc,
    }
}

fn due(t: &T) -> u64 {
    t.env.ledger().timestamp() + DUE_DATE_OFFSET
}

fn submit_for(t: &T, token: &Address, amount: i128) -> u64 {
    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &amount,
        &due(t),
        &DISCOUNT_RATE,
        token,
        &ReferralCode::None,
    )
}

fn usdc_balance(t: &T, who: &Address) -> i128 {
    TokenClient::new(&t.env, &t.usdc).balance(who)
}

/// Assert a submission of `amount` in `token` is rejected as below the
/// decimal-scaled amount floor. Inlined (rather than a typed `try_` helper) to
/// match the proven assertion pattern used across the token test suites.
fn assert_submit_invalid_amount(t: &T, token: &Address, amount: i128) {
    assert_eq!(
        t.contract.try_submit_invoice(
            &t.freelancer,
            &t.payer,
            &amount,
            &due(t),
            &DISCOUNT_RATE,
            token,
            &ReferralCode::None,
        ),
        Err(Ok(ContractError::InvalidAmount))
    );
}

// ================================================================
// INV-DN1 / INV-DN3 — the normalizers are identity
// ================================================================

/// All three `normalize_*` helpers return their argument unchanged, for every
/// representable amount. This is the property that makes "native precision
/// passthrough" true; it is asserted directly rather than assumed.
#[test]
fn normalizers_are_identity_for_every_amount() {
    for v in [
        0_i128,
        1,
        999_999,
        1_000_000,
        10_000_000,
        AMOUNT,
        i128::MAX,
    ] {
        assert_eq!(normalize_xlm_amount(v), v);
        assert_eq!(normalize_eurc_amount(v), v);
        assert_eq!(normalize_usdc_amount(v), v);
    }
}

/// The class matchers resolve purely by `Config` SAC address: the bootstrap
/// tokens match their own slot, and an arbitrary token matches none (so it
/// takes the `else` → `normalize_usdc_amount` branch, INV-DN2).
#[test]
fn token_class_matchers_select_by_config_address() {
    let t = setup();
    let custom = Address::generate(&t.env);

    t.env.as_contract(&t.contract.address, || {
        assert!(is_usdc_token(&t.env, &t.usdc));
        assert!(!is_usdc_token(&t.env, &t.xlm));

        assert!(is_xlm_token(&t.env, &t.xlm));
        assert!(!is_xlm_token(&t.env, &t.usdc));

        assert!(is_eurc_token(&t.env, &t.eurc));
        assert!(!is_eurc_token(&t.env, &t.usdc));

        // A token that is none of the three configured SACs falls through the
        // ladder; because every branch is identity, the result is unchanged.
        assert!(!is_usdc_token(&t.env, &custom));
        assert!(!is_xlm_token(&t.env, &custom));
        assert!(!is_eurc_token(&t.env, &custom));
        assert_eq!(normalize_usdc_amount(AMOUNT), AMOUNT);
    });
}

// ================================================================
// INV-DN4 — the amount floor scales with registered decimals
// ================================================================

/// A 6-decimal token (bootstrap USDC) accepts exactly `1_000_000` base units
/// and rejects one unit below it.
#[test]
fn six_decimal_token_floor_is_one_whole_unit() {
    let t = setup();
    assert_eq!(t.contract.get_token_decimals(&t.usdc), Some(6));

    assert_submit_invalid_amount(&t, &t.usdc, 999_999);
    let id = submit_for(&t, &t.usdc, 1_000_000);
    assert_eq!(t.contract.get_invoice(&id).status, InvoiceStatus::Pending);
}

/// A 7-decimal token (bootstrap XLM) needs `10_000_000` base units; one whole
/// unit at 6 decimals is rejected.
#[test]
fn seven_decimal_token_floor_is_one_whole_unit() {
    let t = setup();
    assert_eq!(t.contract.get_token_decimals(&t.xlm), Some(7));

    assert_submit_invalid_amount(&t, &t.xlm, 9_999_999);
    let id = submit_for(&t, &t.xlm, 10_000_000);
    assert_eq!(t.contract.get_invoice(&id).status, InvoiceStatus::Pending);
}

/// A token with no registered decimals fails closed rather than falling back
/// to a guessed 6-decimal scale (INV-DN1).
#[test]
fn unregistered_token_decimals_fail_closed() {
    let t = setup();
    let rogue = Address::generate(&t.env);

    let res = t
        .env
        .as_contract(&t.contract.address, || {
            token_safety::registered_token_decimals(&t.env, &rogue)
        });
    assert_eq!(res, Err(ContractError::TokenDecimalsNotConfigured));
    assert_eq!(t.contract.get_token_decimals(&rogue), None);
}

// ================================================================
// INV-DN3 — funding and settlement move native base units verbatim
// ================================================================

/// Funding moves exactly `amount - discount` base units from the LP, pays the
/// freelancer the same, and records the *raw* invoice amount — no rescaling.
#[test]
fn funding_transfers_exactly_amount_minus_discount_in_native_units() {
    let t = setup();
    let id = submit_for(&t, &t.usdc, AMOUNT);

    let lp_before = usdc_balance(&t, &t.lp);
    let freelancer_before = usdc_balance(&t, &t.freelancer);

    t.contract.fund_invoice(&t.lp, &id, &AMOUNT, &false);

    let cost = AMOUNT - discount(AMOUNT);
    assert_eq!(lp_before - usdc_balance(&t, &t.lp), cost);
    assert_eq!(usdc_balance(&t, &t.freelancer) - freelancer_before, cost);

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Funded);
    assert_eq!(invoice.amount_funded, AMOUNT);

    // Volume accounting uses the raw funded amount too.
    let volume =
        t.env
            .as_contract(&t.contract.address, || get_token_volume(&t.env, &t.usdc));
    assert_eq!(volume, AMOUNT);
}

/// Settlement moves exactly the requested amount in base units and records the
/// same value in `amount_paid` — the transferred and accounted amounts agree.
#[test]
fn settlement_transfers_exactly_amount_in_native_units() {
    let t = setup();
    let id = submit_for(&t, &t.usdc, AMOUNT);
    t.contract.fund_invoice(&t.lp, &id, &AMOUNT, &false);

    let payer_before = usdc_balance(&t, &t.payer);
    t.contract.mark_paid(&id, &AMOUNT);

    assert_eq!(payer_before - usdc_balance(&t, &t.payer), AMOUNT);
    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Paid);
    assert_eq!(invoice.amount_paid, AMOUNT);
}

// ================================================================
// INV-DN2 — custom tokens take the identity `else` branch
// ================================================================

/// A custom 7-decimal token that is neither USDC, EURC nor XLM is accepted via
/// `add_token` and its amounts pass through the `else` branch unchanged: the
/// LP and freelancer move exactly `amount - discount` base units.
#[test]
fn custom_token_amount_is_not_rescaled() {
    let t = setup();

    let token_addr = t.env.register_contract(None, NormalizeToken);
    let token = NormalizeTokenClient::new(&t.env, &token_addr);
    token.set_decimals(&7);
    token.mint(&t.admin, &(AMOUNT * 10));
    token.mint(&t.payer, &(AMOUNT * 10));
    token.mint(&t.lp, &(AMOUNT * 10));

    t.contract.add_token(&token_addr, &7);
    assert_eq!(t.contract.get_token_decimals(&token_addr), Some(7));

    let invoice_id = submit_for(&t, &token_addr, AMOUNT);
    let lp_before = token.balance(&t.lp);
    let freelancer_before = token.balance(&t.freelancer);

    t.contract.fund_invoice(&t.lp, &invoice_id, &AMOUNT, &false);

    let cost = AMOUNT - discount(AMOUNT);
    assert_eq!(lp_before - token.balance(&t.lp), cost);
    assert_eq!(token.balance(&t.freelancer) - freelancer_before, cost);
    assert_eq!(t.contract.get_invoice(&invoice_id).amount_funded, AMOUNT);
}
