#![cfg(test)]

//! Issue #1175 — Audit LP-position transfer ownership and balance changes.
//!
//! `transfer_lp_position` reassigns a *fully funded* invoice's LP position
//! (`Funded` only; every other status is rejected). This module verifies the
//! ownership triad it mutates and the downstream claims that follow the new
//! owner:
//!   * the primary funder pointer (`invoice.funder`),
//!   * the `InvoiceFunders` index (address/amount rows), and
//!   * the `LpInvoices` reverse index and the reputation-gated NFT holder,
//! plus that the *yield claim* right and the *settlement payout* move to the
//! transferee — with no token movement at transfer time.
//!
//! Findings are catalogued in `docs/lp-position-transfer.md`; each test maps
//! to an invariant (INV-LPT*) or verified finding (VF-LPT*).
//!
//! Implementation-only audit: nothing here changes contract behavior.

use super::*;
use crate::invoice::{get_invoice_funders, get_lp_invoices, get_lp_score, InvoiceStatus, ReferralCode};
use soroban_sdk::{
    testutils::{Address as _, Ledger, MockAuth, MockAuthInvoke},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env, IntoVal,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;
const SETUP_TIMESTAMP: u64 = 1_700_000_000;

struct TransferEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: TokenClient<'static>,
    freelancer: Address,
    payer: Address,
    lp_a: Address,
    lp_b: Address,
    lp_c: Address,
}

fn setup() -> TransferEnv {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let usdc_id = env.register_stellar_asset_contract_v2(admin.clone());
    let usdc_addr = usdc_id.address();

    let token = TokenClient::new(&env, &usdc_addr);
    let token_admin = StellarAssetClient::new(&env, &usdc_addr);

    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let lp_a = Address::generate(&env);
    let lp_b = Address::generate(&env);
    let lp_c = Address::generate(&env);

    token_admin.mint(&payer, &(INVOICE_AMOUNT * 100));
    token_admin.mint(&lp_a, &(INVOICE_AMOUNT * 100));
    token_admin.mint(&lp_b, &(INVOICE_AMOUNT * 100));
    token_admin.mint(&lp_c, &(INVOICE_AMOUNT * 100));

    let contract_id = env.register(InvoiceLiquidityContract, ());
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    // Fund the contract so it can service refunds / payouts.
    token_admin.mint(&contract.address, &(INVOICE_AMOUNT * 1000));

    let eurc_id = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let xlm_id = env.register_stellar_asset_contract_v2(Address::generate(&env));
    contract.initialize(&admin, &usdc_addr, &eurc_id.address(), &xlm_id.address());

    let mut ledger = env.ledger().get();
    ledger.timestamp = SETUP_TIMESTAMP;
    env.ledger().set(ledger);

    TransferEnv {
        env,
        contract,
        token,
        freelancer,
        payer,
        lp_a,
        lp_b,
        lp_c,
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

fn submit(t: &TransferEnv) -> u64 {
    let due = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due,
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    )
}

fn fund(t: &TransferEnv, lp: &Address, id: u64, amount: i128) {
    t.contract.fund_invoice(lp, &id, &amount, &false);
}

fn fund_full(t: &TransferEnv, lp: &Address, id: u64) {
    fund(t, lp, id, INVOICE_AMOUNT);
}

fn status_of(t: &TransferEnv, id: u64) -> InvoiceStatus {
    t.contract.get_invoice(&id).status
}

fn funder_of(t: &TransferEnv, id: u64) -> Option<Address> {
    t.contract.get_invoice(&id).funder
}

fn funders(t: &TransferEnv, id: u64) -> soroban_sdk::Vec<(Address, i128)> {
    t.env
        .as_contract(&t.contract.address, || get_invoice_funders(&t.env, id))
}

fn lp_index(t: &TransferEnv, lp: &Address) -> soroban_sdk::Vec<u64> {
    t.env
        .as_contract(&t.contract.address, || get_lp_invoices(&t.env, lp))
}

fn lp_score(t: &TransferEnv, lp: &Address) -> u32 {
    t.env
        .as_contract(&t.contract.address, || get_lp_score(&t.env, lp))
}

fn nft_owner(t: &TransferEnv, id: u64) -> Option<Address> {
    t.contract.query_nft_owner(&id)
}

fn count_id(v: &soroban_sdk::Vec<u64>, id: u64) -> u32 {
    let mut n = 0;
    for i in 0..v.len() {
        if v.get(i).unwrap() == id {
            n += 1;
        }
    }
    n
}

fn contains_id(v: &soroban_sdk::Vec<u64>, id: u64) -> bool {
    count_id(v, id) > 0
}

fn advance_past_due(t: &TransferEnv, id: u64) {
    let due = u64::from(t.contract.get_invoice(&id).due_date);
    let mut ledger = t.env.ledger().get();
    ledger.timestamp = due + 10;
    t.env.ledger().set(ledger);
}

// ================================================================
// Ownership triad — INV-LPT1 / INV-LPT2 / INV-LPT3
// ================================================================

/// INV-LPT1: a successful transfer re-points *every* ownership index at the
/// transferee in a single invocation: `invoice.funder`, the `InvoiceFunders`
/// row, the NFT holder, and the `LpInvoices` reverse index.
#[test]
fn test_transfer_updates_ownership_triad_atomically() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id);
    assert_eq!(status_of(&t, id), InvoiceStatus::Funded);

    assert_eq!(funder_of(&t, id), Some(t.lp_a.clone()));
    assert_eq!(funders(&t, id).len(), 1);
    assert_eq!(nft_owner(&t, id), Some(t.lp_a.clone()));
    assert!(contains_id(&lp_index(&t, &t.lp_a), id));
    assert!(!contains_id(&lp_index(&t, &t.lp_b), id));

    t.contract.transfer_lp_position(&id, &t.lp_b);

    assert_eq!(funder_of(&t, id), Some(t.lp_b.clone()));
    let f = funders(&t, id);
    assert_eq!(f.len(), 1);
    assert_eq!(f.get(0).unwrap().0, t.lp_b);
    assert_eq!(f.get(0).unwrap().1, INVOICE_AMOUNT);
    assert_eq!(nft_owner(&t, id), Some(t.lp_b.clone()));
    assert!(!contains_id(&lp_index(&t, &t.lp_a), id));
    assert_eq!(count_id(&lp_index(&t, &t.lp_b), id), 1);
}

