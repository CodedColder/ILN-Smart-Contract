#![cfg(test)]

//! Issue #1172 — Audit dispute submission and resolution transitions.
//!
//! The dispute mechanism spans three entry points:
//!   * `dispute_invoice`       — payer raises a dispute before settlement.
//!   * `resolve_dispute`       — admin/governance resolves an open dispute.
//!   * `auto_resolve_dispute`  — permissionless timeout escape hatch.
//!
//! This module verifies that these entry points cannot bypass the invoice
//! state machine (see `InvoiceStatus` transition map in `invoice.rs`) or the
//! authorization rules, and that the "evidence window" (`dispute_timeout_ledgers`
//! plus the stored `DisputeRecord`) behaves as documented.
//!
//! Findings are catalogued in `docs/dispute-transitions.md`; each test below
//! maps to one invariant (INV-DT*) or verified finding (VF-*) there.
//!
//! Implementation-only audit: nothing here changes contract behavior.

use super::*;
use crate::invoice::{get_dispute, InvoiceStatus, ReferralCode};
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger, MockAuth, MockAuthInvoke},
    token::{Client as TokenClient, StellarAssetClient},
    xdr, Address, BytesN, Env, IntoVal, TryFromVal, Val,
};

const INVOICE_AMOUNT: i128 = 10_000_000; // 10 USDC
const DISCOUNT_RATE: u32 = 300; // 3%
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30; // 30 days
const SETUP_TIMESTAMP: u64 = 1_700_000_000;

#[allow(dead_code)]
struct DisputeTestEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: TokenClient<'static>,
    admin: Address,
    freelancer: Address,
    payer: Address,
    funder: Address,
}

fn setup() -> DisputeTestEnv {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let usdc_id = env.register_stellar_asset_contract_v2(admin.clone());
    let usdc_addr = usdc_id.address();

    let token = TokenClient::new(&env, &usdc_addr);
    let token_admin = StellarAssetClient::new(&env, &usdc_addr);

    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let funder = Address::generate(&env);

    token_admin.mint(&funder, &(INVOICE_AMOUNT * 20));
    token_admin.mint(&payer, &(INVOICE_AMOUNT * 20));

    let contract_id = env.register(InvoiceLiquidityContract, ());
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    // Fund the contract so it can service LP / payer refunds on an upheld dispute.
    token_admin.mint(&contract.address, &(INVOICE_AMOUNT * 100));

    let eurc_id = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let xlm_id = env.register_stellar_asset_contract_v2(Address::generate(&env));

    contract.initialize(&admin, &usdc_addr, &eurc_id.address(), &xlm_id.address());

    let mut ledger = env.ledger().get();
    ledger.timestamp = SETUP_TIMESTAMP;
    env.ledger().set(ledger);

    DisputeTestEnv {
        env,
        contract,
        token,
        admin,
        freelancer,
        payer,
        funder,
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

fn reason(env: &Env, byte: u8) -> BytesN<32> {
    BytesN::from_array(env, &[byte; 32])
}

fn submit(t: &DisputeTestEnv) -> u64 {
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

fn fund(t: &DisputeTestEnv, id: u64, amount: i128) {
    t.contract.fund_invoice(&t.funder, &id, &amount, &false);
}

fn dispute(t: &DisputeTestEnv, id: u64) {
    t.contract.dispute_invoice(&id, &reason(&t.env, 0xAA));
}

fn status_of(t: &DisputeTestEnv, id: u64) -> InvoiceStatus {
    t.contract.get_invoice(&id).status
}

fn due_of(t: &DisputeTestEnv, id: u64) -> u64 {
    u64::from(t.contract.get_invoice(&id).due_date)
}

/// Advance the ledger timestamp past the invoice's due date so default/expiry
/// paths are reachable.
fn advance_past_due(t: &DisputeTestEnv, id: u64) {
    let mut ledger = t.env.ledger().get();
    ledger.timestamp = due_of(t, id) + 10;
    t.env.ledger().set(ledger);
}

/// Advance the ledger sequence number by `delta`.
fn advance_ledger(t: &DisputeTestEnv, delta: u32) {
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number = ledger.sequence_number.saturating_add(delta);
    t.env.ledger().set(ledger);
}

/// Read the stored `DisputeRecord` directly from contract storage.
fn stored_dispute(t: &DisputeTestEnv, id: u64) -> Option<(BytesN<32>, u32)> {
    t.env
        .as_contract(&t.contract.address, || {
            get_dispute(&t.env, id).map(|d| (d.reason_hash, d.disputed_at))
        })
}

/// Scan the contract's most recent events for one whose data decodes as `T`.
fn find_last_event<T>(t: &DisputeTestEnv) -> Option<T>
where
    T: TryFromVal<Env, Val>,
{
    let raw = t
        .env
        .events()
        .all()
        .filter_by_contract(&t.contract.address)
        .events();
    let mut i = raw.len();
    while i > 0 {
        i -= 1;
        let ev = raw.get(i).expect("event index in range");
        let body = match &ev.body {
            xdr::ContractEventBody::V0(body) => body,
        };
        if let Ok(val) = Val::try_from_val(&t.env, &body.data) {
            if let Ok(decoded) = T::try_from_val(&t.env, &val) {
                return Some(decoded);
            }
        }
    }
    None
}

// ================================================================
// Authorization — INV-DT1 / INV-DT2
// ================================================================

/// `dispute_invoice` must be authorized by the invoice payer only (INVARIANT A5).
#[test]
fn test_dispute_requires_payer_authorization() {
    let t = setup();
    let id = submit(&t);
    let imposter = Address::generate(&t.env);

    // Replace the permissive all-auths mode with an explicit non-payer auth:
    // the payer's `require_auth()` must then fail.
    t.env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &t.contract.address,
            fn_name: "dispute_invoice",
            args: (id, reason(&t.env, 0x01)).into_val(&t.env),
            sub_invokes: &[],
        },
    }]);

    let res = t.contract.try_dispute_invoice(&id, &reason(&t.env, 0x01));
    assert!(res.is_err(), "dispute_invoice must require the payer's auth");
    assert_eq!(
        status_of(&t, id),
        InvoiceStatus::Pending,
        "a rejected dispute must not mutate the invoice"
    );
}

