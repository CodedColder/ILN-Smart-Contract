#![cfg(test)]

//! Tests for Issue #532 — governance-controlled oracle registry.
//!
//! Covers:
//! 1. Registry resolution priority: per-token override > feed-type default > legacy price_oracle.
//! 2. register_oracle / remove_oracle / register_token_oracle / remove_token_oracle CRUD.
//! 3. fund_invoice queries the registry-resolved oracle, not just the legacy field.
//! 4. Oracle health recording (staleness) after a query.

use super::*;
use crate::oracle_registry::{FeederHealth, OracleFeedType};
use crate::test::setup;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger as _, MockAuth, MockAuthInvoke},
    Address, Env, IntoVal,
};

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;

/// Same shape as tests_oracle_freshness.rs's MockTimestampedOracle: returns
/// a stored `is_verified` + `timestamp` pair configurable per test.
#[contract]
struct MockRegistryOracle;

#[contractimpl]
impl MockRegistryOracle {
    pub fn interface_version(_env: Env) -> u32 {
        crate::oracle_interface::ORACLE_INTERFACE_VERSION
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
        let ts: u32 = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("ts"))
            .unwrap_or(env.ledger().sequence());
        OracleVerificationResponse {
            is_verified,
            timestamp: ts,
        }
    }
}

/// `set_price_oracle` / `set_max_oracle_age` / the oracle-registry admin
/// fns are rate-limited (`check_rate_limit`, cooldown =
/// `DEFAULT_RATE_LIMIT_LEDGERS` = 120 ledgers). Advance the ledger past
/// that cooldown so back-to-back rate-limited calls in one test don't trip
/// each other. (`check_rate_limit` allows a function's first-ever call
/// unconditionally, so this is only needed to space *repeats*.)
fn advance_past_rate_limit_cooldown(env: &Env) {
    let mut info = env.ledger().get();
    info.sequence_number += 150;
    info.timestamp += 150 * 5;
    env.ledger().set(info);
}

/// Same shape as tests_access_control.rs's setup_env(): an env WITHOUT
/// mock_all_auths(), so require_auth() actually enforces the caller.
fn setup_env_no_mock_auths() -> (
    Env,
    Address,
    Address,
    InvoiceLiquidityContractClient<'static>,
) {
    let env = Env::default();
    let admin = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let usdc_contract = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token_address = usdc_contract.address();

    let xlm_admin = Address::generate(&env);
    let xlm_contract = env.register_stellar_asset_contract_v2(xlm_admin.clone());
    let xlm_address = xlm_contract.address();

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let client = InvoiceLiquidityContractClient::new(&env, &contract_id);

    env.mock_all_auths();
    client.initialize(&admin, &token_address, &token_address, &xlm_address);

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    env.ledger().set(ledger);

    (env, admin, token_address, client)
}

fn deploy_mock_oracle(t: &crate::test::TestEnv, verified: bool, ts: u32) -> Address {
    let id = t.env.register_contract(None, MockRegistryOracle);
    let client = MockRegistryOracleClient::new(&t.env, &id);
    client.set_response(&verified, &ts);
    id
}

fn make_invoice(t: &crate::test::TestEnv) -> u64 {
    let now = t.env.ledger().timestamp();
    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &INVOICE_AMOUNT,
        &(now + DUE_DATE_OFFSET),
        &DISCOUNT_RATE,
        &t.token.address,
        &ReferralCode::None,
    )
}

// ── Registry CRUD ────────────────────────────────────────────────────────────

#[test]
fn test_no_oracle_registered_resolves_to_none() {
    let t = setup();
    assert_eq!(
        t.contract
            .get_oracle_for_token(&OracleFeedType::Identity, &t.token.address),
        None
    );
}

#[test]
fn test_register_oracle_sets_feed_type_default() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    assert_eq!(
        t.contract
            .get_oracle_for_token(&OracleFeedType::Identity, &t.token.address),
        Some(oracle)
    );
}

#[test]
fn test_remove_oracle_clears_feed_type_default() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);
    t.contract.remove_oracle(&OracleFeedType::Identity);

    assert_eq!(
        t.contract
            .get_oracle_for_token(&OracleFeedType::Identity, &t.token.address),
        None
    );
}

#[test]
fn test_per_token_override_takes_priority_over_feed_type_default() {
    let t = setup();
    let default_oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    let token_oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());

    t.contract
        .register_oracle(&OracleFeedType::Identity, &default_oracle);
    t.contract
        .register_token_oracle(&OracleFeedType::Identity, &t.token.address, &token_oracle);

    assert_eq!(
        t.contract
            .get_oracle_for_token(&OracleFeedType::Identity, &t.token.address),
        Some(token_oracle)
    );
}

#[test]
fn test_remove_token_oracle_falls_back_to_feed_type_default() {
    let t = setup();
    let default_oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    let token_oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());

    t.contract
        .register_oracle(&OracleFeedType::Identity, &default_oracle);
    t.contract
        .register_token_oracle(&OracleFeedType::Identity, &t.token.address, &token_oracle);
    t.contract
        .remove_token_oracle(&OracleFeedType::Identity, &t.token.address);

    assert_eq!(
        t.contract
            .get_oracle_for_token(&OracleFeedType::Identity, &t.token.address),
        Some(default_oracle)
    );
}

#[test]
fn test_legacy_price_oracle_used_as_fallback_for_identity_feed() {
    let t = setup();
    advance_past_rate_limit_cooldown(&t.env);
    let legacy_oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract.set_price_oracle(&legacy_oracle);

    // No registry entries at all — resolution falls through to the legacy field.
    assert_eq!(
        t.contract
            .get_oracle_for_token(&OracleFeedType::Identity, &t.token.address),
        Some(legacy_oracle)
    );
}

#[test]
fn test_registry_default_takes_priority_over_legacy_price_oracle() {
    let t = setup();
    advance_past_rate_limit_cooldown(&t.env);
    let legacy_oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    let registry_oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());

    t.contract.set_price_oracle(&legacy_oracle);
    t.contract
        .register_oracle(&OracleFeedType::Identity, &registry_oracle);

    assert_eq!(
        t.contract
            .get_oracle_for_token(&OracleFeedType::Identity, &t.token.address),
        Some(registry_oracle)
    );
}

#[test]
fn test_price_feed_type_has_no_legacy_fallback() {
    let t = setup();
    advance_past_rate_limit_cooldown(&t.env);
    let legacy_oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract.set_price_oracle(&legacy_oracle);

    // The legacy field is Identity-only; the Price feed type has nothing to
    // fall back to since it didn't exist pre-#532.
    assert_eq!(
        t.contract
            .get_oracle_for_token(&OracleFeedType::Price, &t.token.address),
        None
    );
}

// ── fund_invoice integration ─────────────────────────────────────────────────

#[test]
fn test_fund_invoice_uses_registry_resolved_oracle() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_token_oracle(&OracleFeedType::Identity, &t.token.address, &oracle);

    let invoice_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

    let invoice = t.contract.get_invoice(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Funded);
}

#[test]
fn test_fund_invoice_rejects_unverified_payer_from_registry_oracle() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, false, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    let invoice_id = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(res, Err(Ok(ContractError::PayerUnverified)));
}

// ── Oracle health monitoring ──────────────────────────────────────────────────

#[test]
fn test_oracle_health_unrecorded_before_first_query() {
    let t = setup();
    assert_eq!(
        t.contract
            .get_oracle_health(&OracleFeedType::Identity, &t.token.address),
        None
    );
}

#[test]
fn test_oracle_health_recorded_as_fresh_after_successful_query() {
    let t = setup();
    let current_seq = t.env.ledger().sequence();
    let oracle = deploy_mock_oracle(&t, true, current_seq);
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    let invoice_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

    let health = t
        .contract
        .get_oracle_health(&OracleFeedType::Identity, &t.token.address)
        .unwrap();
    assert_eq!(health.oracle, oracle);
    assert!(!health.is_stale);
    assert_eq!(health.consecutive_stale_count, 0);
}

// Note: fund_invoice's health write for a REJECTED (stale/unverified) call
// does not persist — Soroban rolls back all storage writes made during an
// invocation that returns Err, health snapshot included. That's why the
// tests below use check_oracle_health (which never errors) to observe
// staleness, rather than asserting on state left behind by a failed
// fund_invoice call.

#[test]
fn test_check_oracle_health_detects_stale_data_without_erroring() {
    let t = setup();
    // Lower max_oracle_age so a small, TTL-safe ledger jump is enough to
    // trigger staleness (a 20_000-ledger jump would archive unrelated
    // storage entries in the test sandbox).
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.set_max_oracle_age(&10);

    let old_seq = t.env.ledger().sequence();
    let oracle = deploy_mock_oracle(&t, true, old_seq);
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    let mut info = t.env.ledger().get();
    info.sequence_number += 20;
    info.timestamp += 20 * 5;
    t.env.ledger().set(info);

    let health = t
        .contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer)
        .unwrap();
    assert!(health.is_stale);
    assert_eq!(health.consecutive_stale_count, 1);

    // Confirm it's durable — a fresh read sees the same recorded snapshot.
    let reread = t
        .contract
        .get_oracle_health(&OracleFeedType::Identity, &t.token.address)
        .unwrap();
    assert_eq!(reread, health);
}

#[test]
fn test_check_oracle_health_consecutive_stale_count_increments() {
    let t = setup();
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.set_max_oracle_age(&10);

    let old_seq = t.env.ledger().sequence();
    let oracle = deploy_mock_oracle(&t, true, old_seq);
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    let mut info = t.env.ledger().get();
    info.sequence_number += 20;
    info.timestamp += 20 * 5;
    t.env.ledger().set(info);

    let first = t
        .contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer)
        .unwrap();
    assert_eq!(first.consecutive_stale_count, 1);

    let second = t
        .contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer)
        .unwrap();
    assert_eq!(second.consecutive_stale_count, 2);
}

#[test]
fn test_check_oracle_health_resets_count_after_fresh_query() {
    let t = setup();
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.set_max_oracle_age(&10);

    let old_seq = t.env.ledger().sequence();
    let oracle = deploy_mock_oracle(&t, true, old_seq);
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    let mut info = t.env.ledger().get();
    info.sequence_number += 20;
    info.timestamp += 20 * 5;
    t.env.ledger().set(info);

    let stale = t
        .contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer)
        .unwrap();
    assert!(stale.is_stale);
    assert_eq!(stale.consecutive_stale_count, 1);

    // Refresh the oracle's timestamp to the current ledger — next query is fresh.
    let fresh_ts = t.env.ledger().sequence();
    let oracle_client = MockRegistryOracleClient::new(&t.env, &oracle);
    oracle_client.set_response(&true, &fresh_ts);

    let fresh = t
        .contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer)
        .unwrap();
    assert!(!fresh.is_stale);
    assert_eq!(fresh.consecutive_stale_count, 0);
}

#[test]
fn test_check_oracle_health_returns_none_when_no_oracle_registered() {
    let t = setup();
    assert_eq!(
        t.contract
            .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer),
        None
    );
}

// ── Access control ────────────────────────────────────────────────────────────

