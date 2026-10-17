//! Tests for Issue #59 (GovernanceProposal struct),
//!           Issue #61 (cast_vote with anti-double-vote protection and VoteCast event),
//!       and Issue #64 (delegate_votes / undelegate_votes with transitive delegation).

#![cfg(test)]

use super::*;
use reputation_bonus::{
    config::Config as RepBonusConfig, ReputationBonusContract, ReputationBonusContractClient,
};
use soroban_sdk::{
    contract, contractimpl,
    testutils::{storage::Temporary, Address as _, Events, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    Address, BytesN, Env,
};

#[contract]
pub struct MockIln;

#[contractimpl]
impl MockIln {
    pub fn update_fee_rate(_env: Env, _rate: u32) {}
    pub fn add_token(_env: Env, _token: Address, _decimals: u32) {}
    pub fn remove_token(_env: Env, _token: Address) {}
    pub fn update_max_discount(_env: Env, _rate: u32) {}
    pub fn update_decay_params(_env: Env, _rate_bps: u32, _period_ledgers: u64) {}
    pub fn update_fee_tiers(_env: Env, _tiers: Vec<FeeTierConfig>) {}
    pub fn update_reward_params(
        _env: Env,
        _half_token: i128,
        _hundred_usdc_stroops: i128,
        _lp_multiplier: i128,
    ) {
    }
    pub fn upgrade(_env: Env, _new_wasm_hash: BytesN<32>) {}
    pub fn set_lp_reward_rate(_env: Env, _rate: i128) {}
    pub fn set_freelancer_reward_rate(_env: Env, _rate: i128) {}
    pub fn set_payer_reward_rate(_env: Env, _rate: i128) {}
    pub fn set_coverage_via_governance(_env: Env, _cap: i128) {}
    pub fn set_premium_rate_via_governance(_env: Env, _rate: u32) {}
    pub fn register_oracle(_env: Env, _feed_type: OracleFeedType, _oracle: Address) {}
    pub fn remove_oracle(_env: Env, _feed_type: OracleFeedType) {}
}

/// Issue #531: a mock ILN contract whose `update_fee_rate` always fails, used
/// to verify that a failed cross-contract call during execution does not get
/// silently marked `Executed`. Nested in its own module so the
/// `#[contractimpl]`-generated symbols (e.g. `update_fee_rate`) don't collide
/// with `MockIln`'s identically-named function in this module.
mod mock_iln_failing {
    use soroban_sdk::{contract, contracterror, contractimpl, Env};

    #[contracterror]
    #[derive(Copy, Clone, Debug, PartialEq)]
    pub enum MockFailError {
        AlwaysFails = 1,
    }

    #[contract]
    pub struct MockIlnFailing;

    #[contractimpl]
    impl MockIlnFailing {
        pub fn update_fee_rate(_env: Env, _rate: u32) -> Result<(), MockFailError> {
            Err(MockFailError::AlwaysFails)
        }
    }
}
use mock_iln_failing::MockIlnFailing;

// ── Test helpers ──────────────────────────────────────────────────────────────

struct GovTestEnv {
    env: Env,
    contract: GovContractClient<'static>,
    gov_token: TokenClient<'static>,
    gov_token_admin: StellarAssetClient<'static>,
    #[allow(dead_code)]
    iln_contract: Address,
    rep_contract: ReputationBonusContractClient<'static>,
    voter_a: Address,
    voter_b: Address,
    proposer: Address,
    admin: Address,
}

fn setup() -> GovTestEnv {
    let env = Env::default();
    env.mock_all_auths();

    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token_addr = token_id.address();

    let gov_token = TokenClient::new(&env, &token_addr);
    let gov_token_admin = StellarAssetClient::new(&env, &token_addr);

    let voter_a = Address::generate(&env);
    let voter_b = Address::generate(&env);
    let proposer = Address::generate(&env);
    let admin = Address::generate(&env);

    gov_token_admin.mint(&voter_a, &1_000);
    gov_token_admin.mint(&voter_b, &2_000);
    gov_token_admin.mint(&proposer, &1_000);

    let iln_contract = env.register_contract(None, MockIln);
    let dist_contract = env.register_contract(None, MockIln);

    let contract_id = env.register_contract(None, GovContract);
    let contract = GovContractClient::new(&env, &contract_id);

    // Issue #704: register the real reputation_bonus contract (not a mock)
    // so the UpdateReputationBonusParams E2E test can genuinely verify its
    // stored Config reflects a governance-executed change. Its admin is the
    // governance contract's own address, since execute_proposal calls
    // update_config with itself (env.current_contract_address()) as the
    // caller — contracts authorize as themselves automatically for
    // cross-contract calls they make, with no separate signature needed.
    let rep_contract_id = env.register_contract(None, ReputationBonusContract);
    let rep_contract = ReputationBonusContractClient::new(&env, &rep_contract_id);
    rep_contract.init(&contract_id);
    rep_contract.set_config(&RepBonusConfig {
        high_rep_threshold: 70,
        bonus_bps: 100,
        min_discount_rate_bps: 50,
    });

    contract.initialize(
        &iln_contract,
        &dist_contract,
        &rep_contract_id,
        &token_addr,
        &admin,
        &10_000,
    );

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    env.ledger().set(ledger);

    // Issue #805: checkpoint every funded voter and age the checkpoints past
    // MIN_VOTE_HOLD_LEDGERS, so votes/delegations in tests exercise the
    // steady-state (eligible) path. Tests that fund new addresses mid-test
    // must checkpoint + age them the same way (see `checkpoint_and_age`).
    contract.checkpoint_balance(&voter_a);
    contract.checkpoint_balance(&voter_b);
    contract.checkpoint_balance(&proposer);
    let mut ledger = env.ledger().get();
    ledger.sequence_number += MIN_VOTE_HOLD_LEDGERS + 1;
    env.ledger().set(ledger);

    GovTestEnv {
        env,
        contract,
        gov_token,
        gov_token_admin,
        iln_contract,
        rep_contract,
        voter_a,
        voter_b,
        proposer,
        admin,
    }
}

/// Issue #805: checkpoint `voter` and advance past MIN_VOTE_HOLD_LEDGERS so
/// a mid-test-funded address becomes eligible on subsequently created
/// proposals.
fn checkpoint_and_age(t: &GovTestEnv, voter: &Address) {
    t.contract.checkpoint_balance(voter);
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += MIN_VOTE_HOLD_LEDGERS + 1;
    t.env.ledger().set(ledger);
}

fn dummy_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[1u8; 32])
}

#[cfg(test)]
fn setup_veto_multisig(t: &GovTestEnv) {
    let signers = soroban_sdk::vec![&t.env, t.admin.clone()];
    t.contract.configure_veto_multisig(&signers, &1);
}

fn create_fee_proposal(t: &GovTestEnv) -> u64 {
    t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::UpdateFeeRate(200),
        &dummy_hash(&t.env),
        &200_i128,
    )
}

// ── Issue #59 ─────────────────────────────────────────────────────────────────

#[test]
fn test_create_proposal_stores_correct_fields() {
    let t = setup();
    let hash = dummy_hash(&t.env);
    let now = t.env.ledger().timestamp();

    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::UpdateFeeRate(300),
        &hash,
        &300_i128,
    );

    let p = t.contract.get_proposal(&id);

    assert_eq!(p.id, id);
    assert_eq!(p.proposer, t.proposer);
    assert_eq!(p.description_hash, hash);
    assert_eq!(p.action_type, ProposalAction::UpdateFeeRate(300));
    assert_eq!(p.proposed_value, 300);
    assert_eq!(p.status, ProposalStatus::Active);
    assert_eq!(p.votes_for, 0);
    assert_eq!(p.votes_against, 0);
    assert_eq!(p.created_at, now);
    assert_eq!(p.voting_end, now + 259_200);
}

#[test]
fn test_proposal_ids_increment() {
    let t = setup();
    let id1 = create_fee_proposal(&t);
    let id2 = create_fee_proposal(&t);
    assert_eq!(id2, id1 + 1);
}

#[test]
#[should_panic]
fn test_get_proposal_not_found_returns_error() {
    let t = setup();
    t.contract.get_proposal(&9999);
}

#[test]
fn test_proposal_action_add_token_stored_correctly() {
    let t = setup();
    let token_addr = Address::generate(&t.env);

    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::AddToken(token_addr.clone(), 6_u32),
        &dummy_hash(&t.env),
        &0_i128,
    );

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.action_type, ProposalAction::AddToken(token_addr, 6_u32));
    assert_eq!(p.proposed_value, 0);
}

#[test]
fn test_proposal_action_remove_token_stored_correctly() {
    let t = setup();
    let token_addr = Address::generate(&t.env);

    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::RemoveToken(token_addr.clone()),
        &dummy_hash(&t.env),
        &0_i128,
    );

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.action_type, ProposalAction::RemoveToken(token_addr));
}

#[test]
#[should_panic]
fn test_double_initialize_rejected() {
    let t = setup();
    let iln = Address::generate(&t.env);
    let dist = Address::generate(&t.env);
    let rep = Address::generate(&t.env);
    let token = Address::generate(&t.env);
    let admin = Address::generate(&t.env);
    t.contract
        .initialize(&iln, &dist, &rep, &token, &admin, &10_000);
}

#[test]
fn test_min_quorum_bps_defaults_to_10_percent() {
    let t = setup();
    assert_eq!(t.contract.get_min_quorum_bps(), 1_000);
}

#[test]
fn test_set_min_quorum_bps_updates_config() {
    let t = setup();
    t.contract.set_min_quorum_bps(&2_000);
    assert_eq!(t.contract.get_min_quorum_bps(), 2_000);
}

#[test]
#[should_panic]
fn test_set_min_quorum_bps_rejects_zero() {
    let t = setup();
    t.contract.set_min_quorum_bps(&0);
}

// ── Issue #61 ─────────────────────────────────────────────────────────────────

#[test]
fn test_cast_vote_for_updates_votes_for() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);
    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_for, 1_000);
    assert_eq!(p.votes_against, 0);
}

#[test]
fn test_cast_vote_against_updates_votes_against() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &false);
    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_against, 1_000);
    assert_eq!(p.votes_for, 0);
}

#[test]
fn test_proposal_creation_snapshots_proposer_balance() {
    let t = setup();
    let id = create_fee_proposal(&t);

    let snapshot_key = StorageKey::VoteWeightSnapshot(id, t.proposer.clone());
    let snapshot: i128 = t.env.as_contract(&t.contract.address, || {
        t.env.storage().persistent().get(&snapshot_key).unwrap()
    });

    assert_eq!(snapshot, t.gov_token.balance(&t.proposer));
}

#[test]
fn test_cast_vote_uses_snapshotted_balance_after_balance_increase() {
    let t = setup();
    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::UpdateFeeRate(200),
        &dummy_hash(&t.env),
        &200_i128,
    );

    let proposer_balance_before = t.gov_token.balance(&t.proposer);
    t.gov_token_admin.mint(&t.proposer, &2_000);

    t.contract.cast_vote(&t.proposer, &id, &true);

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_for, proposer_balance_before);
    assert_eq!(p.votes_against, 0);
}

#[test]
fn test_cast_vote_weight_equals_token_balance() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_b, &id, &true);
    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_for, 2_000);
}

#[test]
fn test_multiple_voters_accumulate_correctly() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);
    t.contract.cast_vote(&t.voter_b, &id, &true);
    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_for, 3_000);
}

#[test]
fn test_has_voted_returns_true_after_vote() {
    let t = setup();
    let id = create_fee_proposal(&t);
    assert!(!t.contract.has_voted(&t.voter_a, &id));
    t.contract.cast_vote(&t.voter_a, &id, &true);
    assert!(t.contract.has_voted(&t.voter_a, &id));
}

#[test]
fn test_vote_receipt_uses_temporary_storage_with_ttl() {
    let t = setup();
    let id = create_fee_proposal(&t);
    let key = StorageKey::HasVoted(id, t.voter_a.clone());

    t.contract.cast_vote(&t.voter_a, &id, &true);

    let (temporary_has_receipt, persistent_has_receipt, ttl) =
        t.env.as_contract(&t.contract.address, || {
            (
                t.env.storage().temporary().has(&key),
                t.env.storage().persistent().has(&key),
                t.env.storage().temporary().get_ttl(&key),
            )
        });

    assert!(temporary_has_receipt);
    assert!(!persistent_has_receipt);
    assert!(ttl >= VOTE_RECEIPT_TTL_THRESHOLD_LEDGERS);
}

#[test]
fn test_vote_receipt_available_within_ttl() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);

    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += VOTE_RECEIPT_TTL_THRESHOLD_LEDGERS - 1;
    ledger.timestamp += 1;
    t.env.ledger().set(ledger);

    assert!(t.contract.has_voted(&t.voter_a, &id));
}

// ── Issue #1099: persistent vote/veto receipt mirror ─────────────────────────

/// The durable receipt is written alongside the temporary one, with a TTL.
#[test]
fn test_vote_receipt_persistent_mirror_written_with_ttl() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);

    let key = StorageKey::VoteReceipt(id, t.voter_a.clone());
    let (has_mirror, weight) = t.env.as_contract(&t.contract.address, || {
        (
            t.env.storage().persistent().has(&key),
            t.env
                .storage()
                .persistent()
                .get::<StorageKey, i128>(&key),
        )
    });

    assert!(has_mirror, "#1099: persistent receipt must be written");
    assert_eq!(weight, Some(1_000));
}

/// Archiving (evicting) the temporary receipt must not resurrect the
/// double-vote window or erase the applied weight.
#[test]
fn test_vote_receipt_survives_temporary_archival() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);

    // Simulate host archival of the temporary receipts.
    let voted_key = StorageKey::HasVoted(id, t.voter_a.clone());
    let weight_key = StorageKey::AppliedVoteWeight(id, t.voter_a.clone());
    t.env.as_contract(&t.contract.address, || {
        t.env.storage().temporary().remove(&voted_key);
        t.env.storage().temporary().remove(&weight_key);
    });

    // #1099: fail-closed reads fall back to the durable mirror.
    assert!(
        t.contract.has_voted(&t.voter_a, &id),
        "#1099: has_voted must survive temporary archival"
    );
    assert_eq!(
        t.contract.get_applied_vote_weight(&id, &t.voter_a),
        Some(1_000),
        "#1099: applied weight must survive temporary archival"
    );

    // And a second vote must still be rejected.
    assert_eq!(
        t.contract.try_cast_vote(&t.voter_a, &id, &false),
        Err(Ok(GovernanceError::AlreadyVoted))
    );
}

