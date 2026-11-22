#![cfg(test)]

//! Issue #1167 — replace remaining invoice-contract unwrap assumptions.
//!
//! Several state reads used to *assume* the state was present: the legacy
//! `load_invoice` helper panics via `expect("invoice not found")`, and a few
//! paths read `DataKey::Admin` with `.unwrap()`. A missing key therefore
//! trapped the host instead of surfacing a typed error. These tests pin the
//! observable contract: predictable missing-state reads resolve to a
//! `ContractError`, never to a panic.
//!
//! Invariants:
//! * INV-NOUNWRAP1 — reading an absent invoice returns
//!   `ContractError::InvoiceNotFound` (not a host trap).
//! * INV-NOUNWRAP2 — paginated views tolerate a dangling index entry (the
//!   index names the id but the payload is gone) and omit it.
//! * INV-NOUNWRAP3 — skipping a dangling entry does not mutate the index.

use super::*;
use crate::invoice::StorageKey;
use crate::test::{setup, TestEnv};
use soroban_sdk::BytesN;

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

fn submit_one(t: &TestEnv) -> u64 {
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

/// INV-NOUNWRAP1: representative entry points that sat behind an
/// `invoice_exists` probe plus the panicking `load_invoice` all report
/// `InvoiceNotFound` for an id that was never created — the probe and the
/// load now agree on a single typed error.
#[test]
fn missing_invoice_reads_return_invoice_not_found() {
    let t = setup();
    let missing = 9_999u64;
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;

    assert_eq!(
        t.contract.try_get_invoice(&missing),
        Err(Ok(ContractError::InvoiceNotFound))
    );
    assert_eq!(
        t.contract.try_update_invoice(
            &t.freelancer,
            &missing,
            &INVOICE_AMOUNT,
            &due_date,
            &DISCOUNT_RATE,
        ),
        Err(Ok(ContractError::InvoiceNotFound))
    );
    assert_eq!(
        t.contract
            .try_convert_invoice_token(&t.freelancer, &missing, &t.token.address),
        Err(Ok(ContractError::InvoiceNotFound))
    );
    assert_eq!(
        t.contract.try_transfer_invoice(&missing, &t.payer),
        Err(Ok(ContractError::InvoiceNotFound))
    );
    assert_eq!(
        t.contract.try_cancel_invoice(&missing),
        Err(Ok(ContractError::InvoiceNotFound))
    );
    assert_eq!(
        t.contract.try_expire_invoice(&missing),
        Err(Ok(ContractError::InvoiceNotFound))
    );
    assert_eq!(
        t.contract
            .try_dispute_invoice(&missing, &BytesN::from_array(&t.env, &[7u8; 32])),
        Err(Ok(ContractError::InvoiceNotFound))
    );
}

/// INV-NOUNWRAP2/INV-NOUNWRAP3: a dangling submitter-index entry (index
/// present, invoice payload gone) must not trap the paginated view; the
/// entry is skipped and the index is left intact for later repair.
#[test]
fn list_submitter_skips_dangling_index_entry() {
    let t = setup();
    let id = submit_one(&t);

    let present = t
        .contract
        .list_invoices_by_submitter(&t.freelancer, &0u32, &10u32);
    assert_eq!(present.len(), 1, "sanity: the invoice is listed");

    // Drop only the invoice payload; keep the submitter index entry.
    t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .persistent()
            .remove(&StorageKey::Invoice(id));
    });

    // The list must not trap; the dangling id is omitted.
    let after = t
        .contract
        .list_invoices_by_submitter(&t.freelancer, &0u32, &10u32);
    assert_eq!(after.len(), 0, "dangling index entry must be skipped");

    // The payload really is gone for the single-invoice read too.
    assert_eq!(
        t.contract.try_get_invoice(&id),
        Err(Ok(ContractError::InvoiceNotFound))
    );

    // INV-NOUNWRAP3: the index itself is untouched by the read.
    let reached = t
        .contract
        .list_invoices_by_submitter(&t.freelancer, &0u32, &10u32);
    assert_eq!(reached.len(), 0);
}
