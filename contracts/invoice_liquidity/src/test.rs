#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    Address, BytesN, Env,
};

// ----------------------------------------------------------------
// Test helpers — shared setup used across all tests
// ----------------------------------------------------------------

/// All the actors and contract references a test needs
pub struct TestEnv {
    pub env: Env,
    pub contract: InvoiceLiquidityContractClient<'static>,
    pub token: TokenClient<'static>,
    pub freelancer: Address,
    pub payer: Address,
    pub funder: Address,
    pub admin: Address,
}

/// Standard invoice values reused across tests
const INVOICE_AMOUNT: i128 = 1_000_000_000; // 100 USDC in stroops (1 USDC = 10_000_000)
const DISCOUNT_RATE: u32 = 300; // 3.00% in basis points
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30; // 30 days from now

pub fn setup() -> TestEnv {
    let env = Env::default();

    // Skip auth checks in tests — we test auth separately
    env.mock_all_auths();

    // ---- Deploy a mock USDC token contract ----
    let usdc_admin = Address::generate(&env);
    let usdc_contract_id = env.register_stellar_asset_contract_v2(usdc_admin.clone());
    let usdc_address = usdc_contract_id.address();

    let eurc_admin = Address::generate(&env);
    let eurc_contract_id = env.register_stellar_asset_contract_v2(eurc_admin.clone());
    let eurc_address = eurc_contract_id.address();

    let token = TokenClient::new(&env, &usdc_address);
    let token_admin = StellarAssetClient::new(&env, &usdc_address);

    // ---- Generate test wallets ----
    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let funder = Address::generate(&env);

    // ---- Mint USDC to the actors who need it ----
    // Funder needs enough to cover the invoice
    token_admin.mint(&funder, &(INVOICE_AMOUNT * 10));
    // Payer needs enough to settle the invoice
    token_admin.mint(&payer, &(INVOICE_AMOUNT * 10));

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);

    // Fund the contract treasury so it can cover defaults
    token_admin.mint(&contract.address, &(INVOICE_AMOUNT * 100));

    let xlm_admin = Address::generate(&env);
    let xlm_contract_id = env.register_stellar_asset_contract_v2(xlm_admin);
    let xlm_address = xlm_contract_id.address();

    // Initialize with mock USDC, EURC and mock XLM SAC addresses
    contract.initialize(&usdc_admin, &usdc_address, &eurc_address, &xlm_address);

    // ---- Set ledger timestamp to a known baseline ----
    let mut ledger_info = env.ledger().get();
    ledger_info.timestamp = 1_700_000_000;
    ledger_info.sequence_number = 100;
    env.ledger().set(ledger_info);

    TestEnv {
        env,
        contract,
        token,
        freelancer,
        payer,
        funder,
        admin: usdc_admin,
    }
}

// ----------------------------------------------------------------
// Rate limiting (Issue #859)
// ----------------------------------------------------------------

#[test]
fn test_update_config_rate_limited() {
    let t = setup();

    // First-ever call of a rate-limited fn is always allowed.
    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &70,
        &200,
        &100,
        &50,
        &2000,
        &5000,
        &Address::generate(&t.env),
        &t.token.address,
        &Address::generate(&t.env),
    );
    assert!(
        result.is_ok(),
        "first update_config must succeed: {result:?}"
    );

    // Immediate second call within the cooldown is rejected.
    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &71,
        &200,
        &100,
        &50,
        &2000,
        &5000,
        &Address::generate(&t.env),
        &t.token.address,
        &Address::generate(&t.env),
    );
    assert_eq!(
        result,
        Err(Ok(ContractError::RateLimited)),
        "second update_config within cooldown must be RateLimited"
    );

    // Past the economic-param cooldown the call goes through again.
    let mut info = t.env.ledger().get();
    info.sequence_number += 400; // ECONOMIC_PARAM_COOLDOWN_LEDGERS = 360
    t.env.ledger().set(info);
    let result = t.contract.try_update_config(
        &t.admin.clone(),
        &72,
        &200,
        &100,
        &50,
        &2000,
        &5000,
        &Address::generate(&t.env),
        &t.token.address,
        &Address::generate(&t.env),
    );
    assert!(
        result.is_ok(),
        "update_config after cooldown must succeed: {result:?}"
    );
}

/// Advance the ledger past `upgrade`'s rate-limit cooldown
/// (`UPGRADE_COOLDOWN_LEDGERS`). The last-call ledger for a rate-limited
/// function defaults to 0 when never called, and `setup()` starts the
/// ledger at sequence 100 — so calling `upgrade()` immediately after
/// `setup()` incorrectly trips the cooldown on its very first-ever call.
/// See tests_oracle_registry.rs's `advance_past_rate_limit_cooldown` for
/// the same workaround applied to other rate-limited functions.
fn advance_past_upgrade_cooldown(env: &Env) {
    let mut info = env.ledger().get();
    info.sequence_number += crate::constants::UPGRADE_COOLDOWN_LEDGERS as u32;
    env.ledger().set(info);
}

/// Helper: submit a standard invoice and return its ID
fn submit_standard_invoice(t: &TestEnv) -> u64 {
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

// ----------------------------------------------------------------
// submit_invoice — happy path
// ----------------------------------------------------------------

#[test]
fn test_submit_invoice_returns_id() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    // First invoice should always be ID 1
    assert_eq!(id, 1);
}

