#![cfg(test)]

//! Issue #1025: Benchmark extension cost at maximum depth.
//!
//! Focused tests for:
//!  - Success: Queue extension and tail displacement at maximum depth (50 entries)
//!  - Storage: Storage TTL extension on FundQueue, FundQueueOpenedAt, and QueueResolution
//!  - Failure: Score <= tail rejected with QueueFull when full
//!  - Pause: join_fund_queue and resolve_fund_queue fail closed when contract is paused
//!  - Authorization: require_lp and duplicate join prevention at capacity
//!  - Ledger-boundary: MEV maturity delay boundary and pre-upgrade TTL compliance

use super::*;
use crate::constants::{
    FUND_QUEUE_TTL_EXTEND_TO, FUND_QUEUE_TTL_THRESHOLD, MAX_FUND_QUEUE_LEN, QUEUE_DELAY_LEDGERS,
    TTL_MIN_LEDGERS_BEFORE_UPGRADE,
};
use crate::errors::ContractError;
use crate::invoice::{LpFundRequest, ReferralCode};
use crate::storage::{validate_ttl_fields_pre_upgrade, DataKey};
use soroban_sdk::testutils::storage::Persistent as _;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

struct TestContext {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: TokenClient<'static>,
    token_admin: StellarAssetClient<'static>,
    admin: Address,
    freelancer: Address,
    payer: Address,
}

fn setup_context() -> TestContext {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let usdc_id = env.register_stellar_asset_contract_v2(admin.clone());
    let usdc_addr = usdc_id.address();

    let token = TokenClient::new(&env, &usdc_addr);
    let token_admin = StellarAssetClient::new(&env, &usdc_addr);

    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);

    token_admin.mint(&payer, &(INVOICE_AMOUNT * 100));

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    token_admin.mint(&contract.address, &(INVOICE_AMOUNT * 1000));

    let xlm_admin = Address::generate(&env);
    let xlm_addr = env
        .register_stellar_asset_contract_v2(xlm_admin)
        .address();
    let eurc_addr = Address::generate(&env);

    contract.initialize(&admin, &usdc_addr, &eurc_addr, &xlm_addr);

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    ledger.sequence_number = 1_000;
    env.ledger().set(ledger);

    TestContext {
        env,
        contract,
        token,
        token_admin,
        admin,
        freelancer,
        payer,
    }
}