/// `resolve_dispute` must be authorized by the admin only.
#[test]
fn test_resolve_requires_admin_authorization() {
    let t = setup();
    let id = submit(&t);
    dispute(&t, id);

    let imposter = Address::generate(&t.env);
    t.env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &t.contract.address,
            fn_name: "resolve_dispute",
            args: (id, reason(&t.env, 0x02), 1u32).into_val(&t.env),
            sub_invokes: &[],
        },
    }]);

    let res = t.contract.try_resolve_dispute(&id, &reason(&t.env, 0x02), &1);
    assert!(res.is_err(), "resolve_dispute must require admin auth");
    assert_eq!(
        status_of(&t, id),
        InvoiceStatus::Disputed,
        "a rejected resolution must not mutate the invoice"
    );
}

/// `auto_resolve_dispute` is intentionally permissionless — an arbitrary caller
/// with no auth at all can trigger it once the timeout has elapsed.
#[test]
fn test_auto_resolve_is_permissionless() {
    let t = setup();
    let id = submit(&t);
    dispute(&t, id);

    let timeout = t.contract.get_config().dispute_timeout_ledgers as u32;
    advance_ledger(&t, timeout);

    // No authorization granted at all.
    t.env.mock_auths(&[]);
    let res = t.contract.try_auto_resolve_dispute(&id);
    assert!(
        res.is_ok(),
        "auto_resolve_dispute must be callable by anyone after the timeout"
    );
    assert_eq!(status_of(&t, id), InvoiceStatus::Pending);
}

// ================================================================
// dispute_invoice state gates — INV-DT3
// ================================================================

#[test]
fn test_dispute_pending_invoice_succeeds() {
    let t = setup();
    let id = submit(&t);
    t.contract.dispute_invoice(&id, &reason(&t.env, 0x10));
    assert_eq!(status_of(&t, id), InvoiceStatus::Disputed);
}

#[test]
fn test_dispute_partially_funded_invoice_succeeds() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT / 2);
    dispute(&t, id);
    assert_eq!(status_of(&t, id), InvoiceStatus::Disputed);
}

#[test]
fn test_dispute_funded_invoice_succeeds() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT);
    dispute(&t, id);
    assert_eq!(status_of(&t, id), InvoiceStatus::Disputed);
}