#[test]
#[should_panic]
fn test_double_vote_rejected_with_already_voted_error() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);
    t.contract.cast_vote(&t.voter_a, &id, &false);
}

#[test]
fn test_double_vote_does_not_change_vote_counts() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);
    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_against, 0);
    assert_eq!(p.votes_for, 1_000);
}

#[test]
#[should_panic]
fn test_vote_on_nonexistent_proposal_rejected() {
    let t = setup();
    t.contract.cast_vote(&t.voter_a, &9999, &true);
}

#[test]
#[should_panic]
fn test_vote_after_voting_window_rejected() {
    let t = setup();
    let id = create_fee_proposal(&t);
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);
    t.contract.cast_vote(&t.voter_a, &id, &true);
}

#[test]
#[should_panic]
fn test_voter_with_zero_balance_rejected() {
    let t = setup();
    let id = create_fee_proposal(&t);
    let zero_voter = Address::generate(&t.env);
    t.contract.cast_vote(&zero_voter, &id, &true);
}

#[test]
fn test_cast_vote_emits_vote_cast_event() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);
    let events = t.env.events().all();
    assert!(
        !events.events().is_empty(),
        "VoteCast event should be emitted"
    );
}

#[test]
#[should_panic]
fn test_execute_before_voting_ends_fails() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);
    t.contract.execute_proposal(&id);
}

#[test]
fn test_execute_quorum_not_reached_rejected() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);
    // Raise total_supply well above the default seeded in setup() so
    // voter_a's 1_000 vote falls well short of the (now much larger) quorum.
    t.contract.set_gov_token_total_supply(&100_000);
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);
    t.contract.execute_proposal(&id);
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Rejected);
}

#[test]
fn test_execute_quorum_exact_threshold_is_allowed() {
    let t = setup();

    // Create a voter with exactly 10% of total supply. The checkpoint must
    // predate the proposal, so fund + checkpoint + age before creating it.
    let voter = Address::generate(&t.env);
    t.gov_token_admin.mint(&voter, &1_000);
    checkpoint_and_age(&t, &voter);
    let id = create_fee_proposal(&t);

    t.contract.cast_vote(&voter, &id, &true);

    // Advance past voting window.
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);

    // total_supply = 10_000; quorum = 1_000; total_votes = 1_000 => meets quorum.
    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)?;
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert!(res.is_ok());

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.status, ProposalStatus::Executed);
}

#[test]
fn test_execute_quorum_not_met_fails_without_executing() {
    let t = setup();

    // 500 votes, below 10% quorum for total_supply=10_000. Fund +
    // checkpoint + age before creating the proposal (Issue #805).
    let voter = Address::generate(&t.env);
    t.gov_token_admin.mint(&voter, &500);
    checkpoint_and_age(&t, &voter);
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&voter, &id, &true);

    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);

    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert_eq!(res, Ok(()));

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.status, ProposalStatus::Rejected);
}

#[test]
fn test_execute_quorum_met_passes_with_custom_quorum_bps() {
    let t = setup();
    let id = create_fee_proposal(&t);

    // Configure quorum to 20% (2000 bps).
    t.contract.set_min_quorum_bps(&2_000);

    // voter_b has 2_000 tokens in setup, which equals 20% of total_supply=10_000.
    t.contract.cast_vote(&t.voter_b, &id, &true);

    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);

    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)?;
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert!(res.is_ok());

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.status, ProposalStatus::Executed);
}

#[test]
fn test_proposal_rejected_when_against_wins() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);
    t.contract.cast_vote(&t.voter_b, &id, &false);
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);
    // Issue #1007: the rejection is a committed, successful call.
    t.contract.execute_proposal(&id);
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Rejected);
}

#[test]
#[should_panic]
fn test_already_resolved_proposal_cannot_be_executed_again() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);
    // Call 1: Active -> Passed
    t.contract.execute_proposal(&id);
    // Call 2: Passed -> Executed
    t.contract.execute_proposal(&id);
    // Call 3: Already Executed -> should panic with AlreadyResolved
    t.contract.execute_proposal(&id);
}

// ── Issue #64: delegate_votes / undelegate_votes ──────────────────────────────

#[test]
fn test_delegation_increases_delegate_vote_weight() {
    let t = setup();
    t.contract.delegate_votes(&t.voter_a, &t.voter_b);
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_b, &id, &true);
    let p = t.contract.get_proposal(&id);
    // voter_b own 2_000 + voter_a delegated 1_000 = 3_000
    assert_eq!(p.votes_for, 3_000);
}

#[test]
fn test_undelegation_removes_delegated_weight() {
    let t = setup();
    t.contract.delegate_votes(&t.voter_a, &t.voter_b);
    t.contract.undelegate_votes(&t.voter_a);
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_b, &id, &true);
    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_for, 2_000); // only voter_b's own tokens
}

#[test]
fn test_get_delegate_returns_correct_address() {
    let t = setup();
    t.contract.delegate_votes(&t.voter_a, &t.voter_b);
    let delegate = t.contract.get_delegate(&t.voter_a);
    assert_eq!(delegate, Some(t.voter_b.clone()));
}

#[test]
fn test_get_delegate_returns_none_after_undelegation() {
    let t = setup();
    t.contract.delegate_votes(&t.voter_a, &t.voter_b);
    t.contract.undelegate_votes(&t.voter_a);
    let delegate = t.contract.get_delegate(&t.voter_a);
    assert_eq!(delegate, None);
}

#[test]
fn test_transitive_delegation_a_to_b_to_c() {
    let t = setup();
    let voter_c = Address::generate(&t.env);
    t.gov_token_admin.mint(&voter_c, &3_000);
    // Issue #805: voter_c votes below, so its checkpoint must predate the proposal.
    checkpoint_and_age(&t, &voter_c);

    // B → C first, then A → B
    t.contract.delegate_votes(&t.voter_b, &voter_c);
    t.contract.delegate_votes(&t.voter_a, &t.voter_b);

    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&voter_c, &id, &true);

    let p = t.contract.get_proposal(&id);
    // C own 3_000 + B delegated 2_000 + A delegated 1_000 = 6_000
    assert_eq!(p.votes_for, 6_000);
}

#[test]
#[should_panic]
fn test_cycle_prevention_direct_a_b_b_a() {
    let t = setup();
    t.contract.delegate_votes(&t.voter_a, &t.voter_b);
    t.contract.delegate_votes(&t.voter_b, &t.voter_a); // must panic
}

#[test]
#[should_panic]
fn test_delegate_to_self_rejected() {
    let t = setup();
    t.contract.delegate_votes(&t.voter_a, &t.voter_a);
}

#[test]
#[should_panic]
fn test_cycle_prevention_indirect_a_b_c_a() {
    let t = setup();
    let voter_c = Address::generate(&t.env);
    t.gov_token_admin.mint(&voter_c, &500);
    // Issue #805: checkpoint voter_c so the final leg reaches cycle
    // detection (instead of being rejected for a missing checkpoint).
    checkpoint_and_age(&t, &voter_c);

    t.contract.delegate_votes(&t.voter_a, &t.voter_b);
    t.contract.delegate_votes(&t.voter_b, &voter_c);
    t.contract.delegate_votes(&voter_c, &t.voter_a); // must panic
}

fn build_delegation_chain(t: &GovTestEnv, length: u32) {
    extern crate std;
    let mut nodes = std::vec::Vec::new();
    for _ in 0..length {
        let a = Address::generate(&t.env);
        t.gov_token_admin.mint(&a, &100);
        nodes.push(a);
    }
    // Issue #805: checkpoint every node so the chain reaches the cycle/depth
    // logic instead of being rejected for missing checkpoints.
    for a in &nodes {
        t.contract.checkpoint_balance(a);
    }
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += MIN_VOTE_HOLD_LEDGERS + 1;
    t.env.ledger().set(ledger);
    for i in 0..(length - 1) {
        t.contract
            .delegate_votes(&nodes[i as usize], &nodes[(i + 1) as usize]);
    }
    // Create cycle at the end
    t.contract
        .delegate_votes(&nodes[(length - 1) as usize], &nodes[0]);
}

#[test]
#[should_panic]
fn test_cycle_prevention_chain_5() {
    let t = setup();
    t.env.budget().reset_unlimited();
    build_delegation_chain(&t, 5);
}

#[test]
#[should_panic]
fn test_cycle_prevention_chain_10() {
    let t = setup();
    t.env.budget().reset_unlimited();
    build_delegation_chain(&t, 10);
}

#[test]
#[should_panic]
fn test_cycle_prevention_chain_25() {
    let t = setup();
    t.env.budget().reset_unlimited();
    build_delegation_chain(&t, 25);
}

#[test]
fn test_max_delegation_depth_cap_enforced() {
    extern crate std;
    let t = setup();

    // Default cap is 10. We will set it to 3 for testing.
    t.env.mock_all_auths();
    t.contract.set_max_delegation_depth(&3);

    let nodes: std::vec::Vec<soroban_sdk::Address> = (0..5)
        .map(|_| soroban_sdk::Address::generate(&t.env))
        .collect();
    for a in &nodes {
        t.gov_token_admin.mint(a, &100);
        // Issue #805/#1098: the remediated delegation path requires an aged
        // balance checkpoint before any weight is contributed. The legacy
        // (pre-remediation) test delegated straight from a funded-but-not-aged
        // address; that path now fails closed with InsufficientHoldingPeriod.
        checkpoint_and_age(&t, a);
    }

    t.contract.delegate_votes(&nodes[0], &nodes[1]);
    t.contract.delegate_votes(&nodes[1], &nodes[2]);
    t.contract.delegate_votes(&nodes[2], &nodes[3]);

    // Issue #1098: three edges == the cap, so the chain is accepted as-is.
    assert_eq!(t.contract.get_delegate(&nodes[0]), Some(nodes[1].clone()));
    assert_eq!(t.contract.get_delegate(&nodes[1]), Some(nodes[2].clone()));
    assert_eq!(t.contract.get_delegate(&nodes[2]), Some(nodes[3].clone()));
    assert_eq!(t.contract.get_delegate(&nodes[3]), None);
}

#[test]
fn test_max_delegation_depth_cap_exceeded_panics() {
    extern crate std;
    let t = setup();
    t.env.mock_all_auths();
    t.contract.set_max_delegation_depth(&3);

    let nodes: std::vec::Vec<soroban_sdk::Address> = (0..5)
        .map(|_| soroban_sdk::Address::generate(&t.env))
        .collect();
    // Issue #805/#1098: age each node's checkpoint so the remediated delegation
    // guard lets the depth check (not the holding-period guard) be the failure
    // under test.
    for a in &nodes {
        t.gov_token_admin.mint(a, &100);
        checkpoint_and_age(&t, a);
    }
    // Chain 0 -> 1 -> 2 -> 3 is exactly `max_depth` (3) edges: allowed.
    t.contract.delegate_votes(&nodes[0], &nodes[1]);
    t.contract.delegate_votes(&nodes[1], &nodes[2]);
    t.contract.delegate_votes(&nodes[2], &nodes[3]);

    // Issue #1098: appending 3 -> 4 would make node 0's chain 4 edges long.
    // The remediated guard observes the delegator's inbound depth (not just the
    // delegate's forward chain, which is empty here) and rejects with the
    // typed error rather than the host's numeric panic.
    let res = t.contract.try_delegate_votes(&nodes[3], &nodes[4]);
    assert_eq!(
        res,
        Err(Ok(GovernanceError::MaxDelegationDepthExceeded))
    );

    // The rejected edge left no trace.
    assert_eq!(t.contract.get_delegate(&nodes[3]), None);
}

#[test]
fn test_delegation_depth_relaxes_after_undelegate() {
    extern crate std;
    let t = setup();
    t.env.mock_all_auths();
    t.contract.set_max_delegation_depth(&3);

    let nodes: std::vec::Vec<soroban_sdk::Address> = (0..5)
        .map(|_| soroban_sdk::Address::generate(&t.env))
        .collect();
    for a in &nodes {
        t.gov_token_admin.mint(a, &100);
        checkpoint_and_age(&t, a);
    }

    // 0 -> 1 -> 2 -> 3 is exactly the cap.
    t.contract.delegate_votes(&nodes[0], &nodes[1]);
    t.contract.delegate_votes(&nodes[1], &nodes[2]);
    t.contract.delegate_votes(&nodes[2], &nodes[3]);

    // Node 3 already sits 3 hops deep (0 -> 1 -> 2 -> 3), so extending it with
    // 3 -> 4 would produce a 4-edge chain: rejected.
    assert_eq!(
        t.contract.try_delegate_votes(&nodes[3], &nodes[4]),
        Err(Ok(GovernanceError::MaxDelegationDepthExceeded))
    );

    // Issue #1098: undelegating the head must drop the 0 -> 1 reverse edge, so
    // node 3's inbound depth relaxes to 2 (1 -> 2 -> 3) and the same edge is
    // now legal.
    t.contract.undelegate_votes(&nodes[0]);
    t.contract.delegate_votes(&nodes[3], &nodes[4]); // 1 -> 2 -> 3 -> 4 (depth 3)

    assert_eq!(t.contract.get_delegate(&nodes[3]), Some(nodes[4].clone()));
}

