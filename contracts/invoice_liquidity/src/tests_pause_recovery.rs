#![cfg(test)]

//! Issue #1184 — Audit pause / unpause recovery behavior.
//!
//! `pause()` / `unpause()` are the protocol's emergency stop / resume levers.
//! This module verifies the *recovery* properties around them, i.e. that a
//! halt is engaged consistently, that rejection while halted leaves no
//! residue that could block the resume, and that resuming fully restores
//! normal operation:
//!
//!   * authorization — only the admin (single-admin or multisig path) may
//!     halt or resume;
//!   * idempotency — neither lever is gated on the current pause state, so
//!     repeat calls succeed and re-emit events / re-stamp the paused-since
//!     marker;
//!   * fail-closed gating — every mutating entry point returns
//!     `ContractPaused` while halted, and read-only views stay available;
//!   * recovery — an operation rejected while halted succeeds after
//!     `unpause()`, i.e. a rejected call (which may set the reentrancy lock)
//!     never leaks state that prevents recovery — Soroban rolls the failed
//!     invocation back, so the "one-shot" lock is never stuck.
//!
//! Findings are catalogued in `docs/pause-recovery.md`; each test maps to an
//! invariant (INV-PR*) or verified finding (VF-PR*).
//!
//! Implementation-only audit: nothing here changes contract behavior.

use super::*;
use crate::errors::ContractError;
use crate::invoice::{InvoiceStatus, ReferralCode};
use soroban_sdk::{
    testutils::{Address as _, Ledger, MockAuth, MockAuthInvoke},
    token::{Client as TokenClient, StellarAssetClient},
    Address, BytesN, Env, IntoVal,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;
const SETUP_TIMESTAMP: u64 = 1_700_000_000;

struct PauseEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: TokenClient<'static>,
    token_admin: StellarAssetClient<'static>,
    admin: Address,
    freelancer: Address,
    payer: Address,
    lp_a: Address,
    lp_b: Address,
}

fn setup() -> PauseEnv {
    let env = Env::default();
    // Auth is exercised explicitly in the authorization tests below; every
    // other test relies on mocked auths to isolate pause/recovery semantics.
    env.mock_all_auths();

    let admin = Address::generate(&env);

    let usdc_id = env.register_stellar_asset_contract_v2(admin.clone());
    let usdc_addr = usdc_id.address();
    let eurc_id = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let eurc_addr = eurc_id.address();
    let xlm_id = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let xlm_addr = xlm_id.address();

    let token = TokenClient::new(&env, &usdc_addr);
    let token_admin = StellarAssetClient::new(&env, &usdc_addr);

    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let lp_a = Address::generate(&env);
    let lp_b = Address::generate(&env);

    token_admin.mint(&payer, &(INVOICE_AMOUNT * 100));
    token_admin.mint(&lp_a, &(INVOICE_AMOUNT * 100));
    token_admin.mint(&lp_b, &(INVOICE_AMOUNT * 100));

    let contract_id = env.register(InvoiceLiquidityContract, ());
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    // Fund the contract so funded paths can service refunds / payouts.
    token_admin.mint(&contract.address, &(INVOICE_AMOUNT * 1000));

    contract.initialize(&admin, &usdc_addr, &eurc_addr, &xlm_addr);

    let mut ledger = env.ledger().get();
    ledger.timestamp = SETUP_TIMESTAMP;
    ledger.sequence_number = 100;
    env.ledger().set(ledger);

    PauseEnv {
        env,
        contract,
        token,
        admin,
        freelancer,
        payer,
        lp_a,
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

fn submit(t: &PauseEnv) -> u64 {
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

fn fund_full(t: &PauseEnv, lp: &Address, id: u64) {
    t.contract.fund_invoice(lp, &id, &INVOICE_AMOUNT, &false);
}

fn status_of(t: &PauseEnv, id: u64) -> InvoiceStatus {
    t.contract.get_invoice(&id).status
}

fn set_time(t: &PauseEnv, ts: u64) {
    let mut ledger = t.env.ledger().get();
    ledger.timestamp = ts;
    t.env.ledger().set(ledger);
}

/// Count `paused` events in the buffered event stream (drains the buffer).
/// The `paused` symbol is a strict prefix-free match against `unpaused`
/// (the latter contains `paused`), so explicit exclusion is required.
fn count_paused_events(t: &PauseEnv) -> usize {
    t.env
        .events()
        .all()
        .events()
        .iter()
        .filter(|e| {
            let s = std::format!("{:?}", e);
            s.contains("paused") && !s.contains("unpaused")
        })
        .count()
}

/// Count `unpaused` events in the buffered event stream (drains the buffer).
fn count_unpaused_events(t: &PauseEnv) -> usize {
    t.env
        .events()
        .all()
        .events()
        .iter()
        .filter(|e| std::format!("{:?}", e).contains("unpaused"))
        .count()
}

// ================================================================
// Authorization — INV-PR1
// ================================================================

/// INV-PR1: a non-admin cannot engage the halt, and a rejected `pause` leaves
/// the flag untouched (state unchanged on failure).
#[test]
fn test_pause_requires_admin_and_leaves_state_unchanged() {
    let t = setup();
    let imposter = Address::generate(&t.env);

    t.env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &t.contract.address,
            fn_name: "pause",
            args: ().into_val(&t.env),
            sub_invokes: &[],
        },
    }]);

    let res = t.contract.try_pause();
    assert!(res.is_err(), "a non-admin pause must be rejected");
    assert!(
        !t.contract.is_paused(),
        "a rejected pause must not leave the contract halted"
    );
}

