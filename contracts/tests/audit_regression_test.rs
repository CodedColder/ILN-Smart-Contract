//! Issues #1094 (guards on #1070's fail-closed decimals), #1095, #1096 — regression suite for the formal-verification
//! audit paths (docs/formal-verification.md,
//! docs/formal-verification-cross-contract.md,
//! contracts/tests/cross_contract_invariants_test.rs).
//!
//! Each test names the control it pins in docs/audit-evidence-remediation-map.md.
//!
//! Run: `cargo test -p invoice_liquidity --test audit_regression_test`
//!
//!  - C1  token decimals are present for every allowlisted token (fail closed)
//!  - C3  the LP fund queue is bounded and displacement is strictly score-ordered

#![cfg(test)]

extern crate std;

use invoice_liquidity::{
    constants::{MAX_FUND_QUEUE_LEN, QUEUE_DELAY_LEDGERS},
    ContractError, DataKey, InvoiceLiquidityContract, InvoiceLiquidityContractClient, ReferralCode,
};
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

struct T {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: TokenClient<'static>,
    eurc: Address,
    freelancer: Address,
    payer: Address,
}

fn setup() -> T {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let usdc_addr = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let xlm_addr = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let eurc = Address::generate(&env);

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    contract.initialize(&admin, &usdc_addr, &eurc, &xlm_addr);

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    env.ledger().set(ledger);

    let token = TokenClient::new(&env, &usdc_addr);
    StellarAssetClient::new(&env, &usdc_addr).mint(&contract.address, &(INVOICE_AMOUNT * 10));

    T {
        env: env.clone(),
        contract,
        token,
        eurc,
        freelancer: Address::generate(&env),
        payer: Address::generate(&env),
    }
}

fn submit(t: &T, token: &Address) -> Result<u64, ContractError> {
    let due = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    match t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due,
        &DISCOUNT_RATE,
        token,
        &ReferralCode::None,
    ) {
        Ok(Ok(id)) => Ok(id),
        Err(Ok(e)) => Err(e),
        other => panic!("unexpected result: {:?}", other),
    }
}

fn set_lp_score(t: &T, lp: &Address, score: u32) {
    t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .persistent()
            .set(&DataKey::LpScore(lp.clone()), &score);
    });
}

fn queue_of(t: &T, id: u64) -> soroban_sdk::Vec<invoice_liquidity::invoice::LpFundRequest> {
    t.env.as_contract(&t.contract.address, || {
        invoice_liquidity::invoice::get_fund_queue(&t.env, id)
    })
}

// ── C1: token decimal presence (#1094) ───────────────────────────────────────

#[test]
fn c1_initialize_registers_decimals_for_all_core_tokens() {
    let t = setup();
    assert_eq!(t.contract.get_token_decimals(&t.token.address), Some(6));
    // Baseline (pre-#1094): EURC was allowlisted but had no decimals entry.
    assert_eq!(t.contract.get_token_decimals(&t.eurc), Some(6));
}

#[test]
fn c1_submit_invoice_succeeds_when_decimals_present() {
    let t = setup();
    assert!(submit(&t, &t.token.address).is_ok());
}

#[test]
fn c1_missing_decimals_fails_closed_instead_of_assuming_six() {
    let t = setup();
    // Simulate a legacy allowlisted token that never had decimals recorded.
    t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .persistent()
            .remove(&DataKey::TokenDecimals(t.token.address.clone()));
    });
    assert_eq!(t.contract.get_token_decimals(&t.token.address), None);
    assert_eq!(
        submit(&t, &t.token.address),
        Err(ContractError::TokenDecimalsNotConfigured)
    );
    // No invoice id was consumed by the rejected submission.
    assert_eq!(t.contract.get_invoice_count(), 0);
}

#[test]
fn c1_unlisted_token_still_unauthorized_not_decimals_error() {
    let t = setup();
    let rogue = Address::generate(&t.env);
    assert_eq!(submit(&t, &rogue), Err(ContractError::Unauthorized));
}

// Issue #1051: the amount floor must be enforced solely through the token's
// registered decimals. `update_invoice` no longer runs the decimals-blind
// `validate_invoice_terms` pre-check (hardcoded 6-decimal floor), so a token
// with missing decimals now fails closed on update too.
#[test]
fn c1_update_invoice_fails_closed_when_decimals_missing() {
    let t = setup();
    let id = submit(&t, &t.token.address).unwrap();

    // Simulate a legacy allowlisted token that never had decimals recorded.
    t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .persistent()
            .remove(&DataKey::TokenDecimals(t.token.address.clone()));
    });

    let due = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    assert_eq!(
        t.contract
            .try_update_invoice(&t.freelancer, &id, &INVOICE_AMOUNT, &due, &DISCOUNT_RATE),
        Err(Ok(ContractError::TokenDecimalsNotConfigured))
    );
}

