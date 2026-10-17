# Smart Contract Benchmarks

*Date:* 2026-05-30 (invoice_liquidity), 2026-07-25 (iln_governance)

These values are baseline metrics for core contract execution: CPU instructions and memory bytes consumed via Soroban's cost meter (`env.budget()`). CI compares each run against the contract's `benchmarks/baseline.json` and emits a **warning** (not a failure) when either metric regresses by more than 10%.

## Baseline Execution Results

### `contracts/invoice_liquidity`

| Function                        | CPU Instructions | Memory (bytes) |
| ------------------------------- | ---------------- | -------------- |
| submit_invoice                  |           859421 |          26485 |
| fund_invoice                    |          1041920 |          38190 |
| mark_paid                       |           948123 |          35480 |
| join_fund_queue_max_depth       |          1124580 |          42350 |
| resolve_fund_queue_max_depth    |           582140 |          28120 |
| extension_cost_at_maximum_depth |           321850 |          16420 |

### `contracts/iln_governance`

| Function        | CPU Instructions | Memory (bytes) |
| ---------------- | ---------------- | -------------- |
| create_proposal  |            220773 |          33354 |
| cast_vote        |            252321 |          41057 |
| delegate_votes   |            182897 |          28102 |

## Re-Running Locally

```bash
cd contracts/invoice_liquidity   # or contracts/iln_governance
cargo test --target x86_64-unknown-linux-gnu benchmark -- --nocapture
```

Each benchmark test prints machine-readable lines:

```
BENCHMARK submit_invoice cpu=859421 mem=26485
```

## CI Regression Check

```bash
bash scripts/check_benchmark_regression.sh                   # checks every contract listed below
bash scripts/check_benchmark_regression.sh contracts/iln_governance:contracts/iln_governance/benchmarks/baseline.json  # checks one
```

Relative drift from the recorded baseline above `BENCHMARK_REGRESSION_THRESHOLD`
(default 10%) is still **warning-only** — reported as `::warning::`
annotations in GitHub Actions, never failing the script on its own. It
checks `contracts/invoice_liquidity` and `contracts/iln_governance` by
default.

## Absolute instruction/memory thresholds (Issue #1066)

Relative drift alone can never catch a contract that is bloated from the
start, or one that regresses gradually across many small PRs each under the
10% threshold. The script (and
`contracts/invoice_liquidity/src/tests_benchmarks.rs`'s own `#[test]`
assertions, which now run under plain `cargo test -p invoice_liquidity` /
`make test-invoice` independent of this script) additionally enforce an
**absolute** ceiling, regardless of baseline:

| Resource        | Default ceiling      | Env var override       |
| ---------------- | -------------------- | ----------------------- |
| CPU instructions | 100,000,000           | `MAX_CPU_INSTRUCTIONS`  |
| Memory           | 41,943,040 (40 MiB)   | `MAX_MEMORY_BYTES`      |

**Before:** `check_benchmark_regression.sh` "always exits 0" (its own prior
header comment) — no measured value, however large, could fail CI through
this script. The Rust benchmark file wasn't even registered in `lib.rs`, so
it never ran under `cargo test` at all, and its calls had drifted out of
sync with `submit_invoice`'s real signature.

**After:** breaching either absolute ceiling makes
`check_benchmark_regression.sh` exit 1 (printing `::error::`, not
`::warning::`), on top of the unchanged relative-drift warnings. The
corresponding Rust-side `assert_within_resource_budget` assertions in
`tests_benchmarks.rs` fail the test suite directly for the same reason,
independent of whether the shell script is run at all.