#[test]
fn test_register_oracle_unauthorized_caller() {
    let (env, _admin, _, client) = setup_env_no_mock_auths();
    let imposter = Address::generate(&env);
    let oracle = Address::generate(&env);

    env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "register_oracle",
            args: (OracleFeedType::Price, oracle.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    let res = client.try_register_oracle(&OracleFeedType::Price, &oracle);
    assert!(
        res.is_err(),
        "register_oracle should fail for non-admin caller"
    );
}

#[test]
fn test_register_token_oracle_unauthorized_caller() {
    let (env, _admin, token, client) = setup_env_no_mock_auths();
    let imposter = Address::generate(&env);
    let oracle = Address::generate(&env);

    env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "register_token_oracle",
            args: (OracleFeedType::Price, token.clone(), oracle.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    let res = client.try_register_token_oracle(&OracleFeedType::Price, &token, &oracle);
    assert!(
        res.is_err(),
        "register_token_oracle should fail for non-admin caller"
    );
}

#[test]
fn test_remove_oracle_unauthorized_caller() {
    let (env, _admin, _, client) = setup_env_no_mock_auths();

    env.mock_all_auths();
    let oracle_id = env.register_contract(None, MockRegistryOracle);
    MockRegistryOracleClient::new(&env, &oracle_id).set_response(&true, &env.ledger().sequence());
    client.register_oracle(&OracleFeedType::Price, &oracle_id);

    let imposter = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "remove_oracle",
            args: (OracleFeedType::Price,).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    let res = client.try_remove_oracle(&OracleFeedType::Price);
    assert!(
        res.is_err(),
        "remove_oracle should fail for non-admin caller"
    );
}

#[test]
fn test_register_oracle_accepts_compatible_interface_version() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    let result = t
        .contract
        .try_register_oracle(&OracleFeedType::Identity, &oracle);
    assert!(result.is_ok(), "compatible oracle version must be accepted");
    assert_eq!(
        t.contract
            .get_oracle_for_token(&OracleFeedType::Identity, &t.token.address),
        Some(oracle)
    );
}

#[contract]
struct IncompatibleVersionOracle;

#[contractimpl]
impl IncompatibleVersionOracle {
    pub fn interface_version(_env: Env) -> u32 {
        999
    }

    pub fn get_payer_data(env: Env, _payer: Address) -> OracleVerificationResponse {
        OracleVerificationResponse {
            is_verified: true,
            timestamp: env.ledger().sequence(),
        }
    }
}

#[test]
fn test_register_oracle_rejects_incompatible_interface_version() {
    let t = setup();
    let oracle = t.env.register_contract(None, IncompatibleVersionOracle);
    let result = t
        .contract
        .try_register_oracle(&OracleFeedType::Identity, &oracle);
    assert_eq!(
        result.err(),
        Some(Ok(ContractError::IncompatibleInterfaceVersion))
    );
    assert_eq!(
        t.contract
            .get_oracle_for_token(&OracleFeedType::Identity, &t.token.address),
        None,
        "incompatible oracle must not be persisted"
    );
}

// ── Cross-contract pause boundary ─────────────────────────────────────────────
//
// `pause()` only sets a flag read by the *invoice_liquidity* contract's own
// state-changing entry points (see the `is_paused` guards in `fund_invoice`,
// `submit_invoice`, etc. in lib.rs). It has no effect on:
//   1. This contract's own read-only oracle registry queries
//      (`get_oracle_for_token`, `get_oracle_health`, `check_oracle_health`) —
//      they're informational, carry no `is_paused` guard, and stay callable
//      by monitors/keepers throughout an incident.
//   2. The registry's admin/governance mutations (`register_oracle`,
//      `remove_oracle`, `register_token_oracle`, `remove_token_oracle`) —
//      these are gated by `require_admin` only, not `is_paused`, so
//      governance can still repoint or clear a compromised oracle while the
//      protocol is paused (arguably a requirement during an oracle-related
//      incident, not a gap).
//   3. The externally deployed oracle contracts themselves — they are
//      separate contracts with their own lifecycle, wholly outside this
//      contract's `Paused` flag.
// `fund_invoice` never observes any of this: it checks `is_paused` before
// doing anything else, so a paused call never reaches the oracle registry at
// all — it fails fast with `ContractPaused` and leaves no health record.

#[test]
fn test_get_oracle_for_token_readable_while_paused() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    t.contract.pause();

    assert_eq!(
        t.contract
            .get_oracle_for_token(&OracleFeedType::Identity, &t.token.address),
        Some(oracle),
        "registry resolution is a read-only view and must work while paused"
    );
}

#[test]
fn test_check_oracle_health_readable_while_paused() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    t.contract.pause();

    // check_oracle_health performs a live cross-contract call to the oracle
    // and persists a health snapshot. Neither the call nor the write is
    // gated on `is_paused` — it must succeed exactly as it would unpaused.
    let health = t
        .contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer)
        .expect("oracle health check must succeed while contract is paused");
    assert!(!health.is_stale);
    assert_eq!(health.oracle, oracle);

    // And the recorded snapshot is readable back, still while paused.
    assert_eq!(
        t.contract
            .get_oracle_health(&OracleFeedType::Identity, &t.token.address),
        Some(health)
    );
}

#[test]
fn test_fund_invoice_paused_never_reaches_oracle_registry() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    let invoice_id = make_invoice(&t);
    t.contract.pause();

    // fund_invoice must fail fast on the pause guard, before it ever
    // resolves or queries the oracle registry — even with oracle
    // verification requested and a healthy oracle registered.
    let result = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(result, Err(Ok(ContractError::ContractPaused)));

    // No health snapshot was ever recorded — proof the oracle was never
    // touched, rather than silently queried and its result discarded.
    assert_eq!(
        t.contract
            .get_oracle_health(&OracleFeedType::Identity, &t.token.address),
        None,
        "a paused fund_invoice call must not query the oracle registry at all"
    );
}

#[test]
fn test_oracle_registry_mutations_unaffected_by_core_contract_pause() {
    let t = setup();
    let default_oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    let token_oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());

    t.contract.pause();

    // Governance/admin oracle config changes are a different concern from
    // the core contract's operational pause and must go through unaffected.
    t.contract
        .register_oracle(&OracleFeedType::Identity, &default_oracle);
    t.contract
        .register_token_oracle(&OracleFeedType::Identity, &t.token.address, &token_oracle);
    assert_eq!(
        t.contract
            .get_oracle_for_token(&OracleFeedType::Identity, &t.token.address),
        Some(token_oracle)
    );

    t.contract
        .remove_token_oracle(&OracleFeedType::Identity, &t.token.address);
    assert_eq!(
        t.contract
            .get_oracle_for_token(&OracleFeedType::Identity, &t.token.address),
        Some(default_oracle),
        "removing the per-token override must succeed while paused, falling back to the default"
    );

    t.contract.remove_oracle(&OracleFeedType::Identity);
    assert_eq!(
        t.contract
            .get_oracle_for_token(&OracleFeedType::Identity, &t.token.address),
        None,
        "removing the feed-type default must succeed while paused"
    );
}

// ── Circuit breaker ────────────────────────────────────────────────────────────
//
// check_oracle_health / consecutive_stale_count already existed (Issue
// #532) but nothing acted on repeated staleness — funding kept being
// attempted against a degraded oracle indefinitely. This section covers:
//   1. Automatically tripping the breaker after
//      MAX_CONSECUTIVE_STALE_QUERIES consecutive stale observations.
//   2. fund_invoice falling back through the priority chain past a tripped
//      level, or rejecting outright with ContractError::OracleCircuitOpen
//      when no fallback exists.
//   3. Governance-only reset — no auto-recovery on a single fresh query,
//      to avoid flapping.

/// Registers `oracle` as the Identity feed-type default and advances the
/// ledger far enough past `max_age` (already lowered to 10 by the caller
/// via `set_max_oracle_age`) that it reads as stale on every subsequent
/// query, without needing to touch the oracle's own timestamp again.
fn setup_stale_default_oracle(t: &crate::test::TestEnv) -> Address {
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.set_max_oracle_age(&10);

    let old_seq = t.env.ledger().sequence();
    let oracle = deploy_mock_oracle(t, true, old_seq);
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    let mut info = t.env.ledger().get();
    info.sequence_number += 20;
    info.timestamp += 20 * 5;
    t.env.ledger().set(info);

    oracle
}

#[test]
fn test_circuit_trips_after_max_consecutive_stale_queries() {
    let t = setup();
    setup_stale_default_oracle(&t);

    assert!(!t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));

    // Two stale queries: below the MAX_CONSECUTIVE_STALE_QUERIES=3 threshold.
    t.contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer);
    t.contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer);
    assert!(
        !t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "circuit must not trip before the threshold is reached"
    );

    // Third consecutive stale query crosses the threshold and trips it.
    let health = t
        .contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer)
        .unwrap();
    assert_eq!(health.consecutive_stale_count, 3);
    assert!(t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));

    // Further stale queries keep it tripped (idempotent) — the streak keeps
    // climbing for observability, but nothing re-fires on an already-open
    // breaker.
    let health = t
        .contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer)
        .unwrap();
    assert_eq!(health.consecutive_stale_count, 4);
    assert!(t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));
}

#[test]
fn test_fund_invoice_rejects_when_circuit_open_with_no_fallback() {
    let t = setup();
    setup_stale_default_oracle(&t);

    // Only the feed-type default is registered — no per-token override, no
    // legacy price_oracle — so there's nothing to fall back to once it trips.
    for _ in 0..3 {
        t.contract
            .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer);
    }
    assert!(t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));

    let invoice_id = make_invoice(&t);
    let result = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        result,
        Err(Ok(ContractError::OracleCircuitOpen)),
        "fund_invoice must reject oracle-gated funding rather than silently proceeding \
         once the only registered oracle is circuit-tripped"
    );
}

#[test]
fn test_fund_invoice_falls_back_to_feed_type_default_when_override_tripped() {
    let t = setup();
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.set_max_oracle_age(&10);

    // A healthy feed-type-wide default...
    let good_oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &good_oracle);

    // ...and a per-token override (higher priority) that will go stale.
    let bad_oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_token_oracle(&OracleFeedType::Identity, &t.token.address, &bad_oracle);

    // Advance the ledger so the override's timestamp reads as stale.
    let mut info = t.env.ledger().get();
    info.sequence_number += 20;
    info.timestamp += 20 * 5;
    t.env.ledger().set(info);

    // Refresh the default's timestamp so it stays fresh despite the same
    // ledger advance (it was deployed at the same original timestamp).
    let good_client = MockRegistryOracleClient::new(&t.env, &good_oracle);
    good_client.set_response(&true, &t.env.ledger().sequence());

    // check_oracle_health always observes the highest-priority (override)
    // entry via the plain, circuit-agnostic resolve_oracle — three
    // consecutive stale queries against it trips the breaker.
    for _ in 0..3 {
        t.contract
            .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer);
    }
    assert!(t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));

    // fund_invoice's oracle-gated verification must now skip the tripped
    // override and fall back to the healthy feed-type default, rather than
    // rejecting the funding.
    let invoice_id = make_invoice(&t);
    let result = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert!(
        result.is_ok(),
        "fund_invoice must fall back to the healthy feed-type default when the \
         per-token override is circuit-tripped, per the documented priority order"
    );
}

#[test]
fn test_reset_oracle_circuit_requires_admin() {
    let (env, _admin, token_address, client) = setup_env_no_mock_auths();
    let imposter = Address::generate(&env);

    env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "reset_oracle_circuit",
            args: (OracleFeedType::Identity, token_address.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    let res = client.try_reset_oracle_circuit(&OracleFeedType::Identity, &token_address);
    assert!(
        res.is_err(),
        "reset_oracle_circuit should fail for non-admin caller"
    );
}

#[test]
fn test_reset_oracle_circuit_clears_flag_and_restores_verification() {
    let t = setup();
    let oracle = setup_stale_default_oracle(&t);

    for _ in 0..3 {
        t.contract
            .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer);
    }
    assert!(t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));

    t.contract
        .reset_oracle_circuit(&OracleFeedType::Identity, &t.token.address);
    assert!(!t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));

    // Refresh the oracle so it's fresh again, then confirm funding resumes
    // against it directly — no fallback needed, no rejection.
    let oracle_client = MockRegistryOracleClient::new(&t.env, &oracle);
    oracle_client.set_response(&true, &t.env.ledger().sequence());

    let invoice_id = make_invoice(&t);
    let result = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert!(
        result.is_ok(),
        "funding must resume against the original oracle once governance resets the circuit"
    );
}

