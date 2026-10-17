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
    contract, contractimpl, contracttype,
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, Env,
};
use crate::access::ADMIN_ACTION_LOG_CAPACITY;
use crate::constants::{
    DEFAULT_RATE_LIMIT_LEDGERS, FUND_QUEUE_TTL_EXTEND_TO, FUND_QUEUE_TTL_THRESHOLD,
    MAX_FUND_QUEUE_LEN, MAX_PRICE_SOURCES_PER_FEED, TOP_PAYERS_CAPACITY,
};
use crate::oracle_registry::OracleFeedType;

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

// ================================================================
// Issue #1062: TTL cost at maximum queue size
// ================================================================
//
// Issue #1091/#1095 bounded a single invoice's fund queue to
// `MAX_FUND_QUEUE_LEN` (50) entries, admitting a new LP only if it strictly
// outscores the current tail (which it then displaces). That bound is what
// keeps the queue's serialized entry — and therefore the per-write
// `extend_ttl` host call that keeps `FundQueue`, `FundQueueOpenedAt`, and
// `QueueResolution` alive (Issue #1011) — within Soroban's per-transaction
// resource limits.
//
// **Before:** nothing measured that worst case. The benchmarks in this file
// only covered single-invoice flows (`submit_invoice` / `fund_invoice` /
// `mark_paid`), and the queue bounds themselves had no cost coverage at all,
// so a change that enlarged the queue entry (a new field on `LpFundRequest`,
// a longer address encoding, …) could silently push the maximum-size write
// past the network limit with nothing in CI to catch it.
//
// **After:** this section measures the maximum queue size end to end:
//   * `join_fund_queue` admission into a *full* queue (the displacing path —
//     scan, pop, insert, full re-serialize, then the TTL extension over the
//     largest entry the contract can hold);
//   * `resolve_fund_queue` over a full queue;
//   * the isolated `extend_ttl` host call on the queue key at 1 vs 10 vs 50
//     entries, isolating the entry-size-dependent TTL cost the issue asked
//     to measure.
//
// Each measurement is asserted against the same absolute ceilings the rest of
// this file uses (`assert_within_resource_budget`), so a maximum-queue-size
// regression fails `cargo test -p invoice_liquidity` directly.

/// Fill a freshly-submitted invoice's fund queue to exactly `len` entries with
/// default-score LPs (score 50 each). Every entry shares the same score, so
/// insertion appends and the queue ends up in join order. `len` must be
/// `<= MAX_FUND_QUEUE_LEN`.
fn setup_queue_of_len(len: u32) -> (BaseBenchEnv, u64) {
    let bench = setup_benchmark_env();
    let invoice_id = submit_invoice_bench(&bench);
    for _ in 0..len {
        let lp = Address::generate(&bench.env);
        bench.contract.join_fund_queue(&lp, &invoice_id);
    }
    (bench, invoice_id)
}

fn advance_ledgers(env: &Env, delta: u32) {
    let mut ledger = env.ledger().get();
    ledger.sequence_number += delta;
    env.ledger().set(ledger);
}

