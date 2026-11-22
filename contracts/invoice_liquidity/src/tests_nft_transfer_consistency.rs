#![cfg(test)]

//! Issue #851 — LP position NFT transfer consistency.
//!
//! Verifies that when an NFT representing a funded position is transferred
//! (e.g. via LP queue resolution changing the lead LP), the reputation
//! attribution and insurance coverage eligibility remain consistent.

use super::*;
use crate::nft::{get_invoice_nft_metadata, get_invoice_nft_owner, invoice_nft_exists};
use crate::test::setup;
use soroban_sdk::testutils::{Address as _, Events, Ledger};

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

// ================================================================
// Issue #1182 — NFT transfer replay, unauthorized transfers, and
// event / owner-index consistency
// ================================================================
//
// Scope & data sources
// --------------------
// Exercised surface: `crate::nft::{transfer_invoice_nft,
// get_invoice_nft_owner, get_invoice_nft_metadata}` and the invoice entry
// points that drive `sync_nft_state` (`fund_invoice`, `join_fund_queue`,
// `resolve_fund_queue`). The NFT helpers are internal state-sync primitives
// (see the `nft.rs` module docs) and `transfer_invoice_nft` is never a
// top-level entry point.
//
// Authorization
// -------------
// `transfer_invoice_nft`'s `metadata.owner == from` check is a **consistency
// guard, not an authorization boundary** (`RR-1181-1`): the helper performs no
// `require_auth`. These tests therefore treat a "replay from a stale owner" as
// a consistency failure that must be rejected, not as an auth bypass.
//
// Failure behaviour
// -----------------
// A rejected transfer returns `Unauthorized` (owner mismatch) or
// `InvoiceNotFound` (no NFT) **before** any storage write or event publish, so
// the owner index, metadata, and event stream are left untouched (Soroban
// rolls the failed invocation back atomically).

/// Replaying a transfer from the previous (now-stale) owner must fail with
/// `Unauthorized` and must not move the NFT.
#[test]
fn test_nft_transfer_replay_from_stale_owner_rejected() {
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

    let holder_b = Address::generate(&t.env);
    let holder_c = Address::generate(&t.env);

    // First transfer: funder -> holder_b succeeds.
    crate::nft::transfer_invoice_nft(&t.env, id, t.funder.clone(), holder_b.clone()).unwrap();
    assert_eq!(get_invoice_nft_owner(&t.env, id).unwrap(), holder_b);

    // Replay with the *stale* owner (funder) must be rejected.
    let replay = crate::nft::transfer_invoice_nft(&t.env, id, t.funder.clone(), holder_c.clone());
    assert_eq!(
        replay,
        Err(ContractError::Unauthorized),
        "replaying a transfer from the stale owner must be rejected"
    );

    // The replay had no effect: ownership stays with holder_b, and holder_c
    // received nothing.
    assert_eq!(get_invoice_nft_owner(&t.env, id).unwrap(), holder_b);
    assert_eq!(
        get_invoice_nft_metadata(&t.env, id).unwrap().owner,
        holder_b
    );
    assert_ne!(get_invoice_nft_owner(&t.env, id).unwrap(), holder_c);
}

/// A rejected (unauthorized) transfer must leave both the owner index and the
/// full metadata record unchanged.
#[test]
fn test_nft_rejected_transfer_leaves_state_unchanged() {
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

    let owner_before = get_invoice_nft_owner(&t.env, id).unwrap();
    let metadata_before = get_invoice_nft_metadata(&t.env, id).unwrap();
    assert_eq!(owner_before, t.funder);

    // Attempt transfer from an address that is not the owner.
    let imposter = Address::generate(&t.env);
    let target = Address::generate(&t.env);
    let res = crate::nft::transfer_invoice_nft(&t.env, id, imposter, target);
    assert_eq!(res, Err(ContractError::Unauthorized));

    // Owner index and metadata are untouched (no partial update).
    assert_eq!(get_invoice_nft_owner(&t.env, id).unwrap(), owner_before);
    assert_eq!(
        get_invoice_nft_metadata(&t.env, id).unwrap(),
        metadata_before,
        "metadata must be unchanged after a rejected transfer"
    );
}

