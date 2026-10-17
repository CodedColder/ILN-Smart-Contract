# Audit Evidence → Remediation Control Map

Issues #1094, #1095, #1096, #1097 (wave-9, `formal`).

Baseline evidence:

- [`formal-verification.md`](formal-verification.md)
- [`formal-verification-cross-contract.md`](formal-verification-cross-contract.md)
- `contracts/tests/cross_contract_invariants_test.rs`

Regression harness: `contracts/tests/audit_regression_test.rs`
(`cargo test -p invoice_liquidity --test audit_regression_test`).

## 1. Control map

| ID | Audit evidence | Gap found at baseline | Control (code) | Fail-closed behaviour | Test(s) |
|----|----------------|-----------------------|----------------|-----------------------|---------|
| C1 | Cross-contract spec X6: token precision must be known wherever amounts are compared | `validate_invoice_terms_with_token` used `.unwrap_or(6)`; `initialize` allowlisted EURC without decimals | **Landed upstream in #1070** (`token_safety.rs`, `TokenDecimalsNotConfigured`, EURC=6 at `initialize`, `migrate` backfill). This PR adds regression guards only | `submit_invoice` reverts, no invoice id consumed | `c1_*` |
| C2 | Same | `add_token` trusted the admin-supplied `decimals` | **Landed upstream in #1070** (declared decimals must equal the token's `decimals()` and be <= 18, `InvalidTokenDecimals`). Covered by `tests_token_safety.rs` | Admin tx reverts | (upstream) |
| C3 | Cross-contract spec X7 / formal-verification §5 (bounded state) | `MAX_FUND_QUEUE_LEN`, `QueueFull`, `FundQueueEntryDisplaced` were declared but **never enforced**: `join_fund_queue` grew `FundQueue(invoice_id)` without bound (unbounded storage + O(n) duplicate scan and insert) | `join_fund_queue`: at capacity, joiner must strictly outscore the tail, which is displaced (`fund_queue_displaced` event); else `QueueFull` | Equal/lower score join reverts; queue length never exceeds 50 | `c3_*` |
| C4 | `cross_contract_invariants_test.rs` X1–X5 | Harness is the only cross-contract regression; it needs wired distribution + insurance | Left unchanged (see §4) | n/a | existing `x1`–`x5` |

Authorization: no new entry points. `add_token` keeps `require_admin` as its first
check; `join_fund_queue` keeps `require_lp` (LP auth). No path gained a bypass.

## 2. Before / after (measured)

Same `audit_regression_test` suite (10 tests), run against `origin/dev`'s
`lib.rs` and the fixed `lib.rs`:

