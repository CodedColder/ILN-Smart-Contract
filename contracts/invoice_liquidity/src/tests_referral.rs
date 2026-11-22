#![cfg(test)]

use super::*;
use crate::test::setup;
use soroban_sdk::{
    testutils::{Address as _, MockAuth, MockAuthInvoke},
    Address, BytesN, IntoVal,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

#[test]
fn test_referral_count_increments_on_submission() {
    let t = setup();
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let code = BytesN::from_array(&t.env, &[2u8; 32]);

    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::Present(code.clone()),
    );

    let stats = t.contract.get_referral_stats(&code);
    assert_eq!(stats, 1);
}

#[test]
fn test_referral_count_returns_zero_for_unknown_code() {
    let t = setup();
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let known_code = BytesN::from_array(&t.env, &[1u8; 32]);
    let unknown_code = BytesN::from_array(&t.env, &[9u8; 32]);

    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::Present(known_code),
    );

    let stats = t.contract.get_referral_stats(&unknown_code);
    assert_eq!(stats, 0);
}

#[test]
fn test_referral_count_zero_when_no_submissions() {
    let t = setup();
    let code = BytesN::from_array(&t.env, &[5u8; 32]);

    let stats = t.contract.get_referral_stats(&code);
    assert_eq!(stats, 0);
}

#[test]
fn test_submit_invoice_without_referral_does_not_increment_stats() {
    let t = setup();
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let code = BytesN::from_array(&t.env, &[1u8; 32]);

    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    );

    let stats = t.contract.get_referral_stats(&code);
    assert_eq!(stats, 0);
}

#[test]
fn test_batch_submission_counts_referrals_correctly() {
    let t = setup();
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let code = BytesN::from_array(&t.env, &[7u8; 32]);

    let mut batch = soroban_sdk::Vec::new(&t.env);
    for _ in 0..3 {
        batch.push_back(InvoiceParams {
            freelancer: t.freelancer.clone(),
            payer: t.payer.clone(),
            amount: INVOICE_AMOUNT,
            due_date,
            discount_rate: DISCOUNT_RATE,
            token: t.token.address.clone(),
            referral_code: ReferralCode::Present(code.clone()),
        });
    }
    batch.push_back(InvoiceParams {
        freelancer: t.freelancer.clone(),
        payer: t.payer.clone(),
        amount: INVOICE_AMOUNT,
        due_date,
        discount_rate: DISCOUNT_RATE,
        token: t.token.address.clone(),
        referral_code: ReferralCode::None,
    });

    let result = t.contract.try_submit_invoices_batch(&batch);
    assert!(result.is_ok());

    let stats = t.contract.get_referral_stats(&code);
    assert_eq!(stats, 3);
}

// ════════════════════════════════════════════════════════════════════════════
// Audit: referral code validation and replay (see docs/referral-audit.md)
//
// Model: `ReferralCount[code]` is written ONLY at invoice creation, once per
// successfully created invoice carrying `ReferralCode::Present(code)`. No entry
// point edits a count or an invoice's stored referral_code. The contract holds
// no reward logic — counts are informational.
// ════════════════════════════════════════════════════════════════════════════

fn submit_with(t: &crate::test::TestEnv, code: &BytesN<32>) -> u64 {
    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &(t.env.ledger().timestamp() + DUE_DATE_OFFSET),
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::Present(code.clone()),
    )
}

// ── Replay / duplicates ──────────────────────────────────────────────────────

#[test]
fn test_replayed_identical_submission_counts_each_new_invoice() {
    // No dedupe: resubmitting the same code (even same freelancer/params) is a
    // new invoice and a new count. Counts therefore measure submissions, not
    // distinct referees.
    let t = setup();
    let code = BytesN::from_array(&t.env, &[3u8; 32]);

    let a = submit_with(&t, &code);
    let b = submit_with(&t, &code);

    assert_ne!(a, b);
    assert_eq!(t.contract.get_referral_stats(&code), 2);
}

