//! Integration tests for the mock payer-verification oracle.
//!
//! Issue #94 — verifies the mock oracle's controllable behaviour that
//! oracle-dependent tests rely on.

#![cfg(test)]

extern crate std;

#[path = "mocks/mock_oracle.rs"]
mod mock_oracle;

use mock_oracle::{MockOracle, MockOracleClient};
use soroban_sdk::{
    testutils::{Address as _, MockAuth, MockAuthInvoke},
    Address, Env, IntoVal,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn setup() -> (Env, MockOracleClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let id = env.register_contract(None, MockOracle);
    let client = MockOracleClient::new(&env, &id);
    (env, client)
}

/// Set up an env **without** `mock_all_auths`, so the feeder gate introduced in
/// Issue #1046 is exercised with real authorization. Returns the contract id so
/// callers can build `MockAuthInvoke` entries for `update_verification`.
fn setup_real_auth() -> (Env, Address, MockOracleClient<'static>) {
    let env = Env::default();
    // Deliberately no env.mock_all_auths(): auth must be satisfied explicitly.
    let id = env.register_contract(None, MockOracle);
    let client = MockOracleClient::new(&env, &id);
    (env, id, client)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn test_unverified_by_default() {
    let (env, client) = setup();
    let payer = Address::generate(&env);
    let result = client.get_verification(&payer);
    assert!(!result.verified);
    assert_eq!(result.timestamp, 0);
}

#[test]
fn test_set_verified_true() {
    let (env, client) = setup();
    let payer = Address::generate(&env);
    client.set_verified(&payer, &true);
    assert!(client.get_verification(&payer).verified);
}

#[test]
fn test_revoke_verification() {
    let (env, client) = setup();
    let payer = Address::generate(&env);
    client.set_verified(&payer, &true);
    client.set_verified(&payer, &false);
    assert!(!client.get_verification(&payer).verified);
}

#[test]
fn test_set_timestamp() {
    let (env, client) = setup();
    let payer = Address::generate(&env);
    client.set_timestamp(&1_700_000_000u64);
    assert_eq!(client.get_verification(&payer).timestamp, 1_700_000_000u64);
}

#[test]
fn test_stale_data_detection() {
    // Demonstrates that calling code can detect staleness by comparing
    // the returned timestamp against the current time.
    let (env, client) = setup();
    let payer = Address::generate(&env);

    let stale_ts: u64 = 1_000_000_000;
    client.set_verified(&payer, &true);
    client.set_timestamp(&stale_ts);

    let result = client.get_verification(&payer);
    let now: u64 = stale_ts + 31 * 24 * 60 * 60; // 31 days later
    let staleness_threshold: u64 = 7 * 24 * 60 * 60;
    assert!(
        now.saturating_sub(result.timestamp) > staleness_threshold,
        "data older than 7 days should be treated as stale"
    );
}

#[test]
#[should_panic(expected = "forced panic")]
fn test_set_should_panic() {
    let (env, client) = setup();
    let payer = Address::generate(&env);
    client.set_should_panic();
    let _ = client.get_verification(&payer);
}

#[test]
fn test_panic_flag_is_one_shot() {
    let (env, client) = setup();
    let payer = Address::generate(&env);
    // No panic armed — two sequential calls must both succeed
    let r1 = client.get_verification(&payer);
    let r2 = client.get_verification(&payer);
    assert_eq!(r1.verified, r2.verified);
}

#[test]
fn test_independent_per_address() {
    let (env, client) = setup();
    let a = Address::generate(&env);
    let b = Address::generate(&env);
    client.set_verified(&a, &true);
    assert!(client.get_verification(&a).verified);
    assert!(!client.get_verification(&b).verified);
}

#[test]
fn test_update_verification_interface() {
    let (env, client) = setup();
    let payer = Address::generate(&env);
    let feeder = Address::generate(&env);
    client.set_feeder(&feeder);
    // update_verification is the oracle-operator (feeder) method
    // (OracleInterface). With mock_all_auths the configured feeder's signature
    // is satisfied, so the call succeeds.
    client.update_verification(&payer, &true);
    assert!(client.get_verification(&payer).verified);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Issue #1046 — authorize oracle feeder updates
// ═══════════════════════════════════════════════════════════════════════════════
//
// Before #1046 the mock's `update_verification` had no authorization check at
// all: any caller could flip any payer to verified, which — when the ILN is
// configured with `require_oracle_verification = true` — is exactly what unlocks
// funding. The interface always *documented* the requirement (see
// `oracle_interface.rs` and `oracle-integration.md`), but nothing enforced it.
//
// After #1046 the feeder (operator) entry point requires the configured
// feeder's signature and rejects the call outright when no feeder has been
// configured. The tests below pin success, the unconfigured case, the
// unauthorized-signer case (real auth), and feeder rotation.

#[test]
fn test_get_feeder_none_until_configured() {
    let (env, client) = setup();
    assert!(client.get_feeder().is_none());

    let feeder = Address::generate(&env);
    client.set_feeder(&feeder);
    assert_eq!(client.get_feeder(), Some(feeder.clone()));
}

#[test]
#[should_panic(expected = "feeder not configured")]
fn test_update_verification_requires_configured_feeder() {
    let (env, client) = setup();
    let payer = Address::generate(&env);
    // No feeder configured → the write path must reject, even though
    // mock_all_auths would satisfy any require_auth().
    client.update_verification(&payer, &true);
}

#[test]
fn test_update_verification_authorized_feeder_succeeds() {
    let (env, id, client) = setup_real_auth();
    let feeder = Address::generate(&env);
    let payer = Address::generate(&env);
    client.set_feeder(&feeder);

    env.mock_auths(&[MockAuth {
        address: &feeder,
        invoke: &MockAuthInvoke {
            contract: &id,
            fn_name: "update_verification",
            sub_invokes: &[],
            args: (&payer, &true).into_val(&env),
        },
    }]);

    client.update_verification(&payer, &true);
    assert!(client.get_verification(&payer).verified);
}

#[test]
fn test_update_verification_unauthorized_signer_rejected() {
    let (env, id, client) = setup_real_auth();
    let feeder = Address::generate(&env);
    let attacker = Address::generate(&env);
    let payer = Address::generate(&env);
    client.set_feeder(&feeder);

    // Only the attacker signs; the configured feeder does not.
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &id,
            fn_name: "update_verification",
            sub_invokes: &[],
            args: (&payer, &true).into_val(&env),
        },
    }]);

    let result = client.try_update_verification(&payer, &true);
    assert!(
        result.is_err(),
        "an unauthorized signer must not update verification"
    );
    // The rejected write must not have landed.
    assert!(!client.get_verification(&payer).verified);
}