// ── C3: bounded fund queue (#1095) ───────────────────────────────────────────

fn fill_queue(t: &T, id: u64) -> soroban_sdk::Vec<Address> {
    let mut lps = soroban_sdk::Vec::new(&t.env);
    for _ in 0..MAX_FUND_QUEUE_LEN {
        let lp = Address::generate(&t.env);
        t.contract.join_fund_queue(&lp, &id);
        lps.push_back(lp);
    }
    lps
}

#[test]
fn c3_queue_accepts_exactly_max_entries() {
    let t = setup();
    let id = submit(&t, &t.token.address).unwrap();
    fill_queue(&t, id);
    assert_eq!(queue_of(&t, id).len(), MAX_FUND_QUEUE_LEN);
}

#[test]
fn c3_equal_score_joiner_rejected_when_full() {
    let t = setup();
    let id = submit(&t, &t.token.address).unwrap();
    let lps = fill_queue(&t, id);

    let extra = Address::generate(&t.env);
    let res = t.contract.try_join_fund_queue(&extra, &id);
    assert_eq!(res, Err(Ok(ContractError::QueueFull)));

    let q = queue_of(&t, id);
    assert_eq!(q.len(), MAX_FUND_QUEUE_LEN);
    // First-come ordering of the original entries is untouched.
    for i in 0..q.len() {
        assert_eq!(q.get(i).unwrap().lp, lps.get(i).unwrap());
    }
}

#[test]
fn c3_lower_score_joiner_rejected_when_full() {
    let t = setup();
    let id = submit(&t, &t.token.address).unwrap();
    fill_queue(&t, id);

    let weak = Address::generate(&t.env);
    set_lp_score(&t, &weak, 10);
    assert_eq!(
        t.contract.try_join_fund_queue(&weak, &id),
        Err(Ok(ContractError::QueueFull))
    );
    assert_eq!(queue_of(&t, id).len(), MAX_FUND_QUEUE_LEN);
}

#[test]
fn c3_higher_score_joiner_displaces_lowest_and_len_stays_bounded() {
    let t = setup();
    let id = submit(&t, &t.token.address).unwrap();
    let lps = fill_queue(&t, id);
    let evicted = lps.get(lps.len() - 1).unwrap();

    let strong = Address::generate(&t.env);
    set_lp_score(&t, &strong, 90);
    t.contract.join_fund_queue(&strong, &id);
    // fund_queue_displaced + fund_requested are both emitted by this call.
    assert!(t.env.events().all().events().len() >= 2);

    let q = queue_of(&t, id);
    assert_eq!(q.len(), MAX_FUND_QUEUE_LEN);
    assert_eq!(q.get(0).unwrap().lp, strong);
    for i in 0..q.len() {
        assert_ne!(q.get(i).unwrap().lp, evicted);
    }
}

#[test]
fn c3_displaced_lp_may_not_double_join_but_can_rejoin_only_if_outscoring() {
    let t = setup();
    let id = submit(&t, &t.token.address).unwrap();
    let lps = fill_queue(&t, id);
    let evicted = lps.get(lps.len() - 1).unwrap();

    let strong = Address::generate(&t.env);
    set_lp_score(&t, &strong, 90);
    t.contract.join_fund_queue(&strong, &id);

    // The evicted LP has the default score again == lowest queued score.
    assert_eq!(
        t.contract.try_join_fund_queue(&evicted, &id),
        Err(Ok(ContractError::QueueFull))
    );
    // Existing members are still rejected as duplicates, not as full.
    assert_eq!(
        t.contract.try_join_fund_queue(&strong, &id),
        Err(Ok(ContractError::AlreadyInQueue))
    );
}

#[test]
fn c3_full_queue_still_resolves_to_highest_score() {
    let t = setup();
    let id = submit(&t, &t.token.address).unwrap();
    fill_queue(&t, id);
    let strong = Address::generate(&t.env);
    set_lp_score(&t, &strong, 95);
    t.contract.join_fund_queue(&strong, &id);

    let mut info = t.env.ledger().get();
    info.sequence_number += QUEUE_DELAY_LEDGERS;
    t.env.ledger().set(info);
    assert_eq!(t.contract.resolve_fund_queue(&id), strong);
}