#[test]
fn test_circuit_does_not_auto_recover_on_single_fresh_query() {
    let t = setup();
    let oracle = setup_stale_default_oracle(&t);

    for _ in 0..3 {
        t.contract
            .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer);
    }
    assert!(t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));

    // The oracle recovers and starts answering fresh again...
    let oracle_client = MockRegistryOracleClient::new(&t.env, &oracle);
    oracle_client.set_response(&true, &t.env.ledger().sequence());
    let health = t
        .contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer)
        .unwrap();
    assert!(!health.is_stale);
    assert_eq!(health.consecutive_stale_count, 0);

    // ...but the circuit stays tripped regardless — no auto-recovery on a
    // single fresh query, to avoid flapping.
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "a single fresh query must not auto-clear the circuit breaker"
    );

    // Oracle-gated funding is still rejected (no fallback registered) even
    // though the underlying oracle is, right now, reporting fresh data.
    let invoice_id = make_invoice(&t);
    let result = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(result, Err(Ok(ContractError::OracleCircuitOpen)));
}

#[test]
fn test_circuit_retrips_immediately_if_still_stale_right_after_reset() {
    let t = setup();
    setup_stale_default_oracle(&t);

    for _ in 0..3 {
        t.contract
            .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer);
    }
    assert!(t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));

    // Governance resets — but the oracle's own data hasn't actually
    // changed, it's still the same stale timestamp.
    t.contract
        .reset_oracle_circuit(&OracleFeedType::Identity, &t.token.address);
    assert!(!t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));

    // The very next query is still stale — this must re-trip immediately,
    // not require MAX_CONSECUTIVE_STALE_QUERIES more failures: the raw
    // streak counter is untouched by reset_oracle_circuit (only the sticky
    // flag is cleared), so it never dipped below threshold across the reset.
    let health = t
        .contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer)
        .unwrap();
    assert!(health.is_stale);
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "a still-stale oracle must re-trip immediately after a reset, not require \
         a fresh multi-query streak on top of the pre-existing one"
    );
}

// ── Issues #815/#816/#817: TWAP accumulator port + per-feed opt-in + window bounds ──

#[test]
fn test_twap_disabled_by_default_spot_unchanged() {
    use crate::oracle_registry::OracleFeedType;
    let t = setup();
    assert!(!t.contract.is_twap_enabled(&OracleFeedType::Price));
    // Default window is the 1-hour (720-ledger) documented default.
    assert_eq!(t.contract.get_twap_window(), 720);
    assert_eq!(
        t.contract
            .get_twap_price(&OracleFeedType::Price, &t.token.address),
        None
    );
}

#[test]
fn test_twap_enabled_feed_reads_windowed_average() {
    use crate::oracle_registry::OracleFeedType;
    let t = setup();
    t.contract.set_twap_enabled(&OracleFeedType::Price, &true);
    assert!(t.contract.is_twap_enabled(&OracleFeedType::Price));

    // Record $20.00 at T0, then $21.00 thirty minutes later; with the
    // default 1-hour window the average over the pair is $20,500.
    t.contract
        .record_twap_sample(&OracleFeedType::Price, &t.token.address, &20_000);
    let mut info = t.env.ledger().get();
    info.timestamp += 1800;
    t.env.ledger().set(info);
    t.contract
        .record_twap_sample(&OracleFeedType::Price, &t.token.address, &21_000);

    assert_eq!(
        t.contract
            .get_twap_price(&OracleFeedType::Price, &t.token.address),
        Some(20_500)
    );
    // get_verified_price branches to the TWAP average when enabled (no spot
    // sources registered, so spot alone would error with NoPriceSource).
    assert_eq!(
        t.contract
            .get_verified_price(&OracleFeedType::Price, &t.token.address),
        20_500
    );
}

#[test]
fn test_twap_disabled_feed_uses_spot_even_with_samples() {
    use crate::oracle_registry::{OracleFeedType, DEFAULT_MAX_PRICE_DEVIATION_BPS};
    let t = setup();
    // Backfill the same samples as above, but leave the flag off.
    t.contract
        .record_twap_sample(&OracleFeedType::Price, &t.token.address, &20_000);
    let mut info = t.env.ledger().get();
    info.timestamp += 1800;
    t.env.ledger().set(info);
    t.contract
        .record_twap_sample(&OracleFeedType::Price, &t.token.address, &21_000);
    assert!(!t.contract.is_twap_enabled(&OracleFeedType::Price));
    // No spot sources -> spot path errors, proving the TWAP average was NOT
    // consulted while disabled (it would have returned 20_500).
    let res = t
        .contract
        .try_get_verified_price(&OracleFeedType::Price, &t.token.address);
    assert_eq!(res, Err(Ok(crate::errors::ContractError::NoPriceSource)));
    let _ = DEFAULT_MAX_PRICE_DEVIATION_BPS;
}

#[test]
fn test_twap_window_bounds_accept_and_reject() {
    use crate::oracle_registry::{MAX_TWAP_WINDOW_LEDGERS, MIN_TWAP_WINDOW_LEDGERS};
    let t = setup();
    // Bounds themselves are accepted. Each subsequent call is spaced past
    // the rate-limit cooldown so the assertion observes the window validation
    // (or auth) result, never `RateLimited`.
    t.contract.set_twap_window(&MIN_TWAP_WINDOW_LEDGERS);
    assert_eq!(t.contract.get_twap_window(), MIN_TWAP_WINDOW_LEDGERS);
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.set_twap_window(&MAX_TWAP_WINDOW_LEDGERS);
    assert_eq!(t.contract.get_twap_window(), MAX_TWAP_WINDOW_LEDGERS);
    // Outside bounds rejected with the dedicated error.
    advance_past_rate_limit_cooldown(&t.env);
    assert_eq!(
        t.contract
            .try_set_twap_window(&(MIN_TWAP_WINDOW_LEDGERS - 1)),
        Err(Ok(crate::errors::ContractError::InvalidTwapWindow))
    );
    advance_past_rate_limit_cooldown(&t.env);
    assert_eq!(
        t.contract
            .try_set_twap_window(&(MAX_TWAP_WINDOW_LEDGERS + 1)),
        Err(Ok(crate::errors::ContractError::InvalidTwapWindow))
    );
    advance_past_rate_limit_cooldown(&t.env);
    assert_eq!(
        t.contract.try_set_twap_window(&0),
        Err(Ok(crate::errors::ContractError::InvalidTwapWindow))
    );
}

// ----------------------------------------------------------------
// Issue #860 — health-gate coverage on price-dependent reads
// ----------------------------------------------------------------

/// Record a stale health snapshot for `(feed, t.token)` directly through the
/// registry's health machinery. Storage writes must run as the contract, so
/// the call is wrapped in `as_contract`. Data is 1_000 ledgers old against a
/// 10-ledger max age → `is_stale = true`.
fn record_stale_health(t: &crate::test::TestEnv, feed: OracleFeedType, oracle: &Address) {
    let seq = t.env.ledger().sequence();
    t.env.as_contract(&t.contract.address, || {
        crate::oracle_registry::record_oracle_health(
            &t.env,
            feed,
            &t.token.address,
            oracle,
            seq.saturating_sub(1_000),
            10,
        );
    });
}

/// A tripped circuit must reject `get_verified_price` with
/// `OracleCircuitOpen` — before the price-source search runs (with no
/// sources registered, an ungated read would surface `NoPriceSource`, so
/// the asserted error proves the gate fires first).
#[test]
fn test_get_verified_price_rejects_tripped_circuit() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    for _ in 0..crate::oracle_registry::MAX_CONSECUTIVE_STALE_QUERIES {
        record_stale_health(&t, OracleFeedType::Price, &oracle);
    }
    assert!(t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Price, &t.token.address));

    let res = t
        .contract
        .try_get_verified_price(&OracleFeedType::Price, &t.token.address);
    assert_eq!(
        res,
        Err(Ok(crate::errors::ContractError::OracleCircuitOpen))
    );
}

/// A single stale health snapshot (below the trip threshold — circuit still
/// closed) must reject `get_verified_price` with `OracleDataStale`.
#[test]
fn test_get_verified_price_rejects_stale_health() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    record_stale_health(&t, OracleFeedType::Price, &oracle);
    assert!(!t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Price, &t.token.address));

    let res = t
        .contract
        .try_get_verified_price(&OracleFeedType::Price, &t.token.address);
    assert_eq!(res, Err(Ok(crate::errors::ContractError::OracleDataStale)));
}

/// `get_twap_price` collapses both health-gate rejections to `None`
/// (its `Option` signature has no error channel): stale snapshot → `None`;
/// tripped circuit → `None`.
#[test]
fn test_get_twap_price_none_when_health_degraded() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());

    record_stale_health(&t, OracleFeedType::Price, &oracle);
    assert_eq!(
        t.contract
            .get_twap_price(&OracleFeedType::Price, &t.token.address),
        None,
        "stale health snapshot must yield None"
    );

    for _ in 1..crate::oracle_registry::MAX_CONSECUTIVE_STALE_QUERIES {
        record_stale_health(&t, OracleFeedType::Price, &oracle);
    }
    assert!(t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Price, &t.token.address));
    assert_eq!(
        t.contract
            .get_twap_price(&OracleFeedType::Price, &t.token.address),
        None,
        "tripped circuit must yield None"
    );
}

/// Price oracle that answers every query with 10_000 (1.00 USD in bps) —
/// used to observe whether `get_contract_stats` normalized with it.
#[contract]
struct MockPriceOracle;

#[contractimpl]
impl MockPriceOracle {
    pub fn get_price(_env: Env, _token: Address) -> i128 {
        10_000
    }
}

/// `get_contract_stats` must stop USD-normalizing a token's volume once the
/// `(Price, token)` health gate rejects the feed — the bare legacy
/// `invoke_contract` bypass this regression test guards against would keep
/// trusting (or panic on) the degraded oracle.
#[test]
fn test_contract_stats_skips_stale_price_normalization() {
    let t = setup();
    let price_oracle = t.env.register_contract(None, MockPriceOracle);
    t.contract.set_price_oracle(&price_oracle);

    let token = t.token.address.clone();
    t.env.as_contract(&t.contract.address, || {
        crate::invoice::add_volume(&t.env, &token, 10_000);
    });

    // Healthy (no health record yet): volume × 10_000 bps / 10_000 = 10_000.
    let stats = t.contract.get_contract_stats();
    assert_eq!(
        stats.total_volume_usd_normalized, 10_000,
        "healthy feed must be used for USD normalization"
    );

    // Degrade the (Price, token) feed → normalization skipped, raw volume
    // untouched.
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    record_stale_health(&t, OracleFeedType::Price, &oracle);
    let stats = t.contract.get_contract_stats();
    assert_eq!(
        stats.total_volume_usd_normalized, 0,
        "stale feed must not be trusted for USD normalization"
    );
    let mut raw = None;
    for (addr, vol) in stats.token_volumes.iter() {
        if addr == token {
            raw = Some(vol);
        }
    }
    assert_eq!(raw, Some(10_000), "raw per-token volume must be preserved");
}

// ----------------------------------------------------------------
// Issue #859 — rate-limit coverage on oracle-registry admin fns
// ----------------------------------------------------------------

/// First-ever call of a rate-limited admin fn is allowed; an immediate
/// repeat is `RateLimited`; after the cooldown the call succeeds again.
#[test]
fn test_oracle_admin_functions_rate_limited() {
    let t = setup();
    let o1 = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    let o2 = deploy_mock_oracle(&t, true, t.env.ledger().sequence());

    t.contract.register_oracle(&OracleFeedType::Identity, &o1);
    let res = t
        .contract
        .try_register_oracle(&OracleFeedType::Identity, &o2);
    assert_eq!(
        res,
        Err(Ok(crate::errors::ContractError::RateLimited)),
        "second register_oracle within cooldown must be RateLimited"
    );
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.register_oracle(&OracleFeedType::Identity, &o2);

    // Independent key: add_price_source is limited on its own clock.
    t.contract.add_price_source(&OracleFeedType::Price, &o1);
    let res = t.contract.try_add_price_source(&OracleFeedType::Price, &o2);
    assert_eq!(
        res,
        Err(Ok(crate::errors::ContractError::RateLimited)),
        "second add_price_source within cooldown must be RateLimited"
    );
}