#[test]
fn test_feeder_rotation_revokes_previous_feeder() {
    let (env, id, client) = setup_real_auth();
    let old_feeder = Address::generate(&env);
    let new_feeder = Address::generate(&env);
    let payer = Address::generate(&env);

    // Old feeder is authorized.
    client.set_feeder(&old_feeder);
    env.mock_auths(&[MockAuth {
        address: &old_feeder,
        invoke: &MockAuthInvoke {
            contract: &id,
            fn_name: "update_verification",
            sub_invokes: &[],
            args: (&payer, &true).into_val(&env),
        },
    }]);
    client.update_verification(&payer, &true);
    assert!(client.get_verification(&payer).verified);

    // Rotate to the new feeder.
    client.set_feeder(&new_feeder);

    // The previous feeder can no longer write...
    env.mock_auths(&[MockAuth {
        address: &old_feeder,
        invoke: &MockAuthInvoke {
            contract: &id,
            fn_name: "update_verification",
            sub_invokes: &[],
            args: (&payer, &false).into_val(&env),
        },
    }]);
    assert!(client.try_update_verification(&payer, &false).is_err());
    assert!(
        client.get_verification(&payer).verified,
        "the revoked feeder's write must not land"
    );

    // ...but the new feeder can.
    env.mock_auths(&[MockAuth {
        address: &new_feeder,
        invoke: &MockAuthInvoke {
            contract: &id,
            fn_name: "update_verification",
            sub_invokes: &[],
            args: (&payer, &false).into_val(&env),
        },
    }]);
    client.update_verification(&payer, &false);
    assert!(!client.get_verification(&payer).verified);
}
