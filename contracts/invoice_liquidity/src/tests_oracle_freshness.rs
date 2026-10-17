#![cfg(test)]

//! Tests for issue #93 — oracle data freshness validation in fund_invoice().
//!
//! The freshness check compares the oracle's reported ledger sequence number
//! (OracleVerificationResponse.timestamp) against the current ledger sequence.
//! If current_ledger - oracle.timestamp >= max_oracle_age_ledgers the contract
//! returns ContractError::OracleDataStale.
//!
//! Scenarios covered:
//! 1. Fresh oracle (age = 0)                               → succeeds.
//! 2. Stale oracle (age > max_oracle_age_ledgers)          → OracleDataStale.
//! 3. Boundary: age = max_oracle_age_ledgers - 1           → succeeds (just inside).
//! 4. Boundary: age = max_oracle_age_ledgers               → OracleDataStale (exactly at limit).
//! 5. max_oracle_age_ledgers = 0 disables the check        → succeeds regardless of age.
//! 6. Governance can update max_oracle_age_ledgers          → new limit respected.
//! 7. Stale but require_oracle_verification=false           → succeeds (check skipped).
//!
//! Issue #1050 extends this file to the *ledger boundaries* the interior
//! scenarios above don't reach: the exact first-stale / last-fresh ledgers, a
//! one-ledger-at-a-time walk across the boundary, the sequence-range ends, the
//! `max_oracle_age_ledgers == 0` escape hatch applied to those, and — the
//! security fix — a data timestamp *ahead* of the current ledger (future-dated
//! data), which a plain saturating subtraction mis-read as maximally fresh.
//! `oracle_registry::is_oracle_data_stale` is now the single rule shared by the
//! funding gate and the recorded health snapshot; the boundary trio is asserted
//! both directly on that function and end-to-end through `fund_invoice`.

use super::*;
use crate::test::setup;
use proptest::prelude::*;

const INVOICE_AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 60 * 60 * 24 * 30;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger as _},
    Address, Env,
};

// ----------------------------------------------------------------
// Configurable-timestamp oracle for boundary testing.
// The oracle stores an externally-set timestamp and returns it with
// every get_payer_data() call.  Tests control "data age" by setting
// the ledger sequence and then providing an older timestamp.
// ----------------------------------------------------------------
#[contract]
struct MockTimestampedOracle;

#[contractimpl]
impl MockTimestampedOracle {
    /// Initialise the oracle's stored timestamp.
    pub fn init(_env: Env, _timestamp: u32) {}

    /// Return verified=true with the timestamp that was passed at registration.
    /// In tests we use the args Vec to inject the timestamp value.
    pub fn interface_version(_env: Env) -> u32 {
        crate::oracle_interface::ORACLE_INTERFACE_VERSION
    }

    pub fn get_payer_data(env: Env, _payer: Address) -> OracleVerificationResponse {
        // Read the timestamp from instance storage (set via a helper below).
        let ts: u32 = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("ts"))
            .unwrap_or(env.ledger().sequence());
        OracleVerificationResponse {
            is_verified: true,
            timestamp: ts,
        }
    }

    /// Admin helper: store the timestamp so get_payer_data() returns it.
    pub fn set_timestamp(env: Env, ts: u32) {
        env.storage()
            .instance()
            .set(&soroban_sdk::symbol_short!("ts"), &ts);
    }
}

// ----------------------------------------------------------------
// Test helpers
// ----------------------------------------------------------------

/// Register a MockTimestampedOracle, pre-seed its timestamp, wire it into the
/// contract config, and return the oracle's contract address.
fn register_oracle_with_timestamp(t: &crate::test::TestEnv, oracle_ts: u32) -> Address {
    let oracle_id = t.env.register_contract(None, MockTimestampedOracle);
    let oracle_client = MockTimestampedOracleClient::new(&t.env, &oracle_id);
    oracle_client.set_timestamp(&oracle_ts);
    t.contract.set_price_oracle(&oracle_id).unwrap();
    oracle_id
}

fn make_invoice(t: &crate::test::TestEnv) -> u64 {
    let now = t.env.ledger().timestamp();
    t.contract
        .submit_invoice(
            &t.freelancer,
            &t.payer,
            &INVOICE_AMOUNT,
            &(now + DUE_DATE_OFFSET),
            &DISCOUNT_RATE,
            &t.token.address,
            &ReferralCode::None,
        )
        .unwrap()
}

/// Advance the ledger sequence by `delta` ledgers.
fn advance_ledger(env: &soroban_sdk::Env, delta: u32) {
    let mut info = env.ledger().get();
    info.sequence_number += delta;
    // Also advance timestamp proportionally (5 s per ledger) to keep the
    // due-date check from expiring the invoice.
    info.timestamp += delta as u64 * 5;
    env.ledger().set(info);
}

// ----------------------------------------------------------------
// Test 1: fresh oracle (age = 0) passes
// ----------------------------------------------------------------
#[test]
fn test_fresh_oracle_passes() {
    let t = setup();
    // Oracle timestamp == current ledger sequence → age = 0 → fresh.
    let current_seq = t.env.ledger().sequence();
    register_oracle_with_timestamp(&t, current_seq);

    let invoice_id = make_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true)
        .unwrap();

    let invoice = t.contract.get_invoice(&invoice_id).unwrap();
    assert_eq!(invoice.status, InvoiceStatus::Funded);
}

