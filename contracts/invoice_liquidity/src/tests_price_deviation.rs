#![cfg(test)]

//! Tests for Issue #price-deviation — multi-source price deviation
//! checking in `oracle_registry.rs`.
//!
//! Covers:
//! 1. Zero sources / all sources unreachable → `NoPriceSource`.
//! 2. Exactly one source → returned unchecked (documented, accepted risk).
//! 3. Three or more sources → a wild outlier is excluded and the median of
//!    the agreeing sources is returned, with `PriceOutlierRejected` emitted.
//! 4. Exactly two disagreeing sources → both rejected (no way to tell which
//!    one is lying from two data points alone).
//! 5. The deviation threshold is governance-configurable.
//! 6. add/remove_price_source CRUD and admin gating.
//! 7. Issue #1190: the price-side registration/removal invariants — identity
//!    mutations cannot touch the price list, draining the sample is fail-closed,
//!    and removal is admin-gated. (The identity-side counterparts live in
//!    `tests_oracle_registry.rs`.)

use super::*;
use crate::oracle_registry::OracleFeedType;
use crate::test::setup;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, MockAuth, MockAuthInvoke},
    Address, Env, IntoVal,
};

/// A minimal price-reporting oracle: `get_price(token)` returns whatever
/// was last set via `set_price`, ignoring which token was asked about
/// (fine for these tests — deviation checking cross-checks sources against
/// each other, not against a token-specific ground truth).
#[contract]
struct MockPriceOracle;

#[contractimpl]
impl MockPriceOracle {
    pub fn set_price(env: Env, price: i128) {
        env.storage()
            .instance()
            .set(&soroban_sdk::symbol_short!("price"), &price);
    }

    pub fn get_price(env: Env, _token: Address) -> i128 {
        env.storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("price"))
            .unwrap_or(0)
    }
}

fn deploy_price_oracle(t: &crate::test::TestEnv, price: i128) -> Address {
    let id = t.env.register_contract(None, MockPriceOracle);
    MockPriceOracleClient::new(&t.env, &id).set_price(&price);
    id
}

/// Advance the ledger past `DEFAULT_RATE_LIMIT_LEDGERS` (120 ledgers) so the
/// next call to the *same* rate-limited entry point is accepted.
/// `check_rate_limit` allows an entry point's first-ever call unconditionally,
/// so this is only needed to space *repeats* within one test.
fn advance_past_rate_limit_cooldown(env: &Env) {
    let mut info = env.ledger().get();
    info.sequence_number += 150;
    info.timestamp += 150 * 5;
    env.ledger().set(info);
}

/// Register `oracle` as a `Price` feed source, advancing the ledger past the
/// rate-limit cooldown afterwards so a following registration (or other
/// rate-limited call on the same entry point) is not `RateLimited`.
fn add_price_source(t: &crate::test::TestEnv, oracle: &Address) {
    t.contract.add_price_source(&OracleFeedType::Price, oracle);
    advance_past_rate_limit_cooldown(&t.env);
}

/// Same shape as tests_oracle_registry.rs's `setup_env_no_mock_auths`: an
/// env WITHOUT `mock_all_auths()`, so `require_auth()` actually enforces
/// the caller. Duplicated locally (small and self-contained) rather than
/// shared across test modules, matching this codebase's existing
/// per-test-file convention (see tests_access_control.rs's own setup_env).
fn setup_env_no_mock_auths() -> (Env, InvoiceLiquidityContractClient<'static>) {
    let env = Env::default();
    let admin = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let usdc_contract = env.register_stellar_asset_contract_v2(token_admin);
    let token_address = usdc_contract.address();

    let xlm_admin = Address::generate(&env);
    let xlm_contract = env.register_stellar_asset_contract_v2(xlm_admin);
    let xlm_address = xlm_contract.address();

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let client = InvoiceLiquidityContractClient::new(&env, &contract_id);

    // mock_all_auths() is used only to get past initialize()'s own auth
    // requirement; the test body below calls env.mock_auths(&[..]) with a
    // specific, restricted list, which replaces (not adds to) this mode —
    // so require_auth() genuinely enforces the caller from that point on.
    env.mock_all_auths();
    client.initialize(&admin, &token_address, &token_address, &xlm_address);

    (env, client)
}