// ── Issue #1047: multi-oracle Identity quorum ────────────────────────────────
//
// Before #1047, fund_invoice's payer-verification gate trusted a *single*
// Identity oracle's boolean outright, so one compromised/mistaken feeder could
// mint funding eligibility on its own and feeder disagreement was neither
// consulted nor observable. These tests cover the multi-oracle quorum that now
// replaces that single point of trust when governance opts in:
//   1. Success — quorum met (all, or a sufficient subset, of the feeders agree).
//   2. Failure — disagreement (fewer than N agree) fails closed as PayerUnverified
//      and emits OracleFeederDisagreement.
//   3. Robustness — a reverting feeder contributes no agreement and cannot force
//      verification through.
//   4. Ledger boundary — a feeder whose data is exactly at the max age does not
//      count; a fresh one at the boundary does.
//   5. Authorization — add/remove/set-quorum are admin-gated.
//   6. Preserved behavior — an empty quorum list falls back to the unchanged
//      single-oracle path.
//   7. Bounds — the list is capped and the quorum threshold is range-checked.

/// A feeder that always traps on `get_payer_data` — models an unavailable or
/// misconfigured oracle. It must degrade to "no opinion", never force
/// verification through.
#[contract]
struct MockRevertingOracle;

#[contractimpl]
impl MockRevertingOracle {
    pub fn get_payer_data(_env: Env, _payer: Address) -> OracleVerificationResponse {
        panic!("feeder unavailable")
    }
}

/// Register each of `oracles` into `feed_type`'s Identity quorum list, spacing
/// the calls past the rate-limit cooldown (the registry admin fns are all
/// rate-limited with the same `DEFAULT_RATE_LIMIT_LEDGERS` cooldown).
fn add_identity_oracles(t: &crate::test::TestEnv, oracles: &[Address]) {
    for (i, oracle) in oracles.iter().enumerate() {
        if i > 0 {
            advance_past_rate_limit_cooldown(&t.env);
        }
        t.contract
            .add_identity_oracle(&OracleFeedType::Identity, oracle);
    }
}

#[test]
fn test_identity_quorum_default_is_one() {
    let t = setup();
    assert_eq!(t.contract.get_identity_quorum(), 1);
    assert!(t
        .contract
        .get_identity_oracles(&OracleFeedType::Identity)
        .is_empty());
}

#[test]
fn test_identity_quorum_met_funds_invoice() {
    let t = setup();
    let seq = t.env.ledger().sequence();
    let a = deploy_mock_oracle(&t, true, seq);
    let b = deploy_mock_oracle(&t, true, seq);
    add_identity_oracles(&t, &[a, b]);
    t.contract.set_identity_quorum(&2);

    let invoice_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

    assert_eq!(
        t.contract.get_invoice(&invoice_id).status,
        InvoiceStatus::Funded
    );
}

#[test]
fn test_identity_quorum_met_by_subset_of_three() {
    let t = setup();
    let seq = t.env.ledger().sequence();
    let yes_a = deploy_mock_oracle(&t, true, seq);
    let yes_b = deploy_mock_oracle(&t, true, seq);
    let no = deploy_mock_oracle(&t, false, seq);
    add_identity_oracles(&t, &[yes_a, yes_b, no]);
    // Two of three agree — quorum of 2 is met despite the dissenting feeder.
    t.contract.set_identity_quorum(&2);

    let invoice_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

    assert_eq!(
        t.contract.get_invoice(&invoice_id).status,
        InvoiceStatus::Funded
    );
}

#[test]
fn test_identity_quorum_disagreement_fails_closed() {
    let t = setup();
    let seq = t.env.ledger().sequence();
    let yes = deploy_mock_oracle(&t, true, seq);
    let no = deploy_mock_oracle(&t, false, seq);
    add_identity_oracles(&t, &[yes, no]);
    t.contract.set_identity_quorum(&2);

    let invoice_id = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        res,
        Err(Ok(ContractError::PayerUnverified)),
        "feeders that disagree must fail closed, not fund"
    );
}

#[test]
fn test_identity_quorum_reverting_feeder_cannot_force_verification() {
    let t = setup();
    let seq = t.env.ledger().sequence();
    let yes = deploy_mock_oracle(&t, true, seq);
    let reverting = t.env.register_contract(None, MockRevertingOracle);
    add_identity_oracles(&t, &[yes, reverting]);
    t.contract.set_identity_quorum(&2);

    let invoice_id = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        res,
        Err(Ok(ContractError::PayerUnverified)),
        "an unavailable feeder contributes no agreement and must not force verification"
    );
}

#[test]
fn test_identity_quorum_ledger_boundary_excludes_stale_feeder() {
    let t = setup();
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.set_max_oracle_age(&10);

    // Placeholder timestamps; pinned relative to the current ledger below,
    // after the rate-limit advances that registration performs.
    let a = deploy_mock_oracle(&t, true, 0);
    let b = deploy_mock_oracle(&t, true, 0);
    add_identity_oracles(&t, &[a, b]);
    t.contract.set_identity_quorum(&2);

    let current = t.env.ledger().sequence();
    // Fresh side of the boundary: age == max_age - 1 → counts.
    MockRegistryOracleClient::new(&t.env, &a).set_response(&true, &(current - 9));
    // Stale side of the boundary: age == max_age → does NOT count.
    MockRegistryOracleClient::new(&t.env, &b).set_response(&true, &(current - 10));

    let invoice_id = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        res,
        Err(Ok(ContractError::PayerUnverified)),
        "a feeder whose data sits exactly at the max age must not count toward quorum"
    );

    // Refresh the second feeder to the current ledger — both are now fresh and
    // the quorum is met, so funding proceeds.
    MockRegistryOracleClient::new(&t.env, &b).set_response(&true, &current);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.contract.get_invoice(&invoice_id).status,
        InvoiceStatus::Funded
    );
}

#[test]
fn test_identity_quorum_disagreement_emits_event() {
    let t = setup();
    let seq = t.env.ledger().sequence();
    let yes = deploy_mock_oracle(&t, true, seq);
    let no = deploy_mock_oracle(&t, false, seq);
    add_identity_oracles(&t, &[yes, no]);
    t.contract.set_identity_quorum(&2);

    // Clear any events from the setup, then invoke the gate directly so the
    // disagreement event commits (a failed fund_invoice call would roll it back).
    t.env.events().all();
    let outcome = t.env.as_contract(&t.contract.address, || {
        crate::oracle_registry::check_identity_quorum(
            &t.env,
            OracleFeedType::Identity,
            &t.token.address,
            &t.payer,
            DEFAULT_MAX_ORACLE_AGE_LEDGERS,
        )
    });
    assert_eq!(
        outcome,
        crate::oracle_registry::IdentityQuorumOutcome::Disagreement
    );

    let events = t.env.events().all();
    let disagreement: std::vec::Vec<_> = events
        .events()
        .iter()
        .filter(|e| std::format!("{:?}", e).contains("oracle_feeder_disagreement"))
        .collect();
    assert_eq!(
        disagreement.len(),
        1,
        "a failed quorum must emit exactly one OracleFeederDisagreement event"
    );
}

#[test]
fn test_empty_identity_quorum_list_uses_single_oracle_path() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    // Register then remove from the quorum list — an empty list must fall back
    // to the unchanged single-oracle resolution path.
    t.contract
        .add_identity_oracle(&OracleFeedType::Identity, &oracle);
    advance_past_rate_limit_cooldown(&t.env);
    t.contract
        .remove_identity_oracle(&OracleFeedType::Identity, &oracle);
    assert!(t
        .contract
        .get_identity_oracles(&OracleFeedType::Identity)
        .is_empty());

    let invoice_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.contract.get_invoice(&invoice_id).status,
        InvoiceStatus::Funded
    );
}

#[test]
fn test_set_identity_quorum_rejects_out_of_range() {
    use crate::constants::MAX_IDENTITY_ORACLES_PER_FEED;
    let t = setup();
    assert_eq!(
        t.contract.try_set_identity_quorum(&0),
        Err(Ok(ContractError::InvalidIdentityQuorum))
    );
    advance_past_rate_limit_cooldown(&t.env);
    assert_eq!(
        t.contract
            .try_set_identity_quorum(&(MAX_IDENTITY_ORACLES_PER_FEED + 1)),
        Err(Ok(ContractError::InvalidIdentityQuorum))
    );
    // Bounds themselves are accepted.
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.set_identity_quorum(&MAX_IDENTITY_ORACLES_PER_FEED);
    assert_eq!(
        t.contract.get_identity_quorum(),
        MAX_IDENTITY_ORACLES_PER_FEED
    );
}

#[test]
fn test_add_identity_oracle_bounded_and_idempotent() {
    use crate::constants::MAX_IDENTITY_ORACLES_PER_FEED;
    let t = setup();

    // Idempotent: re-adding the same oracle does not grow the list.
    let first = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .add_identity_oracle(&OracleFeedType::Identity, &first);
    advance_past_rate_limit_cooldown(&t.env);
    t.contract
        .add_identity_oracle(&OracleFeedType::Identity, &first);
    assert_eq!(
        t.contract
            .get_identity_oracles(&OracleFeedType::Identity)
            .len(),
        1
    );

    // Fill to the declared cap (registration performs no invoke, so arbitrary
    // addresses are fine to use as stand-ins).
    for _ in 1..MAX_IDENTITY_ORACLES_PER_FEED {
        advance_past_rate_limit_cooldown(&t.env);
        t.contract
            .add_identity_oracle(&OracleFeedType::Identity, &Address::generate(&t.env));
    }
    assert_eq!(
        t.contract
            .get_identity_oracles(&OracleFeedType::Identity)
            .len(),
        MAX_IDENTITY_ORACLES_PER_FEED
    );

    // One more is rejected — the cap holds even with an authorised admin.
    advance_past_rate_limit_cooldown(&t.env);
    let extra = Address::generate(&t.env);
    assert_eq!(
        t.contract
            .try_add_identity_oracle(&OracleFeedType::Identity, &extra),
        Err(Ok(ContractError::TooManyIdentityOracles))
    );
}

#[test]
fn test_add_identity_oracle_unauthorized_caller() {
    let (env, _admin, _token, client) = setup_env_no_mock_auths();
    let imposter = Address::generate(&env);
    let oracle = Address::generate(&env);

    env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "add_identity_oracle",
            args: (OracleFeedType::Identity, oracle.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    assert!(
        client
            .try_add_identity_oracle(&OracleFeedType::Identity, &oracle)
            .is_err(),
        "add_identity_oracle should fail for a non-admin caller"
    );
}

#[test]
fn test_set_identity_quorum_unauthorized_caller() {
    let (env, _admin, _token, client) = setup_env_no_mock_auths();
    let imposter = Address::generate(&env);

    env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "set_identity_quorum",
            args: (2u32,).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    assert!(
        client.try_set_identity_quorum(&2).is_err(),
        "set_identity_quorum should fail for a non-admin caller"
    );
}

// ── Issue #1048: recover after feeder outage ─────────────────────────────────
//
// The circuit breaker above trips after MAX_CONSECUTIVE_STALE_QUERIES stale
// observations, but before this issue could only be cleared by a governance
// `reset_oracle_circuit` — so a transient feeder outage jammed oracle-gated
// funding until a (slow) governance action, and a genuinely recovered feeder
// had no on-chain path to un-jam it. These tests cover the half-open
// auto-recovery that lets a *sustained* run of healthy observations from the
// tripping oracle close the breaker again, while keeping the anti-flapping
// guarantees:
//   1. Success — sustained healthy observations auto-recover; funding resumes
//      with no admin call.
//   2. Failure — a single fresh query, too few observations, an insufficient
//      ledger window, a frozen (non-advancing) oracle, or a stale observation
//      mid-streak all fail to recover.
//   3. Isolation — a *different* healthy oracle cannot clear the breaker.
//   4. Ledger boundary — recovery needs N observations AND >= 120 ledgers.
//   5. Authorization — auto-recovery is driven by the permissionless
//      check_oracle_health; reset_oracle_circuit stays admin-only and also
//      clears recovery progress.
//   6. Bounded flapping — a recovered oracle that goes stale again re-trips.

/// Advance the ledger by `n` sequences (timestamp scaled 5s/ledger, matching
/// the suite's other helpers).
fn advance_ledgers(env: &Env, n: u32) {
    let mut info = env.ledger().get();
    info.sequence_number += n;
    info.timestamp += (n as u64) * 5;
    env.ledger().set(info);
}

/// Trip the Identity breaker for `t.token` against a single stale feed-type
/// default, returning the oracle address — the same shape the existing circuit
/// tests set up.
fn trip_identity_circuit(t: &crate::test::TestEnv) -> Address {
    let oracle = setup_stale_default_oracle(t);
    for _ in 0..crate::oracle_registry::MAX_CONSECUTIVE_STALE_QUERIES {
        t.contract
            .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer);
    }
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "setup must leave the Identity breaker tripped"
    );
    oracle
}