// ----------------------------------------------------------------
// Test 2: stale oracle (age >> max) fails with OracleDataStale
// ----------------------------------------------------------------
#[test]
fn test_stale_oracle_fails() {
    let t = setup();
    // Set a tiny max age so we can manufacture staleness easily.
    let max_age: u64 = 10;
    t.contract.set_max_oracle_age(&max_age).unwrap();

    let current_seq = t.env.ledger().sequence(); // 100
                                                 // Oracle timestamp is older than max_age (age = 20 ≥ 10).
    register_oracle_with_timestamp(&t, current_seq - 20);

    let invoice_id = make_invoice(&t);

    let result = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

    assert_eq!(
        result,
        Err(Ok(ContractError::OracleDataStale)),
        "expected OracleDataStale when oracle data is older than max_oracle_age_ledgers"
    );
}

// ----------------------------------------------------------------
// Test 3: exactly one ledger inside the limit passes (age = max - 1)
// ----------------------------------------------------------------
#[test]
fn test_boundary_one_before_limit_passes() {
    let t = setup();
    let max_age: u64 = 10;
    t.contract.set_max_oracle_age(&max_age).unwrap();

    let current_seq = t.env.ledger().sequence(); // 100
                                                 // age = 9 < 10 → should pass.
    register_oracle_with_timestamp(&t, current_seq - 9);

    let invoice_id = make_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true)
        .unwrap();

    let invoice = t.contract.get_invoice(&invoice_id).unwrap();
    assert_eq!(invoice.status, InvoiceStatus::Funded);
}

// ----------------------------------------------------------------
// Test 4: exactly at the limit fails (age = max)
// ----------------------------------------------------------------
#[test]
fn test_boundary_exactly_at_limit_fails() {
    let t = setup();
    let max_age: u64 = 10;
    t.contract.set_max_oracle_age(&max_age).unwrap();

    let current_seq = t.env.ledger().sequence(); // 100
                                                 // age = 10 = max_age → should fail.
    register_oracle_with_timestamp(&t, current_seq - 10);

    let invoice_id = make_invoice(&t);

    let result = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

    assert_eq!(
        result,
        Err(Ok(ContractError::OracleDataStale)),
        "expected OracleDataStale at exactly the boundary (age == max_oracle_age_ledgers)"
    );
}

// ----------------------------------------------------------------
// Test 5: max_oracle_age_ledgers = 0 disables the freshness check
// ----------------------------------------------------------------
#[test]
fn test_zero_max_age_disables_check() {
    let t = setup();
    // Disable freshness check entirely.
    t.contract.set_max_oracle_age(&0u64).unwrap();

    let current_seq = t.env.ledger().sequence();
    // Very old timestamp — age would exceed any real limit.
    register_oracle_with_timestamp(&t, current_seq.saturating_sub(999_999));

    let invoice_id = make_invoice(&t);

    // Should succeed because max_oracle_age_ledgers == 0 skips the check.
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true)
        .unwrap();

    let invoice = t.contract.get_invoice(&invoice_id).unwrap();
    assert_eq!(invoice.status, InvoiceStatus::Funded);
}

// ----------------------------------------------------------------
// Test 6: governance can tighten max_oracle_age; new limit respected
// ----------------------------------------------------------------
#[test]
fn test_governance_can_update_max_oracle_age() {
    let t = setup();

    // Default max age (17_280) — a 5-ledger-old timestamp is fine.
    let current_seq = t.env.ledger().sequence();
    register_oracle_with_timestamp(&t, current_seq - 5);

    let invoice_id1 = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id1, &INVOICE_AMOUNT, &true)
        .unwrap();

    // Governance tightens the limit to 3 ledgers — same oracle data now stale.
    t.contract.set_max_oracle_age(&3u64).unwrap();
    assert_eq!(t.contract.get_max_oracle_age(), 3u64);

    let invoice_id2 = make_invoice(&t);
    let result = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id2, &INVOICE_AMOUNT, &true);

    assert_eq!(
        result,
        Err(Ok(ContractError::OracleDataStale)),
        "after tightening max age, previously-fresh data should now be stale"
    );
}

// ----------------------------------------------------------------
// Test 7: stale oracle + require_oracle_verification=false → passes
// ----------------------------------------------------------------
#[test]
fn test_stale_oracle_ignored_when_flag_false() {
    let t = setup();
    t.contract.set_max_oracle_age(&5u64).unwrap();

    let current_seq = t.env.ledger().sequence();
    // Deliberately stale: age = 99 ≥ 5.
    register_oracle_with_timestamp(&t, current_seq.saturating_sub(99));

    let invoice_id = make_invoice(&t);

    // flag=false → oracle not consulted → no staleness check → succeeds.
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &false)
        .unwrap();

    let invoice = t.contract.get_invoice(&invoice_id).unwrap();
    assert_eq!(invoice.status, InvoiceStatus::Funded);
}

// ----------------------------------------------------------------
// Test 8: staleness detected after ledger advances (dynamic staleness)
// ----------------------------------------------------------------
#[test]
fn test_data_becomes_stale_as_ledger_advances() {
    let t = setup();
    let max_age: u64 = 5;
    t.contract.set_max_oracle_age(&max_age).unwrap();

    let oracle_ts = t.env.ledger().sequence(); // timestamp frozen at registration
    register_oracle_with_timestamp(&t, oracle_ts);

    // Advance 4 ledgers — still within limit (age = 4 < 5).
    advance_ledger(&t.env, 4);
    let invoice_id1 = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id1, &INVOICE_AMOUNT, &true)
        .unwrap();

    // Advance 1 more ledger — now age = 5 = max_age → stale.
    advance_ledger(&t.env, 1);
    let invoice_id2 = make_invoice(&t);
    let result = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id2, &INVOICE_AMOUNT, &true);

    assert_eq!(
        result,
        Err(Ok(ContractError::OracleDataStale)),
        "oracle data should become stale once ledger advances past max_oracle_age"
    );
}