#[test]
fn test_dispute_paid_invoice_rejected() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT);
    t.contract.mark_paid(&id, &INVOICE_AMOUNT);
    assert_eq!(status_of(&t, id), InvoiceStatus::Paid);

    let res = t.contract.try_dispute_invoice(&id, &reason(&t.env, 0x11));
    assert_eq!(res, Err(Ok(ContractError::AlreadyPaid)));
}

#[test]
fn test_dispute_defaulted_invoice_rejected() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT);
    advance_past_due(&t, id);
    t.contract.claim_default(&t.funder, &id);
    assert_eq!(status_of(&t, id), InvoiceStatus::Defaulted);

    let res = t.contract.try_dispute_invoice(&id, &reason(&t.env, 0x12));
    assert_eq!(res, Err(Ok(ContractError::InvoiceDefaulted)));
}

#[test]
fn test_dispute_appealed_invoice_rejected() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT);
    advance_past_due(&t, id);
    t.contract.claim_default(&t.funder, &id);
    t.contract.appeal_default(&id, &reason(&t.env, 0x13));
    assert_eq!(status_of(&t, id), InvoiceStatus::Appealed);

    let res = t.contract.try_dispute_invoice(&id, &reason(&t.env, 0x14));
    assert_eq!(res, Err(Ok(ContractError::InvoiceAppealed)));
}

#[test]
fn test_dispute_expired_invoice_rejected() {
    let t = setup();
    let id = submit(&t);
    advance_past_due(&t, id);
    t.contract.expire_invoice(&id);
    assert_eq!(status_of(&t, id), InvoiceStatus::Expired);

    let res = t.contract.try_dispute_invoice(&id, &reason(&t.env, 0x15));
    assert_eq!(res, Err(Ok(ContractError::InvoiceExpired)));
}

#[test]
fn test_dispute_cancelled_invoice_rejected() {
    let t = setup();
    let id = submit(&t);
    t.contract.cancel_invoice(&id);
    assert_eq!(status_of(&t, id), InvoiceStatus::Cancelled);

    let res = t.contract.try_dispute_invoice(&id, &reason(&t.env, 0x16));
    assert_eq!(res, Err(Ok(ContractError::AlreadyCancelled)));
}

/// A second dispute on an already-disputed invoice is rejected.
#[test]
fn test_double_dispute_rejected() {
    let t = setup();
    let id = submit(&t);
    dispute(&t, id);

    let res = t.contract.try_dispute_invoice(&id, &reason(&t.env, 0x17));
    assert_eq!(res, Err(Ok(ContractError::AlreadyDisputed)));
    assert_eq!(status_of(&t, id), InvoiceStatus::Disputed);
}

#[test]
fn test_dispute_blocked_when_paused() {
    let t = setup();
    let id = submit(&t);
    t.contract.pause();

    let res = t.contract.try_dispute_invoice(&id, &reason(&t.env, 0x18));
    assert_eq!(res, Err(Ok(ContractError::ContractPaused)));
}

#[test]
fn test_dispute_missing_invoice_rejected() {
    let t = setup();
    let res = t.contract.try_dispute_invoice(&999, &reason(&t.env, 0x19));
    assert_eq!(res, Err(Ok(ContractError::InvoiceNotFound)));
}

// ================================================================
// Dispute record lifetime — INV-DT6 (one dispute per invoice, ever)
// ================================================================

/// The `DisputeRecord` is never cleared on resolution, so once an invoice has
/// been disputed it can never be disputed again — even after a *rejected*
/// resolution returns it to a disputable funding state.
#[test]
fn test_redispute_after_rejected_resolution_rejected() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT);
    dispute(&t, id);

    t.contract.resolve_dispute(&id, &reason(&t.env, 0x1A), &2);
    assert_eq!(status_of(&t, id), InvoiceStatus::Funded);

    let res = t.contract.try_dispute_invoice(&id, &reason(&t.env, 0x1B));
    assert_eq!(res, Err(Ok(ContractError::AlreadyDisputed)));
}

#[test]
fn test_redispute_after_auto_resolve_rejected() {
    let t = setup();
    let id = submit(&t);
    dispute(&t, id);

    let timeout = t.contract.get_config().dispute_timeout_ledgers as u32;
    advance_ledger(&t, timeout);
    t.contract.auto_resolve_dispute(&id);
    assert_eq!(status_of(&t, id), InvoiceStatus::Pending);

    let res = t.contract.try_dispute_invoice(&id, &reason(&t.env, 0x1C));
    assert_eq!(res, Err(Ok(ContractError::AlreadyDisputed)));
}