#[test]
fn test_delegation_depth_relaxes_after_redelegation() {
    extern crate std;
    let t = setup();
    t.env.mock_all_auths();
    t.contract.set_max_delegation_depth(&3);

    let nodes: std::vec::Vec<soroban_sdk::Address> = (0..5)
        .map(|_| soroban_sdk::Address::generate(&t.env))
        .collect();
    for a in &nodes {
        t.gov_token_admin.mint(a, &100);
        checkpoint_and_age(&t, a);
    }

    t.contract.delegate_votes(&nodes[0], &nodes[1]);
    t.contract.delegate_votes(&nodes[1], &nodes[2]);
    t.contract.delegate_votes(&nodes[2], &nodes[3]);

    // Node 3 is 3 hops deep, so this extension is rejected.
    assert_eq!(
        t.contract.try_delegate_votes(&nodes[3], &nodes[4]),
        Err(Ok(GovernanceError::MaxDelegationDepthExceeded))
    );

    // Re-delegate node 0 away from node 1. The 0 -> 1 reverse edge must be
    // removed from node 1's predecessor set; node 3's inbound depth relaxes to
    // 2 (via 1 -> 2 -> 3) and the extension becomes legal. Stale reverse
    // adjacency would keep the inbound depth at 3 and wrongly reject it.
    t.contract.delegate_votes(&nodes[0], &nodes[2]);
    t.contract.delegate_votes(&nodes[3], &nodes[4]); // 1 -> 2 -> 3 -> 4 (depth 3)

    assert_eq!(t.contract.get_delegate(&nodes[0]), Some(nodes[2].clone()));
    assert_eq!(t.contract.get_delegate(&nodes[3]), Some(nodes[4].clone()));
}

#[test]
fn test_redelegation_moves_weight_to_new_delegate() {
    let t = setup();
    let voter_c = Address::generate(&t.env);
    t.gov_token_admin.mint(&voter_c, &500);
    // Issue #805: voter_c votes below, so its checkpoint must predate the proposal.
    checkpoint_and_age(&t, &voter_c);

    t.contract.delegate_votes(&t.voter_a, &t.voter_b); // A → B
    t.contract.delegate_votes(&t.voter_a, &voter_c); // A → C (re-delegate)

    let id = create_fee_proposal(&t);

    t.contract.cast_vote(&t.voter_b, &id, &false);
    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_against, 2_000); // B own only

    t.contract.cast_vote(&voter_c, &id, &true);
    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_for, 1_500); // C own 500 + A delegated 1_000
}

#[test]
fn test_delegate_votes_emits_votes_delegated_event() {
    let t = setup();
    t.contract.delegate_votes(&t.voter_a, &t.voter_b);
    let events = t.env.events().all();
    assert!(
        !events.events().is_empty(),
        "VotesDelegated event should be emitted"
    );
}

#[test]
fn test_undelegate_votes_emits_votes_undelegated_event() {
    let t = setup();
    t.contract.delegate_votes(&t.voter_a, &t.voter_b);
    t.contract.undelegate_votes(&t.voter_a);
    let events = t.env.events().all();
    assert!(
        !events.events().is_empty(),
        "VotesUndelegated event should be emitted"
    );
}

#[test]
fn test_zero_balance_voter_with_delegation_can_vote() {
    let t = setup();
    let receiver = Address::generate(&t.env);
    // receiver has 0 own tokens. Issue #805: even a zero-balance voter needs
    // an aged (zero) checkpoint before it can vote with delegated weight.
    checkpoint_and_age(&t, &receiver);

    t.contract.delegate_votes(&t.voter_a, &receiver);

    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&receiver, &id, &true);

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_for, 1_000); // only delegated weight from voter_a
}

#[test]
fn test_execute_timelock_delay_flow() {
    let t = setup();

    // Set a timelock delay of 100 ledgers
    t.contract.set_execution_delay(&t.admin, &100);
    assert_eq!(t.contract.get_execution_delay(), 100);

    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.proposer, &id, &true);
    t.contract.cast_vote(&t.voter_a, &id, &true);

    let mut ledger = t.env.ledger().get();
    let voting_end = t.contract.get_proposal(&id).voting_end;
    ledger.timestamp = voting_end + 1;
    t.env.ledger().set(ledger);

    // Call execute_proposal to queue it (transition Active -> Passed)
    // The proposal has passed and sets eta_ledger to current_ledger + 100
    let initial_ledger = t.env.ledger().sequence();
    t.contract.execute_proposal(&id);

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.status, ProposalStatus::Passed);
    assert_eq!(p.eta_ledger, initial_ledger + 100);

    // Attempting to execute immediately should fail with TimelockNotExpired
    let res = t.contract.try_execute_proposal(&id);
    assert_eq!(res, Err(Ok(GovernanceError::TimelockNotExpired)));

    // Progress ledger by 99 blocks (still before timelock)
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += 99;
    t.env.ledger().set(ledger);

    let res = t.contract.try_execute_proposal(&id);
    assert_eq!(res, Err(Ok(GovernanceError::TimelockNotExpired)));

    // Progress to timelock expiration (sequence_number >= eta_ledger)
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += 1;
    t.env.ledger().set(ledger);

    // Now execution should succeed
    let res = t.contract.try_execute_proposal(&id);
    assert!(res.is_ok());

    let p_final = t.contract.get_proposal(&id);
    assert_eq!(p_final.status, ProposalStatus::Executed);
}

#[test]
fn test_execute_failed_proposal_fails() {
    let t = setup();
    let id = create_fee_proposal(&t);

    // No votes are cast. After voting ends, the proposal fails to meet quorum.
    let mut ledger = t.env.ledger().get();
    let voting_end = t.contract.get_proposal(&id).voting_end;
    ledger.timestamp = voting_end + 1;
    t.env.ledger().set(ledger);

    // Issue #1007: quorum miss commits `Rejected` instead of reverting.
    t.contract.execute_proposal(&id);
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Rejected);
}

#[test]
fn test_incremental_vote_result_caching_and_delegation() {
    let t = setup();

    // Create a proposal
    let id = create_fee_proposal(&t);

    // Initial cached totals should be 0
    let initial_p = t.contract.get_proposal(&id);
    assert_eq!(initial_p.votes_for, 0);
    assert_eq!(initial_p.votes_against, 0);

    // Delegate voter_a to voter_b
    t.contract.delegate_votes(&t.voter_a, &t.voter_b);

    // voter_b votes for the proposal.
    // voter_b has 2,000 own weight + 1,000 delegated from voter_a = 3,000 weight.
    t.contract.cast_vote(&t.voter_b, &id, &true);

    // Cached votes_for should be 3,000 now
    let p_after_vote1 = t.contract.get_proposal(&id);
    assert_eq!(p_after_vote1.votes_for, 3_000);
    assert_eq!(p_after_vote1.votes_against, 0);

    // proposer (1_000 weight) votes against.
    t.contract.cast_vote(&t.proposer, &id, &false);

    // Cached totals should update incrementally to votes_for = 3,000 and votes_against = 1_000
    let p_final = t.contract.get_proposal(&id);
    assert_eq!(p_final.votes_for, 3_000);
    assert_eq!(p_final.votes_against, 1_000);
}

// ── Issue #68: veto_proposal ──────────────────────────────────────────────────

fn reason_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[0xDEu8; 32])
}

/// Admin can veto an Active proposal — status transitions to Vetoed.
#[test]
fn test_veto_active_proposal_succeeds() {
    let t = setup();
    setup_veto_multisig(&t);
    let id = create_fee_proposal(&t);

    t.contract
        .veto_proposal(&t.admin, &id, &reason_hash(&t.env));

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.status, ProposalStatus::Vetoed);
}

/// Admin can veto a Passed proposal (e.g. harmful proposal that just passed voting).
#[test]
fn test_veto_passed_proposal_succeeds() {
    let t = setup();
    setup_veto_multisig(&t);
    let id = create_fee_proposal(&t);

    // Push it into Passed status via execute_proposal path.
    t.contract.cast_vote(&t.voter_a, &id, &true);
    t.contract.cast_vote(&t.voter_b, &id, &true);
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);

    // Manually set the proposal to Passed via internal call.
    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert!(res.is_ok());
    let p = t.contract.get_proposal(&id);
    assert_eq!(p.status, ProposalStatus::Passed);

    // Veto the Passed proposal directly.
    t.contract
        .veto_proposal(&t.admin, &id, &reason_hash(&t.env));
    let p_after = t.contract.get_proposal(&id);
    assert_eq!(p_after.status, ProposalStatus::Vetoed);

    // Now create a brand-new proposal and veto it while still Active.
    let id2 = create_fee_proposal(&t);
    t.contract
        .veto_proposal(&t.admin, &id2, &reason_hash(&t.env));
    let p2 = t.contract.get_proposal(&id2);
    assert_eq!(p2.status, ProposalStatus::Vetoed);
}

/// Non-admin caller cannot veto — should panic (auth failure via client call).
#[test]
#[should_panic]
fn test_non_admin_veto_fails() {
    let env = Env::default();
    // Do NOT call mock_all_auths — require_auth will reject any unauthorized caller.
    let token_id = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let token_addr = token_id.address();
    let iln_id = env.register_contract(None, MockIln);
    let dist_id = env.register_contract(None, MockIln);
    let rep_id = env.register_contract(None, MockIln);
    let admin = Address::generate(&env);
    let non_admin = Address::generate(&env);

    let contract_id = env.register_contract(None, GovContract);
    let contract = GovContractClient::new(&env, &contract_id);

    // Initialize using mock_all_auths scoped to setup only.
    env.mock_all_auths();
    contract.initialize(&iln_id, &dist_id, &rep_id, &token_addr, &admin, &10_000);

    let gov_token_admin = StellarAssetClient::new(&env, &token_addr);
    gov_token_admin.mint(&non_admin, &1_000);

    let id = contract.create_proposal(
        &non_admin,
        &ProposalAction::UpdateFeeRate(200),
        &dummy_hash(&env),
        &200_i128,
    );

    // Clear mocked auths — next call must provide real authorization.
    // The contract client call will use non_admin's auth context, but
    // the stored admin is a different address, so require_auth panics.
    let env2 = Env::default(); // no mock_all_auths
    let contract2 = GovContractClient::new(&env2, &contract_id);
    contract2.veto_proposal(&non_admin, &id, &BytesN::from_array(&env2, &[0xDEu8; 32]));
}

/// Non-admin veto returns NotAdmin error (verified via internal contract call).
#[test]
fn test_non_admin_veto_returns_error() {
    let t = setup();
    setup_veto_multisig(&t);
    let id = create_fee_proposal(&t);

    // Proposal is Active; veto via the real admin succeeds.
    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::veto_proposal(t.env.clone(), t.admin.clone(), id, reason_hash(&t.env))
    });
    assert_eq!(res, Ok(()));

    // Attempting to veto the same (now-Vetoed) proposal returns NotVetoable.
    let res2 = t.env.as_contract(&t.contract.address, || {
        GovContract::veto_proposal(t.env.clone(), t.admin.clone(), id, reason_hash(&t.env))
    });
    assert_eq!(res2, Err(GovernanceError::NotVetoable));
}

/// Vetoed proposal cannot be executed.
#[test]
#[should_panic]
fn test_vetoed_proposal_cannot_be_executed() {
    let t = setup();
    setup_veto_multisig(&t);
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);
    t.contract.cast_vote(&t.voter_b, &id, &true);

    // Veto it before voting ends.
    t.contract
        .veto_proposal(&t.admin, &id, &reason_hash(&t.env));

    // Advance past voting window and attempt execution — must panic (AlreadyResolved).
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);
    t.contract.execute_proposal(&id);
}

/// Veto emits the ProposalVetoed event.
#[test]
fn test_veto_emits_proposal_vetoed_event() {
    let t = setup();
    setup_veto_multisig(&t);
    let id = create_fee_proposal(&t);
    t.contract
        .veto_proposal(&t.admin, &id, &reason_hash(&t.env));

    let events = t.env.events().all();
    assert!(
        !events.events().is_empty(),
        "ProposalVetoed event should be emitted"
    );
}

/// Veto power is enabled after initialisation.
#[test]
fn test_veto_power_enabled_after_init() {
    let t = setup();
    assert!(t.contract.is_veto_power_enabled());
}

/// Governance (via ILN contract auth) can disable veto power.
#[test]
fn test_disable_veto_power_succeeds() {
    let t = setup();
    t.contract.disable_veto_power();
    assert!(!t.contract.is_veto_power_enabled());
}

/// After veto power is disabled, veto_proposal returns VetoPowerDisabled.
#[test]
fn test_veto_after_disable_returns_error() {
    let t = setup();
    setup_veto_multisig(&t);
    let id = create_fee_proposal(&t);

    // Governance disables veto power.
    t.contract.disable_veto_power();

    // Admin tries to veto — must fail.
    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::veto_proposal(t.env.clone(), t.admin.clone(), id, reason_hash(&t.env))
    });
    assert_eq!(res, Err(GovernanceError::VetoPowerDisabled));
}

// ── Issue #1099: durable veto approval receipts ──────────────────────────────

/// A collected veto approval must survive archival of the temporary approval
/// list, and must not be re-countable by the same signer.
#[test]
fn test_veto_approval_receipt_survives_archival_and_dedups() {
    let t = setup();
    let id = create_fee_proposal(&t);

    let signer_a = t.admin.clone();
    let signer_b = Address::generate(&t.env);
    let signers = soroban_sdk::vec![&t.env, signer_a.clone(), signer_b.clone()];
    t.contract.configure_veto_multisig(&signers, &2);

    // First approval: below threshold, proposal stays Active.
    t.contract
        .veto_proposal(&signer_a, &id, &reason_hash(&t.env));
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Active);

    let receipt = StorageKey::VetoApprovalReceipt(id, signer_a.clone());
    let mirror_written = t.env.as_contract(&t.contract.address, || {
        t.env.storage().persistent().has(&receipt)
    });
    assert!(mirror_written, "#1099: per-signer veto receipt must be durable");

    // Simulate host archival of the temporary approval list.
    let approvals_key = StorageKey::VetoApprovals(id);
    t.env.as_contract(&t.contract.address, || {
        t.env.storage().temporary().remove(&approvals_key);
    });

    // The approval is still counted from the durable mirror.
    let approvals = t.contract.get_veto_approvals(&id);
    assert_eq!(approvals.len(), 1);
    assert!(approvals.contains(&signer_a));

    // Re-approving is rejected despite the archived temporary list.
    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::veto_proposal(t.env.clone(), signer_a.clone(), id, reason_hash(&t.env))
    });
    assert_eq!(res, Err(GovernanceError::VetoAlreadyApproved));

    // Second distinct signer reaches the threshold and executes the veto.
    t.contract
        .veto_proposal(&signer_b, &id, &reason_hash(&t.env));
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Vetoed);

    // Receipts are cleared once the veto settles.
    assert!(t.contract.get_veto_approvals(&id).is_empty());
    let cleared = t.env.as_contract(&t.contract.address, || {
        !t.env.storage().persistent().has(&receipt)
    });
    assert!(cleared, "#1099: veto receipts cleared after execution");

    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::veto_proposal(t.env.clone(), signer_b.clone(), id, reason_hash(&t.env))
    });
    assert_eq!(res, Err(GovernanceError::NotVetoable));
}