/// INV-PR1: recovering (unpausing) is admin-only — a non-admin cannot clear a
/// halt, and a rejected `unpause` leaves the contract halted.
#[test]
fn test_unpause_requires_admin_even_while_paused() {
    let t = setup();
    t.contract.pause();

    let imposter = Address::generate(&t.env);
    t.env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &t.contract.address,
            fn_name: "unpause",
            args: ().into_val(&t.env),
            sub_invokes: &[],
        },
    }]);

    let res = t.contract.try_unpause();
    assert!(res.is_err(), "a non-admin unpause must be rejected");
    assert!(
        t.contract.is_paused(),
        "a rejected unpause must not resurrect normal operation"
    );
}

/// INV-PR1: the configured admin, providing exactly the required auth, can
/// both halt and resume.
#[test]
fn test_admin_can_pause_and_unpause_with_exact_auth() {
    let t = setup();

    t.env.mock_auths(&[MockAuth {
        address: &t.admin,
        invoke: &MockAuthInvoke {
            contract: &t.contract.address,
            fn_name: "pause",
            args: ().into_val(&t.env),
            sub_invokes: &[],
        },
    }]);
    assert!(t.contract.try_pause().is_ok(), "admin pause must succeed");
    assert!(t.contract.is_paused());

    t.env.mock_auths(&[MockAuth {
        address: &t.admin,
        invoke: &MockAuthInvoke {
            contract: &t.contract.address,
            fn_name: "unpause",
            args: ().into_val(&t.env),
            sub_invokes: &[],
        },
    }]);
    assert!(t.contract.try_unpause().is_ok(), "admin unpause must succeed");
    assert!(!t.contract.is_paused());
}

// ================================================================
// Idempotency & event emission — INV-PR2 / INV-PR3 / VF-PR1 / VF-PR2
// ================================================================

/// VF-PR1 (INV-PR2): `pause()` is not gated on the current state — a repeat
/// halt while already paused succeeds and emits a second `paused` event.
#[test]
fn test_pause_is_idempotent_and_emits_each_call() {
    let t = setup();
    t.env.events().all(); // drain setup events

    t.contract.pause();
    t.contract.pause();

    assert_eq!(
        count_paused_events(&t),
        2,
        "no idempotency guard: every halt emits its own paused event"
    );
    assert!(t.contract.is_paused());
}

/// VF-PR2 (INV-PR2): `unpause()` on an already-running contract is accepted
/// (not gated on a paused state) and still emits an `unpaused` event.
#[test]
fn test_unpause_when_not_paused_is_accepted_noop() {
    let t = setup();
    assert!(!t.contract.is_paused());
    t.env.events().all(); // drain setup events

    let res = t.contract.try_unpause();
    assert!(
        res.is_ok(),
        "unpause is not gated on the paused state; a redundant resume is a no-op success"
    );
    assert!(!t.contract.is_paused());
    assert_eq!(
        count_unpaused_events(&t),
        1,
        "a redundant resume still emits an unpaused event"
    );
}

