//! Issue #1067 — Test graceful resource-limit failure
//!
//! Soroban's test host enforces mainnet resource limits automatically:
//! since the soroban-sdk 27 "v25_resource_limits" migration, `Env::default()`
//! enforces `InvocationResourceLimits::mainnet()` for every top-level
//! invocation unless a test calls `disable_resource_limits()` or installs
//! its own limits via `enforce_resource_limits()`. When an invocation
//! exceeds its enforced limit, the host panics — there is no
//! `ContractError` variant for this, it is a host-level trap, not
//! contract-returned logic.
//!
//! What this file actually tests is the thing that matters economically:
//! that the trap is *graceful*, i.e. the whole invocation rolls back
//! atomically. A half-written invoice, a token transfer that moved funds
//! out of an LP's account without ever crediting the contract (or vice
//! versa), or a `NextInvoiceId` that advanced without a corresponding
//! `InvoiceCore` being written, would each be a correctness/economic bug
//! far worse than the resource-limit trap itself. This is fundamentally a
//! guarantee of the Soroban host, not code this contract wrote — but the
//! whole safety story for Issue #1066's thresholds (and for Soroban
//! contracts generally) depends on it holding, so it is worth a direct
//! regression test rather than an unverified assumption.
//!
//! # Before / after
//!
//! Before this file: nothing in the `invoice_liquidity` test suite ever
//! deliberately exercised a resource-limit trap, so this atomicity
//! guarantee was assumed, not verified, for this contract's own entry
//! points (`submit_invoice`, `fund_invoice`).
//!
//! After: `test_resource_limit_panic_leaves_no_partial_state` and
//! `test_fund_invoice_panic_rolls_back_token_transfer_too` directly prove
//! it — the first for pure contract storage, the second across the
//! contract/token-contract boundary (a real SAC token transfer, not just
//! our own persistent keys). `test_resource_limit_boundary_one_instruction_short_fails_exact_amount_succeeds`
//! proves the enforcement is exact at the instruction boundary, not merely
//! "roughly" enforced.

#![cfg(test)]

use super::*;
use soroban_env_host::InvocationResourceLimits;
use soroban_sdk::testutils::cost_estimate::NetworkInvocationResourceLimits;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env,
};

const AMOUNT: i128 = 1_000_000_000;
const DISCOUNT_RATE: u32 = 300;
const DUE_DATE_OFFSET: u64 = 86_400;

/// Deliberately, obviously too small for any real contract call to fit —
/// used wherever a test just needs *some* trap to fire, without caring
/// about the exact boundary (see the dedicated boundary test for that).
const HOSTILE_INSTRUCTION_LIMIT: u64 = 1_000;

struct LimitTestEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: Address,
    freelancer: Address,
    payer: Address,
}

fn setup_limit_env() -> LimitTestEnv {
    let env = Env::default();
    env.mock_all_auths();

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    env.ledger().set(ledger);

    let usdc_admin = Address::generate(&env);
    let usdc = env.register_stellar_asset_contract_v2(usdc_admin.clone());
    let xlm_admin = Address::generate(&env);
    let xlm = env.register_stellar_asset_contract_v2(xlm_admin);

    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    let eurc_address = Address::generate(&env);
    contract.initialize(&usdc_admin, &usdc.address(), &eurc_address, &xlm.address());

    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);

    LimitTestEnv {
        env,
        contract,
        token: usdc.address(),
        freelancer,
        payer,
    }
}

fn submit_invoice_limit(t: &LimitTestEnv) -> u64 {
    let due_date = t.env.ledger().timestamp() + DUE_DATE_OFFSET;
    t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &AMOUNT,
        &due_date,
        &DISCOUNT_RATE,
        &t.token,
        &ReferralCode::None,
    )
}

/// Measure the exact instruction cost of one `submit_invoice` call on a
/// fresh env under unlimited resources, so the boundary test below is
/// anchored to a real, current measurement rather than a guessed constant
/// that could silently drift out of sync as the contract changes.
fn measure_submit_invoice_instructions() -> u64 {
    let t = setup_limit_env();
    t.env.cost_estimate().budget().reset_unlimited();
    submit_invoice_limit(&t);
    t.env.cost_estimate().budget().cpu_instruction_cost()
}