#[test]
fn benchmark_join_fund_queue_at_max_queue_size() {
    let (bench, invoice_id) = setup_queue_of_len(MAX_FUND_QUEUE_LEN);

    // A strictly higher-scored LP forces the worst-case admission path: the
    // queue is already full, so this displaces the tail and rewrites the whole
    // 50-entry vector before extending its TTL.
    let displacing_lp = Address::generate(&bench.env);
    bench.env.as_contract(&bench.contract.address, || {
        set_lp_score(&bench.env, &displacing_lp, 100);
    });

    let (cpu, mem) = measure(&bench.env, "join_fund_queue_at_max_queue_size", || {
        bench.contract.join_fund_queue(&displacing_lp, &invoice_id);
    });
    assert_within_resource_budget(
        "join_fund_queue_at_max_queue_size",
        cpu,
        mem,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

#[test]
fn benchmark_resolve_fund_queue_at_max_queue_size() {
    let (bench, invoice_id) = setup_queue_of_len(MAX_FUND_QUEUE_LEN);
    advance_ledgers(&bench.env, QUEUE_DELAY_LEDGERS);

    let (cpu, mem) = measure(&bench.env, "resolve_fund_queue_at_max_queue_size", || {
        bench.contract.resolve_fund_queue(&invoice_id);
    });
    assert_within_resource_budget(
        "resolve_fund_queue_at_max_queue_size",
        cpu,
        mem,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

/// Age the ledger far enough that `FundQueue(id)`'s remaining TTL has dropped
/// below `FUND_QUEUE_TTL_THRESHOLD`, so the `extend_ttl` measured next does
/// real work. `extend_ttl` short-circuits to a no-op while the entry still has
/// `>= FUND_QUEUE_TTL_THRESHOLD` ledgers left, which would understate the cost
/// this measurement exists to capture.
fn age_fund_queue_ttl_below_threshold(env: &Env) {
    let drop_below_threshold = FUND_QUEUE_TTL_EXTEND_TO - FUND_QUEUE_TTL_THRESHOLD + 1_000;
    advance_ledgers(env, drop_below_threshold);
}

/// Measure the isolated `extend_ttl` host call on the `FundQueue` key of a
/// queue holding `len` entries. Emits a `BENCHMARK` line (via `measure`) and
/// returns the `(cpu, mem)` result.
fn measure_fund_queue_ttl_extend(len: u32, name: &str) -> (u64, u64) {
    let (bench, invoice_id) = setup_queue_of_len(len);
    age_fund_queue_ttl_below_threshold(&bench.env);

    measure(&bench.env, name, || {
        bench.env.as_contract(&bench.contract.address, || {
            bench.env.storage().persistent().extend_ttl(
                &StorageKey::FundQueue(invoice_id),
                FUND_QUEUE_TTL_THRESHOLD,
                FUND_QUEUE_TTL_EXTEND_TO,
            );
        });
    })
}

#[test]
fn benchmark_extend_ttl_fund_queue_at_max_queue_size() {
    let (cpu, mem) = measure_fund_queue_ttl_extend(
        MAX_FUND_QUEUE_LEN,
        "extend_ttl_fund_queue_at_max_queue_size",
    );
    assert_within_resource_budget(
        "extend_ttl_fund_queue_at_max_queue_size",
        cpu,
        mem,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

#[test]
fn benchmark_extend_ttl_fund_queue_at_one_entry() {
    let (cpu, mem) = measure_fund_queue_ttl_extend(1, "extend_ttl_fund_queue_at_one_entry");
    assert_within_resource_budget(
        "extend_ttl_fund_queue_at_one_entry",
        cpu,
        mem,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

/// Record the isolated TTL-extension cost across queue sizes side by side.
/// This is the before/after the issue asks for: before, no such measurement
/// existed at all; after, the maximum-size cost is printed next to the
/// (minimum) 1-entry and midpoint 10-entry costs, so the entry-size-dependent
/// part of the TTL cost is directly visible in the benchmark output.
#[test]
fn benchmark_ttl_cost_across_queue_sizes() {
    let mut results: std::vec::Vec<(u32, (u64, u64))> = std::vec::Vec::new();
    for len in [1u32, 10, MAX_FUND_QUEUE_LEN] {
        let name = std::format!("extend_ttl_fund_queue_len_{len}");
        let measured = measure_fund_queue_ttl_extend(len, &name);
        results.push((len, measured));
    }

    std::println!("\n| Queue length | CPU Instructions | Memory (bytes) |");
    std::println!("| ------------ | ---------------- | -------------- |");
    for (len, (cpu, mem)) in &results {
        std::println!("| {len:>12} | {cpu:>16} | {mem:>14} |");
        assert_within_resource_budget(
            "extend_ttl_fund_queue_across_sizes",
            *cpu,
            *mem,
            MAX_CPU_INSTRUCTIONS,
            MAX_MEMORY_BYTES,
        );
    }
}

// ================================================================
// Issue #1063: stress concurrent invoice settlements
// ================================================================
//
// The settlement path (`mark_paid`) is the contract's highest-value entry
// point, and it touches several pieces of *shared* state on every call: the
// reentrancy lock (`StorageKey::ReentrancyLock`), the aggregate counter
// (`StorageKey::TotalPaid`), and `ContractStats` (`total_paid`,
// `total_volume_*`). Soroban executes one invocation at a time, so
// "concurrent" settlement — two payers settling independent invoices in the
// same ledger — is really a *same-ledger batch* of `mark_paid` calls.
//
// **Before:** this file only ever measured `mark_paid` in isolation
// (`benchmark_mark_paid`, `benchmark_all_functions_summary`), a single
// settlement against a freshly-submitted invoice. Nothing measured a
// settlement that runs *after* a burst of other settlements in the same
// ledger has already mutated the shared counters, so a per-settlement cost
// that grew with the size of the surrounding batch (e.g. re-serializing a
// shared structure that accumulates, or an unbounded counter read) would not
// have been visible here.
//
// **After:** the benchmarks below measure the *per-invocation* cost of a
// settlement executed inside a same-ledger batch of independent invoices —
// which is the cost mainnet's per-invocation resource limit actually applies
// to. `benchmark_settle_batch_same_ledger` measures the last settlement in a
// `SETTLE_BENCH_BATCH`-invoice burst, and
// `benchmark_settle_cost_across_batch_sizes` records that same
// per-invocation cost side by side at batch sizes 1, 10, and
// `SETTLE_BENCH_BATCH`, so a cost that is *not* flat in the surrounding
// batch size becomes directly visible. Each measurement is asserted against
// the same absolute ceilings the rest of this file uses
// (`assert_within_resource_budget`), so a settlement-cost regression fails
// `cargo test -p invoice_liquidity` directly. The behavioural counterpart
// (per-invoice isolation, rejection isolation, authorization, and the
// `paid_on_time` boundary) lives in `tests_storage.rs`.

/// Number of independent invoices settled together in a same-ledger batch.
/// Large enough that any per-settlement cost accumulating across the batch
/// would surface, small enough to stay a fast unit test.
const SETTLE_BENCH_BATCH: u32 = 25;

/// Submit and fund `len` independent invoices, each owned by a fresh payer
/// minted exactly enough to settle once. Returns the invoice ids in submission
/// order. The shared `lp` funds every invoice, matching how a single liquidity
/// provider backstops many independent payers.
fn setup_settle_batch(bench: &BaseBenchEnv, len: u32) -> std::vec::Vec<u64> {
    let token_admin = StellarAssetClient::new(&bench.env, &bench.token);
    let mut ids = std::vec::Vec::new();
    for _ in 0..len {
        let payer = Address::generate(&bench.env);
        token_admin.mint(&payer, &BENCH_INVOICE_AMOUNT);

        let due_date = bench.env.ledger().timestamp() + BENCH_DUE_DATE_OFFSET;
        let invoice_id = bench.contract.submit_invoice(
            &bench.freelancer,
            &payer,
            &BENCH_INVOICE_AMOUNT,
            &due_date,
            &BENCH_DISCOUNT_RATE,
            &bench.token,
            &ReferralCode::None,
        );
        bench
            .contract
            .fund_invoice(&bench.lp, &invoice_id, &BENCH_INVOICE_AMOUNT, &false);
        ids.push(invoice_id);
    }
    ids
}

/// Settle every invoice in `ids` except the last one, then measure the cost of
/// settling that last invoice. Everything runs at one ledger
/// sequence/timestamp, so the measured settlement executes after the rest of
/// the batch has mutated the shared counters — the "concurrent" case.
fn measure_last_settlement_in_batch(bench: &BaseBenchEnv, ids: &[u64], name: &str) -> (u64, u64) {
    let (last, rest) = ids.split_last().expect("batch must be non-empty");
    for id in rest {
        bench.contract.mark_paid(id, &BENCH_INVOICE_AMOUNT);
    }

    measure(&bench.env, name, || {
        bench.contract.mark_paid(last, &BENCH_INVOICE_AMOUNT);
    })
}

#[test]
fn benchmark_settle_batch_same_ledger() {
    let bench = setup_benchmark_env();
    let ids = setup_settle_batch(&bench, SETTLE_BENCH_BATCH);

    let (cpu, mem) = measure_last_settlement_in_batch(&bench, &ids, "settle_batch_same_ledger");
    assert_within_resource_budget(
        "settle_batch_same_ledger",
        cpu,
        mem,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

/// Record the per-invocation settlement cost across batch sizes side by side.
/// This is the before/after the issue asks for: before, a settlement was only
/// ever measured against a freshly-submitted invoice (batch size 1 by
/// construction); after, the cost of a settlement sitting inside a 10- and a
/// `SETTLE_BENCH_BATCH`-invoice same-ledger burst is printed next to it, so
/// any batch-size dependence in the per-invocation cost is directly visible.
#[test]
fn benchmark_settle_cost_across_batch_sizes() {
    let mut results: std::vec::Vec<(u32, (u64, u64))> = std::vec::Vec::new();
    for len in [1u32, 10, SETTLE_BENCH_BATCH] {
        let bench = setup_benchmark_env();
        let ids = setup_settle_batch(&bench, len);
        let name = std::format!("mark_paid_in_batch_len_{len}");
        let measured = measure_last_settlement_in_batch(&bench, &ids, &name);
        results.push((len, measured));
    }

    std::println!("\n| Batch length | CPU Instructions | Memory (bytes) |");
    std::println!("| ------------ | ---------------- | -------------- |");
    for (len, (cpu, mem)) in &results {
        std::println!("| {len:>12} | {cpu:>16} | {mem:>14} |");
        assert_within_resource_budget(
            "mark_paid_across_batch_sizes",
            *cpu,
            *mem,
            MAX_CPU_INSTRUCTIONS,
            MAX_MEMORY_BYTES,
        );
    }
}

// ================================================================
// Issue #1064: preventing unbounded event payloads
// ================================================================
//
// `add_price_source` appended to a per-feed source list
// (`DataKey::PriceSources`) with no upper bound, and `get_verified_price`
// iterates that whole list on every price read, publishing one
// `PriceOutlierRejected` event *per* source that deviates past the configured
// threshold. Both the event fan-out of a single read and the storage/CPU cost
// of the iteration therefore grew without bound with the registry.
//
// **Before:** nothing in this file measured the price-read path of
// `get_verified_price` at all, so the cost of iterating an arbitrarily large
// source list — and emitting an outlier event per deviating source — was never
// recorded. A feed grown large enough (by governance or a compromised admin
// key) would only reveal itself by failing a price read at mainnet's
// per-invocation resource limit, breaking the price path for every token on
// that feed.
//
// **After:** `add_price_source` caps each feed at `MAX_PRICE_SOURCES_PER_FEED`
// (see `constants.rs` / `oracle_registry.rs`), so the worst-case cost of a
// price read is fixed. The benchmarks below measure `get_verified_price` at the
// cap: `benchmark_price_read_across_source_counts` records the per-invocation
// cost side by side at 1, half the cap, and the full cap, so any source-count
// dependence is directly visible; and
// `benchmark_price_read_outlier_event_fan_out_at_max_sources` measures the
// worst case for event payloads — a majority that agrees (so the read
// succeeds) and a deviating minority that is rejected, emitting one
// `PriceOutlierRejected` event per deviating source. Each measurement is
// asserted against the same absolute ceilings the rest of this file uses
// (`assert_within_resource_budget`). The behavioural counterpart (the cap
// itself, idempotency, per-feed isolation, slot recovery, authorization, and
// that a single read's event count is bounded by the cap) lives in
// `tests_storage.rs`.

/// Price every agreeing benchmark source reports.
const BENCH_PRICE: i128 = 100;

/// Price the deviating benchmark sources report — far enough from
/// `BENCH_PRICE` to exceed the default max deviation (500 bps) by orders of
/// magnitude, so they are unambiguously rejected as outliers.
const BENCH_PRICE_OUTLIER: i128 = 10_000_000;

/// A minimal price oracle the benchmark registers as a price source: stores a
/// fixed price and returns it from `get_price(token)`, matching the
/// `get_price(Address) -> i128` shape `query_price` invokes.
#[contract]
struct BenchPriceOracle;

#[contractimpl]
impl BenchPriceOracle {
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

fn deploy_bench_price_oracle(bench: &BaseBenchEnv, price: i128) -> Address {
    let id = bench.env.register_contract(None, BenchPriceOracle);
    BenchPriceOracleClient::new(&bench.env, &id).set_price(&price);
    id
}

/// Register `count` price sources all reporting `price`, advancing the ledger
/// past `add_price_source`'s `DEFAULT_RATE_LIMIT_LEDGERS` cooldown between
/// calls so the fill itself is not `RateLimited`.
fn register_agreeing_price_sources(bench: &BaseBenchEnv, count: u32, price: i128) {
    for _ in 0..count {
        let oracle = deploy_bench_price_oracle(bench, price);
        bench.contract.add_price_source(&OracleFeedType::Price, &oracle);
        advance_ledgers(&bench.env, DEFAULT_RATE_LIMIT_LEDGERS as u32);
    }
}

/// Fill the `Price` feed to `MAX_PRICE_SOURCES_PER_FEED` sources: a majority
/// reporting `BENCH_PRICE` (so the read succeeds and sets the median) and a
/// minority reporting the far-off `BENCH_PRICE_OUTLIER` (so they are rejected).
fn register_outlier_price_sources(bench: &BaseBenchEnv) {
    let agreers = (MAX_PRICE_SOURCES_PER_FEED / 2) + 1;
    for i in 0..MAX_PRICE_SOURCES_PER_FEED {
        let price = if i < agreers {
            BENCH_PRICE
        } else {
            BENCH_PRICE_OUTLIER
        };
        let oracle = deploy_bench_price_oracle(bench, price);
        bench.contract.add_price_source(&OracleFeedType::Price, &oracle);
        advance_ledgers(&bench.env, DEFAULT_RATE_LIMIT_LEDGERS as u32);
    }
}

/// Record the per-invocation price-read cost across source counts side by
/// side. Before, this path was unmeasured entirely; after, the cost of a read
/// at 1, half the cap, and the full `MAX_PRICE_SOURCES_PER_FEED` is printed
/// next to each other, making any source-count dependence in the cost — the
/// thing the cap bounds — directly visible.
#[test]
fn benchmark_price_read_across_source_counts() {
    let half_cap = MAX_PRICE_SOURCES_PER_FEED / 2;
    let mut results: std::vec::Vec<(u32, (u64, u64))> = std::vec::Vec::new();
    for len in [1u32, half_cap, MAX_PRICE_SOURCES_PER_FEED] {
        let bench = setup_benchmark_env();
        register_agreeing_price_sources(&bench, len, BENCH_PRICE);
        assert_eq!(
            bench.contract.get_price_sources(&OracleFeedType::Price).len(),
            len
        );

        let name = std::format!("get_verified_price_sources_{len}");
        let measured = measure(&bench.env, &name, || {
            let _ = bench
                .contract
                .get_verified_price(&OracleFeedType::Price, &bench.token);
        });
        results.push((len, measured));
    }

    std::println!("\n| Price sources | CPU Instructions | Memory (bytes) |");
    std::println!("| ------------- | ---------------- | -------------- |");
    for (len, (cpu, mem)) in &results {
        std::println!("| {len:>13} | {cpu:>16} | {mem:>14} |");
        assert_within_resource_budget(
            "get_verified_price_across_source_counts",
            *cpu,
            *mem,
            MAX_CPU_INSTRUCTIONS,
            MAX_MEMORY_BYTES,
        );
    }
}

/// The worst case for event payloads: a full feed of
/// `MAX_PRICE_SOURCES_PER_FEED` sources with a deviating minority, so a single
/// `get_verified_price` invocation queries every source and publishes one
/// `PriceOutlierRejected` event per deviating source. Capping the source list
/// is what bounds this fan-out; measuring it at the cap fixes the number for
/// the baseline table.
#[test]
fn benchmark_price_read_outlier_event_fan_out_at_max_sources() {
    let bench = setup_benchmark_env();
    register_outlier_price_sources(&bench);
    assert_eq!(
        bench.contract.get_price_sources(&OracleFeedType::Price).len(),
        MAX_PRICE_SOURCES_PER_FEED
    );

    let (cpu, mem) = measure(
        &bench.env,
        "get_verified_price_outlier_fan_out_at_max_sources",
        || {
            let _ = bench
                .contract
                .get_verified_price(&OracleFeedType::Price, &bench.token);
        },
    );
    assert_within_resource_budget(
        "get_verified_price_outlier_fan_out_at_max_sources",
        cpu,
        mem,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

// ================================================================
// Issue #1065: documenting safe resource envelopes
// ================================================================
//
// # Before / after
//
// **Before:** the caps that keep every on-chain collection — and therefore the
// per-invocation resource cost of touching it — finite were enforced in five
// different modules (`constants.rs`, `access.rs`, `twap.rs`, …) with no single
// artifact declaring the whole envelope, and there was no measurement of the
// *read*-side cost of a full (cap-sized) collection: only writes into a full
// collection (the #1062 `join_fund_queue`-at-max-queue-size section above) had
// a figure.
//
// **After:** `storage::RESOURCE_ENVELOPE` declares the entire envelope in one
// place (see its doc comment, and the drift/well-formedness tests in
// `tests_storage.rs`), and the benchmark below records the read-side cost of
// the largest bounded collections *at their caps*, tying the declaration to a
// measured, budget-checked figure.

/// Fill the admin-action ring buffer to `ADMIN_ACTION_LOG_CAPACITY` so the log
/// read below exercises a completely full log rather than a near-empty one.
fn fill_admin_action_log(bench: &BaseBenchEnv) {
    bench.env.as_contract(&bench.contract.address, || {
        for _ in 0..ADMIN_ACTION_LOG_CAPACITY {
            crate::access::record_admin_action(&bench.env, "bench");
        }
    });
}

/// Fill the top-payers min-heap to `TOP_PAYERS_CAPACITY`.
fn fill_top_payers(bench: &BaseBenchEnv) {
    bench.env.as_contract(&bench.contract.address, || {
        for i in 0..TOP_PAYERS_CAPACITY {
            let payer = Address::generate(&bench.env);
            crate::top_payers::update_top_payers_on_score_change(&bench.env, &payer, i + 1);
        }
    });
}

/// Record the read-side cost of the largest bounded collections at their
/// declared caps, and assert each is within the absolute resource ceilings.
/// This is the figure the #1065 envelope declaration is checked against: as the
/// table shows, reading the fullest collection the contract can hold stays far
/// inside the per-invocation budget, which is what makes the declared envelope
/// "safe".
#[test]
fn benchmark_envelope_read_views_at_cap() {
    let mut results: std::vec::Vec<(&str, u32, (u64, u64))> = std::vec::Vec::new();

    // Price-source list read at the cap (`DataKey::PriceSources`, #1064).
    let bench = setup_benchmark_env();
    register_agreeing_price_sources(&bench, MAX_PRICE_SOURCES_PER_FEED, BENCH_PRICE);
    let price_read = measure(&bench.env, "envelope_read_price_sources_at_cap", || {
        let _ = bench.contract.get_price_sources(&OracleFeedType::Price);
    });
    results.push((
        "price_sources_per_feed",
        MAX_PRICE_SOURCES_PER_FEED,
        price_read,
    ));

    // Fund-queue read at the cap (`DataKey::FundQueue`, #1091/#1095).
    let (bench_q, invoice_id) = setup_queue_of_len(MAX_FUND_QUEUE_LEN);
    let queue_read = measure(&bench_q.env, "envelope_read_fund_queue_at_cap", || {
        bench_q.env.as_contract(&bench_q.contract.address, || {
            let _ = crate::invoice::get_fund_queue(&bench_q.env, invoice_id);
        });
    });
    results.push(("fund_queue_entries", MAX_FUND_QUEUE_LEN, queue_read));

    // Admin-action ring-buffer read at the cap (`DataKey::AdminActionLog`, #645).
    let bench_a = setup_benchmark_env();
    fill_admin_action_log(&bench_a);
    let admin_read = measure(
        &bench_a.env,
        "envelope_read_admin_action_log_at_cap",
        || {
            let _ = bench_a
                .contract
                .get_recent_admin_actions(&ADMIN_ACTION_LOG_CAPACITY);
        },
    );
    results.push((
        "admin_action_log_entries",
        ADMIN_ACTION_LOG_CAPACITY,
        admin_read,
    ));

    // Top-payers heap read at the cap (`DataKey::TopPayersHeap`, #77).
    let bench_t = setup_benchmark_env();
    fill_top_payers(&bench_t);
    let top_read = measure(&bench_t.env, "envelope_read_top_payers_at_cap", || {
        let _ = bench_t.contract.get_top_payers(&TOP_PAYERS_CAPACITY);
    });
    results.push(("top_payers_entries", TOP_PAYERS_CAPACITY, top_read));

    std::println!("\n| Envelope collection | Cap | CPU Instructions | Memory (bytes) |");
    std::println!("| ------------------- | --- | ---------------- | -------------- |");
    for (name, cap, (cpu, mem)) in &results {
        std::println!("| {name} | {cap} | {cpu} | {mem} |");
        assert_within_resource_budget(*name, *cpu, *mem, MAX_CPU_INSTRUCTIONS, MAX_MEMORY_BYTES);
    }
}

// -----------------------------------------------------------------------
// Issue #1050: oracle-freshness gate cost on the funding path
// -----------------------------------------------------------------------

/// Minimal verifying oracle for the freshness-gate benchmark: returns
/// `is_verified = true` with an externally-set ledger-sequence timestamp,
/// mirroring `tests_oracle_freshness.rs`'s `MockTimestampedOracle`.
#[contract]
struct BenchTimestampedOracle;

#[contractimpl]
impl BenchTimestampedOracle {
    /// Store the ledger-sequence timestamp `get_payer_data` will report.
    pub fn set_timestamp(env: Env, ts: u32) {
        env.storage()
            .instance()
            .set(&soroban_sdk::symbol_short!("ts"), &ts);
    }

    pub fn interface_version(_env: Env) -> u32 {
        crate::oracle_interface::ORACLE_INTERFACE_VERSION
    }

    pub fn get_payer_data(env: Env, _payer: Address) -> OracleVerificationResponse {
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
}

/// Issue #1050 before/after. The freshness gate is now one shared predicate
/// (`oracle_registry::is_oracle_data_stale`) evaluated on the funding path, in
/// place of the inline saturating-subtraction it replaces. `benchmark_fund_invoice`
/// (above) measures the *ungated* path (`require_oracle_verification = false`);
/// this measures the *gated* path (`true`) against a verifying oracle whose data
/// sits exactly on the **last-fresh** ledger for the configured window
/// (`age == max_age - 1`), i.e. the boundary the rule is built around. The delta
/// between the two figures is the cost the ledger-boundary freshness rule adds,
/// and the absolute figure proves the fix — which additionally rejects
/// future-dated data the old formulation accepted — stays inside the same
/// per-invocation budget.
#[test]
fn benchmark_fund_invoice_oracle_freshness_gate() {
    let bench = setup_benchmark_env();

    // Put the sequence somewhere with headroom so the boundary age cannot
    // underflow, and pin a small explicit window so the last-fresh ledger is
    // reachable without advancing the default 17 280-ledger window.
    let mut ledger = bench.env.ledger().get();
    ledger.sequence_number = 1_000;
    bench.env.ledger().set(ledger);

    let max_age: u64 = 10;
    bench.contract.set_max_oracle_age(&max_age).unwrap();

    let id = submit_invoice_bench(&bench);

    // Data one ledger inside the window: age == max_age - 1 → fresh.
    let current_seq = bench.env.ledger().sequence();
    let oracle_ts = current_seq - (max_age as u32 - 1);

    let oracle_id = bench.env.register_contract(None, BenchTimestampedOracle);
    BenchTimestampedOracleClient::new(&bench.env, &oracle_id).set_timestamp(&oracle_ts);
    bench.contract.set_price_oracle(&oracle_id).unwrap();

    let (cpu, mem) = measure(&bench.env, "fund_invoice_oracle_gate_fresh", || {
        bench
            .contract
            .fund_invoice(&bench.lp, &id, &BENCH_INVOICE_AMOUNT, &true);
    });
    assert_within_resource_budget(
        "fund_invoice_oracle_gate_fresh",
        cpu,
        mem,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

// -----------------------------------------------------------------------
// Issue #1052: oracle-timestamp monotonicity gate cost on the funding path
// -----------------------------------------------------------------------

/// Issue #1052 before/after. The monotonicity gate adds one persisted
/// `OracleHealthStatus` read plus a comparison to the funding path, evaluated
/// *before* `record_oracle_health` overwrites the snapshot. The #1050
/// benchmark above measures the gated path with **no** prior snapshot, so the
/// regression predicate short-circuits on "no snapshot"; this measures the
/// gated path with a **seeded** baseline and the oracle re-serving exactly
/// that timestamp (the non-decreasing equality edge — accepted, so the compare
/// actually runs). The delta between the two gated figures is the cost the
/// monotonicity check adds, and the absolute figure proves it stays inside the
/// same per-invocation budget.
#[test]
fn benchmark_fund_invoice_oracle_timestamp_monotonicity_gate() {
    let bench = setup_benchmark_env();

    let mut ledger = bench.env.ledger().get();
    ledger.sequence_number = 1_000;
    bench.env.ledger().set(ledger);

    let id = submit_invoice_bench(&bench);

    // Oracle reports a timestamp a couple of ledgers inside the default window.
    let current_seq = bench.env.ledger().sequence();
    let oracle_ts = current_seq - 2;

    let oracle_id = bench.env.register_contract(None, BenchTimestampedOracle);
    BenchTimestampedOracleClient::new(&bench.env, &oracle_id).set_timestamp(&oracle_ts);
    bench.contract.set_price_oracle(&oracle_id).unwrap();

    // Seed the monotonicity baseline so the predicate reads and compares a
    // stored snapshot rather than short-circuiting.
    bench.env.as_contract(&bench.contract.address, || {
        crate::oracle_registry::record_oracle_health(
            &bench.env,
            OracleFeedType::Identity,
            &bench.token,
            &oracle_id,
            oracle_ts,
            DEFAULT_MAX_ORACLE_AGE_LEDGERS,
        );
    });

    let (cpu, mem) = measure(&bench.env, "fund_invoice_oracle_monotonic", || {
        bench
            .contract
            .fund_invoice(&bench.lp, &id, &BENCH_INVOICE_AMOUNT, &true);
    });
    assert_within_resource_budget(
        "fund_invoice_oracle_monotonic",
        cpu,
        mem,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

// -----------------------------------------------------------------------
// Issue #1053: duplicate-update-frequency gate cost on the funding path
// -----------------------------------------------------------------------

/// Issue #1053 before/after. The duplicate-frequency bound reads
/// `consecutive_duplicate_count` off the *same* `OracleHealthStatus` snapshot
/// the #1052 monotonicity gate already fetches — no extra storage read, one
/// extra comparison — so it should track the monotonicity figure closely. This
/// measures the gated path with the duplicate streak staged one below the cap
/// and the oracle re-serving exactly the recorded timestamp, so the benchmarked
/// read is the duplicate that is *still accepted* (and pushes the streak onto
/// the cap; the next identical read would be rejected with
/// `OracleDuplicateUpdate`). The absolute figure proves the added bound stays
/// inside the same per-invocation budget as the monotonicity gate above.
#[test]
fn benchmark_fund_invoice_oracle_duplicate_frequency_gate() {
    let bench = setup_benchmark_env();

    let mut ledger = bench.env.ledger().get();
    ledger.sequence_number = 1_000;
    bench.env.ledger().set(ledger);

    let id = submit_invoice_bench(&bench);

    let current_seq = bench.env.ledger().sequence();
    let oracle_ts = current_seq - 2;

    let oracle_id = bench.env.register_contract(None, BenchTimestampedOracle);
    BenchTimestampedOracleClient::new(&bench.env, &oracle_id).set_timestamp(&oracle_ts);
    bench.contract.set_price_oracle(&oracle_id).unwrap();

    // `MAX_CONSECUTIVE_DUPLICATE_UPDATES` identical reads leave the streak at
    // cap - 1, so the benchmarked read is accepted (not the rejection path).
    let cap = crate::oracle_registry::MAX_CONSECUTIVE_DUPLICATE_UPDATES;
    bench.env.as_contract(&bench.contract.address, || {
        for _ in 0..cap {
            crate::oracle_registry::record_oracle_health(
                &bench.env,
                OracleFeedType::Identity,
                &bench.token,
                &oracle_id,
                oracle_ts,
                DEFAULT_MAX_ORACLE_AGE_LEDGERS,
            );
        }
    });

    let (cpu, mem) = measure(&bench.env, "fund_invoice_oracle_duplicate", || {
        bench
            .contract
            .fund_invoice(&bench.lp, &id, &BENCH_INVOICE_AMOUNT, &true);
    });
    assert_within_resource_budget(
        "fund_invoice_oracle_duplicate",
        cpu,
        mem,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

// -----------------------------------------------------------------------
// Issue #1046: oracle feeder-update authorization cost
// -----------------------------------------------------------------------

/// Oracle fixture for the feeder-update benchmark. Exposes both the pre-#1046
/// unrestricted write (baseline) and the #1046 feeder-authorized write, so the
/// cost the authorization adds is directly measurable.
#[contract]
struct BenchFeederOracle;

#[contracttype]
enum BenchFeederKey {
    /// Authorized feeder (oracle operator) address.
    Feeder,
    /// Per-payer verification flag.
    Verified(Address),
}

#[contractimpl]
impl BenchFeederOracle {
    /// Configure the authorized feeder address.
    pub fn set_feeder(env: Env, feeder: Address) {
        env.storage()
            .persistent()
            .set(&BenchFeederKey::Feeder, &feeder);
    }

    /// Pre-#1046 behaviour: unrestricted write, no authorization.
    pub fn update_verification_unrestricted(env: Env, payer: Address, verified: bool) {
        env.storage()
            .persistent()
            .set(&BenchFeederKey::Verified(payer), &verified);
    }

    /// Post-#1046 behaviour: feeder-authorized write.
    pub fn update_verification(env: Env, payer: Address, verified: bool) {
        let feeder: Address = match env
            .storage()
            .persistent()
            .get(&BenchFeederKey::Feeder)
        {
            Some(f) => f,
            None => panic!("feeder not configured"),
        };
        feeder.require_auth();
        env.storage()
            .persistent()
            .set(&BenchFeederKey::Verified(payer), &verified);
    }
}

/// Issue #1046 before/after. The ILN funding path is unchanged — the gate lives
/// on the oracle's `update_verification` write path, which the ILN never calls.
/// This benchmark measures that path directly: the unrestricted write that
/// shipped before #1046 versus the feeder-authorized write that ships now (one
/// feeder read + `require_auth()` + the same write). The delta is the cost of
/// closing the "anyone can mint funding eligibility" hole; the absolute figure
/// proves the authorized path still fits easily inside the per-invocation
/// budget.
#[test]
fn benchmark_oracle_feeder_update_authorization() {
    let bench = setup_benchmark_env();

    let feeder = Address::generate(&bench.env);
    let payer = Address::generate(&bench.env);

    let oracle_id = bench.env.register_contract(None, BenchFeederOracle);
    let client = BenchFeederOracleClient::new(&bench.env, &oracle_id);
    client.set_feeder(&feeder);

    // Pre-#1046 baseline: unrestricted write.
    let (before_cpu, before_mem) = measure(&bench.env, "oracle_feeder_update_unrestricted", || {
        client.update_verification_unrestricted(&payer, &true);
    });

    // Post-#1046: feeder-authorized write.
    let (after_cpu, after_mem) = measure(&bench.env, "oracle_feeder_update_authorized", || {
        client.update_verification(&payer, &true);
    });

    std::println!("\n| Feeder update path | CPU Instructions | Memory (bytes) |");
    std::println!("| ------------------ | ---------------- | -------------- |");
    std::println!("| unrestricted (before #1046) | {before_cpu} | {before_mem} |");
    std::println!("| feeder-authorized (after #1046) | {after_cpu} | {after_mem} |");

    assert_within_resource_budget(
        "oracle_feeder_update_unrestricted",
        before_cpu,
        before_mem,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
    assert_within_resource_budget(
        "oracle_feeder_update_authorized",
        after_cpu,
        after_mem,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

// -----------------------------------------------------------------------
// Issue #1047: multi-oracle Identity quorum cost
// -----------------------------------------------------------------------

/// Identity feeder fixture: answers `get_payer_data` with a configurable
/// verified flag and the current ledger as its data timestamp.
#[contract]
struct BenchQuorumOracle;

#[contractimpl]
impl BenchQuorumOracle {
    pub fn interface_version(_env: Env) -> u32 {
        crate::oracle_interface::ORACLE_INTERFACE_VERSION
    }

    pub fn set_verified(env: Env, verified: bool) {
        env.storage()
            .instance()
            .set(&soroban_sdk::symbol_short!("verified"), &verified);
    }

    pub fn get_payer_data(env: Env, _payer: Address) -> OracleVerificationResponse {
        let is_verified: bool = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("verified"))
            .unwrap_or(true);
        OracleVerificationResponse {
            is_verified,
            timestamp: env.ledger().sequence(),
        }
    }
}

/// Advance the benchmark ledger past the registry admin-fn rate-limit cooldown
/// so back-to-back `add_identity_oracle` registrations don't trip `RateLimited`.
fn advance_benchmark_ledger(env: &Env) {
    let mut info = env.ledger().get();
    info.sequence_number += DEFAULT_RATE_LIMIT_LEDGERS + 30;
    info.timestamp += (DEFAULT_RATE_LIMIT_LEDGERS + 30) as u64 * 5;
    env.ledger().set(info);
}

/// Issue #1047 before/after. Records the cost of `fund_invoice`'s
/// oracle-gated payer-verification path in the prior single-oracle mode
/// (baseline) versus the new multi-oracle quorum mode (three feeders, quorum
/// of 3). Both must fit comfortably inside the per-invocation budget; the
/// delta is the price of no longer trusting a single feeder.
#[test]
fn benchmark_identity_quorum_verification_cost() {
    let bench = setup_benchmark_env();

    // Baseline (pre-#1047 single-oracle path): one registered Identity oracle.
    let single = bench.env.register_contract(None, BenchQuorumOracle);
    BenchQuorumOracleClient::new(&bench.env, &single).set_verified(&true);
    bench
        .contract
        .register_oracle(&OracleFeedType::Identity, &single);
    let baseline_invoice = submit_invoice_bench(&bench);

    let (before_cpu, before_mem) = measure(&bench.env, "identity_single_oracle", || {
        bench
            .contract
            .fund_invoice(&bench.lp, &baseline_invoice, &BENCH_INVOICE_AMOUNT, &true);
    });

    // Multi-oracle quorum (#1047): three Identity feeders, all agreeing.
    let mut oracles = [Address::generate(&bench.env); 3];
    for (i, oracle) in oracles.iter_mut().enumerate() {
        if i > 0 {
            advance_benchmark_ledger(&bench.env);
        }
        let id = bench.env.register_contract(None, BenchQuorumOracle);
        BenchQuorumOracleClient::new(&bench.env, &id).set_verified(&true);
        bench
            .contract
            .add_identity_oracle(&OracleFeedType::Identity, &id);
        *oracle = id;
    }
    bench.contract.set_identity_quorum(&3);
    let quorum_invoice = submit_invoice_bench(&bench);

    let (after_cpu, after_mem) = measure(&bench.env, "identity_quorum_of_three", || {
        bench
            .contract
            .fund_invoice(&bench.lp, &quorum_invoice, &BENCH_INVOICE_AMOUNT, &true);
    });

    std::println!("\n| Identity verification path | CPU Instructions | Memory (bytes) |");
    std::println!("| -------------------------- | ---------------- | -------------- |");
    std::println!("| single oracle (before #1047) | {before_cpu} | {before_mem} |");
    std::println!("| 3-of-3 quorum (after #1047) | {after_cpu} | {after_mem} |");

    assert_within_resource_budget(
        "identity_single_oracle",
        before_cpu,
        before_mem,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
    assert_within_resource_budget(
        "identity_quorum_of_three",
        after_cpu,
        after_mem,
        MAX_CPU_INSTRUCTIONS,
        MAX_MEMORY_BYTES,
    );
}

// -----------------------------------------------------------------------
// Issue #1048: circuit-breaker auto-recovery cost on the keeper path
// -----------------------------------------------------------------------

/// Issue #1048 before/after. **Before**, a tripped oracle circuit breaker was
/// sticky: a transient feeder outage jammed oracle-gated `fund_invoice` until a
/// (slow) governance `reset_oracle_circuit`, even after the feeder recovered.
/// **After**, the breaker is half-open — the permissionless `check_oracle_health`
/// keeper path auto-clears it once the *tripping* oracle serves
/// `CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS` consecutive healthy (fresh +
/// timestamp-advancing) observations spanning `CIRCUIT_RECOVERY_MIN_LEDGERS`.
///
/// This benchmark isolates the marginal cost that recovery bookkeeping adds to
/// the keeper path. The baseline is a `check_oracle_health` observation taken
/// with the breaker **closed** (no recovery work — the pre-#1048 shape). The
/// two subsequent figures are the same call while the breaker is **tripped**:
/// a steady-state streak observation (one extra read + write of
/// `CircuitRecoveryState`) and the completion observation that clears the
/// breaker (two removes + the `oracle_circuit_recovered` event). The small
/// delta is the price of un-jamming a recovered feeder without waiting on
/// governance; every figure must stay inside the per-invocation budget.
#[test]
fn benchmark_oracle_circuit_auto_recovery() {
    let bench = setup_benchmark_env();

    // Small explicit window so the boundary ages are easy to reason about.
    let max_age: u64 = 10;
    bench.contract.set_max_oracle_age(&max_age).unwrap();

    let oracle_id = bench.env.register_contract(None, BenchTimestampedOracle);
    let oracle_client = BenchTimestampedOracleClient::new(&bench.env, &oracle_id);
    bench.contract.set_price_oracle(&oracle_id).unwrap();

    // Baseline: a keeper observation with the breaker *closed* — no recovery
    // bookkeeping runs, matching the pre-#1048 path exactly.
    oracle_client.set_timestamp(&bench.env.ledger().sequence());
    let (closed_cpu, closed_mem) = measure(&bench.env, "oracle_check_health_closed", || {
        bench.contract.check_oracle_health(
            &OracleFeedType::Identity,
            &bench.token,
            &bench.payer,
        );
    });

    // Trip the breaker: three stale observations (timestamp 0, far outside the
    // window) through the same permissionless keeper path.
    oracle_client.set_timestamp(&0);
    for _ in 0..crate::oracle_registry::MAX_CONSECUTIVE_STALE_QUERIES {
        bench
            .contract
            .check_oracle_health(&OracleFeedType::Identity, &bench.token, &bench.payer);
    }
    assert!(
        bench
            .contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &bench.token),
        "setup must leave the Identity breaker tripped"
    );

    let needed = crate::oracle_registry::CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS;
    let window = crate::oracle_registry::CIRCUIT_RECOVERY_MIN_LEDGERS as u32;

    // First healthy observation while tripped starts the recovery streak; then
    // a spaced observation gives the steady-state streak cost (extra read +
    // write of the recovery record, breaker still open because the ledger
    // window is only satisfied once).
    oracle_client.set_timestamp(&bench.env.ledger().sequence());
    bench
        .contract
        .check_oracle_health(&OracleFeedType::Identity, &bench.token, &bench.payer);
    advance_ledgers(&bench.env, window);
    oracle_client.set_timestamp(&bench.env.ledger().sequence());
    let (streak_cpu, streak_mem) = measure(&bench.env, "oracle_circuit_recovery_streak", || {
        bench
            .contract
            .check_oracle_health(&OracleFeedType::Identity, &bench.token, &bench.payer);
    });

    // Remaining in-between observations (unmeasured) bring the streak up to
    // `needed - 1` without completing — the final observation below is the one
    // that clears the breaker.
    for _ in 2..(needed - 1) {
        advance_ledgers(&bench.env, window);
        oracle_client.set_timestamp(&bench.env.ledger().sequence());
        bench
            .contract
            .check_oracle_health(&OracleFeedType::Identity, &bench.token, &bench.payer);
    }

    // Final healthy observation after another full window completes the streak
    // (`needed` observations AND >= `window` ledgers elapsed): the breaker is
    // cleared (two removes + `oracle_circuit_recovered`).
    advance_ledgers(&bench.env, window);
    oracle_client.set_timestamp(&bench.env.ledger().sequence());
    let (done_cpu, done_mem) = measure(&bench.env, "oracle_circuit_recovery_complete", || {
        bench
            .contract
            .check_oracle_health(&OracleFeedType::Identity, &bench.token, &bench.payer);
    });
    assert!(
        !bench
            .contract
            .is_oracle_circuit_tripped(&OracleFeedType::Identity, &bench.token),
        "the completion observation must auto-recover the breaker"
    );

    std::println!("\n| Circuit recovery path (per keeper observation) | CPU Instructions | Memory (bytes) |");
    std::println!("| --------------------------------------------- | ---------------- | -------------- |");
    std::println!("| breaker closed (before #1048 baseline) | {closed_cpu} | {closed_mem} |");
    std::println!("| tripped, streak advances (after #1048) | {streak_cpu} | {streak_mem} |");
    std::println!("| tripped, completion clears breaker (after #1048) | {done_cpu} | {done_mem} |");

    for (name, cpu, mem) in [
        ("oracle_check_health_closed", closed_cpu, closed_mem),
        ("oracle_circuit_recovery_streak", streak_cpu, streak_mem),
        ("oracle_circuit_recovery_complete", done_cpu, done_mem),
    ] {
        assert_within_resource_budget(
            name,
            cpu,
            mem,
            MAX_CPU_INSTRUCTIONS,
            MAX_MEMORY_BYTES,
        );
    }
}

// -----------------------------------------------------------------------
// Issue #1049: cost of the stale + quorum failure events
// -----------------------------------------------------------------------

/// Issue #1049 before/after. **Before**, a rejected `fund_invoice` detected
/// stale data or feeder disagreement but reported it by returning `Err`, and
/// Soroban rolls every event of an `Err`-returning invocation back — so the
/// failure left no on-chain trace. **After**, the shared gate is also exposed
/// as the permissionless `check_oracle_verification` diagnostic, whose
/// `oracle_stale_data_rejected` and `oracle_feeder_disagreement` events commit.
///
/// This measures the diagnostic's cost in three states: a *passing*
/// verification (no failure event — the pre-#1049 equivalent of a silent
/// success), a *stale* rejection that emits `OracleDataStaleRejected`, and a
/// *quorum disagreement* that emits `OracleFeederDisagreement`. The delta is
/// the price of making those failures observable; each figure must stay inside
/// the per-invocation budget.
#[test]
fn benchmark_oracle_verification_diagnostic_events() {
    let bench = setup_benchmark_env();
    let max_age: u64 = 10;
    bench.contract.set_max_oracle_age(&max_age).unwrap();

    // Baseline: fresh, verified single oracle → Verified, no failure event.
    let oracle = bench.env.register_contract(None, BenchTimestampedOracle);
    let oracle_client = BenchTimestampedOracleClient::new(&bench.env, &oracle);
    oracle_client.set_timestamp(&bench.env.ledger().sequence());
    bench
        .contract
        .register_oracle(&OracleFeedType::Identity, &oracle);
    let (ok_cpu, ok_mem) = measure(&bench.env, "oracle_verification_verified", || {
        bench
            .contract
            .check_oracle_verification(&OracleFeedType::Identity, &bench.token, &bench.payer);
    });

    // Stale rejection → emits `oracle_stale_data_rejected`. A single observation
    // stays below MAX_CONSECUTIVE_STALE_QUERIES, so the breaker is still closed.
    advance_ledgers(&bench.env, (max_age as u32) + 10);
    let (stale_cpu, stale_mem) = measure(&bench.env, "oracle_verification_stale_event", || {
        bench
            .contract
            .check_oracle_verification(&OracleFeedType::Identity, &bench.token, &bench.payer);
    });

    // Quorum disagreement → emits `oracle_feeder_disagreement`: three feeders,
    // two agreeing, quorum of three. Fresh timestamps, so only the disagreement
    // is at issue.
    for i in 0..3u32 {
        if i > 0 {
            advance_benchmark_ledger(&bench.env);
        }
        let id = bench.env.register_contract(None, BenchQuorumOracle);
        BenchQuorumOracleClient::new(&bench.env, &id).set_verified(&(i < 2));
        bench
            .contract
            .add_identity_oracle(&OracleFeedType::Identity, &id);
    }
    bench.contract.set_identity_quorum(&3);
    let (disagree_cpu, disagree_mem) =
        measure(&bench.env, "oracle_verification_quorum_disagreement_event", || {
            bench
                .contract
                .check_oracle_verification(&OracleFeedType::Identity, &bench.token, &bench.payer);
        });

    std::println!(
        "\n| Verification diagnostic path (Issue #1049) | CPU Instructions | Memory (bytes) |"
    );
    std::println!("| ------------------------------------------ | ---------------- | -------------- |");
    std::println!("| verified, no failure event (baseline) | {ok_cpu} | {ok_mem} |");
    std::println!("| stale → OracleDataStaleRejected | {stale_cpu} | {stale_mem} |");
    std::println!(
        "| quorum disagreement → OracleFeederDisagreement | {disagree_cpu} | {disagree_mem} |"
    );

    for (name, cpu, mem) in [
        ("oracle_verification_verified", ok_cpu, ok_mem),
        ("oracle_verification_stale_event", stale_cpu, stale_mem),
        (
            "oracle_verification_quorum_disagreement_event",
            disagree_cpu,
            disagree_mem,
        ),
    ] {
        assert_within_resource_budget(name, cpu, mem, MAX_CPU_INSTRUCTIONS, MAX_MEMORY_BYTES);
    }
}
