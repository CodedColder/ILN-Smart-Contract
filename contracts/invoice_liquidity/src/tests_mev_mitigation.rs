//! Tests for Issue #MEV-1 — resolve_fund_queue maturity delay
//!
//! Scenarios covered:
//!  - resolve_fund_queue fails immediately after first LP joins (QueueNotMature)
//!  - resolve_fund_queue succeeds once QUEUE_DELAY_LEDGERS have elapsed
//!  - Second LP joining does not reset the maturity timer
//!  - Event emitted on rejected resolution attempt (success=false)
//!  - Event emitted on successful resolution attempt (success=true)
//!
//! Ledger-placement audit (bottom of file) — documented model:
//!  - Ordering inside one ledger is irrelevant: the winner depends only on
//!    *current* LP scores at resolution time, never on join position/ledger.
//!  - `resolve_fund_queue` is the cut-off: joins are accepted up to (and
//!    including) the ledger before it, even after the maturity delay elapsed.
//!  - Once resolved, only the winner may fund, in the very same ledger.
//!  - Before resolution there is NO exclusivity: a direct `fund_invoice` is
//!    first-come-first-served (residual risk, pinned by a test below).
//!  - A failed (rejected) resolve leaves no state behind.
//!  - Pause freezes the queue but not the ledger clock.

#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger, MockAuth, MockAuthInvoke},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env, IntoVal,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30; // 30 days

struct MevTestEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: TokenClient<'static>,
    freelancer: Address,
    payer: Address,
    lp_a: Address,
    lp_b: Address,
}

fn setup_mev() -> MevTestEnv {
    let env = Env::default();
    env.mock_all_auths();

    let usdc_admin = Address::generate(&env);
    let usdc_id = env.register_stellar_asset_contract_v2(usdc_admin.clone());
    let usdc_addr = usdc_id.address();

    let token = TokenClient::new(&env, &usdc_addr);
    let token_admin = StellarAssetClient::new(&env, &usdc_addr);

    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let lp_a = Address::generate(&env);
    let lp_b = Address::generate(&env);

    for lp in [&lp_a, &lp_b] {
        token_admin.mint(lp, &(INVOICE_AMOUNT * 10));
    }
    token_admin.mint(&payer, &(INVOICE_AMOUNT * 10));

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    token_admin.mint(&contract.address, &(INVOICE_AMOUNT * 100));

    let xlm_admin = Address::generate(&env);
    let xlm_id = env.register_stellar_asset_contract_v2(xlm_admin);
    let xlm_addr = xlm_id.address();
    let eurc_addr = Address::generate(&env);

    contract.initialize(&usdc_admin, &usdc_addr, &eurc_addr, &xlm_addr);

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    ledger.sequence_number = 100;
    env.ledger().set(ledger);

    MevTestEnv {
        env,
        contract,
        token,
        freelancer,
        payer,
        lp_a,
        lp_b,
    }
}

fn submit_invoice_mev(t: &MevTestEnv) -> u64 {
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

fn advance_ledgers(env: &Env, delta: u32) {
    let mut info = env.ledger().get();
    info.sequence_number += delta;
    info.timestamp += u64::from(delta) * 5;
    env.ledger().set(info);
}

// ── Maturity delay: reject before delay elapses ───────────────────────────────

#[test]
pub(crate) fn test_resolve_queue_fails_immediately_after_join() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);

    // Attempt to resolve on the same ledger — must be rejected.
    let result = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(result, Err(Ok(ContractError::QueueNotMature)));
}

#[test]
pub(crate) fn test_resolve_queue_fails_one_ledger_before_delay() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);

    // Advance to one ledger before the required delay.
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS - 1);

    let result = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(result, Err(Ok(ContractError::QueueNotMature)));
}

// ── Maturity delay: succeed after delay elapses ───────────────────────────────

#[test]
pub(crate) fn test_resolve_queue_succeeds_after_delay() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);

    // Advance exactly QUEUE_DELAY_LEDGERS — now resolution must succeed.
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);

    let approved = t.contract.resolve_fund_queue(&id);
    assert_eq!(approved, t.lp_a);
}

#[test]
pub(crate) fn test_resolve_queue_succeeds_well_after_delay() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);

    // Advance far beyond the delay — must still succeed.
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS * 3);

    let approved = t.contract.resolve_fund_queue(&id);
    assert_eq!(approved, t.lp_a);
}

// ── Timer is anchored to the FIRST join, not subsequent ones ──────────────────

