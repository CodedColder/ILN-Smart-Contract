# Audit Evidence → Remediation Control Map

Issues #1094, #1095, #1096, #1097, #1025 (wave-9).

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
| C5 | `invoice.rs:853-881`; `lib.rs:2028-2045,2060-2101`; `storage.rs:228-239` (Issue #1025) | `invoice.rs` queue helpers omitted `extend_ttl`, risking mid-flight expiration and pre-upgrade rejection; `join_fund_queue` / `resolve_fund_queue` lacked `is_paused` checks; extension cost at max depth (`MAX_FUND_QUEUE_LEN` = 50) was unbenchmarked | Extended TTL on `FundQueue`, `QueueResolution`, and `FundQueueOpenedAt` via `FUND_QUEUE_TTL_*` (1M/2M); added fail-closed `is_paused` guard to queue entry points; added max-depth benchmarks and baseline metrics | Paused contract rejects queue ops (`ContractPaused`); queue storage guaranteed >= threshold before upgrades | `c5_*`, `tests_max_depth_extension.rs` |

Authorization: no new entry points. `add_token` keeps `require_admin` as its first
check; `join_fund_queue` keeps `require_lp` (LP auth). No path gained a bypass.

## 2. Before / after (measured)

Same `audit_regression_test` suite, run against `origin/dev`'s
`lib.rs` and the fixed `lib.rs`:

| | `origin/dev` | This PR |
|---|---|---|
| Tests passing | 10 / 12 | 12 / 12 (plus 6 in `tests_max_depth_extension`) |
| Failing tests | Missing C5 guards (unextended queue TTL, unpaused queue operations) | none |
| Max fund-queue length reachable | `MAX_FUND_QUEUE_LEN` = 50 | `MAX_FUND_QUEUE_LEN` = 50 |
| Queue storage TTL | Unextended in `invoice.rs` (risks expiration / pre-upgrade failure) | Extended to 2,000,000 ledgers on every write |
| Emergency pause protection | Bypassed by `join_fund_queue` and `resolve_fund_queue` | Fail-closed (`ContractPaused`) |
| Benchmark at max depth | Unmeasured | Pinned: join=1124580 cpu, resolve=582140 cpu, storage=321850 cpu |
| Missing token decimals | `TokenDecimalsNotConfigured` (from #1070; guarded by `c1_*`) | unchanged |

(The baseline passes are guards that must keep passing: decimals registered,
missing decimals fail closed, unlisted token still `Unauthorized`, normal
submit works, exactly-50 joins accepted, a full queue still resolves.)

## 3. Rollout, recovery and upgrade

**Storage / ABI.** No storage layout change and no new error codes. The queue
cap re-uses the existing `FundQueue`, `FundQueueEntryDisplaced` and `QueueFull`
definitions. Queue keys are explicitly extended to 1,000,000 threshold / 2,000,000 extend-to,
aligning with the pre-upgrade TTL guard in `storage::validate_ttl_fields_pre_upgrade`.

**Queues already above 50.** Existing oversized queues are not truncated.
New joins to such a queue are rejected unless they outscore the tail (then the
tail is displaced and the length stays the same). Queues drain as invoices
resolve, are funded or are cancelled.

**Recovery / rollback.** Reverting the Wasm restores prior behaviour with no
data migration, because no state was rewritten.

**Monitoring.** Alert on `fund_queue_displaced` (queue pressure / griefing
signal), `QueueFull` error rates, and monitor `ContractPaused` rejections on queue entry points.

## 4. Known limits (not addressed here)

- `cross_contract_invariants_test.rs` aborts in `Harness::new`
  (`WasmVm InvalidAction`) on the base branch, so X1–X5 do not run today. X6/X7
  coverage therefore lives in the self-contained `audit_regression_test.rs`.
- At the time of this PR `origin/dev` does not build `insurance_pool` /
  `iln_distribution` (duplicate `DataKey` variants; `initialize` return type),
  which blocks every `invoice_liquidity` integration target. Out of scope.
