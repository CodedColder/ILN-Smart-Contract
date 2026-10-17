#![cfg(test)]

//! Execution cost benchmarks for core contract instructions (Issue #76),
//! extended with absolute WASM/instruction thresholds (Issue #1066) and
//! near-limit invocation monitoring (Issue #1068, see
//! `assert_within_resource_budget`'s doc comment below for why that
//! monitoring has to live here rather than on-chain).
//!
//! # Before / after
//!
//! **Before:** this file measured CPU instructions and memory via
//! `env.cost_estimate().budget()` and printed `BENCHMARK` lines for
//! `scripts/check_benchmark_regression.sh` to compare against the last
//! recorded baseline — but only as a *relative*, warning-only check (>10%
//! drift from baseline => `::warning::`, and the script always exits 0; see
//! `docs/benchmarks.md`). There was no absolute ceiling anywhere: a
//! contract that crept past the network's real resource limits one small
//! change at a time would never fail CI, only print warnings nobody is
//! required to act on. Separately, this file wasn't registered in `lib.rs`
//! (`mod tests_benchmarks;` was missing) and its calls had drifted out of
//! sync with `submit_invoice`'s real signature (e.g.
//! `submit_invoice(&ReferralCode::None)` — a single argument against a
//! 7-argument function) — it compiled as dead code only because it was
//! never built at all.
//!
//! **After:** the module is registered (see `lib.rs`) and its calls fixed
//! to match the current contract API. Each benchmark now also asserts the
//! measured CPU instructions and memory bytes are within
//! `MAX_CPU_INSTRUCTIONS` / `MAX_MEMORY_BYTES` — absolute ceilings mirroring
//! Soroban's commonly documented per-transaction resource limits, not the
//! contract's own historical baseline. A regression that blows through
//! these fails `cargo test -p invoice_liquidity` (`make test-invoice`)
//! directly, independent of whether anyone separately runs the (still
//! warning-only, unchanged) `check_benchmark_regression.sh` relative-drift
//! check. See `docs/wasm-size.md` for the WASM-*binary*-size counterpart,
//! which can't be measured from inside a native `cargo test` run and is
//! instead enforced by `scripts/check_wasm_size.sh` against the built
//! `.wasm` artifact.
//!
//! These thresholds are deliberately far above the contract's actual
//! measured costs (~100x the highest baseline CPU figure, ~1000x the
//! highest baseline memory figure — see `docs/benchmarks.md`'s baseline
//! table) — they are a circuit breaker for catastrophic regressions (e.g.
//! an accidentally introduced O(n²) path), not a tight budget.
//!
//! Note that `soroban-sdk` 27's `Env::default()` *already* enforces
//! `InvocationResourceLimits::mainnet()` automatically for every top-level
//! invocation in these tests (see
//! `contracts/invoice_liquidity/src/tests_resource_limit_failure.rs`,
//! Issue #1067) — as of this SDK version (snapshot dated 2026-07-10) that's
//! 400,000,000 instructions and 41,943,040 bytes (40 MiB) of memory, pulled
//! from `soroban_sdk::testutils::cost_estimate::NetworkInvocationResourceLimits::mainnet()`.
//! `MAX_CPU_INSTRUCTIONS` below is deliberately *stricter* than that
//! default (100,000,000, a quarter of it) so this file's assertions fire
//! well before the SDK's own auto-enforced trap would; `MAX_MEMORY_BYTES`
//! matches the SDK's figure exactly. Either way, the SDK's bundled
//! snapshot is not pulled dynamically from the live network — if it's ever
//! stale, upgrading `soroban-sdk` (not editing these constants by hand) is
//! the correct fix; see that crate's `_migrating::v25_resource_limits`
//! module doc.

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, Env,
};

const BENCH_INVOICE_AMOUNT: i128 = 1_000_000_000;
const BENCH_DISCOUNT_RATE: u32 = 300;
const BENCH_DUE_DATE_OFFSET: u64 = 86_400;

/// Absolute CPU instruction ceiling for a single contract invocation.
/// Intentionally stricter than `InvocationResourceLimits::mainnet().instructions`
/// (400,000,000 per soroban-sdk 27.0.6's bundled snapshot) — a quarter of
/// it — so this file's own assertions fire before the SDK's own
/// auto-enforced default trap would (see module doc comment).
const MAX_CPU_INSTRUCTIONS: u64 = 100_000_000;