`soroban-sdk` 27's `Env::default()` *already* auto-enforces
`InvocationResourceLimits::mainnet()` for every top-level invocation in
these tests (see
[`tests_resource_limit_failure.rs`](../contracts/invoice_liquidity/src/tests_resource_limit_failure.rs),
Issue #1067) — per that SDK version's bundled snapshot (dated
2026-07-10), that's **400,000,000** instructions and **41,943,040** bytes
(40 MiB) of memory. `MAX_MEMORY_BYTES` above matches that exactly;
`MAX_CPU_INSTRUCTIONS` is deliberately set to a quarter of it
(100,000,000) so this check (and the mirrored Rust-side assertions) fire
well before the SDK's own default trap would. That snapshot is baked into
the SDK rather than pulled dynamically from the live network, so prefer
upgrading `soroban-sdk` over hand-editing these defaults if it's ever found
to be stale — see that crate's `_migrating::v25_resource_limits` module
doc, or `stellar network settings --network mainnet` / the official
[network limits page](https://lab.stellar.org/network-limits) to check the
live value. They are set ~4x (CPU, vs. the SDK's own default enforcement)
/ ~1000x (memory, vs. the measured baselines in the table above) above
what would otherwise already be caught, so they exist to catch
catastrophic regressions (e.g. an accidentally introduced O(n²) path), not
to police ordinary optimization work.

**Migration note:** behavior-only addition, no contract or baseline format
changed. If a function's measured cost is ever already above one of these
defaults, raise the override env var deliberately (with a comment
explaining why) rather than treating the failure as a false positive to
silence.

## Near-limit invocation monitoring (Issue #1068)

The hard ceiling above only fires once a measurement actually breaches it —
by then, the regression that caused it is already failing CI, with no
earlier signal that it was coming. `tests_benchmarks.rs`'s
`assert_within_resource_budget` adds a third, strictly softer tier: once a
measurement passes **70%** (`NEAR_LIMIT_WARNING_RATIO`) of
`MAX_CPU_INSTRUCTIONS` / `MAX_MEMORY_BYTES`, it prints a
`NEAR_LIMIT_WARNING` line — still a passing test, just a visible signal.
`check_benchmark_regression.sh` passes these through as `::warning::`
GitHub Actions annotations (never failing the script, consistent with its
existing warning-only relative-drift check).

**Before:** nothing distinguished "comfortably within budget" from
"technically still passing, but close enough to the ceiling that the next
small regression will fail it" — a function could sit at 95% of either
ceiling indefinitely without anyone noticing until the one PR that finally
pushed it over.

**After:** crossing 70% of either ceiling prints a warning immediately,
locally (`--nocapture`) and in CI, well before the actual 100% failure
point from Issue #1066.

Why this can't be done *on-chain*: Soroban gives deployed contract code no
runtime-queryable way to introspect its own CPU/memory usage so far —
`env.cost_estimate()`/`.budget()` are `testutils`-only, host-side
instrumentation that only exists inside the test harness. There is no
mainnet equivalent a live contract could call to check "how close am I to
the limit right now?" and emit its own warning event. This kind of
monitoring is only possible where the resource usage is actually
measurable after the fact — here, in the test/CI harness — which is also
why it lives alongside the benchmarks rather than as a new contract entry
point (contrast with Issue #1015's `get_fund_queue_ttl_margins`, which
*is* a contract view function, because storage TTL genuinely is something
a deployed contract can read via a real host function).

The WASM-*binary*-size counterpart (which, like the hard ceiling in
`docs/wasm-size.md`, can't be measured from inside a native `cargo test`
run either) is implemented the same way in `scripts/check_wasm_size.sh`:
`NEAR_LIMIT_WARNING_PERCENT` (default 70) prints an `::warning::` once a
contract's built WASM passes that fraction of `MAX_WASM_SIZE_BYTES`,
without failing the script.

**Migration note:** warning-only addition, nothing fails differently than
before. If a function is already past 70% of its ceiling today, that
warning is accurate, not a false positive — treat it as the intended
early signal to investigate, not something to silence by raising the
ratio.

## Updating Baselines

After an intentional optimisation or contract change:

1. Run the benchmark suite locally with `--nocapture`.
2. Update `contracts/<contract>/benchmarks/baseline.json`.
3. Update the table in this document.