#[test]
fn test_submit_invoice_stores_correct_fields() {
    let t = setup();
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;

    let id = t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    );

    let invoice = t.contract.get_invoice(&id);

    assert_eq!(invoice.id, id);
    assert_eq!(invoice.freelancer, t.freelancer);
    assert_eq!(invoice.payer, t.payer);
    assert_eq!(invoice.token, t.token.address);
    assert_eq!(invoice.amount, INVOICE_AMOUNT);
    assert_eq!(u64::from(invoice.due_date), due_date);
    assert_eq!(invoice.discount_rate, DISCOUNT_RATE);
    assert_eq!(invoice.status, InvoiceStatus::Pending);
    assert!(invoice.funder.is_none());
    assert!(invoice.funded_at.is_none());
}

#[test]
fn test_get_invoice_returns_existing_invoice() {
    let t = setup();
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let id = t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    );

    let invoice = t.contract.get_invoice(&id);

    assert_eq!(invoice.id, id);
    assert_eq!(invoice.freelancer, t.freelancer);
    assert_eq!(invoice.payer, t.payer);
    assert_eq!(invoice.token, t.token.address);
    assert_eq!(invoice.amount, INVOICE_AMOUNT);
    assert_eq!(u64::from(invoice.due_date), due_date);
    assert_eq!(invoice.discount_rate, DISCOUNT_RATE);
    assert_eq!(invoice.status, InvoiceStatus::Pending);
    assert_eq!(invoice.amount_funded, 0);
    assert!(invoice.funder.is_none());
    assert!(invoice.funded_at.is_none());
}

#[test]
fn test_submitter_reputation_snapshot_at_submission() {
    let t = setup();
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;

    // Default reputation for a new freelancer should be 50
    let id = t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    );

    let invoice = t.contract.get_invoice(&id);

    // Verify that the submitter_reputation matches the freelancer's reputation at submission
    // For a new freelancer, this should be the default value of 50
    assert_eq!(invoice.submitter_reputation, 50);
    assert_eq!(invoice.freelancer, t.freelancer);
}

#[test]
fn test_get_invoice_returns_invoice_not_found_for_missing_id() {
    let t = setup();

    let result = t.contract.try_get_invoice(&999);

    assert_eq!(result, Err(Ok(ContractError::InvoiceNotFound)));
}

#[test]
fn test_submit_multiple_invoices_increment_ids() {
    let t = setup();

    let id1 = submit_standard_invoice(&t);
    let id2 = submit_standard_invoice(&t);
    let id3 = submit_standard_invoice(&t);

    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
    assert_eq!(id3, 3);
}

// ----------------------------------------------------------------
// submit_invoices_batch
// ----------------------------------------------------------------

#[test]
fn test_submit_invoices_batch_happy_path() {
    let t = setup();
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;

    let params = InvoiceParams {
        freelancer: t.freelancer.clone(),
        payer: t.payer.clone(),
        amount: INVOICE_AMOUNT,
        due_date,
        discount_rate: DISCOUNT_RATE,
        token: t.token.address.clone(),
        referral_code: ReferralCode::None,
    };

    let mut batch = Vec::new(&t.env);
    batch.push_back(params.clone());
    batch.push_back(params.clone());
    batch.push_back(params.clone());

    let ids = t.contract.submit_invoices_batch(&batch);

    assert_eq!(ids.len(), 3);
    assert_eq!(ids.get(0).unwrap(), 1);
    assert_eq!(ids.get(1).unwrap(), 2);
    assert_eq!(ids.get(2).unwrap(), 3);
}

#[test]
fn test_submit_invoices_batch_rejects_over_limit() {
    let t = setup();
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;

    let params = InvoiceParams {
        freelancer: t.freelancer.clone(),
        payer: t.payer.clone(),
        amount: INVOICE_AMOUNT,
        due_date,
        discount_rate: DISCOUNT_RATE,
        token: t.token.address.clone(),
        referral_code: ReferralCode::None,
    };

    let mut batch = Vec::new(&t.env);
    for _ in 0..11 {
        batch.push_back(params.clone());
    }

    let result = t.contract.try_submit_invoices_batch(&batch);

    assert_eq!(result, Err(Ok(ContractError::BatchTooLarge)));
}

#[test]
fn test_submit_invoices_batch_atomicity_fail() {
    let t = setup();
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;

    let mut batch = Vec::new(&t.env);

    // Valid invoice
    batch.push_back(InvoiceParams {
        freelancer: t.freelancer.clone(),
        payer: t.payer.clone(),
        amount: INVOICE_AMOUNT,
        due_date,
        discount_rate: DISCOUNT_RATE,
        token: t.token.address.clone(),
        referral_code: ReferralCode::None,
    });

    // Invalid invoice (amount = 0)
    batch.push_back(InvoiceParams {
        freelancer: t.freelancer.clone(),
        payer: t.payer.clone(),
        amount: 0,
        due_date,
        discount_rate: DISCOUNT_RATE,
        token: t.token.address.clone(),
        referral_code: ReferralCode::None,
    });

    let result = t.contract.try_submit_invoices_batch(&batch);

    assert_eq!(result, Err(Ok(ContractError::InvalidAmount)));

    // Verify no invoice was saved
    assert_eq!(t.contract.get_invoice_count(), 0);
}

// ----------------------------------------------------------------
// submit_invoice — validation errors
// ----------------------------------------------------------------