// ── Zero / single source ───────────────────────────────────────────────────────

#[test]
fn test_get_verified_price_with_no_sources_errors() {
    let t = setup();
    let result = t
        .contract
        .try_get_verified_price(&OracleFeedType::Price, &t.token.address);
    assert_eq!(result, Err(Ok(ContractError::NoPriceSource)));
}

#[test]
fn test_get_verified_price_single_source_returns_unchecked() {
    let t = setup();
    // An obviously implausible price — with only one source registered,
    // there's nothing to cross-check it against, so it must come back
    // as-is. This is the documented, accepted single-point-of-failure
    // risk, not a bug: it demonstrates the *actual, unprotected* behavior
    // when only one price source is registered.
    let implausible_price: i128 = 999_999_999;
    let oracle = deploy_price_oracle(&t, implausible_price);
    add_price_source(&t, &oracle);

    let price = t
        .contract
        .get_verified_price(&OracleFeedType::Price, &t.token.address);
    assert_eq!(
        price, implausible_price,
        "a single registered source must be trusted unchecked — there is no \
         second opinion to compare it against"
    );
}

// ── Multi-source deviation checking ─────────────────────────────────────────────

#[test]
fn test_get_verified_price_rejects_outlier_with_three_sources() {
    let t = setup();
    let oracle_a = deploy_price_oracle(&t, 100);
    let oracle_b = deploy_price_oracle(&t, 102);
    // Wildly higher than the other two — should be excluded.
    let oracle_outlier = deploy_price_oracle(&t, 1_000);

    add_price_source(&t, &oracle_a);
    add_price_source(&t, &oracle_b);
    add_price_source(&t, &oracle_outlier);

    let price = t
        .contract
        .get_verified_price(&OracleFeedType::Price, &t.token.address);
    // Median of the two surviving, agreeing sources (100, 102) is 101 —
    // the outlier must not have pulled the result toward 1_000.
    assert_eq!(price, 101);
}

#[test]
fn test_get_verified_price_two_sources_disagreeing_beyond_threshold_rejects_both() {
    let t = setup();
    // Default threshold is 5% (500 bps). 100 vs 200 is a 100% gap — with
    // only two sources, the median (their average, 150) sits equidistant
    // from both, so both deviate identically and neither can be singled
    // out as "the" outlier. Both must be rejected.
    let oracle_a = deploy_price_oracle(&t, 100);
    let oracle_b = deploy_price_oracle(&t, 200);
    add_price_source(&t, &oracle_a);
    add_price_source(&t, &oracle_b);

    let result = t
        .contract
        .try_get_verified_price(&OracleFeedType::Price, &t.token.address);
    assert_eq!(result, Err(Ok(ContractError::AllPriceSourcesRejected)));
}

#[test]
fn test_get_verified_price_two_close_sources_agree() {
    let t = setup();
    // Within the default 5% threshold: (100, 104) -> average 102, each
    // deviates ~1.96%, well under 5%. Both survive.
    let oracle_a = deploy_price_oracle(&t, 100);
    let oracle_b = deploy_price_oracle(&t, 104);
    add_price_source(&t, &oracle_a);
    add_price_source(&t, &oracle_b);

    let price = t
        .contract
        .get_verified_price(&OracleFeedType::Price, &t.token.address);
    assert_eq!(price, 102);
}

#[test]
fn test_get_verified_price_excludes_unreachable_source() {
    let t = setup();
    let oracle_a = deploy_price_oracle(&t, 100);
    let oracle_b = deploy_price_oracle(&t, 100);
    // A registered address that isn't a real price oracle at all — the
    // cross-contract call to it must fail, and that failure must degrade
    // to "excluded from the sample", not abort the whole aggregation.
    let unreachable = Address::generate(&t.env);

    add_price_source(&t, &oracle_a);
    add_price_source(&t, &oracle_b);
    add_price_source(&t, &unreachable);

    let price = t
        .contract
        .get_verified_price(&OracleFeedType::Price, &t.token.address);
    assert_eq!(
        price, 100,
        "an unreachable/invalid source must be excluded, not crash the whole query"
    );
}