/// Serve a fresh, timestamp-advancing response from `oracle` at the current
/// ledger and record one health observation (the keeper path that drives
/// Issue #1048 recovery).
fn observe_fresh(t: &crate::test::TestEnv, oracle: &Address) {
    let client = MockRegistryOracleClient::new(&t.env, oracle);
    client.set_response(&true, &t.env.ledger().sequence());
    t.contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer);
}

#[test]
fn test_circuit_auto_recovers_after_sustained_health() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);
    let window = crate::oracle_registry::CIRCUIT_RECOVERY_MIN_LEDGERS as u32;
    let needed = crate::oracle_registry::CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS;

    // First healthy observation — a single fresh query must not recover.
    observe_fresh(&t, &oracle);
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "a single fresh query must not auto-recover the breaker"
    );

    // Build the streak, spacing observations past the minimum ledger window.
    for _ in 1..needed {
        advance_ledgers(&t.env, window);
        observe_fresh(&t, &oracle);
    }
    assert!(
        !t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "a sustained run of healthy observations must auto-recover the breaker"
    );

    // Funding resumes against the recovered oracle with no governance reset.
    let invoice_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.contract.get_invoice(&invoice_id).status,
        InvoiceStatus::Funded
    );
}

#[test]
fn test_circuit_auto_recovery_needs_full_ledger_window() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);
    let needed = crate::oracle_registry::CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS;

    // Enough healthy observations, but only one ledger apart — below
    // CIRCUIT_RECOVERY_MIN_LEDGERS, so the breaker must stay tripped.
    for _ in 0..needed {
        observe_fresh(&t, &oracle);
        assert!(
            t.contract
                .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
            "recovery must not complete before the ledger window elapses"
        );
        advance_ledgers(&t.env, 1);
    }
}

#[test]
fn test_circuit_auto_recovery_needs_full_observation_count() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);
    let window = crate::oracle_registry::CIRCUIT_RECOVERY_MIN_LEDGERS as u32;
    let needed = crate::oracle_registry::CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS;

    // Observations spanning well past the ledger window but one short of the
    // required count — still not recovered.
    for _ in 0..(needed - 1) {
        observe_fresh(&t, &oracle);
        advance_ledgers(&t.env, window);
    }
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "fewer than CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS must not recover"
    );

    // The final observation completes the streak.
    observe_fresh(&t, &oracle);
    assert!(
        !t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address)
    );
}

#[test]
fn test_circuit_auto_recovery_rejects_frozen_oracle() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);
    let needed = crate::oracle_registry::CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS;

    // Re-serve the same fresh value repeatedly (within the max-age window):
    // non-advancing data is not evidence of recovery.
    let frozen = t.env.ledger().sequence();
    let client = MockRegistryOracleClient::new(&t.env, &oracle);
    client.set_response(&true, &frozen);
    for _ in 0..(needed + 1) {
        t.contract
            .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer);
        advance_ledgers(&t.env, 1);
    }
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "a frozen (non-advancing) oracle must not recover the breaker"
    );
}

#[test]
fn test_circuit_auto_recovery_resets_on_stale_observation() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);
    let window = crate::oracle_registry::CIRCUIT_RECOVERY_MIN_LEDGERS as u32;
    let client = MockRegistryOracleClient::new(&t.env, &oracle);

    // One healthy observation, then a stale one mid-streak resets progress...
    observe_fresh(&t, &oracle);
    advance_ledgers(&t.env, window);
    client.set_response(&true, &0); // ancient timestamp => stale
    t.contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer);
    // ...so a single healthy observation afterwards must not recover.
    advance_ledgers(&t.env, window);
    observe_fresh(&t, &oracle);
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "a stale observation must reset the recovery streak"
    );
}

#[test]
fn test_circuit_auto_recovery_ignores_different_oracle() {
    let t = setup();
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.set_max_oracle_age(&10);

    // A stale feed-type default trips the breaker.
    let stale = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &stale);
    advance_ledgers(&t.env, 20);
    for _ in 0..crate::oracle_registry::MAX_CONSECUTIVE_STALE_QUERIES {
        t.contract
            .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer);
    }
    assert!(t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));

    // A healthy per-token override (higher priority) is then registered, and
    // check_oracle_health observes it...
    advance_past_rate_limit_cooldown(&t.env);
    let healthy = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_token_oracle(&OracleFeedType::Identity, &t.token.address, &healthy);
    let window = crate::oracle_registry::CIRCUIT_RECOVERY_MIN_LEDGERS as u32;
    for _ in 0..crate::oracle_registry::CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS {
        observe_fresh(&t, &healthy);
        advance_ledgers(&t.env, window);
    }

    // ...but the breaker was tripped by `stale`, so a different oracle's
    // health must not clear it.
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "only the oracle that tripped the breaker may auto-recover it"
    );
}

#[test]
fn test_circuit_recovery_state_view_tracks_progress() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);

    // Freshly tripped: pinned to the tripping oracle, streak empty.
    let state = t
        .contract
        .get_circuit_recovery_state(&OracleFeedType::Identity, &t.token.address)
        .unwrap();
    assert_eq!(state.tripped_oracle, oracle);
    assert_eq!(state.consecutive_healthy, 0);
    assert_eq!(state.first_healthy_ledger, 0);

    // After a healthy observation the streak advances and records its start.
    let at = t.env.ledger().sequence() as u64;
    observe_fresh(&t, &oracle);
    let state = t
        .contract
        .get_circuit_recovery_state(&OracleFeedType::Identity, &t.token.address)
        .unwrap();
    assert_eq!(state.consecutive_healthy, 1);
    assert_eq!(state.first_healthy_ledger, at);
}

#[test]
fn test_circuit_auto_recovers_without_admin_reset() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);
    let window = crate::oracle_registry::CIRCUIT_RECOVERY_MIN_LEDGERS as u32;

    for i in 0..crate::oracle_registry::CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS {
        if i > 0 {
            advance_ledgers(&t.env, window);
        }
        observe_fresh(&t, &oracle);
    }

    // No admin/governance call was made anywhere in this test — recovery is
    // driven entirely by the permissionless check_oracle_health keeper path.
    assert!(!t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));
    assert!(t
        .contract
        .get_circuit_recovery_state(&OracleFeedType::Identity, &t.token.address)
        .is_none());
}

#[test]
fn test_circuit_auto_recovery_emits_event() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);
    let window = crate::oracle_registry::CIRCUIT_RECOVERY_MIN_LEDGERS as u32;

    t.env.events().all(); // clear setup events
    for i in 0..crate::oracle_registry::CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS {
        if i > 0 {
            advance_ledgers(&t.env, window);
        }
        observe_fresh(&t, &oracle);
    }

    let events = t.env.events().all();
    let recovered: std::vec::Vec<_> = events
        .events()
        .iter()
        .filter(|e| std::format!("{:?}", e).contains("oracle_circuit_recovered"))
        .collect();
    assert_eq!(
        recovered.len(),
        1,
        "auto-recovery must emit exactly one OracleCircuitRecovered event"
    );
}

#[test]
fn test_circuit_retrips_after_auto_recovery_if_stale_again() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);
    let window = crate::oracle_registry::CIRCUIT_RECOVERY_MIN_LEDGERS as u32;

    for i in 0..crate::oracle_registry::CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS {
        if i > 0 {
            advance_ledgers(&t.env, window);
        }
        observe_fresh(&t, &oracle);
    }
    assert!(!t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));

    // The recovered oracle goes stale again → the breaker re-trips, so
    // auto-recovery cannot be used to keep a flapping oracle trusted.
    let ancient = t.env.ledger().sequence().saturating_sub(window + 100);
    let client = MockRegistryOracleClient::new(&t.env, &oracle);
    client.set_response(&true, &ancient);
    for _ in 0..crate::oracle_registry::MAX_CONSECUTIVE_STALE_QUERIES {
        t.contract
            .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer);
    }
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "a recovered oracle that goes stale again must re-trip the breaker"
    );
}

#[test]
fn test_reset_oracle_circuit_clears_recovery_progress() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);

    observe_fresh(&t, &oracle);
    assert!(t
        .contract
        .get_circuit_recovery_state(&OracleFeedType::Identity, &t.token.address)
        .is_some());

    t.contract
        .reset_oracle_circuit(&OracleFeedType::Identity, &t.token.address);

    assert!(!t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));
    assert!(
        t.contract
            .get_circuit_recovery_state(&OracleFeedType::Identity, &t.token.address)
            .is_none(),
        "the governance reset must also clear half-open recovery progress"
    );
}

// ── Issue #1049: emit stale and quorum failure events ────────────────────────
//
// `fund_invoice`'s Identity gate detects stale data and feeder disagreement, but
// it reports each failure by returning `Err`. Soroban rolls back every event of
// an `Err`-returning invocation, so those failures leave no on-chain trace —
// monitoring cannot tell "payer unverified" from "oracle is stale" from "feeders
// disagree". These tests cover the fix: a shared `evaluate_identity_verification`
// helper backs both the funding gate (which keeps returning the same errors) and
// a new permissionless, never-erroring `check_oracle_verification` diagnostic
// whose events commit.
//   1. Success — a fresh, verified payer passes (single-oracle and quorum).
//   2. Failure — stale data emits `oracle_stale_data_rejected`; feeder
//      disagreement emits `oracle_feeder_disagreement`; an unverified payer
//      reports `Unverified`.
//   3. Before/after — a rejected `fund_invoice` returns `OracleDataStale` but
//      leaves no event; the diagnostic reaches the same decision and its event
//      survives.
//   4. Ledger boundary — `age == max_age - 1` is fresh, `age == max_age` is stale.
//   5. Authorization — the diagnostic is callable by anyone, with no auth.
//   6. Interop with #1048 — the diagnostic records health, so repeated stale
//      queries still trip the breaker and then report `CircuitOpen`.

/// Count events whose debug rendering contains `needle` (matches the existing
/// event assertions in this suite, which key off the published symbol).
fn count_events(env: &Env, needle: &str) -> usize {
    env.events()
        .all()
        .events()
        .iter()
        .filter(|e| std::format!("{:?}", e).contains(needle))
        .count()
}

#[test]
fn test_check_oracle_verification_single_oracle_verified() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    let outcome = t.contract.check_oracle_verification(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(
        outcome,
        crate::oracle_registry::IdentityVerificationOutcome::Verified
    );
}

#[test]
fn test_check_oracle_verification_unverified_payer() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, false, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    let outcome = t.contract.check_oracle_verification(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(
        outcome,
        crate::oracle_registry::IdentityVerificationOutcome::Unverified
    );
}