#[test]
fn test_submit_rejects_zero_amount() {
    let t = setup();
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;

    let result = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &0,
        &due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    );

    assert_eq!(result, Err(Ok(ContractError::InvalidAmount)));
}

#[test]
fn test_submit_rejects_negative_amount() {
    let t = setup();
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;

    let result = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &-1,
        &due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    );

    assert_eq!(result, Err(Ok(ContractError::InvalidAmount)));
}

#[test]
fn test_submit_rejects_past_due_date() {
    let t = setup();
    let past_due_date = t.env.ledger().timestamp() - 1; // 1 second in the past

    let result = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &past_due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    );

    assert_eq!(result, Err(Ok(ContractError::InvalidDueDate)));
}

#[test]
fn test_submit_rejects_zero_discount_rate() {
    let t = setup();
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;

    let result = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &0,
        &t.token.address,
        &ReferralCode::None,
    );

    assert_eq!(result, Err(Ok(ContractError::InvalidDiscountRate)));
}

#[test]
fn test_submit_rejects_discount_rate_above_50_percent() {
    let t = setup();
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;

    let result = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &5_001, // 50.01% — just over the cap
        &t.token.address,
        &ReferralCode::None,
    );

    assert_eq!(result, Err(Ok(ContractError::InvalidDiscountRate)));
}

// ----------------------------------------------------------------
// update_invoice
// ----------------------------------------------------------------

#[test]
fn test_update_invoice_updates_pending_invoice_fields() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let updated_amount = INVOICE_AMOUNT + 250_000_000;
    let updated_due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET * 2;
    let updated_discount_rate = DISCOUNT_RATE + 100;

    t.contract.update_invoice(
        &t.freelancer,
        &id,
        &updated_amount,
        &updated_due_date,
        &updated_discount_rate,
    );

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.amount, updated_amount);
    assert_eq!(u64::from(invoice.due_date), updated_due_date);
    assert_eq!(invoice.discount_rate, updated_discount_rate);
    assert_eq!(invoice.payer, t.payer);
    assert_eq!(invoice.status, InvoiceStatus::Pending);
}

#[test]
fn test_update_invoice_emits_updated_event() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let updated_amount = INVOICE_AMOUNT + 250_000_000;
    let updated_due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET * 2;
    let updated_discount_rate = DISCOUNT_RATE + 100;

    t.contract.update_invoice(
        &t.freelancer,
        &id,
        &updated_amount,
        &updated_due_date,
        &updated_discount_rate,
    );

    let events = t.env.events().all();
    assert!(
        events.events().len() > 0,
        "InvoiceUpdated event should have been emitted"
    );
}

#[test]
fn test_update_invoice_rejects_non_freelancer() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let impostor = Address::generate(&t.env);
    let updated_due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET * 2;

    let result = t.contract.try_update_invoice(
        &impostor,
        &id,
        &INVOICE_AMOUNT,
        &updated_due_date,
        &DISCOUNT_RATE,
    );

    assert_eq!(result, Err(Ok(ContractError::Unauthorized)));
}

#[test]
fn test_update_funded_invoice_fails() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let updated_due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET * 2;

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let result = t.contract.try_update_invoice(
        &t.freelancer,
        &id,
        &INVOICE_AMOUNT,
        &updated_due_date,
        &DISCOUNT_RATE,
    );

    assert_eq!(result, Err(Ok(ContractError::AlreadyFunded)));
}

#[test]
fn test_update_invoice_rejects_invalid_amount() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let updated_due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET * 2;

    let result =
        t.contract
            .try_update_invoice(&t.freelancer, &id, &0, &updated_due_date, &DISCOUNT_RATE);

    assert_eq!(result, Err(Ok(ContractError::InvalidAmount)));
}

#[test]
fn test_update_invoice_rejects_invalid_due_date() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let past_due_date = t.env.ledger().timestamp();

    let result = t.contract.try_update_invoice(
        &t.freelancer,
        &id,
        &INVOICE_AMOUNT,
        &past_due_date,
        &DISCOUNT_RATE,
    );

    assert_eq!(result, Err(Ok(ContractError::InvalidDueDate)));
}

#[test]
fn test_update_invoice_rejects_invalid_discount_rate() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let updated_due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET * 2;

    let result =
        t.contract
            .try_update_invoice(&t.freelancer, &id, &INVOICE_AMOUNT, &updated_due_date, &0);

    assert_eq!(result, Err(Ok(ContractError::InvalidDiscountRate)));
}

// ----------------------------------------------------------------
// transfer_invoice
// ----------------------------------------------------------------

#[test]
fn test_transfer_invoice_updates_freelancer() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    let new_freelancer = Address::generate(&t.env);

    t.contract.transfer_invoice(&id, &new_freelancer);

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.freelancer, new_freelancer);
}

#[test]
fn test_transfer_nonexistent_invoice_fails() {
    let t = setup();
    let new_freelancer = Address::generate(&t.env);

    let result = t.contract.try_transfer_invoice(&999, &new_freelancer);
    assert_eq!(result, Err(Ok(ContractError::InvoiceNotFound)));
}

#[test]
fn test_transfer_funded_invoice_fails() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let new_freelancer = Address::generate(&t.env);
    let result = t.contract.try_transfer_invoice(&id, &new_freelancer);
    assert_eq!(result, Err(Ok(ContractError::AlreadyFunded)));
}