#[test]
fn test_different_codes_are_counted_independently() {
    let t = setup();
    let c1 = BytesN::from_array(&t.env, &[1u8; 32]);
    let c2 = BytesN::from_array(&t.env, &[2u8; 32]);

    submit_with(&t, &c1);
    submit_with(&t, &c1);
    submit_with(&t, &c2);

    assert_eq!(t.contract.get_referral_stats(&c1), 2);
    assert_eq!(t.contract.get_referral_stats(&c2), 1);
}

#[test]
fn test_all_zero_code_is_an_ordinary_code() {
    // The contract does not treat the all-zero value as a sentinel; absence is
    // expressed only by `ReferralCode::None`. Documented residual: off-chain
    // consumers must not treat [0;32] as "no referral".
    let t = setup();
    let zero = BytesN::from_array(&t.env, &[0u8; 32]);

    submit_with(&t, &zero);
    assert_eq!(t.contract.get_referral_stats(&zero), 1);
}

// ── Failed submissions never change counts ───────────────────────────────────

#[test]
fn test_rejected_submissions_do_not_increment_referral_count() {
    let t = setup();
    let code = BytesN::from_array(&t.env, &[4u8; 32]);
    let due = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let present = ReferralCode::Present(code.clone());

    // Invalid amount.
    assert!(t
        .contract
        .try_submit_invoice(&t.freelancer, &t.payer, &0, &due, &DISCOUNT_RATE, &t.token.address, &present)
        .is_err());
    // Invalid discount rate.
    assert!(t
        .contract
        .try_submit_invoice(&t.freelancer, &t.payer, &INVOICE_AMOUNT, &due, &0, &t.token.address, &present)
        .is_err());
    // Past due date.
    assert!(t
        .contract
        .try_submit_invoice(
            &t.freelancer,
            &t.payer,
            &INVOICE_AMOUNT,
            &t.env.ledger().timestamp(),
            &DISCOUNT_RATE,
            &t.token.address,
            &present
        )
        .is_err());
    // Self invoice.
    assert!(t
        .contract
        .try_submit_invoice(&t.freelancer, &t.freelancer, &INVOICE_AMOUNT, &due, &DISCOUNT_RATE, &t.token.address, &present)
        .is_err());
    // Unapproved token.
    let rogue_token = Address::generate(&t.env);
    assert!(t
        .contract
        .try_submit_invoice(&t.freelancer, &t.payer, &INVOICE_AMOUNT, &due, &DISCOUNT_RATE, &rogue_token, &present)
        .is_err());

    assert_eq!(t.contract.get_referral_stats(&code), 0);
    assert_eq!(t.contract.get_invoice_count(), 0);
}

#[test]
fn test_paused_contract_does_not_increment_referral_count() {
    let t = setup();
    let code = BytesN::from_array(&t.env, &[5u8; 32]);
    t.contract.pause();

    let res = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &(t.env.ledger().timestamp() + DUE_DATE_OFFSET),
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::Present(code.clone()),
    );
    assert_eq!(res, Err(Ok(ContractError::ContractPaused)));
    assert_eq!(t.contract.get_referral_stats(&code), 0);
}

#[test]
fn test_unauthorized_submission_does_not_increment_referral_count() {
    let t = setup();
    let code = BytesN::from_array(&t.env, &[6u8; 32]);
    let due = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let attacker = Address::generate(&t.env);

    // The attacker authorizes, but the invoice names `freelancer` as submitter.
    t.env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &t.contract.address,
            fn_name: "submit_invoice",
            args: (
                t.freelancer.clone(),
                t.payer.clone(),
                INVOICE_AMOUNT,
                due,
                DISCOUNT_RATE,
                t.token.address.clone(),
                ReferralCode::Present(code.clone()),
            )
                .into_val(&t.env),
            sub_invokes: &[],
        },
    }]);

    let res = t.contract.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::Present(code.clone()),
    );
    assert!(res.is_err());
    assert_eq!(t.contract.get_referral_stats(&code), 0);
}

// ── Batch path parity and atomicity ──────────────────────────────────────────

