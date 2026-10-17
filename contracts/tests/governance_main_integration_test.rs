//! Integration tests — governance contract → invoice_liquidity contract.
//!
//! Issue #86: verifies that governance proposals which update main-contract
//! parameters actually take effect, and that vetoed proposals are blocked.
//!
//! Contracts deployed per test:
//!   - `InvoiceLiquidityContract` (ILN) — the main protocol
//!   - `GovContract` — governance with ILN as its target
//!   - `MockToken` — deterministic payment token (wraps mocks/mock_token.rs)
//!   - Stellar Asset Contract — governance voting token

extern crate std;

// ── Import the mock token defined in contracts/tests/mocks/mock_token.rs ──────
#[path = "mocks/mock_token.rs"]
mod mock_token;

use mock_token::{MockToken, MockTokenClient};

use iln_governance::{
    GovContract, GovContractClient, GovernanceError, OracleFeedType as GovOracleFeedType,
    ProposalAction, ProposalStatus,
};
use invoice_liquidity::{
    oracle_interface::ORACLE_INTERFACE_VERSION,
    oracle_registry::OracleFeedType as IlnOracleFeedType, ContractError, InvoiceLiquidityContract,
    InvoiceLiquidityContractClient, InvoiceStatus, OracleVerificationResponse, ReferralCode,
};
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env,
};

// ── Shared helpers ────────────────────────────────────────────────────────────

const INVOICE_AMOUNT: i128 = 1_000_000_000; // 100 USDC (7-decimal)
const DISCOUNT_RATE: u32 = 300; // 3 %
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30; // 30 days
const LEDGER_TIMESTAMP: u64 = 1_700_000_000;
const GOV_TOTAL_SUPPLY: i128 = 20_000; // seeded via initialize()'s gov_token_total_supply param

// ── Mock oracle for the RegisterTokenOracle governance test ────────────────────
//
// Implements the same shape `fund_invoice`'s `require_oracle_verification`
// path actually calls (`interface_version` + `get_payer_data` returning
// `OracleVerificationResponse`) — mirroring
// `tests_oracle_registry.rs::MockRegistryOracle` inside the invoice_liquidity
// crate, redefined here since that one is `#[cfg(test)]`-internal and not
// exported for use by this external integration-test crate.

#[contract]
struct MockRegistryOracle;

#[contractimpl]
impl MockRegistryOracle {
    pub fn interface_version(_env: Env) -> u32 {
        ORACLE_INTERFACE_VERSION
    }

    pub fn set_response(env: Env, verified: bool, ts: u32) {
        env.storage()
            .instance()
            .set(&soroban_sdk::symbol_short!("verified"), &verified);
        env.storage()
            .instance()
            .set(&soroban_sdk::symbol_short!("ts"), &ts);
    }

    pub fn get_payer_data(env: Env, _payer: Address) -> OracleVerificationResponse {
        let is_verified: bool = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("verified"))
            .unwrap_or(true);
        let timestamp: u32 = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("ts"))
            .unwrap_or(env.ledger().sequence());
        OracleVerificationResponse {
            is_verified,
            timestamp,
        }
    }
}

struct GovIntegrationEnv {
    env: Env,
    iln: InvoiceLiquidityContractClient<'static>,
    governance: GovContractClient<'static>,
    /// ILN admin / governance admin.
    admin: Address,
    /// Holder of governance voting tokens — can pass any proposal solo.
    voter: Address,
    /// Payment token (MockToken) used for invoices.
    payment_token: MockTokenClient<'static>,
    payment_token_addr: Address,
    /// Freelancer, payer, LP for invoice flow tests.
    freelancer: Address,
    payer: Address,
    lp: Address,
}

fn dummy_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[1u8; 32])
}

