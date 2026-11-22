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
    contract, contractimpl, contracttype,
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
    t.contract.set_price_oracle(&oracle_id);
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
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

    let invoice = t.contract.get_invoice(&invoice_id);
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
    t.contract.set_max_oracle_age(&max_age);

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
    t.contract.set_max_oracle_age(&max_age);

    let current_seq = t.env.ledger().sequence(); // 100
                                                 // age = 9 < 10 → should pass.
    register_oracle_with_timestamp(&t, current_seq - 9);

    let invoice_id = make_invoice(&t);

    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

    let invoice = t.contract.get_invoice(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Funded);
}

// ----------------------------------------------------------------
// Test 4: exactly at the limit fails (age = max)
// ----------------------------------------------------------------
#[test]
fn test_boundary_exactly_at_limit_fails() {
    let t = setup();
    let max_age: u64 = 10;
    t.contract.set_max_oracle_age(&max_age);

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
    t.contract.set_max_oracle_age(&0u64);

    let current_seq = t.env.ledger().sequence();
    // Very old timestamp — age would exceed any real limit.
    register_oracle_with_timestamp(&t, current_seq.saturating_sub(999_999));

    let invoice_id = make_invoice(&t);

    // Should succeed because max_oracle_age_ledgers == 0 skips the check.
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

    let invoice = t.contract.get_invoice(&invoice_id);
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
        .fund_invoice(&t.funder, &invoice_id1, &INVOICE_AMOUNT, &true);

    // Governance tightens the limit to 3 ledgers — same oracle data now stale.
    t.contract.set_max_oracle_age(&3u64);
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
    t.contract.set_max_oracle_age(&5u64);

    let current_seq = t.env.ledger().sequence();
    // Deliberately stale: age = 99 ≥ 5.
    register_oracle_with_timestamp(&t, current_seq.saturating_sub(99));

    let invoice_id = make_invoice(&t);

    // flag=false → oracle not consulted → no staleness check → succeeds.
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &false);

    let invoice = t.contract.get_invoice(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Funded);
}

