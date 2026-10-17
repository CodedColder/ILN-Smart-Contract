#![cfg(test)]

//! Issue #851 — LP position NFT transfer consistency.
//!
//! Verifies that when an NFT representing a funded position is transferred
//! (e.g. via LP queue resolution changing the lead LP), the reputation
//! attribution and insurance coverage eligibility remain consistent.

use super::*;
use crate::nft::{get_invoice_nft_metadata, get_invoice_nft_owner, invoice_nft_exists};
use crate::test::setup;
use soroban_sdk::testutils::{Address as _, Ledger};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

/// Verify that NFT transfer does NOT change reputation attribution.
/// Reputation stays with the original payer address, not the NFT holder.
#[test]
fn test_nft_transfer_does_not_change_reputation_attribution() {
    let t = setup();
    let now = t.env.ledger().timestamp();
    let due_date = now + DUE_DATE_OFFSET;

    let id = t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    );
    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    // Record payer reputation before any transfer
    let rep_before = t.contract.get_reputation(&t.payer);
    let payer_score_before = t.contract.payer_score(&t.payer);

    // Transfer NFT to a new address (simulating LP exit)
    let new_holder = Address::generate(&t.env);
    crate::nft::transfer_invoice_nft(&t.env, id, t.funder.clone(), new_holder.clone()).unwrap();

    // Verify NFT owner changed
    let owner_after = get_invoice_nft_owner(&t.env, id).unwrap();
    assert_eq!(owner_after, new_holder);

    // Verify reputation is unchanged — it belongs to the payer, not the NFT holder
    let rep_after = t.contract.get_reputation(&t.payer);
    let payer_score_after = t.contract.payer_score(&t.payer);
    assert_eq!(
        rep_before, rep_after,
        "Reputation must not change on NFT transfer"
    );
    assert_eq!(
        payer_score_before, payer_score_after,
        "Payer score must not change on NFT transfer"
    );

    // The new holder should have no reputation from this invoice
    let new_holder_rep = t.contract.get_reputation(&new_holder);
    assert_eq!(
        new_holder_rep.score, 0,
        "New NFT holder must not inherit reputation"
    );
}

/// Verify that the NFT owner is correctly updated after LP queue resolution
/// changes the lead LP.
#[test]
fn test_nft_owner_follows_lead_lp_after_queue_resolution() {
    let t = setup();
    let now = t.env.ledger().timestamp();
    let due_date = now + DUE_DATE_OFFSET;
    let third = INVOICE_AMOUNT / 3;

    let id = t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    );

    // First LP funds partially
    t.contract.fund_invoice(&t.funder, &id, &third, &false);
    let first_owner = get_invoice_nft_owner(&t.env, id).unwrap();
    assert_eq!(first_owner, t.funder, "First LP should own the NFT");

    // Second LP joins with more — becomes lead LP
    let funder2 = Address::generate(&t.env);
    let token_admin = soroban_sdk::token::StellarAssetClient::new(&t.env, &t.token.address);
    token_admin.mint(&funder2, &(INVOICE_AMOUNT * 10));

    t.contract.join_fund_queue(&funder2, &id);

    // Advance past queue delay
    let mut ledger_info = t.env.ledger().get();
    ledger_info.sequence_number += 121;
    t.env.ledger().set(ledger_info);

    // Resolve fund queue
    t.contract.resolve_fund_queue(&id);

    // NFT should now be owned by the lead LP (funder2, who contributed more)
    let second_owner = get_invoice_nft_owner(&t.env, id).unwrap();
    assert_eq!(
        second_owner, funder2,
        "NFT should transfer to lead LP after queue resolution"
    );
}

/// Verify that NFT metadata is preserved across transfer.
#[test]
fn test_nft_metadata_preserved_across_transfer() {
    let t = setup();
    let now = t.env.ledger().timestamp();
    let due_date = now + DUE_DATE_OFFSET;

    let id = t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    );
    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let metadata_before = get_invoice_nft_metadata(&t.env, id).unwrap();

    // Transfer
    let new_holder = Address::generate(&t.env);
    crate::nft::transfer_invoice_nft(&t.env, id, t.funder.clone(), new_holder.clone()).unwrap();

    let metadata_after = get_invoice_nft_metadata(&t.env, id).unwrap();

    // Owner changed, everything else preserved
    assert_eq!(metadata_after.owner, new_holder);
    assert_eq!(metadata_after.invoice_id, metadata_before.invoice_id);
    assert_eq!(metadata_after.amount, metadata_before.amount);
    assert_eq!(metadata_after.due_date, metadata_before.due_date);
    assert_eq!(metadata_after.discount_rate, metadata_before.discount_rate);
    assert_eq!(metadata_after.token, metadata_before.token);
}

/// Verify that transferring an NFT does not affect the invoice's funder
/// field used for escrow accounting.
#[test]
fn test_nft_transfer_does_not_affect_escrow_funder() {
    let t = setup();
    let now = t.env.ledger().timestamp();
    let due_date = now + DUE_DATE_OFFSET;

    let id = t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    );
    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    // Transfer NFT
    let new_holder = Address::generate(&t.env);
    crate::nft::transfer_invoice_nft(&t.env, id, t.funder.clone(), new_holder.clone()).unwrap();

    // The invoice's funder field should still be the original funder
    // (escrow accounting uses Invoice.funder, not NFT owner)
    let invoice = load_invoice(&t.env, id);
    assert_eq!(
        invoice.funder,
        Some(t.funder.clone()),
        "Invoice.funder must not change when NFT is transferred"
    );
}

/// Verify that unauthorized transfer attempts are rejected.
#[test]
fn test_nft_unauthorized_transfer_rejected() {
    let t = setup();
    let now = t.env.ledger().timestamp();
    let due_date = now + DUE_DATE_OFFSET;

    let id = t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    );
    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let fake_owner = Address::generate(&t.env);
    let new_holder = Address::generate(&t.env);

    // Attempt transfer from wrong owner — should fail
    let result = crate::nft::transfer_invoice_nft(&t.env, id, fake_owner, new_holder);
    assert_eq!(
        result,
        Err(ContractError::Unauthorized),
        "Transfer from non-owner must be rejected"
    );
}
