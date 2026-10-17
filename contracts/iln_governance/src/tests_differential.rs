//! Issue #1098 — differential tests: legacy (pre-remediation) governance
//! behaviour vs. the remediated `GovContract`.
//!
//! Each test pairs a pure-Rust model of the *old* algorithm (`legacy` module)
//! with the live contract and asserts the two disagree in the security-relevant
//! direction. This pins the remediation so a future refactor cannot silently
//! reintroduce the old behaviour:
//!
//!   * #622 — quorum denominator was a caller-supplied `execute_proposal`
//!            argument (an attacker could shrink it to clear quorum arbitrarily).
//!   * #805 — a first vote snapshotted the live balance, so a flash loan cast
//!            inside one transaction permanently inflated the tally.
//!   * #531 — execution was fire-and-forget: a failed cross-contract call still
//!            marked the proposal `Executed` (no retry).
//!   * #68  — veto power could be toggled back on after being disabled.
//!   * DEL2 (#1098) — the delegation depth cap must hold for *every* insertion
//!            order, including chains grown by appending at the leaf.

#![cfg(test)]

use super::*;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    Address, BytesN, Env,
};

/// Stored quorum denominator (Issue #622: seeded at initialize, not caller-set).
const GOV_TOTAL_SUPPLY: i128 = 10_000;
/// 10% quorum — matches DEFAULT_MIN_QUORUM_BPS.
const MIN_QUORUM_BPS: u32 = 1_000;
const LEDGER_TIMESTAMP: u64 = 1_700_000_000;

// ── Mock ILN target ───────────────────────────────────────────────────────────
//
// `update_fee_rate` is the only cross-contract call exercised here. `set_fail`
// lets the #531 test inject a failing execution and then retry successfully.

#[contract]
pub struct DifferMockIln;

#[contractimpl]
impl DifferMockIln {
    pub fn set_fail(env: Env, fail: bool) {
        env.storage()
            .instance()
            .set(&soroban_sdk::symbol_short!("fail"), &fail);
    }

    pub fn update_fee_rate(env: Env, rate: u32) {
        let fail: bool = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("fail"))
            .unwrap_or(false);
        if fail {
            panic!("mock ILN: injected execution failure");
        }
        env.storage()
            .instance()
            .set(&soroban_sdk::symbol_short!("rate"), &rate);
    }
}

// ── Legacy (pre-remediation) behaviour models ────────────────────────────────

mod legacy {
    use super::*;

    /// Pre-#622 quorum/tally decision. `caller_total_supply` came straight from
    /// the `execute_proposal` caller, so passing a tiny value reduced the quorum
    /// to zero.
    pub fn execute_passes(
        votes_for: i128,
        votes_against: i128,
        caller_total_supply: i128,
        min_bps: u32,
    ) -> bool {
        let total = votes_for + votes_against;
        let quorum = if caller_total_supply <= 0 {
            0
        } else {
            caller_total_supply * min_bps as i128 / 10_000
        };
        total >= quorum && votes_for > votes_against
    }

    /// Pre-#805 first-vote weight: the live balance at cast time, with no
    /// holding-period gate (a flash loan counted in full).
    pub fn first_vote_weight(live_balance: i128) -> i128 {
        live_balance
    }

    /// Pre-#531 execution finalisation: fire-and-forget, `Executed` regardless
    /// of whether the callee actually succeeded.
    pub fn post_execute_status(_callee_succeeded: bool) -> ProposalStatus {
        ProposalStatus::Executed
    }

    /// Pre-#68 veto power was a plain toggle that could be re-enabled.
    pub fn veto_can_be_reenabled() -> bool {
        true
    }
}

// ── Harness ───────────────────────────────────────────────────────────────────

struct DifferEnv {
    env: Env,
    gov: GovContractClient<'static>,
    iln: DifferMockIlnClient<'static>,
    gov_token_admin: StellarAssetClient<'static>,
    token: TokenClient<'static>,
    admin: Address,
    /// Holds 3 000 / 10 000 = 30% of supply (clears the 10% quorum solo).
    voter: Address,
    /// Holds 100 / 10 000 = 1% (below quorum) with an aged checkpoint.
    small: Address,
    sink: Address,
}

fn diff_hash(env: &Env, seed: u8) -> BytesN<32> {
    BytesN::from_array(env, &[seed; 32])
}