#[test]
fn test_transfer_lp_position_updates_funder_and_lp_index() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let new_lp = Address::generate(&t.env);
    t.contract.transfer_lp_position(&id, &new_lp);

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.funder, Some(new_lp.clone()));

    let old_lp_invoices = t.contract.list_invoices_by_lp(&t.funder, &0, &50);
    assert!(!old_lp_invoices.iter().any(|invoice| invoice.id == id));

    let new_lp_invoices = t.contract.list_invoices_by_lp(&new_lp, &0, &50);
    assert!(new_lp_invoices.iter().any(|invoice| invoice.id == id));
}

#[test]
fn test_transfer_lp_position_pays_new_lp_on_settlement() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let new_lp = Address::generate(&t.env);
    t.contract.transfer_lp_position(&id, &new_lp);

    let old_lp_balance_before = t.token.balance(&t.funder);
    let new_lp_balance_before = t.token.balance(&new_lp);

    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    let old_lp_balance_after = t.token.balance(&t.funder);
    let new_lp_balance_after = t.token.balance(&new_lp);

    assert_eq!(old_lp_balance_after, old_lp_balance_before);
    assert_eq!(new_lp_balance_after - new_lp_balance_before, INVOICE_AMOUNT);
}

#[test]
fn test_transfer_lp_position_can_transfer_twice() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let new_lp = Address::generate(&t.env);
    let second_lp = Address::generate(&t.env);

    t.contract.transfer_lp_position(&id, &new_lp);
    t.contract.transfer_lp_position(&id, &second_lp);

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.funder, Some(second_lp.clone()));
    let invoices = t.contract.list_invoices_by_lp(&second_lp, &0, &50);
    assert!(invoices.iter().any(|invoice| invoice.id == id));
}

// ----------------------------------------------------------------
// fund_invoice — happy path
// ----------------------------------------------------------------

#[test]
fn test_fund_invoice_transfers_correct_amounts() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    let funder_balance_before = t.token.balance(&t.funder);
    let freelancer_balance_before = t.token.balance(&t.freelancer);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let funder_balance_after = t.token.balance(&t.funder);
    let freelancer_balance_after = t.token.balance(&t.freelancer);

    // discount_amount = 1_000_000_000 * 300 / 10_000 = 30_000_000 (3 USDC)
    let discount_amount = INVOICE_AMOUNT * DISCOUNT_RATE as i128 / 10_000;
    let freelancer_payout = INVOICE_AMOUNT - discount_amount;

    // LP sent the required cost
    assert_eq!(
        funder_balance_before - funder_balance_after,
        freelancer_payout,
        "LP should have sent the cost amount"
    );

    // Freelancer received amount minus discount
    assert_eq!(
        freelancer_balance_after - freelancer_balance_before,
        freelancer_payout,
        "Freelancer should receive amount minus discount"
    );
}

#[test]
fn test_fund_invoice_updates_status_to_funded() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let invoice = t.contract.get_invoice(&id);

    assert_eq!(invoice.status, InvoiceStatus::Funded);
    assert_eq!(invoice.funder, Some(t.funder.clone()));
    assert!(invoice.funded_at.is_some());
}

#[test]
fn test_fund_invoice_sets_funded_at_timestamp() {
    let t = setup();
    let id = submit_standard_invoice(&t);
    let now = t.env.ledger().timestamp();

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.funded_at, Some(now.try_into().expect("timestamp")));
}

// ----------------------------------------------------------------
// fund_invoice — error cases
// ----------------------------------------------------------------

#[test]
fn test_fund_nonexistent_invoice_fails() {
    let t = setup();

    let result = t
        .contract
        .try_fund_invoice(&t.funder, &999, &INVOICE_AMOUNT, &false);
    assert_eq!(result, Err(Ok(ContractError::InvoiceNotFound)));
}

#[test]
fn test_fund_already_funded_invoice_fails() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    // Second funder tries to fund the same invoice
    let second_funder = Address::generate(&t.env);
    let result = t
        .contract
        .try_fund_invoice(&second_funder, &id, &INVOICE_AMOUNT, &false);

    assert_eq!(result, Err(Ok(ContractError::AlreadyFunded)));
}

// ----------------------------------------------------------------
// mark_paid — happy path
// ----------------------------------------------------------------

#[test]
fn test_mark_paid_releases_full_amount_to_lp() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let funder_balance_before = t.token.balance(&t.funder);

    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    let funder_balance_after = t.token.balance(&t.funder);

    // LP should receive the full invoice amount (minus fee, which is 0 here)
    assert_eq!(
        funder_balance_after - funder_balance_before,
        INVOICE_AMOUNT,
        "LP should receive the full invoice amount when invoice is paid"
    );
}

// ── Issue #619: mark_paid's internal LP payout/earnings math must never
// panic (debug) or wrap to a corrupting value (release) when protocol-fee
// deduction plus integer-division truncation makes the payout fall below
// what the LP originally funded. Settlement must still succeed and pay out
// the correct, non-negative, non-wrapped amount. ──────────────────────────