fn setup() -> GovIntegrationEnv {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();

    // ── Governance voting token (Stellar Asset Contract) ──────────────────
    let gov_token_admin_addr = Address::generate(&env);
    let gov_token_id = env.register_stellar_asset_contract_v2(gov_token_admin_addr);
    let gov_token_addr = gov_token_id.address();
    let gov_token_admin = StellarAssetClient::new(&env, &gov_token_addr);

    let admin = Address::generate(&env);
    let voter = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let lp = Address::generate(&env);

    // voter holds enough tokens to exceed the 10% quorum on GOV_TOTAL_SUPPLY=20_000.
    gov_token_admin.mint(&voter, &3_000);

    // ── MockToken (payment token for ILN invoices) ────────────────────────
    let payment_token_addr = env.register_contract(None, MockToken);
    let payment_token = MockTokenClient::new(&env, &payment_token_addr);

    // Fund LP (enough for discount cost) and payer (full invoice amount).
    payment_token.mint(&lp, &INVOICE_AMOUNT);
    payment_token.mint(&payer, &INVOICE_AMOUNT);

    // ── ILN contract ──────────────────────────────────────────────────────
    // XLM SAC placeholder (not used in these tests).
    let xlm_id = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let xlm_addr = xlm_id.address();

    let iln_id = env.register_contract(None, InvoiceLiquidityContract);
    let iln = InvoiceLiquidityContractClient::new(&env, &iln_id);
    let eurc_addr = Address::generate(&env);
    iln.initialize(&admin, &payment_token_addr, &eurc_addr, &xlm_addr);

    // ── Governance contract ───────────────────────────────────────────────
    // Distribution and reputation_bonus contracts aren't exercised by these
    // tests (no distribution- or reputation-bonus-related proposals), so
    // generated placeholder addresses are sufficient.
    let dist_addr = Address::generate(&env);
    let rep_bonus_addr = Address::generate(&env);
    let governance_id = env.register_contract(None, GovContract);
    let governance = GovContractClient::new(&env, &governance_id);
    governance.initialize(
        &iln_id,
        &dist_addr,
        &rep_bonus_addr,
        &gov_token_addr,
        &admin,
        &GOV_TOTAL_SUPPLY,
    );

    // Issue #805: checkpoint the voter and age it past the holding period
    // (10 ledgers) so votes below exercise the eligible path. The constant
    // lives in the governance contract (`MIN_VOTE_HOLD_LEDGERS`); the `+ 1`
    // keeps the Aged rule (`cp.ledger + 10 <= created_ledger`) satisfied.
    governance.checkpoint_balance(&voter);
    let mut ledger = env.ledger().get();
    ledger.sequence_number += 11;
    env.ledger().set(ledger);

    // Fix ledger timestamp.
    let mut ledger = env.ledger().get();
    ledger.timestamp = LEDGER_TIMESTAMP;
    env.ledger().set(ledger);

    GovIntegrationEnv {
        env,
        iln,
        governance,
        admin,
        voter,
        payment_token,
        payment_token_addr,
        freelancer,
        payer,
        lp,
    }
}

/// Pass a proposal through the full governance lifecycle and execute it.
///
/// Steps:
/// 1. voter casts a FOR vote (3 000 / 20 000 = 15 % > 10 % quorum).
/// 2. Advance timestamp past the 3-day voting window.
/// 3. Call `execute_proposal` twice:
///    - first call: Active → Passed
///    - second call: Passed → Executed (with zero timelock delay)
fn pass_and_execute(t: &GovIntegrationEnv, proposal_id: u64) {
    t.governance.cast_vote(&t.voter, &proposal_id, &true);

    // Advance past the 259 200-second voting window. Also advance the
    // ledger sequence: executing an economic-parameter proposal (e.g.
    // UpdateFeeRate, UpdateMaxDiscountRate) calls into the ILN contract's
    // own rate-limited setters, whose cooldown is keyed off ledger
    // sequence, not timestamp. The last-call ledger for a rate-limited
    // function defaults to 0 when never called, so without this the
    // first-ever call at the default sequence 0 incorrectly trips the
    // cooldown — see tests_oracle_registry.rs's
    // `advance_past_rate_limit_cooldown` for the same workaround applied
    // elsewhere in the ILN contract's own test suite.
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    ledger.sequence_number += invoice_liquidity::constants::ECONOMIC_PARAM_COOLDOWN_LEDGERS as u32;
    t.env.ledger().set(ledger);

    // Active → Passed
    t.governance.execute_proposal(&proposal_id);
    // Passed → Executed  (zero-delay timelock: eta_ledger == current_sequence)
    t.governance.execute_proposal(&proposal_id);
}

