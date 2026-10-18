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

## 9. Oracle freshness across ledger boundaries (Issue #1050)

The ledger-based staleness gate in `fund_invoice` computed age with a *saturating*
subtraction: `age = env.ledger().sequence().saturating_sub(response.timestamp)`,
then rejected when `age >= max_oracle_age_ledgers`. Because the subtraction
saturates at zero, any `response.timestamp` **greater than** the current ledger
sequence collapses to `age == 0` — the freshest possible reading. An oracle (or a
buggy one) reporting a future ledger sequence therefore made its data look
maximally fresh on *every* ledger until the chain caught up with the fake
timestamp, defeating the freshness bound the gate exists to enforce. The
`record_oracle_health` snapshot used the same saturating subtraction, so the
recorded health state agreed with the (wrong) funding decision.

| | Baseline (`origin/dev`) | This PR |
|---|---|---|
| Freshness rule owner | duplicated inline in `fund_invoice`; same saturating formula re-derived in `record_oracle_health` | one shared `oracle_registry::is_oracle_data_stale(current_ledger, data_timestamp, max_age_ledgers)` |
| Future-dated `response.timestamp` (`> current_ledger`) | `age` saturates to `0` → treated as **fresh** → funding allowed | treated as **stale** → `ContractError::OracleDataStale` (34) |
| Future region bounded | no — up to `u32::MAX` read as age 0 | yes — the bound now holds across the full `u32` ledger range |
| `max_oracle_age_ledgers == 0` escape hatch | disables the check | unchanged — still disables the check (short-circuit before the future-region test) |
| Gate ⇄ health-snapshot agreement | same formula, so consistently wrong on future data | both call the shared rule, so the funding decision and the recorded health snapshot cannot disagree |
| Boundary coverage | interior scenarios only | `tests_oracle_freshness.rs` Tests 13–21: last-fresh / first-stale / one-past, one-ledger walk across the boundary, `u32` range ends, escape hatch applied to futures, future-dated data end-to-end (funded-path → `OracleDataStale`; `verify=false` path → funds), health snapshot at the future boundary, plus two proptests (reference-formula equivalence; monotonicity as the ledger advances) |

**Behaviour.** The freshness predicate is extracted to
`oracle_registry::is_oracle_data_stale`, which returns `false` immediately when
`max_age_ledgers == 0` (the governance escape hatch), `true` when
`data_timestamp > current_ledger` (the future region), and otherwise
`(current_ledger - data_timestamp) >= max_age_ledgers`. `fund_invoice`'s
`require_oracle_verification` block and `record_oracle_health` both call it, so a
single rule drives both the funding decision and the recorded health snapshot. All
interior behaviour (age `<` max → fresh; age `>=` max → stale) is unchanged; only
the previously-unbounded future region moves from *fresh* to *stale*. No storage
key, value encoding, or TTL constant changed, so no data migration is required
(see [`threat-model.md`](threat-model.md), [`error-codes.md`](error-codes.md) and
[`oracle-attack-economics.md`](oracle-attack-economics.md) for the before/after
detail).

## 10. Missing decimals weakening precision (Issue #1051)

Follow-up to control C1 above. #1070 made the amount floor fail closed by
resolving decimals from the token (`token_safety::registered_token_decimals`)
instead of a silent `.unwrap_or(6)`. But a **second, decimals-blind** validator
survived alongside it: `validate_invoice_terms` compared every amount against a
hardcoded 6-decimal floor (`min_amount = 1_000_000`) and still ran *before* the
token-aware check at three entry points. Because the token-aware check happened
to run afterwards it masked the weaker floor — but the amount was still being
compared against an assumed scale, so any reordering, early return, or future
call-site that kept the blind check and dropped the token-aware one would
under-enforce the floor: for a 7-decimal asset (XLM) the legacy floor
(1_000_000) is 10× smaller than the correct one (10_000_000).

| | Baseline (`origin/dev`) | This PR |
|---|---|---|
| Amount-floor entry points | two — decimals-blind `validate_invoice_terms` (hardcoded 6) *and* token-aware `validate_invoice_terms_with_token` | one — `validate_invoice_terms_with_token` only |
| `submit_invoice` | blind check, then allowlist, then token-aware check | allowlist, then token-aware check |
| `update_invoice` | blind check, then token-aware check | token-aware check |
| `submit_invoices_batch` | blind check, then allowlist, then token-aware check | allowlist, then token-aware check |
| Non-6-decimal asset below its own floor | accepted by the blind check (blocked only by the later token-aware check) | rejected directly with `InvalidAmount` |
| Missing decimals | `TokenDecimalsNotConfigured` from the token-aware check | unchanged — the *only* check fails closed |
| Unlisted token | `Unauthorized` | unchanged — the allowlist check runs first, so no decimals/amount error leaks |
| Regression coverage | `c1_*` on submit only | `c1_*` on submit **and** update, plus `tests_min_invoice_amount.rs` `test_issue_1051_*` for submit/update/batch success, failure, authorization precedence and fail-closed |

**Behaviour.** `validate_invoice_terms` is removed; `is_approved_token` runs
before the amount check at `submit_invoice` and `submit_invoices_batch`
(preserving `Unauthorized` for unlisted tokens), and every entry point now
validates solely through `validate_invoice_terms_with_token`, which resolves the
token's registered decimals and fails closed with `TokenDecimalsNotConfigured`
when they are absent. Valid submissions, the 6-decimal floor (USDC: 1_000_000)
and the 7-decimal floor (XLM: 10_000_000) are unchanged; only the previously
reachable decimals-blind comparison is gone. No storage key, value encoding,
error code, or TTL constant changed, so no data migration is required (see
[`formal-verification-cross-contract.md`](formal-verification-cross-contract.md)
§7, [`token-safety.md`](token-safety.md) and [`multi-token.md`](multi-token.md)
for the before/after detail).

## 11. Monotonic oracle timestamp checks (Issue #1052)

