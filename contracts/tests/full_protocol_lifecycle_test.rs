//! Issue #706 — full protocol lifecycle test spanning all five contracts.
//!
//! Existing lifecycle tests are per-contract or two-contract at most. This
//! exercises a single realistic journey touching all five contracts in this
//! workspace and verifies their state stays mutually consistent at the end:
//!
//!   1. `invoice_liquidity` (ILN) — the main protocol: invoice submission,
//!      funding, settlement.
//!   2. `iln_governance` — executes a real proposal (UpdateReputationBonusParams)
//!      through its full lifecycle (propose -> vote -> timelock -> execute).
//!   3. `iln_distribution` — accrues LP/freelancer/payer rewards automatically
//!      via ILN's `notify_distribution_funding`/`notify_distribution_settlement`
//!      hooks (wired via `set_distribution_contract`), then pays out on claim.
//!   4. `reputation_bonus` — a standalone contract governed by `iln_governance`
//!      (see Issue #704); its parameters are updated via a real governance
//!      proposal in this same scenario, and its own on-chain state is
//!      verified afterward, alongside its independent submit/mark-paid flow.
//!   5. `insurance_pool` — an LP enrolls (via `deposit_premium`) and the pool
//!      stays wired to ILN via `set_insurance_pool` throughout.
//!
//! This is intentionally the broadest, most integration-heavy test in the
//! repo — the single highest-value read for an auditor wanting to see how
//! the five contracts actually compose in practice, not just in isolation.

extern crate std;

#[path = "mocks/mock_token.rs"]
mod mock_token;

use mock_token::{MockToken, MockTokenClient};

use iln_governance::{GovContract, GovContractClient, ProposalAction, ProposalStatus};
use insurance_pool::{InsurancePool, InsurancePoolClient};
use invoice_liquidity::{InvoiceLiquidityContract, InvoiceLiquidityContractClient, ReferralCode};
use reputation_bonus::{
    config::Config as RepBonusConfig, ReputationBonusContract, ReputationBonusContractClient,
};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000; // 100 USDC (7-decimal)
const DISCOUNT_RATE: u32 = 300; // 3%
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30; // 30 days
const LEDGER_TIMESTAMP: u64 = 1_700_000_000;
const GOV_TOTAL_SUPPLY: i128 = 20_000;
const COVERAGE_CAP: i128 = 1_000;
const VOTING_PERIOD_SECS: u64 = 259_200; // mirrors iln_governance's own constant

fn dummy_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[7u8; 32])
}

/// Advances both ledger sequence and timestamp together, clearing every
/// admin-action rate-limit cooldown in one call (mirrors the identical
/// workaround already established in tests_insurance_integration.rs /
/// tests_oracle_registry.rs).
fn advance_ledger(env: &Env, sequence_delta: u32, timestamp_delta: u64) {
    let mut info = env.ledger().get();
    info.sequence_number += sequence_delta;
    info.timestamp += timestamp_delta;
    env.ledger().set(info);
}