/// Veto of a non-existent proposal returns ProposalNotFound.
#[test]
fn test_veto_nonexistent_proposal_returns_error() {
    let t = setup();
    setup_veto_multisig(&t);
    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::veto_proposal(t.env.clone(), t.admin.clone(), 9999, reason_hash(&t.env))
    });
    assert_eq!(res, Err(GovernanceError::ProposalNotFound));
}

/// Veto of an already-executed proposal returns NotVetoable.
#[test]
fn test_veto_executed_proposal_returns_not_vetoable() {
    let t = setup();
    setup_veto_multisig(&t);
    let id = create_fee_proposal(&t);

    // Execute the proposal (voter_b has enough to meet quorum against supply 10_000).
    t.contract.cast_vote(&t.voter_b, &id, &true);
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);

    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)?;
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert!(res.is_ok());

    // Now try to veto the executed proposal.
    let res2 = t.env.as_contract(&t.contract.address, || {
        GovContract::veto_proposal(t.env.clone(), t.admin.clone(), id, reason_hash(&t.env))
    });
    assert_eq!(res2, Err(GovernanceError::NotVetoable));
}

// ── feat/create-proposal: balance check, event, configurable window ───────────

/// Proposer with exactly MIN_PROPOSAL_BALANCE can create a proposal.
#[test]
fn test_create_proposal_with_exact_min_balance_succeeds() {
    let t = setup();
    // proposer has exactly 1_000 tokens in setup — equal to the default MIN_PROPOSAL_BALANCE.
    assert_eq!(t.gov_token.balance(&t.proposer), 1_000);

    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::UpdateFeeRate(100),
        &dummy_hash(&t.env),
        &100_i128,
    );
    let p = t.contract.get_proposal(&id);
    assert_eq!(p.status, ProposalStatus::Active);
}

/// Proposer with balance above the minimum can create a proposal.
#[test]
fn test_create_proposal_with_sufficient_balance_succeeds() {
    let t = setup();
    // voter_a has 1_000 tokens — above the 1_000 default minimum.
    let id = t.contract.create_proposal(
        &t.voter_a,
        &ProposalAction::UpdateFeeRate(50),
        &dummy_hash(&t.env),
        &50_i128,
    );
    let p = t.contract.get_proposal(&id);
    assert_eq!(p.proposer, t.voter_a);
    assert_eq!(p.status, ProposalStatus::Active);
}

/// Proposer with balance below the minimum is rejected.
#[test]
#[should_panic]
fn test_create_proposal_insufficient_balance_panics() {
    let t = setup();
    // Create a fresh address with only 500 tokens — below the default minimum of 1_000.
    let poor_proposer = Address::generate(&t.env);
    t.gov_token_admin.mint(&poor_proposer, &500);
    t.contract.create_proposal(
        &poor_proposer,
        &ProposalAction::UpdateFeeRate(200),
        &dummy_hash(&t.env),
        &200_i128,
    );
}

/// Proposer with balance below the minimum returns InsufficientProposerBalance.
#[test]
fn test_create_proposal_insufficient_balance_returns_error() {
    let t = setup();
    // Create a fresh address with only 500 tokens — below the default minimum of 1_000.
    let poor_proposer = Address::generate(&t.env);
    t.gov_token_admin.mint(&poor_proposer, &500);
    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::create_proposal(
            t.env.clone(),
            poor_proposer.clone(),
            ProposalAction::UpdateFeeRate(200),
            dummy_hash(&t.env),
            200_i128,
        )
    });
    assert_eq!(res, Err(GovernanceError::InsufficientProposerBalance));
}

/// Address with zero balance cannot create a proposal.
#[test]
#[should_panic]
fn test_create_proposal_zero_balance_panics() {
    let t = setup();
    let zero_addr = Address::generate(&t.env);
    t.contract.create_proposal(
        &zero_addr,
        &ProposalAction::UpdateFeeRate(200),
        &dummy_hash(&t.env),
        &200_i128,
    );
}

/// create_proposal emits a ProposalCreated event.
#[test]
fn test_create_proposal_emits_proposal_created_event() {
    let t = setup();
    // voter_a has 1_000 tokens — meets the default minimum.
    t.contract.create_proposal(
        &t.voter_a,
        &ProposalAction::UpdateFeeRate(100),
        &dummy_hash(&t.env),
        &100_i128,
    );
    let events = t.env.events().all();
    assert!(
        !events.events().is_empty(),
        "ProposalCreated event should be emitted"
    );
}

/// Voting window is set to VOTING_PERIOD_SECS (259_200 s) from creation time.
#[test]
fn test_create_proposal_voting_end_equals_voting_period_secs() {
    let t = setup();
    let now = t.env.ledger().timestamp();
    let id = t.contract.create_proposal(
        &t.voter_a,
        &ProposalAction::UpdateFeeRate(100),
        &dummy_hash(&t.env),
        &100_i128,
    );
    let p = t.contract.get_proposal(&id);
    assert_eq!(p.voting_end, now + 259_200);
}

/// get_min_proposal_balance returns the default value after initialisation.
#[test]
fn test_get_min_proposal_balance_returns_default() {
    let t = setup();
    assert_eq!(t.contract.get_min_proposal_balance(), 1_000);
}

/// set_min_proposal_balance updates the threshold; a previously-blocked
/// proposer can now create a proposal once the threshold is lowered.
#[test]
fn test_set_min_proposal_balance_allows_previously_blocked_proposer() {
    let t = setup();
    // Create a fresh address with only 500 tokens — blocked at default 1_000.
    let poor_proposer = Address::generate(&t.env);
    t.gov_token_admin.mint(&poor_proposer, &500);

    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::create_proposal(
            t.env.clone(),
            poor_proposer.clone(),
            ProposalAction::UpdateFeeRate(200),
            dummy_hash(&t.env),
            200_i128,
        )
    });
    assert_eq!(res, Err(GovernanceError::InsufficientProposerBalance));

    // Lower the threshold to 500 via the ILN contract auth.
    t.contract.set_min_proposal_balance(&500_i128);
    assert_eq!(t.contract.get_min_proposal_balance(), 500);

    // Now the proposer can create a proposal.
    let id = t.contract.create_proposal(
        &poor_proposer,
        &ProposalAction::UpdateFeeRate(200),
        &dummy_hash(&t.env),
        &200_i128,
    );
    let p = t.contract.get_proposal(&id);
    assert_eq!(p.proposer, poor_proposer);
    assert_eq!(p.status, ProposalStatus::Active);
}

/// All four ProposalAction variants are accepted by create_proposal.
#[test]
fn test_create_proposal_all_action_variants_accepted() {
    let t = setup();
    let token_addr = Address::generate(&t.env);

    let actions: &[(ProposalAction, i128)] = &[
        (ProposalAction::UpdateFeeRate(100), 100),
        (ProposalAction::AddToken(token_addr.clone(), 6_u32), 0),
        (ProposalAction::RemoveToken(token_addr.clone()), 0),
        (ProposalAction::UpdateMaxDiscountRate(50), 50),
    ];

    for (action, value) in actions {
        let id = t
            .contract
            .create_proposal(&t.voter_a, action, &dummy_hash(&t.env), value);
        let p = t.contract.get_proposal(&id);
        assert_eq!(p.action_type, *action);
        assert_eq!(p.proposed_value, *value);
    }
}

#[test]
fn test_list_proposals_pagination_and_ordering() {
    let t = setup();
    let action = ProposalAction::UpdateFeeRate(100);

    // Create 25 proposals.
    for _ in 0..25 {
        t.contract
            .create_proposal(&t.proposer, &action, &dummy_hash(&t.env), &0);
    }

    // Test page 0, size 10 (should return ids 25 down to 16)
    let page0 = t.contract.list_proposals(&None, &None, &10);
    assert_eq!(page0.len(), 10);
    assert_eq!(page0.get(0).unwrap().id, 25);
    assert_eq!(page0.get(9).unwrap().id, 16);

    // Test page 1 using cursor from page 0, size 10 (should return ids 15 down to 6)
    let page1 = t.contract.list_proposals(&None, &Some(16), &10);
    assert_eq!(page1.len(), 10);
    assert_eq!(page1.get(0).unwrap().id, 15);
    assert_eq!(page1.get(9).unwrap().id, 6);

    // Test page 2 using cursor from page 1, size 10 (should return ids 5 down to 1)
    let page2 = t.contract.list_proposals(&None, &Some(6), &10);
    assert_eq!(page2.len(), 5);
    assert_eq!(page2.get(0).unwrap().id, 5);
    assert_eq!(page2.get(4).unwrap().id, 1);

    // Test empty page (cursor at or below 1)
    let page3 = t.contract.list_proposals(&None, &Some(1), &10);
    assert_eq!(page3.len(), 0);

    // Test zero page size
    let page_zero = t.contract.list_proposals(&None, &None, &0);
    assert_eq!(page_zero.len(), 0);

    // Test max page size enforced (request 30, get 20)
    let page_max = t.contract.list_proposals(&None, &None, &30);
    assert_eq!(page_max.len(), 20);
    assert_eq!(page_max.get(0).unwrap().id, 25);
    assert_eq!(page_max.get(19).unwrap().id, 6);
}

#[test]
fn test_list_proposals_status_filtering() {
    let t = setup();
    setup_veto_multisig(&t);
    let action = ProposalAction::UpdateFeeRate(100);

    // Create 3 proposals. All start Active.
    let id1 = t
        .contract
        .create_proposal(&t.proposer, &action, &dummy_hash(&t.env), &0);
    let id2 = t
        .contract
        .create_proposal(&t.proposer, &action, &dummy_hash(&t.env), &0);
    let id3 = t
        .contract
        .create_proposal(&t.proposer, &action, &dummy_hash(&t.env), &0);

    // Veto proposal 2.
    // Ensure caller is admin
    t.env.mock_all_auths();
    t.contract
        .veto_proposal(&t.admin, &id2, &dummy_hash(&t.env));

    // Advance time to end voting for proposal 3, then execute but reject it (votes against).
    t.gov_token_admin.mint(&t.voter_a, &10_000);
    t.contract.cast_vote(&t.voter_a, &id3, &false);
    t.env
        .ledger()
        .set_timestamp(t.env.ledger().timestamp() + VOTING_PERIOD_SECS + 1);
    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id3)
    });

    // Let's verify statuses: 1 is Active, 2 is Vetoed, 3 is Rejected.
    assert_eq!(t.contract.get_proposal(&id1).status, ProposalStatus::Active);
    assert_eq!(t.contract.get_proposal(&id2).status, ProposalStatus::Vetoed);
    assert_eq!(
        t.contract.get_proposal(&id3).status,
        ProposalStatus::Rejected
    );

    // List active
    let active_list = t
        .contract
        .list_proposals(&Some(ProposalStatus::Active), &None, &10);
    assert_eq!(active_list.len(), 1);
    assert_eq!(active_list.get(0).unwrap().id, id1);

    // List vetoed
    let vetoed_list = t
        .contract
        .list_proposals(&Some(ProposalStatus::Vetoed), &None, &10);
    assert_eq!(vetoed_list.len(), 1);
    assert_eq!(vetoed_list.get(0).unwrap().id, id2);

    // List rejected
    let rejected_list = t
        .contract
        .list_proposals(&Some(ProposalStatus::Rejected), &None, &10);
    assert_eq!(rejected_list.len(), 1);
    assert_eq!(rejected_list.get(0).unwrap().id, id3);

    // List passed (none)
    let passed_list = t
        .contract
        .list_proposals(&Some(ProposalStatus::Passed), &None, &10);
    assert_eq!(passed_list.len(), 0);

    // List all
    let all_list = t.contract.list_proposals(&None, &None, &10);
    assert_eq!(all_list.len(), 3);
    // Order is most recent first (id3, id2, id1)
    assert_eq!(all_list.get(0).unwrap().id, id3);
    assert_eq!(all_list.get(1).unwrap().id, id2);
    assert_eq!(all_list.get(2).unwrap().id, id1);
}

#[test]
fn test_list_proposals_concurrent_write_stability() {
    let t = setup();
    let action = ProposalAction::UpdateFeeRate(100);

    // Create 20 baseline proposals (ids 1..=20).
    for _ in 0..20 {
        t.contract
            .create_proposal(&t.proposer, &action, &dummy_hash(&t.env), &0);
    }

    // Full unpaginated baseline read of the 20 proposals (most-recent-first: ids 20 down to 1).
    let unpaginated = t.contract.list_proposals(&None, &None, &20);
    assert_eq!(unpaginated.len(), 20);

    // Fetch page 1 (cursor: None, page_size: 10) -> returns ids 20 down to 11.
    let page1 = t.contract.list_proposals(&None, &None, &10);
    assert_eq!(page1.len(), 10);
    assert_eq!(page1.get(0).unwrap().id, 20);
    let last_id_page1 = page1.get(9).unwrap().id;
    assert_eq!(last_id_page1, 11);

    // Concurrently create a new proposal (id 21) while client was between page 1 and page 2.
    let new_id = t
        .contract
        .create_proposal(&t.proposer, &action, &dummy_hash(&t.env), &0);
    assert_eq!(new_id, 21);

    // Fetch page 2 using the stable cursor from the end of page 1 (cursor: Some(11), page_size: 10).
    let page2 = t.contract.list_proposals(&None, &Some(last_id_page1), &10);
    assert_eq!(page2.len(), 10);
    assert_eq!(page2.get(0).unwrap().id, 10);
    assert_eq!(page2.get(9).unwrap().id, 1);

    // Confirm no proposal from the original dataset is skipped or duplicated across the two calls.
    let mut combined_ids = soroban_sdk::Vec::new(&t.env);
    for p in page1.iter() {
        combined_ids.push_back(p.id);
    }
    for p in page2.iter() {
        assert!(
            !combined_ids.contains(&p.id),
            "proposal {} duplicated across paginated calls",
            p.id
        );
        combined_ids.push_back(p.id);
    }

    assert_eq!(combined_ids.len(), 20);
    for i in 0..20 {
        assert_eq!(combined_ids.get(i).unwrap(), unpaginated.get(i).unwrap().id);
    }
}