/// INV-LPT2: the transfer is a *slot replacement* — the funded amount and the
/// row ordering of the `InvoiceFunders` index are preserved, only the address
/// in the matching row changes.
#[test]
fn test_transfer_replaces_index_slot_without_changing_amount_or_order() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id);

    let before = funders(&t, id);
    t.contract.transfer_lp_position(&id, &t.lp_b);
    let after = funders(&t, id);

    assert_eq!(before.len(), after.len(), "transfer must not add or drop funder rows");
    for i in 0..before.len() {
        assert_eq!(
            before.get(i).unwrap().1,
            after.get(i).unwrap().1,
            "funded amount at each position is preserved"
        );
    }
    assert_eq!(after.get(0).unwrap().0, t.lp_b, "the matching row names the transferee");
}

/// INV-LPT3: chained transfers converge on exactly one canonical owner — no
/// stale rows or dangling reverse-index entries accumulate.
#[test]
fn test_chained_transfer_keeps_single_canonical_owner() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id);

    t.contract.transfer_lp_position(&id, &t.lp_b);
    t.contract.transfer_lp_position(&id, &t.lp_c);

    assert_eq!(funder_of(&t, id), Some(t.lp_c.clone()));
    let f = funders(&t, id);
    assert_eq!(f.len(), 1);
    assert_eq!(f.get(0).unwrap().0, t.lp_c);
    assert_eq!(f.get(0).unwrap().1, INVOICE_AMOUNT);
    assert_eq!(nft_owner(&t, id), Some(t.lp_c.clone()));
    assert_eq!(count_id(&lp_index(&t, &t.lp_c), id), 1);
    assert!(!contains_id(&lp_index(&t, &t.lp_a), id));
    assert!(!contains_id(&lp_index(&t, &t.lp_b), id));
}