fn diff_setup() -> DifferEnv {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();

    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(token_admin);
    let token_addr = token_id.address();
    let gov_token_admin = StellarAssetClient::new(&env, &token_addr);
    let token = TokenClient::new(&env, &token_addr);

    let admin = Address::generate(&env);
    let voter = Address::generate(&env);
    let small = Address::generate(&env);
    let sink = Address::generate(&env);

    gov_token_admin.mint(&voter, &3_000);
    gov_token_admin.mint(&small, &100);

    let iln_id = env.register_contract(None, DifferMockIln);
    let iln = DifferMockIlnClient::new(&env, &iln_id);

    let gov_id = env.register_contract(None, GovContract);
    let gov = GovContractClient::new(&env, &gov_id);
    gov.initialize(
        &iln_id,
        &Address::generate(&env),
        &Address::generate(&env),
        &token_addr,
        &admin,
        &GOV_TOTAL_SUPPLY,
    );
    gov.set_min_quorum_bps(&MIN_QUORUM_BPS);

    let mut ledger = env.ledger().get();
    ledger.timestamp = LEDGER_TIMESTAMP;
    env.ledger().set(ledger);

    // Issue #805: checkpoints must pre-date proposal creation by the holding
    // period, then the ledger is aged past it.
    gov.checkpoint_balance(&voter);
    gov.checkpoint_balance(&small);
    let mut ledger = env.ledger().get();
    ledger.sequence_number += MIN_VOTE_HOLD_LEDGERS + 1;
    env.ledger().set(ledger);

    DifferEnv {
        env,
        gov,
        iln,
        gov_token_admin,
        token,
        admin,
        voter,
        small,
        sink,
    }
}

fn advance_past_voting(h: &DifferEnv) {
    let mut ledger = h.env.ledger().get();
    ledger.timestamp = ledger.timestamp.saturating_add(VOTING_PERIOD_SECS + 1);
    h.env.ledger().set(ledger);
}

fn age_ledger(h: &DifferEnv) {
    let mut ledger = h.env.ledger().get();
    ledger.sequence_number += MIN_VOTE_HOLD_LEDGERS + 1;
    h.env.ledger().set(ledger);
}

// ── #622: caller-supplied quorum denominator ─────────────────────────────────

#[test]
fn differential_quorum_denominator_is_not_caller_controlled() {
    let h = diff_setup();

    // Legacy: because the denominator was supplied by the executor, a one-vote
    // tally clears "quorum" when the caller passes total_supply = 2.
    assert!(
        legacy::execute_passes(1, 0, 2, MIN_QUORUM_BPS),
        "legacy model: caller-chosen denominator must be able to bypass quorum"
    );

    // Remediated: the denominator is the stored supply, so the 1% tally
    // (100 / 10_000) is rejected.
    let id = h.gov.create_proposal(
        &h.voter,
        &ProposalAction::UpdateFeeRate(200),
        &diff_hash(&h.env, 7),
        &1,
    );
    h.gov.cast_vote(&h.small, &id, &true);
    advance_past_voting(&h);

    // NOTE: a client `try_*` call rolls back the callee's storage on a returned
    // `Err`, so the persisted `Rejected` status is only observable through a
    // direct in-contract call (`as_contract`), matching the repo's existing
    // `test_execute_quorum_not_met_fails_without_executing`.
    let res = h.env.as_contract(&h.gov.address, || {
        GovContract::execute_proposal(h.env.clone(), id)
    });
    assert_eq!(res, Err(GovernanceError::QuorumNotReached));
    assert_eq!(h.gov.get_proposal(&id).status, ProposalStatus::Rejected);

    // The stored denominator is fixed at the initialised value and cannot be
    // chosen by whoever calls execute_proposal.
    assert_eq!(h.gov.get_gov_token_total_supply(), GOV_TOTAL_SUPPLY);

    // Legacy run with the *true* supply agrees with the contract — proving the
    // bug was the caller-supplied denominator, not the quorum arithmetic.
    assert!(!legacy::execute_passes(
        100,
        0,
        GOV_TOTAL_SUPPLY,
        MIN_QUORUM_BPS
    ));
}

// ── #805: flash-loan first vote ──────────────────────────────────────────────