/// Chained transfers must keep the `InvoiceNftOwner` index and the
/// `InvoiceNft.owner` field in lock-step (INV-NFT-2) at every hop, so a query
/// can never observe a stale owner.
#[test]
fn test_nft_transfer_chain_maintains_owner_index_consistency() {
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

    let holder_b = Address::generate(&t.env);
    let holder_c = Address::generate(&t.env);

    // Hop 1: funder -> holder_b.
    crate::nft::transfer_invoice_nft(&t.env, id, t.funder.clone(), holder_b.clone()).unwrap();
    assert_eq!(get_invoice_nft_owner(&t.env, id).unwrap(), holder_b);
    assert_eq!(
        get_invoice_nft_metadata(&t.env, id).unwrap().owner,
        holder_b
    );

    // Hop 2: holder_b -> holder_c.
    crate::nft::transfer_invoice_nft(&t.env, id, holder_b.clone(), holder_c.clone()).unwrap();
    assert_eq!(get_invoice_nft_owner(&t.env, id).unwrap(), holder_c);
    assert_eq!(
        get_invoice_nft_metadata(&t.env, id).unwrap().owner,
        holder_c
    );
}

/// A single logical ownership change through the public funding path must emit
/// exactly one `invoice_nft_transferred` event — no duplicates — and leave the
/// index and metadata pointing at the new lead LP.
#[test]
fn test_nft_transfer_emits_exactly_one_event_on_lead_lp_change() {
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

    // First LP funds partially.
    t.contract
        .fund_invoice(&t.funder, &id, &half_amount, &false);
    let owner_before = get_invoice_nft_owner(&t.env, id).unwrap();
    assert_eq!(owner_before, t.funder);

    // Second LP joins the queue and wins on resolution.
    let funder2 = Address::generate(&t.env);
    let token_admin = soroban_sdk::token::StellarAssetClient::new(&t.env, &t.token.address);
    token_admin.mint(&funder2, &(INVOICE_AMOUNT * 10));
    t.contract.join_fund_queue(&funder2, &id);

    let mut ledger_info = t.env.ledger().get();
    ledger_info.sequence_number += 121;
    t.env.ledger().set(ledger_info);

    // Discard events captured so far — `events().all()` only reflects the most
    // recently completed invocation, and the next one is the assertion target.
    t.env.events().all();

    t.contract.resolve_fund_queue(&id);

    let events = t.env.events().all();
    let transfer_events: std::vec::Vec<_> = events
        .events()
        .iter()
        .filter(|e| std::format!("{:?}", e).contains("invoice_nft_transferred"))
        .collect();

    assert_eq!(
        transfer_events.len(),
        1,
        "exactly one InvoiceNftTransferred event per logical ownership change"
    );

    // The transfer really happened, and index + metadata agree afterwards.
    let owner_after = get_invoice_nft_owner(&t.env, id).unwrap();
    assert_ne!(
        owner_before, owner_after,
        "NFT owner should have changed after LP queue resolution"
    );
    assert_eq!(owner_after, funder2);
    assert_eq!(
        get_invoice_nft_metadata(&t.env, id).unwrap().owner,
        funder2
    );
}

/// RR-1182-1 (residual risk — characterization, NOT an endorsement):
/// `transfer_invoice_nft` does not reject a self-transfer (`from == to`). The
/// owner consistency guard passes (`metadata.owner == from`), the owner is
/// rewritten to the same address, and a redundant `invoice_nft_transferred`
/// event is emitted. State is a no-op, but the emitted event can look like a
/// duplicate to indexers; if self-transfers should be rejected, this test's
/// assertion must flip to `is_err()`.
#[test]
fn test_nft_self_transfer_observed_behaviour_rr1182_1() {
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

    let owner = get_invoice_nft_owner(&t.env, id).unwrap();
    let res = crate::nft::transfer_invoice_nft(&t.env, id, owner.clone(), owner.clone());
    assert!(
        res.is_ok(),
        "RR-1182-1: self-transfer is currently accepted (no-op)"
    );
    assert_eq!(get_invoice_nft_owner(&t.env, id).unwrap(), owner);
    assert_eq!(get_invoice_nft_metadata(&t.env, id).unwrap().owner, owner);
}
