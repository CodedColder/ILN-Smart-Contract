# Storage Layout

## 1. Overview
The ILN protocol is composed of **five independently deployed Soroban contracts**:

| Contract | Crate | Key Enum(s) |
| -------- | ----- | ----------- |
| Invoice Liquidity | `invoice_liquidity` | `DataKey` (`storage.rs`) |
| Insurance Pool | `insurance_pool` | `DataKey` (`lib.rs`) |
| Reputation Bonus | `reputation_bonus` | `ConfigKey`, `ReputationKey`, `InvoiceKey` |
| ILN Distribution | `iln_distribution` | `StorageKey` |
| ILN Governance | `iln_governance` | `StorageKey` |

Each contract has its own storage namespace on the Soroban host: all ledger
entries are keyed by `(contract_address, key)`, so **no two distinct deployed
contract instances can ever collide with each other**, even if they define an
identically-named key enum/variant (e.g. both `iln_distribution` and
`iln_governance` have a `StorageKey::GovToken`/`IlnContract`-style variant —
these live under different contract addresses and are fully isolated by the
host).

The collision surface that actually matters is therefore **within a single
contract's own key space** — i.e. do any of the enums/variants used *inside
one contract* serialize to the same storage key. This document maps every
storage key used by every contract and confirms there are no intra-contract
collisions.

### Why enum-keyed storage is collision-safe
Soroban's `#[contracttype]` derive serializes an enum value as an
`ScVal::Vec` whose first element is the `Symbol` of the variant name, followed
by any associated data:
- Unit variant: `Admin` → `[Symbol("Admin")]`
- Tuple variant: `Invoice(42)` → `[Symbol("Invoice"), U64(42)]`

Two keys collide only if both the variant name **and** all associated
arguments are identical. Since each contract uses a single family of key
enums with distinct variant names, and dynamic variants are always
parameterized by a unique id/address, no collisions occur.

---

## 2. `invoice_liquidity` — `DataKey` (`contracts/invoice_liquidity/src/storage.rs`)