// ----------------------------------------------------------------
// Test 9: no oracle configured → fail-open (require_oracle_verification=true)
// ----------------------------------------------------------------
#[test]
fn test_no_oracle_configured_fails_open() {
    let t = setup();
    // Do NOT register any oracle — config.price_oracle is None.

    let invoice_id = make_invoice(&t);

    // When require_oracle_verification=true but no oracle is configured,
    // the contract should skip the oracle check and succeed (fail-open).
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true)
        .unwrap();

    let invoice = t.contract.get_invoice(&invoice_id).unwrap();
    assert_eq!(invoice.status, InvoiceStatus::Funded);
}

// ================================================================
// Tests for set_max_oracle_age (Issue #484)
// ================================================================

// ----------------------------------------------------------------
// Test 10: admin can set max oracle age
// ----------------------------------------------------------------
#[test]
fn test_set_max_oracle_age_succeeds() {
    let t = setup();

    t.contract.set_max_oracle_age(&42).unwrap();

    assert_eq!(t.contract.get_max_oracle_age(), 42);
}

// ----------------------------------------------------------------
// Test 11: setting max_oracle_age affects fund_invoice oracle check
// ----------------------------------------------------------------
#[test]
fn test_set_max_oracle_age_affects_fund_invoice_check() {
    let t = setup();
    let current_seq = t.env.ledger().sequence();

    // Set a tight max age of 5 ledgers.
    t.contract.set_max_oracle_age(&5).unwrap();

    // Oracle timestamp is 4 ledgers old → age=4 < 5 → fresh.
    register_oracle_with_timestamp(&t, current_seq - 4);

    let invoice_id1 = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id1, &INVOICE_AMOUNT, &true)
        .unwrap();
    assert_eq!(
        t.contract.get_invoice(&invoice_id1).unwrap().status,
        InvoiceStatus::Funded
    );

    // Tighten to 3 ledgers → same oracle data (age=4) now stale.
    t.contract.set_max_oracle_age(&3).unwrap();

    let invoice_id2 = make_invoice(&t);
    let result = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id2, &INVOICE_AMOUNT, &true);

    assert_eq!(result, Err(Ok(ContractError::OracleDataStale)));
}

// ----------------------------------------------------------------
// Test 12: non-admin cannot call set_max_oracle_age
// ----------------------------------------------------------------
#[test]
fn test_set_max_oracle_age_unauthorized() {
    // Use a fresh env WITHOUT mock_all_auths to test real auth.
    let env = Env::default();
    // Don't call env.mock_all_auths() — we want real auth checks.

    let usdc_admin = Address::generate(&env);
    let usdc = env.register_stellar_asset_contract_v2(usdc_admin.clone());
    let eurc_admin = Address::generate(&env);
    let eurc = env.register_stellar_asset_contract_v2(eurc_admin.clone());
    let xlm_admin = Address::generate(&env);
    let xlm = env.register_stellar_asset_contract_v2(xlm_admin.clone());

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    contract.initialize(&usdc_admin, &usdc.address(), &eurc.address(), &xlm.address());

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    env.ledger().set(ledger);

    let unauthorized = Address::generate(&env);
    // Only the unauthorized address signs — admin does not.
    env.mock_auths(&[
        soroban_sdk::testutils::MockAuth {
            address: &unauthorized,
            invoke: &soroban_sdk::testutils::MockAuthInvoke {
                contract: &contract_id,
                fn_name: "set_max_oracle_age",
                sub_invokes: &[],
                args: (&100u64,).into_val(&env),
            },
        },
    ]);

    let result = contract.try_set_max_oracle_age(&100);
    assert_eq!(result, Err(Ok(ContractError::Unauthorized)));
}

// ================================================================
// Issue #1050 — freshness across ledger boundaries
// ================================================================
//
// The freshness rule is "stale iff age >= max_oracle_age_ledgers", where
// age = current_ledger - oracle.timestamp. The tests above pin the *interior*
// boundary (age == max - 1 passes, age == max fails) at a single ledger. This
// section pins the boundaries that the interior tests cannot reach and that the
// old saturating-subtraction formulation got wrong:
//
//   * the last fresh and first stale ledgers, and a walk across that boundary;
//   * the sequence-number boundary where the data timestamp overtakes the
//     current ledger (future-dated data) — saturating_sub mapped this to age 0,
//     i.e. "maximally fresh", which defeats the bound entirely;
//   * the top of the u32 ledger-sequence range (no wraparound surprise);
//   * max_oracle_age_ledgers == 0 still disabling the rule (escape hatch);
//   * the funding gate and the recorded health snapshot agreeing at the
//     boundary.
//
// Before #1050: future-dated data (timestamp > current ledger) was accepted as
// fresh by fund_invoice and recorded as is_stale: false. After: it is rejected
// (OracleDataStale) and recorded as stale, via the shared
// oracle_registry::is_oracle_data_stale rule.

