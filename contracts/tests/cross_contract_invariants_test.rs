//! Issue #855, #1087, #1088, #1089 — cross-contract system invariants.
//!
//! Spec: docs/formal-verification-cross-contract.md
//!
//! These checks deliberately read state from more than one contract after a
//! realistic multi-contract sequence. They complement per-contract proptest
//! suites, which cannot observe foreign storage.

#![cfg(test)]

extern crate std;

#[path = "mocks/mock_token.rs"]
mod mock_token;

use mock_token::{MockToken, MockTokenClient};

use iln_governance::{
    GovContract, GovContractClient, GovernanceError, ProposalAction, ProposalStatus,
};
use insurance_pool::{InsurancePool, InsurancePoolClient};
use invoice_liquidity::{InvoiceLiquidityContract, InvoiceLiquidityContractClient, ReferralCode};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;
const COVERAGE_CAP: i128 = 1_000;
const GOV_MINT: i128 = 10_000;

fn advance_ledger(env: &Env, sequence_delta: u32, timestamp_delta: u64) {
    let mut info = env.ledger().get();
    info.sequence_number += sequence_delta;
    info.timestamp += timestamp_delta;
    env.ledger().set(info);
}

fn dummy_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[1_u8; 32])
}

struct Harness {
    env: Env,
    admin: Address,
    freelancer: Address,
    payer: Address,
    lp: Address,
    payment_token: MockTokenClient<'static>,
    payment_token_addr: Address,
    iln: InvoiceLiquidityContractClient<'static>,
    iln_id: Address,
    dist: iln_distribution::IlnDistributionClient<'static>,
    pool: InsurancePoolClient<'static>,
    pool_id: Address,
    gov: GovContractClient<'static>,
    gov_id: Address,
    gov_token_addr: Address,
    /// Sum of all payment-token mints performed by this harness.
    inflows: i128,
}

impl Harness {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&env);
        let freelancer = Address::generate(&env);
        let payer = Address::generate(&env);
        let lp = Address::generate(&env);

        let payment_token_addr = env.register_contract(None, MockToken);
        let payment_token = MockTokenClient::new(&env, &payment_token_addr);

        let xlm_id = env.register_stellar_asset_contract_v2(Address::generate(&env));
        let xlm_addr = xlm_id.address();
        let eurc_addr = Address::generate(&env);

        let iln_id = env.register_contract(None, InvoiceLiquidityContract);
        let iln = InvoiceLiquidityContractClient::new(&env, &iln_id);
        iln.initialize(&admin, &payment_token_addr, &eurc_addr, &xlm_addr);

        let mut ledger = env.ledger().get();
        ledger.timestamp = 1_700_000_000;
        env.ledger().set(ledger);

        // Governance token for distribution initialize (mint authority).
        let gov_token_admin_addr = Address::generate(&env);
        let gov_token_id = env.register_stellar_asset_contract_v2(gov_token_admin_addr);
        let gov_token_addr = gov_token_id.address();
        StellarAssetClient::new(&env, &gov_token_addr).mint(&admin, &GOV_MINT);

        let dist_id = env.register_contract(None, iln_distribution::IlnDistribution);
        let dist = iln_distribution::IlnDistributionClient::new(&env, &dist_id);
        dist.initialize(&iln_id, &gov_token_addr);

        advance_ledger(&env, 200, 1_000);
        iln.set_distribution_contract(&dist_id);

        let pool_id = env.register_contract(None, InsurancePool);
        let pool = InsurancePoolClient::new(&env, &pool_id);
        pool.initialize(&iln_id, &COVERAGE_CAP, &payment_token_addr);
        advance_ledger(&env, 200, 1_000);
        iln.set_insurance_pool(&pool_id);

        let rep_bonus_addr = Address::generate(&env);
        let gov_id = env.register_contract(None, GovContract);
        let gov = GovContractClient::new(&env, &gov_id);
        gov.initialize(
            &iln_id,
            &dist_id,
            &rep_bonus_addr,
            &gov_token_addr,
            &admin,
            &20_000_i128,
        );
        gov.checkpoint_balance(&admin);
        advance_ledger(&env, 12, 100);

        Self {
            env,
            admin,
            freelancer,
            payer,
            lp,
            payment_token,
            payment_token_addr,
            iln,
            iln_id,
            dist,
            pool,
            pool_id,
            gov,
            gov_id,
            gov_token_addr,
            inflows: 0,
        }
    }

    fn mint(&mut self, to: &Address, amount: i128) {
        self.payment_token.mint(to, &amount);
        self.inflows += amount;
    }

    fn assert_conservation(&self) {
        let iln_bal = self.payment_token.balance(&self.iln_id);
        let pool_bal = self.payment_token.balance(&self.pool_id);
        let users = self.payment_token.balance(&self.lp)
            + self.payment_token.balance(&self.freelancer)
            + self.payment_token.balance(&self.payer)
            + self.payment_token.balance(&self.admin);
        assert_eq!(
            iln_bal + pool_bal + users,
            self.inflows,
            "X4: ILN escrow + insurance token balance + user balances must equal minted inflows"
        );
    }
}