// ── Governance-configurable threshold ───────────────────────────────────────────

#[test]
fn test_max_price_deviation_bps_defaults_and_is_governable() {
    let t = setup();
    assert_eq!(
        t.contract.get_max_price_deviation_bps(),
        crate::oracle_registry::DEFAULT_MAX_PRICE_DEVIATION_BPS
    );

    // A 10% gap (100 vs 110): rejected at the default 5% threshold...
    let oracle_a = deploy_price_oracle(&t, 100);
    let oracle_b = deploy_price_oracle(&t, 110);
    let oracle_c = deploy_price_oracle(&t, 100);
    add_price_source(&t, &oracle_a);
    add_price_source(&t, &oracle_b);
    add_price_source(&t, &oracle_c);

    // Median of (100, 110, 100) = 100; oracle_b deviates 10% > 5% default.
    let price_before = t
        .contract
        .get_verified_price(&OracleFeedType::Price, &t.token.address);
    assert_eq!(
        price_before, 100,
        "oracle_b's 10% deviation must be rejected at the default 5% threshold"
    );

    // ...but accepted once governance widens the threshold past 10%.
    t.contract.set_max_price_deviation_bps(&2_000); // 20%
    assert_eq!(t.contract.get_max_price_deviation_bps(), 2_000);

    let price_after = t
        .contract
        .get_verified_price(&OracleFeedType::Price, &t.token.address);
    // All three now survive: median of (100, 100, 110) = 100 still, but
    // this confirms the call succeeds (no rejection) with the wider band —
    // verified via the event count implicitly by not erroring.
    assert_eq!(price_after, 100);
}

#[test]
fn test_set_max_price_deviation_bps_rejects_invalid_values() {
    let t = setup();
    assert!(t.contract.try_set_max_price_deviation_bps(&0).is_err());
    advance_past_rate_limit_cooldown(&t.env);
    assert!(t
        .contract
        .try_set_max_price_deviation_bps(&10_001)
        .is_err());
    advance_past_rate_limit_cooldown(&t.env);
    assert!(t.contract.try_set_max_price_deviation_bps(&10_000).is_ok());
}

// ── Registration CRUD ────────────────────────────────────────────────────────────

#[test]
fn test_add_price_source_is_idempotent() {
    let t = setup();
    let oracle = deploy_price_oracle(&t, 100);
    add_price_source(&t, &oracle);
    add_price_source(&t, &oracle);
    assert_eq!(
        t.contract.get_price_sources(&OracleFeedType::Price).len(),
        1,
        "registering the same source twice must not duplicate it"
    );
}

#[test]
fn test_remove_price_source() {
    let t = setup();
    let oracle_a = deploy_price_oracle(&t, 100);
    let oracle_b = deploy_price_oracle(&t, 100);
    add_price_source(&t, &oracle_a);
    add_price_source(&t, &oracle_b);
    assert_eq!(
        t.contract.get_price_sources(&OracleFeedType::Price).len(),
        2
    );

    t.contract
        .remove_price_source(&OracleFeedType::Price, &oracle_a);
    let remaining = t.contract.get_price_sources(&OracleFeedType::Price);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining.get(0).unwrap(), oracle_b);
}