// ----------------------------------------------------------------
// Test 13: direct unit coverage of the shared rule, at every boundary
// ----------------------------------------------------------------
#[test]
fn test_is_oracle_data_stale_boundaries() {
    use crate::oracle_registry::is_oracle_data_stale;

    // max_age == 0 disables the rule at every input — including the future
    // region (governance escape hatch).
    for (cur, ts) in [(100u32, 100u32), (100, 0), (100, 101), (0, u32::MAX)] {
        assert!(
            !is_oracle_data_stale(cur, ts, 0),
            "max_age=0 must disable the rule (cur={cur}, ts={ts})"
        );
    }

    // Interior boundary with max_age = 10.
    assert!(!is_oracle_data_stale(100, 91, 10), "age 9 is the last fresh ledger");
    assert!(is_oracle_data_stale(100, 90, 10), "age 10 is the first stale ledger");
    assert!(is_oracle_data_stale(100, 89, 10), "age 11 is stale");

    // max_age = 1: only the current ledger is fresh.
    assert!(!is_oracle_data_stale(100, 100, 1));
    assert!(is_oracle_data_stale(100, 99, 1));

    // The boundary the old saturating formulation missed: a data timestamp
    // *ahead* of the current ledger is stale, never "maximally fresh".
    assert!(is_oracle_data_stale(100, 101, 10), "one ledger in the future is stale");
    assert!(is_oracle_data_stale(100, u32::MAX, 10), "far-future data is stale");
    assert!(is_oracle_data_stale(0, 1, 1), "future data at ledger 0 is stale");

    // Top of the u32 sequence range: no wraparound surprise.
    assert!(!is_oracle_data_stale(u32::MAX, u32::MAX, 1), "age 0 at the range top is fresh");
    assert!(is_oracle_data_stale(u32::MAX, 0, 1), "old data at the range top is stale");
}

// ----------------------------------------------------------------
// Test 14: one ledger past the limit fails (age = max + 1)
// ----------------------------------------------------------------
#[test]
fn test_boundary_one_past_limit_fails() {
    let t = setup();
    let max_age: u64 = 10;
    t.contract.set_max_oracle_age(&max_age).unwrap();

    let current_seq = t.env.ledger().sequence();
    register_oracle_with_timestamp(&t, current_seq - 11); // age = 11 >= 10

    let invoice_id = make_invoice(&t);
    let result = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

    assert_eq!(
        result,
        Err(Ok(ContractError::OracleDataStale)),
        "expected OracleDataStale one ledger past the limit (age == max + 1)"
    );
}

// ----------------------------------------------------------------
// Test 15: max_age = 1 — only the current ledger is fresh
// ----------------------------------------------------------------
#[test]
fn test_max_age_one_only_the_current_ledger_is_fresh() {
    let t = setup();
    t.contract.set_max_oracle_age(&1u64).unwrap();
    let current_seq = t.env.ledger().sequence();

    // age = 0 → fresh.
    register_oracle_with_timestamp(&t, current_seq);
    let id1 = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &id1, &INVOICE_AMOUNT, &true)
        .unwrap();

    // One ledger later the *same* oracle data is stale (age 1 >= 1).
    advance_ledger(&t.env, 1);
    let id2 = make_invoice(&t);
    let result = t
        .contract
        .try_fund_invoice(&t.funder, &id2, &INVOICE_AMOUNT, &true);

    assert_eq!(
        result,
        Err(Ok(ContractError::OracleDataStale)),
        "with max_age = 1 the data must go stale the very next ledger"
    );
}

// ----------------------------------------------------------------
// Test 16: walk the ledger one at a time across the boundary
// ----------------------------------------------------------------
#[test]
fn test_freshness_walk_across_the_boundary() {
    let t = setup();
    let max_age: u64 = 5;
    t.contract.set_max_oracle_age(&max_age).unwrap();

    let oracle_ts = t.env.ledger().sequence();
    register_oracle_with_timestamp(&t, oracle_ts);

    // age climbs 0,1,...,max_age as the ledger advances one at a time. Funding
    // succeeds on every ledger strictly inside the window and flips to
    // OracleDataStale on the first ledger at the boundary — and stays stale.
    for age in 0..=max_age + 1 {
        if age > 0 {
            advance_ledger(&t.env, 1);
        }
        let invoice_id = make_invoice(&t);
        let result = t
            .contract
            .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

        if (age as u64) < max_age {
            assert!(
                result.is_ok(),
                "age {age} should still be fresh (< max_age {max_age})"
            );
        } else {
            assert_eq!(
                result,
                Err(Ok(ContractError::OracleDataStale)),
                "age {age} should be stale (>= max_age {max_age})"
            );
        }
    }
}

// ----------------------------------------------------------------
// Test 17: a future-dated timestamp is stale, not "maximally fresh"
// ----------------------------------------------------------------
#[test]
fn test_future_dated_oracle_timestamp_is_stale() {
    let t = setup();
    // Default max age (17_280) — wide enough that only the future region can
    // make this stale. An oracle timestamp one ledger ahead of the current
    // ledger must be rejected, not treated as age 0.
    let current_seq = t.env.ledger().sequence();
    register_oracle_with_timestamp(&t, current_seq + 1);

    let invoice_id = make_invoice(&t);
    let result = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

    assert_eq!(
        result,
        Err(Ok(ContractError::OracleDataStale)),
        "future-dated oracle data must be rejected rather than accepted as freshest"
    );
}

// ----------------------------------------------------------------
// Test 18: even a far-future timestamp is stale (no saturating bypass)
// ----------------------------------------------------------------
#[test]
fn test_far_future_oracle_timestamp_is_stale() {
    let t = setup();
    register_oracle_with_timestamp(&t, u32::MAX);

    let invoice_id = make_invoice(&t);
    let result = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

    assert_eq!(
        result,
        Err(Ok(ContractError::OracleDataStale)),
        "a timestamp at the top of the u32 range must not bypass the freshness bound"
    );
}

// ----------------------------------------------------------------
// Test 19: max_age = 0 still disables the rule, even for future data
// ----------------------------------------------------------------
#[test]
fn test_zero_max_age_still_accepts_future_timestamp() {
    let t = setup();
    t.contract.set_max_oracle_age(&0u64).unwrap();

    let current_seq = t.env.ledger().sequence();
    register_oracle_with_timestamp(&t, current_seq + 1); // future-dated

    let invoice_id = make_invoice(&t);
    // The escape hatch (max_age == 0) disables the rule at every input, so the
    // future-timestamp rejection must not fire here.
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true)
        .unwrap();

    assert_eq!(
        t.contract.get_invoice(&invoice_id).unwrap().status,
        InvoiceStatus::Funded
    );
}

