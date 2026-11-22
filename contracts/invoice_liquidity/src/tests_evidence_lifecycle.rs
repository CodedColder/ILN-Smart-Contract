#![cfg(test)]

//! Issue #1165 — invoice evidence commitment + verification lifecycle.
//!
//! Invoice submission separates a *cryptographic commitment* to off-chain
//! evidence from any claim of legitimacy. Only a versioned content digest is
//! stored on-chain — never the document or PII. These tests pin the auditable
//! lifecycle and the guarantees that make it safe:
//!
//! * a freelancer can attach a versioned commitment, stored `Unverified`;
//! * an authorized verifier records `Verified` / `Rejected` exactly once;
//! * the commitment is frozen once reviewed (no swap-out after a decision);
//! * the same digest may only ever be attached to one invoice, which prevents
//!   the same off-chain evidence from being financed twice; correcting an
//!   unreviewed commitment releases the superseded digest;
//! * the read view surfaces the invoice-level `disputed` condition so clients
//!   can render submitted/verified/rejected/disputed from one call.
//!
//! Authoritative invariants:
//! * INV-EVID1 — `attach_invoice_evidence` requires the invoice freelancer and
//!   a usable (non-zero, versioned) commitment.
//! * INV-EVID2 — a commitment is attachable only while the invoice is live and
//!   the current commitment is still `Unverified`.
//! * INV-EVID3 — the same digest maps to at most one invoice (duplicate-finance
//!   guard).
//! * INV-EVID4 — `verify_invoice_evidence` is admin-only and terminal: it
//!   records a decided status exactly once.
//! * INV-EVID5 — `get_invoice_evidence` reports the full lifecycle, including
//!   the invoice's live `disputed` condition.

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger, MockAuth, MockAuthInvoke},
    Address, BytesN, Env, IntoVal,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;
const EVIDENCE_VERSION: u32 = 1;

struct TestEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    admin: Address,
    freelancer: Address,
    payer: Address,
    token: Address,
}

/// A deterministic 32-byte digest filled with `byte`, so each test can name a
/// distinct commitment without a real off-chain document.
fn digest(env: &Env, byte: u8) -> BytesN<32> {
    BytesN::from_array(env, &[byte; 32])
}

fn setup() -> TestEnv {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();

    let xlm_admin = Address::generate(&env);
    let xlm = env.register_stellar_asset_contract_v2(xlm_admin).address();

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    contract.initialize(&admin, &token, &token, &xlm);

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    env.ledger().set(ledger);

    TestEnv {
        env,
        contract,
        admin,
        freelancer,
        payer,
        token,
    }
}

/// Submit a fresh invoice owned by `freelancer`/`payer` and return its id.
fn submit(t: &TestEnv) -> u64 {
    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &(t.env.ledger().timestamp() + DUE_DATE_OFFSET),
        &DISCOUNT_RATE,
        &t.token,
        &ReferralCode::None,
    )
}

fn attach(t: &TestEnv, invoice_id: u64, byte: u8) {
    t.contract.attach_invoice_evidence(
        &t.freelancer,
        &invoice_id,
        &EVIDENCE_VERSION,
        &digest(&t.env, byte),
    );
}

// ================================================================
// Attachment (INV-EVID1 / INV-EVID2)
// ================================================================

#[test]
fn attach_records_unverified_commitment() {
    let t = setup();
    let invoice_id = submit(&t);
    let commit = digest(&t.env, 7);

    t.contract.attach_invoice_evidence(
        &t.freelancer,
        &invoice_id,
        &EVIDENCE_VERSION,
        &commit,
    );

    let view = t.contract.get_invoice_evidence(&invoice_id).unwrap();
    assert_eq!(view.invoice_id, invoice_id);
    assert_eq!(view.status, EvidenceStatus::Unverified);
    assert_eq!(view.version, EVIDENCE_VERSION);
    assert_eq!(view.digest, commit);
    assert_eq!(view.submitted_by, t.freelancer);
    assert_eq!(view.submitted_at, t.env.ledger().timestamp());
    assert_eq!(view.verified_by, None);
    assert_eq!(view.verified_at, None);
    assert!(!view.disputed);
}

#[test]
fn zero_digest_commitment_rejected() {
    let t = setup();
    let invoice_id = submit(&t);

    let res = t.contract.try_attach_invoice_evidence(
        &t.freelancer,
        &invoice_id,
        &EVIDENCE_VERSION,
        &digest(&t.env, 0),
    );
    assert_eq!(res, Err(Ok(ContractError::InvalidEvidenceCommitment)));
    assert!(t.contract.get_invoice_evidence(&invoice_id).is_none());
}

#[test]
fn zero_version_commitment_rejected() {
    let t = setup();
    let invoice_id = submit(&t);

    let res = t.contract.try_attach_invoice_evidence(
        &t.freelancer,
        &invoice_id,
        &0,
        &digest(&t.env, 3),
    );
    assert_eq!(res, Err(Ok(ContractError::InvalidEvidenceCommitment)));
    assert!(t.contract.get_invoice_evidence(&invoice_id).is_none());
}