#[test]
fn x1_funding_increases_lp_distribution_accrual() {
    let mut h = Harness::new();
    h.mint(&h.lp.clone(), INVOICE_AMOUNT * 2);

    let before = h.dist.get_accrual(&h.lp);
    let due = h.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let id = h.iln.submit_invoice(
        &h.freelancer,
        &h.payer,
        &INVOICE_AMOUNT,
        &due,
        &DISCOUNT_RATE,
        &h.payment_token_addr,
        &ReferralCode::None,
    );
    h.iln
        .fund_invoice(&h.lp, &id, &INVOICE_AMOUNT, &false);
    let after = h.dist.get_accrual(&h.lp);
    assert!(
        after > before,
        "X1: fund_invoice must increase LP distribution accrual when wired (before={before}, after={after})"
    );
    h.assert_conservation();
}

#[test]
fn x2_settlement_increases_freelancer_distribution_accrual() {
    let mut h = Harness::new();
    h.mint(&h.lp.clone(), INVOICE_AMOUNT * 2);

    let due = h.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let id = h.iln.submit_invoice(
        &h.freelancer,
        &h.payer,
        &INVOICE_AMOUNT,
        &due,
        &DISCOUNT_RATE,
        &h.payment_token_addr,
        &ReferralCode::None,
    );
    h.iln
        .fund_invoice(&h.lp, &id, &INVOICE_AMOUNT, &false);

    let before = h.dist.get_accrual(&h.freelancer);
    h.iln.mark_paid(&id, &INVOICE_AMOUNT);
    let after = h.dist.get_accrual(&h.freelancer);
    assert!(
        after > before,
        "X2: mark_paid must increase freelancer distribution accrual when wired (before={before}, after={after})"
    );
    h.assert_conservation();
}

#[test]
fn x3_default_claim_reduces_insurance_pool_by_payout() {
    let mut h = Harness::new();
    h.mint(&h.lp.clone(), INVOICE_AMOUNT * 2 + 600);
    h.pool.deposit_premium(&h.lp, &600);
    assert!(h.pool.is_enrolled(&h.lp));
    let pool_before = h.pool.get_pool_balance();
    assert_eq!(pool_before, 600);

    let due = h.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let id = h.iln.submit_invoice(
        &h.freelancer,
        &h.payer,
        &INVOICE_AMOUNT,
        &due,
        &DISCOUNT_RATE,
        &h.payment_token_addr,
        &ReferralCode::None,
    );
    h.iln
        .fund_invoice(&h.lp, &id, &INVOICE_AMOUNT, &false);

    let mut info = h.env.ledger().get();
    info.timestamp = due + 1;
    h.env.ledger().set(info);

    h.iln.claim_default(&h.lp, &id);
    let pool_after = h.pool.get_pool_balance();
    assert!(
        pool_after <= pool_before,
        "X3: pool balance must not increase on claim_default"
    );
    assert!(pool_after >= 0, "X3: pool balance must stay non-negative");
    // Enrolled + solvent → some compensation should have been paid for this fixture.
    assert!(
        pool_after < pool_before,
        "X3: enrolled solvent pool should pay out on claim_default (before={pool_before}, after={pool_after})"
    );
    h.assert_conservation();
}

#[test]
fn x4_payment_token_conserved_across_iln_and_insurance() {
    let mut h = Harness::new();
    h.mint(&h.lp.clone(), INVOICE_AMOUNT * 2 + 600);
    h.pool.deposit_premium(&h.lp, &600);

    let due = h.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let id = h.iln.submit_invoice(
        &h.freelancer,
        &h.payer,
        &INVOICE_AMOUNT,
        &due,
        &DISCOUNT_RATE,
        &h.payment_token_addr,
        &ReferralCode::None,
    );
    h.iln
        .fund_invoice(&h.lp, &id, &INVOICE_AMOUNT, &false);
    h.assert_conservation();

    h.iln.mark_paid(&id, &INVOICE_AMOUNT);
    h.assert_conservation();
}