// ----------------------------------------------------------------
// Test 20: the funding gate and the recorded health snapshot agree
//            at the future boundary
// ----------------------------------------------------------------
#[test]
fn test_health_snapshot_agrees_at_future_boundary() {
    let t = setup();
    let current_seq = t.env.ledger().sequence();
    register_oracle_with_timestamp(&t, current_seq + 5);

    // check_oracle_health records the snapshot without erroring on staleness,
    // so the observation is durable even when funding would be rejected.
    let health = t
        .contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer)
        .expect("oracle should resolve for the Identity feed");

    assert!(
        health.is_stale,
        "future-dated data must be recorded as stale, matching the funding gate"
    );
    assert_eq!(health.last_data_timestamp, current_seq + 5);
}

// ----------------------------------------------------------------
// Test 21: the future boundary is authorization-independent — funding
//            through require_oracle_verification still rejects it
// ----------------------------------------------------------------
#[test]
fn test_future_timestamp_rejected_through_funding_gate_not_auth() {
    let t = setup();
    let current_seq = t.env.ledger().sequence();
    register_oracle_with_timestamp(&t, current_seq + 1);

    // The rejection is a data-validity outcome, independent of who funds or
    // how the invoice was authorized: with verification required the call
    // fails with OracleDataStale, while with verification skipped the same
    // invoice funds (the check is simply not consulted).
    let invoice_id = make_invoice(&t);
    let gated = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(gated, Err(Ok(ContractError::OracleDataStale)));

    let invoice_id2 = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id2, &INVOICE_AMOUNT, &false)
        .unwrap();
    assert_eq!(
        t.contract.get_invoice(&invoice_id2).unwrap().status,
        InvoiceStatus::Funded
    );
}

// ----------------------------------------------------------------
// Property: the shared rule matches a reference formulation, and is
//           monotonic non-decreasing as the ledger advances
// ----------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn prop_freshness_rule_matches_reference(
        current in 0u32..2_000_000u32,
        ts in 0u32..2_000_000u32,
        max_age in 1u64..50_000u64,
    ) {
        let got = crate::oracle_registry::is_oracle_data_stale(current, ts, max_age);
        // Reference: stale iff the data is from the future, or its (non-negative)
        // age has reached the bound.
        let expected = ts > current || (current as u64 - ts as u64) >= max_age;
        prop_assert_eq!(got, expected);
    }

    #[test]
    fn prop_freshness_is_monotonic_in_ledger_age(
        current in 0u32..2_000_000u32,
        ts in 0u32..2_000_000u32,
        max_age in 1u64..50_000u64,
    ) {
        // Advancing the ledger (age growing) can only turn fresh data stale,
        // never the reverse.
        let before = crate::oracle_registry::is_oracle_data_stale(current, ts, max_age);
        let after = crate::oracle_registry::is_oracle_data_stale(current.saturating_add(1), ts, max_age);
        prop_assert!(!before || after, "staleness must not decrease as the ledger advances");
    }
}

// ----------------------------------------------------------------
// Issue #1052 — monotonic oracle timestamp checks
//
// An oracle's reported data timestamp must be monotonically
// *non-decreasing* for the same (feed type, token, oracle). A response
// older than the last timestamp recorded for that oracle is replayed or
// rolled-back data and is rejected with `OracleTimestampRegression`
// *before* the freshness / verification gates run. Combined with the
// #1050 rule (future-dated data is stale), an accepted response must lie
// in `[last_recorded, current_ledger]`.
// ----------------------------------------------------------------

/// Stage a prior `OracleHealthStatus` snapshot for the Identity feed on
/// `t.token` with `last_data_timestamp == ts`, recorded through the real
/// registry health machinery (storage writes must run as the contract, hence
/// `as_contract`). `max_age` is generous so the staged snapshot is not itself
/// flagged stale — the monotonicity predicate only reads `last_data_timestamp`.
fn stage_prior_data_timestamp(t: &crate::test::TestEnv, oracle: &Address, ts: u32) {
    t.env.as_contract(&t.contract.address, || {
        crate::oracle_registry::record_oracle_health(
            &t.env,
            OracleFeedType::Identity,
            &t.token.address,
            oracle,
            ts,
            DEFAULT_MAX_ORACLE_AGE_LEDGERS,
        );
    });
}

/// A first read with no prior snapshot must be accepted (no false positive):
/// monotonicity is defined relative to a recorded baseline, and none exists
/// yet. The accepted read also seeds that baseline for the next read.
#[test]
fn test_1052_first_read_without_baseline_is_accepted() {
    let t = setup();
    let current_seq = t.env.ledger().sequence();
    register_oracle_with_timestamp(&t, current_seq);

    let invoice_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true)
        .unwrap();
    assert_eq!(
        t.contract.get_invoice(&invoice_id).unwrap().status,
        InvoiceStatus::Funded
    );

    // The accepted read persisted the baseline used by later checks.
    let health = t
        .contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer)
        .unwrap();
    assert_eq!(health.last_data_timestamp, current_seq);
}