/// Absolute memory ceiling, in bytes, for a single contract invocation.
/// Matches `InvocationResourceLimits::mainnet().mem_bytes` exactly
/// (41,943,040 = 40 MiB, per soroban-sdk 27.0.6's bundled snapshot).
const MAX_MEMORY_BYTES: u64 = 40 * 1024 * 1024;

struct BaseBenchEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    token: Address,
    freelancer: Address,
    payer: Address,
    lp: Address,
}

fn setup_benchmark_env() -> BaseBenchEnv {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

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
    let lp = Address::generate(&env);

    let usdc_client = StellarAssetClient::new(&env, &usdc.address());
    usdc_client.mint(&lp, &1_000_000_000_000);
    usdc_client.mint(&payer, &1_000_000_000_000);

    BaseBenchEnv {
        env,
        contract,
        token: usdc.address(),
        freelancer,
        payer,
        lp,
    }
}

fn submit_invoice_bench(bench: &BaseBenchEnv) -> u64 {
    let due_date = bench.env.ledger().timestamp() + BENCH_DUE_DATE_OFFSET;
    bench.contract.submit_invoice(
        &bench.freelancer,
        &bench.payer,
        &BENCH_INVOICE_AMOUNT,
        &due_date,
        &BENCH_DISCOUNT_RATE,
        &bench.token,
        &ReferralCode::None,
    )
}

fn emit_benchmark(name: &str, cpu: u64, mem: u64) {
    std::println!("BENCHMARK {name} cpu={cpu} mem={mem}");
}

fn measure<F: FnOnce()>(env: &Env, name: &str, action: F) -> (u64, u64) {
    env.cost_estimate().budget().reset_unlimited();
    action();
    let cpu = env.cost_estimate().budget().cpu_instruction_cost();
    let mem = env.cost_estimate().budget().memory_bytes_cost();
    emit_benchmark(name, cpu, mem);
    (cpu, mem)
}

/// Issue #1068: fraction of an absolute ceiling at which a measurement
/// counts as "near the limit" — still passing, but close enough to warrant
/// attention before it actually regresses past the hard ceiling. Mirrors
/// the early-warning idea behind Issue #1015's fund-queue TTL margins:
/// the earlier drift is visible, the more of #1066's ~4x (CPU) / ~1000x
/// (memory) headroom is left to react before a measurement actually fails.
const NEAR_LIMIT_WARNING_RATIO: f64 = 0.70;

/// `max * NEAR_LIMIT_WARNING_RATIO`, as an integer. Kept as its own
/// function (rather than inlined) so the boundary tests below can assert
/// on it directly instead of recomputing the cast/rounding by hand.
fn near_limit_warning_threshold(max: u64) -> u64 {
    ((max as f64) * NEAR_LIMIT_WARNING_RATIO) as u64
}

/// Assert `(cpu, mem)` is within the absolute resource ceilings, and warn
/// (without failing) when either is already past the near-limit threshold.
///
/// Pure comparison/reporting logic, deliberately separated from the actual
/// measurement so it can be unit-tested at its exact boundaries (see the
/// `test_resource_budget_boundary_*` / `test_near_limit_warning_*` tests
/// below) independent of how many instructions any given contract call
/// happens to cost on a given toolchain/day.
///
/// The warning is intentionally *not* a `ContractError` or an on-chain
/// event: Soroban gives deployed contract code no runtime-queryable way to
/// introspect its own CPU/memory usage so far (`cost_estimate()`/`budget()`
/// are `testutils`-only, host-side instrumentation — there is no mainnet
/// equivalent a live contract could call to check "how close am I to the
/// limit?"). This kind of monitoring can only happen where the resources
/// actually *are* measurable: here, in the test/CI harness, immediately
/// after each invocation.
fn assert_within_resource_budget(name: &str, cpu: u64, mem: u64, max_cpu: u64, max_mem: u64) {
    assert!(
        cpu <= max_cpu,
        "{name} exceeded the absolute CPU instruction budget: {cpu} > {max_cpu}"
    );
    assert!(
        mem <= max_mem,
        "{name} exceeded the absolute memory budget: {mem} > {max_mem}"
    );

    let cpu_warn = near_limit_warning_threshold(max_cpu);
    let mem_warn = near_limit_warning_threshold(max_mem);
    if cpu > cpu_warn {
        std::println!(
            "NEAR_LIMIT_WARNING {name} resource=cpu measured={cpu} warn_threshold={cpu_warn} max={max_cpu}"
        );
    }
    if mem > mem_warn {
        std::println!(
            "NEAR_LIMIT_WARNING {name} resource=mem measured={mem} warn_threshold={mem_warn} max={max_mem}"
        );
    }
}