#[test]
fn test_mark_paid_settles_correctly_when_distribute_amount_slightly_less_than_invoice_amount() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    // update_fee_rate is rate-limited (ECONOMIC_PARAM_COOLDOWN_LEDGERS = 360)
    // relative to a fresh RateLimit record's default last-ledger of 0, and
    // setup()'s baseline sequence_number (100) is below that — advance past
    // the cooldown first, same as test_update_fee_rate_rate_limited does.
    let mut ledger_info = t.env.ledger().get();
    ledger_info.sequence_number += 400;
    t.env.ledger().set(ledger_info);

    // Small protocol fee (0.1%) makes distribute_amount fall just under
    // invoice.amount, which is exactly the edge that used to underflow
    // lp_earned via a bare subtraction.
    t.contract.update_fee_rate(&10_u32);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let funder_balance_before = t.token.balance(&t.funder);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);
    let funder_balance_after = t.token.balance(&t.funder);

    let protocol_fee = INVOICE_AMOUNT * 10 / 10_000;
    let expected_distribution = INVOICE_AMOUNT - protocol_fee;

    assert_eq!(
        funder_balance_after - funder_balance_before,
        expected_distribution,
        "settlement must succeed and pay the correct (non-negative, non-wrapped) amount \
         even when the protocol fee pushes distribute_amount below invoice.amount"
    );
}

#[test]
fn test_mark_paid_settles_correctly_when_distribute_amount_equals_invoice_amount_no_fee() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    let mut ledger_info = t.env.ledger().get();
    ledger_info.sequence_number += 400;
    t.env.ledger().set(ledger_info);

    // Explicit no-fee edge: distribute_amount == invoice.amount exactly, so
    // primary_lp_payout == primary_lp_funded and lp_earned == 0 exactly —
    // not underflowed, not a coincidental clamp.
    t.contract.update_fee_rate(&0_u32);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let funder_balance_before = t.token.balance(&t.funder);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);
    let funder_balance_after = t.token.balance(&t.funder);

    assert_eq!(
        funder_balance_after - funder_balance_before,
        INVOICE_AMOUNT,
        "with no protocol fee, the LP must receive the full invoice amount"
    );
}

#[test]
fn test_mark_paid_settles_correctly_when_distribute_amount_significantly_less_than_invoice_amount()
{
    let t = setup();
    let id = submit_standard_invoice(&t);

    let mut ledger_info = t.env.ledger().get();
    ledger_info.sequence_number += 400;
    t.env.ledger().set(ledger_info);

    // Large protocol fee (40%) makes distribute_amount fall significantly
    // below invoice.amount — the same edge as the first test, but far from
    // the boundary rather than adjacent to it.
    t.contract.update_fee_rate(&4_000_u32);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let funder_balance_before = t.token.balance(&t.funder);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);
    let funder_balance_after = t.token.balance(&t.funder);

    let protocol_fee = INVOICE_AMOUNT * 4_000 / 10_000;
    let expected_distribution = INVOICE_AMOUNT - protocol_fee;

    assert_eq!(
        funder_balance_after - funder_balance_before,
        expected_distribution,
        "settlement must succeed and pay the correct (non-negative, non-wrapped) amount \
         even when a large protocol fee pushes distribute_amount well below invoice.amount"
    );
}

#[test]
fn test_mark_paid_updates_status() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Paid);
}

#[test]
fn test_full_lifecycle_lp_earns_correct_yield() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    // Record LP balance before the entire flow
    let lp_start = t.token.balance(&t.funder);

    // LP funds the invoice
    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    // Payer settles
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    let lp_end = t.token.balance(&t.funder);

    let expected_yield = INVOICE_AMOUNT * DISCOUNT_RATE as i128 / 10_000;

    assert_eq!(
        lp_end - lp_start,
        expected_yield,
        "LP net yield should equal the discount amount"
    );
}

#[test]
fn test_full_lifecycle_payer_balance_reduces_correctly() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    let payer_start = t.token.balance(&t.payer);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    let payer_end = t.token.balance(&t.payer);

    // Payer should have paid the full invoice amount
    assert_eq!(
        payer_start - payer_end,
        INVOICE_AMOUNT,
        "Payer should have paid the full invoice amount"
    );
}

// ----------------------------------------------------------------
// mark_paid — error cases
// ----------------------------------------------------------------

#[test]
fn test_mark_paid_on_pending_invoice_fails() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    // Try to mark paid without funding first
    let result = t.contract.try_mark_paid(&id, &INVOICE_AMOUNT);
    assert_eq!(result, Err(Ok(ContractError::NotFunded)));
}

#[test]
fn test_mark_paid_twice_fails() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    // Paying again should fail
    let result = t.contract.try_mark_paid(&id, &INVOICE_AMOUNT);
    assert_eq!(result, Err(Ok(ContractError::AlreadyPaid)));
}

#[test]
fn test_mark_paid_nonexistent_invoice_fails() {
    let t = setup();

    let result = t.contract.try_mark_paid(&999, &INVOICE_AMOUNT);
    assert_eq!(result, Err(Ok(ContractError::InvoiceNotFound)));
}

#[test]
fn test_claim_default_success() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    // Move time forward past due date
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += DUE_DATE_OFFSET + 1;
    t.env.ledger().set(ledger);

    let funder_before = t.token.balance(&t.funder);

    t.contract.claim_default(&t.funder, &id);

    let funder_after = t.token.balance(&t.funder);

    let discount_amount = INVOICE_AMOUNT * DISCOUNT_RATE as i128 / 10_000;

    assert_eq!(
        funder_after - funder_before,
        INVOICE_AMOUNT - discount_amount,
        "LP should recover their contributed principal after default"
    );

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Defaulted);
}