/// Re-reading the *same* timestamp is not a regression (the rule is
/// non-decreasing, not strictly increasing) and must be accepted.
#[test]
fn test_1052_equal_timestamp_reread_is_accepted() {
    let t = setup();
    let current_seq = t.env.ledger().sequence();
    let _oracle_id = register_oracle_with_timestamp(&t, current_seq);

    let invoice_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true)
        .unwrap();

    // Ledger moves on but the oracle re-serves its last timestamp.
    advance_ledger(&t.env, 1);

    let invoice_id2 = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id2, &INVOICE_AMOUNT, &true)
        .unwrap();
    assert_eq!(
        t.contract.get_invoice(&invoice_id2).unwrap().status,
        InvoiceStatus::Funded
    );
}

/// A timestamp strictly older than the recorded baseline for the *same*
/// oracle is a regression and must be rejected with
/// `OracleTimestampRegression`.
#[test]
fn test_1052_regression_is_rejected() {
    let t = setup();
    let current_seq = t.env.ledger().sequence();
    let oracle_id = register_oracle_with_timestamp(&t, current_seq);

    // Baseline = current_seq.
    let invoice_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true)
        .unwrap();

    // Oracle rolls back to an older (but still "fresh") timestamp.
    MockTimestampedOracleClient::new(&t.env, &oracle_id).set_timestamp(&(current_seq - 20));

    let invoice_id2 = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id2, &INVOICE_AMOUNT, &true);
    assert_eq!(res, Err(Ok(ContractError::OracleTimestampRegression)));
}

/// The monotonicity gate is a data-validity check, not an authorization one:
/// with `require_oracle_verification = false` the oracle is never consulted, so
/// the same regressed response does not block funding. This mirrors
/// `test_future_timestamp_rejected_through_funding_gate_not_auth`.
#[test]
fn test_1052_regression_gate_only_applies_when_oracle_required() {
    let t = setup();
    let current_seq = t.env.ledger().sequence();
    let oracle_id = register_oracle_with_timestamp(&t, current_seq);

    let invoice_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true)
        .unwrap();

    MockTimestampedOracleClient::new(&t.env, &oracle_id).set_timestamp(&(current_seq - 30));

    // Gate enabled → rejected.
    let invoice_gated = make_invoice(&t);
    let gated = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_gated, &INVOICE_AMOUNT, &true);
    assert_eq!(gated, Err(Ok(ContractError::OracleTimestampRegression)));

    // Gate disabled → same data accepted (the oracle is not consulted).
    let invoice_ungated = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_ungated, &INVOICE_AMOUNT, &false)
        .unwrap();
    assert_eq!(
        t.contract.get_invoice(&invoice_ungated).unwrap().status,
        InvoiceStatus::Funded
    );
}

/// Ledger-boundary case: a response equal to the baseline is accepted, and the
/// sequence-range lower bound (`0`) must still be rejected once the baseline is
/// above it.
#[test]
fn test_1052_boundary_equal_accepted_and_zero_rejected() {
    let t = setup();
    let oracle_id = register_oracle_with_timestamp(&t, 1);

    // Baseline 1, oracle reports 1 → equal → accepted (non-decreasing edge).
    stage_prior_data_timestamp(&t, &oracle_id, 1);
    let invoice_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true)
        .unwrap();
    assert_eq!(
        t.contract.get_invoice(&invoice_id).unwrap().status,
        InvoiceStatus::Funded
    );

    // Same oracle now reports the sequence lower bound 0 → strictly older →
    // regression.
    MockTimestampedOracleClient::new(&t.env, &oracle_id).set_timestamp(&0);
    let invoice_id2 = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id2, &INVOICE_AMOUNT, &true);
    assert_eq!(res, Err(Ok(ContractError::OracleTimestampRegression)));
}

/// Monotonicity is only meaningful against the *same* oracle: a failover
/// oracle legitimately starts from its own (possibly older) timeline and must
/// not be treated as a regression.
#[test]
fn test_1052_lower_timestamp_from_different_oracle_is_not_a_regression() {
    let t = setup();
    let current_seq = t.env.ledger().sequence();
    let _oracle_a = register_oracle_with_timestamp(&t, current_seq);

    // Seed the baseline for oracle A.
    let invoice_id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true)
        .unwrap();

    // Swap to a different oracle reporting an older timestamp (past the
    // rate-limit cooldown for `set_price_oracle`).
    advance_ledger(&t.env, 150);
    let oracle_b_id = t.env.register_contract(None, MockTimestampedOracle);
    MockTimestampedOracleClient::new(&t.env, &oracle_b_id).set_timestamp(&(current_seq - 40));
    t.contract.set_price_oracle(&oracle_b_id).unwrap();

    let invoice_id2 = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id2, &INVOICE_AMOUNT, &true)
        .unwrap();
    assert_eq!(
        t.contract.get_invoice(&invoice_id2).unwrap().status,
        InvoiceStatus::Funded
    );
}

// ----------------------------------------------------------------
// Property: the regression predicate fires iff the reported timestamp is
//           strictly older than the recorded baseline for the same oracle
// ----------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn prop_1052_regression_iff_strictly_older_for_same_oracle(
        last in 1u32..1_000_000u32,
        ts in 0u32..1_000_000u32,
    ) {
        let t = setup();
        let oracle = t.env.register_contract(None, MockTimestampedOracle);
        stage_prior_data_timestamp(&t, &oracle, last);

        let got = crate::oracle_registry::is_oracle_data_timestamp_regression(
            &t.env,
            OracleFeedType::Identity,
            &t.token.address,
            &oracle,
            ts,
        );
        // Same oracle ⇒ regression iff the new timestamp is strictly older.
        prop_assert_eq!(got, ts < last);
    }
}

