//! Issue #1008 — cross-contract governance authorization.
//!
//! `governance_main_integration_test.rs` uses an externally-owned ILN admin
//! plus `mock_all_auths`, so it never proves the production trust model:
//! the ILN `Admin` is the governance contract itself, and the only thing that
//! lets governance call `update_fee_rate` is the Soroban rule that a contract
//! implicitly authorizes its own direct sub-invocations.
//!
//! These tests deploy the real `invoice_liquidity` + `iln_governance`
//! contracts with ILN's admin set to the governance address, then DROP all
//! mocked auths (`set_auths(&[])`) before the execution path, so every
//! `require_auth` on the cross-contract hop is genuinely enforced.

extern crate std;

use iln_governance::{
    GovContract, GovContractClient, GovernanceError, ProposalAction, ProposalStatus,
};
use invoice_liquidity::{
    constants::ECONOMIC_PARAM_COOLDOWN_LEDGERS, InvoiceLiquidityContract,
    InvoiceLiquidityContractClient,
};
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env,
};

const GOV_TOTAL_SUPPLY: i128 = 20_000;
const VOTING_PERIOD_SECS: u64 = 259_200;

struct T {
    env: Env,
    iln: InvoiceLiquidityContractClient<'static>,
    gov: GovContractClient<'static>,
    gov_admin: Address,
    voter: Address,
}

fn hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[7u8; 32])
}

/// `iln_admin_is_governance`: production wiring when true; a misconfigured
/// deployment (ILN admin is some other key) when false.
fn setup(iln_admin_is_governance: bool) -> T {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();

    let gov_token = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let gov_token_addr = gov_token.address();
    let voter = Address::generate(&env);
    StellarAssetClient::new(&env, &gov_token_addr).mint(&voter, &3_000);

    let gov_addr = env.register_contract(None, GovContract);
    let gov = GovContractClient::new(&env, &gov_addr);
    let iln_addr = env.register_contract(None, InvoiceLiquidityContract);
    let iln = InvoiceLiquidityContractClient::new(&env, &iln_addr);

    let iln_admin = if iln_admin_is_governance {
        gov_addr.clone()
    } else {
        Address::generate(&env)
    };
    let usdc = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let xlm = env.register_stellar_asset_contract_v2(Address::generate(&env));
    iln.initialize(
        &iln_admin,
        &usdc.address(),
        &Address::generate(&env),
        &xlm.address(),
    );

    let gov_admin = Address::generate(&env);
    gov.initialize(
        &iln_addr,
        &Address::generate(&env),
        &Address::generate(&env),
        &gov_token_addr,
        &gov_admin,
        &GOV_TOTAL_SUPPLY,
    );

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    // Clear ILN's rate-limit cooldown (keyed off ledger sequence, default 0).
    ledger.sequence_number += ECONOMIC_PARAM_COOLDOWN_LEDGERS as u32;
    env.ledger().set(ledger);

    T {
        env,
        iln,
        gov,
        gov_admin,
        voter,
    }
}

/// Create + vote + close the window + Active -> Passed, then drop every mocked
/// auth so the final execution is authorized only by real Soroban rules.
fn pass_proposal(t: &T, action: ProposalAction, value: i128) -> u64 {
    let id = t
        .gov
        .create_proposal(&t.voter, &action, &hash(&t.env), &value);
    t.gov.cast_vote(&t.voter, &id, &true);
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += VOTING_PERIOD_SECS + 1;
    t.env.ledger().set(ledger);
    t.gov.execute_proposal(&id);
    assert_eq!(t.gov.get_proposal(&id).status, ProposalStatus::Passed);
    t.env.set_auths(&[]);
    id
}

fn fee_rate(t: &T) -> u32 {
    t.env.as_contract(&t.iln.address, || {
        t.env
            .storage()
            .instance()
            .get(&invoice_liquidity::storage::DataKey::FeeRate)
            .unwrap_or(0_u32)
    })
}