/// VF-DT1: the stored `DisputeRecord.disputed_at` is a *ledger sequence* while
/// the `InvoiceDisputed` event's `disputed_at` field is a *timestamp*. Both are
/// recorded for the same dispute, in different units.
#[test]
fn test_dispute_record_ledger_vs_event_timestamp_units() {
    let t = setup();
    let id = submit(&t);

    let seq_before = t.env.ledger().sequence();
    let ts = 1_800_000_000u64;
    let mut ledger = t.env.ledger().get();
    ledger.timestamp = ts;
    t.env.ledger().set(ledger);

    let hash = reason(&t.env, 0x1D);
    t.contract.dispute_invoice(&id, &hash);

    let (stored_hash, stored_at) = stored_dispute(&t, id).expect("dispute record persisted");
    assert_eq!(stored_hash, hash, "stored record keeps the evidence hash");
    assert_eq!(
        stored_at, seq_before,
        "stored DisputeRecord.disputed_at is the ledger sequence"
    );

    let ev = find_last_event::<InvoiceDisputed>(&t).expect("InvoiceDisputed event emitted");
    assert_eq!(ev.invoice_id, id);
    assert_eq!(ev.reason_hash, hash);
    assert_eq!(
        ev.disputed_at, ts,
        "event disputed_at is a timestamp, not a ledger sequence"
    );
}

// ================================================================
// resolve_dispute state + validation — INV-DT4 / INV-DT5
// ================================================================

#[test]
fn test_resolve_requires_disputed_state() {
    let t = setup();
    let id = submit(&t); // Pending

    let res = t.contract.try_resolve_dispute(&id, &reason(&t.env, 0x20), &1);
    assert_eq!(res, Err(Ok(ContractError::NotDisputed)));
}

#[test]
fn test_resolve_missing_invoice_rejected() {
    let t = setup();
    let res = t.contract.try_resolve_dispute(&999, &reason(&t.env, 0x21), &1);
    assert_eq!(res, Err(Ok(ContractError::InvoiceNotFound)));
}

#[test]
fn test_resolve_invalid_resolution_zero_rejected() {
    let t = setup();
    let id = submit(&t);
    dispute(&t, id);

    let res = t.contract.try_resolve_dispute(&id, &reason(&t.env, 0x22), &0);
    assert_eq!(res, Err(Ok(ContractError::Unauthorized)));
    assert_eq!(
        status_of(&t, id),
        InvoiceStatus::Disputed,
        "an invalid resolution code must not change state"
    );
}

#[test]
fn test_resolve_invalid_resolution_high_rejected() {
    let t = setup();
    let id = submit(&t);
    dispute(&t, id);

    let res = t
        .contract
        .try_resolve_dispute(&id, &reason(&t.env, 0x23), &u32::MAX);
    assert_eq!(res, Err(Ok(ContractError::Unauthorized)));

    // The invoice remains resolvable afterwards with a valid code.
    t.contract.resolve_dispute(&id, &reason(&t.env, 0x24), &2);
    assert_eq!(status_of(&t, id), InvoiceStatus::Pending);
}

#[test]
fn test_resolve_upheld_cancels_and_refunds_funder() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT);
    dispute(&t, id);

    let funder_balance_before = t.token.balance(&t.funder);
    t.contract.resolve_dispute(&id, &reason(&t.env, 0x25), &1);

    assert_eq!(status_of(&t, id), InvoiceStatus::Cancelled);
    assert!(
        t.token.balance(&t.funder) > funder_balance_before,
        "an upheld dispute must refund the LP principal"
    );
}

#[test]
fn test_resolve_upheld_refunds_partial_payer_and_emits_event() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT);
    t.contract.mark_paid(&id, &(INVOICE_AMOUNT / 2));
    dispute(&t, id);

    let payer_balance_before = t.token.balance(&t.payer);
    t.contract.resolve_dispute(&id, &reason(&t.env, 0x26), &1);

    assert_eq!(status_of(&t, id), InvoiceStatus::Cancelled);
    assert!(
        t.token.balance(&t.payer) > payer_balance_before,
        "an upheld dispute must refund the payer's partial payment"
    );

    let refund = find_last_event::<DisputeUpheldPayerRefund>(&t)
        .expect("dispute_upheld_payer_refund event emitted");
    assert_eq!(refund.invoice_id, id);
    assert_eq!(refund.amount, INVOICE_AMOUNT / 2);
}