// ── Test 1 ────────────────────────────────────────────────────────────────────

/// A governance proposal to lower `max_discount_rate` is executed, and the
/// main contract enforces the new limit when validating future invoices.
#[test]
fn test_update_max_discount_via_governance_takes_effect() {
    let t = setup();

    // Before the proposal: discount_rate=2_000 (20 %) is below the default
    // maximum of 5_000 (50 %) so submission succeeds.
    let due_date = LEDGER_TIMESTAMP + DUE_DATE_OFFSET;
    let result_before = t.iln.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &2_000,
        &t.payment_token_addr,
        &ReferralCode::None,
    );
    assert!(
        result_before.is_ok(),
        "submission with rate=2000 should succeed before governance change"
    );

    // Governance proposal: lower MaxDiscountRate to 1 000 (10 %).
    let proposal_id = t.governance.create_proposal(
        &t.voter,
        &ProposalAction::UpdateMaxDiscountRate(1_000),
        &dummy_hash(&t.env),
        &1_000_i128,
    );

    pass_and_execute(&t, proposal_id);

    let p = t.governance.get_proposal(&proposal_id);
    assert_eq!(p.status, ProposalStatus::Executed);

    // After execution: discount_rate=2_000 now exceeds the new maximum of 1_000.
    let due_date_2 = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let result_after = t.iln.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date_2,
        &2_000,
        &t.payment_token_addr,
        &ReferralCode::None,
    );
    assert_eq!(
        result_after,
        Err(Ok(ContractError::InvalidDiscountRate)),
        "submission with rate=2000 must be rejected after max was lowered to 1000"
    );

    // A submission at the new limit (1 000) still succeeds.
    let result_at_limit = t.iln.try_submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date_2,
        &1_000,
        &t.payment_token_addr,
        &ReferralCode::None,
    );
    assert!(
        result_at_limit.is_ok(),
        "submission with rate==new_max (1000) must still succeed"
    );
}

// ── Test 2 ────────────────────────────────────────────────────────────────────

/// A governance proposal to set the protocol fee is executed, and the fee is
/// correctly deducted from LP payout when the invoice is settled.
#[test]
fn test_update_fee_rate_via_governance_takes_effect() {
    let t = setup();

    // Governance proposal: set fee_rate to 200 bps (2 %).
    let fee_rate: u32 = 200;
    let proposal_id = t.governance.create_proposal(
        &t.voter,
        &ProposalAction::UpdateFeeRate(fee_rate),
        &dummy_hash(&t.env),
        &(fee_rate as i128),
    );

    pass_and_execute(&t, proposal_id);

    let p = t.governance.get_proposal(&proposal_id);
    assert_eq!(p.status, ProposalStatus::Executed);

    // Run the invoice lifecycle: submit → fund → mark_paid.
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let invoice_id = t.iln.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &(t.env.ledger().timestamp() + DUE_DATE_OFFSET),
        &DISCOUNT_RATE,
        &t.payment_token_addr,
        &ReferralCode::None,
    );

    t.iln
        .fund_invoice(&t.lp, &invoice_id, &INVOICE_AMOUNT, &false);
    t.iln.mark_paid(&invoice_id, &INVOICE_AMOUNT);

    // Admin should have received the protocol fee.
    // fee = INVOICE_AMOUNT * fee_rate / 10_000 = 1_000_000_000 * 200 / 10_000 = 20_000_000
    let expected_fee = INVOICE_AMOUNT * fee_rate as i128 / 10_000;
    let admin_balance = t.payment_token.balance(&t.admin);

    assert_eq!(
        admin_balance, expected_fee,
        "admin balance ({}) must equal the protocol fee ({})",
        admin_balance, expected_fee,
    );
}