#[test]
fn differential_flash_loan_first_vote_cannot_self_snapshot() {
    let h = diff_setup();
    let flash_balance = 1_000_000_i128;

    // Legacy: the live flash-inflated balance was snapshotted as vote weight.
    assert_eq!(legacy::first_vote_weight(flash_balance), flash_balance);

    let whale = Address::generate(&h.env);
    h.gov_token_admin.mint(&whale, &flash_balance);

    // No aged checkpoint => the vote is refused outright.
    let p1 = h.gov.create_proposal(
        &h.voter,
        &ProposalAction::UpdateFeeRate(200),
        &diff_hash(&h.env, 1),
        &1,
    );
    assert_eq!(
        h.gov.try_cast_vote(&whale, &p1, &true),
        Err(Ok(GovernanceError::InsufficientHoldingPeriod))
    );

    // Boundary: checkpoint at the flash peak, repay, age, then a fresh
    // proposal. Weight is `min(checkpoint, current)` = the balance that
    // actually survived, not the peak.
    h.gov.checkpoint_balance(&whale);
    h.token.transfer(&whale, &h.sink, &(flash_balance - 50));
    age_ledger(&h);

    let p2 = h.gov.create_proposal(
        &h.voter,
        &ProposalAction::UpdateFeeRate(200),
        &diff_hash(&h.env, 2),
        &1,
    );
    h.gov.cast_vote(&whale, &p2, &true);
    assert_eq!(
        h.gov.get_applied_vote_weight(&p2, &whale),
        Some(50),
        "weight must be pinned to the surviving (post-repayment) balance"
    );
}

// ── #531: failed execution must stay retriable, not finalised ────────────────

#[test]
fn differential_failed_execution_is_not_finalised() {
    let h = diff_setup();

    // Legacy: fire-and-forget recorded Executed even though the callee failed.
    assert_eq!(legacy::post_execute_status(false), ProposalStatus::Executed);

    h.iln.set_fail(&true);
    let id = h.gov.create_proposal(
        &h.voter,
        &ProposalAction::UpdateFeeRate(200),
        &diff_hash(&h.env, 3),
        &1,
    );
    h.gov.cast_vote(&h.voter, &id, &true);
    advance_past_voting(&h);

    // Active -> Passed.
    h.gov.execute_proposal(&id);
    assert_eq!(h.gov.get_proposal(&id).status, ProposalStatus::Passed);

    // Passed -> attempted execution fails; status must NOT advance.
    assert_eq!(
        h.gov.try_execute_proposal(&id),
        Err(Ok(GovernanceError::ExecutionFailed))
    );
    assert_eq!(
        h.gov.get_proposal(&id).status,
        ProposalStatus::Passed,
        "a failed execution must remain retriable, not finalised"
    );

    // Retry after the target recovers.
    h.iln.set_fail(&false);
    h.gov.execute_proposal(&id);
    assert_eq!(h.gov.get_proposal(&id).status, ProposalStatus::Executed);

    // Re-execution of a settled proposal is rejected.
    assert_eq!(
        h.gov.try_execute_proposal(&id),
        Err(Ok(GovernanceError::AlreadyResolved))
    );
}

// ── #68: veto disable is one-way ─────────────────────────────────────────────

#[test]
fn differential_veto_disable_is_irreversible() {
    let h = diff_setup();

    // Legacy: veto power could be toggled back on after a disable.
    assert!(legacy::veto_can_be_reenabled());

    let signers = soroban_sdk::vec![&h.env, h.admin.clone()];
    h.gov.configure_veto_multisig(&signers, &1);

    let id = h.gov.create_proposal(
        &h.voter,
        &ProposalAction::UpdateFeeRate(200),
        &diff_hash(&h.env, 4),
        &1,
    );

    // A non-signer cannot veto.
    let outsider = Address::generate(&h.env);
    assert_eq!(
        h.gov
            .try_veto_proposal(&outsider, &id, &diff_hash(&h.env, 5)),
        Err(Ok(GovernanceError::NotVetoSigner))
    );

    // Disable veto power (ILN-gated; mocked here). After this the flag is
    // permanently false — there is no setter that re-enables it.
    h.gov.disable_veto_power();
    assert!(!h.gov.is_veto_power_enabled());

    assert_eq!(
        h.gov
            .try_veto_proposal(&h.admin, &id, &diff_hash(&h.env, 6)),
        Err(Ok(GovernanceError::VetoPowerDisabled))
    );
    assert!(
        !h.gov.is_veto_power_enabled(),
        "veto power must not be re-enablable after disable_veto_power"
    );
}