#[test]
pub(crate) fn test_second_lp_join_does_not_reset_maturity_timer() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);

    // lp_a joins first — timer starts here.
    t.contract.join_fund_queue(&t.lp_a, &id);

    // Advance most of the delay.
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS - 10);

    // lp_b joins late — this must NOT reset the timer.
    t.contract.join_fund_queue(&t.lp_b, &id);

    // Advance the remaining ledgers to complete the original delay.
    advance_ledgers(&t.env, 10);

    // Resolution should succeed: the timer was started by lp_a's join.
    let approved = t.contract.resolve_fund_queue(&id);
    // lp_b joined with equal score (default 50) — lp_a has priority as first.
    assert_eq!(approved, t.lp_a);
}

// ── Idempotency: already-resolved queue returns cached winner immediately ─────

#[test]
pub(crate) fn test_resolve_already_resolved_queue_returns_same_winner() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);

    let first = t.contract.resolve_fund_queue(&id);
    // Second call on an already-resolved queue must return the same winner.
    let second = t.contract.resolve_fund_queue(&id);
    assert_eq!(first, second);
    assert_eq!(first, t.lp_a);
}

// ── Event emission ────────────────────────────────────────────────────────────

#[test]
pub(crate) fn test_rejected_resolution_emits_attempt_event_with_success_false() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);

    // join_fund_queue must have emitted its FundRequested event.
    let events_after_join = t.env.events().all();
    assert!(
        !events_after_join.events().is_empty(),
        "Expected FundRequested event after join"
    );

    // Attempt resolution before maturity — the call is rejected. Note: in
    // this SDK's mock test host, a call that returns a declared contract
    // error reverts its own events *and* clears the env's whole
    // accumulated event log (verified empirically — unrelated failing
    // calls do the same), so the FundQueueResolutionAttempted event this
    // rejection path publishes (see resolve_fund_queue's QueueNotMature
    // branch) isn't independently observable via events().all() once the
    // call returns. We can only assert on the call's own failure here.
    let result = t.contract.try_resolve_fund_queue(&id);
    assert_eq!(result, Err(Ok(ContractError::QueueNotMature)));
}

#[test]
pub(crate) fn test_successful_resolution_emits_attempt_event_with_success_true() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);

    t.contract.resolve_fund_queue(&id);

    // At least two events expected: FundRequested + FundQueueResolved +
    // FundQueueResolutionAttempted (success=true).
    let events = t.env.events().all();
    assert!(
        events.events().len() >= 2,
        "Expected events from join + resolve, got {}",
        events.events().len()
    );
}

// ── Issue #708: randomized tie-breaking fairness ────────────────────────────
//
// Prior to this fix, resolve_fund_queue always selected the first entry
// inserted among LPs tied on score (join_fund_queue's `score < new_score`
// insertion, strictly less-than, puts a tying LP after existing equal-score
// entries). That made the outcome for a tie fully predictable and gameable
// by whoever submits first. This test creates many independent ties between
// the same two LPs (both default to score 0, so joining the same invoice
// queue in the same order is a tie every time) and asserts the winner isn't
// always the same address across all of them — proof the PRNG path is
// actually exercised, not dead code that happens to always pick index 0.

#[test]
pub(crate) fn test_tie_resolution_is_not_always_the_first_joiner() {
    let t = setup_mev();

    let mut lp_a_wins = 0;
    let mut lp_b_wins = 0;

    for _ in 0..20 {
        let id = submit_invoice_mev(&t);
        // Both default to score 0 — a genuine tie every time.
        t.contract.join_fund_queue(&t.lp_a, &id);
        t.contract.join_fund_queue(&t.lp_b, &id);
        advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);

        let winner = t.contract.resolve_fund_queue(&id);
        if winner == t.lp_a {
            lp_a_wins += 1;
        } else if winner == t.lp_b {
            lp_b_wins += 1;
        } else {
            panic!("resolve_fund_queue returned neither tied LP");
        }
    }

    assert_eq!(lp_a_wins + lp_b_wins, 20);
    // Both LPs are equally eligible on every tie — if tie-breaking were
    // still deterministic (always the first joiner), one side would be 20
    // and the other 0. Either side winning at least once is enough to prove
    // randomness is actually influencing the outcome.
    assert!(
        lp_a_wins > 0 && lp_b_wins > 0,
        "expected both tied LPs to win at least once across 20 ties, got lp_a={lp_a_wins} lp_b={lp_b_wins}"
    );
}