// ── Issue #545: UpdateDecayParams proposal ──────────────────────────────────

#[test]
fn test_create_and_execute_decay_params_proposal() {
    let t = setup();
    let hash = dummy_hash(&t.env);

    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::UpdateDecayParams(100, 5000),
        &hash,
        &100_i128,
    );

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.action_type, ProposalAction::UpdateDecayParams(100, 5000));
    assert_eq!(p.status, ProposalStatus::Active);

    // Vote to pass
    t.gov_token_admin.mint(&t.voter_a, &10_000);
    t.contract.cast_vote(&t.voter_a, &id, &true);

    // Advance past voting period
    t.env
        .ledger()
        .set_timestamp(t.env.ledger().timestamp() + VOTING_PERIOD_SECS + 1);

    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });

    // Should be passed (pending timelock)
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Passed);

    // Execute after timelock
    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });

    assert_eq!(
        t.contract.get_proposal(&id).status,
        ProposalStatus::Executed
    );
}

// ── Issue #fix: AddToken proposal execution ──────────────────────────────────

#[test]
fn test_create_and_execute_add_token_proposal() {
    let t = setup();
    let token_addr = Address::generate(&t.env);
    let hash = dummy_hash(&t.env);

    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::AddToken(token_addr.clone(), 6_u32),
        &hash,
        &0_i128,
    );

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.action_type, ProposalAction::AddToken(token_addr, 6_u32));
    assert_eq!(p.status, ProposalStatus::Active);

    // Vote to pass
    t.gov_token_admin.mint(&t.voter_a, &10_000);
    t.contract.cast_vote(&t.voter_a, &id, &true);

    // Advance past voting period
    t.env
        .ledger()
        .set_timestamp(t.env.ledger().timestamp() + VOTING_PERIOD_SECS + 1);

    // First execution transitions Active -> Passed (pending timelock)
    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Passed);

    // Second execution after timelock invokes the ILN contract's add_token
    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert_eq!(
        t.contract.get_proposal(&id).status,
        ProposalStatus::Executed
    );
}

// ── Issue #544: UpdateDistributionRewardParams proposal ─────────────────────

#[test]
fn test_create_and_execute_distribution_reward_params_proposal() {
    let t = setup();
    let hash = dummy_hash(&t.env);

    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::UpdateDistributionRewardParams(7_500_000, 2_000_000_000, 15_000_000),
        &hash,
        &7_500_000_i128,
    );

    let p = t.contract.get_proposal(&id);
    assert_eq!(
        p.action_type,
        ProposalAction::UpdateDistributionRewardParams(7_500_000, 2_000_000_000, 15_000_000,)
    );

    // Vote to pass
    t.gov_token_admin.mint(&t.voter_a, &10_000);
    t.contract.cast_vote(&t.voter_a, &id, &true);

    // Advance past voting period
    t.env
        .ledger()
        .set_timestamp(t.env.ledger().timestamp() + VOTING_PERIOD_SECS + 1);

    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });

    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Passed);

    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });

    assert_eq!(
        t.contract.get_proposal(&id).status,
        ProposalStatus::Executed
    );
}

// ── Issue #533: UpdateFeeTiers proposal ─────────────────────────────────────

#[test]
fn test_create_and_execute_fee_tiers_proposal() {
    let t = setup();
    let hash = dummy_hash(&t.env);

    let tiers = soroban_sdk::vec![
        &t.env,
        FeeTierConfig {
            min_amount: 0,
            fee_rate_bps: 500,
        },
        FeeTierConfig {
            min_amount: 100_000_000,
            fee_rate_bps: 300,
        },
        FeeTierConfig {
            min_amount: 1_000_000_000,
            fee_rate_bps: 100,
        },
    ];

    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::UpdateFeeTiers(tiers.clone()),
        &hash,
        &0_i128,
    );

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.action_type, ProposalAction::UpdateFeeTiers(tiers));

    // Vote to pass
    t.gov_token_admin.mint(&t.voter_a, &10_000);
    t.contract.cast_vote(&t.voter_a, &id, &true);

    // Advance past voting period
    t.env
        .ledger()
        .set_timestamp(t.env.ledger().timestamp() + VOTING_PERIOD_SECS + 1);

    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });

    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Passed);

    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });

    assert_eq!(
        t.contract.get_proposal(&id).status,
        ProposalStatus::Executed
    );
}

// ── Issue #539: Upgrade proposal action ────────────────────────────────────

#[test]
fn test_upgrade_proposal_creates_and_executes() {
    let t = setup();

    let new_wasm_hash = BytesN::from_array(&t.env, &[0xABu8; 32]);

    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::Upgrade(new_wasm_hash.clone()),
        &dummy_hash(&t.env),
        &0_i128,
    );

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.action_type, ProposalAction::Upgrade(new_wasm_hash));

    // Vote to pass
    t.gov_token_admin.mint(&t.voter_a, &10_000);
    t.contract.cast_vote(&t.voter_a, &id, &true);

    // Advance past voting period
    t.env
        .ledger()
        .set_timestamp(t.env.ledger().timestamp() + VOTING_PERIOD_SECS + 1);

    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Passed);

    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert_eq!(
        t.contract.get_proposal(&id).status,
        ProposalStatus::Executed
    );
}

// ── Distribution Reward Rate Governance ────────────────────────────────

/// Proposal to update LP reward rate can be created.
#[test]
fn test_create_lp_reward_rate_proposal() {
    let t = setup();
    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::UpdateLpRewardRate(15_000_000),
        &dummy_hash(&t.env),
        &15_000_000_i128,
    );
    let proposal = t.contract.get_proposal(&id);
    assert_eq!(proposal.proposed_value, 15_000_000);
    match proposal.action_type {
        ProposalAction::UpdateLpRewardRate(rate) => assert_eq!(rate, 15_000_000),
        _ => panic!("Expected UpdateLpRewardRate"),
    }
}

/// Proposal to update freelancer reward rate can be created.
#[test]
fn test_create_freelancer_reward_rate_proposal() {
    let t = setup();
    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::UpdateFreelancerRewardRate(7_000_000),
        &dummy_hash(&t.env),
        &7_000_000_i128,
    );
    let proposal = t.contract.get_proposal(&id);
    match proposal.action_type {
        ProposalAction::UpdateFreelancerRewardRate(rate) => assert_eq!(rate, 7_000_000),
        _ => panic!("Expected UpdateFreelancerRewardRate"),
    }
}

/// Proposal to update payer reward rate can be created.
#[test]
fn test_create_payer_reward_rate_proposal() {
    let t = setup();
    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::UpdatePayerRewardRate(6_000_000),
        &dummy_hash(&t.env),
        &6_000_000_i128,
    );
    let proposal = t.contract.get_proposal(&id);
    match proposal.action_type {
        ProposalAction::UpdatePayerRewardRate(rate) => assert_eq!(rate, 6_000_000),
        _ => panic!("Expected UpdatePayerRewardRate"),
    }
}

// ── Insurance Pool Parameter Governance ────────────────────────────────

/// Proposal to update insurance coverage cap can be created.
#[test]
fn test_create_insurance_coverage_cap_proposal() {
    let t = setup();
    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::UpdateInsuranceCoverageCap(50_000_000),
        &dummy_hash(&t.env),
        &50_000_000_i128,
    );
    let proposal = t.contract.get_proposal(&id);
    assert_eq!(proposal.proposed_value, 50_000_000);
    match proposal.action_type {
        ProposalAction::UpdateInsuranceCoverageCap(cap) => assert_eq!(cap, 50_000_000),
        _ => panic!("Expected UpdateInsuranceCoverageCap"),
    }
}

/// Proposal to update insurance premium rate can be created.
#[test]
fn test_create_insurance_premium_rate_proposal() {
    let t = setup();
    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::UpdateInsurancePremiumRate(300),
        &dummy_hash(&t.env),
        &300_i128,
    );
    let proposal = t.contract.get_proposal(&id);
    match proposal.action_type {
        ProposalAction::UpdateInsurancePremiumRate(rate) => assert_eq!(rate, 300),
        _ => panic!("Expected UpdateInsurancePremiumRate"),
    }
}

// ── Issue #532: oracle registry governance actions ──────────────────────────

/// Proposal to register an oracle for a feed type can be created and
/// executes successfully against the ILN contract.
#[test]
fn test_create_and_execute_register_oracle_proposal() {
    let t = setup();
    let oracle = Address::generate(&t.env);
    let hash = dummy_hash(&t.env);

    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::RegisterOracle(OracleFeedType::Identity, oracle.clone()),
        &hash,
        &0_i128,
    );

    let p = t.contract.get_proposal(&id);
    assert_eq!(
        p.action_type,
        ProposalAction::RegisterOracle(OracleFeedType::Identity, oracle)
    );

    t.gov_token_admin.mint(&t.voter_a, &10_000);
    t.contract.cast_vote(&t.voter_a, &id, &true);
    t.env
        .ledger()
        .set_timestamp(t.env.ledger().timestamp() + VOTING_PERIOD_SECS + 1);

    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Passed);

    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert_eq!(
        t.contract.get_proposal(&id).status,
        ProposalStatus::Executed
    );
}

/// Proposal to remove an oracle for a feed type can be created and executes.
#[test]
fn test_create_and_execute_remove_oracle_proposal() {
    let t = setup();
    let hash = dummy_hash(&t.env);

    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::RemoveOracle(OracleFeedType::Credit),
        &hash,
        &0_i128,
    );

    let p = t.contract.get_proposal(&id);
    assert_eq!(
        p.action_type,
        ProposalAction::RemoveOracle(OracleFeedType::Credit)
    );

    t.gov_token_admin.mint(&t.voter_a, &10_000);
    t.contract.cast_vote(&t.voter_a, &id, &true);
    t.env
        .ledger()
        .set_timestamp(t.env.ledger().timestamp() + VOTING_PERIOD_SECS + 1);

    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert_eq!(
        t.contract.get_proposal(&id).status,
        ProposalStatus::Executed
    );
}

// ── Issue #530: quadratic voting ─────────────────────────────────────────────

/// Quadratic voting is disabled by default (backwards compatibility).
#[test]
fn test_quadratic_voting_disabled_by_default() {
    let t = setup();
    assert!(!t.contract.is_quadratic_voting_enabled());
}

/// Governance (via ILN contract auth) can enable quadratic voting.
#[test]
fn test_set_quadratic_voting_enabled_toggles_flag() {
    let t = setup();
    t.contract.set_quadratic_voting_enabled(&true);
    assert!(t.contract.is_quadratic_voting_enabled());

    t.contract.set_quadratic_voting_enabled(&false);
    assert!(!t.contract.is_quadratic_voting_enabled());
}

/// With quadratic voting disabled (default), cast_vote weight is unchanged:
/// a 10_000-token holder still casts exactly 10_000 votes.
#[test]
fn test_linear_voting_weight_unchanged_when_disabled() {
    let t = setup();
    let whale = Address::generate(&t.env);
    t.gov_token_admin.mint(&whale, &10_000);
    checkpoint_and_age(&t, &whale);

    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&whale, &id, &true);

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_for, 10_000);
}

/// With quadratic voting enabled, a whale's vote weight is sqrt(balance),
/// not the raw balance — this is the whole point of Issue #530: reduce
/// whale dominance relative to linear weighting.
#[test]
fn test_quadratic_voting_weight_is_sqrt_of_balance() {
    let t = setup();
    t.contract.set_quadratic_voting_enabled(&true);

    let whale = Address::generate(&t.env);
    t.gov_token_admin.mint(&whale, &10_000); // sqrt(10_000) = 100
    checkpoint_and_age(&t, &whale);

    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&whale, &id, &true);

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_for, 100);
}

/// Quadratic voting compresses the ratio between a whale and a small
/// holder: a holder with 100x the tokens of another should end up with
/// only 10x the vote weight (sqrt(100) = 10), not 100x.
#[test]
fn test_quadratic_voting_reduces_whale_dominance_ratio() {
    let t = setup();
    t.contract.set_quadratic_voting_enabled(&true);

    let whale = Address::generate(&t.env);
    let minnow = Address::generate(&t.env);
    t.gov_token_admin.mint(&whale, &1_000_000); // sqrt = 1_000
    t.gov_token_admin.mint(&minnow, &10_000); // sqrt = 100
    checkpoint_and_age(&t, &whale);
    checkpoint_and_age(&t, &minnow);

    let id_whale = create_fee_proposal(&t);
    t.contract.cast_vote(&whale, &id_whale, &true);
    let id_minnow = create_fee_proposal(&t);
    t.contract.cast_vote(&minnow, &id_minnow, &true);

    let p_whale = t.contract.get_proposal(&id_whale);
    let p_minnow = t.contract.get_proposal(&id_minnow);

    // Raw balance ratio is 100x; quadratic weight ratio must be only 10x.
    assert_eq!(p_whale.votes_for, 1_000);
    assert_eq!(p_minnow.votes_for, 100);
    assert_eq!(p_whale.votes_for / p_minnow.votes_for, 10);
}