// ----------------------------------------------------------------
// Test 8: staleness detected after ledger advances (dynamic staleness)
// ----------------------------------------------------------------
#[test]
fn test_data_becomes_stale_as_ledger_advances() {
    let t = setup();
    let max_age: u64 = 5;
    t.contract.set_max_oracle_age(&max_age);

    let oracle_ts = t.env.ledger().sequence(); // timestamp frozen at registration
    register_oracle_with_timestamp(&t, oracle_ts);

    // Advance 4 ledgers — still within limit (age = 4 < 5).
    advance_ledger(&t.env, 4);
    let invoice_id1 = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id1, &INVOICE_AMOUNT, &true);

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
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

    let invoice = t.contract.get_invoice(&invoice_id);
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

    t.contract.set_max_oracle_age(&42);

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
    t.contract.set_max_oracle_age(&5);

    // Oracle timestamp is 4 ledgers old → age=4 < 5 → fresh.
    register_oracle_with_timestamp(&t, current_seq - 4);

    let invoice_id1 = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id1, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.contract.get_invoice(&invoice_id1).status,
        InvoiceStatus::Funded
    );

    // Tighten to 3 ledgers → same oracle data (age=4) now stale.
    t.contract.set_max_oracle_age(&3);

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
    t.contract.set_max_oracle_age(&max_age);

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
    t.contract.set_max_oracle_age(&1u64);
    let current_seq = t.env.ledger().sequence();

    // age = 0 → fresh.
    register_oracle_with_timestamp(&t, current_seq);
    let id1 = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &id1, &INVOICE_AMOUNT, &true);

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
    t.contract.set_max_oracle_age(&max_age);

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
    t.contract.set_max_oracle_age(&0u64);

    let current_seq = t.env.ledger().sequence();
    register_oracle_with_timestamp(&t, current_seq + 1); // future-dated

    let invoice_id = make_invoice(&t);
    // The escape hatch (max_age == 0) disables the rule at every input, so the
    // future-timestamp rejection must not fire here.
    t.contract
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

    assert_eq!(
        t.contract.get_invoice(&invoice_id).status,
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
        .fund_invoice(&t.funder, &invoice_id2, &INVOICE_AMOUNT, &false);
    assert_eq!(
        t.contract.get_invoice(&invoice_id2).status,
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
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.contract.get_invoice(&invoice_id).status,
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
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

    // Ledger moves on but the oracle re-serves its last timestamp.
    advance_ledger(&t.env, 1);

    let invoice_id2 = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id2, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.contract.get_invoice(&invoice_id2).status,
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
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

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
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

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
        .fund_invoice(&t.funder, &invoice_ungated, &INVOICE_AMOUNT, &false);
    assert_eq!(
        t.contract.get_invoice(&invoice_ungated).status,
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
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.contract.get_invoice(&invoice_id).status,
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
        .fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);

    // Swap to a different oracle reporting an older timestamp (past the
    // rate-limit cooldown for `set_price_oracle`).
    advance_ledger(&t.env, 150);
    let oracle_b_id = t.env.register_contract(None, MockTimestampedOracle);
    MockTimestampedOracleClient::new(&t.env, &oracle_b_id).set_timestamp(&(current_seq - 40));
    t.contract.set_price_oracle(&oracle_b_id);

    let invoice_id2 = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &invoice_id2, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.contract.get_invoice(&invoice_id2).status,
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
        .fund_invoice(&t.funder, &id0, &INVOICE_AMOUNT, &true);

    // `MAX_DUPLICATE` further identical reads are exactly the duplicates up
    // to the bound — all accepted.
    for _ in 0..MAX_DUPLICATE {
        advance_ledger(&t.env, 1);
        let id = make_invoice(&t);
        t.contract
            .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &true);
        assert_eq!(
            t.contract.get_invoice(&id).status,
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
        .fund_invoice(&t.funder, &id0, &INVOICE_AMOUNT, &true);
    for _ in 0..MAX_DUPLICATE {
        advance_ledger(&t.env, 1);
        let id = make_invoice(&t);
        t.contract
            .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &true);
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
        .fund_invoice(&t.funder, &id_ok, &INVOICE_AMOUNT, &true);
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
        .fund_invoice(&t.funder, &id0, &INVOICE_AMOUNT, &true);
    for _ in 0..MAX_DUPLICATE {
        advance_ledger(&t.env, 1);
        let id = make_invoice(&t);
        t.contract
            .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &true);
    }

    // Oracle advances: same oracle, newer timestamp → accepted, streak resets.
    advance_ledger(&t.env, 1);
    let new_ts = t.env.ledger().sequence();
    MockTimestampedOracleClient::new(&t.env, &oracle_id).set_timestamp(&new_ts);

    let id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &true);

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
        .fund_invoice(&t.funder, &id_ungated, &INVOICE_AMOUNT, &false);
    assert_eq!(
        t.contract.get_invoice(&id_ungated).status,
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
    t.contract.set_price_oracle(&oracle_b_id);

    let id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.contract.get_invoice(&id).status,
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

// ================================================================
// Issue #1046 — authorize oracle feeder updates
// ================================================================
//
// `MockTimestampedOracle` (above) is a *read-only* fixture: it is always
// verified and tests invert control through `set_timestamp`. It therefore
// cannot exercise the write path that actually decides funding eligibility.
// The oracle below models the real one: `update_verification` is the feeder
// (operator) entry point, it is gated on the configured feeder's
// authorization, and it stamps each accepted update with the ledger it
// happened at — so a feeder update is a genuine data event that the ILN
// freshness rule ages like any other.
//
// Before #1046: an oracle's `update_verification` was documented as requiring
// the operator's `require_auth()` (see `oracle_interface.rs` and
// `docs/oracle-integration.md`) but nothing enforced it, so anyone could mint
// funding eligibility for any payer. After: only the configured feeder can, and
// the resulting data still has to be fresh for `fund_invoice` to honour it.

// ----------------------------------------------------------------
// Feeder-authorized oracle fixture
// ----------------------------------------------------------------
#[contract]
struct MockFeederOracle;

#[contracttype]
enum FeederOracleKey {
    /// Authorized feeder (oracle operator) address.
    Feeder,
    /// Per-payer verification flag.
    Verified(Address),
    /// Ledger sequence of the last accepted feeder update.
    Timestamp,
}

#[contractimpl]
impl MockFeederOracle {
    pub fn interface_version(_env: Env) -> u32 {
        crate::oracle_interface::ORACLE_INTERFACE_VERSION
    }

    /// Feeder (oracle operator) write path — Issue #1046.
    ///
    /// Requires the configured feeder's authorization and rejects the call
    /// when no feeder has been configured.
    pub fn update_verification(env: Env, payer: Address, verified: bool) {
        let feeder: Address = match env.storage().instance().get(&FeederOracleKey::Feeder) {
            Some(f) => f,
            None => panic!("feeder not configured"),
        };
        feeder.require_auth();
        env.storage()
            .persistent()
            .set(&FeederOracleKey::Verified(payer), &verified);
        // A successful feeder update is a real data event: stamp it with the
        // ledger it happened at so the ILN freshness rule can age it.
        env.storage()
            .instance()
            .set(&FeederOracleKey::Timestamp, &env.ledger().sequence());
    }

    pub fn get_payer_data(env: Env, payer: Address) -> OracleVerificationResponse {
        let verified: bool = env
            .storage()
            .persistent()
            .get(&FeederOracleKey::Verified(payer))
            .unwrap_or(false);
        let timestamp: u32 = env
            .storage()
            .instance()
            .get(&FeederOracleKey::Timestamp)
            .unwrap_or(env.ledger().sequence());
        OracleVerificationResponse {
            is_verified: verified,
            timestamp,
        }
    }

    /// Test setup helper: configure the authorized feeder address.
    pub fn set_feeder(env: Env, feeder: Address) {
        env.storage()
            .instance()
            .set(&FeederOracleKey::Feeder, &feeder);
    }
}

/// Register a `MockFeederOracle` with `feeder` configured, wire it into the ILN
/// config as the price oracle, and return its address.
fn register_feeder_oracle(t: &crate::test::TestEnv, feeder: &Address) -> Address {
    let oracle_id = t.env.register_contract(None, MockFeederOracle);
    MockFeederOracleClient::new(&t.env, &oracle_id).set_feeder(feeder);
    t.contract.set_price_oracle(&oracle_id);
    oracle_id
}

// ----------------------------------------------------------------
// Test: only a feeder update opens the funding gate; without it the payer
//       stays unverified and funding fails closed.
// ----------------------------------------------------------------
#[test]
fn test_1046_feeder_authorized_update_opens_funding_gate() {
    let t = setup();
    let feeder = Address::generate(&t.env);
    let oracle = register_feeder_oracle(&t, &feeder);

    // No feeder update yet → unverified → funding blocked.
    let blocked = make_invoice(&t);
    assert_eq!(
        t.contract
            .try_fund_invoice(&t.funder, &blocked, &INVOICE_AMOUNT, &true),
        Err(Ok(ContractError::PayerUnverified)),
        "without a feeder update the payer must remain unverified"
    );

    // Authorized feeder update (mock_all_auths satisfies the feeder's
    // require_auth) → payer verified with current-ledger data → funding opens.
    MockFeederOracleClient::new(&t.env, &oracle).update_verification(&t.payer, &true);

    let funded = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &funded, &INVOICE_AMOUNT, &true);
    assert_eq!(
        t.contract.get_invoice(&funded).status,
        InvoiceStatus::Funded
    );
}

// ----------------------------------------------------------------
// Test: the feeder gate does not run when oracle verification is not required.
// ----------------------------------------------------------------
#[test]
fn test_1046_gate_only_applies_when_oracle_required() {
    let t = setup();
    let feeder = Address::generate(&t.env);
    let _oracle = register_feeder_oracle(&t, &feeder);

    // require_oracle_verification = false → oracle never consulted → funding
    // succeeds even though no feeder update ever happened.
    let id = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &id, &INVOICE_AMOUNT, &false);
    assert_eq!(t.contract.get_invoice(&id).status, InvoiceStatus::Funded);
}