/// INV-PR3: both levers publish an event carrying the current ledger
/// timestamp, so an indexer can reconstruct incident timing.
#[test]
fn test_pause_and_unpause_emit_timestamped_events() {
    let t = setup();
    set_time(&t, SETUP_TIMESTAMP + 5_000);
    t.env.events().all(); // drain setup events

    t.contract.pause();
    let paused: std::vec::Vec<_> = t
        .env
        .events()
        .all()
        .events()
        .iter()
        .filter(|e| {
            let s = std::format!("{:?}", e);
            s.contains("paused") && !s.contains("unpaused")
        })
        .collect();
    assert_eq!(paused.len(), 1, "one paused event on halt");
    let needle = (SETUP_TIMESTAMP + 5_000).to_string();
    assert!(
        std::format!("{:?}", paused[0]).contains(needle.as_str()),
        "the paused event must carry the ledger timestamp"
    );

    set_time(&t, SETUP_TIMESTAMP + 9_000);
    t.contract.unpause();
    let unpaused: std::vec::Vec<_> = t
        .env
        .events()
        .all()
        .events()
        .iter()
        .filter(|e| std::format!("{:?}", e).contains("unpaused"))
        .collect();
    assert_eq!(unpaused.len(), 1, "one unpaused event on resume");
    let needle = (SETUP_TIMESTAMP + 9_000).to_string();
    assert!(
        std::format!("{:?}", unpaused[0]).contains(needle.as_str()),
        "the unpaused event must carry the ledger timestamp"
    );
}

/// INV-PR8 (rate-limit exemption): pause/unpause are emergency controls and
/// are deliberately exempt from rate limiting — back-to-back toggles all pass
/// with no ledger advance between them.
#[test]
fn test_pause_and_unpause_are_not_rate_limited() {
    let t = setup();
    assert!(t.contract.try_pause().is_ok());
    assert!(t.contract.try_unpause().is_ok());
    assert!(t.contract.try_pause().is_ok());
    assert!(t.contract.try_unpause().is_ok());
}

// ================================================================
// Fail-closed gating — INV-PR4 / INV-PR5
// ================================================================

