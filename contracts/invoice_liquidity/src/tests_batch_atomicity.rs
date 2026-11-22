//! Audit tests: `submit_invoices_batch` partial-failure behaviour.
//!
//! Verified model (see the doc comment on `submit_invoices_batch`):
//!  - The batch is all-or-nothing. A failure on any entry reverts every write
//!    from earlier entries (invoices, submitter index, counters, reputation,
//!    referral counts, ID allocation).
//!  - Each entry is validated with the same rules as `submit_invoice`.
//!
//! Scenarios: failure at the last entry (the worst case for partial state),
//! each validation error mid-batch, unauthorized co-submitter, paused
//! contract, empty batch, repeated identical batch, and parity with
//! `submit_invoice` for the checks the batch path used to skip.

#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger, MockAuth, MockAuthInvoke},
    token::StellarAssetClient,
    Address, BytesN, Env, IntoVal, Vec,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

struct T {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: Address,
    freelancer: Address,
    freelancer_b: Address,
    payer: Address,
}

fn setup() -> T {
    let env = Env::default();
    env.mock_all_auths();

    let usdc_admin = Address::generate(&env);
    let usdc = env.register_stellar_asset_contract_v2(usdc_admin.clone()).address();
    StellarAssetClient::new(&env, &usdc).mint(&Address::generate(&env), &INVOICE_AMOUNT);

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);

    let xlm = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let eurc = Address::generate(&env);
    contract.initialize(&usdc_admin, &usdc, &eurc, &xlm);

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    env.ledger().set(ledger);

    T {
        freelancer: Address::generate(&env),
        freelancer_b: Address::generate(&env),
        payer: Address::generate(&env),
        env,
        contract,
        token: usdc,
    }
}

fn valid(t: &T, freelancer: &Address) -> InvoiceParams {
    InvoiceParams {
        freelancer: freelancer.clone(),
        payer: t.payer.clone(),
        amount: INVOICE_AMOUNT,
        due_date: t.env.ledger().timestamp() + DUE_DATE_OFFSET,
        discount_rate: DISCOUNT_RATE,
        token: t.token.clone(),
        referral_code: ReferralCode::None,
    }
}

/// Assert that nothing observable changed: no invoice, no index entry, no
/// counters, no reputation, and the next ID is not consumed.
fn assert_no_state(t: &T) {
    assert_eq!(t.contract.get_invoice_count(), 0, "ID counter must not advance");
    assert_eq!(t.contract.get_contract_stats().total_invoices, 0);
    assert_eq!(
        t.contract
            .list_invoices_by_submitter(&t.freelancer, &0, &50)
            .len(),
        0
    );
    assert_eq!(t.contract.get_reputation(&t.freelancer).invoices_submitted, 0);
    assert_eq!(t.contract.get_reputation(&t.freelancer_b).invoices_submitted, 0);
}

// ── Failure at the last entry leaves nothing behind ──────────────────────────

#[test]
fn test_batch_invalid_last_entry_reverts_all_earlier_entries() {
    let t = setup();
    let code = BytesN::from_array(&t.env, &[7u8; 32]);

    let mut first = valid(&t, &t.freelancer);
    first.referral_code = ReferralCode::Present(code.clone());
    let mut bad = valid(&t, &t.freelancer);
    bad.amount = 0;

    let batch: Vec<InvoiceParams> = soroban_sdk::vec![
        &t.env,
        first,
        valid(&t, &t.freelancer_b),
        valid(&t, &t.freelancer),
        bad
    ];

    let res = t.contract.try_submit_invoices_batch(&batch);
    assert_eq!(res, Err(Ok(ContractError::InvalidAmount)));

    assert_no_state(&t);
    assert_eq!(
        t.contract.get_referral_stats(&code),
        0,
        "referral count from reverted entries must not persist"
    );
}