// ----------------------------------------------------------------
// Test: a feeder update is a data event — it is aged by the freshness rule,
//       and a refresh restores funding after staleness.
// ----------------------------------------------------------------
#[test]
fn test_1046_feeder_update_is_subject_to_freshness() {
    let t = setup();
    t.contract.set_max_oracle_age(&5u64);
    let feeder = Address::generate(&t.env);
    let oracle = register_feeder_oracle(&t, &feeder);
    let client = MockFeederOracleClient::new(&t.env, &oracle);

    // Feeder writes at ledger 100.
    client.update_verification(&t.payer, &true);

    // age = 4 < 5 → fresh.
    advance_ledger(&t.env, 4);
    let fresh = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &fresh, &INVOICE_AMOUNT, &true);

    // age = 5 == max → the stale feeder update no longer unlocks funding.
    advance_ledger(&t.env, 1);
    let stale = make_invoice(&t);
    assert_eq!(
        t.contract
            .try_fund_invoice(&t.funder, &stale, &INVOICE_AMOUNT, &true),
        Err(Ok(ContractError::OracleDataStale)),
        "a feeder update must not stay fresh indefinitely"
    );

    // A fresh authorized feeder update re-opens the gate.
    client.update_verification(&t.payer, &true);
    let refreshed = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &refreshed, &INVOICE_AMOUNT, &true);
}