#[test]
fn non_freelancer_cannot_attach() {
    let t = setup();
    let invoice_id = submit(&t);

    // The payer (or anyone else) is not the invoice's freelancer.
    let res = t.contract.try_attach_invoice_evidence(
        &t.payer,
        &invoice_id,
        &EVIDENCE_VERSION,
        &digest(&t.env, 5),
    );
    assert_eq!(res, Err(Ok(ContractError::Unauthorized)));
    assert!(t.contract.get_invoice_evidence(&invoice_id).is_none());
}

#[test]
fn attach_unknown_invoice_rejected() {
    let t = setup();

    let res = t.contract.try_attach_invoice_evidence(
        &t.freelancer,
        &9_999,
        &EVIDENCE_VERSION,
        &digest(&t.env, 6),
    );
    assert_eq!(res, Err(Ok(ContractError::InvoiceNotFound)));
}

#[test]
fn reattach_replaces_commitment_while_unverified() {
    let t = setup();
    let invoice_id = submit(&t);
    let corrected = digest(&t.env, 22);

    attach(&t, invoice_id, 11);
    // Correcting the commitment before any review is allowed.
    t.contract.attach_invoice_evidence(
        &t.freelancer,
        &invoice_id,
        &EVIDENCE_VERSION,
        &corrected,
    );

    let view = t.contract.get_invoice_evidence(&invoice_id).unwrap();
    assert_eq!(view.digest, corrected);
    assert_eq!(view.status, EvidenceStatus::Unverified);
}

#[test]
fn attach_blocked_once_invoice_cancelled() {
    let t = setup();
    let invoice_id = submit(&t);
    t.contract.cancel_invoice(&invoice_id);

    let res = t.contract.try_attach_invoice_evidence(
        &t.freelancer,
        &invoice_id,
        &EVIDENCE_VERSION,
        &digest(&t.env, 44),
    );
    assert_eq!(res, Err(Ok(ContractError::AlreadyCancelled)));
}

// ================================================================
// Duplicate-financing guard (INV-EVID3)
// ================================================================

#[test]
fn same_digest_on_different_invoice_rejected() {
    let t = setup();
    let inv1 = submit(&t);
    let inv2 = submit(&t);
    let commit = digest(&t.env, 9);

    attach(&t, inv1, 9);

    let res = t.contract.try_attach_invoice_evidence(
        &t.freelancer,
        &inv2,
        &EVIDENCE_VERSION,
        &commit,
    );
    assert_eq!(res, Err(Ok(ContractError::DuplicateEvidenceCommitment)));
    assert!(t.contract.get_invoice_evidence(&inv2).is_none());
}

#[test]
fn same_digest_on_same_invoice_allowed() {
    let t = setup();
    let invoice_id = submit(&t);
    let commit = digest(&t.env, 12);

    t.contract.attach_invoice_evidence(
        &t.freelancer,
        &invoice_id,
        &EVIDENCE_VERSION,
        &commit,
    );
    // Re-attaching the identical digest to its own invoice is a no-op, not a
    // duplicate.
    t.contract.attach_invoice_evidence(
        &t.freelancer,
        &invoice_id,
        &EVIDENCE_VERSION,
        &commit,
    );

    assert_eq!(
        t.contract.get_invoice_evidence(&invoice_id).unwrap().digest,
        commit
    );
}

#[test]
fn correcting_commitment_releases_superseded_digest() {
    let t = setup();
    let inv1 = submit(&t);
    let inv2 = submit(&t);
    let stale = digest(&t.env, 30);
    let corrected = digest(&t.env, 40);

    // Wrong digest attached first, then corrected before review.
    t.contract.attach_invoice_evidence(
        &t.freelancer,
        &inv1,
        &EVIDENCE_VERSION,
        &stale,
    );
    t.contract.attach_invoice_evidence(
        &t.freelancer,
        &inv1,
        &EVIDENCE_VERSION,
        &corrected,
    );

    // The superseded digest is no longer locked to inv1, so it is free to be
    // committed to its rightful invoice.
    t.contract.attach_invoice_evidence(
        &t.freelancer,
        &inv2,
        &EVIDENCE_VERSION,
        &stale,
    );
    assert_eq!(
        t.contract.get_invoice_evidence(&inv2).unwrap().digest,
        stale
    );
    assert_eq!(
        t.contract.get_invoice_evidence(&inv1).unwrap().digest,
        corrected
    );
}

// ================================================================
// Verification (INV-EVID4)
// ================================================================