/// VF-LPT1 (verified): the funder-row rewrite is *per matching row* and does
/// not coalesce. If the transferee is already a partial funder of the same
/// invoice, the address ends up listed twice with independent amounts. This is
/// benign for settlement (each row is paid its own share) but is a real
/// consumer/UI hazard: the index can hold duplicate `(address, amount)` rows.
#[test]
fn test_transfer_to_prior_partial_funder_duplicates_index_rows() {
    let t = setup();
    let id = submit(&t);
    let half = INVOICE_AMOUNT / 2;

    // A funds half → PartiallyFunded; B completes → Funded, B is the pointer.
    fund(&t, &t.lp_a, id, half);
    assert_eq!(status_of(&t, id), InvoiceStatus::PartiallyFunded);
    fund(&t, &t.lp_b, id, half);
    assert_eq!(status_of(&t, id), InvoiceStatus::Funded);
    assert_eq!(funder_of(&t, id), Some(t.lp_b.clone()));
    assert_eq!(funders(&t, id).len(), 2);

    // Transfer B's completing position to A, who is already a partial funder.
    t.contract.transfer_lp_position(&id, &t.lp_a);

    let after = funders(&t, id);
    assert_eq!(after.len(), 2, "rows are replaced, not coalesced");
    assert_eq!(after.get(0).unwrap().0, t.lp_a);
    assert_eq!(after.get(1).unwrap().0, t.lp_a);
    assert_eq!(after.get(0).unwrap().1, half);
    assert_eq!(after.get(1).unwrap().1, half);
    // The reverse index still de-duplicates the single invoice id for A.
    assert_eq!(count_id(&lp_index(&t, &t.lp_a), id), 1);
    assert_eq!(funder_of(&t, id), Some(t.lp_a.clone()));
    assert_eq!(nft_owner(&t, id), Some(t.lp_a.clone()));
}

/// INV-LPT4: the reverse-index rewrite is scoped to the one invoice; an LP's
/// other positions are untouched, and the transferee gains only this id.
#[test]
fn test_transfer_only_reindexes_this_invoice() {
    let t = setup();
    let id1 = submit(&t);
    let id2 = submit(&t);
    fund_full(&t, &t.lp_a, id1);
    fund_full(&t, &t.lp_a, id2);

    t.contract.transfer_lp_position(&id1, &t.lp_b);

    let a_idx = lp_index(&t, &t.lp_a);
    assert!(!contains_id(&a_idx, id1), "transferred invoice leaves A's index");
    assert!(contains_id(&a_idx, id2), "A's other position is untouched");
    assert!(contains_id(&lp_index(&t, &t.lp_b), id1));
    assert!(!contains_id(&lp_index(&t, &t.lp_b), id2));
}

// ================================================================
// Yield claims follow the owner — INV-LPT5
// ================================================================

/// INV-LPT5: `claim_yield` gates on the *current* `invoice.funder`, so the old
/// owner loses the yield right the moment the transfer lands.
#[test]
fn test_old_lp_loses_yield_claim_after_transfer() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id);
    t.contract.transfer_lp_position(&id, &t.lp_b);

    // Only A authorises; the contract now requires the current funder (B) to
    // authorise, so A's authorisation must be insufficient.
    t.env.mock_auths(&[MockAuth {
        address: &t.lp_a,
        invoke: &MockAuthInvoke {
            contract: &t.contract.address,
            fn_name: "claim_yield",
            args: (id,).into_val(&t.env),
            sub_invokes: &[],
        },
    }]);

    assert!(
        t.contract.try_claim_yield(&id).is_err(),
        "old LP must no longer control the yield claim"
    );
}

/// INV-LPT5: after settlement the transferee — and only the transferee — can
/// realise the yield.
#[test]
fn test_new_lp_controls_yield_after_settlement() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id);
    t.contract.transfer_lp_position(&id, &t.lp_b);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);
    assert_eq!(status_of(&t, id), InvoiceStatus::Paid);

    let expected = INVOICE_AMOUNT * DISCOUNT_RATE as i128 / 10_000;
    assert_eq!(t.contract.claim_yield(&id), expected);
}

/// INV-LPT5: while the invoice is still `Funded` the claimable yield is zero,
/// regardless of who owns the position.
#[test]
fn test_claim_yield_zero_while_still_funded_after_transfer() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id);
    t.contract.transfer_lp_position(&id, &t.lp_b);
    assert_eq!(t.contract.claim_yield(&id), 0);
}

/// INV-LPT6: the settlement payout is driven by the `InvoiceFunders` index, so
/// after the rewrite the transferee receives the position payout and the old
/// owner receives nothing.
#[test]
fn test_settlement_pays_new_owner_only() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id);
    t.contract.transfer_lp_position(&id, &t.lp_b);

    let a_before = t.token.balance(&t.lp_a);
    let b_before = t.token.balance(&t.lp_b);

    t.contract.mark_paid(&id, &INVOICE_AMOUNT);

    assert_eq!(t.token.balance(&t.lp_a), a_before, "old LP receives nothing");
    assert!(
        t.token.balance(&t.lp_b) > b_before,
        "new LP receives the funded position payout"
    );
}