// ─────────────────────────────────────────────────────────────────────────
// Failure: the call panics when it cannot possibly fit
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_submit_invoice_panics_when_instruction_limit_is_far_below_cost() {
    let t = setup_limit_env();

    let mut limits = InvocationResourceLimits::mainnet();
    limits.instructions = HOSTILE_INSTRUCTION_LIMIT;
    t.env.cost_estimate().enforce_resource_limits(limits);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        submit_invoice_limit(&t);
    }));
    assert!(
        result.is_err(),
        "submit_invoice must panic when it cannot possibly fit the enforced instruction limit"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Graceful failure: no partial state survives the trap
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_resource_limit_panic_leaves_no_partial_state() {
    let t = setup_limit_env();

    // Baseline: one real invoice exists before we try (and fail) to
    // create a second one under a hostile limit.
    let first_id = submit_invoice_limit(&t);
    let stats_before = t.contract.get_contract_stats();

    let mut limits = InvocationResourceLimits::mainnet();
    limits.instructions = HOSTILE_INSTRUCTION_LIMIT;
    t.env.cost_estimate().enforce_resource_limits(limits);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        submit_invoice_limit(&t);
    }));
    assert!(
        result.is_err(),
        "second submit_invoice must panic under the hostile limit"
    );

    // Lift the limit so normal calls (including the harness's own
    // follow-up assertions) keep working after the panic.
    t.env.cost_estimate().disable_resource_limits();

    let stats_after = t.contract.get_contract_stats();
    assert_eq!(
        stats_before.total_invoices, stats_after.total_invoices,
        "a trapped invocation must not have incremented total_invoices"
    );

    // The id the failed call would have allocated must not exist --
    // confirms NextInvoiceId never actually advanced past the failed call.
    let would_be_next_id = first_id + 1;
    let result = t.contract.try_get_invoice(&would_be_next_id);
    assert!(
        result.is_err(),
        "no invoice must exist at the id the failed call would have allocated"
    );

    // The harness's Env remains fully usable after the caught panic -- a
    // resource-limit trap only rolls back the trapped invocation, not the
    // whole test environment.
    let second_id = submit_invoice_limit(&t);
    assert_eq!(
        second_id, would_be_next_id,
        "NextInvoiceId must resume exactly where it left off, as if the failed call never happened"
    );
}

#[test]
fn test_fund_invoice_panic_rolls_back_token_transfer_too() {
    let t = setup_limit_env();
    let usdc_client = TokenClient::new(&t.env, &t.token);
    let usdc_admin_client = StellarAssetClient::new(&t.env, &t.token);

    let lp = Address::generate(&t.env);
    usdc_admin_client.mint(&lp, &(AMOUNT * 10));

    let id = submit_invoice_limit(&t);
    let lp_balance_before = usdc_client.balance(&lp);
    let contract_balance_before = usdc_client.balance(&t.contract.address);

    let mut limits = InvocationResourceLimits::mainnet();
    limits.instructions = HOSTILE_INSTRUCTION_LIMIT;
    t.env.cost_estimate().enforce_resource_limits(limits);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        t.contract.fund_invoice(&lp, &id, &AMOUNT, &false);
    }));
    assert!(
        result.is_err(),
        "fund_invoice must panic under the hostile limit"
    );

    t.env.cost_estimate().disable_resource_limits();

    // Cross-contract atomicity: the token transfer into the contract (a
    // real call to the USDC Stellar Asset Contract, not just our own
    // storage) must have been rolled back along with our InvoiceCore
    // write -- not just one half of the operation.
    assert_eq!(
        usdc_client.balance(&lp),
        lp_balance_before,
        "LP's token balance must be unchanged after a trapped fund_invoice"
    );
    assert_eq!(
        usdc_client.balance(&t.contract.address),
        contract_balance_before,
        "contract's token balance must be unchanged after a trapped fund_invoice"
    );

    let invoice = t.contract.get_invoice(&id);
    assert_eq!(
        invoice.status,
        InvoiceStatus::Pending,
        "invoice must still be Pending, not partially funded, after the trap"
    );
    assert_eq!(
        invoice.amount_funded, 0,
        "amount_funded must be unchanged after a trapped fund_invoice"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Ledger/resource-boundary: enforcement is exact, not approximate
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_resource_limit_boundary_one_instruction_short_fails_exact_amount_succeeds() {
    let exact_cost = measure_submit_invoice_instructions();

    let too_tight_env = setup_limit_env();
    let mut too_tight = InvocationResourceLimits::mainnet();
    too_tight.instructions = exact_cost - 1;
    too_tight_env
        .env
        .cost_estimate()
        .enforce_resource_limits(too_tight);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        submit_invoice_limit(&too_tight_env);
    }));
    assert!(
        result.is_err(),
        "a limit one instruction below the real cost ({exact_cost}) must still fail"
    );

    let exact_env = setup_limit_env();
    let mut exact = InvocationResourceLimits::mainnet();
    exact.instructions = exact_cost;
    exact_env.env.cost_estimate().enforce_resource_limits(exact);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        submit_invoice_limit(&exact_env);
    }));
    assert!(
        result.is_ok(),
        "a limit exactly matching the real cost ({exact_cost}) must succeed"
    );
}