#[test]
fn verify_marks_evidence_verified() {
    let t = setup();
    let invoice_id = submit(&t);
    attach(&t, invoice_id, 50);

    t.contract.verify_invoice_evidence(&invoice_id, &true);

    let view = t.contract.get_invoice_evidence(&invoice_id).unwrap();
    assert_eq!(view.status, EvidenceStatus::Verified);
    assert_eq!(view.verified_by, Some(t.admin.clone()));
    assert_eq!(view.verified_at, Some(t.env.ledger().timestamp()));
}

#[test]
fn reject_marks_evidence_rejected() {
    let t = setup();
    let invoice_id = submit(&t);
    attach(&t, invoice_id, 51);

    t.contract.verify_invoice_evidence(&invoice_id, &false);

    let view = t.contract.get_invoice_evidence(&invoice_id).unwrap();
    assert_eq!(view.status, EvidenceStatus::Rejected);
    assert_eq!(view.verified_by, Some(t.admin.clone()));
    assert!(view.verified_at.is_some());
}

#[test]
fn review_is_terminal_and_cannot_be_repeated() {
    let t = setup();
    let invoice_id = submit(&t);
    attach(&t, invoice_id, 52);

    t.contract.verify_invoice_evidence(&invoice_id, &true);

    // A second decision — reversing a prior verification — is rejected.
    let res = t.contract.try_verify_invoice_evidence(&invoice_id, &false);
    assert_eq!(res, Err(Ok(ContractError::EvidenceAlreadyReviewed)));

    // The original decision stands.
    assert_eq!(
        t.contract.get_invoice_evidence(&invoice_id).unwrap().status,
        EvidenceStatus::Verified
    );
}

#[test]
fn verified_commitment_cannot_be_replaced() {
    let t = setup();
    let invoice_id = submit(&t);
    attach(&t, invoice_id, 53);
    t.contract.verify_invoice_evidence(&invoice_id, &true);

    let res = t.contract.try_attach_invoice_evidence(
        &t.freelancer,
        &invoice_id,
        &EVIDENCE_VERSION,
        &digest(&t.env, 54),
    );
    assert_eq!(res, Err(Ok(ContractError::EvidenceAlreadyReviewed)));
}

#[test]
fn rejected_commitment_cannot_be_replaced() {
    let t = setup();
    let invoice_id = submit(&t);
    attach(&t, invoice_id, 55);
    t.contract.verify_invoice_evidence(&invoice_id, &false);

    let res = t.contract.try_attach_invoice_evidence(
        &t.freelancer,
        &invoice_id,
        &EVIDENCE_VERSION,
        &digest(&t.env, 56),
    );
    assert_eq!(res, Err(Ok(ContractError::EvidenceAlreadyReviewed)));
}

#[test]
fn verify_without_commitment_rejected() {
    let t = setup();
    let invoice_id = submit(&t);

    let res = t.contract.try_verify_invoice_evidence(&invoice_id, &true);
    assert_eq!(res, Err(Ok(ContractError::EvidenceNotFound)));
}

#[test]
fn verify_unknown_invoice_rejected() {
    let t = setup();

    let res = t.contract.try_verify_invoice_evidence(&9_999, &true);
    assert_eq!(res, Err(Ok(ContractError::InvoiceNotFound)));
}

#[test]
fn verify_requires_admin_authorization() {
    let t = setup();
    let invoice_id = submit(&t);
    attach(&t, invoice_id, 57);

    // Replace the blanket mock with a single imposter authorization: the real
    // admin does not authorize, so `require_admin` must reject the call.
    let imposter = Address::generate(&t.env);
    t.env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &t.contract.address,
            fn_name: "verify_invoice_evidence",
            args: (invoice_id, true).into_val(&t.env),
            sub_invokes: &[],
        },
    }]);

    let res = t.contract.try_verify_invoice_evidence(&invoice_id, &true);
    assert!(res.is_err(), "verify must require the admin's authorization");

    // The commitment is untouched by the rejected call.
    assert_eq!(
        t.contract.get_invoice_evidence(&invoice_id).unwrap().status,
        EvidenceStatus::Unverified
    );
}

// ================================================================
// Lifecycle view (INV-EVID5)
// ================================================================

#[test]
fn unknown_invoice_has_no_evidence_view() {
    let t = setup();
    let invoice_id = submit(&t);
    assert!(t.contract.get_invoice_evidence(&invoice_id).is_none());
}

#[test]
fn view_reports_live_disputed_condition() {
    let t = setup();
    let invoice_id = submit(&t);
    attach(&t, invoice_id, 60);
    assert!(!t.contract.get_invoice_evidence(&invoice_id).unwrap().disputed);

    t.contract.dispute_invoice(&invoice_id, &digest(&t.env, 61));

    let view = t.contract.get_invoice_evidence(&invoice_id).unwrap();
    assert!(view.disputed, "dispute must surface on the evidence view");
    // The commitment itself is unaffected by the dispute condition.
    assert_eq!(view.status, EvidenceStatus::Unverified);
}