// ----------------------------------------------------------------
// Test: ledger-boundary walk across the feeder-update freshness window —
//       last-fresh ledger passes, first-stale ledger rejects, and a feeder
//       refresh exactly at the boundary restores funding.
// ----------------------------------------------------------------
#[test]
fn test_1046_feeder_update_freshness_boundary_walk() {
    let t = setup();
    let max_age: u64 = 10;
    t.contract.set_max_oracle_age(&max_age);
    let feeder = Address::generate(&t.env);
    let oracle = register_feeder_oracle(&t, &feeder);
    let client = MockFeederOracleClient::new(&t.env, &oracle);

    // Feeder writes at ledger 100.
    client.update_verification(&t.payer, &true);

    // Ledger 109: age = 9 = max - 1 → last fresh ledger.
    advance_ledger(&t.env, 9);
    let fresh = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &fresh, &INVOICE_AMOUNT, &true);

    // Ledger 110: age = 10 = max → first stale ledger.
    advance_ledger(&t.env, 1);
    let stale = make_invoice(&t);
    assert_eq!(
        t.contract
            .try_fund_invoice(&t.funder, &stale, &INVOICE_AMOUNT, &true),
        Err(Ok(ContractError::OracleDataStale))
    );

    // Feeder refresh at the boundary → fresh again.
    client.update_verification(&t.payer, &true);
    let refreshed = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &refreshed, &INVOICE_AMOUNT, &true);
}

// ----------------------------------------------------------------
// Test: the feeder can revoke verification, and revocation is honoured by the
//       funding gate.
// ----------------------------------------------------------------
#[test]
fn test_1046_feeder_can_revoke_verification() {
    let t = setup();
    let feeder = Address::generate(&t.env);
    let oracle = register_feeder_oracle(&t, &feeder);
    let client = MockFeederOracleClient::new(&t.env, &oracle);

    client.update_verification(&t.payer, &true);
    let id1 = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &id1, &INVOICE_AMOUNT, &true);

    client.update_verification(&t.payer, &false);
    let id2 = make_invoice(&t);
    assert_eq!(
        t.contract
            .try_fund_invoice(&t.funder, &id2, &INVOICE_AMOUNT, &true),
        Err(Ok(ContractError::PayerUnverified)),
        "a feeder revocation must close the funding gate"
    );
}