| | `origin/dev` | This PR |
|---|---|---|
| Tests passing | 6 / 10 | 10 / 10 |
| Failing tests | 4 × `c3_*` (equal-score, lower-score, higher-score displacement, displaced-LP rejoin) | none |
| Max fund-queue length reachable | unbounded | `MAX_FUND_QUEUE_LEN` = 50 |
| Missing token decimals | `TokenDecimalsNotConfigured` (from #1070; guarded by `c1_*`) | unchanged |

(The 6 baseline passes are guards that must keep passing: decimals registered,
missing decimals fail closed, unlisted token still `Unauthorized`, normal
submit works, exactly-50 joins accepted, a full queue still resolves.)

## 3. Rollout, recovery and upgrade

**Storage / ABI.** No storage layout change and no new error codes. The queue
cap re-uses the existing `FundQueue`, `FundQueueEntryDisplaced` and `QueueFull`
definitions. Decimals rollout/migration is documented in
[`token-safety.md`](token-safety.md) (#1070).

**Queues already above 50.** Existing oversized queues are not truncated.
New joins to such a queue are rejected unless they outscore the tail (then the
tail is displaced and the length stays the same). Queues drain as invoices
resolve, are funded or are cancelled.

**Recovery / rollback.** Reverting the Wasm restores prior behaviour with no
data migration, because no state was rewritten.

**Monitoring.** Alert on `fund_queue_displaced` (queue pressure / griefing
signal) and on `QueueFull` error rates.

## 4. Known limits (not addressed here)

- `cross_contract_invariants_test.rs` aborts in `Harness::new`
  (`WasmVm InvalidAction`) on the base branch, so X1–X5 do not run today. X6/X7
  coverage therefore lives in the self-contained `audit_regression_test.rs`.
- At the time of this PR `origin/dev` does not build `insurance_pool` /
  `iln_distribution` (duplicate `DataKey` variants; `initialize` return type),
  which blocks every `invoice_liquidity` integration target. Out of scope.

## 5. Maximum fund-queue TTL cost (Issue #1062)

Follow-up to control C3 above: the queue bound (`MAX_FUND_QUEUE_LEN` = 50) was
enforced, but the *retention* policy that keeps a bounded queue alive
(Issue #1011's `extend_queue_key_ttl`) was only ever tested at single-entry
scale, and the cost benchmarks measured only single-invoice flows.

| | Baseline (`origin/dev`) | This PR |
|---|---|---|
| Retention asserted at `MAX_FUND_QUEUE_LEN` | no | `tests_storage.rs` success/failure/auth/boundary tests |
| Max-size admission/resolution/TTL cost measured | no | `tests_benchmarks.rs` `*_at_max_queue_size` benchmarks |
| Isolated `extend_ttl` cost vs. queue length | unmeasured | printed table across 1 / 10 / 50 entries |

**Behaviour.** Unchanged — `extend_ttl` is key-based, so a maximum-size queue
is retained for exactly as long as a single-entry one. The addition is
measurement + regression coverage only. No storage layout, key format, TTL
constant, or error code changed, so no data migration is required (see
[`storage-layout.md`](storage-layout.md) and
[`benchmarks.md`](benchmarks.md) for the before/after detail).

## 6. Concurrent invoice settlement (Issue #1063)

Follow-up to control C1 above: settlement (`mark_paid`) mutates *shared* state
(the reentrancy lock, `StorageKey::TotalPaid`, and the `ContractStats`
aggregate) while per-invoice data stays independent — but settlement was only
ever exercised one invoice at a time, so the batched case (independent payers
settling in one ledger) had no isolation or accounting coverage, and no
per-invocation cost measurement inside such a batch.

| | Baseline (`origin/dev`) | This PR |
|---|---|---|
| Batch of settlements in one ledger asserted | no | `tests_storage.rs` success / boundary tests (`SETTLE_STRESS_BATCH` = 25) |
| Shared-counter advancement under batching | untested | exact-count assertions on `total_paid` / `total_invoices` / per-payer `invoices_paid` |
| Rejected settlement leaving others settleable | untested | double-pay / overpayment / unfunded rejection-isolation tests |
| `mark_paid` authorization + `paid_on_time` boundary | single-invoice only | explicit auth test and exact `timestamp == due_date` flip test |
| Per-invocation settlement cost inside a batch | unmeasured | `tests_benchmarks.rs` `settle_batch_same_ledger` + `mark_paid_in_batch_len_{1,10,25}` table |

**Behaviour.** Unchanged — settlement is already per-invoice isolated in
accounting; the addition is regression coverage + cost measurement only. No
storage layout, key format, error code, or counting semantics changed, so no
data migration is required (see [`storage-layout.md`](storage-layout.md) and
[`benchmarks.md`](benchmarks.md) for the before/after detail).

## 7. Bounded price-source list / event fan-out (Issue #1064)

The per-feed price-source list (`DataKey::PriceSources`) was appended to by
`add_price_source` with no upper bound, and `get_verified_price` iterates the
whole list on every price read, publishing one `PriceOutlierRejected` event per
source that deviates past the threshold. An unbounded list therefore made both
the event payload a single price read could emit and the CPU/storage cost of the
iteration unbounded — growth to the point of a resource-limit failure would break
the price path for every token on that feed.

| | Baseline (`origin/dev`) | This PR |
|---|---|---|
| Per-feed source count bound | none | `MAX_PRICE_SOURCES_PER_FEED` (20), enforced by `add_price_source` |
| Overflow behaviour | appended indefinitely | `ContractError::TooManyPriceSources` (76) for a new source once full |
| Idempotent re-registration at the cap | n/a | still a no-op (cap check only applies to a genuinely new source) |
| Slot recovery | n/a | `remove_price_source` frees a slot |
| Single read's outlier-event count | unbounded | bounded by the cap; asserted in `tests_storage.rs` |
| Price-read cost / fan-out measured | unmeasured | `tests_benchmarks.rs` `get_verified_price_sources_{1,10,20}` + `get_verified_price_outlier_fan_out_at_max_sources` |

**Behaviour.** `add_price_source` gains an upper bound (`TooManyPriceSources`,
error 76) and a new constant `MAX_PRICE_SOURCES_PER_FEED = 20`; the stored value
stays the same `Vec<Address>` and all reads outside a full feed are unchanged. No
storage layout, key format, or TTL constant changed, and an already-over-cap feed
is not truncated (the cap gates only further additions), so no data migration is
required (see [`storage-layout.md`](storage-layout.md) and
[`benchmarks.md`](benchmarks.md) for the before/after detail).

## 8. Safe resource envelope (Issue #1065)

The contract only stays inside Soroban's per-invocation CPU/memory budget if every
on-chain collection it iterates or rewrites is bounded. Those bounds existed but
were enforced (and discoverable) one module at a time, with no single artifact
declaring the envelope — so a newly added unbounded-by-design collection would not
stand out, and no figure tied the declared bounds to a measured read cost.

| | Baseline (`origin/dev`) | This PR |
|---|---|---|
| Envelope declaration | implicit, spread across `constants.rs` / `access.rs` / `twap.rs` | `storage::RESOURCE_ENVELOPE` — one `(name, cap)` table, caps referencing the enforcing constants |
| Drift protection | none | `tests_storage.rs` asserts each declared cap equals the constant that enforces it, plus well-formedness (non-empty, unique, positive) |
| Live boundary coverage | none | fund queue filled to its declared cap, ledger boundary crossed, one further join rejected (`QueueFull`), length stays at cap |
| Authorization coverage | none | admin with `mock_all_auths()` still cannot exceed the price-source cap (`TooManyPriceSources`) |
| Full-collection **read** cost measured | unmeasured | `tests_benchmarks.rs` `benchmark_envelope_read_views_at_cap` (`envelope_read_{price_sources,fund_queue,admin_action_log,top_payers}_at_cap`), each budget-checked |

**Behaviour.** No contract behavior, storage key, value encoding, or TTL constant
changed — this is a declaration-plus-measurement remediation. `RESOURCE_ENVELOPE`
is a `pub const` slice read only by tests and docs, so no migration is required
(see [`storage-layout.md`](storage-layout.md) and [`benchmarks.md`](benchmarks.md)
for the before/after detail).