// ── Test 3 ────────────────────────────────────────────────────────────────────

/// A proposal progress through the live governance lifecycle and explicitly lands
/// in `Passed` before the final `Executed` transition.
#[test]
fn test_proposal_status_transitions_are_exercised() {
    let t = setup();

    let proposal_id = t.governance.create_proposal(
        &t.voter,
        &ProposalAction::UpdateFeeRate(250),
        &dummy_hash(&t.env),
        &250_i128,
    );

    let initial = t.governance.get_proposal(&proposal_id);
    assert_eq!(initial.status, ProposalStatus::Active);

    t.governance.cast_vote(&t.voter, &proposal_id, &true);

    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    ledger.sequence_number += invoice_liquidity::constants::ECONOMIC_PARAM_COOLDOWN_LEDGERS as u32;
    t.env.ledger().set(ledger);

    assert_eq!(t.governance.try_execute_proposal(&proposal_id), Ok(Ok(())));
    let passed = t.governance.get_proposal(&proposal_id);
    assert_eq!(passed.status, ProposalStatus::Passed);

    assert_eq!(t.governance.try_execute_proposal(&proposal_id), Ok(Ok(())));
    let executed = t.governance.get_proposal(&proposal_id);
    assert_eq!(executed.status, ProposalStatus::Executed);
}

/// A vetoed proposal cannot be executed; the target parameter is unchanged.
#[test]
fn test_veto_proposal_prevents_execution() {
    let t = setup();

    // Create a proposal that would change the fee rate.
    let proposal_id = t.governance.create_proposal(
        &t.voter,
        &ProposalAction::UpdateFeeRate(500),
        &dummy_hash(&t.env),
        &500_i128,
    );

    // Cast a FOR vote so the proposal would pass if not vetoed.
    t.governance.cast_vote(&t.voter, &proposal_id, &true);

    // Veto multisig (1-of-1, the admin) vetoes the proposal.
    t.governance
        .configure_veto_multisig(&soroban_sdk::vec![&t.env, t.admin.clone()], &1);
    t.governance
        .veto_proposal(&t.admin, &proposal_id, &dummy_hash(&t.env));

    let p = t.governance.get_proposal(&proposal_id);
    assert_eq!(p.status, ProposalStatus::Vetoed);

    // Advance past the voting window.
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += 259_201;
    t.env.ledger().set(ledger);

    // Attempting to execute a vetoed proposal must return AlreadyResolved.
    let execute_result = t.governance.try_execute_proposal(&proposal_id);
    assert_eq!(
        execute_result,
        Err(Ok(GovernanceError::AlreadyResolved)),
        "executing a vetoed proposal must fail with AlreadyResolved"
    );

    // The ILN fee rate was NOT changed — submitting and funding an invoice
    // with the default fee (0) means admin receives no fee.
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let invoice_id = t.iln.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &(t.env.ledger().timestamp() + DUE_DATE_OFFSET),
        &DISCOUNT_RATE,
        &t.payment_token_addr,
        &ReferralCode::None,
    );
    t.iln
        .fund_invoice(&t.lp, &invoice_id, &INVOICE_AMOUNT, &false);
    t.iln.mark_paid(&invoice_id, &INVOICE_AMOUNT);

    // With fee_rate still at 0, admin receives no protocol fee.
    let admin_balance = t.payment_token.balance(&t.admin);
    assert_eq!(
        admin_balance, 0,
        "admin balance must be 0 — the vetoed fee-rate change must not have taken effect"
    );
}

// ── Test 4 ────────────────────────────────────────────────────────────────────

