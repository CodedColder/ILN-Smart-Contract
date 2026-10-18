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

## Worst-case queue insertion (Issue #1057)

`join_fund_queue` has two scans (a duplicate check and the sorted-insert
position) plus, when the queue is full, a tail displacement and a full
re-serialize of the whole `Vec<LpFundRequest>`. The worst case is therefore an
*admission into a full queue*; the fund-queue bound at `MAX_FUND_QUEUE_LEN`
(50, Issues #1091/#1095) is what keeps that write inside Soroban's
per-transaction limits.

**Before:** the #1062 section above measured a single point — one displacing
admission into a full queue whose entries all carried the *same* score. With
equal scores every admitted joiner is forced to index 0, so that measurement
could not distinguish a head insert from a tail one, never established that the
head is the worst position, and never measured the `QueueFull` rejection path at
the bound. "Worst case" was an assertion, not a demonstration.

**After:** `contracts/invoice_liquidity/src/tests_benchmarks.rs` adds
`benchmark_worst_case_queue_insertion`, which builds a full queue with
*distinct, strictly descending* scores (`100, 98, …, 2`) and sweeps the
insertion position, asserting each point against the same absolute ceilings as
the rest of the file:

| Benchmark (`BENCHMARK` name) | What it measures |
| ---------------------------- | ---------------- |
| `join_fund_queue_head_insert_at_max` | admission at **index 0** — the maximum element-shift position, displacing the tail |
| `join_fund_queue_middle_insert_at_max` | admission at the **middle** (index 25) |
| `join_fund_queue_tail_insert_at_max` | admission at the **tail** (index 49) — the minimum element-shift position |
| `join_fund_queue_rejected_at_max` | the **`QueueFull`** rejection path at the bound (full duplicate scan + bound check, no write) |

The sweep's finding: the cost is dominated by the serialized entry size at the
bound (the O(n) scans, the full re-serialize, and the TTL extension over the
largest possible value), **not** by the insertion index — `Vec::insert`'s
element shift runs host-native and is negligible beside the ledger read/write of
a 50-entry vector. The rows are therefore near-flat, and the thing CI must hold
is the bound (`MAX_FUND_QUEUE_LEN`), with the head-insert row as the (marginally)
worst admission. The behavioural counterpart — head/middle/tail insertion order,
the exact `QueueFull` admission boundary, the displacing-join authorization
requirement, and the `FundQueueOpenedAt` ledger boundary — lives in
`tests_storage.rs` (`test_1057_*`).

Run it with:

```bash
cd contracts/invoice_liquidity
cargo test --target x86_64-unknown-linux-gnu benchmark_worst_case_queue_insertion -- --nocapture
```

**Migration note:** measurement-only addition — no contract behavior, storage
layout, or baseline format changed. To record these as a tracked baseline, run
the command above and add the printed `BENCHMARK` values to
`contracts/invoice_liquidity/benchmarks/baseline.json` alongside the existing
entries.

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

## Oracle feeder-update authorization cost (Issue #1046)

Unlike the #1050/#1052/#1053 gates above — which live on `fund_invoice`'s read
path — this issue's control lives on the **oracle contract's write path**:
`update_verification`, the single entry point that can flip a payer to verified
and thereby open `fund_invoice`'s oracle gate. The ILN never calls it, so the
benchmark measures the oracle write path directly rather than a funding flow
(see
[`audit-evidence-remediation-map.md`](audit-evidence-remediation-map.md#13-authorize-oracle-feeder-updates-issue-1046)).

**Before:** `update_verification` had no authorization — a bare storage write.
Any caller could set any payer's verification status.

**After:** the write reads the persisted feeder address, calls
`feeder.require_auth()`, then writes. `benchmark_oracle_feeder_update_authorization`
measures both shapes side by side in one run (the two-entry oracle fixture
exposes the pre-#1046 unrestricted write and the #1046 authorized write), each
asserted against the same absolute ceilings as the rest of the file:

| Benchmark (`BENCHMARK` name) | Path measured |
| ---------------------------- | ------------- |
| `oracle_feeder_update_unrestricted` | pre-#1046: bare write, no auth (baseline) |
| `oracle_feeder_update_authorized` | post-#1046: feeder read + `require_auth()` + same write |

The delta between the two rows is the cost of closing the "anyone can mint
funding eligibility" hole; the absolute figure shows the authorized path still
fits easily inside the per-invocation budget. Authorization correctness itself is
covered under **real auth** (no `mock_all_auths`) by `oracle_integration_test.rs`
(configured-feeder success, no-feeder rejection, unauthorized-signer rejection,
feeder rotation) and on the funding path by the `test_1046_*` tests in
`tests_oracle_freshness.rs`.

**Migration note:** measurement-only addition — no `invoice_liquidity` behavior,
storage layout, or baseline format changed (the change is confined to the
external oracle contract). To record this as a tracked baseline, run the command
below and add the printed `BENCHMARK` values to
`contracts/invoice_liquidity/benchmarks/baseline.json` alongside the existing
entries.

## Multi-oracle Identity quorum cost (Issue #1047)

Unlike #1046 (which lives on the oracle contract's write path), this issue's
control lives on `fund_invoice`'s **read** path: instead of resolving one
Identity oracle, the gate queries every registered Identity feeder and requires
a configured quorum to agree. The cost of interest is therefore *per-feeder
cross-contract calls* on the funding flow.

**Before:** a single resolved Identity oracle — one cross-contract
`get_payer_data` call per funded invoice.

**After:** every registered feeder is queried via `try_invoke_contract`; the
`N`-of-`M` decision adds one call per feeder. With an **empty** list nothing
changes (the gate falls back to the single-oracle path, measured above).

`benchmark_identity_quorum_verification_cost` measures both shapes side by side
in one run (baseline single-oracle funding vs a three-feeder quorum of 3, all
agreeing), each asserted against the same absolute ceilings as the rest of the
file:

| Benchmark (`BENCHMARK` name) | Path measured |
| ---------------------------- | ------------- |
| `identity_single_oracle` | pre-#1047: one resolved Identity oracle |
| `identity_quorum_of_three` | post-#1047: three registered feeders, quorum 3 |

The delta is the price of no longer trusting a single feeder, and the absolute
figure shows even the three-feeder quorum fits comfortably inside the
per-invocation budget. Quorum correctness itself (agreement met / disagreement
fails closed / reverting feeder cannot force verification / ledger-boundary
staleness / disagreement event / auth) is covered by the `test_identity_quorum_*`,
`test_set_identity_quorum_*` and `test_add_identity_oracle_*` tests in
`tests_oracle_registry.rs`.

**Migration note:** the multi-oracle list is bounded by
`MAX_IDENTITY_ORACLES_PER_FEED` (20), so the worst-case funding cost is bounded
at 20 cross-contract calls. To record this as a tracked baseline, run the
command below and add the printed `BENCHMARK` values to
`contracts/invoice_liquidity/benchmarks/baseline.json` alongside the existing
entries.

## Circuit-breaker auto-recovery cost on the keeper path (Issue #1048)

The oracle circuit breaker trips after `MAX_CONSECUTIVE_STALE_QUERIES` (3)
consecutive stale responses and, before #1048, was **sticky** — cleared only by
the governance-gated `reset_oracle_circuit`, so a transient feeder outage jammed
oracle-gated funding even after the feeder recovered. Issue #1048 makes the
breaker **half-open**: the permissionless `check_oracle_health` keeper path
auto-clears it once the *tripping* oracle serves
`CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS` (3) consecutive healthy (fresh +
timestamp-advancing) observations spanning `CIRCUIT_RECOVERY_MIN_LEDGERS`
(120) — see
[`audit-evidence-remediation-map.md`](audit-evidence-remediation-map.md#15-recover-after-feeder-outage-issue-1048).

**Before:** a keeper `check_oracle_health` observation did no recovery work —
the breaker, once tripped, could not be affected by the keeper path at all.

**After:** while a breaker is tripped, each observation from the tripping oracle
costs one extra read + write of `CircuitRecoveryState`; the completing
observation removes the recovery record and the tripped flag and emits
`oracle_circuit_recovered`. `benchmark_oracle_circuit_auto_recovery` measures
three points in one run, each asserted against the same absolute ceilings as the
rest of the file:

| Benchmark (`BENCHMARK` name) | Path measured |
| ---------------------------- | ------------- |
| `oracle_check_health_closed` | breaker **closed** — pre-#1048 baseline, no recovery work |
| `oracle_circuit_recovery_streak` | breaker tripped, streak advances (extra recovery read + write) |
| `oracle_circuit_recovery_complete` | breaker tripped, completion observation clears it (two removes + recovery event) |

The small delta between the closed-breaker baseline and the tripped observations
is the price of un-jamming a recovered feeder without waiting on a governance
action; every figure sits far inside the per-invocation budget. Recovery
correctness itself (sustained-health success, insufficient window, insufficient
observation count, frozen oracle, mid-streak stale reset, different-oracle
isolation, view tracking, recovery event, re-trip, reset clearing progress) is
covered by the `test_circuit_auto_recovery_*` / `test_circuit_auto_recovers_*`
tests in `tests_oracle_registry.rs`.

## Verification diagnostic events cost (Issue #1049)

`fund_invoice`'s Identity gate reports stale data / feeder disagreement by
returning `Err`, and Soroban rolls every event of an `Err`-returning invocation
back — so those failure signals never reached a ledger. Issue #1049 exposes the
*same* gate through the permissionless, never-erroring
`check_oracle_verification`, whose `oracle_stale_data_rejected` (new) and
`oracle_feeder_disagreement` events commit — see
[`audit-evidence-remediation-map.md`](audit-evidence-remediation-map.md#16-emit-stale-and-quorum-failure-events-issue-1049).

**Before:** a rejected `fund_invoice` cost the same but left no observable
failure signal (its events rolled back).

**After:** the diagnostic adds the cost of running the gate plus the emitted
failure event. `benchmark_oracle_verification_diagnostic_events` measures three
points in one run, each asserted against the same absolute ceilings as the rest
of the file:

| Benchmark (`BENCHMARK` name) | Path measured |
| ---------------------------- | ------------- |
| `oracle_verification_verified` | passing verification — **no** failure event (pre-#1049 silent-success equivalent) |
| `oracle_verification_stale_event` | stale data → emits `oracle_stale_data_rejected` |
| `oracle_verification_quorum_disagreement_event` | quorum disagreement → emits `oracle_feeder_disagreement` |

The delta between the verified baseline and the two failure figures is the price
of making a rejected gate diagnosable. Correctness (success, stale event, quorum
event, the funding-path rollback vs. diagnostic commit, ledger boundary,
permissionless access, breaker interop) is covered by the
`test_check_oracle_verification_*` /
`test_fund_invoice_stale_rejection_events_roll_back_but_diagnostic_commits` tests
in `tests_oracle_registry.rs`.

## Compromised-feeder rejection-path cost (Issue #1054)

The adversary behind the Identity defences is a **compromised feeder**: an oracle
that is still an *authorized* registry entry but whose `get_payer_data` payload is
attacker-controlled. Issue #1054 adds the focused adversarial suite that pins each
defence against a hostile payload (see
[`audit-evidence-remediation-map.md`](audit-evidence-remediation-map.md#17-test-compromised-feeder-data-issue-1054))
and this benchmark measures the cost of every rejection branch.

**Before:** the individual defences existed but there was no measured figure for
rejecting a hostile payload, and the #1052/#1053 adversarial tests lived in the
undeclared `tests_oracle_freshness.rs` module (never compiled).

**After:** each hostile payload is rejected at a distinct, named branch of the
shared gate. Because a rejected `fund_invoice` rolls its events back, the
benchmark drives the **permissionless, never-erroring** `check_oracle_verification`
diagnostic (#1049) so each outcome is directly assertable and nothing rolls back.
`benchmark_compromised_feeder_rejection_paths` measures one row per branch in one
run, each asserted against the same absolute ceilings as the rest of the file:

| Benchmark (`BENCHMARK` name) | Outcome asserted | Path measured |
| ---------------------------- | ---------------- | ------------- |
| `compromised_feeder_baseline_verified` | `Verified` | honest, fresh, verified feeder (baseline) |
| `compromised_feeder_stale_replay` | `Stale` | replayed attestation older than the window |
| `compromised_feeder_future_timestamp` | `Stale` | future-dated (clock-skewed) timestamp |
| `compromised_feeder_timestamp_regression` | `TimestampRegression` | same oracle rolls its timestamp back |
| `compromised_feeder_frozen_snapshot` | `DuplicateUpdate` | identical timestamp past the duplicate cap |
| `compromised_feeder_quorum_disagreement` | `QuorumDisagreement` | compromised minority cannot reach the quorum |

Each hostile figure should sit close to the honest baseline (the rejection branches
are comparisons + one failure event on the same snapshot the passing path already
reads), and every figure stays far inside the per-invocation budget. Rejection
correctness itself is covered by the `test_1054_*` suite in
`tests_oracle_registry.rs`.

**Migration note:** measurement-only addition — no contract behavior, storage
layout, or baseline format changed. To record this as a tracked baseline, run the
command below and add the printed `BENCHMARK` values to
`contracts/invoice_liquidity/benchmarks/baseline.json` alongside the existing
entries.

## Oracle health / quorum exposure cost (Issue #1055)

Answering "is oracle gating for this feed + token healthy?" used to mean fanning
out across five read entry points, and the live health of an Identity quorum had
no reader at all (`check_identity_quorum` never writes the single-oracle
`OracleHealth` key, so `get_oracle_health` is `None` forever in quorum mode).
Issue #1055 adds two permissionless views — `get_oracle_status` (an aggregate of
recorded state) and `check_identity_quorum_health` (live per-feeder health +
quorum verdict) — see
[`audit-evidence-remediation-map.md`](audit-evidence-remediation-map.md#18-expose-oracle-health-and-quorum-issue-1055).

**Before:** no single figure for either a configuration/health probe or a live
quorum probe existed, and the quorum path was unobservable.

**After:** `benchmark_oracle_status_and_quorum_exposure` measures both views in
one run, each asserted against the same absolute ceilings as the rest of the
file:

| Benchmark (`BENCHMARK` name) | View | State measured |
| ---------------------------- | ---- | -------------- |
| `oracle_status_view_unconfigured` | `get_oracle_status` | nothing registered (all-empty view) |
| `oracle_status_view_healthy` | `get_oracle_status` | one oracle, recorded health (breaker closed) |
| `oracle_status_view_tripped_recovery` | `get_oracle_status` | breaker open + half-open recovery bookkeeping |
| `identity_quorum_health_fanout_1` | `check_identity_quorum_health` | 1 registered feeder, all verified |
| `identity_quorum_health_fanout_5` | `check_identity_quorum_health` | 5 feeders, all verified |
| `identity_quorum_health_fanout_20` | `check_identity_quorum_health` | `MAX_IDENTITY_ORACLES_PER_FEED` feeders, all verified |

`get_oracle_status` performs no cross-contract calls and writes nothing, so its
three rows are cheap and flat regardless of registration state — suitable for a
high-frequency `/health` probe. `check_identity_quorum_health`'s cost grows with
the feeder list, but the list is capped at `MAX_IDENTITY_ORACLES_PER_FEED` (20),
so its worst case is a fixed, pre-bounded figure; the 1 / 5 / 20 rows make that
gradient explicit, and every row stays inside the per-invocation budget. View
correctness is covered by the `test_1055_*` suite in `tests_oracle_registry.rs`.

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