#[test]
fn test_resolve_rejected_restores_funded() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT);
    dispute(&t, id);

    t.contract.resolve_dispute(&id, &reason(&t.env, 0x27), &2);
    assert_eq!(status_of(&t, id), InvoiceStatus::Funded);
}

#[test]
fn test_resolve_rejected_restores_partially_funded() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT / 2);
    dispute(&t, id);

    t.contract.resolve_dispute(&id, &reason(&t.env, 0x28), &2);
    assert_eq!(status_of(&t, id), InvoiceStatus::PartiallyFunded);
}

#[test]
fn test_resolve_rejected_restores_pending() {
    let t = setup();
    let id = submit(&t); // no funding
    dispute(&t, id);

    t.contract.resolve_dispute(&id, &reason(&t.env, 0x29), &2);
    assert_eq!(status_of(&t, id), InvoiceStatus::Pending);
}

/// The `dispute_resolved` event reports the resolution code that was applied.
#[test]
fn test_resolve_emits_dispute_resolved_event_with_code() {
    let t = setup();
    let id = submit(&t);
    dispute(&t, id);

    let resolution_hash = reason(&t.env, 0x2A);
    t.contract.resolve_dispute(&id, &resolution_hash, &2);

    let ev = find_last_event::<DisputeResolved>(&t).expect("dispute_resolved event emitted");
    assert_eq!(ev.invoice_id, id);
    assert_eq!(ev.resolution, 2);
    assert_eq!(ev.resolution_hash, resolution_hash);
}

// ================================================================
// auto_resolve_dispute window — INV-DT7
// ================================================================

#[test]
fn test_auto_resolve_before_timeout_rejected() {
    let t = setup();
    let id = submit(&t);
    dispute(&t, id);

    let timeout = t.contract.get_config().dispute_timeout_ledgers as u32;
    // One ledger short of the window closing.
    advance_ledger(&t, timeout - 1);

    let res = t.contract.try_auto_resolve_dispute(&id);
    assert_eq!(res, Err(Ok(ContractError::Unauthorized)));
    assert_eq!(
        status_of(&t, id),
        InvoiceStatus::Disputed,
        "the invoice stays disputed until the window closes"
    );
}

/// The window closes inclusively: `now == disputed_at + timeout` is allowed
/// (the guard is a strict `<`).
#[test]
fn test_auto_resolve_at_timeout_boundary_succeeds() {
    let t = setup();
    let id = submit(&t);
    dispute(&t, id);

    let timeout = t.contract.get_config().dispute_timeout_ledgers as u32;
    advance_ledger(&t, timeout);

    t.contract.auto_resolve_dispute(&id);
    assert_eq!(status_of(&t, id), InvoiceStatus::Pending);
}

#[test]
fn test_auto_resolve_requires_disputed_state() {
    let t = setup();
    let id = submit(&t); // Pending
    let res = t.contract.try_auto_resolve_dispute(&id);
    assert_eq!(res, Err(Ok(ContractError::NotDisputed)));
}

#[test]
fn test_auto_resolve_missing_invoice_rejected() {
    let t = setup();
    let res = t.contract.try_auto_resolve_dispute(&999);
    assert_eq!(res, Err(Ok(ContractError::InvoiceNotFound)));
}

/// Auto-resolution always rules for the freelancer (code 2) and publishes an
/// empty resolution hash — the payer's evidence is discarded by design.
#[test]
fn test_auto_resolve_emits_rejected_resolution_with_empty_hash() {
    let t = setup();
    let id = submit(&t);
    fund(&t, id, INVOICE_AMOUNT);
    dispute(&t, id);

    let timeout = t.contract.get_config().dispute_timeout_ledgers as u32;
    advance_ledger(&t, timeout);
    t.contract.auto_resolve_dispute(&id);

    assert_eq!(status_of(&t, id), InvoiceStatus::Funded);
    let ev = find_last_event::<DisputeResolved>(&t).expect("dispute_resolved event emitted");
    assert_eq!(ev.resolution, 2, "auto-resolution always rules for the freelancer");
    assert_eq!(
        ev.resolution_hash,
        BytesN::from_array(&t.env, &[0u8; 32]),
        "auto-resolution publishes an empty resolution hash"
    );
}
