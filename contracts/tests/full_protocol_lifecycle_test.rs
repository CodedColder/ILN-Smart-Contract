//! Issue #706 / #1161 — full protocol lifecycle test spanning all five
//! contracts, built on a hermetic, resettable fixture.
//!
//! The journey exercises one realistic flow across every contract in the
//! workspace and verifies their state stays mutually consistent:
//!
//!   1. `invoice_liquidity` (ILN) — invoice submission, funding, settlement.
//!   2. `iln_governance` — executes a real proposal (UpdateReputationBonusParams)
//!      through propose -> vote -> timelock -> execute.
//!   3. `iln_distribution` — accrues LP/freelancer/payer rewards through ILN's
//!      funding/settlement hooks, then pays out on claim.
//!   4. `reputation_bonus` — governed by `iln_governance` (Issue #704).
//!   5. `insurance_pool` — an LP enrolls via `deposit_premium`; the pool stays
//!      wired to ILN throughout.
//!
//! # Fixture contract (Issue #1161)
//!
//! Every scenario builds its own [`Protocol`] and nothing else, so no scenario
//! can observe another's state:
//!
//! * **Fresh `Env` per fixture.** Contract storage, auth records, events and
//!   address generators live in the `Env`; none of it is shared or static.
//! * **Pinned ledger.** Timestamp and sequence are set explicitly before any
//!   contract call, so rate limits, holding periods and timelocks never depend
//!   on a default or on wall-clock time.
//! * **Testnet-independent balances.** The payment token is the in-tree
//!   `MockToken`; the only source of balance is the explicit `mint` calls in
//!   [`Protocol::new`] and [`Protocol::enroll_lp`]. No network, faucet,
//!   account, friendbot or environment variable is read.
//! * **No on-disk side effects.** soroban-sdk writes a `test_snapshots/*.json`
//!   file when an `Env` drops unless `capture_snapshot_at_drop` is disabled.
//!   The fixture disables it, so running the suite never dirties the tree
//!   (previously this test rewrote a tracked snapshot, which had also gone
//!   stale — see `fixture_writes_no_snapshot_file`).
//! * **Closed supply.** Every holder of the payment token is enumerated in
//!   [`Protocol::holders`], so conservation is checkable at any step.
//!
//! See `docs/integration-test-fixtures.md` for the review notes.

extern crate std;

#[path = "mocks/mock_token.rs"]
mod mock_token;

use mock_token::{MockToken, MockTokenClient};

use iln_distribution::{IlnDistribution, IlnDistributionClient};
use iln_governance::{
    GovContract, GovContractClient, GovernanceError, ProposalAction, ProposalStatus,
};
use insurance_pool::{InsurancePool, InsurancePoolClient};
use invoice_liquidity::{
    ContractError, InvoiceLiquidityContract, InvoiceLiquidityContractClient, ReferralCode,
};
use reputation_bonus::{
    config::Config as RepBonusConfig, ReputationBonusContract, ReputationBonusContractClient,
};
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000; // 100 USDC (7-decimal)
const DISCOUNT_RATE: u32 = 300; // 3%
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30; // 30 days
const GOV_TOTAL_SUPPLY: i128 = 20_000;
const COVERAGE_CAP: i128 = 1_000;
const VOTING_PERIOD_SECS: u64 = 259_200; // mirrors iln_governance's own constant

/// Pinned starting ledger.
const T0: u64 = 1_700_000_000;
const SEQ0: u32 = 10_000;

/// Setup advances (sequence, seconds); each clears ILN's admin-action rate
/// limits or ages the governance voter past its holding period (Issue #805).
const ADVANCE_DISTRIBUTION: (u32, u64) = (200, 1_000);
const ADVANCE_HOLDING: (u32, u64) = (11, 55);
const ADVANCE_POOL: (u32, u64) = (200, 1_000);

/// The only sources of payment-token balance in any scenario.
const PAYER_MINT: i128 = INVOICE_AMOUNT;
const LP_MINT: i128 = INVOICE_AMOUNT * 2;
const PREMIUM: i128 = 600;
const VOTER_GOV_MINT: i128 = 3_000; // exceeds 10% quorum on GOV_TOTAL_SUPPLY

/// What an LP pays (and a freelancer receives) for a full-invoice funding.
const FUNDING_COST: i128 = INVOICE_AMOUNT - INVOICE_AMOUNT * DISCOUNT_RATE as i128 / 10_000;

fn dummy_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[7u8; 32])
}