#[test]
fn test_check_oracle_verification_stale_emits_event() {
    let t = setup();
    setup_stale_default_oracle(&t); // max_age=10, data already 20 ledgers old

    t.env.events().all(); // clear setup events
    let outcome = t.contract.check_oracle_verification(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(
        outcome,
        crate::oracle_registry::IdentityVerificationOutcome::Stale
    );

    assert_eq!(
        count_events(&t.env, "oracle_stale_data_rejected"),
        1,
        "a stale diagnostic must commit exactly one OracleDataStaleRejected event"
    );
}

#[test]
fn test_check_oracle_verification_ledger_boundary() {
    let t = setup();
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.set_max_oracle_age(&10);

    let oracle = deploy_mock_oracle(&t, true, 0);
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    // Fresh side of the boundary: age == max_age - 1 (9) → counts as fresh.
    let current = t.env.ledger().sequence();
    MockRegistryOracleClient::new(&t.env, &oracle).set_response(&true, &(current - 9));
    let outcome = t.contract.check_oracle_verification(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(
        outcome,
        crate::oracle_registry::IdentityVerificationOutcome::Verified,
        "age == max_age - 1 must still be accepted"
    );

    // Advance one ledger: the same data now ages to exactly max_age → stale.
    advance_ledgers(&t.env, 1);
    let outcome = t.contract.check_oracle_verification(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(
        outcome,
        crate::oracle_registry::IdentityVerificationOutcome::Stale,
        "age == max_age must be rejected as stale"
    );
}

#[test]
fn test_check_oracle_verification_quorum_verified() {
    let t = setup();
    let seq = t.env.ledger().sequence();
    let a = deploy_mock_oracle(&t, true, seq);
    let b = deploy_mock_oracle(&t, true, seq);
    add_identity_oracles(&t, &[a, b]);
    t.contract.set_identity_quorum(&2);

    let outcome = t.contract.check_oracle_verification(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(
        outcome,
        crate::oracle_registry::IdentityVerificationOutcome::Verified
    );
}

#[test]
fn test_check_oracle_verification_quorum_disagreement_emits_event() {
    let t = setup();
    let seq = t.env.ledger().sequence();
    let yes = deploy_mock_oracle(&t, true, seq);
    let no = deploy_mock_oracle(&t, false, seq);
    add_identity_oracles(&t, &[yes, no]);
    t.contract.set_identity_quorum(&2);

    t.env.events().all(); // clear setup events
    let outcome = t.contract.check_oracle_verification(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(
        outcome,
        crate::oracle_registry::IdentityVerificationOutcome::QuorumDisagreement
    );

    assert_eq!(
        count_events(&t.env, "oracle_feeder_disagreement"),
        1,
        "a failed quorum must commit exactly one OracleFeederDisagreement event"
    );
}

#[test]
fn test_fund_invoice_stale_rejection_events_roll_back_but_diagnostic_commits() {
    let t = setup();
    setup_stale_default_oracle(&t); // max_age=10, data 20 ledgers old

    // Baseline (before the fix): the funding gate rejects stale data, but the
    // `Err` return rolls every emitted event back — nothing is observable.
    let invoice_id = make_invoice(&t);
    t.env.events().all(); // clear setup events
    let res = t.contract.try_fund_invoice(
        &t.funder,
        &invoice_id,
        &INVOICE_AMOUNT,
        &true,
    );
    assert_eq!(
        res,
        Err(Ok(ContractError::OracleDataStale)),
        "the refactor must preserve the exact funding-gate error"
    );
    assert_eq!(
        count_events(&t.env, "oracle_stale_data_rejected"),
        0,
        "a rejected fund_invoice must not leave the stale event on chain"
    );

    // After the fix: the permissionless diagnostic reaches the same decision and
    // its event — and its health write — commit, so the failure is observable.
    t.env.events().all(); // clear
    let outcome = t.contract.check_oracle_verification(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(
        outcome,
        crate::oracle_registry::IdentityVerificationOutcome::Stale
    );
    assert_eq!(
        count_events(&t.env, "oracle_stale_data_rejected"),
        1,
        "the diagnostic's stale event must survive on chain"
    );
}

#[test]
fn test_check_oracle_verification_is_permissionless() {
    let (env, _admin, token, client) = setup_env_no_mock_auths();

    // Register an Identity oracle while setup's mock_all_auths is still active.
    let oracle = env.register_contract(None, MockRegistryOracle);
    MockRegistryOracleClient::new(&env, &oracle).set_response(&true, &env.ledger().sequence());
    client.register_oracle(&OracleFeedType::Identity, &oracle);

    // Drop every mocked auth: any require_auth() in the call would now fail.
    env.mock_auths(&[]);

    // The diagnostic has no authorisation gate — any caller gets an outcome.
    let outcome = client.check_oracle_verification(
        &OracleFeedType::Identity,
        &token,
        &Address::generate(&env),
    );
    assert_eq!(
        outcome,
        crate::oracle_registry::IdentityVerificationOutcome::Verified,
        "check_oracle_verification must be callable without any authorization"
    );
}

#[test]
fn test_check_oracle_verification_records_health_and_reports_circuit_open() {
    let t = setup();
    setup_stale_default_oracle(&t); // max_age=10, data 20 ledgers old

    // The diagnostic shares `record_oracle_health` with the funding gate, so
    // repeated stale observations advance the same streak and trip the breaker.
    for _ in 0..crate::oracle_registry::MAX_CONSECUTIVE_STALE_QUERIES {
        let outcome = t.contract.check_oracle_verification(
            &OracleFeedType::Identity,
            &t.token.address,
            &t.payer,
        );
        assert_eq!(
            outcome,
            crate::oracle_registry::IdentityVerificationOutcome::Stale
        );
    }
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "repeated stale diagnostics must trip the breaker via record_oracle_health"
    );

    // Once the breaker is open the diagnostic reports it — never a stale event,
    // since nothing is queried.
    t.env.events().all();
    let outcome = t.contract.check_oracle_verification(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(
        outcome,
        crate::oracle_registry::IdentityVerificationOutcome::CircuitOpen
    );
    assert_eq!(
        count_events(&t.env, "oracle_stale_data_rejected"),
        0,
        "an open breaker short-circuits before any oracle is queried"
    );
}

// ── Issue #1054: compromised feeder data ────────────────────────────────────
//
// A "compromised feeder" is an Identity oracle whose operator key — or the node
// itself — has been taken over, so its `get_payer_data` payload is
// attacker-controlled while it remains an *authorized* registry entry. The
// registry never trusts that payload blindly; this section pins each defense
// against a hostile feeder and, importantly, documents the one case that only
// governance can cover.
//   1. Isolation — a single compromised feeder cannot mint funding eligibility:
//      the honest quorum disagrees and the gate fails closed.
//   2. Availability — a compromised feeder that goes silent/reverts cannot jam
//      an honest quorum.
//   3. Boundary (documented) — a compromised *majority* satisfies the quorum;
//      only governance removing a rogue feeder restores safety.
//   4. Replay — a captured-but-now-stale attestation is rejected by freshness
//      (the walk goes fresh → stale as the ledger advances).
//   5. Clock skew — a future-dated timestamp is treated as stale.
//   6. Monotonicity — a regression to an older timestamp is rejected (#1052).
//   7. Freeze — re-serving one identical snapshot is bounded (#1053).
//   8. Quarantine — repeated stale observations advance the breaker only via
//      the permissionless keeper path; a rejected `fund_invoice` rolls back.
//   9. Authorization — a rogue feeder cannot join the quorum without admin.
//
// The hostile vectors reuse the `MockRegistryOracle` above (arbitrary
// `is_verified` + `timestamp`) and `MockRevertingOracle`; a compromised feeder
// is simply one whose response we set to a hostile value.

#[test]
fn test_1054_compromised_feeder_cannot_lone_mint_eligibility() {
    let t = setup();
    let seq = t.env.ledger().sequence();
    // Two honest feeders report the payer unverified; the third is compromised
    // and claims the payer is verified, trying to mint eligibility alone.
    let honest_a = deploy_mock_oracle(&t, false, seq);
    let honest_b = deploy_mock_oracle(&t, false, seq);
    let compromised = deploy_mock_oracle(&t, true, seq);
    add_identity_oracles(&t, &[honest_a, honest_b, compromised]);
    t.contract.set_identity_quorum(&2);

    let invoice_id = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        res,
        Err(Ok(ContractError::PayerUnverified)),
        "one compromised feeder must not reach the 2-of-3 quorum on its own"
    );

    // The same decision is observable on the never-erroring keeper path.
    let outcome = t.contract.check_oracle_verification(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(
        outcome,
        crate::oracle_registry::IdentityVerificationOutcome::QuorumDisagreement
    );
}

#[test]
fn test_1054_compromised_feeder_silence_does_not_block_honest_quorum() {
    let t = setup();
    let seq = t.env.ledger().sequence();
    let honest_a = deploy_mock_oracle(&t, true, seq);
    let honest_b = deploy_mock_oracle(&t, true, seq);
    // A compromised feeder that traps contributes no agreement — but it must
    // not jam funding either.
    let silent = t.env.register_contract(None, MockRevertingOracle);
    add_identity_oracles(&t, &[honest_a, honest_b, silent]);
    t.contract.set_identity_quorum(&2);

    let invoice_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.contract.get_invoice(&invoice_id).status,
        InvoiceStatus::Funded,
        "an unavailable feeder must not block the honest quorum"
    );
}

#[test]
fn test_1054_compromised_majority_requires_governance_removal() {
    let t = setup();
    let seq = t.env.ledger().sequence();
    // Two of three feeders are compromised and collude to verify a payer the
    // honest feeder reports unverified.
    let rogue_a = deploy_mock_oracle(&t, true, seq);
    let rogue_b = deploy_mock_oracle(&t, true, seq);
    let honest = deploy_mock_oracle(&t, false, seq);
    add_identity_oracles(&t, &[rogue_a, rogue_b, honest]);
    t.contract.set_identity_quorum(&2);

    // The gate alone cannot stop a compromised majority: 2 of 3 agree.
    let invoice_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.contract.get_invoice(&invoice_id).status,
        InvoiceStatus::Funded,
        "a compromised majority satisfies the quorum — a documented, \
         governance-mitigated boundary rather than a gate guarantee"
    );

    // Governance removing one rogue feeder drops the agreement below quorum,
    // and the gate fails closed again.
    advance_past_rate_limit_cooldown(&t.env);
    t.contract
        .remove_identity_oracle(&OracleFeedType::Identity, &rogue_a);

    let invoice_id = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        res,
        Err(Ok(ContractError::PayerUnverified)),
        "removing one colluding feeder must restore fail-closed behavior"
    );
}

#[test]
fn test_1054_compromised_feeder_stale_replay_rejected() {
    let t = setup();
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.set_max_oracle_age(&10);

    // The feeder captures a valid attestation at `seq`…
    let seq = t.env.ledger().sequence();
    let oracle = deploy_mock_oracle(&t, true, seq);
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    // …and while the data is fresh it is accepted.
    let fresh = t.contract.check_oracle_verification(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(
        fresh,
        crate::oracle_registry::IdentityVerificationOutcome::Verified
    );

    // The attacker later replays that same captured snapshot; freshness defeats
    // it because the data has aged past the configured window.
    advance_ledgers(&t.env, 20);
    let replayed = t.contract.check_oracle_verification(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(
        replayed,
        crate::oracle_registry::IdentityVerificationOutcome::Stale,
        "a replayed snapshot must age out and be rejected"
    );

    // The funding gate rejects the replayed data too.
    let invoice_id = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(res, Err(Ok(ContractError::OracleDataStale)));
}

#[test]
fn test_1054_compromised_feeder_future_timestamp_rejected() {
    let t = setup();
    let current = t.env.ledger().sequence();
    // A compromised feeder stamps its data in the future to try to look
    // perpetually fresh.
    let oracle = deploy_mock_oracle(&t, true, current + 1_000);
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    let outcome = t.contract.check_oracle_verification(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(
        outcome,
        crate::oracle_registry::IdentityVerificationOutcome::Stale,
        "future-dated data must be treated as stale, never as maximally fresh"
    );
}

#[test]
fn test_1054_compromised_feeder_timestamp_regression_rejected() {
    let t = setup();
    advance_past_rate_limit_cooldown(&t.env);
    let base = t.env.ledger().sequence();
    let oracle = deploy_mock_oracle(&t, true, base);
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    // First attestation at `base` is accepted and recorded.
    let first = t.contract.check_oracle_verification(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(
        first,
        crate::oracle_registry::IdentityVerificationOutcome::Verified
    );

    // The compromised feeder then serves an *older* timestamp — a replay of a
    // superseded attestation — which monotonicity rejects.
    MockRegistryOracleClient::new(&t.env, &oracle).set_response(&true, &(base - 1));
    let second = t.contract.check_oracle_verification(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(
        second,
        crate::oracle_registry::IdentityVerificationOutcome::TimestampRegression
    );

    let invoice_id = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(res, Err(Ok(ContractError::OracleTimestampRegression)));
}

#[test]
fn test_1054_compromised_feeder_frozen_snapshot_rejected() {
    let t = setup();
    let cap = crate::oracle_registry::MAX_CONSECUTIVE_DUPLICATE_UPDATES;
    let seq = t.env.ledger().sequence();
    // A frozen (or replaying) feeder keeps re-serving one captured timestamp.
    let oracle = deploy_mock_oracle(&t, true, seq);
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    // Re-serving the same snapshot up to the cap is tolerated…
    for _ in 0..(cap + 1) {
        let outcome = t.contract.check_oracle_verification(
            &OracleFeedType::Identity,
            &t.token.address,
            &t.payer,
        );
        assert_eq!(
            outcome,
            crate::oracle_registry::IdentityVerificationOutcome::Verified
        );
    }
    // …but the next identical update is rejected as a duplicate.
    let outcome = t.contract.check_oracle_verification(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(
        outcome,
        crate::oracle_registry::IdentityVerificationOutcome::DuplicateUpdate,
        "a frozen feeder re-serving one timestamp must be bounded"
    );
}

#[test]
fn test_1054_compromised_feeder_quarantine_needs_keeper_path() {
    let t = setup();
    setup_stale_default_oracle(&t); // max_age=10, data already 20 ledgers old

    // A compromised feeder stuck on stale data makes every `fund_invoice`
    // reject — but each rejection rolls back, so the stale streak never
    // advances and the breaker stays closed.
    for _ in 0..crate::oracle_registry::MAX_CONSECUTIVE_STALE_QUERIES {
        let invoice_id = make_invoice(&t);
        let res = t
            .contract
            .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
        assert_eq!(res, Err(Ok(ContractError::OracleDataStale)));
    }
    assert!(
        !t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "rejected fund_invoice calls roll back, so they cannot advance the stale streak"
    );

    // The permissionless keeper path commits the same observations and trips
    // the breaker, quarantining the compromised feeder.
    for _ in 0..crate::oracle_registry::MAX_CONSECUTIVE_STALE_QUERIES {
        let outcome = t.contract.check_oracle_verification(
            &OracleFeedType::Identity,
            &t.token.address,
            &t.payer,
        );
        assert_eq!(
            outcome,
            crate::oracle_registry::IdentityVerificationOutcome::Stale
        );
    }
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "the keeper path must trip the breaker on sustained stale data"
    );

    // With the breaker open, funding fails closed.
    let invoice_id = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(res, Err(Ok(ContractError::OracleCircuitOpen)));
}

#[test]
fn test_1054_rogue_feeder_cannot_join_quorum_without_admin() {
    let (env, _admin, _token, client) = setup_env_no_mock_auths();

    // An attacker deploys their own oracle and tries to inject it as an
    // Identity feeder / quorum member to mint eligibility.
    let rogue = env.register_contract(None, MockRegistryOracle);
    MockRegistryOracleClient::new(&env, &rogue).set_response(&true, &env.ledger().sequence());
    let imposter = Address::generate(&env);

    env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "add_identity_oracle",
            args: (OracleFeedType::Identity, rogue.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert_eq!(
        client.try_add_identity_oracle(&OracleFeedType::Identity, &rogue),
        Err(Ok(ContractError::Unauthorized)),
        "only admin may add an Identity feeder to the quorum"
    );

    // The attacker also cannot loosen the quorum threshold to let themselves in.
    env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "set_identity_quorum",
            args: (1u32,).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert_eq!(
        client.try_set_identity_quorum(&1),
        Err(Ok(ContractError::Unauthorized)),
        "only admin may change the Identity quorum"
    );
}

// ── Issue #1055: exposing oracle health & quorum ────────────────────────────
//
// Before, answering "is oracle gating for this feed + token healthy?" meant
// fanning out five reads (`get_oracle_health`, `is_oracle_circuit_tripped`,
// `get_circuit_recovery_state`, `get_identity_oracles`, `get_identity_quorum`),
// and the *live* health of a multi-oracle Identity quorum had no reader at all
// (the quorum path never writes `OracleHealth`, so in quorum mode
// `get_oracle_health` reported `None` forever).
//
// After, two additive permissionless views:
//   1. `get_oracle_status` — one cheap call (no cross-contract calls, no
//      writes) with the whole recorded configuration + health state.
//   2. `check_identity_quorum_health` — the live per-feeder health + quorum
//      verdict, sharing the gate's own feeder predicate.
//
// Cases below cover success (healthy/verified), failure (stale / unverified /
// unreachable / disagreeing quorum), authorization (both views answer with no
// signatures at all) and ledger boundaries (the `age == max_age` flip), plus
// the read-only guarantee of the quorum probe.

#[test]
fn test_1055_status_view_unconfigured_feed_is_empty() {
    let t = setup();
    let status = t
        .contract
        .get_oracle_status(&OracleFeedType::Identity, &t.token.address);

    assert_eq!(status.resolved_oracle, None);
    assert!(status.health.is_none(), "never queried → no health recorded");
    assert!(!status.circuit_tripped);
    assert!(status.recovery.is_none());
    assert!(status.identity_oracles.is_empty());
    assert_eq!(
        status.identity_quorum,
        crate::oracle_registry::DEFAULT_IDENTITY_QUORUM,
        "the threshold reports its documented default before governance sets one"
    );
    assert!(status.price_sources.is_empty());
}

#[test]
fn test_1055_status_view_reports_recorded_health_after_query() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    // Resolution is visible immediately; health only once something has
    // actually queried the oracle.
    let before = t
        .contract
        .get_oracle_status(&OracleFeedType::Identity, &t.token.address);
    assert_eq!(before.resolved_oracle, Some(oracle.clone()));
    assert!(before.health.is_none());

    t.contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer);

    let after = t
        .contract
        .get_oracle_status(&OracleFeedType::Identity, &t.token.address);
    let health = after.health.expect("health must be readable after a query");
    assert_eq!(health.oracle, oracle);
    assert!(!health.is_stale);
    assert!(!after.circuit_tripped);
    assert!(after.recovery.is_none());
}

#[test]
fn test_1055_status_view_exposes_open_circuit_and_recovery_progress() {
    let t = setup();
    let stale_oracle = setup_stale_default_oracle(&t);

    for _ in 0..crate::oracle_registry::MAX_CONSECUTIVE_STALE_QUERIES {
        t.contract
            .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer);
    }

    let status = t
        .contract
        .get_oracle_status(&OracleFeedType::Identity, &t.token.address);
    assert!(status.circuit_tripped, "three stale queries must trip it");
    assert_eq!(status.resolved_oracle, Some(stale_oracle.clone()));

    // The half-open recovery record is part of the same view, so an operator
    // can see *which* oracle is on the hook to recover it without a second call.
    let recovery = status
        .recovery
        .expect("a tripped breaker pins its tripped oracle for recovery");
    assert_eq!(recovery.tripped_oracle, stale_oracle);
    assert_eq!(
        recovery.consecutive_healthy, 0,
        "no healthy observation has been served yet"
    );
}

#[test]
fn test_1055_status_view_exposes_quorum_configuration() {
    let t = setup();
    let seq = t.env.ledger().sequence();
    let a = deploy_mock_oracle(&t, true, seq);
    let b = deploy_mock_oracle(&t, true, seq);
    let c = deploy_mock_oracle(&t, true, seq);
    add_identity_oracles(&t, &[a.clone(), b.clone(), c.clone()]);
    t.contract.set_identity_quorum(&2);

    let status = t
        .contract
        .get_oracle_status(&OracleFeedType::Identity, &t.token.address);
    assert_eq!(status.identity_oracles.len(), 3);
    assert_eq!(status.identity_oracles.get(0), Some(a));
    assert_eq!(status.identity_oracles.get(1), Some(b));
    assert_eq!(status.identity_oracles.get(2), Some(c));
    assert_eq!(status.identity_quorum, 2);

    // The threshold is a single protocol-wide setting, so it is reported for
    // every feed; only the feeder list is Identity-specific.
    let price_status = t
        .contract
        .get_oracle_status(&OracleFeedType::Price, &t.token.address);
    assert!(price_status.identity_oracles.is_empty());
    assert_eq!(price_status.identity_quorum, 2);
}

#[test]
fn test_1055_quorum_health_reports_per_feeder_observations() {
    let t = setup();
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.set_max_oracle_age(&10);

    // Register first (each registration advances the ledger), then pin the
    // timestamps so freshness is relative to the ledger the probe runs at.
    let yes = deploy_mock_oracle(&t, true, 0);
    let no = deploy_mock_oracle(&t, false, 0);
    let stale_one = deploy_mock_oracle(&t, true, 0);
    add_identity_oracles(&t, &[yes.clone(), no.clone(), stale_one.clone()]);
    t.contract.set_identity_quorum(&2);

    let now = t.env.ledger().sequence();
    MockRegistryOracleClient::new(&t.env, &yes).set_response(&true, &now);
    MockRegistryOracleClient::new(&t.env, &no).set_response(&false, &now);
    MockRegistryOracleClient::new(&t.env, &stale_one).set_response(&true, &(now - 50));

    let qh = t.contract.check_identity_quorum_health(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(qh.feeders.len(), 3);
    assert_eq!(qh.observations.get(0), Some(FeederHealth::Verified));
    assert_eq!(qh.observations.get(1), Some(FeederHealth::Unverified));
    assert_eq!(qh.observations.get(2), Some(FeederHealth::Stale));
    assert_eq!(qh.verified_count, 1);
    assert_eq!(qh.required_quorum, 2);
    assert!(!qh.quorum_met, "1 of 3 agreeing is below the 2-of-3 threshold");

    // The verdict tracks the funding gate exactly.
    assert_eq!(
        t.contract.check_oracle_verification(
            &OracleFeedType::Identity,
            &t.token.address,
            &t.payer
        ),
        crate::oracle_registry::IdentityVerificationOutcome::QuorumDisagreement
    );
}

#[test]
fn test_1055_quorum_health_reports_unreachable_feeder() {
    let t = setup();
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.set_max_oracle_age(&10);

    let honest = deploy_mock_oracle(&t, true, 0);
    // A feeder that traps contributes no opinion — and must be *reported* as
    // such rather than propagating an error out of the diagnostic.
    let silent = t.env.register_contract(None, MockRevertingOracle);
    add_identity_oracles(&t, &[honest.clone(), silent.clone()]);
    t.contract.set_identity_quorum(&2);

    let now = t.env.ledger().sequence();
    MockRegistryOracleClient::new(&t.env, &honest).set_response(&true, &now);

    let qh = t.contract.check_identity_quorum_health(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(qh.observations.get(0), Some(FeederHealth::Verified));
    assert_eq!(qh.observations.get(1), Some(FeederHealth::Unreachable));
    assert_eq!(qh.verified_count, 1);
    assert!(
        !qh.quorum_met,
        "an unreachable feeder cannot contribute, so 1 of 2 is below quorum"
    );
}

#[test]
fn test_1055_quorum_health_freshness_boundary_matches_gate() {
    let t = setup();
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.set_max_oracle_age(&10);

    let feeder = deploy_mock_oracle(&t, true, 0);
    add_identity_oracles(&t, &[feeder.clone()]);
    t.contract.set_identity_quorum(&1);

    // Last-fresh ledger: age == max_age - 1 (9 < 10) → Verified.
    let now = t.env.ledger().sequence();
    MockRegistryOracleClient::new(&t.env, &feeder).set_response(&true, &(now - 9));
    let fresh = t.contract.check_identity_quorum_health(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(fresh.observations.get(0), Some(FeederHealth::Verified));
    assert!(fresh.quorum_met, "age 9 is inside the 10-ledger window");

    // Cross one ledger: age == max_age == 10 → Stale, and the quorum flips with
    // it (staleness is `age >= max_age`, so this is the exact boundary).
    let mut info = t.env.ledger().get();
    info.sequence_number += 1;
    info.timestamp += 5;
    t.env.ledger().set(info);

    let aged = t.contract.check_identity_quorum_health(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(aged.observations.get(0), Some(FeederHealth::Stale));
    assert!(!aged.quorum_met, "age 10 hits the stale boundary exactly");

    // Same flip, same ledger, through the funding gate's own decision.
    assert_eq!(
        t.contract.check_oracle_verification(
            &OracleFeedType::Identity,
            &t.token.address,
            &t.payer
        ),
        crate::oracle_registry::IdentityVerificationOutcome::QuorumDisagreement
    );
}

#[test]
fn test_1055_quorum_health_empty_list_defers_to_single_oracle_chain() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    let qh = t.contract.check_identity_quorum_health(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert!(qh.feeders.is_empty());
    assert!(qh.observations.is_empty());
    assert_eq!(qh.verified_count, 0);
    assert!(
        !qh.quorum_met,
        "no quorum is in use, so the metric is false — not a rejection signal"
    );

    // And funding is not blocked: with no feeder list the single-oracle chain
    // still governs (the pre-#1047 behavior is preserved).
    assert_eq!(
        t.contract.check_oracle_verification(
            &OracleFeedType::Identity,
            &t.token.address,
            &t.payer
        ),
        crate::oracle_registry::IdentityVerificationOutcome::Verified
    );
}

#[test]
fn test_1055_quorum_health_probe_is_read_only() {
    let t = setup();
    let seq = t.env.ledger().sequence();
    // Two feeders configured, neither verified: the quorum fails closed.
    let a = deploy_mock_oracle(&t, false, seq);
    let b = deploy_mock_oracle(&t, false, seq);
    add_identity_oracles(&t, &[a, b]);
    t.contract.set_identity_quorum(&1);

    let qh = t.contract.check_identity_quorum_health(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert!(!qh.quorum_met);

    // Probing must not manufacture the disagreement event (that belongs to the
    // funding gate, #1049) nor write the single-oracle health snapshot — it is
    // a pure reader, so polling it cannot perturb what it observes.
    assert_eq!(
        count_events(&t.env, "oracle_feeder_disagreement"),
        0,
        "the health probe must not emit the gate's disagreement event"
    );
    assert_eq!(
        t.contract
            .get_oracle_health(&OracleFeedType::Identity, &t.token.address),
        None,
        "the quorum probe must not write the single-oracle health key"
    );
    assert!(
        !t.contract
            .get_oracle_status(&OracleFeedType::Identity, &t.token.address)
            .circuit_tripped,
        "probing health must not trip the circuit breaker"
    );
}

#[test]
fn test_1055_views_are_permissionless_and_pause_safe() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);
    add_identity_oracles(&t, &[oracle.clone()]);

    t.contract.pause();

    // Both views are read-only monitors: neither is gated on `is_paused`, and
    // the quorum probe still performs its live cross-contract call.
    let status = t
        .contract
        .get_oracle_status(&OracleFeedType::Identity, &t.token.address);
    assert_eq!(status.resolved_oracle, Some(oracle.clone()));
    assert_eq!(status.identity_oracles.len(), 1);

    let qh = t.contract.check_identity_quorum_health(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );
    assert_eq!(qh.observations.get(0), Some(FeederHealth::Verified));
    assert!(qh.quorum_met);
}

#[test]
fn test_1055_views_require_no_authorization() {
    let (env, _admin, token, client) = setup_env_no_mock_auths();
    let payer = Address::generate(&env);

    // Mock an *empty* authorization set: any `require_auth` would now fail.
    env.mock_auths(&[]);

    let status = client.get_oracle_status(&OracleFeedType::Identity, &token);
    assert_eq!(status.resolved_oracle, None);

    let qh = client.check_identity_quorum_health(&OracleFeedType::Identity, &token, &payer);
    assert!(qh.feeders.is_empty());
    assert!(!qh.quorum_met);
}

// ── Issue #1056: fail-closed oracle behavior (documented) ───────────────────
//
// "Fail-closed" is the guarantee these tests pin down: **once an Identity
// oracle is configured for a token, a payer funds only when the gate can
// positively verify them with fresh data.** Every way of *failing* to verify —
// stale data, a regressed or frozen timestamp, feeder disagreement, an open
// breaker, or a plain `is_verified = false` — rejects funding instead of
// falling through to the legacy fail-open default. The one deliberate
// exception, preserved unchanged, is the truly-unconfigured case (no per-token
// override, no feed-type default, no legacy `price_oracle`): with nothing to
// consult the gate stays fail-open.
//
// The gate is `oracle_registry::evaluate_identity_verification`, shared by
// `fund_invoice` (which maps each outcome to a `ContractError`) and the
// permissionless, never-erroring `check_oracle_verification` diagnostic. These
// tests cover the documented contract of that behavior: the success path, the
// fail-open exception, each rejection class, the ledger-boundary exactly where
// staleness flips, atomicity of a rejection, gate/diagnostic agreement, and the
// fact that oracle verification never substitutes for funder authorization.
//
// Note: the audit citation for this issue (`lib.rs:3832-3847`) points at the
// dispute-resolution ("Upheld") refund block, not oracle logic. The real
// fail-closed path is the `require_oracle_verification` block in `fund_invoice`
// (`lib.rs:2656-2711`), which delegates to `evaluate_identity_verification`.

#[test]
fn test_1056_fail_closed_success_verified_payer_funds() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    let invoice_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.contract.get_invoice(&invoice_id).status,
        InvoiceStatus::Funded,
        "a positively verified payer with fresh data must fund"
    );

    // The gate and the diagnostic agree on the happy path.
    assert_eq!(
        t.contract.check_oracle_verification(
            &OracleFeedType::Identity,
            &t.token.address,
            &t.payer
        ),
        crate::oracle_registry::IdentityVerificationOutcome::Verified
    );
}

#[test]
fn test_1056_fail_open_is_preserved_only_when_nothing_is_configured() {
    let t = setup();
    // No per-token override, no feed-type default, no legacy price_oracle.
    let invoice_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.contract.get_invoice(&invoice_id).status,
        InvoiceStatus::Funded,
        "with nothing configured the gate is fail-open, as documented"
    );
    assert_eq!(
        t.contract.check_oracle_verification(
            &OracleFeedType::Identity,
            &t.token.address,
            &t.payer
        ),
        crate::oracle_registry::IdentityVerificationOutcome::Verified,
        "the unconfigured case reports Verified — the only fail-open path"
    );
}

#[test]
fn test_1056_unverified_payer_rejected_and_invoice_stays_pending() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, false, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    let invoice_id = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(res, Err(Ok(ContractError::PayerUnverified)));

    // A rejection is atomic — the invoice is untouched.
    assert_eq!(
        t.contract.get_invoice(&invoice_id).status,
        InvoiceStatus::Pending,
        "a fail-closed rejection must leave the invoice pending"
    );
    assert_eq!(
        t.contract.check_oracle_verification(
            &OracleFeedType::Identity,
            &t.token.address,
            &t.payer
        ),
        crate::oracle_registry::IdentityVerificationOutcome::Unverified
    );
}

#[test]
fn test_1056_quorum_disagreement_rejected() {
    let t = setup();
    let seq = t.env.ledger().sequence();
    let yes = deploy_mock_oracle(&t, true, seq);
    let no = deploy_mock_oracle(&t, false, seq);
    add_identity_oracles(&t, &[yes, no]);
    t.contract.set_identity_quorum(&2);

    let invoice_id = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        res,
        Err(Ok(ContractError::PayerUnverified)),
        "feeders disagreeing below quorum fails closed"
    );
    assert_eq!(
        t.contract.check_oracle_verification(
            &OracleFeedType::Identity,
            &t.token.address,
            &t.payer
        ),
        crate::oracle_registry::IdentityVerificationOutcome::QuorumDisagreement
    );
}

#[test]
fn test_1056_stale_ledger_boundary_fresh_funds_then_stale_rejects() {
    let t = setup();
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.set_max_oracle_age(&10);

    // Data pinned 9 ledgers behind: age == max_age - 1, the last fresh ledger.
    let now = t.env.ledger().sequence();
    let oracle = deploy_mock_oracle(&t, true, now - 9);
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    let fresh_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &fresh_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.contract.get_invoice(&fresh_id).status,
        InvoiceStatus::Funded,
        "age max_age - 1 is inside the freshness window"
    );

    // Cross one ledger: age == max_age == 10 → stale, and the gate fails closed.
    advance_ledgers(&t.env, 1);
    let stale_id = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &stale_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        res,
        Err(Ok(ContractError::OracleDataStale)),
        "age max_age is the exact staleness boundary (stale is age >= max_age)"
    );
    assert_eq!(
        t.contract.check_oracle_verification(
            &OracleFeedType::Identity,
            &t.token.address,
            &t.payer
        ),
        crate::oracle_registry::IdentityVerificationOutcome::Stale
    );
}

#[test]
fn test_1056_frozen_snapshot_fails_closed_at_the_gate() {
    let t = setup();
    // A frozen (or replaying) feeder keeps re-serving one captured timestamp.
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    // Drive the duplicate streak to the cap through the never-erroring keeper
    // path (a rejected fund_invoice would roll its health write back).
    let cap = crate::oracle_registry::MAX_CONSECUTIVE_DUPLICATE_UPDATES;
    for _ in 0..(cap + 1) {
        assert_eq!(
            t.contract.check_oracle_verification(
                &OracleFeedType::Identity,
                &t.token.address,
                &t.payer
            ),
            crate::oracle_registry::IdentityVerificationOutcome::Verified
        );
    }

    // The gate now refuses the still-frozen snapshot.
    let invoice_id = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        res,
        Err(Ok(ContractError::OracleDuplicateUpdate)),
        "a frozen feeder past the duplicate cap fails closed at the gate"
    );
}

#[test]
fn test_1056_rejected_funding_rolls_back_and_leaves_no_health_trace() {
    let t = setup();
    setup_stale_default_oracle(&t); // max_age=10, data already 20 ledgers old

    // Nothing has queried the oracle yet, so no health snapshot exists.
    assert_eq!(
        t.contract
            .get_oracle_health(&OracleFeedType::Identity, &t.token.address),
        None
    );

    let invoice_id = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(res, Err(Ok(ContractError::OracleDataStale)));

    // The rejection is atomic: no invoice state moved and the health write
    // `evaluate_identity_verification` performed was rolled back with the call
    // — which is exactly why the #1049 diagnostic exists.
    assert_eq!(
        t.contract.get_invoice(&invoice_id).status,
        InvoiceStatus::Pending
    );
    assert_eq!(
        t.contract
            .get_oracle_health(&OracleFeedType::Identity, &t.token.address),
        None,
        "an Err-returning funding call must roll its health write back"
    );
}

#[test]
fn test_1056_diagnostic_reports_the_same_decision_the_gate_enforces() {
    let t = setup();
    setup_stale_default_oracle(&t);

    // The gate rejects…
    let invoice_id = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(res, Err(Ok(ContractError::OracleDataStale)));

    // …and the never-erroring diagnostic reports the identical reason, so an
    // operator can observe a rejection the gate itself would roll back.
    assert_eq!(
        t.contract.check_oracle_verification(
            &OracleFeedType::Identity,
            &t.token.address,
            &t.payer
        ),
        crate::oracle_registry::IdentityVerificationOutcome::Stale
    );
}

#[test]
fn test_1056_fail_closed_does_not_bypass_funder_authorization() {
    let t = setup();
    let oracle = deploy_mock_oracle(&t, true, t.env.ledger().sequence());
    t.contract
        .register_oracle(&OracleFeedType::Identity, &oracle);

    let invoice_id = make_invoice(&t);

    // Strip every authorization: the oracle would verify the payer, but the
    // funder's `require_auth` still gates the call. Verification is a
    // precondition, never a substitute for authorization.
    t.env.mock_auths(&[]);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert!(
        res.is_err(),
        "fund_invoice must require the funder's authorization regardless of oracle state"
    );

    // Restoring auth lets the same, still-pending invoice fund.
    t.env.mock_all_auths();
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.contract.get_invoice(&invoice_id).status,
        InvoiceStatus::Funded
    );
}