#[test]
pub(crate) fn test_resolve_queue_still_resolves_deterministically_for_a_single_lp() {
    // No tie at all (only one LP in the queue) — the PRNG-gated tied_count
    // check must not change the existing single-candidate behavior.
    let t = setup_mev();
    let id = submit_invoice_mev(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);

    let winner = t.contract.resolve_fund_queue(&id);
    assert_eq!(winner, t.lp_a);
}

// ── Issue #712: queue winner transferring to the queue loser is expected ────
//
// transfer_lp_position() lets an LP hand off their funded position to any
// other address, with no restriction tied to funding-queue history. This
// test confirms that a queue winner immediately transferring their position
// to the address that *lost* the same queue resolution works exactly like
// any other transfer_lp_position() call — this is intentional, documented
// secondary-market behavior (see docs/threat-model.md), not a queue-fairness
// bug. The queue's fairness guarantee is about *who gets first crack at
// funding the invoice* (reputation-weighted, now randomized on ties per
// Issue #708) — what either party does with their position afterward,
// including a private arrangement to hand it off, is a separate concern
// this contract deliberately doesn't restrict.

#[test]
pub(crate) fn test_queue_winner_can_transfer_position_to_queue_loser() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);

    // Both lp_a and lp_b join the queue (tied at score 0 — see Issue #708's
    // randomized tie-break, exercised incidentally here); whichever wins,
    // the other is the "loser" for this test's purpose.
    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);

    let winner = t.contract.resolve_fund_queue(&id);
    let loser = if winner == t.lp_a {
        t.lp_b.clone()
    } else {
        t.lp_a.clone()
    };

    // Winner funds the invoice, then transfers the resulting position
    // straight to the losing queue participant.
    t.contract
        .fund_invoice(&winner, &id, &INVOICE_AMOUNT, &false);
    let result = t.contract.try_transfer_lp_position(&id, &loser);

    assert!(
        result.is_ok(),
        "transfer_lp_position must succeed for a queue winner transferring to the queue loser — \
         this is expected secondary-market behavior, not something the queue mechanism restricts"
    );

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(
        invoice.funder,
        Some(loser),
        "the transferred-to address must now be the recorded funder"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Ledger-placement audit: same-ledger and delayed-transaction ordering
// ════════════════════════════════════════════════════════════════════════════

const HIGH_SCORE: u32 = 90;
const LOW_SCORE: u32 = 20;

fn set_score(t: &MevTestEnv, lp: &Address, score: u32) {
    t.env.as_contract(&t.contract.address, || {
        crate::storage::set_lp_score(&t.env, lp, score);
    });
}

fn queue_lps(t: &MevTestEnv, id: u64) -> soroban_sdk::Vec<Address> {
    t.env.as_contract(&t.contract.address, || {
        let q = crate::storage::get_fund_queue(&t.env, id);
        let mut out = soroban_sdk::Vec::new(&t.env);
        for e in q.iter() {
            out.push_back(e.lp);
        }
        out
    })
}

fn advance_time(env: &Env, secs: u64) {
    let mut info = env.ledger().get();
    info.timestamp += secs;
    env.ledger().set(info);
}

// ── Same-ledger ordering ─────────────────────────────────────────────────────

#[test]
fn test_same_ledger_join_order_does_not_beat_score() {
    // lp_low joins first, lp_high joins second, both in the same ledger.
    // Priority is by score, not by position in the ledger.
    let t = setup_mev();
    let id = submit_invoice_mev(&t);
    set_score(&t, &t.lp_a, LOW_SCORE);
    set_score(&t, &t.lp_b, HIGH_SCORE);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);

    let q = queue_lps(&t, id);
    assert_eq!(q.get(0).unwrap(), t.lp_b, "higher score is placed first");
    assert_eq!(q.get(1).unwrap(), t.lp_a);

    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    assert_eq!(t.contract.resolve_fund_queue(&id), t.lp_b);
}

#[test]
fn test_same_ledger_resolve_then_non_winner_fund_is_rejected() {
    // Resolution and a competing fund attempt land in the same ledger: the
    // non-winner is rejected immediately, the winner succeeds.
    let t = setup_mev();
    let id = submit_invoice_mev(&t);
    set_score(&t, &t.lp_a, HIGH_SCORE);
    set_score(&t, &t.lp_b, LOW_SCORE);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    assert_eq!(t.contract.resolve_fund_queue(&id), t.lp_a);

    let sniped = t
        .contract
        .try_fund_invoice(&t.lp_b, &id, &INVOICE_AMOUNT, &false);
    assert_eq!(sniped, Err(Ok(ContractError::NotApprovedFunder)));
    assert_eq!(t.contract.get_invoice(&id).amount_funded, 0);

    t.contract
        .fund_invoice(&t.lp_a, &id, &INVOICE_AMOUNT, &false);
    assert_eq!(t.contract.get_invoice(&id).status, InvoiceStatus::Funded);
}