#[test]
fn test_batch_failure_does_not_consume_invoice_ids() {
    let t = setup();

    let mut bad = valid(&t, &t.freelancer);
    bad.due_date = t.env.ledger().timestamp(); // not in the future
    let failing = soroban_sdk::vec![&t.env, valid(&t, &t.freelancer), bad];
    assert_eq!(
        t.contract.try_submit_invoices_batch(&failing),
        Err(Ok(ContractError::InvalidDueDate))
    );

    // The next successful batch starts from the very first ID: no gaps.
    let ok = soroban_sdk::vec![&t.env, valid(&t, &t.freelancer), valid(&t, &t.freelancer)];
    let ids = t.contract.submit_invoices_batch(&ok);
    assert_eq!(ids.len(), 2);
    assert_eq!(ids.get(0).unwrap(), 1);
    assert_eq!(ids.get(1).unwrap(), 2);
    assert_eq!(t.contract.get_invoice_count(), 2);
}

// ── Each validation error mid-batch is reported and fully reverts ────────────

#[test]
fn test_batch_mid_entry_errors_are_exact_and_atomic() {
    let t = setup();

    // (mutation, expected error)
    let mut unapproved = valid(&t, &t.freelancer);
    unapproved.token = Address::generate(&t.env);
    let mut bad_rate = valid(&t, &t.freelancer);
    bad_rate.discount_rate = 0;
    let mut rate_over_cap = valid(&t, &t.freelancer);
    rate_over_cap.discount_rate = crate::constants::MAX_DISCOUNT_RATE + 1;
    let mut too_small = valid(&t, &t.freelancer);
    too_small.amount = 1;
    let mut too_soon = valid(&t, &t.freelancer);
    too_soon.due_date = t.env.ledger().timestamp() + 1;
    let mut too_far = valid(&t, &t.freelancer);
    too_far.due_date = t.env.ledger().timestamp() + MAX_INVOICE_DURATION + 1;

    let cases: [(InvoiceParams, ContractError); 6] = [
        (unapproved, ContractError::Unauthorized),
        (bad_rate, ContractError::InvalidDiscountRate),
        (rate_over_cap, ContractError::InvalidDiscountRate),
        (too_small, ContractError::InvalidAmount),
        (too_soon, ContractError::DueDateTooSoon),
        (too_far, ContractError::DueDateTooFar),
    ];

    for (bad, expected) in cases {
        let batch = soroban_sdk::vec![&t.env, valid(&t, &t.freelancer), bad];
        assert_eq!(
            t.contract.try_submit_invoices_batch(&batch),
            Err(Ok(expected))
        );
        assert_no_state(&t);
    }
}

// ── Parity with submit_invoice (checks the batch path used to skip) ──────────

#[test]
fn test_batch_rejects_self_invoice_like_single_submit() {
    let t = setup();

    let mut selfie = valid(&t, &t.freelancer);
    selfie.payer = t.freelancer.clone();

    // Single-submit behaviour is the reference.
    let single = t.contract.try_submit_invoice(
        &selfie.freelancer,
        &selfie.payer,
        &selfie.amount,
        &selfie.due_date,
        &selfie.discount_rate,
        &selfie.token,
        &ReferralCode::None,
    );
    assert_eq!(single, Err(Ok(ContractError::SelfInvoice)));

    let batch = soroban_sdk::vec![&t.env, valid(&t, &t.freelancer), selfie];
    assert_eq!(
        t.contract.try_submit_invoices_batch(&batch),
        Err(Ok(ContractError::SelfInvoice))
    );
    assert_no_state(&t);
}

#[test]
fn test_batch_enforces_max_invoice_amount_cap() {
    let t = setup();
    t.contract.set_max_invoice_amount(&INVOICE_AMOUNT);

    // At the cap: accepted.
    let at_cap = soroban_sdk::vec![&t.env, valid(&t, &t.freelancer)];
    assert_eq!(t.contract.submit_invoices_batch(&at_cap).len(), 1);

    // One above the cap mid-batch: whole batch rejected, count unchanged.
    let mut over = valid(&t, &t.freelancer);
    over.amount = INVOICE_AMOUNT + 1;
    let batch = soroban_sdk::vec![&t.env, valid(&t, &t.freelancer), over];
    assert_eq!(
        t.contract.try_submit_invoices_batch(&batch),
        Err(Ok(ContractError::MaxInvoiceAmountExceeded))
    );
    assert_eq!(t.contract.get_invoice_count(), 1);
    assert_eq!(t.contract.get_reputation(&t.freelancer).invoices_submitted, 1);
}