/// Advances both ledger sequence and timestamp together.
fn advance_ledger(env: &Env, sequence_delta: u32, timestamp_delta: u64) {
    let mut info = env.ledger().get();
    info.sequence_number += sequence_delta;
    info.timestamp += timestamp_delta;
    env.ledger().set(info);
}

/// A complete, freshly deployed five-contract protocol.
struct Protocol {
    env: Env,
    admin: Address,
    voter: Address,
    freelancer: Address,
    payer: Address,
    lp: Address,
    token: MockTokenClient<'static>,
    iln: InvoiceLiquidityContractClient<'static>,
    dist: IlnDistributionClient<'static>,
    governance: GovContractClient<'static>,
    rep: ReputationBonusContractClient<'static>,
    pool: InsurancePoolClient<'static>,
}

impl Protocol {
    /// Deploy and wire all five contracts from scratch. Deterministic: two
    /// calls produce byte-for-byte identical initial state.
    fn new() -> Self {
        let env = Env::new_with_config(EnvTestConfig {
            // No `test_snapshots/*.json` written when the Env drops.
            capture_snapshot_at_drop: false,
        });
        env.mock_all_auths_allowing_non_root_auth();

        // Pin the ledger before any contract call.
        let mut info = env.ledger().get();
        info.timestamp = T0;
        info.sequence_number = SEQ0;
        env.ledger().set(info);

        // ── Roles ────────────────────────────────────────────────────────
        let admin = Address::generate(&env);
        let voter = Address::generate(&env);
        let freelancer = Address::generate(&env);
        let payer = Address::generate(&env);
        let lp = Address::generate(&env);

        // ── Payment token: in-tree mock, balances only from explicit mints
        let token_id = env.register(MockToken, ());
        let token = MockTokenClient::new(&env, &token_id);
        token.mint(&lp, &LP_MINT);
        token.mint(&payer, &PAYER_MINT);

        // ── 1. invoice_liquidity ─────────────────────────────────────────
        let xlm_addr = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        let eurc_addr = Address::generate(&env);
        let iln_id = env.register(InvoiceLiquidityContract, ());
        let iln = InvoiceLiquidityContractClient::new(&env, &iln_id);
        iln.initialize(&admin, &token_id, &eurc_addr, &xlm_addr);

        // ── 3. iln_distribution ──────────────────────────────────────────
        // The governance token's SAC must be administered by the distribution
        // contract itself (`initialize` enforces it, Issue #861), so the
        // distribution contract is registered before the token it administers.
        let dist_id = env.register(IlnDistribution, ());
        let gov_token_addr = env
            .register_stellar_asset_contract_v2(dist_id.clone())
            .address();
        StellarAssetClient::new(&env, &gov_token_addr).mint(&voter, &VOTER_GOV_MINT);
        let dist = IlnDistributionClient::new(&env, &dist_id);
        dist.initialize(&iln_id, &gov_token_addr);

        advance_ledger(&env, ADVANCE_DISTRIBUTION.0, ADVANCE_DISTRIBUTION.1);
        iln.set_distribution_contract(&dist_id);

        // ── 4. reputation_bonus ──────────────────────────────────────────
        let rep_id = env.register(ReputationBonusContract, ());
        let rep = ReputationBonusContractClient::new(&env, &rep_id);

        // ── 2. iln_governance ────────────────────────────────────────────
        let governance_id = env.register(GovContract, ());
        let governance = GovContractClient::new(&env, &governance_id);
        governance.initialize(
            &iln_id,
            &dist_id,
            &rep_id,
            &gov_token_addr,
            &admin,
            &GOV_TOTAL_SUPPLY,
        );

        // Issue #805: checkpoint the voter and age it past the holding period.
        governance.checkpoint_balance(&voter);
        advance_ledger(&env, ADVANCE_HOLDING.0, ADVANCE_HOLDING.1);

        // reputation_bonus's admin is governance's own address (Issue #704).
        rep.init(&governance_id);
        rep.set_config(&RepBonusConfig {
            high_rep_threshold: 700,
            bonus_bps: 100,
            min_discount_rate_bps: 50,
        });

        // ── 5. insurance_pool (wired; LP enrolls later via `enroll_lp`) ──
        let pool_id = env.register(InsurancePool, ());
        let pool = InsurancePoolClient::new(&env, &pool_id);
        pool.initialize(&iln_id, &COVERAGE_CAP, &token_id);
        advance_ledger(&env, ADVANCE_POOL.0, ADVANCE_POOL.1);
        iln.set_insurance_pool(&pool_id);

        Protocol {
            env,
            admin,
            voter,
            freelancer,
            payer,
            lp,
            token,
            iln,
            dist,
            governance,
            rep,
            pool,
        }
    }