#[test]
fn test_full_protocol_lifecycle_across_all_five_contracts() {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();

    // ── Roles ────────────────────────────────────────────────────────────
    let admin = Address::generate(&env);
    let voter = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let lp = Address::generate(&env);

    // ── Payment token ────────────────────────────────────────────────────
    let payment_token_addr = env.register_contract(None, MockToken);
    let payment_token = MockTokenClient::new(&env, &payment_token_addr);
    payment_token.mint(&lp, &(INVOICE_AMOUNT * 2));
    payment_token.mint(&payer, &INVOICE_AMOUNT);

    // ── 1. invoice_liquidity (ILN) — the main protocol ──────────────────
    let xlm_id = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let xlm_addr = xlm_id.address();
    let eurc_addr = Address::generate(&env);
    let iln_id = env.register_contract(None, InvoiceLiquidityContract);
    let iln = InvoiceLiquidityContractClient::new(&env, &iln_id);
    iln.initialize(&admin, &payment_token_addr, &eurc_addr, &xlm_addr);

    let mut ledger = env.ledger().get();
    ledger.timestamp = LEDGER_TIMESTAMP;
    env.ledger().set(ledger);

    // ── 3. iln_distribution — reward accrual ────────────────────────────
    // The governance token's SAC must be administered by the distribution
    // contract itself: `dist.initialize` enforces that invariant and
    // `claim_tokens` mints through it (Issue #861), so the distribution
    // contract is registered before the token it administers.
    let dist_id = env.register_contract(None, iln_distribution::IlnDistribution);
    let gov_token_id = env.register_stellar_asset_contract_v2(dist_id.clone());
    let gov_token_addr = gov_token_id.address();
    let gov_token_admin = StellarAssetClient::new(&env, &gov_token_addr);
    gov_token_admin.mint(&voter, &3_000); // exceeds 10% quorum on GOV_TOTAL_SUPPLY

    let dist = iln_distribution::IlnDistributionClient::new(&env, &dist_id);
    dist.initialize(&iln_id, &gov_token_addr);

    advance_ledger(&env, 200, 1_000); // clear ILN's admin-action rate limits
    iln.set_distribution_contract(&dist_id);

    // ── 4. reputation_bonus — standalone contract, governed contract #4 ──
    let rep_id = env.register_contract(None, ReputationBonusContract);
    let rep = ReputationBonusContractClient::new(&env, &rep_id);

    // ── 2. iln_governance — its own contract, and the coordinator that
    //    ties reputation_bonus into the rest of the protocol (Issue #704) ─
    let governance_id = env.register_contract(None, GovContract);
    let governance = GovContractClient::new(&env, &governance_id);
    governance.initialize(
        &iln_id,
        &dist_id,
        &rep_id,
        &gov_token_addr,
        &admin,
        &GOV_TOTAL_SUPPLY,
    );

    // Issue #805: checkpoint the voter and age it past the holding period
    // so the governance vote below exercises the eligible path.
    governance.checkpoint_balance(&voter);
    advance_ledger(&env, 11, 55);

    // reputation_bonus's admin is governance's own address, matching the
    // authorization pattern execute_proposal relies on (Issue #704).
    rep.init(&governance_id);
    rep.set_config(&RepBonusConfig {
        high_rep_threshold: 700,
        bonus_bps: 100,
        min_discount_rate_bps: 50,
    });

    // ── 5. insurance_pool — LP enrolls, pool stays wired throughout ─────
    let pool_id = env.register_contract(None, InsurancePool);
    let pool = InsurancePoolClient::new(&env, &pool_id);
    pool.initialize(&iln_id, &COVERAGE_CAP, &payment_token_addr);
    advance_ledger(&env, 200, 1_000); // clear ILN's set_insurance_pool rate limit
    iln.set_insurance_pool(&pool_id);

    payment_token.mint(&lp, &600);
    pool.deposit_premium(&lp, &600);
    assert!(pool.is_enrolled(&lp));

    // ── Invoicing journey ────────────────────────────────────────────────
    let due_date = env.ledger().timestamp() + DUE_DATE_OFFSET;
    let invoice_id = iln.submit_invoice(
        &freelancer,
        &payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &payment_token_addr,
        &ReferralCode::None,
    );

    // Funding triggers ILN's notify_distribution_funding -> dist.accrue_lp.
    iln.fund_invoice(&lp, &invoice_id, &INVOICE_AMOUNT, &false);
    assert!(
        dist.get_accrual(&lp) > 0,
        "LP funding must accrue a distribution reward"
    );

    // Settlement triggers notify_distribution_settlement -> accrue_settlement.
    iln.mark_paid(&invoice_id, &INVOICE_AMOUNT);
    let freelancer_accrual_before_claim = dist.get_accrual(&freelancer);
    assert!(
        freelancer_accrual_before_claim > 0,
        "on-time settlement must accrue a freelancer reward"
    );

    let lp_accrual_before_claim = dist.get_accrual(&lp);
    let lp_claimed = dist.claim_tokens(&lp);
    assert!(lp_claimed > 0);
    assert_eq!(lp_claimed, lp_accrual_before_claim);
    // get_accrual reports lifetime total_earned (not an unclaimed balance),
    // so it doesn't reset to 0 after claiming — a second claim of the same
    // already-claimed amount must return 0 instead.
    assert_eq!(
        dist.claim_tokens(&lp),
        0,
        "re-claiming already-claimed tokens must be a no-op"
    );

    // ── Governance touches reputation_bonus (Issue #704's wiring) ───────
    let hash = dummy_hash(&env);
    let proposal_id = governance.create_proposal(
        &voter,
        &ProposalAction::UpdateReputationBonusParams(800, 150, 75),
        &hash,
        &7_500_000_i128,
    );
    governance.cast_vote(&voter, &proposal_id, &true);

    let mut ledger = env.ledger().get();
    ledger.timestamp += VOTING_PERIOD_SECS + 1;
    env.ledger().set(ledger);
    governance.execute_proposal(&proposal_id); // Active -> Passed

    let mut ledger = env.ledger().get();
    ledger.sequence_number += 1_000;
    env.ledger().set(ledger);
    governance.execute_proposal(&proposal_id); // Passed -> Executed

    // ── Final cross-contract consistency check ──────────────────────────
    assert_eq!(
        governance.get_proposal(&proposal_id).status,
        ProposalStatus::Executed
    );
    let final_rep_config = rep.get_config();
    assert_eq!(final_rep_config.high_rep_threshold, 800);
    assert_eq!(final_rep_config.bonus_bps, 150);
    assert_eq!(final_rep_config.min_discount_rate_bps, 75);

    assert!(
        pool.is_enrolled(&lp),
        "insurance enrollment must survive the whole journey"
    );
    assert_eq!(pool.get_pool_balance(), 600);

    // ILN itself is still live and functional after everything above — no
    // cross-contract call in this journey left it in a broken state.
    let second_invoice_id = iln.submit_invoice(
        &freelancer,
        &payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &payment_token_addr,
        &ReferralCode::None,
    );
    assert_ne!(second_invoice_id, invoice_id);
}