// ================================================================
// NFT holder — INV-LPT7
// ================================================================

/// INV-LPT7: the invoice NFT tracks the funder pointer — it is transferred on
/// transfer and burned once the invoice leaves the fundable set (settlement).
#[test]
fn test_nft_owner_follows_transfer_then_burns_on_settlement() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id);
    assert_eq!(nft_owner(&t, id), Some(t.lp_a.clone()));

    t.contract.transfer_lp_position(&id, &t.lp_b);
    assert_eq!(nft_owner(&t, id), Some(t.lp_b.clone()));

    t.contract.mark_paid(&id, &INVOICE_AMOUNT);
    assert_eq!(nft_owner(&t, id), None, "NFT is burned once the invoice settles");
}

// ================================================================
// Balances & reputation are not moved by the transfer — INV-LPT8 / VF-LPT2
// ================================================================

/// INV-LPT8: `transfer_lp_position` moves *no* tokens — it is a pure
/// re-indexing of an already-funded position.
#[test]
fn test_transfer_moves_no_tokens() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id);

    let a = t.token.balance(&t.lp_a);
    let b = t.token.balance(&t.lp_b);
    let c = t.token.balance(&t.contract.address);

    t.contract.transfer_lp_position(&id, &t.lp_b);

    assert_eq!(t.token.balance(&t.lp_a), a);
    assert_eq!(t.token.balance(&t.lp_b), b);
    assert_eq!(t.token.balance(&t.contract.address), c);
}

/// VF-LPT2 (verified): reputation is *not* part of the transferred position.
/// The funding-time score credit stays with the funder of record; the
/// transferee gains none. Consumers must not infer LP quality from the current
/// holder alone.
#[test]
fn test_transfer_does_not_move_reputation_score() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id);

    let a_score_before = lp_score(&t, &t.lp_a);
    let b_score_before = lp_score(&t, &t.lp_b);

    t.contract.transfer_lp_position(&id, &t.lp_b);

    assert_eq!(
        lp_score(&t, &t.lp_a),
        a_score_before,
        "funding credit stays with the funder of record"
    );
    assert_eq!(
        lp_score(&t, &t.lp_b),
        b_score_before,
        "the transferee gains no reputation from a transfer"
    );
}

// ================================================================
// Event — INV-LPT9
// ================================================================

/// INV-LPT9: a successful transfer emits exactly one `lp_position_transferred`
/// event, giving indexers the authority transfer they rely on.
#[test]
fn test_transfer_emits_single_position_event() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id);
    t.env.events().all(); // drop prior events

    t.contract.transfer_lp_position(&id, &t.lp_b);

    let events = t.env.events().all();
    let matched = events
        .events()
        .iter()
        .filter(|e| std::format!("{:?}", e).contains("lp_position_transferred"))
        .count();
    assert_eq!(matched, 1, "exactly one position-transfer event");
}

// ================================================================
// Authorization — INV-LPT10
// ================================================================

/// INV-LPT10: only the current holder's authorization is accepted; an
/// unauthorised caller cannot move the position and mutates no state.
#[test]
fn test_transfer_requires_current_lp_authorization() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id);
    let imposter = Address::generate(&t.env);

    t.env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &t.contract.address,
            fn_name: "transfer_lp_position",
            args: (id, t.lp_b.clone()).into_val(&t.env),
            sub_invokes: &[],
        },
    }]);

    assert!(t.contract.try_transfer_lp_position(&id, &t.lp_b).is_err());
    assert_eq!(
        funder_of(&t, id),
        Some(t.lp_a.clone()),
        "state is untouched when authorization fails"
    );
}

/// INV-LPT10: transferring to the current holder is rejected and leaves the
/// ownership triad unchanged.
#[test]
fn test_same_lp_transfer_rejected_and_state_untouched() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id);

    let res = t.contract.try_transfer_lp_position(&id, &t.lp_a);
    assert_eq!(res, Err(Ok(ContractError::Unauthorized)));
    assert_eq!(funder_of(&t, id), Some(t.lp_a.clone()));
    assert_eq!(funders(&t, id).len(), 1);
    assert_eq!(nft_owner(&t, id), Some(t.lp_a.clone()));
    assert_eq!(count_id(&lp_index(&t, &t.lp_a), id), 1);
}

// ================================================================
// Status gates — INV-LPT11
// ================================================================