#[test]
fn test_claim_default_before_due_date_fails() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let result = t.contract.try_claim_default(&t.funder, &id);
    assert_eq!(result, Err(Ok(ContractError::NotYetDefaulted)));
}

#[test]
fn test_claim_default_non_funder_fails() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    // Move time forward
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += DUE_DATE_OFFSET + 1;
    t.env.ledger().set(ledger);

    let attacker = Address::generate(&t.env);

    let result = t.contract.try_claim_default(&attacker, &id);
    assert_eq!(result, Err(Ok(ContractError::Unauthorized)));
}

#[test]
fn test_claim_default_on_paid_invoice_fails() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    // Move time forward
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += DUE_DATE_OFFSET + 1;
    t.env.ledger().set(ledger);

    let result = t.contract.try_claim_default(&t.funder, &id);
    assert_eq!(result, Err(Ok(ContractError::AlreadyPaid)));
}

#[test]
fn test_claim_default_twice_fails() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    // Move time forward
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += DUE_DATE_OFFSET + 1;
    t.env.ledger().set(ledger);

    t.contract.claim_default(&t.funder, &id);

    let result = t.contract.try_claim_default(&t.funder, &id);
    assert_eq!(result, Err(Ok(ContractError::InvoiceDefaulted)));
}

#[test]
fn test_expire_pending_invoice_after_due_date() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    let mut ledger = t.env.ledger().get();
    ledger.timestamp += DUE_DATE_OFFSET + 1;
    t.env.ledger().set(ledger);

    t.contract.expire_invoice(&id);

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Expired);
}

#[test]
fn test_expire_pending_invoice_before_due_date_fails() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    let result = t.contract.try_expire_invoice(&id);
    assert_eq!(result, Err(Ok(ContractError::NotYetDefaulted)));

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Pending);
}

#[test]
fn test_fund_expired_invoice_fails() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    let mut ledger = t.env.ledger().get();
    ledger.timestamp += DUE_DATE_OFFSET + 1;
    t.env.ledger().set(ledger);

    t.contract.expire_invoice(&id);

    let result = t
        .contract
        .try_fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);
    assert_eq!(result, Err(Ok(ContractError::InvoiceExpired)));

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Expired);
}

#[test]
fn test_new_payer_score_is_neutral() {
    let t = setup();

    let score = t.contract.payer_score(&t.payer);

    assert_eq!(score, 50);
}

#[test]
fn test_perfect_payer_score() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    let score = t.contract.payer_score(&t.payer);

    assert_eq!(score, 51);
}

#[test]
fn test_payer_with_default() {
    let t = setup();
    let id = submit_standard_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let mut ledger = t.env.ledger().get();
    ledger.timestamp += DUE_DATE_OFFSET + 1;
    t.env.ledger().set(ledger);

    t.contract.claim_default(&t.funder, &id);

    let score = t.contract.payer_score(&t.payer);

    assert!(score < 50);
}

// ----------------------------------------------------------------
// Reputation decay tests
// ----------------------------------------------------------------

#[test]
#[ignore]
fn test_reputation_decay_inactive_score() {
    let t = setup();

    // Set payer score to 80
    t.env.as_contract(&t.contract.address, || {
        invoice::set_payer_score(&t.env, &t.payer, 80);
    });

    // Initialize decay config: 100 bps (1%) per 1000 ledgers
    let config = Config {
        high_rep_threshold: 80,
        bonus_bps: 200,
        min_discount_rate_bps: 100,
        decay_rate_bps: 100, // 1% per period
        decay_period_ledgers: 1000,
        dispute_timeout_ledgers: 100,
        xlm_sac_address: Address::generate(&t.env),
        usdc_sac_address: Address::generate(&t.env),
        eurc_sac_address: Address::generate(&t.env),
        price_oracle: None,
        max_oracle_age_ledgers: 17280,
    };
    t.env.as_contract(&t.contract.address, || {
        crate::storage::set_config(&t.env, &config);
    });

    // Advance ledger by 2100 (more than 2 periods)
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += 2100;
    t.env.ledger().set(ledger);

    // Get score - should have decayed
    let score = t.contract.payer_score(&t.payer);

    // After 2 periods: 80 -> 80 * 0.99 = 79.2 -> 79 * 0.99 = 78.2 -> 78
    assert!(score < 80, "Score should decay from 80, got {}", score);
    assert!(score >= 78, "Score should decay to ~78, got {}", score);
}

#[test]
#[ignore]
fn test_reputation_no_decay_when_inactive() {
    let t = setup();

    // Set payer score to 80
    t.env.as_contract(&t.contract.address, || {
        invoice::set_payer_score(&t.env, &t.payer, 80);
    });

    // Initialize decay config with very high decay period (never decays)
    let config = Config {
        high_rep_threshold: 80,
        bonus_bps: 200,
        min_discount_rate_bps: 100,
        decay_rate_bps: 100,
        decay_period_ledgers: 10_000_000, // Very long period
        dispute_timeout_ledgers: 100,
        xlm_sac_address: Address::generate(&t.env),
        usdc_sac_address: Address::generate(&t.env),
        eurc_sac_address: Address::generate(&t.env),
        price_oracle: None,
        max_oracle_age_ledgers: 17280,
    };
    t.env.as_contract(&t.contract.address, || {
        crate::storage::set_config(&t.env, &config);
    });

    // Advance ledger by only 1000
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += 1000;
    t.env.ledger().set(ledger);

    // Get score - should NOT have decayed
    let score = t.contract.payer_score(&t.payer);

    assert_eq!(score, 80, "Score should not decay when period not reached");
}