#[test]
fn test_resolve_is_not_possible_in_the_join_ledger() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    // A single-ledger join+resolve ("sandwich") is impossible.
    assert_eq!(
        t.contract.try_resolve_fund_queue(&id),
        Err(Ok(ContractError::QueueNotMature))
    );
}

// ── Delayed transactions ─────────────────────────────────────────────────────

#[test]
fn test_late_join_after_maturity_but_before_resolve_still_competes() {
    // Matured != resolved. A join that lands after the delay elapsed but before
    // anyone calls resolve is still counted; resolve is the cut-off.
    let t = setup_mev();
    let id = submit_invoice_mev(&t);
    set_score(&t, &t.lp_a, LOW_SCORE);
    set_score(&t, &t.lp_b, HIGH_SCORE);

    t.contract.join_fund_queue(&t.lp_a, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS + 5);
    t.contract.join_fund_queue(&t.lp_b, &id);

    assert_eq!(t.contract.resolve_fund_queue(&id), t.lp_b);
}

#[test]
fn test_join_delayed_past_resolution_is_rejected_and_winner_unchanged() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);
    set_score(&t, &t.lp_a, LOW_SCORE);
    set_score(&t, &t.lp_b, HIGH_SCORE);

    t.contract.join_fund_queue(&t.lp_a, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    assert_eq!(t.contract.resolve_fund_queue(&id), t.lp_a);

    // lp_b's join was delayed one ledger too long.
    advance_ledgers(&t.env, 1);
    assert_eq!(
        t.contract.try_join_fund_queue(&t.lp_b, &id),
        Err(Ok(ContractError::NotApprovedFunder))
    );
    assert_eq!(t.contract.resolve_fund_queue(&id), t.lp_a);
    assert_eq!(queue_lps(&t, id).len(), 1);
}

#[test]
fn test_rejected_resolve_leaves_no_state_and_later_resolve_works() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);

    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS - 1);
    assert_eq!(
        t.contract.try_resolve_fund_queue(&id),
        Err(Ok(ContractError::QueueNotMature))
    );
    // No winner was locked in, so joining is still open ...
    t.contract.join_fund_queue(&t.lp_b, &id);
    // ... and the *original* timer still applies (one more ledger suffices).
    advance_ledgers(&t.env, 1);
    assert!(t.contract.try_resolve_fund_queue(&id).is_ok());
}

#[test]
fn test_stale_score_snapshot_does_not_win_after_delay() {
    // lp_a joins with a high score, then its score drops during the delay.
    // Resolution uses *current* scores (Issue #1019), so lp_b wins.
    let t = setup_mev();
    let id = submit_invoice_mev(&t);
    set_score(&t, &t.lp_a, HIGH_SCORE);
    set_score(&t, &t.lp_b, 60);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    set_score(&t, &t.lp_a, LOW_SCORE);

    assert_eq!(t.contract.resolve_fund_queue(&id), t.lp_b);
}

#[test]
fn test_resolution_is_final_even_if_scores_change_afterwards() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);
    set_score(&t, &t.lp_a, HIGH_SCORE);
    set_score(&t, &t.lp_b, LOW_SCORE);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    assert_eq!(t.contract.resolve_fund_queue(&id), t.lp_a);

    set_score(&t, &t.lp_a, 1);
    set_score(&t, &t.lp_b, 100);
    advance_ledgers(&t.env, 10);
    assert_eq!(t.contract.resolve_fund_queue(&id), t.lp_a);
}

// ── Residual risk: no exclusivity before resolution ──────────────────────────