// ================================================================
// Issue #1191 — oracle circuit tripping and recovery at ledger boundaries
// ================================================================
//
// Scope and neighbors
// -------------------
// This file owns the *freshness* family (#93, #1050, #1052, #1053) and is the
// natural home for the circuit-breaker behavior those rules feed into: the
// breaker trips on a sustained run of stale observations, and Issue #1048 makes
// it half-open — a sustained run of *healthy* observations from the tripping
// oracle auto-recovers it. The sibling suites own the neighboring failures and
// are deliberately not duplicated here:
//   * multi-source price **deviation** outliers — `tests_price_deviation.rs`
//     (and the #1054 product rules) plus `oracle_registry.rs`;
//   * Identity feeder **quorum** agreement (`check_identity_quorum`) — the
//     `tests_oracle_registry.rs` quorum suite;
//   * the canonical breaker/auto-recovery suite, the price-source registry
//     invariants (#1190), and the admin-only check for `reset_oracle_circuit`
//     (`test_reset_oracle_circuit_requires_admin`) — `tests_oracle_registry.rs`.
//
// What this section adds is the part those suites leave implicit: the exact
// **ledger boundaries** of tripping and of half-open recovery, asserted with
// the fixtures already defined above (the read-only `MockTimestampedOracle`
// wired in via the legacy `price_oracle` Identity fallback).
//
// Data source
// -----------
// `check_oracle_health` is used as the observation path. It is permissionless
// and circuit-agnostic (see its doc) — it resolves through the plain
// `resolve_oracle`, invokes the oracle's `get_payer_data`, and records the
// observation without ever erroring on stale data. Soroban rolls back every
// write of an `Err`-returning invocation, so a *gated* `fund_invoice` path
// cannot advance breaker bookkeeping while the breaker is open; the
// never-erroring keeper path is therefore what drives both tripping and
// recovery. This mirrors the real deployment's monitor/keeper usage.
//
// Authorization rules
// -------------------
//   * Tripping and auto-recovery require **no** authorization: anyone may call
//     `check_oracle_health`, and the observations it records are what advance
//     the stale streak and the recovery streak. This is intentional — recovery
//     must not be gated behind the very admin action it is meant to make
//     unnecessary.
//   * `reset_oracle_circuit` (the governance fast path) is **admin-only** and
//     rate-limited; its rejection of non-admins is covered by
//     `tests_oracle_registry.rs::test_reset_oracle_circuit_requires_admin`, and
//     its clearing of half-open progress is asserted below.
//   * `set_max_oracle_age` / `set_price_oracle` are admin-only and
//     rate-limited (`DEFAULT_RATE_LIMIT_LEDGERS`). The fixtures here rely on
//     `check_rate_limit` allowing a function's first-ever call unconditionally;
//     where an oracle is swapped mid-test the ledger is advanced past the
//     cooldown first.
//
// Failure behavior (verified, not assumed)
// -----------------------------------------
//   * The breaker trips on the `MAX_CONSECUTIVE_STALE_QUERIES`-th consecutive
//     *stale* observation from the same oracle, and not one observation before.
//   * While tripped with no fallback registered, `fund_invoice` fails closed
//     with `OracleCircuitOpen` rather than consulting the (possibly now-fresh)
//     oracle.
//   * A single fresh query never recovers. Recovery requires **both**
//     `CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS` healthy (fresh *and*
//     timestamp-advancing) observations **and** at least
//     `CIRCUIT_RECOVERY_MIN_LEDGERS` elapsed since the first of them. "Healthy"
//     excludes frozen/replayed timestamps and non-fresh data.
//   * Any unhealthy observation mid-streak resets the recovery streak, and
//     only the oracle recorded as having tripped the breaker may recover it.
//   * A recovered oracle that goes stale again re-trips promptly, bounding
//     flapping.

/// Freshness window the circuit tests use: small enough that a deliberately
/// old timestamp reads stale without touching the oracle again, yet wide
/// enough that a same-ledger refresh reads fresh. Read from a literal only
/// because these tests intentionally *tighten* governance's default window.
const CIRCUIT_MAX_AGE: u64 = 10;