/// Full governance-gated production flow for a per-token oracle override:
/// create a `RegisterTokenOracle` proposal, vote it through quorum, let the
/// timelock elapse, execute it (`Active` -> `Passed` -> `Executed`), then
/// confirm both that the ILN contract's oracle registry reflects it *and*
/// that `fund_invoice()` actually queries the newly-registered oracle,
/// rather than the proposal merely reaching `Executed` status without its
/// cross-contract effect landing.
///
/// This closes the gap between `register_token_oracle`'s direct-admin-call
/// unit tests (`contracts/invoice_liquidity/src/tests_oracle_registry.rs`,
/// e.g. `test_register_token_oracle_unauthorized_caller`) and genuine
/// governance-mediated production usage.
#[test]
fn test_register_token_oracle_via_governance_takes_effect_in_fund_invoice() {
    let t = setup();

    // Deploy a real oracle contract, not yet wired to the ILN contract.
    let oracle_id = t.env.register_contract(None, MockRegistryOracle);
    let oracle_client = MockRegistryOracleClient::new(&t.env, &oracle_id);

    // Before the proposal: no oracle is registered for this token/feed, so
    // require_oracle_verification is a no-op and funding succeeds freely.
    let due_date = LEDGER_TIMESTAMP + DUE_DATE_OFFSET;
    let invoice_before = t.iln.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.payment_token_addr,
        &ReferralCode::None,
    );
    let fund_before = t
        .iln
        .try_fund_invoice(&t.lp, &invoice_before, &INVOICE_AMOUNT, &true);
    assert!(
        fund_before.is_ok(),
        "with no oracle registered yet, oracle verification must be a no-op"
    );

    // Governance proposal: register `oracle_id` as the Identity-feed
    // per-token override for `payment_token_addr`.
    let proposal_id = t.governance.create_proposal(
        &t.voter,
        &ProposalAction::RegisterTokenOracle(
            GovOracleFeedType::Identity,
            t.payment_token_addr.clone(),
            oracle_id.clone(),
        ),
        &dummy_hash(&t.env),
        &0_i128,
    );

    pass_and_execute(&t, proposal_id);

    let p = t.governance.get_proposal(&proposal_id);
    assert_eq!(p.status, ProposalStatus::Executed);

    // The ILN contract's oracle registry now resolves this token to the
    // governance-registered oracle — proving the proposal's cross-contract
    // call actually landed, not just that the proposal itself reached
    // `Executed`.
    assert_eq!(
        t.iln
            .get_oracle_for_token(&IlnOracleFeedType::Identity, &t.payment_token_addr),
        Some(oracle_id.clone())
    );

    // fund_invoice() genuinely queries it now: arm the oracle to report this
    // payer as unverified (fresh timestamp — pass_and_execute advanced the
    // ledger sequence, so a stale-from-before-the-proposal timestamp would
    // wrongly trigger OracleDataStale instead of the PayerUnverified path
    // this assertion is targeting).
    oracle_client.set_response(&false, &t.env.ledger().sequence());
    // The LP already spent most of its balance funding invoice_before above
    // (setup() only mints enough for one invoice) — top up before funding a
    // second one.
    t.payment_token.mint(&t.lp, &INVOICE_AMOUNT);
    let due_date_2 = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    let invoice_after = t.iln.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &due_date_2,
        &DISCOUNT_RATE,
        &t.payment_token_addr,
        &ReferralCode::None,
    );
    let rejected = t
        .iln
        .try_fund_invoice(&t.lp, &invoice_after, &INVOICE_AMOUNT, &true);
    assert_eq!(
        rejected,
        Err(Ok(ContractError::PayerUnverified)),
        "fund_invoice must consult the governance-registered oracle and reject an unverified payer"
    );

    // Flip the oracle's answer to verified (again with a fresh timestamp) —
    // funding the same invoice now succeeds, confirming a live oracle read
    // (not a cached/stale one) drives the outcome.
    oracle_client.set_response(&true, &t.env.ledger().sequence());
    t.iln
        .fund_invoice(&t.lp, &invoice_after, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.iln.get_invoice(&invoice_after).status,
        InvoiceStatus::Funded
    );
}