// ----------------------------------------------------------------
// Issue #1053 — bound duplicate update frequency
//
// The #1052 rule forbids a timestamp going *backwards* but deliberately
// allows an *equal* one (non-decreasing). Nothing there stopped an oracle
// from standing still: an oracle that stops advancing and keeps re-serving
// the same data timestamp produces non-stale responses (within the freshness
// window) and keeps resetting the stale streak, so it could stand in for
// live verification indefinitely. #1053 bounds how many *consecutive*
// duplicate (identical-timestamp) updates the same oracle may serve:
// once the streak reaches `MAX_CONSECUTIVE_DUPLICATE_UPDATES`, the next
// duplicate is rejected with `OracleDuplicateUpdate`. The count is exposed
// on the health snapshot (`consecutive_duplicate_count`) and reset by any
// advance or oracle failover.
// ----------------------------------------------------------------

/// The number of consecutive duplicate updates the funding gate accepts
/// before rejecting — read from the enforcing constant so the tests can't
/// drift from the implementation (see also `tests_storage.rs` cap-drift
/// discipline).
const MAX_DUPLICATE: u32 = crate::oracle_registry::MAX_CONSECUTIVE_DUPLICATE_UPDATES;

/// Stage a duplicate streak of `calls - 1` for the Identity feed on
/// `t.token` from `oracle`, by replaying an *identical* `ts` through the real
/// health machinery (storage writes must run as the contract, hence
/// `as_contract`). The first read seeds the baseline; each subsequent
/// identical read is one duplicate. `max_age` is generous so the staged reads
/// are not flagged stale.
fn stage_duplicate_streak(t: &crate::test::TestEnv, oracle: &Address, ts: u32, calls: u32) {
    t.env.as_contract(&t.contract.address, || {
        for _ in 0..calls {
            crate::oracle_registry::record_oracle_health(
                &t.env,
                OracleFeedType::Identity,
                &t.token.address,
                oracle,
                ts,
                DEFAULT_MAX_ORACLE_AGE_LEDGERS,
            );
        }
    });
}

/// A duplicate update *up to and including* the bound is accepted (the bound
/// bounds duplicate frequency, it does not forbid re-reads outright). The
/// accepted reads build the streak on the persisted health snapshot.
#[test]
fn test_1053_duplicate_updates_up_to_bound_are_accepted() {
    let t = setup();
    let current_seq = t.env.ledger().sequence();
    let _oracle_id = register_oracle_with_timestamp(&t, current_seq);

    // First read seeds the baseline (streak 0 — not yet a duplicate).
    let id0 = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &id0, &INVOICE_AMOUNT, &true)
        .unwrap();

    // `MAX_DUPLICATE` further identical reads are exactly the duplicates up
    // to the bound — all accepted.
    for _ in 0..MAX_DUPLICATE {
        advance_ledger(&t.env, 1);
        let id = make_invoice(&t);
        t.contract
            .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &true)
            .unwrap();
        assert_eq!(
            t.contract.get_invoice(&id).unwrap().status,
            InvoiceStatus::Funded
        );
    }

    let health = t
        .contract
        .get_oracle_health(&OracleFeedType::Identity, &t.token.address)
        .unwrap();
    assert_eq!(health.consecutive_duplicate_count, MAX_DUPLICATE);
    assert_eq!(health.last_data_timestamp, current_seq);
}

/// One duplicate past the bound is rejected with `OracleDuplicateUpdate`.
/// Because a rejected funding call rolls its health write back, the streak
/// stays at the cap and the gate stays shut until the oracle advances.
#[test]
fn test_1053_duplicate_update_beyond_bound_is_rejected() {
    let t = setup();
    let current_seq = t.env.ledger().sequence();
    let _oracle_id = register_oracle_with_timestamp(&t, current_seq);

    let id0 = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &id0, &INVOICE_AMOUNT, &true)
        .unwrap();
    for _ in 0..MAX_DUPLICATE {
        advance_ledger(&t.env, 1);
        let id = make_invoice(&t);
        t.contract
            .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &true)
            .unwrap();
    }

    // Streak is now at the cap; the next identical update is one too many.
    let id_next = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &id_next, &INVOICE_AMOUNT, &true);
    assert_eq!(res, Err(Ok(ContractError::OracleDuplicateUpdate)));

    // Rejected call rolls back → streak unchanged, so a retry is rejected too.
    let id_retry = make_invoice(&t);
    let retry = t
        .contract
        .try_fund_invoice(&t.funder, &id_retry, &INVOICE_AMOUNT, &true);
    assert_eq!(retry, Err(Ok(ContractError::OracleDuplicateUpdate)));
    assert_eq!(
        t.contract
            .get_oracle_health(&OracleFeedType::Identity, &t.token.address)
            .unwrap()
            .consecutive_duplicate_count,
        MAX_DUPLICATE
    );
}

/// Ledger/streak boundary: at `cap - 1` the next duplicate is still accepted
/// (and lands on the cap); at the cap it is rejected. Exercised directly on
/// the predicate's storage baseline to pin the exact edge.
#[test]
fn test_1053_boundary_below_cap_accepted_at_cap_rejected() {
    let t = setup();
    let current_seq = t.env.ledger().sequence();
    let oracle_id = register_oracle_with_timestamp(&t, current_seq);

    // `MAX_DUPLICATE` identical reads leave the streak at `MAX_DUPLICATE - 1`.
    stage_duplicate_streak(&t, &oracle_id, current_seq, MAX_DUPLICATE);
    assert_eq!(
        t.contract
            .get_oracle_health(&OracleFeedType::Identity, &t.token.address)
            .unwrap()
            .consecutive_duplicate_count,
        MAX_DUPLICATE - 1
    );

    // One below the cap → the next identical read is accepted (→ cap).
    let id_ok = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &id_ok, &INVOICE_AMOUNT, &true)
        .unwrap();
    assert_eq!(
        t.contract
            .get_oracle_health(&OracleFeedType::Identity, &t.token.address)
            .unwrap()
            .consecutive_duplicate_count,
        MAX_DUPLICATE
    );

    // At the cap → the next identical read is rejected.
    let id_rej = make_invoice(&t);
    let res = t
        .contract
        .try_fund_invoice(&t.funder, &id_rej, &INVOICE_AMOUNT, &true);
    assert_eq!(res, Err(Ok(ContractError::OracleDuplicateUpdate)));
}