#[test]
fn x5_insurance_compensation_not_paid_twice_for_same_invoice() {
    let mut h = Harness::new();
    h.mint(&h.lp.clone(), INVOICE_AMOUNT * 2 + 600);
    h.pool.deposit_premium(&h.lp, &600);

    let due = h.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let id = h.iln.submit_invoice(
        &h.freelancer,
        &h.payer,
        &INVOICE_AMOUNT,
        &due,
        &DISCOUNT_RATE,
        &h.payment_token_addr,
        &ReferralCode::None,
    );
    h.iln
        .fund_invoice(&h.lp, &id, &INVOICE_AMOUNT, &false);

    let mut info = h.env.ledger().get();
    info.timestamp = due + 1;
    h.env.ledger().set(info);

    h.iln.claim_default(&h.lp, &id);
    let pool_after_first = h.pool.get_pool_balance();

    // Second claim_default must not drain the pool again.
    let second = h.iln.try_claim_default(&h.lp, &id);
    assert!(
        second.is_err(),
        "X5: second claim_default for the same invoice must fail"
    );
    assert_eq!(
        h.pool.get_pool_balance(),
        pool_after_first,
        "X5: pool balance must be unchanged after a rejected re-claim"
    );
    h.assert_conservation();
}

#[test]
fn x6_governance_execution_not_before_eta_invariant() {
    let mut h = Harness::new();

    // Configure execution timelock delay = 50 ledgers
    h.gov.set_execution_delay(&h.admin, &50);
    assert_eq!(h.gov.get_execution_delay(), 50);

    // Initial state: fee rate on ILN is 0
    assert_eq!(h.iln.get_fee_rate(), 0);

    // Create proposal targeting cross-contract update_fee_rate(400)
    let prop_id = h.gov.create_proposal(
        &h.admin,
        &ProposalAction::UpdateFeeRate(400),
        &dummy_hash(&h.env),
        &400_i128,
    );

    // Admin casts vote FOR (holds 10_000 / 20_000 = 50% > 10% quorum)
    h.gov.cast_vote(&h.admin, &prop_id, &true);

    // Guard 1: Cannot execute before voting_end (VotingOngoing)
    let res_early = h.gov.try_execute_proposal(&prop_id);
    assert_eq!(res_early, Err(Ok(GovernanceError::VotingOngoing)));
    assert_eq!(h.iln.get_fee_rate(), 0);

    // Advance ledger past the 3-day voting window (259_200 s)
    // Also advance past rate-limit cooldown
    advance_ledger(&h.env, 100, 259_201);

    // First execute_proposal call: Active -> Passed
    h.gov.execute_proposal(&prop_id);
    let prop_passed = h.gov.get_proposal(&prop_id);
    assert_eq!(prop_passed.status, ProposalStatus::Passed);
    let eta = prop_passed.eta_ledger;
    assert_eq!(eta, h.env.ledger().sequence() + 50);

    // Cross-contract call has NOT executed yet
    assert_eq!(h.iln.get_fee_rate(), 0);

    // Guard 2: Timelock boundary check at eta - 1 (TimelockNotExpired)
    let cur_seq = h.env.ledger().sequence();
    if cur_seq < eta - 1 {
        advance_ledger(&h.env, (eta - 1) - cur_seq, 10);
    }
    assert_eq!(h.env.ledger().sequence(), eta - 1);
    let res_boundary = h.gov.try_execute_proposal(&prop_id);
    assert_eq!(res_boundary, Err(Ok(GovernanceError::TimelockNotExpired)));
    assert_eq!(h.iln.get_fee_rate(), 0);

    // Guard 3: At current_ledger == eta, execution succeeds! Passed -> Executed
    advance_ledger(&h.env, 1, 10);
    assert_eq!(h.env.ledger().sequence(), eta);
    h.gov.execute_proposal(&prop_id);

    let prop_executed = h.gov.get_proposal(&prop_id);
    assert_eq!(prop_executed.status, ProposalStatus::Executed);

    // Cross-contract action took effect on ILN
    assert_eq!(h.iln.get_fee_rate(), 400);

    // Guard 4: Cannot re-execute once Executed (AlreadyResolved)
    let res_repeat = h.gov.try_execute_proposal(&prop_id);
    assert_eq!(res_repeat, Err(Ok(GovernanceError::AlreadyResolved)));
    assert_eq!(h.iln.get_fee_rate(), 400);
}

