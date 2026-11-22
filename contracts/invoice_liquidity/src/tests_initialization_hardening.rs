#![cfg(test)]

//! Issue #1162 — deploy-time initialization hardening.
//!
//! Deployment (`scripts/deploy-testnet.sh`) installs the WASM, but
//! `initialize` is a *separate* invocation. These tests pin the two properties
//! that make that separation safe:
//!
//! 1. `initialize` requires the intended administrator to authorize the call,
//!    so a first-come caller cannot name itself admin.
//! 2. `initialize` is replay-safe: any explicit or legacy initialization
//!    signal makes a repeat call fail deterministically with
//!    `AlreadyInitialized`, including on an instance that was set up before
//!    this change (no `Initialized` marker yet).
//!
//! Authoritative invariants:
//! * INV-INIT1 — `initialize` is callable only by (an authorizer for) `admin`.
//! * INV-INIT2 — `Initialized` is written before any other state (CEI).
//! * INV-INIT3 — a second/competing `initialize` never mutates stored state.
//! * INV-INIT4 — legacy deployments (`Admin`/`Config` present, no marker)
//!   remain closed to re-initialization after an upgrade.

use super::*;
use soroban_sdk::{
    testutils::{Address as _, MockAuth, MockAuthInvoke},
    Address, Env, IntoVal,
};

/// Registers a fresh `invoice_liquidity` instance on an env that does **not**
/// mock authorizations, so `require_auth()` is genuinely enforced.
fn setup_no_auth() -> (
    Env,
    Address,
    Address,
    InvoiceLiquidityContractClient<'static>,
) {
    let env = Env::default();
    let admin = Address::generate(&env);
    // Reuse one address for all three bootstrap tokens; the values are
    // irrelevant to the authorization/guard behavior under test.
    let token = Address::generate(&env);

    let contract_id = env.register(InvoiceLiquidityContract, ());
    let client = InvoiceLiquidityContractClient::new(&env, &contract_id);

    (env, admin, token, client)
}

// ── Authorization (INV-INIT1) ────────────────────────────────────────────────

#[test]
fn initialize_requires_admin_authorization() {
    let (_env, admin, token, client) = setup_no_auth();

    // No auth mocked for `admin` → the call must not succeed.
    let res = client.try_initialize(&admin, &token, &token, &token);
    assert!(
        res.is_err(),
        "initialize must require the intended admin's authorization"
    );
}