### Instance Storage
| Variant | Stored Value | Purpose |
| ------- | ------------ | ------- |
| `Admin` | `Address` | Contract administrator |
| `Config` | `Config` | System parameters (fees, decay logic, token addresses, oracle) |
| `FeeRate` | `u32` | Protocol fee rate |
| `MaxDiscountRate` | `u32` | Maximum allowed discount rate for an invoice |
| `DistributionContract` | `Address` | External distribution contract address |
| `Paused` | `bool` | Emergency pause flag |
| `MinPayerReputation` | `u32` | Minimum payer reputation required to fund an invoice (Issue #28) |
| `NextInvoiceId` | `u64` | Auto-incrementing invoice id counter |

### Persistent Storage
| Variant | Stored Value | Purpose |
| ------- | ------------ | ------- |
| `Invoice(u64)` | `Invoice` | Individual invoice state (legacy monolithic format, retained for backward compatibility) |
| `InvoiceCore(u64)` | `InvoiceCore` | Hot-path invoice state (amount, status, due_date, token, payer, submitter; accessed >95% of operations) |
| `InvoiceMetadata(u64)` | `InvoiceMetadata` | Cold-path invoice metadata (retained separately to minimize read footprint) |
| `InvoiceCount` | `u64` | Legacy invoice counter |
| `Token` | `Address` | Primary token address |
| `PayerScore(Address)` | `ReputationScore` | Payer reputation + last-activity ledger |
| `InvoiceFunders(u64)` | `Vec<(Address, i128)>` | LP funders for partial funding |
| `ApprovedToken(Address)` | `bool` | Whitelisted token flag |
| `TokenList` | `Vec<Address>` | Iterable list of approved tokens |
| `TokenDecimals(Address)` | `u32` | Decimal precision per allowlisted token |
| `Reputation(Address)` | reputation profile | Detailed reputation profile (Issue #26) |
| `Appeal(u64)` | `AppealRecord` | Evidence hash for a default appeal |
| `PreDefaultPayerScore(u64)` | `u32` | Snapshot of payer score before a default penalty |
| `LpScore(Address)` | `u32` | LP reputation score |
| `FundQueue(u64)` | `Vec<LpFundRequest>` | LPs waiting to fund an invoice |
| `QueueResolution(u64)` | `Address` | Selected LP from a funding queue |
| `FundQueueOpenedAt(u64)` | `u32` | Ledger the first LP joined the queue (MEV maturity anchor) |
| `QueueResolvedAt(u64)` | `u32` | Ledger the winner was locked in (stalled-winner recovery anchor, Issue #1020); additive key, no migration |
| `TotalInvoices` | `u64` | Global protocol stat |
| `TotalFunded` | `u64` | Global protocol stat |
| `TotalPaid` | `u64` | Global protocol stat |
| `TotalVolumeUsdc` | `i128` | Global protocol stat |
| `TotalVolumeEurc` | `i128` | Global protocol stat |
| `TotalVolumeXlm` | `i128` | Global protocol stat |
| `TokenVolume(Address)` | `i128` | Per-token volume stat |
| `ReferralCount(BytesN<32>)` | `u32` | Referral counts keyed by fixed-size code |
| `Dispute(u64)` | dispute record | Dispute record for an invoice |
| `SubmitterInvoices(Address)` | `Vec<u64>` | Invoice ids submitted by a freelancer |
| `LpInvoices(Address)` | `Vec<u64>` | Invoice ids funded by an LP |
| `TopPayersHeap` | fixed-size heap | Top payers by reputation score (Issue #77) |
| `InvoiceNft(u64)` | `InvoiceNftMetadata` | NFT metadata for a funded invoice (Issue #423) |
| `InvoiceNftOwner(u64)` | `Address` | NFT owner tracking (Issue #423) |
| `StorageVersion` | `u32` | Storage schema version (defaults to 1 if unset; current version is 2) |

No two variants share the same name, and every parameterized variant
(`Invoice(u64)`, `InvoiceCore(u64)`, `PayerScore(Address)`, etc.) is disambiguated by its type and
value — a `u64` invoice id and an `Address` can never serialize identically.

### TTL Management and Upgrade Guard (Issue #1082)

Five persistent key categories carry **protocol-critical data** and have explicit
TTL management. Losing any of these entries to expiry mid-flight would cause
silent data loss without any on-chain indication:

| Key | TTL managed by | Threshold / Extend-to |
|-----|---------------|-----------------------|
| `InvoiceCore(u64)` | `storage::save_invoice` | 1,000,000 / 2,000,000 ledgers |
| `InvoiceMetadata(u64)` | `storage::save_invoice` | 1,000,000 / 2,000,000 ledgers |
| `FundQueue(u64)` | `invoice::save_fund_queue` | 1,000,000 / 2,000,000 ledgers |
| `FundQueueOpenedAt(u64)` | `invoice::try_set_fund_queue_opened_at` | 1,000,000 / 2,000,000 ledgers |
| `QueueResolution(u64)` | `invoice::save_queue_resolution` | 1,000,000 / 2,000,000 ledgers |

**Pre-upgrade TTL guard:** `upgrade()` calls
`storage::validate_ttl_fields_pre_upgrade()` before swapping the WASM binary.
The guard samples the most-recent `TTL_UPGRADE_SAMPLE_SIZE` (50) invoices and
checks each of the five categories above. If any existing entry has a remaining
TTL below `TTL_MIN_LEDGERS_BEFORE_UPGRADE` (500,000 ledgers ≈ 29 days), the
upgrade is **rejected fail-closed** with
`ContractError::UpgradeRemovesTtlFields` (code 50). The admin must refresh the
affected entries by re-invoking the relevant storage helper before retrying.

> **Note on the "TTL managed by" column:** the functions named above are the
> ones in `invoice.rs` that `lib.rs` actually imports and calls from
> `join_fund_queue` / `resolve_fund_queue`. Until Issue #1014, `storage.rs`
> also defined a second, identically-named set of `get_fund_queue` /
> `save_fund_queue` / `get_queue_resolution` / `save_queue_resolution` /
> `try_set_fund_queue_opened_at` / `get_fund_queue_opened_at` helpers (keyed
> off the same `DataKey` enum, re-exported as `StorageKey` in `invoice.rs`)
> that were **never called from anywhere** — an earlier, superseded draft of
> the Issue #1091/#1011 TTL-extension fix that landed in the wrong module.
> Issue #1014 removed that dead copy, so the `invoice.rs` functions above are
> now the only implementation. Extend TTL logic there only; do not
> reintroduce a parallel copy in `storage.rs`.

### Queued-entry retention guarantees (Issues #1011, #1012, #1013, #1014, #1015, #1016, #1018)

`resolve_fund_queue`'s MEV-mitigation delay (Issue #MEV-1, `QUEUE_DELAY_LEDGERS`
= 120 ledgers ≈ 10 minutes) depends entirely on `FundQueueOpenedAt(id)` still
being readable when someone calls `resolve_fund_queue`. The queue itself
(`FundQueue(id)`) and the eventual winner record (`QueueResolution(id)`) carry
the same requirement — losing any of the three mid-flight is silent data loss
for an invoice that may still be actively fundable.

**Before (pre-#1011):** `invoice::save_fund_queue`,
`invoice::try_set_fund_queue_opened_at`, and `invoice::save_queue_resolution`
— the versions actually wired into `join_fund_queue` / `resolve_fund_queue` —
wrote these three keys with Soroban's default persistent TTL and never called
`extend_ttl`. A TTL-safe version of the same three functions existed in
`storage.rs` (written for Issue #1091) but was never imported or called by
`lib.rs`, so it provided no protection in practice. A queue that sat open for
longer than the default TTL (e.g. an invoice nobody resolved promptly) could
have its `FundQueueOpenedAt` entry archived before `resolve_fund_queue` ran.
Because the maturity check was written as
`if let Some(opened_at) = get_fund_queue_opened_at(...) { ... }`, a missing
entry **silently skipped the delay check entirely** — fail-open — letting the
queue resolve immediately with no MEV protection, rather than surfacing the
lost record as an error.

**After:**
- **Retention (#1011):** `invoice::save_fund_queue`,
  `invoice::try_set_fund_queue_opened_at`, and `invoice::save_queue_resolution`
  each call `extend_ttl(key, FUND_QUEUE_TTL_THRESHOLD, FUND_QUEUE_TTL_EXTEND_TO)`
  = `(1_000_000, 2_000_000)` ledgers on every write — the same policy
  `save_invoice` already applies to `InvoiceCore`/`InvoiceMetadata`. Every LP
  join refreshes `FundQueueOpenedAt`'s TTL even though its *value* is only set
  once, so a queue that keeps receiving joins never drifts toward expiry, and
  even a queue frozen right after it opens has ~1,000,000+ ledgers (~58+ days
  at 5s/ledger) of headroom before the pre-upgrade threshold, let alone actual
  archival.
- **Fail-closed on loss anyway (#1013):** `resolve_fund_queue` no longer
  treats a missing `FundQueueOpenedAt` as "no gate required." Since the queue
  is already confirmed non-empty at that point in the function, a missing
  entry can only mean it was archived or otherwise lost — never that no LP
  ever joined. That case now returns
  `ContractError::QueueStateArchived` (code 75) and publishes a
  `queue_resolve_attempt` event with `success: false`, instead of silently
  granting an unprotected resolution.
- **Instrumentation (#1015):** a new read-only contract method,
  `get_fund_queue_ttl_margins(invoice_id) -> FundQueueTtlMargins`, reports the
  live TTL and the margin above `TTL_MIN_LEDGERS_BEFORE_UPGRADE` for each of
  the three keys (`None` when a key doesn't exist). Operators can poll this
  per-invoice to catch TTL drift before it ever reaches the upgrade guard
  (#1082) or the `QueueStateArchived` failure path (#1013) — see
  [`docs/exploit-pattern-monitoring.md`](exploit-pattern-monitoring.md).

**Regression coverage (#1012):**
[`contracts/invoice_liquidity/src/tests_queue_archival.rs`](../contracts/invoice_liquidity/src/tests_queue_archival.rs)
exercises all of the above: retention (TTL extension on `join_fund_queue` /
`resolve_fund_queue`, including that a second join refreshes
`FundQueueOpenedAt`'s TTL), the fail-closed `QueueStateArchived` path
(including that it's distinguished from the ordinary `QueueNotMature` and
`NotFunded` cases, so the new guard doesn't misfire), the
`get_fund_queue_ttl_margins` instrumentation (including the exact ledger at
which its margin crosses zero), and that neither of the touched entry points
gates on caller identity.
[`tests_queue_renewal_proptest.rs`](../contracts/invoice_liquidity/src/tests_queue_renewal_proptest.rs)
(#1018) generalizes the renewal and fail-closed-archival scenarios into two
properties checked across randomly-spaced, arbitrary-length join sequences,
rather than just the fixed scenarios above: renewal never loses the record
as long as gaps between joins stay under `FUND_QUEUE_TTL_THRESHOLD`, and the
fail-closed rejection fires regardless of how much renewal history or
post-loss delay preceded it.

**Guarantee going forward:** as long as at least one `join_fund_queue` call
has ever succeeded for an invoice, `FundQueue(id)` and `FundQueueOpenedAt(id)`
are retained for ≥1,000,000 ledgers from that call, and `QueueResolution(id)`
is retained for ≥1,000,000 ledgers from `resolve_fund_queue` succeeding. If an
entry is ever found missing despite this (e.g. stale state written before
this fix shipped), `resolve_fund_queue` fails closed rather than silently
dropping the MEV-mitigation delay.

**Migration note:** this is a behavior-only fix — no storage layout or key
format changed, so no data migration is required. The one operational
follow-up: any invoice whose fund queue was opened *before* this fix deployed
and has not received a new `join_fund_queue` call since will still be sitting
on its pre-fix (un-extended) TTL. Operators should run
`get_fund_queue_ttl_margins` across invoices with an open, unresolved queue
after deploying this fix and, for any with a low or negative margin, prompt a
new LP join (or have the admin re-invoke the relevant `invoice::save_*`
helper) to refresh the TTL before it actually expires.

### TTL cost at maximum queue size (Issue #1062)

The retention guarantees above were validated only at single-entry scale
(`tests_queue_archival.rs`), and the cost benchmarks covered only
single-invoice flows. `FundQueue(id)`'s entry size is bounded by
`MAX_FUND_QUEUE_LEN` (50) — Issue #1095, with the bound expressed as
property X7 in
[`formal-verification-cross-contract.md`](formal-verification-cross-contract.md)
— so the largest value the contract can ever persist is a 50-entry queue,
and that is the case the retention policy actually has to survive.

**Before:** nothing asserted or measured the retention policy at that bound.
`extend_queue_key_ttl` (called by `save_fund_queue` /
`save_queue_resolution` / `try_set_fund_queue_opened_at`) is key-based and
therefore independent of the entry's serialized size, but no test confirmed
that the full-queue admission path — including the displacing-join branch,
which pops the tail and re-serializes the whole vector before extending — still
extended the TTL, or where the `get_fund_queue_ttl_margins` boundary fell once
the queue was at maximum size.

**After:** `tests_storage.rs` fills a queue to exactly `MAX_FUND_QUEUE_LEN`
and asserts, at that bound: a full-queue join still extends `FundQueue` TTL to
≥ `FUND_QUEUE_TTL_THRESHOLD`; a strictly higher-scored displacing join refreshes
it; an equal- or lower-scored joiner is rejected with `ContractError::QueueFull`
without evicting the retained tail or touching its TTL; `join_fund_queue`
genuinely enforces the LP's authorization while `get_fund_queue_ttl_margins`
stays permissionless; and the margin crosses exactly `0 → -1` at
`TTL_MIN_LEDGERS_BEFORE_UPGRADE` one ledger apart. `tests_benchmarks.rs`
records the corresponding CPU/memory cost (see
[`benchmarks.md`](benchmarks.md#ttl-cost-at-maximum-queue-size-issue-1062)).

**Guarantee going forward:** the retention floor above holds irrespective of
queue length, including at the `MAX_FUND_QUEUE_LEN` bound — the TTL target is
per-key, so a maximum-size queue is retained for exactly as long as a
single-entry one.

**Migration note:** behavior-only — no storage layout, key format, or TTL
constant changed, so no data migration is required.

### Concurrent settlement and shared counters (Issue #1063)

`mark_paid` (the settlement path) is the only entry point that mutates the
contract's *shared* accounting on every successful call: the instance
reentrancy lock (`StorageKey::ReentrancyLock`), the persistent aggregate
counter (`StorageKey::TotalPaid`), and the `ContractStats` aggregate
(`total_paid`, `total_volume_usdc` / `_eurc` / `_xlm`, and
`total_volume_usd_normalized`). Per-invoice data (`DataKey::Invoice(id)`) is
independent by construction (INVARIANT S1 in `invoice.rs`), but the shared
counters are not, so settling a batch of invoices in one ledger is the case
where a crossed write would actually show up.

**Before:** settlement was only exercised one invoice at a time (single-invoice
tests in `test.rs` / `tests_coverage_gate.rs`). No test settled a batch in one
ledger, so nothing pinned down that the shared counters advance exactly once
per settlement, that a *rejected* settlement rolls back without disturbing the
others, or where the `paid_on_time` boundary actually falls.

**After:** `tests_storage.rs` settles a `SETTLE_STRESS_BATCH` (25) batch of
independent invoices in a single ledger and asserts per-invoice isolation
(each fresh payer's `invoices_paid` counts exactly its own settlement), exact
aggregate accounting (`total_paid` / `total_invoices` equal the batch size, not
more), correctness across a ledger-sequence boundary mid-batch, that a partial
payment does not count as a settlement, that rejected settlements (double-pay,
overpayment, unfunded) leave every other invoice settleable and never leak the
reentrancy lock, that `mark_paid` genuinely requires the payer's authorization,
and that `paid_on_time` flips exactly at `timestamp == due_date`. The
per-invocation CPU/memory cost of a settlement inside such a batch — the figure
mainnet's resource limit applies to — is recorded in `tests_benchmarks.rs` (see
[`benchmarks.md`](benchmarks.md#concurrent-settlement-stress-issue-1063)).

**Guarantee going forward:** shared counters advance once per completed
settlement regardless of how many settlements share the ledger, and a rejected
settlement never leaves shared state (reentrancy lock, counters) in a state
that blocks a subsequent settlement — i.e. settlement is per-invoice isolated
in accounting even when batched in one ledger.

**Migration note:** behavior-only — no storage layout, key format, or TTL
constant changed; no data migration is required.

### Bounding the per-feed price-source list (Issue #1064)

Each feed type's price-source list (`DataKey::PriceSources(OracleFeedType)`,
instance storage) is appended to by `add_price_source` and iterated in full by
`get_verified_price` on every price read. That read publishes one
`PriceOutlierRejected` event *per* source deviating beyond the configured
threshold, so an unbounded list meant both the event payload and the CPU/storage
cost of a read were unbounded in the registry's size.

**Before:** `add_price_source` had no upper bound — any number of distinct
sources could be appended to a feed, so `get_verified_price` could be driven to
emit an unbounded number of outlier events (and do unbounded iteration work) in a
single invocation.

**After:** `add_price_source` rejects a genuinely new source with
`ContractError::TooManyPriceSources` (76) once the feed already holds
`MAX_PRICE_SOURCES_PER_FEED` (20) sources. Re-registering an *existing* source
remains a no-op (idempotency preserved), and `remove_price_source` frees a slot.
The stored value is still the same `Vec<Address>` — only its maximum length is
now bounded. See `tests_storage.rs` (cap at the boundary, per-feed independence,
idempotent re-registration at the cap, slot recovery on removal, authorization,
and that a single read's event count is bounded) and
[`benchmarks.md`](benchmarks.md#price-read-event-fan-out-issue-1064).

**Guarantee going forward:** the number of `PriceOutlierRejected` events a single
`get_verified_price` call can emit — and the iteration cost of that call — is
bounded by `MAX_PRICE_SOURCES_PER_FEED`, independent of how the feed was grown.

**Migration note:** behavior-only — no storage layout, key format, or TTL
constant changed. An already-over-cap feed (only possible if written by the old
code) is not truncated; the cap only gates *further* additions, so no backfill is
required.

### The safe resource envelope (Issue #1065)

Every collection the `invoice_liquidity` contract stores on chain is bounded, so
the per-invocation CPU/memory cost of *touching* it is bounded too — that is what
keeps every code path inside Soroban's per-invocation resource budget. Before
Issue #1065 those bounds lived, unremarked, in the modules that enforced them,
with no single artifact declaring the whole envelope.

**Before:** the bounds were enforced (and therefore discoverable) one module at a
time — `MAX_FUND_QUEUE_LEN` and `MAX_PRICE_SOURCES_PER_FEED` in `constants.rs`,
`ADMIN_ACTION_LOG_CAPACITY` in `access.rs`, `TOP_PAYERS_CAPACITY` in
`constants.rs`, `MAX_TWAP_SAMPLES` in `twap.rs`. Nothing stated "these are *all*
of the unbounded-by-design collections, and these are their caps", so adding a new
one without a bound would not have stood out.

**After:** `storage::RESOURCE_ENVELOPE` (`contracts/invoice_liquidity/src/
storage.rs`) declares the whole envelope in one place, as `(name, cap)` pairs
where each `cap` references the authoritative constant that enforces it (never a
hand-written literal):

| Envelope name | Backing key | Cap (enforcing constant) |
| ------------- | ----------- | ------------------------ |
| `fund_queue_entries` | `FundQueue(u64)` | `constants::MAX_FUND_QUEUE_LEN` (50) |
| `price_sources_per_feed` | `PriceSources(OracleFeedType)` | `constants::MAX_PRICE_SOURCES_PER_FEED` (20) |
| `identity_oracles_per_feed` | `IdentityOracles(OracleFeedType)` | `constants::MAX_IDENTITY_ORACLES_PER_FEED` (20) |
| `admin_action_log_entries` | `AdminActionLog(u32)` | `access::ADMIN_ACTION_LOG_CAPACITY` (50) |
| `top_payers_entries` | `TopPayersHeap` | `constants::TOP_PAYERS_CAPACITY` (50) |
| `twap_samples_per_feed_token` | `TwapSamples(feed, token)` | `twap::MAX_TWAP_SAMPLES` (100) |
| `ttl_upgrade_sample` | (upgrade TTL sweep) | `constants::TTL_UPGRADE_SAMPLE_SIZE` (50) |
| `rollback_sample` | (rollback rehearsal sweep) | `constants::ROLLBACK_SAMPLE_SIZE` (50) |

**Guarantee going forward:** the declaration cannot silently drift from the code.
`tests_storage.rs` (Issue #1065) asserts each declared cap equals the constant that
actually enforces it, and that the declaration is well-formed (non-empty, unique,
positive caps); a live boundary test fills the fund queue to its declared cap,
crosses a ledger boundary, and confirms the cap still holds; an authorization test
confirms an admin with full auth cannot exceed a declared cap. The read-side cost
of the largest bounded collections *at* their caps is measured and budget-checked
in `tests_benchmarks.rs` (`benchmark_envelope_read_views_at_cap`; see
[`benchmarks.md`](benchmarks.md#safe-resource-envelope-read-cost-issue-1065)).

**Migration note:** documentation/measurement-only — no storage key, value
encoding, TTL constant, or contract behavior changed. No migration is required.

### Multi-oracle Identity quorum keys (Issue #1047)

`fund_invoice`'s oracle-gated path can now require several Identity feeders to
agree. Two new **instance** keys back this:

| Key | Value | Notes |
| --- | ----- | ----- |
| `IdentityOracles(OracleFeedType)` | `Vec<Address>` | the registered Identity feeders for that feed type; bounded by `MAX_IDENTITY_ORACLES_PER_FEED` (20) |
| `IdentityQuorum` | `u32` | the `N` in `N`-of-`M`; validated to `[1, 20]`, defaults to 1 when unset |

**Before:** the Identity feed resolved a single oracle
(`resolve_oracle_for_verification`); there was no feeder list and no quorum key.

**After:** `add_identity_oracle` / `remove_identity_oracle` (admin, rate-limited)
maintain the list — re-adding an existing feeder is a no-op, and a genuinely new
feeder past the cap fails with `TooManyIdentityOracles` (79) — while
`set_identity_quorum` (admin, rate-limited) sets the threshold, rejecting `0` or
`> 20` with `InvalidIdentityQuorum` (80). When the list is non-empty,
`check_identity_quorum` iterates it in full (one `get_payer_data` call per
feeder), so the cap bounds the worst-case `fund_invoice` cross-contract cost and
the `oracle_feeder_disagreement` payload's `total_oracles` field. See
`tests_oracle_registry.rs` (quorum met / disagreement fails closed / reverting
feeder / ledger-boundary staleness / event / cap + idempotency / authorization)
and [`benchmarks.md`](benchmarks.md#multi-oracle-identity-quorum-cost-issue-1047).

**Guarantee going forward:** the feeder list is bounded by
`MAX_IDENTITY_ORACLES_PER_FEED`, so the number of cross-contract calls a single
`fund_invoice` makes on the quorum path — and the size of its disagreement event
— is bounded independent of how the list was grown.

**Migration note:** the keys are absent on an upgraded contract and read as
"empty list" / "default quorum (1)", so a plain `upgrade()` suffices and **no
data migration is required** (storage version unchanged); an empty list preserves
the pre-#1047 single-oracle path. The list is bounded and drift-tested in
`tests_storage.rs` like every other entry in `RESOURCE_ENVELOPE`.

### Circuit-breaker recovery key (Issue #1048)

A tripped oracle circuit breaker can now auto-recover (half-open) once the
tripping oracle proves a sustained run of healthy observations. One new
**persistent** key backs the in-progress recovery:

| Key | Value | Notes |
| --- | ----- | ----- |
| `OracleCircuitRecovery(OracleFeedType, Address)` | `CircuitRecoveryState { tripped_oracle, consecutive_healthy, first_healthy_ledger }` | written when a breaker trips, updated on each healthy observation, removed on recovery or admin reset |

**Before:** a tripped breaker (`OracleCircuitTripped`) was sticky and could only
be cleared by the governance `reset_oracle_circuit`; there was no per-trip
bookkeeping.

**After:** on a trip, `record_oracle_health` writes the recovery state pinned to
the oracle that tripped it. The permissionless `check_oracle_health` path
advances `consecutive_healthy` for fresh, timestamp-advancing observations from
that oracle (any other observation resets it to 0). Once the streak reaches
`CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS` (3) **and**
`CIRCUIT_RECOVERY_MIN_LEDGERS` (120) ledgers have elapsed since the streak began,
both this key and `OracleCircuitTripped` are removed and `oracle_circuit_recovered`
is emitted. `reset_oracle_circuit` also removes this key. See
`tests_oracle_registry.rs` (sustained-health success / insufficient window /
insufficient count / frozen oracle / mid-streak stale reset / different-oracle
isolation / recovery event / re-trip / reset clears progress) and
[`benchmarks.md`](benchmarks.md#circuit-breaker-auto-recovery-cost-on-the-keeper-path-issue-1048).

**Guarantee going forward:** the key is created only when a breaker trips and
removed when it clears, so at most one such entry exists per currently-tripped
`(feed type, token)` — it cannot accumulate.

**Migration note:** the key is absent on an upgraded contract and simply reads as
`None`, which the recovery helper treats as **fail-safe** — no auto-recovery, so
an already-tripped breaker still requires the admin `reset_oracle_circuit` until
it re-trips under the new code. No storage key, value encoding, error code, or
TTL constant changed, so **no data migration is required**.

### Oracle gate-failure events — no new keys (Issue #1049)

Exposing the Identity gate through the permissionless `check_oracle_verification`
diagnostic — so its `oracle_stale_data_rejected` / `oracle_feeder_disagreement`
failure events commit instead of being rolled back with a rejected
`fund_invoice` — adds **no storage key**. The shared
`oracle_registry::evaluate_identity_verification` helper reads and writes exactly
the same `OracleHealth` (`DataKey::OracleHealth`), price-source, Identity-feeder,
and circuit-breaker keys the funding path already used, in the same order, so the
storage layout is unchanged and **no data migration is required** (storage version
unchanged). The only new on-chain artifact is the `OracleDataStaleRejected` event
type.

### Resolving a queue after invoice cleanup (Issue #1017)

Distinct from the TTL/retention guarantees above: `resolve_fund_queue` had
no awareness of `invoice.status` at all — unlike `join_fund_queue`, which
gates every call on it. `cancel_invoice` and `expire_invoice` never touch
`FundQueue` / `FundQueueOpenedAt` / `QueueResolution`, so a queue that was
non-empty but not yet resolved when the invoice was cancelled or expired
stayed fully resolvable afterward: anyone could still call
`resolve_fund_queue` and have it pick and announce a reputation-weighted
"winning" LP (`FundQueueResolved`) for an invoice that could never actually
be funded.

**Before:** `resolve_fund_queue` succeeded (or failed only on
`QueueNotMature` / `QueueStateArchived` / empty-queue grounds) regardless of
`invoice.status`.

**After:** `resolve_fund_queue` loads the invoice and applies the same
status match `join_fund_queue` already uses. Once the invoice has left
`Pending` / `PartiallyFunded`, resolving a *not-yet-resolved* queue now
fails with the matching status-specific error (`AlreadyCancelled`,
`InvoiceExpired`, `AlreadyFunded`, `AlreadyPaid`, `InvoiceDefaulted`,
`InvoiceAppealed`, `InvoiceDisputed`) instead of finalizing dead economic
state. A queue resolved *before* cleanup is unaffected — the pre-existing
"already resolved" cache check still runs first and keeps returning that
historical winner. See `INVARIANT Q1`/`Q2` in
[`invoice.rs`](../contracts/invoice_liquidity/src/invoice.rs)'s formal
verification header, and
[`tests_queue_status_gate.rs`](../contracts/invoice_liquidity/src/tests_queue_status_gate.rs)
for the regression suite (cancelled/expired-with-unresolved-queue,
resolved-before-cleanup-is-preserved, and the ordinary success path).

**Migration note:** behavior-only change, no storage or key format impact.
Any invoice that was cancelled/expired in the past while its queue was
non-empty and unresolved will simply have its queue permanently
unresolvable now (it already was unfundable; this only stops a late,
misleading `resolve_fund_queue` call from pretending otherwise). No backfill
is needed — there is nothing to migrate in existing `QueueResolution` data,
since none could have been incorrectly written by the old code for an
*already-terminal* invoice in a way this fix needs to undo.

---

## 3. `insurance_pool` — `DataKey` (`contracts/insurance_pool/src/lib.rs`)

| Variant | Storage Type | Stored Value | Purpose |
| ------- | ------------ | ------------ | ------- |
| `Admin` | Instance | `Address` | Authorised admin (reports confirmed defaults) |
| `Balance` | Instance | `i128` | Total pool balance (premiums minus payouts) |
| `Coverage` | Instance | `i128` | Active flat per-claim coverage cap |
| `Enrolled(Address)` | Persistent | `bool` | LP enrollment flag |
| `Premiums(Address)` | Persistent | `i128` | Cumulative premium paid per LP |
| `Claimed(u64)` | Persistent | `bool` | Whether a claim has been processed for an invoice id |
| `PendingCoverage` | Instance | `i128` | Proposed new coverage cap awaiting timelock (Issue #542) |
| `PendingAdmin` | Instance | `Address` | Proposed new admin awaiting timelock (Issue #542) |
| `CoverageEta` | Instance | `u64` | Ledger timestamp at which pending coverage change becomes executable (Issue #542) |
| `AdminEta` | Instance | `u64` | Ledger timestamp at which pending admin transfer becomes executable (Issue #542) |

> **TTL note:** Persistent keys (`Enrolled`, `Premiums`, `Claimed`) use Soroban's default persistent TTL (min 1,000,000 / max 2,000,000 ledgers). Instance keys live for the contract's lifetime and are removed when the contract instance is deleted.

---

## 4. `reputation_bonus` — three key enums, one contract

This contract splits its keys across three small enums defined in separate
modules. Because all three are used by the **same** deployed contract, they
share one storage namespace, so the check that matters is whether their
variant names can serialize identically. They cannot: each enum has disjoint
variant names (`Config`/`Admin` vs. `Reputation(Address)` vs.
`Invoice(u64)`/`InvoiceCount`), so there is no cross-enum collision risk.

| Enum | Variant | Stored Value | Purpose |
| ---- | ------- | ------------ | ------- |
| `ConfigKey` (`config.rs`) | `Config` | `Config` | Bonus/discount configuration |
| `ConfigKey` (`config.rs`) | `Admin` | `Address` | Contract administrator |
| `ReputationKey` (`reputation.rs`) | `Reputation(Address)` | `ReputationScore` | Per-address reputation counters |
| `InvoiceKey` (`invoice.rs`) | `Invoice(u64)` | `Invoice` | Invoice record |
| `InvoiceKey` (`invoice.rs`) | `InvoiceCount` | `u64` | Invoice counter |

---

## 5. `iln_distribution` — `StorageKey` (`contracts/iln_distribution/src/lib.rs`)

| Variant | Stored Value | Purpose |
| ------- | ------------ | ------- |
| `Initialized` | `bool` | One-shot init guard |
| `IlnContract` | `Address` | Address of the `invoice_liquidity` contract allowed to accrue |
| `GovToken` | `Address` | Governance token distributed as rewards |
| `LpFundedVolume(Address)` | `i128` | Cumulative volume funded per LP |
| `FreelancerSettled(Address)` | `i128` | Cumulative settled volume per freelancer |
| `PayerOnTimeSettled(Address)` | `i128` | Cumulative on-time settled volume per payer |
| `Claimed(Address)` | `i128` | Cumulative rewards already claimed per address |

---

## 6. `iln_governance` — `StorageKey` (`contracts/iln_governance/src/lib.rs`)

| Variant | Stored Value | Purpose |
| ------- | ------------ | ------- |
| `IlnContract` | `Address` | Target contract that executed proposals invoke into |
| `GovToken` | `Address` | Governance token used for voting weight |
| `MinQuorumBps` | `u32` | Minimum participation (bps of total supply) for a proposal to pass |
| `Proposal(u64)` | `GovernanceProposal` | Proposal record |
| `ProposalCount` | `u64` | Proposal id counter |
| `VoteWeightSnapshot(u64, Address)` | `i128` | Snapshotted vote weight per proposal/voter |
| `HasVoted(u64, Address)` | `bool` | Whether an address has voted on a proposal (temporary) |
| `VoteReceipt(u64, Address)` | `i128` | Durable mirror of the applied vote weight — fail-closed double-vote guard if the temporary receipt is archived (Issue #1099, persistent) |
| `VetoApprovals(u64)` | `Vec<Address>` | Per-proposal veto approvals (temporary mirror) |
| `VetoApprovalReceipt(u64, Address)` | `bool` | Durable per-signer veto approval flag; the veto tally is derived from these so an archived approval list cannot reset a partially-collected veto (Issue #1099, persistent) |
| `Delegation(Address)` | `Address` | Forward delegation pointer (Issue #64) |
| `Delegators(Address)` | `Vec<Address>` | Reverse delegation adjacency — direct predecessors of `Address`; required to enforce the DEL2 depth bound for every insertion order (Issue #1098) |
| `DelegatedToMe(Address)` | `i128` | Running tally of weight delegated to an address (Issue #64) |
| `ExecutionDelay` | `u32` | Timelock delay (in ledgers) applied between a proposal passing and executing (Issue #62) |
| `Admin` | `Address` | Admin address (veto power) |
| `VetoPowerEnabled` | `bool` | Whether admin veto power is currently active (Issue #68) |
| `MinProposalBalance` | `i128` | Minimum token balance required to create a proposal |

`VoteWeightSnapshot` and `HasVoted` are both 2-tuples of `(u64, Address)` but
remain distinguishable because they are different enum variants — the
variant name is always the first serialized element, so
`VoteWeightSnapshot(1, addr)` and `HasVoted(1, addr)` never collide.

---

## 7. Cross-Contract Interaction Notes

Contracts call into each other via `env.invoke_contract` / generated clients
(e.g. `iln_governance` → `invoice_liquidity`, `invoice_liquidity` →
`iln_distribution`, `invoice_liquidity` → `insurance_pool`/oracle). These are
ordinary cross-contract calls, not shared storage — the callee always reads
and writes its **own** storage, addressed by its own contract id. There is no
mechanism in Soroban by which one contract's storage read/write can target
another contract's storage, so cross-contract calls cannot introduce
collisions regardless of how similarly the two contracts name their keys.

## 8. Collision Prevention Strategy (summary)
- Every contract's storage keys are centralized in one (or a small, disjoint
  set of) `#[contracttype]` enum(s) — no raw `symbol_short!`/string keys are
  used for structured state.
- Enum variant names are unique within each contract's key space.
- Parameterized variants are disambiguated by argument type/value, and by
  variant name when two variants share an argument shape (e.g.
  `VoteWeightSnapshot(u64, Address)` vs `HasVoded(u64, Address)` in
  `iln_governance`).
- Storage is isolated per deployed contract address at the host level, so
  identical variant names across different contracts (`Admin`, `IlnContract`,
  `GovToken`, etc.) never collide.

## 9. Upgrade/Migration Notes

### 9.1 Key Enum Evolution
`invoice_liquidity` previously used separate `ConfigKey`/`StorageKey` enums
which were merged into the single `DataKey` enum. Variant names were
preserved during the merge, so existing serialized data remains
byte-compatible.

### 9.2 Storage Schema Versioning & Sequencing (Issue #1086)
`invoice_liquidity` uses `DataKey::StorageVersion` in instance storage to track
the schema migration level:
- **Baseline (Version 1):** If `DataKey::StorageVersion` is unset in instance storage,
  it defaults to `1` (`LEGACY_STORAGE_VERSION`). Legacy state stores invoices monolithically
  under `DataKey::Invoice(u64)`.
- **Target (Version 2):** Current schema version is `2` (`CURRENT_STORAGE_VERSION`).
  Invoices are split into hot-path `DataKey::InvoiceCore(u64)` and cold-path
  `DataKey::InvoiceMetadata(u64)`.
- **Backward-Compatible Fallback:** The accessor `try_load_invoice_core` (and `load_invoice`)
  checks `DataKey::InvoiceCore(id)` first; if missing, it transparently falls back to
  reading legacy monolithic `DataKey::Invoice(id)`. This prevents read regressions for
  invoices written under prior schema versions.
- **Migration Function:** The `migrate()` entrypoint verifies caller authorization (`admin.require_auth()`),
  checks that the current storage version is `< CURRENT_STORAGE_VERSION`, advances
  `DataKey::StorageVersion` to `CURRENT_STORAGE_VERSION`, and returns the updated version.
  Calling `migrate()` when already at `CURRENT_STORAGE_VERSION` is an idempotent no-op
  that succeeds and returns the current version without state disruption.
- **Operational Sequencing:** State migrations must be coordinated with contract code upgrades
  following the authoritative 6-step operational procedure (Pause -> Upgrade -> Migrate -> Validate -> Unpause -> Contingency).
  See [Upgrade Guide: Upgrade Migration Sequencing & State Versioning](upgrade-guide.md#upgrade-migration-sequencing--state-versioning-issue-1086).

## 10. Storage Layout Freeze — Sign-off Gate (Issue #651)

The "Storage layout frozen" item on the
[Mainnet Launch Checklist](mainnet-launch-checklist.md) requires an explicit,
recorded sign-off — not just the absence of open storage PRs — before mainnet
deployment. This section is that gate.

### 10.1 What is frozen

Once frozen, no PR may add, remove, rename, reorder, or retype a variant in
any of the key enums documented in sections 2–6 of this document
(`invoice_liquidity::DataKey`, `insurance_pool::DataKey`,
`reputation_bonus::{ConfigKey, ReputationKey, InvoiceKey}`,
`iln_distribution::StorageKey`, `iln_governance::StorageKey`) without first:

1. Reopening this freeze via a PR that updates this section (unfreezing is an
   explicit, reviewed action, not something that happens implicitly).
2. Getting the same sign-offs listed in §10.2 again after the change.
3. Updating sections 2–6 above and, if the change is not purely additive,
   the migration guidance in [Upgrade Guide](upgrade-guide.md).

Purely additive changes that a future upgrade's `migrate()` entrypoint
defaults for existing records (the pattern used by the
[v1→v2 migration](upgrade-guide.md#v1--v2-state-migration-script--issue-114))
remain possible post-launch without breaking this freeze — the freeze covers
*silent, unmigrated* schema drift, not the supported upgrade path itself.

### 10.2 Sign-off

Mainnet deployment must not proceed until every row below is signed:

| Area | Maintainer | Signed off | Date | Notes |
|------|------------|------------|------|-------|
| Contracts (storage authoring) | TBD | No | TBD | Confirms sections 2–6 match the code that will be deployed to mainnet. |
| Security (collision/migration review) | TBD | No | TBD | Confirms section 1 and 7's collision-safety argument and section 9's migration notes hold for the deployed code. |
| Release (freeze enforcement) | TBD | No | TBD | Confirms no open PR touches a frozen enum at deployment time. |

### 10.3 Enforcement until launch

Between sign-off and mainnet deployment, any PR that touches a frozen key
enum's definition must link back to this section in its description and
explain why the freeze does not apply (e.g. it is testnet-only, or it follows
the reopen procedure in §10.1). Reviewers should treat an unexplained diff to
a frozen enum as a launch-blocking finding.