#[test]
fn test_batch_failure_rolls_back_referral_counts_of_earlier_entries() {
    let t = setup();
    let code = BytesN::from_array(&t.env, &[7u8; 32]);
    let due = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let mk = |amount: i128| InvoiceParams {
        freelancer: t.freelancer.clone(),
        payer: t.payer.clone(),
        amount,
        due_date: due,
        discount_rate: DISCOUNT_RATE,
        token: t.token.address.clone(),
        referral_code: ReferralCode::Present(code.clone()),
    };

    let batch = soroban_sdk::vec![&t.env, mk(INVOICE_AMOUNT), mk(INVOICE_AMOUNT), mk(0)];
    assert!(t.contract.try_submit_invoices_batch(&batch).is_err());
    assert_eq!(t.contract.get_referral_stats(&code), 0);
}

#[test]
fn test_batch_and_single_submission_accumulate_on_the_same_counter() {
    let t = setup();
    let code = BytesN::from_array(&t.env, &[8u8; 32]);
    let due = t.env.ledger().timestamp() + DUE_DATE_OFFSET;

    submit_with(&t, &code);
    let batch = soroban_sdk::vec![
        &t.env,
        InvoiceParams {
            freelancer: t.freelancer.clone(),
            payer: t.payer.clone(),
            amount: INVOICE_AMOUNT,
            due_date: due,
            discount_rate: DISCOUNT_RATE,
            token: t.token.address.clone(),
            referral_code: ReferralCode::Present(code.clone()),
        }
    ];
    t.contract.submit_invoices_batch(&batch);

    assert_eq!(t.contract.get_referral_stats(&code), 2);
}

// ── Later lifecycle calls cannot alter the code or the count ─────────────────

#[test]
fn test_update_convert_cancel_do_not_change_referral_code_or_count() {
    let t = setup();
    let code = BytesN::from_array(&t.env, &[9u8; 32]);
    let id = submit_with(&t, &code);
    let expected = ReferralCode::Present(code.clone());

    // update_invoice
    t.contract.update_invoice(
        &t.freelancer,
        &id,
        &(INVOICE_AMOUNT * 2),
        &(t.env.ledger().timestamp() + DUE_DATE_OFFSET),
        &(DISCOUNT_RATE + 100),
    );
    assert_eq!(t.contract.get_invoice(&id).referral_code, expected);
    assert_eq!(t.contract.get_referral_stats(&code), 1);

    // cancel_invoice (a cancelled invoice keeps its count: counts measure
    // submissions, not outcomes).
    t.contract.cancel_invoice(&id);
    assert_eq!(t.contract.get_invoice(&id).referral_code, expected);
    assert_eq!(t.contract.get_referral_stats(&code), 1);
}

#[test]
fn test_funding_and_payment_do_not_change_referral_code_or_count() {
    let t = setup();
    let code = BytesN::from_array(&t.env, &[10u8; 32]);
    let id = submit_with(&t, &code);
    let expected = ReferralCode::Present(code.clone());

    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);
    assert_eq!(t.contract.get_invoice(&id).referral_code, expected);
    assert_eq!(t.contract.get_referral_stats(&code), 1);

    t.contract.mark_paid(&id, &INVOICE_AMOUNT);
    assert_eq!(t.contract.get_invoice(&id).referral_code, expected);
    assert_eq!(t.contract.get_referral_stats(&code), 1);
}

#[test]
fn test_invoice_without_referral_stores_none() {
    let t = setup();
    let id = t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &(t.env.ledger().timestamp() + DUE_DATE_OFFSET),
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    );
    assert_eq!(t.contract.get_invoice(&id).referral_code, ReferralCode::None);
}

// ── Anyone can inflate a count (documented residual risk) ────────────────────

#[test]
fn test_any_submitter_can_inflate_any_code_count() {
    // Referral codes are unauthenticated: the contract does not bind a code to
    // an owner or require the referrer's consent. Any submitter can bump any
    // code. Counts must therefore never gate rewards on their own.
    let t = setup();
    let victim_code = BytesN::from_array(&t.env, &[11u8; 32]);

    for _ in 0..3 {
        submit_with(&t, &victim_code);
    }
    assert_eq!(t.contract.get_referral_stats(&victim_code), 3);
}