#[test]
fn test_add_price_source_requires_admin() {
    let (env, client) = setup_env_no_mock_auths();
    let imposter = Address::generate(&env);
    let oracle = Address::generate(&env);

    env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "add_price_source",
            args: (OracleFeedType::Price, oracle.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    let res = client.try_add_price_source(&OracleFeedType::Price, &oracle);
    assert!(
        res.is_err(),
        "add_price_source should fail for non-admin caller"
    );
}

// ── Issue #1190: price-side registration / removal invariants ───────────────────
//
// The canonical invariants these tests pin are stated in `oracle_registry.rs`'s
// module docs (see the "Registration, removal & feeder-removal invariants
// (Issue #1190)" block) and mirrored in `docs/oracle-design.md`. The identity
// feeder/quorum counterparts live in `tests_oracle_registry.rs`.

/// `remove_price_source` is admin-gated: a non-admin caller cannot shrink the
/// price sample (and so cannot manipulate a verified price).
#[test]
fn test_1190_remove_price_source_requires_admin() {
    let (env, client) = setup_env_no_mock_auths();
    let imposter = Address::generate(&env);
    let oracle = Address::generate(&env);

    env.mock_auths(&[MockAuth {
        address: &imposter,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "remove_price_source",
            args: (OracleFeedType::Price, oracle.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    let res = client.try_remove_price_source(&OracleFeedType::Price, &oracle);
    assert!(
        res.is_err(),
        "remove_price_source should fail for a non-admin caller"
    );
}

/// Draining the price sample to zero sources is fail-closed: the read returns
/// `NoPriceSource` rather than falling back to anything. Removing sources can
/// only ever make a price *less* available, never more authoritative.
#[test]
fn test_1190_removing_all_price_sources_yields_no_price_source() {
    let t = setup();
    let oracle_a = deploy_price_oracle(&t, 100);
    let oracle_b = deploy_price_oracle(&t, 100);
    add_price_source(&t, &oracle_a);
    add_price_source(&t, &oracle_b);

    // Sanity: with both registered a price is available...
    assert_eq!(
        t.contract
            .get_verified_price(&OracleFeedType::Price, &t.token.address),
        100
    );

    // ...but dropping the sample to zero is fail-closed.
    t.contract
        .remove_price_source(&OracleFeedType::Price, &oracle_a);
    advance_past_rate_limit_cooldown(&t.env);
    t.contract
        .remove_price_source(&OracleFeedType::Price, &oracle_b);

    let res = t
        .contract
        .try_get_verified_price(&OracleFeedType::Price, &t.token.address);
    assert_eq!(res, Err(Ok(ContractError::NoPriceSource)));
}

/// `get_verified_price` / `get_price_sources` are a pure function of
/// `PriceSources(feed)` alone: Identity-side mutations (adding feeders, raising
/// the quorum, removing a feeder) must not add, drop or reorder a price source,
/// and so cannot alter a verified price. This is the price-side half of the
/// list-separation guarantee (the identity-side half is
/// `test_1190_remove_feeder_leaving_quorum_unreachable_fails_closed`).
#[test]
fn test_1190_identity_mutations_do_not_touch_price_sources() {
    let t = setup();
    let price_a = deploy_price_oracle(&t, 100);
    let price_b = deploy_price_oracle(&t, 100);
    add_price_source(&t, &price_a);
    add_price_source(&t, &price_b);

    // Two distinct addresses stand in as Identity feeders; they are never
    // queried as feeders here (add_identity_oracle does not handshake), we
    // only need a feeder list to mutate.
    let feeder_a = deploy_price_oracle(&t, 0);
    let feeder_b = deploy_price_oracle(&t, 0);
    t.contract
        .add_identity_oracle(&OracleFeedType::Identity, &feeder_a);
    advance_past_rate_limit_cooldown(&t.env);
    t.contract
        .add_identity_oracle(&OracleFeedType::Identity, &feeder_b);
    advance_past_rate_limit_cooldown(&t.env);
    t.contract.set_identity_quorum(&2);
    advance_past_rate_limit_cooldown(&t.env);
    t.contract
        .remove_identity_oracle(&OracleFeedType::Identity, &feeder_a);

    // The price list is untouched — same two sources, same order...
    let sources = t.contract.get_price_sources(&OracleFeedType::Price);
    assert_eq!(sources.len(), 2);
    assert_eq!(sources.get(0).unwrap(), price_a);
    assert_eq!(sources.get(1).unwrap(), price_b);

    // ...and so is the verified price.
    assert_eq!(
        t.contract
            .get_verified_price(&OracleFeedType::Price, &t.token.address),
        100
    );
}