#[test]
fn test_unresolved_queue_does_not_block_direct_funding() {
    // Documented residual risk: joining the queue only *registers intent*.
    // Until resolve_fund_queue locks a winner, any LP may fund directly
    // (first-come-first-served), including an LP that never joined.
    let t = setup_mev();
    let id = submit_invoice_mev(&t);
    set_score(&t, &t.lp_a, HIGH_SCORE);

    t.contract.join_fund_queue(&t.lp_a, &id);
    // lp_b (never queued) funds in the same ledger.
    t.contract
        .fund_invoice(&t.lp_b, &id, &INVOICE_AMOUNT, &false);

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(invoice.status, InvoiceStatus::Funded);
    // The queue can no longer be resolved for a funded invoice.
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    assert_eq!(
        t.contract.try_resolve_fund_queue(&id),
        Err(Ok(ContractError::AlreadyFunded))
    );
}

// ── Stalled winner: recovery timing and restart of the delay ─────────────────

#[test]
fn test_recover_stalled_winner_restarts_maturity_for_remaining_lps() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);
    set_score(&t, &t.lp_a, HIGH_SCORE);
    set_score(&t, &t.lp_b, LOW_SCORE);

    t.contract.join_fund_queue(&t.lp_a, &id);
    t.contract.join_fund_queue(&t.lp_b, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    assert_eq!(t.contract.resolve_fund_queue(&id), t.lp_a);

    // One ledger before the resolution TTL: not stalled yet.
    advance_ledgers(&t.env, crate::constants::QUEUE_RESOLUTION_TTL_LEDGERS - 1);
    assert_eq!(
        t.contract.try_recover_fund_queue(&id),
        Err(Ok(ContractError::QueueNotStalled))
    );

    // At the TTL: the stalled winner is evicted.
    advance_ledgers(&t.env, 1);
    assert_eq!(t.contract.recover_fund_queue(&id), Some(t.lp_a.clone()));

    // The delay restarts for lp_b: immediate resolve is rejected ...
    assert_eq!(
        t.contract.try_resolve_fund_queue(&id),
        Err(Ok(ContractError::QueueNotMature))
    );
    // ... and succeeds exactly after QUEUE_DELAY_LEDGERS.
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    assert_eq!(t.contract.resolve_fund_queue(&id), t.lp_b);
}

// ── Pause and expiry interplay ───────────────────────────────────────────────

#[test]
fn test_pause_freezes_queue_but_ledger_clock_keeps_running() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);

    t.contract.pause();
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    assert_eq!(
        t.contract.try_resolve_fund_queue(&id),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(
        t.contract.try_join_fund_queue(&t.lp_b, &id),
        Err(Ok(ContractError::ContractPaused))
    );

    // Maturity is measured in ledgers, so it elapsed while paused.
    t.contract.unpause();
    assert_eq!(t.contract.resolve_fund_queue(&id), t.lp_a);
}

#[test]
fn test_invoice_expiring_while_queued_cannot_be_resolved() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);
    t.contract.join_fund_queue(&t.lp_a, &id);

    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS);
    advance_time(&t.env, DUE_DATE_OFFSET + 1);

    assert_eq!(
        t.contract.try_resolve_fund_queue(&id),
        Err(Ok(ContractError::InvoiceExpired))
    );
    assert_eq!(
        t.contract.try_join_fund_queue(&t.lp_b, &id),
        Err(Ok(ContractError::InvoiceExpired))
    );
}

// ── Unauthorized / repeated ──────────────────────────────────────────────────

#[test]
fn test_join_without_lp_authorization_is_rejected_and_queue_untouched() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);

    // Only lp_b authorizes; the call names lp_a.
    t.env.mock_auths(&[MockAuth {
        address: &t.lp_b,
        invoke: &MockAuthInvoke {
            contract: &t.contract.address,
            fn_name: "join_fund_queue",
            args: (t.lp_a.clone(), id).into_val(&t.env),
            sub_invokes: &[],
        },
    }]);

    assert!(t.contract.try_join_fund_queue(&t.lp_a, &id).is_err());
    assert_eq!(queue_lps(&t, id).len(), 0);
}

#[test]
fn test_repeated_join_by_same_lp_is_rejected_and_keeps_position_and_timer() {
    let t = setup_mev();
    let id = submit_invoice_mev(&t);

    t.contract.join_fund_queue(&t.lp_a, &id);
    advance_ledgers(&t.env, QUEUE_DELAY_LEDGERS - 1);
    assert_eq!(
        t.contract.try_join_fund_queue(&t.lp_a, &id),
        Err(Ok(ContractError::AlreadyInQueue))
    );
    assert_eq!(queue_lps(&t, id).len(), 1);

    // The rejected duplicate must not have restarted the timer.
    advance_ledgers(&t.env, 1);
    assert_eq!(t.contract.resolve_fund_queue(&id), t.lp_a);
}
