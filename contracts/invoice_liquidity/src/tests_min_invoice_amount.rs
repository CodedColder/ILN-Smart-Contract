#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env,
};

const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

struct TestEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    usdc: Address,
    xlm: Address,
    admin: Address,
    freelancer: Address,
    payer: Address,
    funder: Address,
}

fn setup_extended() -> TestEnv {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);

    let usdc_admin = Address::generate(&env);
    let usdc_id = env.register_stellar_asset_contract_v2(usdc_admin);
    let usdc_address = usdc_id.address();
    let usdc_sac = StellarAssetClient::new(&env, &usdc_address);

    let xlm_admin = Address::generate(&env);
    let xlm_id = env.register_stellar_asset_contract_v2(xlm_admin);
    let xlm_address = xlm_id.address();
    let xlm_sac = StellarAssetClient::new(&env, &xlm_address);

    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let funder = Address::generate(&env);

    usdc_sac.mint(&funder, &1_000_000_000_000);
    usdc_sac.mint(&payer, &1_000_000_000_000);
    xlm_sac.mint(&funder, &1_000_000_000_000);
    xlm_sac.mint(&payer, &1_000_000_000_000);

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);

    let eurc_address = Address::generate(&env);
    contract.initialize(&admin, &usdc_address, &eurc_address, &xlm_address);

    TestEnv {
        env,
        contract,
        usdc: usdc_address,
        xlm: xlm_address,
        admin,
        freelancer,
        payer,
        funder,
    }
}

fn due_date(t: &TestEnv) -> u64 {
    t.env.ledger().timestamp() + DUE_DATE_OFFSET
}

/// Advance the ledger past `add_token`'s rate-limit cooldown
/// (`DEFAULT_RATE_LIMIT_LEDGERS`). The last-call ledger for a rate-limited
/// function defaults to 0 when never called, so calling `add_token()` at
/// the default sequence 0 incorrectly trips the cooldown on its very
/// first-ever call. See tests_oracle_registry.rs's
/// `advance_past_rate_limit_cooldown` for the same workaround applied to
/// other rate-limited functions.
fn advance_past_rate_limit_cooldown(env: &Env) {
    let mut info = env.ledger().get();
    info.sequence_number += crate::constants::DEFAULT_RATE_LIMIT_LEDGERS as u32;
    env.ledger().set(info);
}

#[test]
fn test_usdc_submit_at_minimum_succeeds() {
    let t = setup_extended();
    let min_usdc = 1_000_000i128;

    let id = t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &min_usdc,
        &due_date(&t),
        &DISCOUNT_RATE,
        &t.usdc,
        &ReferralCode::None,
    );

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.amount, min_usdc);
}

#[test]
fn test_rejection_below_minimum() {
    let t = setup_extended();
    let below_min = 999_999i128;

    let result = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &below_min,
        &due_date(&t),
        &DISCOUNT_RATE,
        &t.usdc,
        &ReferralCode::None,
    );
    assert_eq!(result, Err(Ok(ContractError::InvalidAmount)));
}

#[test]
fn test_zero_amount_rejected() {
    let t = setup_extended();

    let result = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &0,
        &due_date(&t),
        &DISCOUNT_RATE,
        &t.usdc,
        &ReferralCode::None,
    );
    assert_eq!(result, Err(Ok(ContractError::InvalidAmount)));
}

#[test]
fn test_different_minimums_usdc_vs_xlm() {
    let t = setup_extended();
    let amount_valid_usdc_invalid_xlm = 5_000_000i128;

    let id = t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &amount_valid_usdc_invalid_xlm,
        &due_date(&t),
        &DISCOUNT_RATE,
        &t.usdc,
        &ReferralCode::None,
    );
    assert_eq!(id, 1);

    let result = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &amount_valid_usdc_invalid_xlm,
        &due_date(&t),
        &DISCOUNT_RATE,
        &t.xlm,
        &ReferralCode::None,
    );
    assert_eq!(result, Err(Ok(ContractError::InvalidAmount)));
}

#[test]
fn test_xlm_at_minimum_succeeds() {
    let t = setup_extended();
    let min_xlm = 10_000_000i128;

    let id = t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &min_xlm,
        &due_date(&t),
        &DISCOUNT_RATE,
        &t.xlm,
        &ReferralCode::None,
    );

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.amount, min_xlm);
}

#[test]
fn test_xlm_below_minimum_rejected() {
    let t = setup_extended();
    let below_xlm_min = 5_000_000i128;

    let result = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &below_xlm_min,
        &due_date(&t),
        &DISCOUNT_RATE,
        &t.xlm,
        &ReferralCode::None,
    );
    assert_eq!(result, Err(Ok(ContractError::InvalidAmount)));
}

#[test]
fn test_admin_adds_token_with_different_decimals() {
    let t = setup_extended();

    let new_admin = Address::generate(&t.env);
    let new_id = t.env.register_stellar_asset_contract_v2(new_admin.clone());
    let new_token = new_id.address();
    let new_sac = StellarAssetClient::new(&t.env, &new_token);
    new_sac.mint(&t.funder, &1_000_000_000_000);
    // Contract admin needs tokens on the new token so add_token() can verify
    // it isn't fee-on-transfer (admin is the one that performs the test transfer).
    new_sac.mint(&t.admin, &10_000_000);

    advance_past_rate_limit_cooldown(&t.env);
    // Issue #1070: declared decimals must match the token's own
    // `decimals()`; Stellar Asset Contracts always report 7.
    t.contract.add_token(&new_token, &7);

    let min_7 = 10_000_000i128;
    let id = t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &min_7,
        &due_date(&t),
        &DISCOUNT_RATE,
        &new_token,
        &ReferralCode::None,
    );
    assert_eq!(id, 1);

    let below = 9_999_999i128;
    let result = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &below,
        &due_date(&t),
        &DISCOUNT_RATE,
        &new_token,
        &ReferralCode::None,
    );
    assert_eq!(result, Err(Ok(ContractError::InvalidAmount)));
}