Companion to the freshness control in §9 (#1050). #1050 bounded *how old* an
oracle response may be (`[current_ledger - max_age, current_ledger]`), but said
nothing about *direction*: an oracle could re-serve a response it had already
served before — replayed data, or data from a rolled-back internal clock — and
as long as it was still inside the freshness window the gate accepted it. A
replayed response can mean a payer that was verified at ledger L is re-attested
at L again later (so a previously-fresh verification is presented as if it were
current) and, more generally, that the oracle's reported timeline is not a
reliable monotonic progress indicator.

| | Baseline (`origin/dev`) | This PR |
|---|---|---|
| Direction check | none — any timestamp inside the freshness window accepted, including one *older* than the last recorded for the same oracle | a timestamp strictly older than the last recorded for the same `(feed type, token, oracle)` is rejected with `OracleTimestampRegression` |
| Baseline source | — | the per-`(feed type, token)` `OracleHealthStatus.last_data_timestamp` snapshot already maintained by `record_oracle_health` |
| Accepted-timestamp envelope | `[current_ledger - max_age, current_ledger]` | `[last_recorded, current_ledger]` |
| Equal timestamp re-read | accepted | accepted (rule is *non-decreasing*, not strictly increasing) |
| First read (no snapshot) | accepted | accepted — monotonicity is defined relative to a recorded baseline |
| Oracle swap / failover | n/a | not compared: the predicate only fires for the **same** oracle address, so a failover oracle starting on an older timeline is not a false positive |
| Interaction with #1050 | future-dated data already stale | unchanged — the regression gate runs first, then the shared freshness predicate |
| Regression coverage | — | `tests_oracle_freshness.rs` `test_1052_*` (first read, equal re-read, regression rejected, gate-requires-verification, ledger lower-bound `0`, different-oracle non-regression) plus `prop_1052_regression_iff_strictly_older_for_same_oracle` |

**Behaviour.** A new shared predicate,
`oracle_registry::is_oracle_data_timestamp_regression(env, feed_type, token,
oracle, data_timestamp)`, returns `true` iff a snapshot exists for that
`(feed type, token)`, it was recorded for the **same** oracle address, and the
new `data_timestamp` is strictly smaller than the snapshot's
`last_data_timestamp`; it returns `false` when no snapshot exists or the oracle
address differs. `fund_invoice`'s `require_oracle_verification` block calls it
**before** `record_oracle_health` (so the previous snapshot is read before being
overwritten) and returns `ContractError::OracleTimestampRegression` on a
violation. Because a rejected `fund_invoice` rolls back all of its storage
writes, the baseline can never be *lowered* through funding; `check_oracle_health`
still records every observed snapshot as-is (including a regression) so
monitoring sees the anomaly. Valid responses, the freshness rule, and the
`max_oracle_age_ledgers == 0` escape hatch are unchanged; the only new behaviour
is that a strictly-older same-oracle timestamp is now rejected. No storage key,
value encoding, or TTL constant changed, so no data migration is required (see
[`oracle-design.md`](oracle-design.md),
[`oracle-attack-economics.md`](oracle-attack-economics.md),
[`threat-model.md`](threat-model.md) and [`error-codes.md`](error-codes.md) for
the before/after detail).

## 12. Bound duplicate update frequency (Issue #1053)

Companion to the monotonicity control in §11 (#1052). #1052 forbade a
timestamp going *backwards* but deliberately allowed an *equal* one — the rule
is *non-decreasing*, not strictly increasing — precisely so a legitimate
re-read of unchanged data is not treated as a replayed response. That left the
opposite hole: an oracle that simply **stands still** produces an identical
timestamp on every read, which is always inside the freshness window (§9,
#1050) and always *non-stale*, so `record_oracle_health` keeps resetting the
consecutive-stale streak and the circuit breaker never trips. A stalled or
compromised oracle could therefore keep oracle-gated funding open — standing in
for live verification — indefinitely, at no cost, just by not advancing.

| | Baseline (`origin/dev`) | This PR |
|---|---|---|
| Duplicate (unchanged) timestamp | accepted for as long as the value stays inside the freshness window; each read resets the stale streak | the **same** oracle may serve an identical timestamp at most `MAX_CONSECUTIVE_DUPLICATE_UPDATES` (3) consecutive times; the next duplicate is rejected with `OracleDuplicateUpdate` |
| Duplicate counter | none recorded | `OracleHealthStatus.consecutive_duplicate_count`, exposed on the `oracle_health_recorded` event |
| Reset condition | n/a | the moment the timestamp **advances**, or resolution fails over to a **different** oracle — the streak is per `(feed type, token, oracle)` |
| Interaction with #1052 | equal timestamps always accepted | unchanged — a duplicate is only counted when it is *not* a regression; the regression gate still runs first |
| Interaction with #1050 | a re-served value eventually ages out of the window | unchanged — the freshness rule still applies; the duplicate bound additionally rejects a value that stays frozen *while still fresh* |
| Writes back | n/a | a rejected `fund_invoice` rolls its health write back, so a rejected duplicate never advances the streak — the gate stays shut until the oracle actually advances (no "reset by retrying") |
| Monitoring | — | `check_oracle_health` (never errors) records every duplicate as-is, so keepers can watch the streak build before funding starts failing |
| Coverage | — | `tests_oracle_freshness.rs` `test_1053_*` (up-to-bound accepted, over-bound rejected, boundary at `cap - 1` / `cap`, advance resets, gate-requires-verification, different-oracle non-duplicate) plus `prop_1053_duplicate_predicate_matches_streak_bound` |

**Behaviour.** A new shared predicate,
`oracle_registry::is_oracle_data_duplicate_update(env, feed_type, token, oracle,
data_timestamp)`, returns `true` iff a snapshot exists for that
`(feed type, token)`, it was recorded for the **same** oracle address, the new
`data_timestamp` **equals** the snapshot's `last_data_timestamp`, and the
snapshot's `consecutive_duplicate_count` has already reached
`MAX_CONSECUTIVE_DUPLICATE_UPDATES` (3, mirroring `MAX_CONSECUTIVE_STALE_QUERIES`).
`fund_invoice`'s `require_oracle_verification` block calls it immediately after
the #1052 regression check and **before** `record_oracle_health` (so the previous
snapshot — including its streak — is read before being overwritten), returning
`ContractError::OracleDuplicateUpdate` on a violation. `record_oracle_health`
maintains the streak: an identical `data_timestamp` from the same oracle
increments it, while the first read, any advance, or any oracle change resets it
to `0`; the value is persisted on `OracleHealthStatus` and mirrored on the
`oracle_health_recorded` event. Valid responses, the freshness rule, the
monotonicity rule, and the `max_oracle_age_ledgers == 0` escape hatch are
unchanged; the only new behaviour is that a same-oracle response repeating an
identical timestamp more than the cap admits is now rejected. The added state is
a single bounded scalar on an existing record — no new storage key or collection
(so it does not affect the [`storage.rs` `RESOURCE_ENVELOPE`](../contracts/invoice_liquidity/src/storage.rs)
bounds) and no value-encoding migration is required (see
[`oracle-design.md`](oracle-design.md),
[`oracle-attack-economics.md`](oracle-attack-economics.md),
[`threat-model.md`](threat-model.md) and [`error-codes.md`](error-codes.md) for
the before/after detail).

## 13. Authorize oracle feeder updates (Issue #1046)

The payer-verification oracle exposes a **write** path, `update_verification`,
whose only purpose is to flip a payer from unverified to verified. When the ILN
is configured with `require_oracle_verification = true`, that flip is precisely
what unlocks `fund_invoice`. The requirement to authorize this entry point was
*documented* — on the `OracleInterface` trait
([`oracle_interface.rs`](../contracts/invoice_liquidity/src/oracle_interface.rs))
and in the operator guide — but nothing *enforced* it: the reference oracle
shipped an unrestricted `update_verification`, so any caller could mint funding
eligibility for any payer. The oracle gate is only as strong as this one entry
point, and an unrestricted entry point reduces it to fail-open.

| | Baseline (`origin/dev`) | This PR |
|---|---|---|
| `update_verification` authorization | documented, **not enforced** — any caller could write any payer's status | requires the configured feeder's `feeder.require_auth()`; an unauthorized signer is rejected and the write does not land |
| No feeder configured | unrestricted write still succeeded | call rejected (`feeder not configured`); fail closed, never "anyone may write" |
| Feeder configurability / rotation | n/a — no feeder concept | persisted feeder address; re-pointing it immediately revokes the previous feeder |
| Funding impact (`require_oracle_verification = true`) | anyone could open the funding gate for any payer | only the configured feeder can |
| Interface contract | trait docs described the requirement loosely | trait docs state the MUST contract (set feeder → `require_auth()` → reject-if-unset); enforced by the reference implementation |
| Regression coverage | none | `oracle_integration_test.rs`: success, unconfigured, unauthorized-signer under **real auth** (`setup_real_auth`, no `mock_all_auths`), feeder rotation; `tests_oracle_freshness.rs` `test_1046_*`: feeder-authorized update opens the gate, gate only applies when oracle required, freshness ageing + boundary walk of feeder data, feeder revocation |
| Cost measured | unmeasured | `tests_benchmarks.rs` `benchmark_oracle_feeder_update_authorization` — unrestricted vs feeder-authorized write table, both budget-checked |

**Behaviour.** The ILN contract is **unchanged**: it never calls
`update_verification`; the authorization is enforced by the oracle contract the
registry points at. The reference oracle (`contracts/tests/mocks/mock_oracle.rs`)
gains a persisted feeder (`set_feeder` / `get_feeder`) and now requires
`feeder.require_auth()` on `update_verification`, rejecting the call when no
feeder is set. No `invoice_liquidity` storage key, value encoding, error code, or
TTL constant changed, so **no data migration is required** for the ILN contract.
Oracle operators upgrading to a conforming implementation must configure their
feeder at deployment — see
[`oracle-integration.md`](oracle-integration.md#authorizing-the-feeder-issue-1046)
and [`upgrade-guide.md`](upgrade-guide.md#oracle-contracts-feeder-authorization-issue-1046).

## 14. Handle feeder disagreement safely (Issue #1047)

`fund_invoice`'s oracle-gated path (the `require_oracle_verification` block)
resolved **one** Identity oracle — `oracle_registry::resolve_oracle_for_verification`,
via the per-token override → feed-type default → legacy `Config.price_oracle`
chain — and trusted that oracle's boolean outright. That is a single point of
both trust and failure: one mistaken, offline, or compromised feeder decides
funding eligibility for every payer, and there is no on-chain way to observe
that feeders *disagree* — the "multiple redundant oracles" recommendation in
[`oracle-attack-economics.md`](oracle-attack-economics.md) was unimplemented for
the Identity feed.

| | Baseline (`origin/dev`) | This PR |
|---|---|---|
| Identity resolution | one resolved oracle, its boolean trusted outright | governance may register up to `MAX_IDENTITY_ORACLES_PER_FEED` (20) Identity feeders; `fund_invoice` queries **all** of them |
| Decision rule | that one oracle's `is_verified` + freshness | `N`-of-`M` agreement (`N` = `get_identity_quorum()`, default 1); only responses that are `is_verified` **and** fresh count |
| Feeder disagreement | neither consulted nor observable | fails **closed** → `ContractError::PayerUnverified`, and an `oracle_feeder_disagreement` event is emitted with `verified_count` / `required_quorum` / `total_oracles` |
| Reverting / stale / unverified feeder | n/a (there is only one) | degrades to "no opinion" via the non-panicking `try_invoke_contract` (mirroring `query_price`) — it can never *force* verification through |
| Backward compatibility | — | empty list → `NotConfigured` → the **unchanged** single-oracle path (regression #1052, duplicate #1053, freshness #1050, circuit-breaker checks all intact) |
| Interface | `OracleInterface` v1 | **unchanged** — each feeder is an independent deployment of the same `get_payer_data`; no ABI or interface-version bump |
| Governance surface | `register_oracle` / `register_token_oracle` | `add_identity_oracle` / `remove_identity_oracle` / `set_identity_quorum` (admin, rate-limited), `get_identity_oracles` / `get_identity_quorum` (anyone) |
| Bounded collection | — | `MAX_IDENTITY_ORACLES_PER_FEED = 20`, declared in `RESOURCE_ENVELOPE` (`identity_oracles_per_feed`) with a drift test; adding beyond it → `TooManyIdentityOracles` (79); `set_identity_quorum` outside `[1, 20]` → `InvalidIdentityQuorum` (80) |
| Regression coverage | none | `tests_oracle_registry.rs`: quorum met (single + subset-of-three), disagreement fails closed, reverting feeder cannot force verification, ledger-boundary excludes a stale feeder, disagreement event, empty-list single-oracle fallback, cap + idempotency, unauthorized callers |
| Cost measured | unmeasured | `tests_benchmarks.rs` `benchmark_identity_quorum_verification_cost` — baseline single-oracle vs 3-of-3 quorum table, both budget-checked |

**Behaviour.** Governance registers Identity feeders with `add_identity_oracle`
(admin-only, rate-limited, idempotent, capped at 20) and sets the agreement
threshold with `set_identity_quorum` (`[1, 20]`). When the list is non-empty,
`fund_invoice` calls
`oracle_registry::check_identity_quorum(env, Identity, token, payer, max_age)`,
which queries **every** registered feeder with `get_payer_data` through
`env.try_invoke_contract`. A response counts toward the quorum only when it is
both `is_verified` **and** fresh under the same shared
`is_oracle_data_stale` rule the single-oracle path uses (#1050). If fewer than
`N` feeders agree, the gate fails **closed** with `PayerUnverified` and emits
`OracleFeederDisagreement { feed_type, token, verified_count, required_quorum,
total_oracles }` so the disagreement is observable off-chain. The safety
property is that *a feeder which reverts, returns unverified, or returns stale
data contributes no agreement* — so a single bad feeder can never force
verification through, and a single good feeder can never be the only thing
holding the gate open once `N > 1`.

The default quorum is `DEFAULT_IDENTITY_QUORUM = 1`, which keeps the multi-oracle
list a strict **superset** of the prior behaviour: an operator who registers
feeders but never widens the threshold still only needs one agreement. Operators
relying on the list for disagreement safety must explicitly raise the threshold
(e.g. a majority of registered feeders). An **empty** list returns
`NotConfigured` and `fund_invoice` runs the unchanged single-oracle path, so
unmigrated deployments behave exactly as before. `check_identity_quorum`
deliberately does **not** write the single-oracle `OracleHealth` snapshot (that
key models one oracle per feed + token and is owned by the single-oracle path);
disagreement is surfaced through the dedicated event instead. The added state is
two `instance` keys — `IdentityOracles(feed_type)` and `IdentityQuorum` — and the
list is bounded and drift-tested like every other collection (see
[`oracle-design.md`](oracle-design.md), [`oracle-integration.md`](oracle-integration.md),
[`access-control.md`](access-control.md), [`threat-model.md`](threat-model.md)
and [`error-codes.md`](error-codes.md) for the before/after detail); no
value-encoding migration is required.

## 15. Recover after feeder outage (Issue #1048)

`record_oracle_health` (in `oracle_registry.rs`) trips the per-feed circuit
breaker after `MAX_CONSECUTIVE_STALE_QUERIES` (3) consecutive stale responses
from the same oracle, and `fund_invoice`'s gated path then refuses to resolve
that oracle (`OracleResolution::CircuitOpen`). The breaker was **sticky by
design** — it could only be cleared by the governance-gated
`reset_oracle_circuit` — precisely so a flapping oracle could not quietly
resume being trusted. But a *transient* feeder outage therefore jammed
oracle-gated funding with **no on-chain recovery path**: a feeder that came
back, healthy and advancing, still left `fund_invoice` rejecting until a slow
governance action landed. There was no way to distinguish "still broken" from
"recovered but still latched".

| | Baseline (`origin/dev`) | This PR |
|---|---|---|
| Breaker lifecycle | open until an explicit `reset_oracle_circuit`; a recovered feeder had no path back | **half-open** — a sustained run of healthy observations from the *tripping* oracle auto-closes it |
| Recovery trigger | governance `reset_oracle_circuit` only (admin, rate-limited) | permissionless `check_oracle_health` keeper path advances a recovery streak; no privilege required |
| Recovery threshold | n/a | `CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS` (3) **consecutive** healthy observations **and** `CIRCUIT_RECOVERY_MIN_LEDGERS` (120) ledgers elapsed since the streak began |
| "Healthy" definition | n/a | fresh (`!is_oracle_data_stale`) **and** timestamp-advancing vs the same oracle's previous snapshot (a frozen/replayed value never counts) |
| Anti-flapping | a single fresh query never cleared it (sticky) | preserved — a single fresh query advances the streak to 1 but does **not** recover; any stale/frozen observation resets the streak to 0 |
| Which oracle may recover | n/a | only the oracle recorded as having *caused* the trip (`CircuitRecoveryState.tripped_oracle`); a different, even healthy, oracle observed meanwhile cannot clear it |
| Missing recovery record | n/a | fails safe — no auto-recovery, admin `reset_oracle_circuit` still required (covers breakers tripped before this feature) |
| Event | `oracle_circuit_reset` | plus `oracle_circuit_recovered { feed_type, token, healthy_observations, elapsed_ledgers }` |
| Observability | `is_oracle_circuit_tripped` | plus `get_circuit_recovery_state` (anyone) → in-progress streak + start ledger, so a stuck recovery is visible |
| Admin fast path | `reset_oracle_circuit` | unchanged, still admin-only/rate-limited, and now also **clears** in-progress recovery bookkeeping |
| New state | — | `DataKey::OracleCircuitRecovery(feed_type, token) → CircuitRecoveryState` (persistent), written on trip, updated on healthy observations, removed on recovery/reset |
| Regression coverage | none | `tests_oracle_registry.rs`: sustained-health success + funding resumes with no admin call, insufficient ledger window, insufficient observation count, frozen oracle, mid-streak stale reset, different-oracle isolation, view tracking, no-admin auto-recovery, recovery event, re-trip after recovery, reset clearing progress |
| Cost measured | unmeasured | `tests_benchmarks.rs` `benchmark_oracle_circuit_auto_recovery` — closed-breaker keeper observation (pre-#1048 baseline) vs tripped streak observation vs tripped completion observation, all budget-checked |

**Behaviour.** On a trip, `record_oracle_health` now writes
`CircuitRecoveryState { tripped_oracle, consecutive_healthy: 0,
first_healthy_ledger: 0 }` pinning the breaker to the oracle that tripped it,
then calls `maybe_recover_oracle_circuit`. That helper is a no-op while the
breaker is closed, and while it is open it advances the streak only for the
pinned oracle on a fresh, timestamp-advancing response; it clears the streak on
anything else. When the streak reaches both thresholds it removes the recovery
record and the tripped flag and emits `oracle_circuit_recovered`. Because an
open breaker makes `fund_invoice` reject *before* querying the oracle, recovery
is driven exclusively by the permissionless, never-erroring `check_oracle_health`
path (keepers/monitors), whose writes commit — not by the funding path.

**Migration.** The added key is one persistent entry per tripped feed + token,
created lazily on a trip and removed on recovery; it is never read on the happy
path. No existing key, value encoding, error code, or TTL constant changed, so
**no data migration is required**. A breaker tripped before this release has no
recovery record and therefore fails safe: it still needs the admin
`reset_oracle_circuit`, exactly as before. See
[`oracle-design.md`](oracle-design.md#failure-handling-circuit-breaker-and-recovery-issue-1048),
[`oracle-integration.md`](oracle-integration.md#recovering-after-a-feeder-outage-issue-1048),
[`storage-layout.md`](storage-layout.md), [`threat-model.md`](threat-model.md),
[`upgrade-guide.md`](upgrade-guide.md) and [`benchmarks.md`](benchmarks.md) for
the before/after detail.

## 16. Emit stale and quorum failure events (Issue #1049)

`fund_invoice`'s Identity gate detects several rejection reasons — stale oracle
data (#1050), feeder disagreement (#1047), a regressed (#1052) or duplicated
(#1053) timestamp, an open circuit breaker, an unverified payer — but it reports
**every one of them by returning `Err`**. Soroban rolls back *all* events (and
storage writes) of an invocation that returns an error, so the gate's failure
signals never reached a ledger: monitoring could see that funding was failing but
could not distinguish "payer unverified" from "feeder data is stale" from
"feeders disagree", and the `oracle_feeder_disagreement` / (proposed)
stale-data events were structurally unobservable on the funding path. The one
existing way to observe a disagreement — invoking `check_identity_quorum`
directly — is a test-only workaround, not a production path.

The fix follows the ADR-010 "never-erroring diagnostic entrypoint" pattern that
`check_oracle_health` already established: expose the *same* decision logic
through a permissionless entry point that returns a value instead of an error, so
its side effects commit.

| | Baseline (`origin/dev`) | This PR |
|---|---|---|
| Failure observability | none — a rejected `fund_invoice` rolls its events back; stale / disagreement are invisible on chain | a permissionless `check_oracle_verification(feed_type, token, payer)` returns a structured `IdentityVerificationOutcome` and its events commit |
| Gate implementation | the decision is inline in `fund_invoice` (single copy, untestable in isolation) | one shared `oracle_registry::evaluate_identity_verification`; `fund_invoice` maps its outcome back to the exact error it returned, the diagnostic returns the outcome |
| Stale-data event | none | new `OracleDataStaleRejected` (`oracle_stale_data_rejected`) with `{ feed_type, token, oracle, data_timestamp, age_ledgers, max_age_ledgers }` |
| Disagreement event | `OracleFeederDisagreement` emitted inside `check_identity_quorum`, but only reachable from a `fund_invoice` that then rolls it back | unchanged type; now committed via the diagnostic (and still emitted on the funding path, where it rolls back with the `Err`) |
| `fund_invoice` error precedence | `OracleCircuitOpen` → `OracleTimestampRegression` → `OracleDuplicateUpdate` → `OracleDataStale` → `PayerUnverified` | **unchanged** — the shared helper runs the checks in the same order and `fund_invoice` maps them identically |
| `record_oracle_health` on the gate | called before the freshness check, so health reflects every query | unchanged — the shared helper calls it in the same place; the diagnostic also drives #1048 recovery |
| Authorization | funding is LP-only and admin sweep is admin-only | the diagnostic is **permissionless** (no `require_auth`), mirroring `check_oracle_health`; it has no successful-path effect beyond the health snapshot and the failure events |
| Regression coverage | none for the failure events on a committed path | `tests_oracle_registry.rs` `test_check_oracle_verification_*` + `test_fund_invoice_stale_rejection_events_roll_back_but_diagnostic_commits` + `test_check_oracle_verification_is_permissionless` |
| Cost measured | unmeasured | `tests_benchmarks.rs` `benchmark_oracle_verification_diagnostic_events` — verified (no event) vs stale-event vs disagreement-event table, all budget-checked |

**Behaviour.** `oracle_registry::evaluate_identity_verification` is the single
implementation of the Identity gate. It runs the multi-oracle quorum first
(`check_identity_quorum`, #1047), then the legacy single-oracle chain: circuit
status, timestamp monotonicity (#1052), duplicate-update cap (#1053),
`record_oracle_health` (#532), freshness (#1050 — emitting `OracleDataStaleRejected`
when stale), and finally the `is_verified` boolean. It **never returns `Err`**:
a reverting feeder degrades to "no opinion" via `try_invoke_contract`, exactly as
before. The public
`check_oracle_verification` returns `IdentityVerificationOutcome` (a
`#[contracttype]` enum: `Verified`, `Stale`, `QuorumDisagreement`,
`TimestampRegression`, `DuplicateUpdate`, `CircuitOpen`, `Unverified`); because it
returns `Ok`, its emitted failure events and its `OracleHealth` write survive.
`fund_invoice` calls the same helper and maps each outcome to the error it
returned before, so funding behavior — including precedence and the rollback of
its own events — is byte-for-byte unchanged. The diagnostic is additive: it adds
one non-erroring entry point and one new event type, with no authorization gate,
no new storage key, and no change to any existing key, value encoding, error
code, or TTL constant, so **no data migration is required**. (See
[`oracle-design.md`](oracle-design.md), [`oracle-integration.md`](oracle-integration.md),
[`event-types.md`](event-types.md) / [`events.md`](events.md),
[`error-codes.md`](error-codes.md), [`access-control.md`](access-control.md),
[`threat-model.md`](threat-model.md), [`upgrade-guide.md`](upgrade-guide.md) and
[`benchmarks.md`](benchmarks.md) for the before/after detail.)

## 17. Test compromised feeder data (Issue #1054)

The Identity defences added across this Wave all share one adversary: a
**compromised feeder** — an oracle that is still an *authorized* entry in the
registry (so no allowlist or interface-version check stops it) but whose
`get_payer_data` payload is attacker-controlled. Individually the defences
(#1050 freshness, #1052 monotonicity, #1053 duplicate cap, #1047 quorum,
#1048 circuit breaker, #1046 feeder authorization) were built and, for some of
them, unit-tested — but there was no focused suite that framed them as *one*
threat and proved, end to end, that a hostile payload from a registered feeder
cannot open the funding gate. The `test_1046_*` / `test_1052_*` / `test_1053_*`
cases also live only in `tests_oracle_freshness.rs`, which is **not declared** in
`lib.rs` (`mod tests_oracle_freshness;` is absent), so those cases never compiled
or ran (the #1046 write-path authorization separately has live coverage in the
`contracts/tests/` integration suite).

This PR adds a focused, adversarial suite in the **registered**
`tests_oracle_registry.rs` module that drives each defence with a hostile
payload and pins the expected outcome, plus a cost benchmark.

| | Baseline (`origin/dev`) | This PR |
|---|---|---|
| Compromised-feeder threat model | covered only implicitly, by separate feature tests | 9 focused tests in `tests_oracle_registry.rs` (`test_1054_*`) that frame one adversary against every defence |
| Lone rogue feeder | untested | quorum 2-of-3 with one rogue claiming *verified* → `fund_invoice` returns `PayerUnverified`; diagnostic reports `QuorumDisagreement` |
| Rogue feeder goes silent / reverts | untested | an honest quorum still funds (reverting feeder contributes "no opinion" via `try_invoke_contract`) — availability is not held hostage |
| Compromised **majority** | untested | documented boundary: the quorum is legitimately satisfied, so funding *succeeds*; only governance `remove_identity_oracle` restores safety (both legs asserted) |
| Stale replay | defence from #1050; no adversarial test | fresh → advance past the window → `OracleDataStale` |
| Future-dated clock skew | defence from #1050; no adversarial test | a timestamp ahead of the current ledger is rejected as stale |
| Timestamp regression | tested only in the orphaned `tests_oracle_freshness.rs` (never compiled) | regression → `OracleTimestampRegression` |
| Frozen snapshot (duplicate) | tested only in the orphaned `tests_oracle_freshness.rs` (never compiled) | one identical timestamp over `MAX_CONSECUTIVE_DUPLICATE_UPDATES` → `OracleDuplicateUpdate` |
| Quarantine / rollback | untested | repeated rejected `fund_invoice` calls cannot advance the breaker (they roll back); the permissionless `check_oracle_verification` keeper path commits the streak and trips it → `OracleCircuitOpen` |
| Rogue feeder joining the quorum | untested | `add_identity_oracle` / `set_identity_quorum` are admin-only → `Unauthorized` for an imposter |
| Coverage location | the `test_1046_*`/`test_1052_*`/`test_1053_*` cases sit in an **undeclared** module and never run | the #1054 suite is in the **declared** `tests_oracle_registry` module, so it is compiled and executed by `make test-invoice` |
| Cost measured | unmeasured | `tests_benchmarks.rs` `benchmark_compromised_feeder_rejection_paths` — one budget-checked row per rejection branch (stale replay, clock skew, regression, frozen snapshot, quorum disagreement) against the honest baseline |

**Behaviour.** No contract behaviour changes: this issue is test- and
documentation-only. Every assertion exercises existing code — the shared
`oracle_registry::evaluate_identity_verification` gate (and its
`check_oracle_verification` diagnostic, #1049), `check_identity_quorum` (#1047),
the freshness/monotonicity/duplicate predicates (#1050/#1052/#1053), the circuit
breaker (#1048), and the admin-gated identity registry (#1047). The suite also
records the one case no on-chain rule can cover: when a **majority** of
registered feeders is compromised the quorum is genuinely met, so the gate
correctly *cannot* distinguish it from honest agreement — recovery there is a
governance action (`remove_identity_oracle`), and the test asserts exactly that
boundary rather than pretending otherwise. Reusing the existing
`MockRegistryOracle` / `MockRevertingOracle` fixtures keeps the change purely
additive.

**Migration.** Test- and documentation-only: no storage key, value encoding,
error code, ABI, or TTL constant changed, so **no data migration is required**
and no deployed state is affected. Operators should note the operational
consequence surfaced by the quarantine test — a compromised feeder is only
*contained* (by the breaker) via the permissionless `check_oracle_verification`
keeper path, never by a rejected `fund_invoice` (whose writes roll back) — so
keepers must run that diagnostic continuously. See
[`oracle-design.md`](oracle-design.md),
[`oracle-attack-economics.md`](oracle-attack-economics.md),
[`oracle-provider-vetting.md`](oracle-provider-vetting.md),
[`threat-model.md`](threat-model.md) and [`benchmarks.md`](benchmarks.md) for the
before/after detail.

## 18. Expose oracle health and quorum (Issue #1055)

Answering the operational question "is oracle gating for this feed + token
healthy?" required an off-chain probe to know and fan out across **five**
separate read entry points — `get_oracle_health`, `is_oracle_circuit_tripped`,
`get_circuit_recovery_state`, `get_identity_oracles`, `get_identity_quorum` —
and, worse, the *live* health of a multi-oracle Identity quorum (the #1047 trust
model) had **no reader at all**. `check_identity_quorum` deliberately never
writes the single-oracle `OracleHealth` key (the quorum models *N* oracles per
feed + token, not one), so once a quorum list is registered `get_oracle_health`
reports `None` forever — the exact blind spot a monitoring page cannot afford,
and the action item already recorded in
[`game-day-exercise-execution.md`](game-day-exercise-execution.md) ("Add oracle
state visibility to `/health` endpoint").

The fix is additive and follows the ADR-010 "never-erroring diagnostic
entrypoint" pattern established by `check_oracle_health` (#532) and extended by
`check_oracle_verification` (#1049): expose the state through permissionless
views that read (and, for the quorum, re-probe) rather than mutate.

| | Baseline (`origin/dev`) | This PR |
|---|---|---|
| Configuration + health reads | five separate entry points, each of which the probe must know and call | one aggregate `get_oracle_status(feed_type, token)` returns `OracleStatusView`: resolution, last recorded `OracleHealthStatus`, breaker flag, half-open `CircuitRecoveryState`, Identity feeder list + threshold, and price sources |
| Live quorum health | **no reader** — in quorum mode `OracleHealth` is never written, so `get_oracle_health` is permanently `None` | permissionless `check_identity_quorum_health(feed_type, token, payer)` returns `IdentityQuorumHealth`: one `FeederHealth` observation per registered feeder (`Verified` / `Unverified` / `Stale` / `Unreachable`) plus `verified_count`, `required_quorum` and the resulting `quorum_met` verdict |
| Gate / diagnostic agreement | n/a (no reader existed) | the funding gate `check_identity_quorum` and the new diagnostic both classify each feeder through one shared helper, `oracle_registry::classify_identity_feeder`, so they can never disagree about *why* a feeder did or didn't count |
| Reverting / silent feeder | n/a | degrades to `FeederHealth::Unreachable` ("no opinion") via the non-panicking `try_invoke_contract`, mirroring the gate — the probe never aborts on one bad feeder |
| Cross-contract calls / writes | n/a | `get_oracle_status` performs **none** (pure local reads — cheap, high-frequency, and safe while paused); `check_identity_quorum_health` performs the live fan-out, bounded by `MAX_IDENTITY_ORACLES_PER_FEED` (20) |
| Authorization | funding is LP-only, sweep is admin-only | both views are **permissionless** (no `require_auth`), mirroring `check_oracle_health`; neither has a write side effect |
| Regression coverage | none | `tests_oracle_registry.rs` `test_1055_*` (10 cases): unconfigured feed, recorded health, open breaker + recovery, quorum configuration round-trip, per-feeder observations, unreachable feeder, the `age == max_age` ledger boundary, empty-list legacy fallback, read-only proof, and pause-safe / no-auth |
| Cost measured | unmeasured | `tests_benchmarks.rs` `benchmark_oracle_status_and_quorum_exposure` — the three `get_oracle_status` states plus `check_identity_quorum_health` at 1 / 5 / 20 feeders, every row budget-checked |

**Behaviour.** `oracle_registry::get_oracle_status` composes already-recorded
local state into one `#[contracttype]` view; it calls no oracle and writes
nothing, so it is safe to poll at any frequency and while the contract is
paused. `oracle_registry::check_identity_quorum_health` reads the registered
feeders and threshold, classifies each feeder with
`classify_identity_feeder` — the *same* predicate the funding gate's
`check_identity_quorum` now calls — and returns the per-feeder observations plus
`quorum_met` (`active && verified_count >= required_quorum`, always `false` when
the feeder list is empty so the single-oracle chain governs). Existing funding
behaviour is byte-for-byte unchanged: the only edit to a live path is the
quorum loop's internally-extracted call to the shared predicate, which produces
the identical count. The change is additive — two new entry points and two new
`#[contracttype]` views — with **no new storage key** and no change to any
existing key, value encoding, error code, or TTL constant, so **no data
migration is required**.

**Migration.** None required. Operators gain a one-call `/health` probe
(`get_oracle_status`) and a live quorum-health probe
(`check_identity_quorum_health`); neither needs a keeper/reconfiguration step and
neither changes funding, the breaker, or any recorded state. See
[`oracle-design.md`](oracle-design.md),
[`oracle-integration.md`](oracle-integration.md),
[`access-control.md`](access-control.md),
[`threat-model.md`](threat-model.md), [`upgrade-guide.md`](upgrade-guide.md) and
[`benchmarks.md`](benchmarks.md) for the before/after detail.

## 19. Document fail-closed oracle behavior (Issue #1056)

Earlier wave-9 oracle entries each landed one defence — freshness (#1050),
monotonicity (#1052), duplicate bounding (#1053), feeder authorization (#1046),
quorum agreement (#1047), breaker recovery (#1048), failure observability
(#1049), compromised-feeder coverage (#1054), and health/quorum exposure
(#1055) — but the **overall disposition they share was never stated in one
place**: once an Identity oracle is configured, the gate is *fail-closed*, and
every way of failing to positively verify a payer rejects funding instead of
falling through to the historical fail-open default. This section documents that
contract explicitly and pins it with regression tests, so the guarantee is a
specified, tested behaviour rather than an emergent property of nine separate
fixes.

**Corrected evidence pointer.** The audit evidence cites
`lib.rs:3832-3847` as the fail-closed oracle code path. Those lines are the
dispute-resolution ("Upheld") refund loop — they contain **no oracle logic**.
The real fail-closed path is `fund_invoice`'s `require_oracle_verification`
block (`lib.rs:2656-2711`), which delegates the entire decision to
`oracle_registry::evaluate_identity_verification`
(`oracle_registry.rs:1305`). This section is grounded in that path; no code fix
is implied by the citation, only the corrected reference.

| | Baseline (`origin/dev` / pre-#1056 docs) | This PR |
|---|---|---|
| Where fail-closed is described | scattered across nine per-issue notes; the end-to-end disposition (fail-closed once configured; fail-open only when unconfigured) is never stated together | one explicit statement of the disposition in [`oracle-design.md`](oracle-design.md) + this section, with the exact outcome → error mapping |
| Audit evidence pointer | `lib.rs:3832-3847` cited as the oracle path (it is dispute resolution) | corrected to `fund_invoice`'s `require_oracle_verification` block (`lib.rs:2656-2711`) → `evaluate_identity_verification` (`oracle_registry.rs:1305`) |
| Outcome → error mapping | implicit in `fund_invoice`'s `match` | documented as a table: `Stale → OracleDataStale`, `QuorumDisagreement → PayerUnverified`, `TimestampRegression → OracleTimestampRegression`, `DuplicateUpdate → OracleDuplicateUpdate`, `CircuitOpen → OracleCircuitOpen`, `Unverified → PayerUnverified`, `Verified → Ok` (funds) |
| Fail-open exception | "no oracle → all pass" noted, but not tied to the gate's `Unconfigured` branch | documented as the **only** fail-open path: no per-token override, no feed-type default, no legacy `price_oracle` (`IdentityQuorumOutcome::NotConfigured` + `OracleResolution::Unconfigured → Verified`) |
| Error precedence | documented per issue | documented as the fixed order `CircuitOpen → TimestampRegression → DuplicateUpdate → Stale → Unverified`, reached by the shared helper and therefore identical for the gate and the diagnostic |
| Rejection atomicity | implied by Soroban rollback semantics | tested: a rejected `fund_invoice` leaves the invoice `Pending` **and** rolls back the health snapshot it wrote (the reason the #1049 diagnostic must be the never-erroring path) |
| Authorization interaction | not stated together | tested: oracle verification is a *precondition*, never a substitute for the funder's `require_auth` — with authorizations stripped, `fund_invoice` still fails |
| Regression coverage | per-issue suites only (`test_1049_*`, `test_1050_*`, `test_1052_*`, `test_1053_*`, `test_1054_*`, `test_1055_*`) | `tests_oracle_registry.rs` `test_1056_*` (9 cases) covering success, the fail-open exception, unverified, quorum disagreement, the exact staleness ledger boundary, frozen/duplicate, rejection atomicity, gate/diagnostic agreement, and the authorization interaction |
| Cost measured | unchanged code path | no new branch — the gate and its per-branch cost are already measured by `benchmark_compromised_feeder_rejection_paths` (#1054) and `benchmark_oracle_verification_diagnostic_events` (#1049); see §17/§16 and [`benchmarks.md`](benchmarks.md) |

**Behaviour.** This issue adds **no new contract behaviour** — it documents and
pins what the gate already does. The disposition is: (1) if nothing is
configured for the `(feed type, token)`, the gate is fail-open (`Verified`), the
historical default, preserved byte-for-byte; (2) if anything is configured, a
payer funds only on a positive, fresh verification, and every non-positive
outcome of `evaluate_identity_verification` maps to a `ContractError` on
`fund_invoice` exactly as it did before. The mapping and its precedence
(`CircuitOpen` before `TimestampRegression` before `DuplicateUpdate` before
`Stale` before `Unverified`) live in the one shared helper, so the funding gate
and the permissionless `check_oracle_verification` diagnostic can never
disagree. Because a rejected `fund_invoice` returns `Err`, its writes and events
roll back — a rejection is atomic and leaves no invoice or health trace, which
is precisely why the diagnostic exists (#1049). Oracle verification is a
precondition on top of, never a replacement for, the funder's `require_auth`.
No storage key, value encoding, error code, event, entry-point signature, or TTL
constant is touched, so **existing funding behaviour and integrations are
unchanged**.

**Migration.** None required. The change is documentation plus regression tests;
no operator action, keeper step, reconfiguration, or data backfill is needed.
See [`oracle-design.md`](oracle-design.md),
[`oracle-integration.md`](oracle-integration.md) and
[`threat-model.md`](threat-model.md) for the added fail-closed documentation, and
[`benchmarks.md`](benchmarks.md) (§ Compromised-feeder rejection-path cost) for
the measured per-branch baseline this section relies on.

## 20. Benchmark worst-case queue insertion (Issue #1057)

Follow-up to §5 (Issue #1062): the max-size *retention* cost was measured, but
`join_fund_queue`'s **admission** cost at the bound was not measured across
positions. #1062's reproduction filled the queue with *equal* scores, so every
admitted joiner was forced to `insert_pos = 0` — a single point that cannot
distinguish a head insert from a tail insert, cannot aim at the middle, and
cannot exercise the `QueueFull` rejection branch. The audit asks for the
*worst case* explicitly, and equal scores cannot demonstrate where it is.

| | Baseline (`origin/dev`) | This PR |
|---|---|---|
| Admission cost at `MAX_FUND_QUEUE_LEN` | one point only (equal-score, forced index 0) | position sweep: head (index 0), middle (index 25), tail (index 49) |
| `QueueFull` rejection branch measured | no | `join_fund_queue_rejected_at_max` (tail-score joiner, `try_*` → `QueueFull`) |
| Duplicate-scan vs. insert-scan vs. re-serialize separated | not distinguished | sweep makes the row deltas attributable (same O(n) scans, differing shift + write position) |
| Where the worst case actually is | assumed to be the head | measured: rows are near-flat; cost is dominated by the serialized entry size at the bound, not the insertion index; head insert is marginally the worst *admitted* case |
| Behavioural coverage of positioning/displacement | equal-score only (`test_max_queue_size_*`) | `tests_storage.rs` `test_1057_*` (6 cases): head displacement, middle placement, tail+1 admission boundary, exact-tail-score rejection with queue-unchanged, stripped-auth rejection, and `FundQueueOpenedAt` non-reset on a displacing re-join |

**Behaviour.** Unchanged — this issue adds **no new contract behaviour**. The
change is a benchmark plus regression tests over the existing
`join_fund_queue` path. The measured disposition is: at the bound, a joiner
that strictly outscores the tail is admitted (displacing the tail and emitting
`fund_queue_displaced`), and a joiner at or below the tail score is rejected
with `QueueFull`; the insertion index does not materially change cost because
the host-native `Vec::insert` shift is negligible beside the full-queue
re-serialize (`save_fund_queue`) and the `extend_ttl` host call. The
`FundQueueOpenedAt` MEV marker (#MEV-1) is set on the first join only
(`try_set_fund_queue_opened_at`), so a displacing re-join never resets the
resolution maturity gate. No storage key, value encoding, error code, event,
entry-point signature, or TTL constant is touched, so **existing admission
behaviour and integrations are unchanged**.

**Migration.** None required. The change is measurement plus regression tests;
no operator action, keeper step, reconfiguration, or data backfill is needed.
See [`benchmarks.md`](benchmarks.md) (§ Worst-case queue insertion) for the
run command and the head/middle/tail/reject table, and §5 above for the
retention-cost baseline this section complements.