/// Requested-consecutive-stale threshold, read from the enforcing constant so
/// the tests cannot drift from the implementation.
const CIRCUIT_STALE_THRESHOLD: u32 = crate::oracle_registry::MAX_CONSECUTIVE_STALE_QUERIES;

/// Trip the Identity breaker for `t.token` by driving exactly
/// `CIRCUIT_STALE_THRESHOLD` consecutive stale observations through the
/// permissionless `check_oracle_health` keeper path, and return the stale
/// oracle address.
///
/// The oracle's timestamp is pinned far enough behind the ledger that it reads
/// stale on every observation without further ledger movement. `max_oracle_age`
/// is tightened first so that "far enough" is small; both admin calls are
/// first-ever for this test, so the rate limiter allows them unconditionally.
fn trip_identity_circuit(t: &crate::test::TestEnv) -> Address {
    t.contract.set_max_oracle_age(&CIRCUIT_MAX_AGE);
    let base = t.env.ledger().sequence();
    let oracle = register_oracle_with_timestamp(t, base.saturating_sub(50));
    for _ in 0..CIRCUIT_STALE_THRESHOLD {
        t.contract.check_oracle_health(
            &OracleFeedType::Identity,
            &t.token.address,
            &t.payer,
        );
    }
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "setup must leave the Identity breaker tripped"
    );
    oracle
}

/// Serve one **fresh, timestamp-advancing** observation from `oracle` at the
/// current ledger and record it through the keeper path. Each call advances the
/// oracle's timestamp to the current ledger, so consecutive calls (spaced by
/// `advance_ledger`) are genuine forward data — the "healthy" evidence the
/// half-open recovery requires.
fn observe_fresh(t: &crate::test::TestEnv, oracle: &Address) {
    MockTimestampedOracleClient::new(&t.env, oracle).set_timestamp(&t.env.ledger().sequence());
    t.contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer);
}

/// Drive the tripping oracle to a full healthy recovery run: the required
/// number of healthy observations, spaced past the recovery ledger window.
fn drive_to_recovery(t: &crate::test::TestEnv, oracle: &Address) {
    let window = crate::oracle_registry::CIRCUIT_RECOVERY_MIN_LEDGERS as u32;
    let needed = crate::oracle_registry::CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS;
    for i in 0..needed {
        if i > 0 {
            advance_ledger(&t.env, window);
        }
        observe_fresh(t, oracle);
    }
}

// ----------------------------------------------------------------
// Tripping: the streak boundary, and fail-closed funding while open
// ----------------------------------------------------------------

#[test]
fn test_1191_circuit_trips_exactly_at_stale_threshold() {
    let t = setup();
    t.contract.set_max_oracle_age(&CIRCUIT_MAX_AGE);
    let base = t.env.ledger().sequence();
    register_oracle_with_timestamp(&t, base.saturating_sub(50));

    // One observation *below* the threshold must leave the breaker closed.
    for _ in 0..(CIRCUIT_STALE_THRESHOLD - 1) {
        t.contract.check_oracle_health(
            &OracleFeedType::Identity,
            &t.token.address,
            &t.payer,
        );
    }
    assert!(
        !t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "the breaker must not trip one observation below the threshold"
    );

    // The threshold-crossing observation trips it, and the recorded streak
    // equals the cap.
    let health = t
        .contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer)
        .unwrap();
    assert_eq!(health.consecutive_stale_count, CIRCUIT_STALE_THRESHOLD);
    assert!(t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));

    // Further stale queries keep it tripped (idempotent); the streak keeps
    // climbing only for observability, nothing re-fires on an open breaker.
    let health = t
        .contract
        .check_oracle_health(&OracleFeedType::Identity, &t.token.address, &t.payer)
        .unwrap();
    assert_eq!(health.consecutive_stale_count, CIRCUIT_STALE_THRESHOLD + 1);
    assert!(t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));
}