#[test]
fn x7_governance_veto_transition_safety() {
    let mut h = Harness::new();

    // Configure veto multisig with admin as sole signer
    let signers = soroban_sdk::vec![&h.env, h.admin.clone()];
    h.gov.configure_veto_multisig(&signers, &1);
    assert_eq!(h.gov.get_veto_signers().len(), 1);

    // Case 1: Vetoing an Active proposal
    let prop_active = h.gov.create_proposal(
        &h.admin,
        &ProposalAction::UpdateFeeRate(300),
        &dummy_hash(&h.env),
        &300_i128,
    );
    h.gov.veto_proposal(&h.admin, &prop_active, &dummy_hash(&h.env));
    let prop1 = h.gov.get_proposal(&prop_active);
    assert_eq!(prop1.status, ProposalStatus::Vetoed);

    // Subsequent vote on vetoed proposal rejected
    let vote_res = h.gov.try_cast_vote(&h.admin, &prop_active, &true);
    assert_eq!(vote_res, Err(Ok(GovernanceError::ProposalNotActive)));

    // Subsequent execute on vetoed proposal rejected
    let exec_res1 = h.gov.try_execute_proposal(&prop_active);
    assert_eq!(exec_res1, Err(Ok(GovernanceError::AlreadyResolved)));
    assert_eq!(h.iln.get_fee_rate(), 0);

    // Case 2: Vetoing a Passed proposal during timelock delay
    h.gov.set_execution_delay(&h.admin, &100);
    let prop_passed = h.gov.create_proposal(
        &h.admin,
        &ProposalAction::UpdateFeeRate(350),
        &dummy_hash(&h.env),
        &350_i128,
    );
    h.gov.cast_vote(&h.admin, &prop_passed, &true);
    advance_ledger(&h.env, 100, 259_201);
    h.gov.execute_proposal(&prop_passed); // Active -> Passed
    assert_eq!(h.gov.get_proposal(&prop_passed).status, ProposalStatus::Passed);

    // Veto while Passed
    h.gov.veto_proposal(&h.admin, &prop_passed, &dummy_hash(&h.env));
    assert_eq!(h.gov.get_proposal(&prop_passed).status, ProposalStatus::Vetoed);

    // Advance past timelock eta: execute still rejected
    advance_ledger(&h.env, 200, 1000);
    let exec_res2 = h.gov.try_execute_proposal(&prop_passed);
    assert_eq!(exec_res2, Err(Ok(GovernanceError::AlreadyResolved)));
    assert_eq!(h.iln.get_fee_rate(), 0);

    // Case 3: Terminal states reject veto (NotVetoable)
    let veto_terminal = h.gov.try_veto_proposal(&h.admin, &prop_passed, &dummy_hash(&h.env));
    assert_eq!(veto_terminal, Err(Ok(GovernanceError::NotVetoable)));

    // Case 4: Veto power disablement
    h.gov.disable_veto_power();
    assert_eq!(h.gov.is_veto_power_enabled(), false);

    let prop_unvetoable = h.gov.create_proposal(
        &h.admin,
        &ProposalAction::UpdateFeeRate(380),
        &dummy_hash(&h.env),
        &380_i128,
    );
    let veto_disabled = h.gov.try_veto_proposal(&h.admin, &prop_unvetoable, &dummy_hash(&h.env));
    assert_eq!(veto_disabled, Err(Ok(GovernanceError::VetoPowerDisabled)));
}

#[test]
fn x8_governance_proposal_lifecycle_authorization() {
    let mut h = Harness::new();

    // 1. Proposer authorization: caller with insufficient token balance rejected
    let poor_user = Address::generate(&h.env);
    let res_proposer = h.gov.try_create_proposal(
        &poor_user,
        &ProposalAction::UpdateFeeRate(200),
        &dummy_hash(&h.env),
        &200_i128,
    );
    assert_eq!(
        res_proposer,
        Err(Ok(GovernanceError::InsufficientProposerBalance))
    );

    // 2. Proposer with sufficient balance succeeds
    let prop_id = h.gov.create_proposal(
        &h.admin,
        &ProposalAction::UpdateFeeRate(200),
        &dummy_hash(&h.env),
        &200_i128,
    );
    assert_eq!(prop_id, 1);

    // 3. Voter authorization & anti-double-vote
    h.gov.cast_vote(&h.admin, &prop_id, &true);
    let res_double = h.gov.try_cast_vote(&h.admin, &prop_id, &true);
    assert_eq!(res_double, Err(Ok(GovernanceError::AlreadyVoted)));

    // 4. Holding period requirement (Issue #805)
    let new_voter = Address::generate(&h.env);
    StellarAssetClient::new(&h.env, &h.gov_token_addr).mint(&new_voter, &2_000);
    // new_voter has not checkpointed balance at least MIN_VOTE_HOLD_LEDGERS prior
    let res_unheld = h.gov.try_cast_vote(&new_voter, &prop_id, &true);
    assert_eq!(res_unheld, Err(Ok(GovernanceError::InsufficientHoldingPeriod)));

    // 5. Gated configuration functions: parameter bounds check
    let res_invalid_quorum = h.gov.try_set_min_quorum_bps(&0);
    assert_eq!(res_invalid_quorum, Err(Ok(GovernanceError::InvalidQuorumBps)));
    let res_invalid_quorum_high = h.gov.try_set_min_quorum_bps(&10_001);
    assert_eq!(res_invalid_quorum_high, Err(Ok(GovernanceError::InvalidQuorumBps)));

    // 6. Non-signer cannot veto proposal
    let non_signer = Address::generate(&h.env);
    let res_non_signer = h.gov.try_veto_proposal(&non_signer, &prop_id, &dummy_hash(&h.env));
    assert!(res_non_signer.is_err());
}