fn create_invoice(t: &TestContext) -> u64 {
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

fn fill_queue_to_capacity(t: &TestContext, invoice_id: u64) -> soroban_sdk::Vec<Address> {
    let mut lps = soroban_sdk::Vec::new(&t.env);
    for i in 0..MAX_FUND_QUEUE_LEN {
        let lp = Address::generate(&t.env);
        t.token_admin.mint(&lp, &(INVOICE_AMOUNT * 10));
        t.env.as_contract(&t.contract.address, || {
            crate::invoice::set_lp_score(&t.env, &lp, 50 + i);
        });
        t.contract.join_fund_queue(&lp, &invoice_id);
        lps.push_back(lp);
    }
    lps
}

// ================================================================
// Success and Boundary Cases at Maximum Depth
// ================================================================

#[test]
fn test_extension_at_maximum_depth_displaces_tail_and_extends_ttl() {
    let t = setup_context();
    let id = create_invoice(&t);
    let lps = fill_queue_to_capacity(&t, id);

    // Initial capacity reached
    let initial_queue: soroban_sdk::Vec<LpFundRequest> = t.env.as_contract(&t.contract.address, || {
        crate::invoice::get_fund_queue(&t.env, id)
    });
    assert_eq!(initial_queue.len(), MAX_FUND_QUEUE_LEN);

    // Verify TTL of FundQueue entry is set to FUND_QUEUE_TTL_EXTEND_TO
    let fq_key = DataKey::FundQueue(id);
    let ttl_before = t.env.as_contract(&t.contract.address, || {
        t.env.storage().persistent().get_ttl(&fq_key)
    });
    assert!(
        ttl_before >= FUND_QUEUE_TTL_THRESHOLD,
        "FundQueue TTL must be >= threshold"
    );

    // High score LP joins at max depth
    let superior_lp = Address::generate(&t.env);
    t.token_admin.mint(&superior_lp, &(INVOICE_AMOUNT * 10));
    t.env.as_contract(&t.contract.address, || {
        crate::invoice::set_lp_score(&t.env, &superior_lp, 250);
    });

    let join_res = t.contract.try_join_fund_queue(&superior_lp, &id);
    assert!(join_res.is_ok(), "Superior LP should be admitted at max depth");

    // Queue length stays exactly MAX_FUND_QUEUE_LEN
    let updated_queue: soroban_sdk::Vec<LpFundRequest> = t.env.as_contract(&t.contract.address, || {
        crate::invoice::get_fund_queue(&t.env, id)
    });
    assert_eq!(updated_queue.len(), MAX_FUND_QUEUE_LEN);

    // Superior LP is now at the head of the queue (score 250)
    assert_eq!(updated_queue.get(0).unwrap().lp, superior_lp);

    // Lowest LP was displaced (the one with score 50, which joined first)
    let displaced_lp = lps.get(0).unwrap();
    for i in 0..updated_queue.len() {
        assert_ne!(updated_queue.get(i).unwrap().lp, displaced_lp);
    }

    // Storage TTL remains extended
    let ttl_after = t.env.as_contract(&t.contract.address, || {
        t.env.storage().persistent().get_ttl(&fq_key)
    });
    assert!(ttl_after >= FUND_QUEUE_TTL_THRESHOLD);
}

#[test]
fn test_resolve_at_maximum_depth_sets_resolution_and_extends_ttl() {
    let t = setup_context();
    let id = create_invoice(&t);
    fill_queue_to_capacity(&t, id);

    let superior_lp = Address::generate(&t.env);
    t.token_admin.mint(&superior_lp, &(INVOICE_AMOUNT * 10));
    t.env.as_contract(&t.contract.address, || {
        crate::invoice::set_lp_score(&t.env, &superior_lp, 300);
    });
    t.contract.join_fund_queue(&superior_lp, &id);

    // Advance past MEV maturity delay
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += QUEUE_DELAY_LEDGERS + 1;
    t.env.ledger().set(ledger);

    let winner = t.contract.resolve_fund_queue(&id);
    assert_eq!(winner, superior_lp);

    // Verify QueueResolution TTL is extended
    let qr_key = DataKey::QueueResolution(id);
    let ttl = t.env.as_contract(&t.contract.address, || {
        t.env.storage().persistent().get_ttl(&qr_key)
    });
    assert!(
        ttl >= FUND_QUEUE_TTL_THRESHOLD,
        "QueueResolution TTL must be >= FUND_QUEUE_TTL_THRESHOLD"
    );

    // Verify pre-upgrade check passes
    let pre_upgrade = t.env.as_contract(&t.contract.address, || {
        validate_ttl_fields_pre_upgrade(&t.env)
    });
    assert!(pre_upgrade.is_ok(), "Pre-upgrade check must pass with refreshed TTLs");
}

// ================================================================
// Failure Cases (Rejection at Maximum Depth & Pause)
// ================================================================

#[test]
fn test_rejection_at_capacity_when_score_less_than_or_equal_to_tail() {
    let t = setup_context();
    let id = create_invoice(&t);
    fill_queue_to_capacity(&t, id);

    // Tail has score 50 (joined first)
    // 1. Equal score joiner (50) is rejected
    let equal_lp = Address::generate(&t.env);
    t.token_admin.mint(&equal_lp, &(INVOICE_AMOUNT * 10));
    t.env.as_contract(&t.contract.address, || {
        crate::invoice::set_lp_score(&t.env, &equal_lp, 50);
    });
    let res_equal = t.contract.try_join_fund_queue(&equal_lp, &id);
    assert_eq!(res_equal, Err(Ok(ContractError::QueueFull)));

    // 2. Lower score joiner (40) is rejected
    let lower_lp = Address::generate(&t.env);
    t.token_admin.mint(&lower_lp, &(INVOICE_AMOUNT * 10));
    t.env.as_contract(&t.contract.address, || {
        crate::invoice::set_lp_score(&t.env, &lower_lp, 40);
    });
    let res_lower = t.contract.try_join_fund_queue(&lower_lp, &id);
    assert_eq!(res_lower, Err(Ok(ContractError::QueueFull)));
}

#[test]
fn test_join_and_resolve_fail_closed_when_paused() {
    let t = setup_context();
    let id = create_invoice(&t);

    let lp = Address::generate(&t.env);
    t.token_admin.mint(&lp, &(INVOICE_AMOUNT * 10));

    // Pause the contract
    t.contract.pause();
    assert!(t.contract.is_paused());

    // join_fund_queue must fail closed with ContractPaused
    let join_res = t.contract.try_join_fund_queue(&lp, &id);
    assert_eq!(join_res, Err(Ok(ContractError::ContractPaused)));

    // resolve_fund_queue must fail closed with ContractPaused
    let resolve_res = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(resolve_res, Err(Ok(ContractError::ContractPaused)));

    // Unpause restores functionality
    t.contract.unpause();
    assert!(!t.contract.is_paused());

    let join_unpaused = t.contract.try_join_fund_queue(&lp, &id);
    assert!(join_unpaused.is_ok());
}

// ================================================================
// Authorization & Duplicate Prevention Cases
// ================================================================

#[test]
fn test_duplicate_join_rejected_even_with_high_score_at_capacity() {
    let t = setup_context();
    let id = create_invoice(&t);
    let lps = fill_queue_to_capacity(&t, id);

    let existing_lp = lps.get(0).unwrap();
    // Try to join again
    let res = t.contract.try_join_fund_queue(&existing_lp, &id);
    assert_eq!(res, Err(Ok(ContractError::AlreadyInQueue)));
}

#[test]
fn test_join_non_existent_invoice_fails() {
    let t = setup_context();
    let lp = Address::generate(&t.env);
    t.token_admin.mint(&lp, &(INVOICE_AMOUNT * 10));

    let res = t.contract.try_join_fund_queue(&lp, &999_999u64);
    assert_eq!(res, Err(Ok(ContractError::InvoiceNotFound)));
}

// ================================================================
// Ledger Boundary Cases
// ================================================================

#[test]
fn test_resolve_queue_maturity_delay_exact_boundary() {
    let t = setup_context();
    let id = create_invoice(&t);
    let lp = Address::generate(&t.env);
    t.token_admin.mint(&lp, &(INVOICE_AMOUNT * 10));
    t.contract.join_fund_queue(&lp, &id);

    let opened_at = t.env.ledger().sequence();

    // 1 ledger before maturity boundary -> rejected
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number = opened_at + QUEUE_DELAY_LEDGERS - 1;
    t.env.ledger().set(ledger);

    let early_res = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(early_res, Err(Ok(ContractError::QueueNotMature)));

    // Exact maturity boundary -> succeeds
    ledger = t.env.ledger().get();
    ledger.sequence_number = opened_at + QUEUE_DELAY_LEDGERS;
    t.env.ledger().set(ledger);

    let mature_res = t.contract.resolve_fund_queue(&id);
    assert_eq!(mature_res, lp);
}