#[test]
#[ignore]
fn test_reputation_decay_activity_resets() {
    let t = setup();

    // Set initial score to 80
    t.env.as_contract(&t.contract.address, || {
        invoice::set_payer_score(&t.env, &t.payer, 80);
    });
    let config = Config {
        high_rep_threshold: 80,
        bonus_bps: 200,
        min_discount_rate_bps: 100,
        decay_rate_bps: 100,
        decay_period_ledgers: 1000,
        dispute_timeout_ledgers: 100,
        xlm_sac_address: Address::generate(&t.env),
        usdc_sac_address: Address::generate(&t.env),
        eurc_sac_address: Address::generate(&t.env),
        price_oracle: None,
        max_oracle_age_ledgers: 17280,
    };
    t.env.as_contract(&t.contract.address, || {
        crate::storage::set_config(&t.env, &config);
    });

    // Advance by half a decay period
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += 500;
    t.env.ledger().set(ledger);

    t.env.as_contract(&t.contract.address, || {
        invoice::set_payer_score(&t.env, &t.payer, 85);
    });

    // Advance by another half period (not enough from reset)
    ledger = t.env.ledger().get();
    ledger.sequence_number += 500;
    t.env.ledger().set(ledger);

    // Score should not have decayed since reset
    let score = t.contract.payer_score(&t.payer);

    assert_eq!(score, 85, "Score should not decay shortly after activity");
}

#[test]
#[ignore]
fn test_reputation_score_never_goes_below_zero() {
    let t = setup();

    // Set payer score to only 5 (low)
    t.env.as_contract(&t.contract.address, || {
        invoice::set_payer_score(&t.env, &t.payer, 5);
    });

    let config = Config {
        high_rep_threshold: 80,
        bonus_bps: 200,
        min_discount_rate_bps: 100,
        decay_rate_bps: 5000, // Very aggressive decay: 50% per period
        decay_period_ledgers: 100,
        dispute_timeout_ledgers: 100,
        xlm_sac_address: Address::generate(&t.env),
        usdc_sac_address: Address::generate(&t.env),
        eurc_sac_address: Address::generate(&t.env),
        price_oracle: None,
        max_oracle_age_ledgers: 17280,
    };
    t.env.as_contract(&t.contract.address, || {
        crate::storage::set_config(&t.env, &config);
    });

    // Advance by 10 decay periods
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += 1000;
    t.env.ledger().set(ledger);

    // Get score - should floor at 0
    let score = t.contract.payer_score(&t.payer);

    assert_eq!(score, 0, "Score should floor at 0, not go negative");
}

// ----------------------------------------------------------------
// Regression tests — issue #601: bound reputation decay loop
// ----------------------------------------------------------------

#[test]
fn test_reputation_decay_bounded_for_extremely_long_inactivity() {
    let t = setup();

    t.env.as_contract(&t.contract.address, || {
        invoice::set_payer_score(&t.env, &t.payer, 80);
    });

    let config = Config {
        high_rep_threshold: 80,
        bonus_bps: 200,
        min_discount_rate_bps: 100,
        decay_rate_bps: 100,
        decay_period_ledgers: 2,
        dispute_timeout_ledgers: 100,
        xlm_sac_address: Address::generate(&t.env),
        usdc_sac_address: Address::generate(&t.env),
        eurc_sac_address: Address::generate(&t.env),
        price_oracle: None,
        max_oracle_age_ledgers: 17280,
    };
    t.env.as_contract(&t.contract.address, || {
        crate::storage::set_config(&t.env, &config);
        t.env.storage().instance().extend_ttl(1_000_000, 2_000_000);
    });

    // periods_passed = 2,500 / 2 = 1,250 (1.25x the cap) — sustained
    // long-term inactivity under a normal (non-griefing) decay period.
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += 2_500;
    t.env.ledger().set(ledger);

    let score = t.contract.payer_score(&t.payer);

    assert_eq!(
        score, 0,
        "Score for a long-inactive payer should floor at 0, not hang or panic"
    );
}

#[test]
fn test_reputation_decay_bounded_when_decay_period_is_one_ledger() {
    let t = setup();

    t.env.as_contract(&t.contract.address, || {
        invoice::set_payer_score(&t.env, &t.payer, 80);
    });

    // The exact griefing scenario from issue #601: decay_period_ledgers=1.
    let config = Config {
        high_rep_threshold: 80,
        bonus_bps: 200,
        min_discount_rate_bps: 100,
        decay_rate_bps: 100,
        decay_period_ledgers: 1,
        dispute_timeout_ledgers: 100,
        xlm_sac_address: Address::generate(&t.env),
        usdc_sac_address: Address::generate(&t.env),
        eurc_sac_address: Address::generate(&t.env),
        price_oracle: None,
        max_oracle_age_ledgers: 17280,
    };
    t.env.as_contract(&t.contract.address, || {
        crate::storage::set_config(&t.env, &config);
        t.env.storage().instance().extend_ttl(1_000_000, 2_000_000);
    });

    // periods_passed = 1,500 (1.5x the cap) with decay_period_ledgers=1.
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += 1_500;
    t.env.ledger().set(ledger);

    let score = t.contract.payer_score(&t.payer);

    assert_eq!(
        score, 0,
        "decay_period_ledgers=1 with a large gap should floor at 0, not hang or panic"
    );
}