#[test]
fn benchmark_submit_invoice() {
    let bench = setup_benchmark_env();

    let (cpu, mem) = measure(&bench.env, "submit_invoice", || {
        submit_invoice_bench(&bench);
    });
    assert_within_resource_budget(
        "submit_invoice",
        cpu,
        mem,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

#[test]
fn benchmark_fund_invoice() {
    let bench = setup_benchmark_env();
    let id = submit_invoice_bench(&bench);

    let (cpu, mem) = measure(&bench.env, "fund_invoice", || {
        bench
            .contract
            .fund_invoice(&bench.lp, &id, &BENCH_INVOICE_AMOUNT, &false);
    });
    assert_within_resource_budget(
        "fund_invoice",
        cpu,
        mem,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

#[test]
fn benchmark_mark_paid() {
    let bench = setup_benchmark_env();
    let id = submit_invoice_bench(&bench);
    bench
        .contract
        .fund_invoice(&bench.lp, &id, &BENCH_INVOICE_AMOUNT, &false);

    let (cpu, mem) = measure(&bench.env, "mark_paid", || {
        bench.contract.mark_paid(&id, &BENCH_INVOICE_AMOUNT);
    });
    assert_within_resource_budget(
        "mark_paid",
        cpu,
        mem,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

#[test]
fn benchmark_all_functions_summary() {
    let mut results: std::vec::Vec<(&str, (u64, u64))> = std::vec::Vec::new();

    let bench = setup_benchmark_env();

    results.push((
        "submit_invoice",
        measure(&bench.env, "submit_invoice", || {
            submit_invoice_bench(&bench);
        }),
    ));

    let id = submit_invoice_bench(&bench);
    results.push((
        "fund_invoice",
        measure(&bench.env, "fund_invoice", || {
            bench
                .contract
                .fund_invoice(&bench.lp, &id, &BENCH_INVOICE_AMOUNT, &false);
        }),
    ));
    results.push((
        "mark_paid",
        measure(&bench.env, "mark_paid", || {
            bench.contract.mark_paid(&id, &BENCH_INVOICE_AMOUNT);
        }),
    ));

    std::println!("\n| Function       | CPU Instructions | Memory (bytes) |");
    std::println!("| -------------- | ---------------- | -------------- |");
    for (name, (cpu, mem)) in &results {
        std::println!("| {name:<14} | {cpu:>16} | {mem:>14} |");
        assert_within_resource_budget(name, *cpu, *mem, MAX_CPU_INSTRUCTIONS, MAX_MEMORY_BYTES);
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Boundary coverage for the threshold-checking logic itself (Issue #1066).
// Synthetic values, not measured costs — these exist to prove the
// comparison itself is correct (inclusive upper bound) independent of
// whatever any given contract call happens to measure at.
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_resource_budget_boundary_passes_at_exact_limit() {
    assert_within_resource_budget(
        "synthetic",
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

#[test]
#[should_panic(expected = "exceeded the absolute CPU instruction budget")]
fn test_resource_budget_boundary_fails_one_instruction_over() {
    assert_within_resource_budget(
        "synthetic",
        MAX_CPU_INSTRUCTIONS + 1,
        0,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

#[test]
#[should_panic(expected = "exceeded the absolute memory budget")]
fn test_resource_budget_boundary_fails_one_byte_over() {
    assert_within_resource_budget(
        "synthetic",
        0,
        MAX_MEMORY_BYTES + 1,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Near-limit invocation monitoring (Issue #1068). Synthetic values, same
// rationale as the boundary tests above: prove the warning-threshold math
// and its on/off boundary directly, independent of any measured cost.
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_near_limit_warning_threshold_is_seventy_percent_of_max() {
    assert_eq!(near_limit_warning_threshold(100), 70);
    assert_eq!(near_limit_warning_threshold(MAX_CPU_INSTRUCTIONS), 70_000_000);
    assert_eq!(near_limit_warning_threshold(MAX_MEMORY_BYTES), 29_360_128);
}

#[test]
fn test_at_or_below_near_limit_threshold_does_not_panic() {
    // Success case: sitting exactly at the warning threshold (or anywhere
    // below the hard ceiling) must never fail the test -- near-limit
    // monitoring is a softer signal than #1066's hard ceiling, not a
    // second ceiling with the same enforcement.
    let cpu_warn = near_limit_warning_threshold(MAX_CPU_INSTRUCTIONS);
    let mem_warn = near_limit_warning_threshold(MAX_MEMORY_BYTES);
    assert_within_resource_budget(
        "synthetic",
        cpu_warn,
        mem_warn,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
    // One past the warning threshold, but still comfortably under the hard
    // ceiling, must also still pass -- it only adds a printed warning.
    assert_within_resource_budget(
        "synthetic",
        cpu_warn + 1,
        mem_warn + 1,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

#[test]
fn test_near_limit_warning_does_not_fire_for_the_current_measured_baselines() {
    // Regression guard: the recorded baselines (docs/benchmarks.md) sit
    // around 1% of MAX_CPU_INSTRUCTIONS / well under 1% of
    // MAX_MEMORY_BYTES, far below the 70% warning band. This pins that
    // down so a future change to the warning ratio can't silently start
    // warning on every single benchmark run without anyone noticing.
    let baseline_cpu = 1_041_920u64; // fund_invoice, the highest recorded baseline
    let baseline_mem = 38_190u64;
    assert!(baseline_cpu < near_limit_warning_threshold(MAX_CPU_INSTRUCTIONS));
    assert!(baseline_mem < near_limit_warning_threshold(MAX_MEMORY_BYTES));
}

// ================================================================
// Tests for get_contract_stats with multiple tokens (Issue #485)
// ================================================================

use soroban_sdk::{contract, contractimpl, token::Client as TokenClient};

#[contract]
struct MockPriceOracle;

#[contractimpl]
impl MockPriceOracle {
    pub fn get_price(_env: soroban_sdk::Env, _token: Address) -> i128 {
        20_000
    }
}

struct MultiTokenBenchEnv {
    env: Env,
    contract: InvoiceLiquidityContractClient<'static>,
    usdc: Address,
    usdc_client: TokenClient<'static>,
    xlm: Address,
    eurc: Address,
    freelancer: Address,
    payer: Address,
    lp: Address,
    admin: Address,
}

fn setup_multi_token() -> MultiTokenBenchEnv {
    let env = Env::default();
    env.mock_all_auths();

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    env.ledger().set(ledger);

    let usdc_admin = Address::generate(&env);
    let usdc = env.register_stellar_asset_contract_v2(usdc_admin.clone());
    let xlm_admin = Address::generate(&env);
    let xlm = env.register_stellar_asset_contract_v2(xlm_admin);
    let eurc_admin = Address::generate(&env);
    let eurc = env.register_stellar_asset_contract_v2(eurc_admin);

    let admin = usdc_admin.clone();
    let contract_id = env.register_contract(None, InvoiceLiquidityContract);
    let contract = InvoiceLiquidityContractClient::new(&env, &contract_id);
    contract.initialize(&admin, &usdc.address(), &eurc.address(), &xlm.address());

    let freelancer = Address::generate(&env);
    let payer = Address::generate(&env);
    let lp = Address::generate(&env);

    let usdc_client = StellarAssetClient::new(&env, &usdc.address());
    let usdc_token = TokenClient::new(&env, &usdc.address());
    let xlm_client = StellarAssetClient::new(&env, &xlm.address());
    let eurc_client = StellarAssetClient::new(&env, &eurc.address());

    usdc_client.mint(&lp, &10_000_000_000_000);
    usdc_client.mint(&payer, &10_000_000_000_000);
    xlm_client.mint(&lp, &10_000_000_000_000);
    xlm_client.mint(&payer, &10_000_000_000_000);
    eurc_client.mint(&lp, &10_000_000_000_000);
    eurc_client.mint(&payer, &10_000_000_000_000);

    MultiTokenBenchEnv {
        env,
        contract,
        usdc: usdc.address(),
        usdc_client,
        xlm: xlm.address(),
        eurc: eurc.address(),
        freelancer,
        payer,
        lp,
        admin,
    }
}

fn submit_and_fund(
    t: &MultiTokenBenchEnv,
    token: &Address,
    amount: i128,
) -> u64 {
    let due_date = t.env.ledger().timestamp() + 86_400 * 30;
    let id = t.contract.submit_invoice(
        &t.freelancer,
        &t.payer,
        &amount,
        &due_date,
        &300u32,
        token,
        &ReferralCode::None,
    );
    t.contract.fund_invoice(&t.lp, &id, &amount, &false);
    t.contract.mark_paid(&id, &amount);
    id
}

#[test]
fn test_stats_tracks_usdc_volume() {
    let t = setup_multi_token();
    submit_and_fund(&t, &t.usdc, 500_000_000);

    let stats = t.contract.get_contract_stats();
    assert_eq!(stats.total_volume_usdc, 500_000_000);
    assert_eq!(stats.total_invoices, 1);
    assert_eq!(stats.total_funded, 1);
    assert_eq!(stats.total_paid, 1);
}

#[test]
fn test_stats_tracks_xlm_volume() {
    let t = setup_multi_token();
    submit_and_fund(&t, &t.xlm, 2_000_000_000);

    let stats = t.contract.get_contract_stats();
    assert_eq!(stats.total_volume_xlm, 2_000_000_000);
    assert_eq!(stats.total_volume_usdc, 0);
}

#[test]
fn test_stats_tracks_eurc_volume() {
    let t = setup_multi_token();
    submit_and_fund(&t, &t.eurc, 750_000_000);

    let stats = t.contract.get_contract_stats();
    assert_eq!(stats.total_volume_eurc, 750_000_000);
    assert_eq!(stats.total_volume_usdc, 0);
    assert_eq!(stats.total_volume_xlm, 0);
}

#[test]
fn test_stats_multi_token_aggregation() {
    let t = setup_multi_token();
    submit_and_fund(&t, &t.usdc, 1_000_000_000);
    submit_and_fund(&t, &t.xlm, 2_000_000_000);
    submit_and_fund(&t, &t.eurc, 3_000_000_000);

    let stats = t.contract.get_contract_stats();
    assert_eq!(stats.total_invoices, 3);
    assert_eq!(stats.total_funded, 3);
    assert_eq!(stats.total_paid, 3);
    assert_eq!(stats.total_volume_usdc, 1_000_000_000);
    assert_eq!(stats.total_volume_xlm, 2_000_000_000);
    assert_eq!(stats.total_volume_eurc, 3_000_000_000);
}

#[test]
fn test_stats_usd_normalized_with_price_oracle() {
    let t = setup_multi_token();
    submit_and_fund(&t, &t.usdc, 1_000_000_000);

    // Install a mock price oracle that returns a fixed price.
    let oracle_id = t.env.register_contract(None, MockPriceOracle);
    t.env.as_contract(&t.contract.address, || {
        let mut config = crate::storage::get_config(&t.env).unwrap();
        config.price_oracle = Some(oracle_id.clone());
        crate::storage::set_config(&t.env, &config);
    });

    let stats = t.contract.get_contract_stats();
    // MockPriceOracle returns 20_000; normalized = 1_000_000_000 * 20_000 / 10_000
    assert_eq!(
        stats.total_volume_usd_normalized,
        1_000_000_000 * 20_000 / 10_000
    );
}

#[test]
fn test_stats_without_price_oracle_returns_zero_normalized() {
    let t = setup_multi_token();
    submit_and_fund(&t, &t.usdc, 1_000_000_000);

    // No oracle configured — should return 0 for normalized volume.
    let stats = t.contract.get_contract_stats();
    assert_eq!(stats.total_volume_usd_normalized, 0);
}