    /// Every address that can hold the payment token.
    fn holders(&self) -> [Address; 9] {
        [
            self.admin.clone(),
            self.voter.clone(),
            self.freelancer.clone(),
            self.payer.clone(),
            self.lp.clone(),
            self.iln.address.clone(),
            self.dist.address.clone(),
            self.governance.address.clone(),
            self.pool.address.clone(),
        ]
    }

    fn balances(&self) -> std::vec::Vec<i128> {
        self.holders().iter().map(|a| self.token.balance(a)).collect()
    }

    fn total_supply(&self) -> i128 {
        self.balances().iter().sum()
    }

    fn bal(&self, a: &Address) -> i128 {
        self.token.balance(a)
    }

    /// Total payment token that has ever been minted in this fixture.
    fn minted(with_premium: bool) -> i128 {
        PAYER_MINT + LP_MINT + if with_premium { PREMIUM } else { 0 }
    }

    // ── Journey steps ────────────────────────────────────────────────────

    /// LP mints and deposits a premium, which auto-enrolls it in the pool.
    fn enroll_lp(&self) {
        self.token.mint(&self.lp, &PREMIUM);
        self.pool.deposit_premium(&self.lp, &PREMIUM);
    }

    fn submit(&self) -> u64 {
        self.iln.submit_invoice(
            &self.freelancer,
            &self.payer,
            &INVOICE_AMOUNT,
            &(self.env.ledger().timestamp() + DUE_DATE_OFFSET),
            &DISCOUNT_RATE,
            &self.token.address,
            &ReferralCode::None,
        )
    }

    /// Funding triggers ILN's `notify_distribution_funding`.
    fn fund(&self, id: u64) {
        self.iln.fund_invoice(&self.lp, &id, &INVOICE_AMOUNT, &false);
    }

    /// Settlement triggers ILN's `notify_distribution_settlement`.
    fn settle(&self, id: u64) {
        self.iln.mark_paid(&id, &INVOICE_AMOUNT);
    }

    fn propose(&self) -> u64 {
        let id = self.governance.create_proposal(
            &self.voter,
            &ProposalAction::UpdateReputationBonusParams(800, 150, 75),
            &dummy_hash(&self.env),
            &7_500_000_i128,
        );
        self.governance.cast_vote(&self.voter, &id, &true);
        id
    }

    /// Active -> Passed.
    fn pass(&self, id: u64) {
        advance_ledger(&self.env, 0, VOTING_PERIOD_SECS + 1);
        self.governance.execute_proposal(&id);
    }

    /// Passed -> Executed.
    fn execute(&self, id: u64) {
        advance_ledger(&self.env, 1_000, 0);
        self.governance.execute_proposal(&id);
    }

