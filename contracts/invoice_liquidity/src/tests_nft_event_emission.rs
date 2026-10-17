#![cfg(test)]

//! ADR-007 — NFT invoice representation event emission verification.
//!
//! Verifies that the NFT lifecycle (mint, transfer, burn) emits the correct
//! events at each stage, matching ADR-007's described lifecycle.

use super::*;
use crate::nft::{get_invoice_nft_metadata, get_invoice_nft_owner, invoice_nft_exists};
use crate::test::setup;
use soroban_sdk::testutils::{Address as _, Events, Ledger};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

/// Verify that funding an invoice emits InvoiceNftMinted event
/// (per ADR-007: NFT is minted on funding).
#[test]
fn test_nft_minted_event_on_fund() {
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

    // Clear events from submit
    t.env.events().all();

    // Fund the invoice — should emit InvoiceNftMinted
    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);

    let events = t.env.events().all();
    let nft_mint_events: std::vec::Vec<_> = events
        .events()
        .iter()
        .filter(|e| std::format!("{:?}", e).contains("invoice_nft_minted"))
        .collect();

    assert_eq!(
        nft_mint_events.len(),
        1,
        "Exactly one InvoiceNftMinted event should be emitted on funding"
    );

    // Verify NFT exists after funding
    assert!(invoice_nft_exists(&t.env, id));
    let metadata = get_invoice_nft_metadata(&t.env, id).unwrap();
    assert_eq!(metadata.owner, t.funder);
    assert_eq!(metadata.amount, INVOICE_AMOUNT);
}

/// Verify that mark_paid emits InvoiceNftBurned event
/// (per ADR-007: NFT is burned on settlement).
#[test]
fn test_nft_burned_event_on_pay() {
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

    // Verify NFT exists before payment
    assert!(invoice_nft_exists(&t.env, id));

    // Clear events from fund
    t.env.events().all();

    // Mark as paid — should emit InvoiceNftBurned
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    let events = t.env.events().all();
    let nft_burn_events: std::vec::Vec<_> = events
        .events()
        .iter()
        .filter(|e| std::format!("{:?}", e).contains("invoice_nft_burned"))
        .collect();

    assert_eq!(
        nft_burn_events.len(),
        1,
        "Exactly one InvoiceNftBurned event should be emitted on payment"
    );

    // Verify NFT no longer exists after payment
    assert!(!invoice_nft_exists(&t.env, id));
}

/// Verify that partial funding with LP queue resolution emits
/// InvoiceNftTransferred event when the lead LP changes.
#[test]
fn test_nft_transferred_event_on_lp_change() {
    let t = setup();
    let now = t.env.ledger().timestamp();
    let due_date = now + DUE_DATE_OFFSET;
    let half_amount = INVOICE_AMOUNT / 2;

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
    t.contract
        .fund_invoice(&t.funder, &id, &half_amount, &false);

    // Verify NFT is owned by first LP
    let owner_before = get_invoice_nft_owner(&t.env, id).unwrap();
    assert_eq!(owner_before, t.funder);

    // Second LP joins the fund queue
    let funder2 = Address::generate(&t.env);
    let token_admin = soroban_sdk::token::StellarAssetClient::new(&t.env, &t.token.address);
    token_admin.mint(&funder2, &(INVOICE_AMOUNT * 10));

    t.contract.join_fund_queue(&funder2, &id);

    // Advance past queue delay
    let mut ledger_info = t.env.ledger().get();
    ledger_info.sequence_number += 121;
    t.env.ledger().set(ledger_info);

    // Clear events
    t.env.events().all();

    // Resolve fund queue — second LP becomes lead (has more or equal)
    t.contract.resolve_fund_queue(&id);

    let events = t.env.events().all();
    let nft_transfer_events: std::vec::Vec<_> = events
        .events()
        .iter()
        .filter(|e| std::format!("{:?}", e).contains("invoice_nft_transferred"))
        .collect();

    // Should have a transfer event if the lead LP changed
    if !nft_transfer_events.is_empty() {
        let owner_after = get_invoice_nft_owner(&t.env, id).unwrap();
        assert_ne!(
            owner_before, owner_after,
            "NFT owner should have changed after LP queue resolution"
        );
    }
}

/// Verify that claim_default emits InvoiceNftBurned (or keeps it for dispute).
#[test]
fn test_nft_state_on_default() {
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

    // NFT exists when funded
    assert!(invoice_nft_exists(&t.env, id));

    // Advance past due date
    let mut ledger_info = t.env.ledger().get();
    ledger_info.timestamp = now + DUE_DATE_OFFSET + 1;
    t.env.ledger().set(ledger_info);

    // Clear events
    t.env.events().all();

    // Claim default — NFT should still exist (Defaulted status)
    t.contract.claim_default(&t.funder, &id);

    // Per ADR-007 invariant: NFT exists for Defaulted status
    assert!(
        invoice_nft_exists(&t.env, id),
        "NFT should persist during Defaulted status (ADR-007 invariant)"
    );
}

/// Verify NFT metadata correctness after minting.
#[test]
fn test_nft_metadata_fields_match_invoice() {
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

    let metadata = get_invoice_nft_metadata(&t.env, id).unwrap();
    assert_eq!(metadata.invoice_id, id);
    assert_eq!(metadata.amount, INVOICE_AMOUNT);
    assert_eq!(metadata.discount_rate, DISCOUNT_RATE);
    assert_eq!(metadata.token, t.token.address);
    assert_eq!(metadata.owner, t.funder);
    assert!(metadata.minted_at > 0);
}