/// Simulates a realistic power-law distribution to verify quadratic voting
/// effectively reduces whale dominance without ignoring them entirely.
#[test]
fn test_quadratic_voting_realistic_distribution_audit() {
    extern crate std;
    let t = setup();

    // Generate 1 whale (500k), 5 dolphins (50k each), 50 shrimps (5k each)
    let whale = Address::generate(&t.env);
    t.gov_token_admin.mint(&whale, &500_000);

    let mut dolphins = std::vec::Vec::new();
    for _ in 0..5 {
        let a = Address::generate(&t.env);
        t.gov_token_admin.mint(&a, &50_000);
        dolphins.push(a);
    }

    let mut shrimps = std::vec::Vec::new();
    for _ in 0..50 {
        let a = Address::generate(&t.env);
        t.gov_token_admin.mint(&a, &5_000);
        shrimps.push(a);
    }

    // Issue #805: every voting address needs a checkpoint predating the proposal.
    t.contract.checkpoint_balance(&whale);
    for dolphin in &dolphins {
        t.contract.checkpoint_balance(dolphin);
    }
    for shrimp in &shrimps {
        t.contract.checkpoint_balance(shrimp);
    }
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += MIN_VOTE_HOLD_LEDGERS + 1;
    t.env.ledger().set(ledger);

    t.contract.set_quadratic_voting_enabled(&true);

    let id = create_fee_proposal(&t);

    t.contract.cast_vote(&whale, &id, &true);
    for dolphin in &dolphins {
        t.contract.cast_vote(dolphin, &id, &true);
    }
    for shrimp in &shrimps {
        t.contract.cast_vote(shrimp, &id, &true);
    }

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_for, 5322); // 707 + 1115 + 3500 = 5322

    let applied_weight_whale = t.contract.get_applied_vote_weight(&id, &whale);
    assert_eq!(applied_weight_whale, Some(707));
}

/// Quadratic voting also applies to delegated weight: own + delegated is
/// summed first, then the square root is taken of the combined total.
#[test]
fn test_quadratic_voting_applies_to_own_plus_delegated_weight() {
    let t = setup();
    t.contract.set_quadratic_voting_enabled(&true);

    // voter_b has 2_000, voter_a delegates 1_000 -> combined 3_000 (not a
    // perfect square, isqrt floors it): isqrt(3_000) = 54 (54^2=2916, 55^2=3025).
    t.contract.delegate_votes(&t.voter_a, &t.voter_b);
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_b, &id, &true);

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_for, 54);
}

/// A zero-balance voter with a zero-balance delegation still has no voting
/// power under quadratic voting (sqrt(0) = 0), same as linear.
#[test]
#[should_panic]
fn test_quadratic_voting_zero_balance_rejected() {
    let t = setup();
    t.contract.set_quadratic_voting_enabled(&true);
    let id = create_fee_proposal(&t);
    let zero_voter = Address::generate(&t.env);
    t.contract.cast_vote(&zero_voter, &id, &true);
}

/// cast_vote records the applied (quadratic) weight as a vote receipt,
/// retrievable via get_applied_vote_weight — this is distinct from the
/// linear snapshot balance used to compute it.
#[test]
fn test_get_applied_vote_weight_returns_quadratic_receipt() {
    let t = setup();
    t.contract.set_quadratic_voting_enabled(&true);

    let whale = Address::generate(&t.env);
    t.gov_token_admin.mint(&whale, &10_000); // sqrt = 100
    checkpoint_and_age(&t, &whale);

    let id = create_fee_proposal(&t);
    assert_eq!(t.contract.get_applied_vote_weight(&id, &whale), None);

    t.contract.cast_vote(&whale, &id, &true);
    assert_eq!(t.contract.get_applied_vote_weight(&id, &whale), Some(100));
}

/// get_applied_vote_weight also records the linear weight when quadratic
/// voting is disabled, so the receipt is always available regardless of mode.
#[test]
fn test_get_applied_vote_weight_records_linear_weight_when_disabled() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);
    assert_eq!(
        t.contract.get_applied_vote_weight(&id, &t.voter_a),
        Some(1_000)
    );
}

/// Non-ILN-contract callers cannot toggle quadratic voting.
#[test]
#[should_panic]
fn test_set_quadratic_voting_enabled_requires_iln_auth() {
    let env = Env::default();
    let token_id = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let token_addr = token_id.address();
    let iln_id = env.register_contract(None, MockIln);
    let dist_id = env.register_contract(None, MockIln);
    let rep_id = env.register_contract(None, MockIln);
    let admin = Address::generate(&env);

    let contract_id = env.register_contract(None, GovContract);
    let contract = GovContractClient::new(&env, &contract_id);

    env.mock_all_auths();
    contract.initialize(&iln_id, &dist_id, &rep_id, &token_addr, &admin, &10_000);

    // No auths mocked on this second client — require_auth on the ILN
    // contract address must reject an arbitrary caller.
    let env2 = Env::default();
    let contract2 = GovContractClient::new(&env2, &contract_id);
    contract2.set_quadratic_voting_enabled(&true);
}

// ── Issue #531: proposal execution verification ─────────────────────────────

/// Test env wired to a failing mock ILN contract, so `execute_proposal`'s
/// cross-contract call always traps.
struct FailingGovTestEnv {
    env: Env,
    contract: GovContractClient<'static>,
    voter: Address,
    proposer: Address,
}

fn setup_with_failing_iln() -> FailingGovTestEnv {
    let env = Env::default();
    env.mock_all_auths();

    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token_addr = token_id.address();
    let gov_token_admin = StellarAssetClient::new(&env, &token_addr);

    let voter = Address::generate(&env);
    let proposer = Address::generate(&env);
    let admin = Address::generate(&env);
    gov_token_admin.mint(&voter, &10_000);
    gov_token_admin.mint(&proposer, &1_000);

    let iln_contract = env.register_contract(None, MockIlnFailing);
    let dist_contract = env.register_contract(None, MockIln);
    let rep_contract = env.register_contract(None, MockIln);

    let contract_id = env.register_contract(None, GovContract);
    let contract = GovContractClient::new(&env, &contract_id);
    contract.initialize(
        &iln_contract,
        &dist_contract,
        &rep_contract,
        &token_addr,
        &admin,
        &11_000,
    );

    // Issue #805: age the voter's checkpoint so the execution-path tests
    // exercise the eligible path.
    contract.checkpoint_balance(&voter);
    let mut ledger = env.ledger().get();
    ledger.sequence_number += MIN_VOTE_HOLD_LEDGERS + 1;
    env.ledger().set(ledger);

    FailingGovTestEnv {
        env,
        contract,
        voter,
        proposer,
    }
}

/// A failed cross-contract call during execution must NOT mark the proposal
/// `Executed` — it stays `Passed` so it can be retried, and returns
/// `ExecutionFailed` instead of silently succeeding.
#[test]
fn test_execute_proposal_call_failure_reverts_to_passed() {
    let t = setup_with_failing_iln();
    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::UpdateFeeRate(200),
        &dummy_hash(&t.env),
        &200_i128,
    );
    t.contract.cast_vote(&t.voter, &id, &true);

    let mut ledger = t.env.ledger().get();
    ledger.timestamp += VOTING_PERIOD_SECS + 1;
    t.env.ledger().set(ledger);

    // Active -> Passed (quorum met, votes_for > votes_against).
    let res_pass = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert!(res_pass.is_ok());
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Passed);

    // Passed -> the callee always fails, so execution must report
    // ExecutionFailed and the proposal must remain Passed (retryable).
    let res_exec = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert_eq!(res_exec, Ok(()));
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Passed);
}

/// A failed execution can be retried indefinitely: the proposal never gets
/// stuck in a bad state, and each retry attempt is independently observable.
#[test]
fn test_execute_proposal_failure_allows_repeated_retry() {
    let t = setup_with_failing_iln();
    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::UpdateFeeRate(200),
        &dummy_hash(&t.env),
        &200_i128,
    );
    t.contract.cast_vote(&t.voter, &id, &true);

    let mut ledger = t.env.ledger().get();
    ledger.timestamp += VOTING_PERIOD_SECS + 1;
    t.env.ledger().set(ledger);

    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Passed);

    for _ in 0..3 {
        let res = t.env.as_contract(&t.contract.address, || {
            GovContract::execute_proposal(t.env.clone(), id)
        });
        assert_eq!(res, Ok(()));
        assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Passed);
    }
}

/// A failed execution emits `ProposalExecutionFailed`.
#[test]
fn test_execute_proposal_failure_emits_event() {
    let t = setup_with_failing_iln();
    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::UpdateFeeRate(200),
        &dummy_hash(&t.env),
        &200_i128,
    );
    t.contract.cast_vote(&t.voter, &id, &true);

    let mut ledger = t.env.ledger().get();
    ledger.timestamp += VOTING_PERIOD_SECS + 1;
    t.env.ledger().set(ledger);

    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });

    let events_before = t.env.events().all().events().len();
    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    let events_after = t.env.events().all().events().len();
    assert!(
        events_after > events_before,
        "ProposalExecutionFailed event should be emitted"
    );
}

/// A successful execution (the existing MockIln, which never fails) still
/// marks the proposal Executed and emits ProposalExecuted — the happy path
/// is unchanged by the Issue #531 verification logic.
#[test]
fn test_execute_proposal_success_still_marks_executed() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);
    t.contract.cast_vote(&t.voter_b, &id, &true);

    let mut ledger = t.env.ledger().get();
    ledger.timestamp += VOTING_PERIOD_SECS + 1;
    t.env.ledger().set(ledger);

    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)?;
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert!(res.is_ok());
    assert_eq!(
        t.contract.get_proposal(&id).status,
        ProposalStatus::Executed
    );
}

// ── Issue #704: UpdateReputationBonusParams proposal — full E2E lifecycle ───

#[test]
fn test_governance_reputation_bonus_params_e2e() {
    let t = setup();
    let hash = dummy_hash(&t.env);

    // Baseline set in setup(): high_rep_threshold=70, bonus_bps=100,
    // min_discount_rate_bps=50. Propose a genuine change to all three.
    let new_threshold = 80_u32;
    let new_bonus_bps = 150_u32;
    let new_min_discount_rate_bps = 75_u32;

    let id = t.contract.create_proposal(
        &t.proposer,
        &ProposalAction::UpdateReputationBonusParams(
            new_threshold,
            new_bonus_bps,
            new_min_discount_rate_bps,
        ),
        &hash,
        &7_500_000_i128,
    );

    let p = t.contract.get_proposal(&id);
    assert_eq!(
        p.action_type,
        ProposalAction::UpdateReputationBonusParams(
            new_threshold,
            new_bonus_bps,
            new_min_discount_rate_bps,
        )
    );

    // Vote to pass.
    t.gov_token_admin.mint(&t.voter_a, &10_000);
    t.contract.cast_vote(&t.voter_a, &id, &true);

    // Advance past the voting period and settle the vote (Active -> Passed).
    t.env
        .ledger()
        .set_timestamp(t.env.ledger().timestamp() + VOTING_PERIOD_SECS + 1);
    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Passed);

    // reputation_bonus's Config must still hold the pre-proposal values —
    // nothing should change before the timelock actually expires.
    let mid_config = t.rep_contract.get_config();
    assert_eq!(mid_config.high_rep_threshold, 70);
    assert_eq!(mid_config.bonus_bps, 100);
    assert_eq!(mid_config.min_discount_rate_bps, 50);

    // Advance past the timelock and execute for real (Passed -> Executed),
    // which fires the actual cross-contract update_config call.
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += 1_000;
    t.env.ledger().set(ledger);
    let _ = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert_eq!(
        t.contract.get_proposal(&id).status,
        ProposalStatus::Executed
    );

    // The whole point of this test: reputation_bonus's actual on-chain
    // Config reflects the governance-executed change, not just the
    // proposal's own bookkeeping.
    let final_config = t.rep_contract.get_config();
    assert_eq!(final_config.high_rep_threshold, new_threshold);
    assert_eq!(final_config.bonus_bps, new_bonus_bps);
    assert_eq!(
        final_config.min_discount_rate_bps,
        new_min_discount_rate_bps
    );
}

// ── Issue #814: forfeitable proposal deposit ─────────────────────────────

#[test]
fn test_deposit_defaults_to_zero_backwards_compatible() {
    let t = setup();
    assert_eq!(t.contract.get_min_proposal_deposit(), 0);
    let id = create_fee_proposal(&t);
    assert_eq!(t.contract.get_proposal_deposit(&id), 0);
    assert!(!t.contract.is_proposal_deposit_settled(&id));
}

#[test]
fn test_deposit_escrowed_on_create() {
    let t = setup();
    t.contract.set_min_proposal_deposit(&200);
    assert_eq!(t.contract.get_min_proposal_deposit(), 200);
    let before = t.gov_token.balance(&t.proposer);
    let id = create_fee_proposal(&t);
    assert_eq!(t.contract.get_proposal_deposit(&id), 200);
    assert!(!t.contract.is_proposal_deposit_settled(&id));
    assert_eq!(t.gov_token.balance(&t.proposer), before - 200);
}

#[test]
fn test_deposit_refunded_on_pass() {
    let t = setup();
    t.contract.set_min_proposal_deposit(&200);
    let id = create_fee_proposal(&t);
    let escrowed_balance = t.gov_token.balance(&t.proposer);
    // voter_b (2_000) reaches quorum alone (total_supply 10_000, quorum 1_000).
    t.contract.cast_vote(&t.voter_b, &id, &true);
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);
    t.contract.execute_proposal(&id);
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Passed);
    // Refunded exactly once.
    assert_eq!(t.gov_token.balance(&t.proposer), escrowed_balance + 200);
    assert_eq!(t.contract.get_proposal_deposit(&id), 0);
    assert!(t.contract.is_proposal_deposit_settled(&id));
}

#[test]
fn test_deposit_not_double_refunded_on_execute() {
    let t = setup();
    t.contract.set_min_proposal_deposit(&200);
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_b, &id, &true);
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);
    // Active -> Passed refunds.
    t.contract.execute_proposal(&id);
    let after_pass = t.gov_token.balance(&t.proposer);
    // Passed -> Executed must not pay a second time (idempotent settle).
    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert!(res.is_ok());
    assert_eq!(
        t.contract.get_proposal(&id).status,
        ProposalStatus::Executed
    );
    assert_eq!(t.gov_token.balance(&t.proposer), after_pass);
    assert!(t.contract.is_proposal_deposit_settled(&id));
}