#[test]
fn initialize_succeeds_when_admin_authorizes() {
    let (env, admin, token, client) = setup_no_auth();

    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &client.address,
            fn_name: "initialize",
            args: (admin.clone(), token.clone(), token.clone(), token.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    let res = client.try_initialize(&admin, &token, &token, &token);
    assert!(
        res.is_ok(),
        "initialize should succeed when the admin authorizes: {res:?}"
    );

    // INV-INIT2/INV-INIT3: the marker and admin are recorded.
    env.as_contract(&client.address, || {
        assert!(
            env.storage().instance().has(&DataKey::Initialized),
            "initialize must persist the Initialized marker"
        );
        let stored: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        assert_eq!(stored, admin);
    });
}

// ── Replay safety / competing initializers (INV-INIT3) ───────────────────────

#[test]
fn repeated_initialize_fails_deterministically() {
    let (env, admin, token, client) = setup_no_auth();
    env.mock_all_auths();

    client.initialize(&admin, &token, &token, &token);

    let res = client.try_initialize(&admin, &token, &token, &token);
    assert_eq!(res, Err(Ok(ContractError::AlreadyInitialized)));
}

#[test]
fn competing_initializer_cannot_seize_admin() {
    let (env, admin, token, client) = setup_no_auth();
    env.mock_all_auths();

    client.initialize(&admin, &token, &token, &token);

    // A competing caller — even one that authorizes — cannot re-run initialize
    // nor displace the recorded admin.
    let attacker = Address::generate(&env);
    let res = client.try_initialize(&attacker, &token, &token, &token);
    assert_eq!(res, Err(Ok(ContractError::AlreadyInitialized)));

    env.as_contract(&client.address, || {
        let stored: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        assert_eq!(stored, admin, "admin must be unchanged by a rejected call");
    });
}

// ── Explicit marker vs. legacy state (INV-INIT2 / INV-INIT4) ─────────────────

#[test]
fn explicit_marker_alone_blocks_reinitialization() {
    let (env, admin, token, client) = setup_no_auth();
    env.mock_all_auths();

    // A partially-applied / already-started setup: only the marker exists.
    env.as_contract(&client.address, || {
        env.storage().instance().set(&DataKey::Initialized, &true);
    });

    let res = client.try_initialize(&admin, &token, &token, &token);
    assert_eq!(res, Err(Ok(ContractError::AlreadyInitialized)));
}

// ================================================================
// Issue #1166 — initialize write-sequence hardening
// ================================================================
//
// The write sequence must be *all or nothing*: a reverted `initialize` must
// leave no partial authority (marker/admin/config) and no partial token state
// (approvals/decimals/token list) behind. These tests pin both directions and
// exercise the read-only `get_initialization_state` snapshot that lets
// operators verify an instance in the field.
//
// * INV-INIT5 — a reverted `initialize` commits no authority or token state.
// * INV-INIT6 — a successful `initialize` commits authority *and* fully
//   registered bootstrap tokens together; `consistent` is then `true`.

/// Every key `initialize` would write, so a reverted call can be checked for
/// the absence of *all* of them at once.
fn assert_no_initialization_state(env: &Env, contract: &Address, token: &Address) {
    env.as_contract(contract, || {
        let instance = env.storage().instance();
        assert!(
            !instance.has(&DataKey::Initialized),
            "no Initialized marker after a reverted initialize"
        );
        assert!(!instance.has(&DataKey::Admin), "no Admin after a revert");
        assert!(!instance.has(&DataKey::Config), "no Config after a revert");
        assert!(
            !instance.has(&DataKey::NextInvoiceId),
            "no NextInvoiceId after a revert"
        );
        assert!(!instance.has(&DataKey::FeeRate), "no FeeRate after a revert");

        let persistent = env.storage().persistent();
        assert!(
            !persistent.has(&DataKey::TokenList),
            "no TokenList after a revert"
        );
        assert!(
            !persistent
                .get::<DataKey, bool>(&DataKey::ApprovedToken(token.clone()))
                .unwrap_or(false),
            "no token approval after a revert"
        );
        assert!(
            !persistent.has(&DataKey::TokenDecimals(token.clone())),
            "no token decimals after a revert"
        );
    });
}

#[test]
fn reverted_initialize_leaves_no_partial_authority_or_token_state() {
    let (env, admin, token, client) = setup_no_auth();

    // No authorizer for `admin` → the call fails before it can commit anything.
    let res = client.try_initialize(&admin, &token, &token, &token);
    assert!(res.is_err(), "unauthorized initialize must fail");

    // INV-INIT5: the reverted call left the instance completely untouched.
    assert_no_initialization_state(&env, &client.address, &token);

    let state = client.get_initialization_state();
    assert!(!state.initialized);
    assert!(!state.marker_set);
    assert!(!state.admin_set);
    assert!(!state.config_set);
    assert_eq!(state.token_list_len, 0);
    assert!(!state.consistent);
}

#[test]
fn successful_initialize_commits_complete_consistent_state() {
    let (env, admin, token, client) = setup_no_auth();
    env.mock_all_auths();

    client.initialize(&admin, &token, &token, &token);

    // INV-INIT6: authority and bootstrap-token state landed together.
    env.as_contract(&client.address, || {
        let instance = env.storage().instance();
        assert!(instance.has(&DataKey::Initialized));
        assert_eq!(
            instance.get::<DataKey, Address>(&DataKey::Admin).unwrap(),
            admin
        );
        assert_eq!(instance.get::<DataKey, u32>(&DataKey::FeeRate).unwrap(), 0);
        assert_eq!(
            instance.get::<DataKey, u32>(&DataKey::MaxDiscountRate).unwrap(),
            5000
        );

        let config: Config = instance.get(&DataKey::Config).unwrap();
        assert_eq!(config.usdc_sac_address, token);
        assert_eq!(config.eurc_sac_address, token);
        assert_eq!(config.xlm_sac_address, token);

        let persistent = env.storage().persistent();
        assert!(
            persistent
                .get::<DataKey, bool>(&DataKey::ApprovedToken(token.clone()))
                .unwrap_or(false),
            "bootstrap token must be approved"
        );
        assert!(
            persistent.has(&DataKey::TokenDecimals(token.clone())),
            "bootstrap token must have registered decimals"
        );
    });

    let state = client.get_initialization_state();
    assert!(state.initialized && state.marker_set && state.admin_set && state.config_set);
    assert_eq!(state.token_list_len, 3);
    assert!(state.consistent, "a completed initialize must be consistent");
}

#[test]
fn initialization_state_view_detects_partial_setup() {
    let (env, admin, token, client) = setup_no_auth();

    // A half-applied setup: authority marked, but no admin/config/tokens yet.
    env.as_contract(&client.address, || {
        env.storage().instance().set(&DataKey::Initialized, &true);
    });

    let state = client.get_initialization_state();
    assert!(state.initialized, "the marker is an initialization signal");
    assert!(state.marker_set);
    assert!(!state.admin_set);
    assert!(!state.config_set);
    assert_eq!(state.token_list_len, 0);
    assert!(
        !state.consistent,
        "a marker without authority/token state is not consistent"
    );

    // The partial state still blocks re-initialization, so the operator must
    // recover through the documented admin/governance flow rather than a rerun.
    env.mock_all_auths();
    let res = client.try_initialize(&admin, &token, &token, &token);
    assert_eq!(res, Err(Ok(ContractError::AlreadyInitialized)));
}

#[test]
fn competing_initialize_leaves_authority_and_token_state_intact() {
    let (env, admin, token, client) = setup_no_auth();
    env.mock_all_auths();

    client.initialize(&admin, &token, &token, &token);
    let before = client.get_initialization_state();

    let attacker = Address::generate(&env);
    let res = client.try_initialize(&attacker, &token, &token, &token);
    assert_eq!(res, Err(Ok(ContractError::AlreadyInitialized)));

    // Nothing the rejected call touched changed.
    let after = client.get_initialization_state();
    assert_eq!(before, after);
    env.as_contract(&client.address, || {
        assert_eq!(
            env.storage()
                .instance()
                .get::<DataKey, Address>(&DataKey::Admin)
                .unwrap(),
            admin
        );
        assert!(
            env.storage()
                .persistent()
                .get::<DataKey, bool>(&DataKey::ApprovedToken(token.clone()))
                .unwrap_or(false)
        );
    });
}