    fn rep_config(&self) -> (u32, u32, u32) {
        let c = self.rep.get_config();
        (c.high_rep_threshold, c.bonus_bps, c.min_discount_rate_bps)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Scenario 1 — the full journey, now with token-ledger assertions
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn test_full_protocol_lifecycle_across_all_five_contracts() {
    let p = Protocol::new();

    p.enroll_lp();
    assert!(p.pool.is_enrolled(&p.lp));
    let supply = Protocol::minted(true);
    assert_eq!(p.total_supply(), supply);

    // ── Invoicing journey ────────────────────────────────────────────────
    let invoice_id = p.submit();

    p.fund(invoice_id);
    assert!(
        p.dist.get_accrual(&p.lp) > 0,
        "LP funding must accrue a distribution reward"
    );
    assert_eq!(p.bal(&p.lp), LP_MINT - FUNDING_COST, "LP paid its discounted cost");
    assert_eq!(p.bal(&p.freelancer), FUNDING_COST, "freelancer was paid up front");
    assert_eq!(p.bal(&p.iln.address), 0, "single funder: ILN keeps nothing");
    assert_eq!(p.total_supply(), supply);

    p.settle(invoice_id);
    let freelancer_accrual_before_claim = p.dist.get_accrual(&p.freelancer);
    assert!(
        freelancer_accrual_before_claim > 0,
        "on-time settlement must accrue a freelancer reward"
    );
    assert_eq!(p.bal(&p.payer), 0, "payer paid the full invoice exactly once");
    assert_eq!(
        p.bal(&p.lp),
        LP_MINT - FUNDING_COST + INVOICE_AMOUNT,
        "LP received the full invoice amount (no protocol fee configured)"
    );
    assert_eq!(p.bal(&p.iln.address), 0);
    assert_eq!(p.total_supply(), supply, "settlement conserves the supply");

    let lp_accrual_before_claim = p.dist.get_accrual(&p.lp);
    let lp_claimed = p.dist.claim_tokens(&p.lp);
    assert!(lp_claimed > 0);
    assert_eq!(lp_claimed, lp_accrual_before_claim);
    // get_accrual reports lifetime total_earned (not an unclaimed balance),
    // so a second claim of the same already-claimed amount must return 0.
    assert_eq!(
        p.dist.claim_tokens(&p.lp),
        0,
        "re-claiming already-claimed tokens must be a no-op"
    );
    assert_eq!(p.total_supply(), supply, "reward claims mint governance token only");

    // ── Governance touches reputation_bonus (Issue #704's wiring) ───────
    let proposal_id = p.propose();
    p.pass(proposal_id);
    p.execute(proposal_id);

    // ── Final cross-contract consistency check ──────────────────────────
    assert_eq!(
        p.governance.get_proposal(&proposal_id).status,
        ProposalStatus::Executed
    );
    assert_eq!(p.rep_config(), (800, 150, 75));

    assert!(
        p.pool.is_enrolled(&p.lp),
        "insurance enrollment must survive the whole journey"
    );
    assert_eq!(p.pool.get_pool_balance(), PREMIUM);
    assert_eq!(p.bal(&p.pool.address), PREMIUM, "pool tokens match its accounting");

    // ILN itself is still live and functional after everything above.
    let second_invoice_id = p.submit();
    assert_ne!(second_invoice_id, invoice_id);
}

// ───────────────────────────────────────────────────────────────────────────
// Scenario 2 — fixtures are pristine, deterministic and isolated
// ───────────────────────────────────────────────────────────────────────────

/// A fixture starts from exactly the documented state: pinned ledger, balances
/// only from explicit mints, and every contract's counters at zero. Nothing
/// depends on a network, faucet or pre-existing account.
#[test]
fn fixture_initial_state_is_pinned_and_testnet_independent() {
    let p = Protocol::new();

    let (seq, secs) = (
        ADVANCE_DISTRIBUTION.0 + ADVANCE_HOLDING.0 + ADVANCE_POOL.0,
        ADVANCE_DISTRIBUTION.1 + ADVANCE_HOLDING.1 + ADVANCE_POOL.1,
    );
    assert_eq!(p.env.ledger().timestamp(), T0 + secs);
    assert_eq!(p.env.ledger().sequence(), SEQ0 + seq);

    // Balances come only from the explicit mints.
    assert_eq!(p.bal(&p.payer), PAYER_MINT);
    assert_eq!(p.bal(&p.lp), LP_MINT);
    for (name, who) in [
        ("admin", &p.admin),
        ("voter", &p.voter),
        ("freelancer", &p.freelancer),
        ("iln", &p.iln.address),
        ("dist", &p.dist.address),
        ("governance", &p.governance.address),
        ("pool", &p.pool.address),
    ] {
        assert_eq!(p.bal(who), 0, "{name} must start empty");
    }
    assert_eq!(p.total_supply(), Protocol::minted(false), "closed supply");
    assert_eq!(p.token.decimals(), 7, "in-tree mock token, not a network asset");

    // Every contract's own state is empty.
    assert_eq!(p.iln.get_invoice_count(), 0);
    let stats = p.iln.get_contract_stats();
    assert_eq!((stats.total_funded, stats.total_paid), (0, 0));
    for who in [&p.admin, &p.voter, &p.freelancer, &p.payer, &p.lp] {
        assert_eq!(p.dist.get_accrual(who), 0);
    }
    assert_eq!(p.pool.get_pool_balance(), 0);
    assert!(!p.pool.is_enrolled(&p.lp));
    assert_eq!(p.rep_config(), (700, 100, 50));
}

/// Running a whole journey in one fixture must not change a sibling fixture,
/// and a fixture built *after* a dirty one must equal one built before it.
#[test]
fn fixtures_share_no_state_and_reset_deterministically() {
    let before = Protocol::new();
    let pristine_balances = before.balances();
    let pristine_ledger = (before.env.ledger().timestamp(), before.env.ledger().sequence());

    let dirty = Protocol::new();
    dirty.enroll_lp();
    let dirty_id = dirty.submit();
    dirty.fund(dirty_id);
    dirty.settle(dirty_id);
    let gov_id = dirty.propose();
    dirty.pass(gov_id);
    dirty.execute(gov_id);
    assert!(dirty.dist.get_accrual(&dirty.lp) > 0);
    assert_eq!(dirty.rep_config(), (800, 150, 75));

    // A sibling built before the dirty run is untouched by it.
    assert_eq!(before.balances(), pristine_balances);
    assert_eq!(before.iln.get_invoice_count(), 0);
    assert_eq!(before.dist.get_accrual(&before.lp), 0);
    assert_eq!(before.pool.get_pool_balance(), 0);
    assert_eq!(before.rep_config(), (700, 100, 50));
    assert_eq!(
        (before.env.ledger().timestamp(), before.env.ledger().sequence()),
        pristine_ledger
    );

    // A fixture built after the dirty run starts identical to the pristine one.
    let after = Protocol::new();
    assert_eq!(after.balances(), pristine_balances);
    assert_eq!(
        (after.env.ledger().timestamp(), after.env.ledger().sequence()),
        pristine_ledger
    );
    assert_eq!(after.iln.get_invoice_count(), 0);
    assert_eq!(after.rep_config(), (700, 100, 50));
    // Deterministic ids: the first invoice is numbered identically everywhere.
    assert_eq!(after.submit(), dirty_id);
    assert_eq!(before.submit(), dirty_id);
}

/// Dropping a fixture must not leave a `test_snapshots/*.json` behind. The
/// SDK writes one per `Env` by default; that silently dirties the working
/// tree and had left a stale tracked snapshot for this suite.
#[test]
fn fixture_writes_no_snapshot_file() {
    let snapshot = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test_snapshots")
        .join("fixture_writes_no_snapshot_file.1.json");
    {
        let p = Protocol::new();
        p.enroll_lp();
        let id = p.submit();
        p.fund(id);
    } // Env dropped here
    assert!(
        !snapshot.exists(),
        "fixture must disable capture_snapshot_at_drop; found {}",
        snapshot.display()
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Scenario 3 — repeated / stale / interrupted steps leave state untouched
// ───────────────────────────────────────────────────────────────────────────

/// After a completed journey, replaying any step fails (or is a no-op) and
/// changes no balance, accrual, status or parameter.
#[test]
fn replayed_steps_after_completion_change_nothing() {
    let p = Protocol::new();
    p.enroll_lp();
    let invoice_id = p.submit();
    p.fund(invoice_id);
    p.settle(invoice_id);
    let claimed = p.dist.claim_tokens(&p.lp);
    assert!(claimed > 0);
    let proposal_id = p.propose();
    p.pass(proposal_id);
    p.execute(proposal_id);

    let balances = p.balances();
    let lp_accrual = p.dist.get_accrual(&p.lp);
    let freelancer_accrual = p.dist.get_accrual(&p.freelancer);
    let config = p.rep_config();

    // Stale settlement and funding.
    assert_eq!(
        p.iln.try_mark_paid(&invoice_id, &1),
        Err(Ok(ContractError::AlreadyPaid))
    );
    assert!(p.iln.try_fund_invoice(&p.lp, &invoice_id, &1, &false).is_err());
    // Repeated reward claim and proposal execution.
    assert_eq!(p.dist.claim_tokens(&p.lp), 0);
    assert_eq!(
        p.governance.try_execute_proposal(&proposal_id),
        Err(Ok(GovernanceError::ProposalAlreadyExecuted))
    );

    assert_eq!(p.balances(), balances);
    assert_eq!(p.dist.get_accrual(&p.lp), lp_accrual);
    assert_eq!(p.dist.get_accrual(&p.freelancer), freelancer_accrual);
    assert_eq!(p.rep_config(), config);
    assert_eq!(
        p.governance.get_proposal(&proposal_id).status,
        ProposalStatus::Executed
    );
}

/// An interrupted governance step (execution attempted while voting is still
/// open) is rejected without side effects, and the proposal then completes
/// normally once the window closes.
#[test]
fn early_governance_execution_is_rejected_then_succeeds_after_window() {
    let p = Protocol::new();
    let balances = p.balances();
    let proposal_id = p.propose();

    assert_eq!(
        p.governance.try_execute_proposal(&proposal_id),
        Err(Ok(GovernanceError::VotingOngoing))
    );
    assert_eq!(
        p.governance.get_proposal(&proposal_id).status,
        ProposalStatus::Active
    );
    assert_eq!(p.rep_config(), (700, 100, 50), "parameters unchanged");
    assert_eq!(p.balances(), balances, "payment token untouched");

    p.pass(proposal_id);
    p.execute(proposal_id);
    assert_eq!(
        p.governance.get_proposal(&proposal_id).status,
        ProposalStatus::Executed
    );
    assert_eq!(p.rep_config(), (800, 150, 75));
}