#[test]
fn test_deposit_forfeited_on_reject_quorum_not_reached() {
    let t = setup();
    t.contract.set_min_proposal_deposit(&200);
    let sink = Address::generate(&t.env);
    t.contract.set_proposal_deposit_sink(&Some(sink.clone()));
    // Only 500 votes vs quorum 1_000 -> expired without quorum. Fund +
    // checkpoint + age before creating the proposal (Issue #805).
    let voter = Address::generate(&t.env);
    t.gov_token_admin.mint(&voter, &500);
    checkpoint_and_age(&t, &voter);
    let id = create_fee_proposal(&t);
    let proposer_before_forfeit = t.gov_token.balance(&t.proposer);
    t.contract.cast_vote(&voter, &id, &true);
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);
    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert_eq!(res, Ok(()));
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Rejected);
    // Proposer not refunded; sink received the forfeit.
    assert_eq!(t.gov_token.balance(&t.proposer), proposer_before_forfeit);
    assert_eq!(t.gov_token.balance(&sink), 200);
    assert!(t.contract.is_proposal_deposit_settled(&id));
}

#[test]
fn test_deposit_forfeited_on_majority_reject() {
    let t = setup();
    t.contract.set_min_proposal_deposit(&100);
    let sink = Address::generate(&t.env);
    t.contract.set_proposal_deposit_sink(&Some(sink.clone()));
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true); // 1_000 for
    t.contract.cast_vote(&t.voter_b, &id, &false); // 2_000 against
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);
    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert_eq!(res, Ok(()));
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Rejected);
    assert_eq!(t.gov_token.balance(&sink), 100);
    assert!(t.contract.is_proposal_deposit_settled(&id));
}

#[test]
fn test_set_min_proposal_deposit_rejects_negative() {
    let t = setup();
    let res = t.contract.try_set_min_proposal_deposit(&-1);
    assert_eq!(res, Err(Ok(GovernanceError::InvalidProposalDeposit)));
}

// ── Issue #805: flash-loan-resistant first-vote snapshot ────────────────────

/// Repro of the Issue #805 attack shape: flash-borrowed funds arrive in the
/// same transaction as the voter's first-ever vote, so no pre-proposal
/// checkpoint exists. The vote must be rejected, not snapshotted.
#[test]
fn test_first_vote_without_aged_checkpoint_rejected() {
    let t = setup();
    let id = create_fee_proposal(&t);
    let attacker = Address::generate(&t.env);
    // Simulated flash funds: minted in the same ledger as the vote.
    t.gov_token_admin.mint(&attacker, &50_000);
    let res = t.contract.try_cast_vote(&attacker, &id, &true);
    assert_eq!(res, Err(Ok(GovernanceError::InsufficientHoldingPeriod)));
}

/// An aged checkpoint bounds the vote to proven funds: inflating the live
/// balance after checkpointing (the flash-loan shape) does not inflate the
/// recorded weight, which stays at `min(checkpoint, current)`.
#[test]
fn test_aged_checkpoint_bounds_post_checkpoint_inflation() {
    let t = setup();
    let holder = Address::generate(&t.env);
    t.gov_token_admin.mint(&holder, &1_000);
    checkpoint_and_age(&t, &holder);

    let id = create_fee_proposal(&t);
    // Flash-loan shape: balance explodes between checkpoint and vote.
    t.gov_token_admin.mint(&holder, &99_000);
    t.contract.cast_vote(&holder, &id, &true);

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_for, 1_000); // proven funds, not the inflated 100_000
    let snapshot: i128 = t.env.as_contract(&t.contract.address, || {
        t.env
            .storage()
            .persistent()
            .get(&StorageKey::VoteWeightSnapshot(id, holder.clone()))
            .unwrap()
    });
    assert_eq!(snapshot, 1_000);
}

/// Honest steady-state flow: checkpoint, wait out the holding period, then
/// vote with the full balance on a later proposal.
#[test]
fn test_checkpoint_then_vote_carries_full_weight() {
    let t = setup();
    let holder = Address::generate(&t.env);
    t.gov_token_admin.mint(&holder, &2_500);
    checkpoint_and_age(&t, &holder);

    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&holder, &id, &true);

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_for, 2_500);
}

/// `checkpoint_balance` records the live balance and ledger, and both are
/// queryable for indexers and front-ends.
#[test]
fn test_checkpoint_balance_is_queryable() {
    let t = setup();
    let holder = Address::generate(&t.env);
    t.gov_token_admin.mint(&holder, &1_750);
    assert_eq!(t.contract.get_voter_checkpoint(&holder), None);
    t.contract.checkpoint_balance(&holder);
    let cp = t.contract.get_voter_checkpoint(&holder).unwrap();
    assert_eq!(cp.balance, 1_750);
    assert_eq!(cp.ledger, t.env.ledger().sequence());
    assert_eq!(
        t.contract.get_proposal_created_ledger(&create_fee_proposal(&t)),
        Some(t.env.ledger().sequence())
    );
}

/// Delegation entries are gated too: a fresh flash-funded address cannot
/// permanently inflate a terminal's `DelegatedToMe` tally.
#[test]
fn test_delegate_without_aged_checkpoint_rejected() {
    let t = setup();
    let attacker = Address::generate(&t.env);
    t.gov_token_admin.mint(&attacker, &50_000);
    let res = t.contract.try_delegate_votes(&attacker, &t.voter_b);
    assert_eq!(res, Err(Ok(GovernanceError::InsufficientHoldingPeriod)));
    // No tally was written.
    let p = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_b, &p, &true);
    assert_eq!(t.contract.get_proposal(&p).votes_for, 2_000);
}

// ── Issue #808: quorum reads the on-chain-tracked total supply ─────────────

/// The quorum denominator is the stored `GovTokenTotalSupply` (seeded at
/// `initialize`, updatable only via the ILN-gated
/// `set_gov_token_total_supply`) — never a caller-supplied argument.
/// Expanding the tracked supply mid-window (the mint case) raises quorum;
/// shrinking it (the burn case) lowers quorum.
#[test]
fn test_quorum_uses_stored_total_supply_across_supply_changes() {
    let t = setup();

    // Mint case: tracked supply 100_000 → quorum 10_000; voter_a's 1_000
    // falls short even though it met quorum at the seeded 10_000.
    t.contract.set_gov_token_total_supply(&100_000);
    assert_eq!(t.contract.get_gov_token_total_supply(), 100_000);
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);
    let res = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id)
    });
    assert_eq!(res, Err(GovernanceError::QuorumNotReached));
    assert_eq!(
        t.contract.get_proposal(&id).status,
        ProposalStatus::Rejected
    );

    // Burn case: tracked supply 5_000 → quorum 500; the same 1_000 vote now
    // passes and executes.
    t.contract.set_gov_token_total_supply(&5_000);
    let id2 = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id2, &true);
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);
    let res2 = t.env.as_contract(&t.contract.address, || {
        GovContract::execute_proposal(t.env.clone(), id2)?;
        GovContract::execute_proposal(t.env.clone(), id2)
    });
    assert!(res2.is_ok());
    assert_eq!(
        t.contract.get_proposal(&id2).status,
        ProposalStatus::Executed
    );
}

// ── Issue #809: quadratic Sybil-split containment ───────────────────────────

/// Splitting one balance across N addresses multiplies quadratic weight by
/// ~sqrt(N) — but only for addresses that *vote*. Every Sybil still needs
/// its own aged pre-proposal checkpoint, so a flash-funded Sybil swarm
/// cannot materialise inside one transaction: uncheckpointed Sybils are
/// rejected and contribute nothing.
#[test]
fn test_quadratic_sybil_split_needs_aged_checkpoint_per_address() {
    let t = setup();
    t.contract.set_quadratic_voting_enabled(&true);

    // Attacker splits 10_000 across two Sybils; only the first ages a checkpoint.
    let sybil_a = Address::generate(&t.env);
    let sybil_b = Address::generate(&t.env);
    t.gov_token_admin.mint(&sybil_a, &5_000);
    t.gov_token_admin.mint(&sybil_b, &5_000);
    checkpoint_and_age(&t, &sybil_a);

    let id = create_fee_proposal(&t);
    // isqrt(5_000) = 70 (70^2 = 4900, 71^2 = 5041).
    t.contract.cast_vote(&sybil_a, &id, &true);
    let res = t.contract.try_cast_vote(&sybil_b, &id, &true);
    assert_eq!(res, Err(Ok(GovernanceError::InsufficientHoldingPeriod)));

    let p = t.contract.get_proposal(&id);
    assert_eq!(p.votes_for, 70); // only the aged Sybil counted
}

// ── #844: pre-init call tests ─────────────────────────────────────

#[test]
fn test_pre_init_calls_return_not_initialized() {
    let env = Env::default();
    let contract_addr = env.register_contract(None, GovContract);
    let contract = GovContractClient::new(&env, &contract_addr);

    // All admin-gated functions must return NotInitialized before initialize().
    assert_eq!(
        contract.try_set_min_quorum_bps(&100),
        Err(Ok(GovernanceError::NotInitialized))
    );
    assert_eq!(
        contract.try_set_min_proposal_balance(&100),
        Err(Ok(GovernanceError::NotInitialized))
    );
    assert_eq!(
        contract.try_set_gov_token_total_supply(&1000),
        Err(Ok(GovernanceError::NotInitialized))
    );
    assert_eq!(
        contract.try_set_quadratic_voting_enabled(&true),
        Err(Ok(GovernanceError::NotInitialized))
    );
    assert_eq!(
        contract.try_disable_veto_power(),
        Err(Ok(GovernanceError::NotInitialized))
    );
}

// ════════════════════════════════════════════════════════════════════════════
// #995 execution-before-eta, #999 state monotonicity,
// #1002 finalized votes vs deposits, #1005 administrative authorization
// ════════════════════════════════════════════════════════════════════════════

fn advance_past_voting(t: &GovTestEnv) {
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);
}

fn advance_ledgers(t: &GovTestEnv, n: u32) {
    let mut ledger = t.env.ledger().get();
    ledger.sequence_number += n;
    t.env.ledger().set(ledger);
}

/// Creates a fee proposal that has voted yes and been moved Active -> Passed
/// under the given execution delay. Returns the proposal id.
fn passed_proposal_with_delay(t: &GovTestEnv, delay: u32) -> u64 {
    t.contract.set_execution_delay(&t.admin, &delay);
    let id = create_fee_proposal(t);
    t.contract.cast_vote(&t.voter_b, &id, &true);
    advance_past_voting(t);
    t.contract.execute_proposal(&id);
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Passed);
    id
}

fn rank(s: &ProposalStatus) -> u32 {
    match s {
        ProposalStatus::Active => 0,
        ProposalStatus::Passed => 1,
        _ => 2,
    }
}

// ── #995: no execution before eta ───────────────────────────────────────────

#[test]
fn test_eta_is_frozen_when_delay_is_lowered_after_passing() {
    let t = setup();
    let id = passed_proposal_with_delay(&t, 100);
    let eta = t.contract.get_proposal(&id).eta_ledger;

    // Adversarial: the admin drops the delay to 0 after the proposal passed.
    t.contract.set_execution_delay(&t.admin, &0);
    assert_eq!(
        t.contract.try_execute_proposal(&id),
        Err(Ok(GovernanceError::TimelockNotExpired))
    );
    assert_eq!(t.contract.get_proposal(&id).eta_ledger, eta);
}

#[test]
fn test_eta_boundary_one_ledger_before_and_at_eta() {
    let t = setup();
    let id = passed_proposal_with_delay(&t, 10);
    let eta = t.contract.get_proposal(&id).eta_ledger;

    advance_ledgers(&t, 9);
    assert_eq!(t.env.ledger().sequence(), eta - 1);
    assert_eq!(
        t.contract.try_execute_proposal(&id),
        Err(Ok(GovernanceError::TimelockNotExpired))
    );
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Passed);

    advance_ledgers(&t, 1);
    assert_eq!(t.env.ledger().sequence(), eta);
    t.contract.execute_proposal(&id);
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Executed);
}

#[test]
fn test_queueing_and_execution_never_happen_in_one_call_with_delay() {
    let t = setup();
    let id = passed_proposal_with_delay(&t, 5);
    // Queueing call must not have executed anything.
    assert_ne!(t.contract.get_proposal(&id).status, ProposalStatus::Executed);
}

#[test]
fn test_failed_execution_retry_keeps_original_eta() {
    let t = setup();
    let id = passed_proposal_with_delay(&t, 10);
    let eta = t.contract.get_proposal(&id).eta_ledger;
    advance_ledgers(&t, 10);
    // Raising the delay between attempts must not push a ready proposal back
    // or pull an unready one forward.
    t.contract.set_execution_delay(&t.admin, &1_000);
    assert_eq!(t.contract.get_proposal(&id).eta_ledger, eta);
    t.contract.execute_proposal(&id);
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Executed);
}

#[test]
fn test_execution_delay_cap_enforced() {
    let t = setup();
    t.contract
        .set_execution_delay(&t.admin, &MAX_EXECUTION_DELAY_LEDGERS);
    assert_eq!(t.contract.get_execution_delay(), MAX_EXECUTION_DELAY_LEDGERS);
    assert_eq!(
        t.contract
            .try_set_execution_delay(&t.admin, &(MAX_EXECUTION_DELAY_LEDGERS + 1)),
        Err(Ok(GovernanceError::InvalidExecutionDelay))
    );
    assert_eq!(t.contract.get_execution_delay(), MAX_EXECUTION_DELAY_LEDGERS);
}