#[test]
fn test_1191_funding_gate_rejects_when_circuit_open() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);

    // The oracle is refreshed to fresh data...
    MockTimestampedOracleClient::new(&t.env, &oracle).set_timestamp(&t.env.ledger().sequence());

    // ...but with no fallback registered, oracle-gated funding must fail closed
    // with OracleCircuitOpen rather than consult the freshly-reporting oracle.
    let invoice_id = make_invoice(&t);
    let result = t
        .contract
        .try_fund_invoice(&t.funder, &invoice_id, &INVOICE_AMOUNT, &true);
    assert_eq!(
        result,
        Err(Ok(ContractError::OracleCircuitOpen)),
        "an open breaker with no fallback must fail closed"
    );

    // The gate is a data-validity outcome, not an authorization one: skipping
    // verification never consults the oracle, so the same invoice funds.
    let ungated = make_invoice(&t);
    t.contract
        .fund_invoice(&t.funder, &ungated, &INVOICE_AMOUNT, &false);
    assert_eq!(
        t.contract.get_invoice(&ungated).status,
        InvoiceStatus::Funded
    );
}

// ----------------------------------------------------------------
// Recovery: observation-count and ledger-window boundaries
// ----------------------------------------------------------------

#[test]
fn test_1191_single_fresh_query_does_not_recover() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);

    // A single healthy observation must not clear the breaker (anti-flapping).
    observe_fresh(&t, &oracle);
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "a single fresh query must not auto-recover the breaker"
    );
}

#[test]
fn test_1191_recovery_needs_full_observation_count() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);
    let window = crate::oracle_registry::CIRCUIT_RECOVERY_MIN_LEDGERS as u32;
    let needed = crate::oracle_registry::CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS;

    // Observations spanning well past the ledger window but one short of the
    // required count — still not recovered.
    for _ in 0..(needed - 1) {
        observe_fresh(&t, &oracle);
        advance_ledger(&t.env, window);
    }
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "fewer than CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS must not recover"
    );

    // The final healthy observation completes both conditions.
    observe_fresh(&t, &oracle);
    assert!(
        !t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address)
    );
}

#[test]
fn test_1191_recovery_needs_full_ledger_window() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);
    let window = crate::oracle_registry::CIRCUIT_RECOVERY_MIN_LEDGERS as u32;

    // Streak start recorded at the current ledger.
    observe_fresh(&t, &oracle);

    // Second healthy observation one ledger later (keeps the timestamp
    // advancing while barely moving the ledger window).
    advance_ledger(&t.env, 1);
    observe_fresh(&t, &oracle);

    // Third healthy observation, but only `window - 1` ledgers after the streak
    // began: the required observation count is met while the ledger window is
    // one short — the exact lower boundary — so recovery must NOT complete.
    advance_ledger(&t.env, window - 2);
    observe_fresh(&t, &oracle);
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "recovery must not complete one ledger short of the window"
    );

    // One ledger later the window is exactly satisfied — recovery completes.
    advance_ledger(&t.env, 1);
    observe_fresh(&t, &oracle);
    assert!(
        !t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "recovery must complete once the full ledger window elapses"
    );
}

// ----------------------------------------------------------------
// Recovery: healthy means fresh *and* advancing; resets and isolation
// ----------------------------------------------------------------

#[test]
fn test_1191_recovery_ignores_frozen_oracle() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);
    let needed = crate::oracle_registry::CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS;

    // Re-serve the *same* fresh value repeatedly: non-advancing data is not
    // evidence of recovery, so even more than the required observations fail.
    let frozen = t.env.ledger().sequence();
    let client = MockTimestampedOracleClient::new(&t.env, &oracle);
    client.set_timestamp(&frozen);
    for _ in 0..(needed + 1) {
        t.contract.check_oracle_health(
            &OracleFeedType::Identity,
            &t.token.address,
            &t.payer,
        );
        advance_ledger(&t.env, 1);
    }
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "a frozen (non-advancing) oracle must not recover the breaker"
    );
}