/// An oracle that finally advances its timestamp is not a duplicate: funding
/// resumes and the streak resets to 0.
#[test]
fn test_1053_advancing_timestamp_resets_duplicate_streak() {
    let t = setup();
    let current_seq = t.env.ledger().sequence();
    let oracle_id = register_oracle_with_timestamp(&t, current_seq);

    // Drive the streak to the cap through the real funding path.
    let id0 = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &id0, &INVOICE_AMOUNT, &true)
        .unwrap();
    for _ in 0..MAX_DUPLICATE {
        advance_ledger(&t.env, 1);
        let id = make_invoice(&t);
        t.contract
            .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &true)
            .unwrap();
    }

    // Oracle advances: same oracle, newer timestamp → accepted, streak resets.
    advance_ledger(&t.env, 1);
    let new_ts = t.env.ledger().sequence();
    MockTimestampedOracleClient::new(&t.env, &oracle_id).set_timestamp(&new_ts);

    let id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &true)
        .unwrap();

    let health = t
        .contract
        .get_oracle_health(&OracleFeedType::Identity, &t.token.address)
        .unwrap();
    assert_eq!(health.last_data_timestamp, new_ts);
    assert_eq!(health.consecutive_duplicate_count, 0);
}

/// The duplicate-frequency bound is a data-validity check, not an
/// authorization one: with `require_oracle_verification = false` the oracle is
/// never consulted, so an over-bound duplicate does not block funding. Mirrors
/// `test_1052_regression_gate_only_applies_when_oracle_required`.
#[test]
fn test_1053_duplicate_gate_only_applies_when_oracle_required() {
    let t = setup();
    let current_seq = t.env.ledger().sequence();
    let oracle_id = register_oracle_with_timestamp(&t, current_seq);

    // Stage the streak at the cap, with the oracle still serving `current_seq`.
    stage_duplicate_streak(&t, &oracle_id, current_seq, MAX_DUPLICATE + 1);

    // Gate enabled → rejected.
    let id_gated = make_invoice(&t);
    let gated = t
        .contract
        .try_fund_invoice(&t.funder, &id_gated, &INVOICE_AMOUNT, &true);
    assert_eq!(gated, Err(Ok(ContractError::OracleDuplicateUpdate)));

    // Gate disabled → the oracle is not consulted → accepted.
    let id_ungated = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &id_ungated, &INVOICE_AMOUNT, &false)
        .unwrap();
    assert_eq!(
        t.contract.get_invoice(&id_ungated).unwrap().status,
        InvoiceStatus::Funded
    );
}

/// Duplicates are only meaningful against the *same* oracle: a failover
/// oracle legitimately starts from its own timeline — even re-serving the same
/// numeric timestamp is its first read, not a duplicate.
#[test]
fn test_1053_different_oracle_is_not_a_duplicate() {
    let t = setup();
    let current_seq = t.env.ledger().sequence();
    let oracle_a = register_oracle_with_timestamp(&t, current_seq);

    // Put oracle A at the cap.
    stage_duplicate_streak(&t, &oracle_a, current_seq, MAX_DUPLICATE + 1);

    // Fail over to a different oracle (past the `set_price_oracle` rate-limit
    // cooldown) reporting the *same* timestamp.
    advance_ledger(&t.env, 150);
    let oracle_b_id = t.env.register_contract(None, MockTimestampedOracle);
    MockTimestampedOracleClient::new(&t.env, &oracle_b_id).set_timestamp(&current_seq);
    t.contract.set_price_oracle(&oracle_b_id).unwrap();

    let id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &true)
        .unwrap();
    assert_eq!(
        t.contract.get_invoice(&id).unwrap().status,
        InvoiceStatus::Funded
    );

    let health = t
        .contract
        .get_oracle_health(&OracleFeedType::Identity, &t.token.address)
        .unwrap();
    assert_eq!(health.oracle, oracle_b_id);
    assert_eq!(health.consecutive_duplicate_count, 0);
}

// ----------------------------------------------------------------
// Property: the duplicate predicate fires iff the reported timestamp equals
//           the recorded baseline for the same oracle AND the recorded
//           duplicate streak has already reached the cap
// ----------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn prop_1053_duplicate_predicate_matches_streak_bound(
        ts in 1u32..1_000_000u32,
        duplicates in 0u32..8u32,
        new_ts in 0u32..1_000_000u32,
    ) {
        let t = setup();
        let oracle = t.env.register_contract(None, MockTimestampedOracle);
        // `duplicates + 1` identical reads leave the streak at `duplicates`.
        stage_duplicate_streak(&t, &oracle, ts, duplicates + 1);

        let got = crate::oracle_registry::is_oracle_data_duplicate_update(
            &t.env,
            OracleFeedType::Identity,
            &t.token.address,
            &oracle,
            new_ts,
        );
        // Same oracle ⇒ duplicate iff the timestamp is unchanged *and* the
        // streak has already reached the cap (the next duplicate exceeds it).
        prop_assert_eq!(got, new_ts == ts && duplicates >= MAX_DUPLICATE);
    }
}