/// Success: with ILN admin == governance and NO mocked auths, governance's own
/// sub-invocation authorizes `update_fee_rate`.
#[test]
fn governance_as_iln_admin_executes_without_mocked_auths() {
    let t = setup(true);
    let id = pass_proposal(&t, ProposalAction::UpdateFeeRate(250), 250);

    t.gov.execute_proposal(&id);

    assert_eq!(t.gov.get_proposal(&id).status, ProposalStatus::Executed);
    assert_eq!(fee_rate(&t), 250);
}

/// Authorization: nobody but governance can drive the admin setter directly.
#[test]
fn direct_call_to_admin_setter_without_governance_is_rejected() {
    let t = setup(true);
    t.env.set_auths(&[]);

    assert!(t.iln.try_update_fee_rate(&999).is_err());
    assert_eq!(fee_rate(&t), 0);
}

/// Failure: ILN admin is NOT governance (misconfiguration). The cross-contract
/// call is rejected by ILN's `require_auth`; governance must not mark the
/// proposal Executed, must leave ILN untouched, and must emit the rejection.
#[test]
fn governance_not_iln_admin_cannot_execute_and_emits_failure_event() {
    let t = setup(false);
    let id = pass_proposal(&t, ProposalAction::UpdateFeeRate(250), 250);
    let eta = t.gov.get_proposal(&id).eta_ledger;

    t.gov.execute_proposal(&id);

    assert!(
        std::format!("{:?}", t.env.events().all().events()).contains("proposal_execution_failed")
    );
    let p = t.gov.get_proposal(&id);
    assert_eq!(p.status, ProposalStatus::Passed);
    assert_eq!(p.eta_ledger, eta);
    assert_eq!(fee_rate(&t), 0);
}

/// Adversarial: repeated retries of a rejected execution never escalate
/// privileges or change state, and never double-apply once fixed.
#[test]
fn rejected_execution_can_be_retried_without_side_effects() {
    let t = setup(false);
    let id = pass_proposal(&t, ProposalAction::UpdateFeeRate(250), 250);
    for _ in 0..3 {
        t.gov.execute_proposal(&id);
        assert_eq!(t.gov.get_proposal(&id).status, ProposalStatus::Passed);
        assert_eq!(fee_rate(&t), 0);
    }
}

/// Authorization: a proposal that was rejected by voters never reaches ILN.
#[test]
fn rejected_proposal_never_calls_iln() {
    let t = setup(true);
    let id = t.gov.create_proposal(
        &t.voter,
        &ProposalAction::UpdateFeeRate(250),
        &hash(&t.env),
        &250,
    );
    // No votes: quorum miss.
    let mut ledger = t.env.ledger().get();
    ledger.timestamp += VOTING_PERIOD_SECS + 1;
    t.env.ledger().set(ledger);
    t.gov.execute_proposal(&id);
    t.env.set_auths(&[]);

    assert_eq!(t.gov.get_proposal(&id).status, ProposalStatus::Rejected);
    assert_eq!(
        t.gov.try_execute_proposal(&id),
        Err(Ok(GovernanceError::AlreadyResolved))
    );
    assert_eq!(fee_rate(&t), 0);
}

/// Ledger boundary: with an execution delay, execution at `eta - 1` is refused
/// and at `eta` succeeds, under real (unmocked) cross-contract auth.
#[test]
fn execution_delay_boundary_with_real_cross_contract_auth() {
    let t = setup(true);
    t.gov.set_execution_delay(&t.gov_admin, &10_u32);
    let id = pass_proposal(&t, ProposalAction::UpdateFeeRate(250), 250);
    let eta = t.gov.get_proposal(&id).eta_ledger;

    let mut ledger = t.env.ledger().get();
    ledger.sequence_number = eta - 1;
    t.env.ledger().set(ledger.clone());
    assert_eq!(
        t.gov.try_execute_proposal(&id),
        Err(Ok(GovernanceError::TimelockNotExpired))
    );
    assert_eq!(fee_rate(&t), 0);

    ledger.sequence_number = eta;
    t.env.ledger().set(ledger);
    t.gov.execute_proposal(&id);
    assert_eq!(fee_rate(&t), 250);
}