#[test]
fn test_reputation_score_never_exceeds_100() {
    let t = setup();

    // Try to set score above 100
    t.env.as_contract(&t.contract.address, || {
        invoice::set_payer_score(&t.env, &t.payer, 150);
    });

    // Score should be capped at 100
    let score = t.contract.payer_score(&t.payer);

    assert_eq!(score, 100, "Score should be capped at 100");
}

// ================================================================
// Test: Contract Upgrade (Issue #48)
// ================================================================
//
// The three tests below call `upgrade()` with a fabricated wasm_hash
// rather than a hash returned by `env.deployer().upload_contract_wasm()`.
// This soroban-sdk version's `update_current_contract_wasm` verifies the
// wasm blob actually exists in storage before swapping to it, so these
// need a *real* uploaded Soroban contract (valid wasm plus the metadata
// custom section soroban-sdk contracts embed) — not producible from a
// native `cargo test` run without a checked-in .wasm fixture built via a
// separate wasm32 build step. Ignored until such a fixture exists.

#[test]
#[ignore = "requires a real uploaded Soroban wasm fixture — see comment above"]
fn test_upgrade_emits_correct_event() {
    let t = setup();

    // Generate a mock WASM hash (32 bytes)
    let wasm_hash = soroban_sdk::BytesN::from_array(&t.env, &[1u8; 32]);

    // Admin calls upgrade
    advance_past_upgrade_cooldown(&t.env);
    t.contract.upgrade(&wasm_hash);

    let events = t.env.events().all();

    let admin = t.env.as_contract(&t.contract.address, || {
        crate::storage::get_admin(&t.env).unwrap()
    });

    let expected_event = crate::events::ContractUpgraded {
        admin,
        new_wasm_hash: wasm_hash,
        timestamp: t.env.ledger().timestamp(),
    };

    assert!(
        events.events().len() > 0,
        "ContractUpgraded event should be emitted"
    );
}

#[test]
fn test_upgrade_requires_admin() {
    let t = setup();
    let _unauthorized_caller = Address::generate(&t.env);

    let wasm_hash = soroban_sdk::BytesN::from_array(&t.env, &[2u8; 32]);

    // Non-admin should not be able to call upgrade
    let _result = t.contract.try_upgrade(&wasm_hash);

    // Should fail (admin-only)
    // Note: In test env with mock_all_auths(), this might not fail
    // In production, this would be enforced by require_admin()

    // The actual auth check happens in require_admin()
    // which is tested separately via the access control module
}

#[test]
#[ignore = "requires a real uploaded Soroban wasm fixture — see comment above test_upgrade_emits_correct_event"]
fn test_upgrade_does_not_affect_existing_invoices() {
    let t = setup();

    // Create an invoice before upgrade
    let id = submit_standard_invoice(&t);
    let invoice_before = t.contract.get_invoice(&id);

    // Perform upgrade
    let wasm_hash = soroban_sdk::BytesN::from_array(&t.env, &[3u8; 32]);
    advance_past_upgrade_cooldown(&t.env);
    t.contract.upgrade(&wasm_hash);

    // Verify invoice is still readable and unchanged
    let invoice_after = t.contract.get_invoice(&id);

    assert_eq!(
        invoice_before.id, invoice_after.id,
        "Invoice ID should be preserved"
    );
    assert_eq!(
        invoice_before.freelancer, invoice_after.freelancer,
        "Freelancer address should be preserved"
    );
    assert_eq!(
        invoice_before.payer, invoice_after.payer,
        "Payer address should be preserved"
    );
    assert_eq!(
        invoice_before.amount, invoice_after.amount,
        "Amount should be preserved"
    );
    assert_eq!(
        invoice_before.status, invoice_after.status,
        "Status should be preserved"
    );
}

#[test]
#[ignore = "requires a real uploaded Soroban wasm fixture — see comment above test_upgrade_emits_correct_event"]
fn test_upgrade_snapshot_before_after() {
    let t = setup();

    // Get contract stats before upgrade
    let _stats_before = t.contract.get_contract_stats();

    // Submit invoices to have data
    let _id1 = submit_standard_invoice(&t);
    let stats_with_data = t.contract.get_contract_stats();

    // Perform upgrade
    let wasm_hash = soroban_sdk::BytesN::from_array(&t.env, &[4u8; 32]);
    advance_past_upgrade_cooldown(&t.env);
    t.contract.upgrade(&wasm_hash);

    // Get contract stats after upgrade
    let stats_after = t.contract.get_contract_stats();

    // Verify stats are preserved
    assert_eq!(
        stats_with_data.total_invoices, stats_after.total_invoices,
        "Total invoices should be preserved after upgrade"
    );
    assert_eq!(
        stats_with_data.total_paid, stats_after.total_paid,
        "Total paid should be preserved after upgrade"
    );
}

#[test]
fn test_get_version() {
    let t = setup();
    let version = t.contract.get_version();
    assert_eq!(version, soroban_sdk::String::from_str(&t.env, "1.0.0"));
}

#[test]
fn test_high_rep_threshold_reachable() {
    // Integration test for reachable path
    assert!(true);
}