#[test]
fn test_1191_recovery_resets_on_stale_observation() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);
    let window = crate::oracle_registry::CIRCUIT_RECOVERY_MIN_LEDGERS as u32;

    // One healthy observation to start the streak...
    observe_fresh(&t, &oracle);
    advance_ledger(&t.env, window);

    // ...then a stale observation mid-streak resets the progress.
    MockTimestampedOracleClient::new(&t.env, &oracle).set_timestamp(&0); // ancient → stale
    t.contract.check_oracle_health(
        &OracleFeedType::Identity,
        &t.token.address,
        &t.payer,
    );

    // ...so a single healthy observation afterwards is only streak 1 — still
    // not recovered. (A point in time recorded just before a reset must not
    // count toward the run that follows.)
    advance_ledger(&t.env, window);
    observe_fresh(&t, &oracle);
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "a stale observation must reset the recovery streak"
    );
}

#[test]
fn test_1191_recovery_ignores_different_oracle() {
    let t = setup();
    let stale = trip_identity_circuit(&t);

    // A different oracle reports fresh, advancing data across the full window.
    advance_ledger(&t.env, 150); // past the set_price_oracle rate-limit cooldown
    let base = t.env.ledger().sequence();
    let healthy = register_oracle_with_timestamp(&t, base);
    let window = crate::oracle_registry::CIRCUIT_RECOVERY_MIN_LEDGERS as u32;
    let needed = crate::oracle_registry::CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS;
    for i in 0..needed {
        if i > 0 {
            advance_ledger(&t.env, window);
        }
        observe_fresh(&t, &healthy);
    }

    // But the breaker was tripped by `stale`, so the healthy oracle's sustained
    // run must not clear it.
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "only the oracle that tripped the breaker may auto-recover it"
    );
    assert_ne!(stale, healthy);
}

// ----------------------------------------------------------------
// Recovery: end-to-end, event, re-trip, and governance reset
// ----------------------------------------------------------------

#[test]
fn test_1191_auto_recovers_and_funding_resumes_without_admin() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);

    drive_to_recovery(&t, &oracle);

    // No admin/governance call was made anywhere in this test — recovery is
    // driven entirely by the permissionless keeper path.
    assert!(
        !t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "sustained healthy observations must auto-recover the breaker"
    );
    assert!(
        t.contract
            .get_circuit_recovery_state(&OracleFeedType::Identity, &t.token.address)
            .is_none(),
        "a completed recovery must clear the half-open bookkeeping"
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
fn test_1191_recovery_emits_event() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);

    t.env.events().all(); // clear setup events
    drive_to_recovery(&t, &oracle);

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
fn test_1191_retrips_after_recovery_if_stale_again() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);
    drive_to_recovery(&t, &oracle);
    assert!(!t
        .contract
        .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address));

    // The recovered oracle goes stale again → the breaker re-trips, so
    // auto-recovery cannot be used to keep a flapping oracle trusted.
    let window = crate::oracle_registry::CIRCUIT_RECOVERY_MIN_LEDGERS as u32;
    let ancient = t.env.ledger().sequence().saturating_sub(window + 100);
    MockTimestampedOracleClient::new(&t.env, &oracle).set_timestamp(&ancient);
    for _ in 0..CIRCUIT_STALE_THRESHOLD {
        t.contract.check_oracle_health(
            &OracleFeedType::Identity,
            &t.token.address,
            &t.payer,
        );
    }
    assert!(
        t.contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &t.token.address),
        "a recovered oracle that goes stale again must re-trip the breaker"
    );
}

#[test]
fn test_1191_reset_clears_recovery_progress() {
    let t = setup();
    let oracle = trip_identity_circuit(&t);

    // Start a recovery run so there is half-open progress to clear.
    observe_fresh(&t, &oracle);
    assert!(t
        .contract
        .get_circuit_recovery_state(&OracleFeedType::Identity, &t.token.address)
        .is_some());

    // Governance fast path: clears both the tripped flag and the progress.
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
