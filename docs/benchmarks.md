# Smart Contract Benchmarks

*Date:* 2026-05-30 (invoice_liquidity), 2026-07-25 (iln_governance)

These values are baseline metrics for core contract execution: CPU instructions and memory bytes consumed via Soroban's cost meter (`env.budget()`). CI compares each run against the contract's `benchmarks/baseline.json` and emits a **warning** (not a failure) when either metric regresses by more than 10%.

## Baseline Execution Results

### `contracts/invoice_liquidity`

| Function       | CPU Instructions | Memory (bytes) |
| -------------- | ---------------- | -------------- |
| submit_invoice |           859421 |          26485 |
| fund_invoice   |          1041920 |          38190 |
| mark_paid      |           948123 |          35480 |

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

## TTL cost at maximum queue size (Issue #1062)

Issues #1091/#1095 bounded a single invoice's fund queue to
`MAX_FUND_QUEUE_LEN` (50) entries, admitting a new LP only if it strictly
outscores the current tail (which it then displaces). That bound is what
keeps the serialized `FundQueue(id)` entry — and therefore the per-write
`extend_ttl` host call that keeps the queue alive (Issue #1011) — inside
Soroban's per-transaction resource limits. Nothing previously measured that
worst case.

**Before:** the [`invoice_liquidity` baseline table](#baseline-execution-results)
covered only the three single-invoice flows (`submit_invoice`, `fund_invoice`,
`mark_paid`). No benchmark filled a fund queue at all, so the maximum-size
admission (`join_fund_queue`'s displacing path), resolution, and TTL
extension were entirely unmeasured — a change that enlarged a queue entry
could push the maximum-size write past the network limit with nothing in CI
to catch it.

**After:** `contracts/invoice_liquidity/src/tests_benchmarks.rs` adds five
benchmarks under Issue #1062, each asserting against the same absolute
ceilings as the rest of the file:

| Benchmark (`BENCHMARK` name) | What it measures |
| ---------------------------- | ---------------- |
| `join_fund_queue_at_max_queue_size` | admission into a *full* queue (scan, pop, insert, full re-serialize, TTL extension) |
| `resolve_fund_queue_at_max_queue_size` | `resolve_fund_queue` over a full queue |
| `extend_ttl_fund_queue_at_max_queue_size` | the isolated `extend_ttl` host call on the queue key at 50 entries |
| `extend_ttl_fund_queue_at_one_entry` | the same isolated call at 1 entry (minimum) |
| `extend_ttl_fund_queue_len_{1,10,50}` | the isolated call at 1 / 10 / 50 entries (emitted by `benchmark_ttl_cost_across_queue_sizes`), printed as a table |

The TTL measurements first age the ledger below `FUND_QUEUE_TTL_THRESHOLD`,
because `extend_ttl` short-circuits to a no-op while the entry still has
`>= FUND_QUEUE_TTL_THRESHOLD` ledgers left — without that, the measurement
would understate the cost it exists to capture. Because the TTL target is
key-based (not value-based), the entry-size-dependent part of the cost shows
up in the *write/serialization* portion, which the
`extend_ttl_fund_queue_across_sizes` table isolates.

Run them with:

```bash
cd contracts/invoice_liquidity
cargo test --target x86_64-unknown-linux-gnu benchmark_ -- --nocapture
```

**Migration note:** measurement-only addition — no contract behavior, storage
layout, or baseline format changed. To record these as a tracked baseline,
run the command above and add the printed `BENCHMARK` values to
`contracts/invoice_liquidity/benchmarks/baseline.json` alongside the existing
three entries.

## Concurrent settlement stress (Issue #1063)

Settlement (`mark_paid`) is the contract's highest-value entry point, and it
touches several pieces of *shared* state on every call: the reentrancy lock
(`StorageKey::ReentrancyLock`), the aggregate counter (`StorageKey::TotalPaid`),
and `ContractStats` (`total_paid`, `total_volume_*`). Soroban executes one
invocation at a time, so "concurrent" settlement — independent payers settling
their invoices in the same ledger — is really a *same-ledger batch* of
`mark_paid` calls.

**Before:** the [`invoice_liquidity` baseline table](#baseline-execution-results)
measured `mark_paid` only in isolation, against a freshly-submitted invoice. No
benchmark settled a batch, so a per-settlement cost that grew with the size of
the surrounding batch (e.g. re-serializing a shared structure that accumulates,
or an unbounded counter read) was invisible to CI.

**After:** `contracts/invoice_liquidity/src/tests_benchmarks.rs` adds two
benchmarks under Issue #1063, each asserting against the same absolute ceilings
as the rest of the file:

| Benchmark (`BENCHMARK` name) | What it measures |
| ---------------------------- | ---------------- |
| `settle_batch_same_ledger` | the *per-invocation* cost of the last settlement in a 25-invoice same-ledger burst |
| `mark_paid_in_batch_len_{1,10,25}` | the same per-invocation cost at batch sizes 1 / 10 / 25 (emitted by `benchmark_settle_cost_across_batch_sizes`), printed as a table |

The per-invocation cost is the figure mainnet's resource limit actually applies
to: each `mark_paid` is its own transaction regardless of how many other
settlements land in the same ledger. The `mark_paid_in_batch_len_*` table makes
any batch-size dependence directly visible — a flat row set confirms settlement
cost stays O(1) in the surrounding burst, which is what the shared-counter and
per-invoice-index design depends on. The behavioural counterpart (per-invoice
isolation, rejection isolation, authorization, and the `paid_on_time`
boundary) lives in `tests_storage.rs`.

**Migration note:** measurement-only addition — no contract behavior, storage
layout, or baseline format changed. To record these as a tracked baseline, run
the command above and add the printed `BENCHMARK` values to
`contracts/invoice_liquidity/benchmarks/baseline.json` alongside the existing
entries.

## Price-read event fan-out (Issue #1064)

`get_verified_price` queries *every* registered source for a feed
(`DataKey::PriceSources`) and publishes one `PriceOutlierRejected` event **per
source** that deviates from the cross-source median beyond the configured
threshold. `add_price_source` appended to that list with no upper bound, so both
the number of events a single price read could emit and the CPU/storage cost of
iterating the list were unbounded.

**Before:** no benchmark in this file measured the `get_verified_price` path at
all. The cost of a price read — and its per-deviating-source event fan-out — grew
without bound with the registry, so a feed grown large enough (by governance or a
compromised admin key) would only reveal itself by exceeding mainnet's
per-invocation resource limit, failing the price path for every token on that
feed.

**After:** `add_price_source` caps each feed at `MAX_PRICE_SOURCES_PER_FEED`
(20 — see [`oracle-design.md`](oracle-design.md)), which bounds the worst-case
cost of a price read to a fixed maximum.
`contracts/invoice_liquidity/src/tests_benchmarks.rs` adds two benchmarks under
Issue #1064, each asserting against the same absolute ceilings as the rest of the
file:

| Benchmark (`BENCHMARK` name) | What it measures |
| ---------------------------- | ---------------- |
| `get_verified_price_sources_{1,10,20}` | the per-invocation cost of a price read at 1 / half-cap / full-cap source counts (emitted by `benchmark_price_read_across_source_counts`), printed as a table |
| `get_verified_price_outlier_fan_out_at_max_sources` | the worst case for event payloads: a full 20-source feed with a deviating minority (one `PriceOutlierRejected` per deviating source), read succeeding on the agreeing majority |

The `get_verified_price_sources_*` table makes any source-count dependence
directly visible; `get_verified_price_outlier_fan_out_at_max_sources` pins the
event-payload worst case at the cap. The behavioural counterpart (the cap itself,
per-feed isolation, idempotent re-registration, slot recovery on removal,
authorization, and the assertion that a single read emits at most one outlier
event per registered source) lives in `tests_storage.rs`.

**Migration note:** measurement-only addition — no contract behavior, storage
layout, or baseline format changed. To record these as a tracked baseline, run
the command above and add the printed `BENCHMARK` values to
`contracts/invoice_liquidity/benchmarks/baseline.json` alongside the existing
entries.

## Safe resource envelope read cost (Issue #1065)

The contract stays inside Soroban's per-invocation CPU/memory budget only if every
on-chain collection it iterates or rewrites is bounded. Those bounds are now
declared in one place — `storage::RESOURCE_ENVELOPE` in
`contracts/invoice_liquidity/src/storage.rs` (see
[`storage-layout.md`](storage-layout.md#the-safe-resource-envelope-issue-1065)).
Issue #1065 ties that declaration to a measured figure.

**Before:** the caps were enforced across five modules with no single artifact
declaring the envelope, and no benchmark in this file measured the *read*-side cost
of a **full** (cap-sized) collection — only writes into a full collection (the
#1062 section above) had a figure.

**After:** `contracts/invoice_liquidity/src/tests_benchmarks.rs` adds one
benchmark, `benchmark_envelope_read_views_at_cap`, that reads the largest bounded
collections at their declared caps, each measurement asserted against the same
absolute ceilings as the rest of the file:

| Benchmark (`BENCHMARK` name) | Collection read at its cap |
| ---------------------------- | -------------------------- |
| `envelope_read_price_sources_at_cap` | `PriceSources(Price)` at `MAX_PRICE_SOURCES_PER_FEED` (20) |
| `envelope_read_fund_queue_at_cap` | `FundQueue` at `MAX_FUND_QUEUE_LEN` (50) |
| `envelope_read_admin_action_log_at_cap` | `AdminActionLog` ring buffer at `ADMIN_ACTION_LOG_CAPACITY` (50) |
| `envelope_read_top_payers_at_cap` | `TopPayersHeap` at `TOP_PAYERS_CAPACITY` (50) |

The test prints these as a table and asserts each is within budget, which is what
"safe" means here: reading the fullest collection the contract can hold stays far
inside the per-invocation limit. The declaration itself is kept honest by the
Issue #1065 tests in `tests_storage.rs` (drift, well-formedness, a live
ledger-boundary cap, and an authorization case).

**Migration note:** measurement-only addition — no contract behavior, storage
layout, or baseline format changed. To record these as a tracked baseline, run the
command below and add the printed `BENCHMARK` values to
`contracts/invoice_liquidity/benchmarks/baseline.json` alongside the existing
entries.

## Oracle-freshness gate cost on funding (Issue #1050)

`fund_invoice`'s oracle gate decides staleness with one shared predicate,
`oracle_registry::is_oracle_data_stale(current_ledger, data_timestamp,
max_age_ledgers)` (see
[`audit-evidence-remediation-map.md`](audit-evidence-remediation-map.md#9-oracle-freshness-across-ledger-boundaries-issue-1050)).
Issue #1050 ties that rule to a measured figure on the funding path.

**Before:** the staleness check was an inline saturating subtraction
(`current_ledger.saturating_sub(response.timestamp) >= max_age`) duplicated in the
gate and in `record_oracle_health`. Because the subtraction saturates at zero, a
`response.timestamp` **ahead of** the current ledger read as age 0 — "maximally
fresh" — so a future-dated timestamp slipped past the bound. There was no
benchmark of the *gated* funding path at all (`benchmark_fund_invoice` runs with
`require_oracle_verification = false`).

**After:** `tests_benchmarks.rs` adds
`benchmark_fund_invoice_oracle_freshness_gate`, which funds with
`require_oracle_verification = true` against a verifying oracle whose data sits
exactly on the last-fresh ledger (`age == max_age - 1`), i.e. the boundary the rule
is built around. Measured against the same absolute ceilings as the rest of the
file:

| Benchmark (`BENCHMARK` name) | Path measured |
| ---------------------------- | ------------- |
| `fund_invoice` (existing) | ungated: `require_oracle_verification = false`, no oracle lookup |
| `fund_invoice_oracle_gate_fresh` | gated: `require_oracle_verification = true`, oracle resolved and freshness rule evaluated at the last-fresh boundary |

The delta between the two is the cost the ledger-boundary freshness rule adds; the
absolute figure shows the fix — which additionally rejects future-dated data the
old formulation accepted — stays inside the same per-invocation budget. Boundary
correctness itself is covered by the Issue #1050 tests in
`tests_oracle_freshness.rs` (last-fresh / first-stale / one-past, a one-ledger walk
across the boundary, the `u32` range ends, the `max_oracle_age_ledgers == 0` escape
hatch, future-dated data end-to-end, and two proptests).

**Migration note:** measurement-only addition — no contract behavior, storage
layout, or baseline format changed. To record this as a tracked baseline, run the
command below and add the printed `BENCHMARK` values to
`contracts/invoice_liquidity/benchmarks/baseline.json` alongside the existing
entries.

## Oracle-timestamp monotonicity gate cost on funding (Issue #1052)

The companion direction check to #1050 above: `fund_invoice` also rejects a
`response.timestamp` **strictly older** than the last recorded for the **same**
oracle on that `(feed type, token)`, using the shared predicate
`oracle_registry::is_oracle_data_timestamp_regression` (see
[`audit-evidence-remediation-map.md`](audit-evidence-remediation-map.md#11-monotonic-oracle-timestamp-checks-issue-1052)).
Issue #1052 ties that check to a measured figure on the funding path.

**Before:** no direction check existed — an oracle could re-serve a
previously-seen timestamp (replay / rolled-back clock) and, as long as it was
inside `max_oracle_age_ledgers`, the gate accepted it. There was no measured cost
for a monotonicity comparison because there was no comparison to make.

**After:** `fund_invoice`'s gate reads the persisted `OracleHealthStatus`
snapshot (before `record_oracle_health` overwrites it) and rejects a lower
same-oracle timestamp with `ContractError::OracleTimestampRegression`. The new
benchmark measures the gated path with a **seeded** baseline and the oracle
re-serving exactly that timestamp (the non-decreasing equality edge — accepted,
so the compare actually runs), against the same absolute ceilings as the rest of
the file:

| Benchmark (`BENCHMARK` name) | Path measured |
| ---------------------------- | ------------- |
| `fund_invoice_oracle_gate_fresh` (Issue #1050) | gated, but **no** prior snapshot → the regression predicate short-circuits |
| `fund_invoice_oracle_monotonic` | gated, **seeded** baseline → the regression predicate reads and compares a stored snapshot |

The delta between the two gated figures is the cost the monotonicity check adds;
the absolute figure shows it stays inside the same per-invocation budget.
Boundary correctness itself is covered by the Issue #1052 tests in
`tests_oracle_freshness.rs` (first read with no snapshot, equal re-read, a
strictly-older same-oracle timestamp rejected, gate-requires-verification, the
sequence lower bound `0`, a different oracle's timeline not compared, and a
proptest).

**Migration note:** measurement-only addition — no contract behavior, storage
layout, or baseline format changed. To record this as a tracked baseline, run the
command below and add the printed `BENCHMARK` values to
`contracts/invoice_liquidity/benchmarks/baseline.json` alongside the existing
entries.

## Oracle duplicate-update-frequency gate cost on funding (Issue #1053)

The companion to the #1052 monotonicity check above: `fund_invoice` also rejects
a same-oracle response repeating an **identical** `timestamp` more than
`MAX_CONSECUTIVE_DUPLICATE_UPDATES` (3) times in a row, using the shared
predicate `oracle_registry::is_oracle_data_duplicate_update` (see
[`audit-evidence-remediation-map.md`](audit-evidence-remediation-map.md#12-bound-duplicate-update-frequency-issue-1053)).
Issue #1053 ties that bound to a measured figure on the funding path.

**Before:** #1052's non-decreasing rule accepted an equal timestamp for as long
as it stayed inside `max_oracle_age_ledgers` — a "frozen" oracle that never
advanced kept passing (and kept resetting the stale streak). There was no
counter and no measured cost for a duplicate bound.

**After:** `fund_invoice`'s gate reads `OracleHealthStatus.consecutive_duplicate_count`
off the **same** snapshot the #1052 check already fetches — no extra storage read,
one extra comparison — and rejects the over-cap duplicate with
`ContractError::OracleDuplicateUpdate`. The new benchmark measures the gated path
with the duplicate streak staged one below the cap and the oracle re-serving
exactly the recorded timestamp (the *still-accepted* duplicate edge), against the
same absolute ceilings as the rest of the file:

| Benchmark (`BENCHMARK` name) | Path measured |
| ---------------------------- | ------------- |
| `fund_invoice_oracle_gate_fresh` (Issue #1050) | gated, but **no** prior snapshot |
| `fund_invoice_oracle_monotonic` (Issue #1052) | gated, **seeded** baseline, equality edge |
| `fund_invoice_oracle_duplicate` | gated, streak one below the cap, accepted duplicate edge |

The duplicate figure should track `fund_invoice_oracle_monotonic` closely (same
read, one extra compare); the absolute figure shows the added bound stays inside
the same per-invocation budget. Boundary correctness itself is covered by the
Issue #1053 tests in `tests_oracle_freshness.rs` (duplicates up to the bound
accepted, one past the bound rejected, the `cap - 1` / `cap` boundary, an advance
resetting the streak, gate-requires-verification, a different oracle not counted,
and a proptest).

**Migration note:** measurement-only addition — no contract behavior, storage
layout, or baseline format changed. To record this as a tracked baseline, run the
command below and add the printed `BENCHMARK` values to
`contracts/invoice_liquidity/benchmarks/baseline.json` alongside the existing
entries.

## Updating Baselines

After an intentional optimisation or contract change:

1. Run the benchmark suite locally with `--nocapture`.
2. Update `contracts/<contract>/benchmarks/baseline.json`.
3. Update the table in this document.