// ── Authorization ────────────────────────────────────────────────────────────

#[test]
fn test_batch_requires_auth_from_every_distinct_freelancer() {
    let t = setup();

    let batch = soroban_sdk::vec![
        &t.env,
        valid(&t, &t.freelancer),
        valid(&t, &t.freelancer_b)
    ];

    // Only freelancer A authorizes the call; B's entry must fail the batch.
    t.env.mock_auths(&[MockAuth {
        address: &t.freelancer,
        invoke: &MockAuthInvoke {
            contract: &t.contract.address,
            fn_name: "submit_invoices_batch",
            args: (batch.clone(),).into_val(&t.env),
            sub_invokes: &[],
        },
    }]);

    assert!(t.contract.try_submit_invoices_batch(&batch).is_err());
    assert_no_state(&t);
}

// ── Pause / size / empty ─────────────────────────────────────────────────────

#[test]
fn test_batch_rejected_while_paused_without_side_effects() {
    let t = setup();
    t.contract.pause();

    let batch = soroban_sdk::vec![&t.env, valid(&t, &t.freelancer)];
    assert_eq!(
        t.contract.try_submit_invoices_batch(&batch),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_no_state(&t);
}

#[test]
fn test_batch_size_boundary_ten_ok_eleven_rejected() {
    let t = setup();

    let mut ten: Vec<InvoiceParams> = Vec::new(&t.env);
    for _ in 0..10 {
        ten.push_back(valid(&t, &t.freelancer));
    }
    let mut eleven = ten.clone();
    eleven.push_back(valid(&t, &t.freelancer));

    assert_eq!(
        t.contract.try_submit_invoices_batch(&eleven),
        Err(Ok(ContractError::BatchTooLarge))
    );
    assert_no_state(&t);

    assert_eq!(t.contract.submit_invoices_batch(&ten).len(), 10);
    assert_eq!(t.contract.get_invoice_count(), 10);
}

#[test]
fn test_empty_batch_is_a_noop() {
    let t = setup();
    let ids = t.contract.submit_invoices_batch(&Vec::new(&t.env));
    assert_eq!(ids.len(), 0);
    assert_no_state(&t);
}

// ── Repeated (replayed) batch ────────────────────────────────────────────────

#[test]
fn test_repeated_identical_batch_creates_distinct_invoices() {
    // There is no idempotency key: resubmitting the same batch is a new,
    // independent set of invoices. A caller that retries after an *unknown*
    // outcome must check `list_invoices_by_submitter` first.
    let t = setup();
    let batch = soroban_sdk::vec![&t.env, valid(&t, &t.freelancer), valid(&t, &t.freelancer)];

    let first = t.contract.submit_invoices_batch(&batch);
    let second = t.contract.submit_invoices_batch(&batch);

    assert_eq!(first.len(), 2);
    assert_eq!(second.len(), 2);
    assert_eq!(second.get(0).unwrap(), first.get(1).unwrap() + 1);
    assert_eq!(t.contract.get_invoice_count(), 4);
    assert_eq!(t.contract.get_reputation(&t.freelancer).invoices_submitted, 4);
}

// ── Success returns one ID per entry, in input order ─────────────────────────

#[test]
fn test_successful_batch_returns_one_sequential_id_per_entry_in_order() {
    let t = setup();
    let batch = soroban_sdk::vec![
        &t.env,
        valid(&t, &t.freelancer),
        valid(&t, &t.freelancer_b),
        valid(&t, &t.freelancer)
    ];

    let ids = t.contract.submit_invoices_batch(&batch);
    assert_eq!(ids.len(), 3);
    for i in 0..3u32 {
        let id = ids.get(i).unwrap();
        assert_eq!(id, u64::from(i) + 1);
        let inv = t.contract.get_invoice(&id);
        assert_eq!(inv.freelancer, batch.get(i).unwrap().freelancer);
        assert_eq!(inv.status, InvoiceStatus::Pending);
    }
    assert_eq!(t.contract.get_contract_stats().total_invoices, 3);
}