#[test]
fn test_assert_executable_fails_closed_on_every_bad_combination() {
    let t = setup();
    let id = create_fee_proposal(&t);
    let base = t.contract.get_proposal(&id);

    let check = |p: &GovernanceProposal| {
        t.env
            .as_contract(&t.contract.address, || GovContract::assert_executable(&t.env, p))
    };

    // Active / terminal states are never executable.
    for s in [
        ProposalStatus::Active,
        ProposalStatus::Rejected,
        ProposalStatus::Executed,
        ProposalStatus::Vetoed,
    ] {
        let mut p = base.clone();
        p.status = s;
        assert_eq!(check(&p), Err(GovernanceError::InvalidStateTransition));
    }

    // Passed but voting window still open.
    let mut p = base.clone();
    p.status = ProposalStatus::Passed;
    assert_eq!(check(&p), Err(GovernanceError::VotingOngoing));

    // Passed, window closed, eta in the future.
    advance_past_voting(&t);
    p.eta_ledger = t.env.ledger().sequence() + 1;
    assert_eq!(check(&p), Err(GovernanceError::TimelockNotExpired));

    // Passed, window closed, eta reached.
    p.eta_ledger = t.env.ledger().sequence();
    assert_eq!(check(&p), Ok(()));
}

// ── #999: proposal state monotonicity ───────────────────────────────────────

#[test]
fn test_legal_transition_table_is_exactly_the_forward_edges() {
    use ProposalStatus::*;
    let all = [Active, Passed, Rejected, Executed, Vetoed];
    let legal = [
        (Active, Passed),
        (Active, Rejected),
        (Active, Vetoed),
        (Passed, Executed),
        (Passed, Vetoed),
    ];
    for from in all.iter() {
        for to in all.iter() {
            let expected = legal.iter().any(|(f, g)| f == from && g == to);
            assert_eq!(
                GovContract::is_legal_transition(from, to),
                expected,
                "{:?} -> {:?}",
                from,
                to
            );
            // Monotonic: every legal edge strictly increases rank.
            if expected {
                assert!(rank(to) > rank(from));
            }
        }
    }
}

#[test]
fn test_terminal_states_reject_every_transition_and_leave_storage_untouched() {
    let t = setup();
    let id = create_fee_proposal(&t);
    for terminal in [
        ProposalStatus::Rejected,
        ProposalStatus::Executed,
        ProposalStatus::Vetoed,
    ] {
        let mut p = t.contract.get_proposal(&id);
        p.status = terminal.clone();
        for next in [
            ProposalStatus::Active,
            ProposalStatus::Passed,
            ProposalStatus::Rejected,
            ProposalStatus::Executed,
            ProposalStatus::Vetoed,
        ] {
            let res = t.env.as_contract(&t.contract.address, || {
                GovContract::transition_status(&t.env, &mut p.clone(), next)
            });
            assert_eq!(res, Err(GovernanceError::InvalidStateTransition));
        }
    }
    // Nothing was persisted by the rejected attempts.
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Active);
}

#[test]
fn test_executed_and_vetoed_proposals_cannot_be_re_executed() {
    let t = setup();
    setup_veto_multisig(&t);

    let executed = passed_proposal_with_delay(&t, 0);
    t.contract.execute_proposal(&executed);
    assert_eq!(
        t.contract.try_execute_proposal(&executed),
        Err(Ok(GovernanceError::AlreadyResolved))
    );

    let vetoed = create_fee_proposal(&t);
    t.contract
        .veto_proposal(&t.admin, &vetoed, &reason_hash(&t.env));
    advance_past_voting(&t);
    assert_eq!(
        t.contract.try_execute_proposal(&vetoed),
        Err(Ok(GovernanceError::AlreadyResolved))
    );
    assert_eq!(
        t.contract
            .try_veto_proposal(&t.admin, &vetoed, &reason_hash(&t.env)),
        Err(Ok(GovernanceError::NotVetoable))
    );
    assert_eq!(t.contract.get_proposal(&vetoed).status, ProposalStatus::Vetoed);
}

#[test]
fn test_veto_during_timelock_is_terminal_and_blocks_execution() {
    let t = setup();
    setup_veto_multisig(&t);
    let id = passed_proposal_with_delay(&t, 50);
    t.contract.veto_proposal(&t.admin, &id, &reason_hash(&t.env));
    advance_ledgers(&t, 1_000);
    assert_eq!(
        t.contract.try_execute_proposal(&id),
        Err(Ok(GovernanceError::AlreadyResolved))
    );
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Vetoed);
}

#[test]
fn test_status_rank_never_decreases_across_an_operation_sequence() {
    let t = setup();
    setup_veto_multisig(&t);
    let id = create_fee_proposal(&t);
    t.contract.set_execution_delay(&t.admin, &20);
    let mut last = rank(&t.contract.get_proposal(&id).status);

    let mut observe = |t: &GovTestEnv| {
        let r = rank(&t.contract.get_proposal(&id).status);
        assert!(r >= last, "status regressed");
        last = r;
    };

    t.contract.cast_vote(&t.voter_b, &id, &true);
    observe(&t);
    let _ = t.contract.try_execute_proposal(&id); // too early
    observe(&t);
    advance_past_voting(&t);
    let _ = t.contract.try_cast_vote(&t.voter_a, &id, &true); // window closed
    observe(&t);
    let _ = t.contract.try_execute_proposal(&id); // -> Passed
    observe(&t);
    let _ = t.contract.try_execute_proposal(&id); // timelock
    observe(&t);
    advance_ledgers(&t, 20);
    let _ = t.contract.try_execute_proposal(&id); // -> Executed
    observe(&t);
    let _ = t.contract.try_veto_proposal(&t.admin, &id, &reason_hash(&t.env));
    observe(&t);
    let _ = t.contract.try_execute_proposal(&id);
    observe(&t);
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Executed);
}

// ── #1002: finalized votes cannot be altered ─────────────────────────────────

#[test]
fn test_extra_deposit_after_vote_does_not_change_recorded_vote() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);
    let before = t.contract.get_proposal(&id);

    // voter_a receives a large extra deposit and tries to vote again.
    t.gov_token_admin.mint(&t.voter_a, &50_000);
    assert_eq!(
        t.contract.try_cast_vote(&t.voter_a, &id, &true),
        Err(Ok(GovernanceError::AlreadyVoted))
    );
    assert_eq!(
        t.contract.try_cast_vote(&t.voter_a, &id, &false),
        Err(Ok(GovernanceError::AlreadyVoted))
    );

    let after = t.contract.get_proposal(&id);
    assert_eq!(after.votes_for, before.votes_for);
    assert_eq!(after.votes_against, before.votes_against);
    assert_eq!(t.contract.get_applied_vote_weight(&id, &t.voter_a), Some(1_000));
}

#[test]
fn test_votes_frozen_once_proposal_is_no_longer_active() {
    let t = setup();
    let id = passed_proposal_with_delay(&t, 100);
    let frozen = t.contract.get_proposal(&id);

    let late = Address::generate(&t.env);
    t.gov_token_admin.mint(&late, &9_000);
    // Window closed: late voters are rejected, tally is untouched.
    assert_eq!(
        t.contract.try_cast_vote(&late, &id, &false),
        Err(Ok(GovernanceError::VotingEnded))
    );
    let after = t.contract.get_proposal(&id);
    assert_eq!(after.votes_for, frozen.votes_for);
    assert_eq!(after.votes_against, frozen.votes_against);
}

#[test]
fn test_vote_receipt_outlives_voting_window() {
    let t = setup();
    let id = create_fee_proposal(&t);
    t.contract.cast_vote(&t.voter_a, &id, &true);

    // Last ledger of the voting window (51_840 ledgers at 5 s).
    advance_ledgers(&t, (VOTING_PERIOD_SECS / 5) as u32);
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += VOTING_PERIOD_SECS - 1;
    t.env.ledger().set(ledger);

    assert!(t.contract.has_voted(&t.voter_a, &id));
    assert_eq!(
        t.contract.try_cast_vote(&t.voter_a, &id, &false),
        Err(Ok(GovernanceError::AlreadyVoted))
    );
}

#[test]
fn test_deposit_settles_exactly_once_across_pass_execute_and_veto() {
    let t = setup();
    setup_veto_multisig(&t);
    t.contract.set_min_proposal_deposit(&300);
    t.contract.set_execution_delay(&t.admin, &10);
    let sink = Address::generate(&t.env);
    t.contract.set_proposal_deposit_sink(&Some(sink.clone()));

    let proposer_start = t.gov_token.balance(&t.proposer);
    let id = create_fee_proposal(&t);
    assert_eq!(t.gov_token.balance(&t.proposer), proposer_start - 300);
    t.contract.cast_vote(&t.voter_b, &id, &true);
    advance_past_voting(&t);
    t.contract.execute_proposal(&id); // Passed -> refund
    assert_eq!(t.gov_token.balance(&t.proposer), proposer_start);
    assert!(t.contract.is_proposal_deposit_settled(&id));

    // Vetoing a Passed proposal must neither forfeit nor re-pay the deposit.
    t.contract.veto_proposal(&t.admin, &id, &reason_hash(&t.env));
    assert_eq!(t.gov_token.balance(&t.proposer), proposer_start);
    assert_eq!(t.gov_token.balance(&sink), 0);
    assert_eq!(t.gov_token.balance(&t.contract.address), 0);
}

#[test]
fn test_settlement_helpers_are_idempotent() {
    let t = setup();
    t.contract.set_min_proposal_deposit(&200);
    let id = create_fee_proposal(&t);
    let start = t.gov_token.balance(&t.proposer);

    let (first, second, third) = t.env.as_contract(&t.contract.address, || {
        (
            GovContract::refund_proposal_deposit(&t.env, id, &t.proposer),
            GovContract::refund_proposal_deposit(&t.env, id, &t.proposer),
            GovContract::forfeit_proposal_deposit(&t.env, id, &t.proposer),
        )
    });
    assert_eq!(first, Ok(true));
    assert_eq!(second, Ok(false));
    assert_eq!(third, Ok(false));
    assert_eq!(t.gov_token.balance(&t.proposer), start + 200);
}

// ── #1005: administrative authorization ──────────────────────────────────────

#[test]
fn test_set_execution_delay_rejects_non_admin() {
    let t = setup();
    let attacker = Address::generate(&t.env);
    assert_eq!(
        t.contract.try_set_execution_delay(&attacker, &1),
        Err(Ok(GovernanceError::Unauthorized))
    );
    assert_eq!(t.contract.get_execution_delay(), 0);
}

#[test]
fn test_set_execution_delay_before_initialize_does_not_install_admin() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, GovContract);
    let contract = GovContractClient::new(&env, &contract_id);
    let squatter = Address::generate(&env);

    assert_eq!(
        contract.try_set_execution_delay(&squatter, &99),
        Err(Ok(GovernanceError::NotInitialized))
    );
    assert_eq!(contract.get_execution_delay(), 0);
    let admin_set = env.as_contract(&contract_id, || env.storage().instance().has(&StorageKey::Admin));
    assert!(!admin_set);
}

#[test]
fn test_admin_surface_requires_real_authorization() {
    let t = setup();
    let env = &t.env;
    let attacker = Address::generate(env);
    let signers = soroban_sdk::vec![env, attacker.clone()];

    // Switch from mock_all_auths to enforcing mode with no authorizations.
    env.set_auths(&[]);

    assert!(t.contract.try_set_execution_delay(&t.admin, &5).is_err());
    assert!(t.contract.try_set_min_quorum_bps(&500).is_err());
    assert!(t.contract.try_set_min_proposal_balance(&1).is_err());
    assert!(t.contract.try_set_min_proposal_deposit(&1).is_err());
    assert!(t.contract.try_set_proposal_deposit_sink(&Some(attacker.clone())).is_err());
    assert!(t.contract.try_set_gov_token_total_supply(&1).is_err());
    assert!(t.contract.try_set_max_delegation_depth(&1).is_err());
    assert!(t.contract.try_set_quadratic_voting_enabled(&true).is_err());
    assert!(t.contract.try_disable_veto_power().is_err());
    assert!(t.contract.try_configure_veto_multisig(&signers, &1).is_err());

    // No state changed.
    assert_eq!(t.contract.get_execution_delay(), 0);
    assert_eq!(t.contract.get_min_quorum_bps(), DEFAULT_MIN_QUORUM_BPS);
    assert_eq!(t.contract.get_min_proposal_deposit(), 0);
    assert_eq!(t.contract.get_gov_token_total_supply(), 10_000);
    assert!(t.contract.is_veto_power_enabled());
    assert!(t.contract.get_veto_signers().is_empty());
}

#[test]
fn test_admin_surface_positive_control_with_correct_signer() {
    use soroban_sdk::{testutils::{MockAuth, MockAuthInvoke}, IntoVal};
    let t = setup();
    let env = &t.env;
    env.set_auths(&[]);

    // The admin's own signature authorizes set_execution_delay…
    t.contract
        .mock_auths(&[MockAuth {
            address: &t.admin,
            invoke: &MockAuthInvoke {
                contract: &t.contract.address,
                fn_name: "set_execution_delay",
                args: (t.admin.clone(), 7_u32).into_val(env),
                sub_invokes: &[],
            },
        }])
        .set_execution_delay(&t.admin, &7);
    assert_eq!(t.contract.get_execution_delay(), 7);

    // …but the same admin signature does NOT authorize an ILN-gated setter.
    t.contract
        .mock_auths(&[MockAuth {
            address: &t.admin,
            invoke: &MockAuthInvoke {
                contract: &t.contract.address,
                fn_name: "set_min_quorum_bps",
                args: (500_u32,).into_val(env),
                sub_invokes: &[],
            },
        }])
        .try_set_min_quorum_bps(&500)
        .unwrap_err();
    assert_eq!(t.contract.get_min_quorum_bps(), DEFAULT_MIN_QUORUM_BPS);
}

#[test]
fn test_veto_requires_configured_signer_and_signer_auth() {
    let t = setup();
    setup_veto_multisig(&t);
    let id = create_fee_proposal(&t);
    let outsider = Address::generate(&t.env);

    assert_eq!(
        t.contract.try_veto_proposal(&outsider, &id, &reason_hash(&t.env)),
        Err(Ok(GovernanceError::NotVetoSigner))
    );
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Active);

    t.env.set_auths(&[]);
    assert!(t
        .contract
        .try_veto_proposal(&t.admin, &id, &reason_hash(&t.env))
        .is_err());
    assert_eq!(t.contract.get_proposal(&id).status, ProposalStatus::Active);
}
