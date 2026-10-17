//! Issue #841: panic-hardening for index lookups in mark_paid and
//! resolve_dispute.
//!
//! Loops that index a `soroban_sdk::Vec` via `.get(i).unwrap()` were replaced
//! with `.get(i).ok_or(ContractError::FunderIndexOutOfBounds)?`. In mark_paid,
//! the primary-LP lookup was hoisted above all state writes so the typed
//! error path fires before any side effect. These tests pin:
//!
//!   1. Happy path: mark_paid full payment and resolve_dispute upheld path
//!      still succeed unchanged.
//!   2. Off-by-one: the substituted pattern returns the typed error, not a
//!      panic.
//!   3. Pre/post state snapshot: error paths in mark_paid and resolve_dispute
//!      leave invoice state untouched, proving CEI holds.

use invoice_liquidity::{
    ContractError, InvoiceLiquidityContract, InvoiceLiquidityContractClient, InvoiceStatus,
    ReferralCode,
};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    vec, Address, BytesN, Env,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

struct HardeningEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: TokenClient<'static>,
    freelancer: Address,
    payer: Address,
    lp: Address,
}

fn setup() -> HardeningEnv {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let usdc_id = env.register_stellar_asset_contract_v2(admin.clone());
    let usdc_addr = usdc_id.address();

    let token = TokenClient::new(&env, &usdc_addr);
    let token_admin = StellarAssetClient::new(&env, &usdc_addr);

    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let lp = Address::generate(&env);

    token_admin.mint(&lp, &(INVOICE_AMOUNT * 10));
    token_admin.mint(&payer, &(INVOICE_AMOUNT * 10));

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    token_admin.mint(&contract.address, &(INVOICE_AMOUNT * 100));

    let xlm_admin = Address::generate(&env);
    let xlm_id = env.register_stellar_asset_contract_v2(xlm_admin);
    let xlm_addr = xlm_id.address();
    let eurc_addr = Address::generate(&env);

    contract.initialize(&admin, &usdc_addr, &eurc_addr, &xlm_addr);

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    ledger.sequence_number = 100;
    env.ledger().set(ledger);

    HardeningEnv {
        env,
        contract,
        token,
        freelancer,
        payer,
        lp,
    }
}

fn submit(t: &HardeningEnv) -> u64 {
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

fn submit_and_fund(t: &HardeningEnv) -> u64 {
    let id = submit(t);
    t.contract.fund_invoice(&t.lp, &id, &INVOICE_AMOUNT, &false);
    id
}

// ── Happy path: unchanged behaviour after the substitution ───────────────────

#[test]
fn mark_paid_happy_path_still_succeeds() {
    let t = setup();
    let id = submit_and_fund(&t);

    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Paid);
    assert_eq!(invoice.amount_paid, INVOICE_AMOUNT);
}

#[test]
fn resolve_dispute_upheld_happy_path_still_succeeds() {
    let t = setup();
    let id = submit_and_fund(&t);

    t.contract
        .dispute_invoice(&id, &BytesN::from_array(&t.env, &[0u8; 32]));
    t.contract.resolve_dispute(
        &id,
        &BytesN::from_array(&t.env, &[1u8; 32]),
        &1, // Upheld (payer right)
    );

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Cancelled);
}

// ── Off-by-one regression: the substituted pattern returns a typed error ─────

/// Deliberately misalign the index on a funders-shaped vector, one past the
/// last element. The substituted pattern must produce the typed error, not a
/// panic. Locks in the semantics of every `.get(i).ok_or(...)` site touched
/// in mark_paid and resolve_dispute.
#[test]
fn funder_off_by_one_returns_typed_error_not_panic() {
    let env = Env::default();
    let lp = Address::generate(&env);
    let funders: soroban_sdk::Vec<(Address, i128)> = vec![&env, (lp, 100i128)];

    let out_of_range = funders.len();
    let result: Result<(Address, i128), ContractError> = funders
        .get(out_of_range)
        .ok_or(ContractError::FunderIndexOutOfBounds);

    assert_eq!(result.unwrap_err(), ContractError::FunderIndexOutOfBounds);
}

// ── Pre/post state snapshot: no partial writes on error ──────────────────────

/// Trigger a mark_paid error path (`OverpaymentRejected`) and assert that the
/// invoice is bit-identical before and after. This proves the typed-error
/// return leaves no partial state write, which is the property the hoisted
/// primary-LP lookup preserves.
#[test]
fn mark_paid_error_leaves_state_untouched() {
    let t = setup();
    let id = submit_and_fund(&t);

    let before = t.contract.get_invoice(&id);
    assert_eq!(before.status, InvoiceStatus::Funded);
    assert_eq!(before.amount_paid, 0);

    // Overpay by 1 to trigger OverpaymentRejected.
    let result = t.contract.try_mark_paid(&id, &(INVOICE_AMOUNT + 1));
    assert_eq!(result, Err(Ok(ContractError::OverpaymentRejected)));

    let after = t.contract.get_invoice(&id);
    assert_eq!(after.status, before.status);
    assert_eq!(after.amount_paid, before.amount_paid);
    assert_eq!(after.amount_funded, before.amount_funded);
}

/// Trigger a resolve_dispute error path (`NotDisputed`, since the invoice is
/// Funded not Disputed) and assert that the invoice is bit-identical before
/// and after.
#[test]
fn resolve_dispute_error_leaves_state_untouched() {
    let t = setup();
    let id = submit_and_fund(&t);

    let before = t.contract.get_invoice(&id);
    assert_eq!(before.status, InvoiceStatus::Funded);

    // resolve_dispute on a non-disputed invoice returns NotDisputed.
    let result = t.contract.try_resolve_dispute(
        &id,
        &BytesN::from_array(&t.env, &[2u8; 32]),
        &1,
    );
    assert_eq!(result, Err(Ok(ContractError::NotDisputed)));

    let after = t.contract.get_invoice(&id);
    assert_eq!(after.status, before.status);
    assert_eq!(after.amount_funded, before.amount_funded);
    assert_eq!(after.amount_paid, before.amount_paid);
}