/// INV-PR4: while halted every pause-gated mutating entry point fails closed
/// with `ContractPaused` — none partially executes.
#[test]
fn test_paused_gates_all_mutating_entry_points() {
    let t = setup();

    let pending = submit(&t);
    let funded = submit(&t);
    fund_full(&t, &t.lp_a, funded);

    t.contract.pause();
    assert!(t.contract.is_paused());

    let new_lp = Address::generate(&t.env);
    let due = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let hash = BytesN::from_array(&t.env, &[7u8; 32]);

    // Submission / configuration.
    assert_eq!(
        t.contract.try_submit_invoice(
            &t.freelancer,
            &t.payer,
            &INVOICE_AMOUNT,
            &due,
            &DISCOUNT_RATE,
            &t.token.address,
            &ReferralCode::None,
        ),
        Err(Ok(ContractError::ContractPaused))
    );

    // Funding & queue.
    assert_eq!(
        t.contract.try_fund_invoice(&t.lp_a, &pending, &INVOICE_AMOUNT, &false),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(
        t.contract.try_join_fund_queue(&t.lp_a, &pending),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(
        t.contract.try_resolve_fund_queue(&pending),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(
        t.contract.try_recover_fund_queue(&pending),
        Err(Ok(ContractError::ContractPaused))
    );

    // Lifecycle transitions.
    assert_eq!(
        t.contract.try_transfer_invoice(&pending, &new_lp),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(
        t.contract.try_cancel_invoice(&pending),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(
        t.contract.try_expire_invoice(&pending),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(
        t.contract.try_mark_paid(&funded, &INVOICE_AMOUNT),
        Err(Ok(ContractError::ContractPaused))
    );

    // Position / claims.
    assert_eq!(
        t.contract.try_transfer_lp_position(&funded, &new_lp),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(
        t.contract
            .try_claim_default(&t.lp_a, &funded),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(
        t.contract.try_appeal_default(&funded, &hash),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(
        t.contract.try_dispute_invoice(&funded, &hash),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(
        t.contract
            .try_claim_pending_payout(&t.lp_a, &t.token.address),
        Err(Ok(ContractError::ContractPaused))
    );
}

/// INV-PR5: read-only views are not pause-gated — operators and indexers can
/// still observe protocol state while halted.
#[test]
fn test_read_only_views_stay_available_while_paused() {
    let t = setup();
    let id = submit(&t);

    t.contract.pause();

    assert!(t.contract.is_paused());
    assert_eq!(t.contract.get_invoice(&id).status, InvoiceStatus::Pending);
    assert!(t.contract.get_protocol_status().paused);
    let _ = t.contract.get_invoice_count();
    let _ = t.contract.lp_score(&t.lp_a);
    let _ = t.contract.get_contract_stats();
    let _ = t.contract.get_version();
}

// ================================================================
// Recovery — INV-PR6 / INV-PR7 / INV-PR9 / VF-PR3
// ================================================================

/// INV-PR6 / INV-PR9: operations rejected while halted succeed after
/// `unpause()`. Repeated halted rejections (each acquiring and releasing the
/// reentrancy lock) leave no residue: if the "one-shot" lock leaked, the
/// second rejection would surface `Reentrancy` and the post-resume call would
/// fail. Soroban rolls the failed invocation back, so recovery is total.
#[test]
fn test_unpause_restores_operation_after_repeated_rejections() {
    let t = setup();
    let id = submit(&t);

    t.contract.pause();
    for _ in 0..3 {
        assert_eq!(
            t.contract.try_fund_invoice(&t.lp_a, &id, &INVOICE_AMOUNT, &false),
            Err(Ok(ContractError::ContractPaused)),
            "halted funding must fail closed, never with a stale lock error"
        );
        assert_eq!(
            t.contract.try_mark_paid(&id, &INVOICE_AMOUNT),
            Err(Ok(ContractError::ContractPaused)),
            "halted settlement must fail closed, never with a stale lock error"
        );
    }

    t.contract.unpause();
    fund_full(&t, &t.lp_a, id);
    assert_eq!(
        status_of(&t, id),
        InvoiceStatus::Funded,
        "funding resumes normally once the halt is lifted"
    );
}

/// VF-PR3 (INV-PR9): a rejected call that sets the reentrancy lock and then
/// returns an error *without* an explicit unlock (`fund_invoice` on a missing
/// invoice) does not leave the lock engaged — a second identical call reports
/// the same typed error rather than `Reentrancy`. This is the concrete
/// evidence that the one-shot lock cannot wedge normal operation.
#[test]
fn test_error_path_without_explicit_unlock_does_not_leak_lock() {
    let t = setup();
    let missing = 999_u64;

    assert_eq!(
        t.contract
            .try_fund_invoice(&t.lp_a, &missing, &INVOICE_AMOUNT, &false),
        Err(Ok(ContractError::InvoiceNotFound))
    );
    // If the lock persisted, this second call would return `Reentrancy`.
    assert_eq!(
        t.contract
            .try_fund_invoice(&t.lp_a, &missing, &INVOICE_AMOUNT, &false),
        Err(Ok(ContractError::InvoiceNotFound))
    );
}

/// INV-PR7: `claim_yield` is intentionally NOT pause-gated — an LP can still
/// query / realise yield while the protocol is halted (an exit path stays
/// open), even though every fund-moving transition is blocked.
#[test]
fn test_claim_yield_reads_through_while_paused() {
    let t = setup();
    let id = submit(&t);
    fund_full(&t, &t.lp_a, id); // Funded

    t.contract.pause();

    let yield_now = t.contract.claim_yield(&id);
    assert_eq!(yield_now, 0, "yield is zero while the invoice is still Funded");

    // Sanity: a fund-moving transition is blocked under the same halt.
    assert_eq!(
        t.contract.try_cancel_invoice(&id),
        Err(Ok(ContractError::ContractPaused))
    );
}

// ================================================================
// Paused-since marker — INV-PR10 / VF-PR1
// ================================================================

/// INV-PR10: `pause()` stamps the ledger timestamp; `unpause()` preserves it
/// as a "last halted at" reference (never cleared).
#[test]
fn test_last_pause_timestamp_stamped_and_preserved() {
    let t = setup();
    assert_eq!(
        t.contract.get_protocol_status().last_pause_timestamp,
        0,
        "no halt yet, so the marker is unset"
    );

    set_time(&t, SETUP_TIMESTAMP + 1_000);
    t.contract.pause();
    assert_eq!(
        t.contract.get_protocol_status().last_pause_timestamp,
        SETUP_TIMESTAMP + 1_000
    );

    t.contract.unpause();
    assert_eq!(
        t.contract.get_protocol_status().last_pause_timestamp,
        SETUP_TIMESTAMP + 1_000,
        "unpause keeps the last-halted marker"
    );
}

/// VF-PR1: a repeat `pause()` re-stamps `last_pause_timestamp` to the newer
/// ledger, moving the "paused since" marker forward (and emitting a fresh
/// event) even though the contract was already halted.
#[test]
fn test_second_pause_restamps_last_pause_timestamp() {
    let t = setup();

    set_time(&t, SETUP_TIMESTAMP + 1_000);
    t.contract.pause();
    let first = t.contract.get_protocol_status().last_pause_timestamp;

    set_time(&t, SETUP_TIMESTAMP + 9_000);
    t.contract.pause();
    let second = t.contract.get_protocol_status().last_pause_timestamp;

    assert_eq!(first, SETUP_TIMESTAMP + 1_000);
    assert_eq!(
        second,
        SETUP_TIMESTAMP + 9_000,
        "a redundant halt moves the paused-since marker forward"
    );
}

// ================================================================
// Multisig recovery path — INV-PR11
// ================================================================

/// INV-PR11: the multisig `execute_proposal` path drives the *same* pause
/// flag and enforces the *same* gate as the single-admin entry points, and a
/// multisig-approved unpause restores normal operation.
#[test]
fn test_multisig_pause_then_unpause_recovers() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let tok_id = env.register_stellar_asset_contract_v2(admin.clone());
    let tok = tok_id.address();

    let cid = env.register(InvoiceLiquidityContract, ());
    let client = InvoiceLiquidityContractClient::new(&env, &cid);
    client.initialize(&admin, &tok, &tok, &tok);

    let mut ledger = env.ledger().get();
    ledger.timestamp = SETUP_TIMESTAMP;
    env.ledger().set(ledger);

    let s1 = Address::generate(&env);
    let s2 = Address::generate(&env);
    let s3 = Address::generate(&env);
    let mut signers = soroban_sdk::Vec::new(&env);
    signers.push_back(s1.clone());
    signers.push_back(s2.clone());
    signers.push_back(s3.clone());
    client.initialize_multisig_admin(&signers, &2);

    // Halt via the multisig path.
    let pid = client.propose_pause(&s1);
    client.sign_proposal(&s1, &pid);
    client.sign_proposal(&s2, &pid);
    client.execute_proposal(&s1, &pid);
    assert!(client.is_paused(), "multisig pause engages the shared flag");

    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let due = env.ledger().timestamp() + DUE_DATE_OFFSET;
    assert_eq!(
        client.try_submit_invoice(
            &freelancer,
            &payer,
            &INVOICE_AMOUNT,
            &due,
            &DISCOUNT_RATE,
            &tok,
            &ReferralCode::None,
        ),
        Err(Ok(ContractError::ContractPaused)),
        "the shared halt gate blocks operations after a multisig pause"
    );

    // Recover via the multisig path.
    let uid = client.propose_unpause(&s1);
    client.sign_proposal(&s1, &uid);
    client.sign_proposal(&s2, &uid);
    client.execute_proposal(&s1, &uid);
    assert!(!client.is_paused(), "multisig unpause clears the flag");

    assert!(
        client
            .try_submit_invoice(
                &freelancer,
                &payer,
                &INVOICE_AMOUNT,
                &due,
                &DISCOUNT_RATE,
                &tok,
                &ReferralCode::None,
            )
            .is_ok(),
        "operations resume once the multisig lifts the halt"
    );
}