/// INV-LPT11: transfer is only valid for `Funded`. Unfunded statuses surface
/// `NotFunded` and set no funder pointer.
#[test]
fn test_transfer_rejected_for_pending_and_partial_states() {
    let t = setup();
    let new_lp = Address::generate(&t.env);

    let pending = submit(&t);
    assert_eq!(
        t.contract.try_transfer_lp_position(&pending, &new_lp),
        Err(Ok(ContractError::NotFunded))
    );
    assert_eq!(funder_of(&t, pending), None);

    let partial = submit(&t);
    fund(&t, &t.lp_a, partial, INVOICE_AMOUNT / 2);
    assert_eq!(status_of(&t, partial), InvoiceStatus::PartiallyFunded);
    assert_eq!(
        t.contract.try_transfer_lp_position(&partial, &new_lp),
        Err(Ok(ContractError::NotFunded))
    );
    assert_eq!(
        funder_of(&t, partial),
        None,
        "partial funding sets no primary funder"
    );
}

/// INV-LPT11: terminal / outcome-withheld statuses each surface their typed
/// error rather than silently succeeding.
#[test]
fn test_transfer_rejected_for_paid_expired_and_cancelled() {
    let t = setup();

    // Paid
    let paid = submit(&t);
    fund_full(&t, &t.lp_a, paid);
    t.contract.mark_paid(&paid, &INVOICE_AMOUNT);
    assert_eq!(
        t.contract.try_transfer_lp_position(&paid, &t.lp_b),
        Err(Ok(ContractError::AlreadyPaid))
    );

    // Expired (Pending, past due)
    let expired = submit(&t);
    advance_past_due(&t, expired);
    t.contract.expire_invoice(&expired);
    assert_eq!(status_of(&t, expired), InvoiceStatus::Expired);
    assert_eq!(
        t.contract.try_transfer_lp_position(&expired, &t.lp_b),
        Err(Ok(ContractError::InvoiceExpired))
    );

    // Cancelled (Pending)
    let cancelled = submit(&t);
    t.contract.cancel_invoice(&cancelled);
    assert_eq!(status_of(&t, cancelled), InvoiceStatus::Cancelled);
    assert_eq!(
        t.contract.try_transfer_lp_position(&cancelled, &t.lp_b),
        Err(Ok(ContractError::AlreadyCancelled))
    );
}

/// INV-LPT11: a defaulted position cannot be transferred — the outcome must be
/// resolved on the funded holder, not reassigned mid-flight.
#[test]
fn test_transfer_rejected_for_defaulted_position() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id);
    advance_past_due(&t, id);
    t.contract.claim_default(&t.lp_a, &id);
    assert_eq!(status_of(&t, id), InvoiceStatus::Defaulted);

    assert_eq!(
        t.contract.try_transfer_lp_position(&id, &t.lp_b),
        Err(Ok(ContractError::InvoiceDefaulted))
    );
}

// ================================================================
// Boundary gates — INV-LPT12
// ================================================================

/// INV-LPT12: an unknown invoice is rejected before any auth or state access.
#[test]
fn test_transfer_unknown_invoice_rejected() {
    let t = setup();
    let new_lp = Address::generate(&t.env);
    assert_eq!(
        t.contract.try_transfer_lp_position(&9999, &new_lp),
        Err(Ok(ContractError::InvoiceNotFound))
    );
}

/// INV-LPT12: a paused contract rejects the transfer and leaves ownership
/// intact.
#[test]
fn test_transfer_blocked_when_paused() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id);

    t.contract.pause();
    assert_eq!(
        t.contract.try_transfer_lp_position(&id, &t.lp_b),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(funder_of(&t, id), Some(t.lp_a.clone()));
    t.contract.unpause();
}

// ================================================================
// Round-trip — INV-LPT3
// ================================================================

/// INV-LPT3: a position can be transferred back to its original owner; the
/// ownership triad remains singular and consistent in both directions.
#[test]
fn test_position_can_be_transferred_back_to_original_lp() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id);

    t.contract.transfer_lp_position(&id, &t.lp_b);
    t.contract.transfer_lp_position(&id, &t.lp_a);

    assert_eq!(funder_of(&t, id), Some(t.lp_a.clone()));
    let f = funders(&t, id);
    assert_eq!(f.len(), 1);
    assert_eq!(f.get(0).unwrap().0, t.lp_a);
    assert_eq!(nft_owner(&t, id), Some(t.lp_a.clone()));
    assert_eq!(count_id(&lp_index(&t, &t.lp_a), id), 1);
    assert!(!contains_id(&lp_index(&t, &t.lp_b), id));
}
