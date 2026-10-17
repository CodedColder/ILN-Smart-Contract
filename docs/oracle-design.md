# Oracle Design — Payer Verification

## Overview

The Invoice Liquidity Network (ILN) supports an optional off-chain payer verification oracle. The oracle surfaces KYC or creditworthiness data gathered off-chain into the smart contract, allowing invoice funding to be gated on verified payers.

The oracle is entirely optional. When no oracle address is registered, all payer-verification checks pass (fail-open). This preserves backwards compatibility and lets the system operate without an oracle in development or permissive deployments.

**Once an oracle *is* configured for a `(feed type, token)`, the gate is
fail-closed** (Issue #1056): a payer funds only on a positive, fresh
verification, and every way of failing to verify rejects funding rather than
falling through to the fail-open default. The only exception is the
truly-unconfigured case above. See [Fail-Closed Semantics](#fail-closed-semantics-issue-1056) below for the exact disposition.

---

## Oracle Data Format

Each payer verification record carries two fields:

| Field | Type | Description |
|---|---|---|
| `verified` | `bool` | Whether the payer has been verified by the oracle operator |
| `timestamp` | `u64` | Unix epoch seconds when the oracle last updated this entry |

The ILN contract reads these fields via a single cross-contract call per query. The oracle contract is responsible for storing and updating them per-address.

---

## Update Mechanism

The oracle follows a **pull model**:

1. The oracle operator calls `update_verification(payer, verified)` on the oracle contract off-chain at any time (e.g., after completing a KYC check). This write path is **feeder-authorized** (Issue #1046): the oracle must persist a designated feeder (operator) address and require its signature (`feeder.require_auth()`), rejecting the call when no feeder has been configured. See [oracle-integration.md § Authorizing the Feeder](oracle-integration.md#authorizing-the-feeder-issue-1046).
2. When the ILN contract needs to verify a payer, it calls `oracle.get_verification(payer)` and reads the current record.
3. There is no push or callback mechanism — the ILN contract reads on demand.

This design keeps the oracle stateless from ILN's perspective and avoids the complexity of push-based oracle patterns.

---

## Trust Model

- A single oracle address is stored in `Config.price_oracle` (shared with the price oracle slot for MVP simplicity), or, since Issue #532, resolved through the governance-controlled per-feed-type/per-token registry in `oracle_registry.rs` (see [ADR-010](adr/ADR-010-oracle-registry.md)).
- The oracle address is set by the ILN admin via `set_price_oracle(oracle_address)` (or `register_oracle`/`register_token_oracle`) in a separate transaction after `initialize()`.
- The oracle contract is fully trusted once registered — ILN does not validate oracle operator identity beyond the on-chain address, **unless** multiple sources are configured for a feed: cross-source deviation checking applies to numeric price feeds, and an `N`-of-`M` agreement quorum applies to boolean Identity (payer-verification) feeds — see [Multi-Source Price Deviation Checking](#multi-source-price-deviation-checking-price-feed) and [Multi-Oracle Identity Quorum](#multi-oracle-identity-quorum-issue-1047) below.
- The `N`-of-`M` agreement quorum for the Identity feed (Issue #1047) is opt-in: an empty feeder list or a quorum of 1 keeps the single-oracle trust model, and the multi-source *price* list remains a separate mechanism (outlier filtering rather than a quorum).

---

## Failure Modes

| Scenario | ILN Behaviour (fail-closed) |
|---|---|
| No oracle configured for the `(feed type, token)` | Permissive — verification always passes. **The only fail-open path** (Issue #1056) |
| Oracle reports `is_verified = false` | Funding fails with `PayerUnverified` |
| Oracle data is stale — age `>= max_oracle_age_ledgers` (default 17 280 ledgers ≈ 24h); a **future-dated** timestamp also counts as stale | Funding fails with `OracleDataStale` (Issue #93 / #1050) |
| Oracle re-serves an older timestamp than it last reported (Issue #1052) | Funding fails with `OracleTimestampRegression` |
| Oracle re-serves an identical timestamp more than `MAX_CONSECUTIVE_DUPLICATE_UPDATES` (3) times in a row (Issue #1053) | Funding fails with `OracleDuplicateUpdate` |
| Oracle served 3 consecutive stale responses (circuit breaker) | The feed + token's circuit breaker trips; `fund_invoice` stops resolving that oracle (`OracleCircuitOpen`) until it is cleared — by the admin `reset_oracle_circuit` fast path, or automatically (Issue #1048) once the *tripping* oracle serves 3 fresh, advancing observations over ≥120 ledgers |
| Identity feeders disagree — fewer than the quorum agree (Issue #1047) | Funding fails with `PayerUnverified`; an `oracle_feeder_disagreement` event is emitted |
| One Identity feeder reverts / is stale out of several (Issue #1047) | That feeder contributes no agreement; the other feeders can still meet the quorum |
| Oracle contract panics / traps (single-oracle path) | The cross-contract call traps and the whole funding invocation reverts — it is never treated as "verified" |
| Oracle contract address is wrong / undeployed | Cross-contract call panics; funding reverts |

When several conditions hold at once the gate reports the **first** match, in a
fixed order: `OracleCircuitOpen` → `OracleTimestampRegression` →
`OracleDuplicateUpdate` → `OracleDataStale` → `PayerUnverified`.

---

## Fail-Closed Semantics (Issue #1056)

The individual defences above share one disposition, made explicit here: **on
any configured `(feed type, token)`, funding is fail-closed.** A payer funds
only when the gate positively verifies them with fresh data; every other outcome
rejects the funding call. The sole fail-open path is the genuinely unconfigured
case (no per-token override, no feed-type default, no legacy `price_oracle`),
which is preserved for backwards compatibility.

The whole decision is one shared helper,
`oracle_registry::evaluate_identity_verification`, called by both `fund_invoice`
(which maps each outcome to a `ContractError`) and the permissionless
`check_oracle_verification` diagnostic (which returns the outcome). The mapping
is exact and its precedence is fixed by the helper:

| `IdentityVerificationOutcome` | `fund_invoice` result | Meaning |
|---|---|---|
| `Verified` | funds (`Ok`) | verified with fresh data — or nothing is configured (fail-open) |
| `Stale` | `Err(OracleDataStale)` | data aged out (or is future-dated) |
| `QuorumDisagreement` | `Err(PayerUnverified)` | fewer than the quorum of feeders agreed |
| `TimestampRegression` | `Err(OracleTimestampRegression)` | same oracle re-served an older timestamp |
| `DuplicateUpdate` | `Err(OracleDuplicateUpdate)` | identical timestamp past the duplicate cap |
| `CircuitOpen` | `Err(OracleCircuitOpen)` | every candidate oracle is breaker-tripped |
| `Unverified` | `Err(PayerUnverified)` | the oracle reports the payer is not verified |

Two properties follow and are pinned by the `test_1056_*` suite in
`tests_oracle_registry.rs`:

- **Rejection is atomic.** A rejected `fund_invoice` returns `Err`, so Soroban
  rolls back everything it wrote — the invoice stays `Pending` and the health
  snapshot the gate recorded is discarded. This is exactly why observing a
  *rejection* requires the never-erroring `check_oracle_verification` diagnostic
  ([Failure Observability](#failure-observability-stale-and-quorum-failure-events-issue-1049)).
- **Verification is a precondition, not an authorization.** Oracle state never
  substitutes for the funder's `require_auth`: with authorizations stripped,
  `fund_invoice` still fails regardless of oracle configuration.

No code path changed for this issue — the semantics above were already enforced
by the #1046–#1055 work; #1056 states them, corrects the audit evidence pointer
(the cited `lib.rs:3832-3847` is the dispute-resolution refund block, not oracle
logic — the real path is `fund_invoice`'s `require_oracle_verification` block,
`lib.rs:2656-2711`), and adds the regression suite. See
[`audit-evidence-remediation-map.md`](audit-evidence-remediation-map.md#19-document-fail-closed-oracle-behavior-issue-1056).

---

## Staleness Threshold

The default staleness threshold is **7 days** (604 800 seconds), defined as `ORACLE_STALENESS_THRESHOLD_SECS` in `oracle_interface.rs`.

Staleness is computed as:

```
now_seconds - oracle_timestamp > ORACLE_STALENESS_THRESHOLD_SECS
```

where `now_seconds` comes from `env.ledger().timestamp()`.

Oracle operators **must** refresh verification records at least every 7 days to keep payers active. The threshold can be tightened by changing the constant in a future contract upgrade if stricter freshness is required.

> **Note (live rule, Issue #1050).** The constant above is **not** the rule the on-chain gate actually enforces. `fund_invoice`'s `require_oracle_verification` block uses a **ledger-sequence** window — `max_oracle_age_ledgers`, default `DEFAULT_MAX_ORACLE_AGE_LEDGERS` (17 280 ledgers ≈ 24h) — compared against `env.ledger().sequence()`, not seconds against `env.ledger().timestamp()` (see [`oracle-attack-economics.md`](oracle-attack-economics.md) §6). That rule is the single shared predicate `oracle_registry::is_oracle_data_stale(current_ledger, data_timestamp, max_age_ledgers)`, also used by `record_oracle_health`. A `data_timestamp` **ahead of** the current ledger sequence is treated as **stale** (not as age 0), so a future-dated timestamp can no longer read as maximally fresh; and `max_oracle_age_ledgers == 0` disables the check entirely (governance escape hatch). Oracle operators should therefore refresh on the ledger window, not the 7-day one.
>
> **Note (monotonicity, Issue #1052).** The freshness gate bounds *how old* a response may be but not its *direction*, so — before #1052 — an oracle could re-serve a response it had already served (replayed data, or a rolled-back clock) and still pass while inside the window. `fund_invoice` now also rejects a `data_timestamp` **strictly older** than the last timestamp recorded for the **same** oracle on that `(feed type, token)` with `ContractError::OracleTimestampRegression`. The baseline is the per-`(feed type, token)` `OracleHealthStatus.last_data_timestamp` maintained by `record_oracle_health`, read *before* the snapshot is overwritten. Equal timestamps are allowed (the rule is non-decreasing), a first read with no snapshot is allowed, and a *different* oracle's timeline is never compared (so a failover oracle isn't a false positive). Combined with the #1050 rule, an accepted response must lie in `[last_recorded, current_ledger]`.
>
> **Note (duplicate update frequency, Issue #1053).** #1052's non-decreasing rule allows an *equal* timestamp, which left the opposite hole: an oracle that simply **stands still** re-serves an identical `timestamp` on every read, stays inside the freshness window, and keeps resetting the consecutive-stale streak — so a stalled/compromised oracle could keep funding open indefinitely at no cost. `fund_invoice` now also rejects a same-oracle response repeating an identical `data_timestamp` more than `MAX_CONSECUTIVE_DUPLICATE_UPDATES` (3) times in a row with `ContractError::OracleDuplicateUpdate`. The streak is `OracleHealthStatus.consecutive_duplicate_count`, maintained by `record_oracle_health` (read *before* it is overwritten), reset to `0` by any advance or oracle failover, and mirrored on the `oracle_health_recorded` event. Duplicates up to the cap are still accepted, and a rejected funding call rolls its write back so the gate does not advance by retrying — it re-opens only when the oracle actually advances (or governance changes the resolved oracle).
>
> **Note (compromised-feeder coverage, Issue #1054).** The notes above describe the individual rules; Issue #1054 adds the adversarial suite that frames their shared adversary — a feeder that is still an *authorized* registry entry but whose `get_payer_data` payload is attacker-controlled. `tests_oracle_registry.rs`'s `test_1054_*` set proves, end to end, that a lone rogue cannot reach the quorum (`PayerUnverified` / `QuorumDisagreement`), a silent or reverting feeder cannot jam an honest quorum, and a stale-replay / future-dated / regressed / frozen payload is rejected by the freshness, monotonicity and duplicate rules respectively. Two operational properties are pinned there explicitly: a rejected `fund_invoice` cannot advance the circuit breaker (its writes roll back), so containment is only *committed* via the permissionless `check_oracle_verification` keeper path; and a compromised **majority** of feeders genuinely satisfies the quorum, so it is not distinguishable on chain from honest agreement — that case is a governance `remove_identity_oracle` action, not something any on-chain rule can catch. The cost of each rejection branch is recorded by `benchmark_compromised_feeder_rejection_paths`.

---

## Multi-Source Price Deviation Checking (Price Feed)

The single-oracle model above (one resolved address per feed type/token) has no defense against a single misbehaving or compromised oracle reporting a wildly incorrect **price** — there's nothing on-chain to compare it against. `oracle_registry.rs` addresses this with a separate, optional multi-source registration list specifically for numeric price data:

- `add_price_source(feed_type, oracle)` / `remove_price_source(feed_type, oracle)` (admin-gated) maintain a list of price-reporting sources per feed type, independent of the single-oracle `register_oracle`/`register_token_oracle` registry used for boolean payer verification. **Each feed is capped at `MAX_PRICE_SOURCES_PER_FEED` (20) sources** (Issue #1064): a genuinely new source is rejected with `TooManyPriceSources` once the feed is full, while re-registering an existing one stays a no-op and `remove_price_source` frees a slot.
- `get_verified_price(feed_type, token)` queries every registered source (via `get_price(token) -> i128`, dynamically invoked — a single non-responding or panicking source is excluded from the sample rather than failing the whole query) and:
  - **Zero sources, or all queries failed:** returns an error (`NoPriceSource`).
  - **Exactly one source:** returns its price **unchecked**.
  - **Two or more sources:** computes the median of all successfully-queried prices, excludes any source deviating from that median beyond `get_max_price_deviation_bps()` (emitting `PriceOutlierRejected` per exclusion), and returns the median of the survivors — or `AllPriceSourcesRejected` if nothing survives. Because this emits one outlier event per deviating source, the per-feed source cap also bounds the number of events a single price read can publish (Issue #1064).
- The deviation threshold is governance-configurable via `set_max_price_deviation_bps` (default 500 bps / 5%), the same admin-gated, governance-controlled-in-production pattern used throughout this registry.

### The single-source risk is explicit, not silent

**If only one price source is registered for a feed type, there is no deviation protection, by construction — a lone source's report is trusted outright.** This is not an oversight or a gap to be closed later in this module; it is an accepted, unavoidable limitation of running a single-source price feed, identical in kind to the single-oracle payer-verification model's own single-point-of-failure risk (see [oracle-attack-economics.md](oracle-attack-economics.md) for that model's cost/benefit analysis, which compounds with this one). Deploying at least two independent, non-colluding price sources is what actually activates this section's protection. Governance and integrators should treat "only one price source registered" as a known, live risk to actively remediate (by registering additional sources), not a configuration that happens to work fine.

A closely related, less obvious limitation: **with exactly two sources, a single outlier cannot be selectively identified.** The median of two values is their average, which sits equidistant from both by construction — an honest source and a lying source deviate from that average by the *same* amount, so past the threshold both are rejected together (`AllPriceSourcesRejected`) rather than the bad one being picked out. This is the safer failure mode (refusing to guess which of two disagreeing sources is right) but it means two sources alone don't provide the selective-outlier-rejection this section is otherwise designed for — three or more independent sources are needed for that.

## Multi-Oracle Identity Quorum (Issue #1047)

The single-oracle model has the same single-point-of-failure problem for the
**boolean** Identity (payer-verification) feed that the price feed above solves
with outlier filtering: one mistaken, offline, or compromised feeder decides
whether *any* payer passes the funding gate, and disagreement between feeders is
neither consultable nor observable. `oracle_registry.rs` addresses this with a
per-feed-type list of Identity feeders and an `N`-of-`M` agreement quorum:

- `add_identity_oracle(feed_type, oracle)` / `remove_identity_oracle(feed_type, oracle)`
  (admin-gated, rate-limited) maintain the feeder list, **capped at
  `MAX_IDENTITY_ORACLES_PER_FEED` (20)** with the same defensive bound as the
  price list (Issue #1064's logic applied to this list too — the worst-case
  funding cost and the `oracle_feeder_disagreement` payload stay bounded).
  Re-adding an existing feeder is a no-op; a genuinely new feeder past the cap
  fails with `TooManyIdentityOracles`.
- `set_identity_quorum(quorum)` (admin-gated, rate-limited) sets `N`, validated
  to `[1, 20]` (`InvalidIdentityQuorum` otherwise); `get_identity_quorum()`
  defaults to `DEFAULT_IDENTITY_QUORUM` (**1**) until set.
- `check_identity_quorum(feed_type, token, payer, max_age)` queries **every**
  registered feeder via the existing `get_payer_data` interface, using the
  non-panicking `try_invoke_contract` (a reverting/incompatible feeder degrades
  to "no opinion", mirroring `query_price`). A response counts only if it is
  both `is_verified` **and** fresh under the shared `is_oracle_data_stale` rule
  (#1050). Fewer than `N` agreements → the gate fails **closed**
  (`PayerUnverified`) and an `OracleFeederDisagreement` event
  (`verified_count` / `required_quorum` / `total_oracles`) is emitted.

The safety property is **disagreement fails closed**: a feeder that reverts,
returns unverified, or returns stale data contributes no agreement and therefore
can never *force* verification through. Note the symmetric consequence — a
**quiet** feeder also withholds its agreement, so the quorum must be chosen to
tolerate the number of feeders that may legitimately be offline (e.g. `2-of-3`
rather than `3-of-3`). Jamming the funding gate by taking feeders offline or
stale is a denial-of-service, not a bypass.

This mechanism is **opt-in and backward compatible**: with an empty feeder list
`fund_invoice` returns to the unchanged single-oracle resolution chain
(`resolve_oracle_for_verification`), and `OracleInterface` v1 is untouched —
each feeder is an independent `get_payer_data` deployment, so there is no ABI or
interface-version change and no re-registration of existing oracles.

## Failure Handling: Circuit Breaker and Recovery (Issue #1048)

`record_oracle_health` trips a per-`(feed type, token)` circuit breaker after
`MAX_CONSECUTIVE_STALE_QUERIES` (3) consecutive stale responses **from the same
oracle**, and `fund_invoice`'s gated path then refuses to resolve that oracle
(`OracleResolution::CircuitOpen`) until it clears.

Before Issue #1048 the breaker was **sticky by design** — only the
governance-gated `reset_oracle_circuit` could clear it — so a *transient* feeder
outage jammed oracle-gated funding even after the feeder genuinely recovered,
with no on-chain way to distinguish "still broken" from "recovered but still
latched". #1048 makes the breaker **half-open**:

- On a trip, `record_oracle_health` records `CircuitRecoveryState
  { tripped_oracle, consecutive_healthy, first_healthy_ledger }`, pinning the
  breaker to the oracle that caused it.
- The permissionless `check_oracle_health` keeper path advances
  `consecutive_healthy` only for a **healthy** observation from that oracle —
  fresh (per `is_oracle_data_stale`) **and** timestamp-advancing past its own
  previous snapshot (a frozen/replayed value never counts). Any other
  observation resets the streak to 0.
- Once the streak reaches `CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS` (3) **and**
  `CIRCUIT_RECOVERY_MIN_LEDGERS` (120) ledgers have elapsed since the streak
  began, the breaker is cleared and `oracle_circuit_recovered` is emitted.

Design properties, all preserving the original anti-flapping intent:

- **A single fresh query never recovers** (it advances the streak to 1 only);
  both thresholds are required.
- **Only the tripping oracle can recover its own breaker** — a different, even
  healthy, oracle observed meanwhile does not clear it, so a previously-tripped
  oracle isn't silently re-trusted.
- **A missing recovery record fails safe** (e.g. a breaker tripped before this
  feature): no auto-recovery, so admin `reset_oracle_circuit` is still required.
- **Recovery is driven by `check_oracle_health`, not `fund_invoice`** — while
  the breaker is open the funding path rejects *before* querying the oracle, so
  only the never-erroring keeper path (whose writes commit) can make progress.
- `reset_oracle_circuit` remains the **admin fast path** and additionally clears
  any in-progress recovery bookkeeping. `get_circuit_recovery_state` exposes the
  in-progress streak so a stuck recovery is observable off-chain.

See [`oracle-integration.md`](oracle-integration.md#recovering-after-a-feeder-outage-issue-1048)
for the operator view and [`benchmarks.md`](benchmarks.md#circuit-breaker-auto-recovery-cost-on-the-keeper-path-issue-1048)
for the measured per-observation cost.

## Failure Observability: Stale and Quorum Failure Events (Issue #1049)

`fund_invoice`'s Identity gate detects several rejection reasons — stale data
(#1050), feeder disagreement (#1047), a regressed (#1052) or duplicated (#1053)
timestamp, an open breaker, an unverified payer — but it reports **all of them by
returning `Err`**. Soroban discards every event (and storage write) of an
invocation that returns an error, so those failure signals never land on a
ledger: monitoring sees that funding failed, not *why*. (See
[ADR-010](adr/ADR-010-oracle-registry.md) for the same rollback reasoning behind
`check_oracle_health`.)

#1049 makes the gate's decision observable by extracting it into one shared
helper and exposing it through a permissionless, never-erroring entry point:

- `oracle_registry::evaluate_identity_verification(env, feed_type, token, payer,
  max_age_ledgers) -> IdentityVerificationOutcome` is the **single**
  implementation of the gate. It runs the multi-oracle quorum first
  (`check_identity_quorum`, #1047), then the legacy single-oracle chain — circuit
  status, monotonicity (#1052), duplicate cap (#1053), `record_oracle_health`
  (#532), freshness (#1050 — emitting `OracleDataStaleRejected` when stale), and
  the `is_verified` boolean. It **never returns `Err`** (a reverting feeder
  degrades to "no opinion" via `try_invoke_contract`).
- `check_oracle_verification(feed_type, token, payer)` (Anyone) returns the
  `IdentityVerificationOutcome` — `Verified`, `Stale`, `QuorumDisagreement`,
  `TimestampRegression`, `DuplicateUpdate`, `CircuitOpen`, `Unverified`. Because
  it returns `Ok`, its failure events and its `OracleHealth` write commit.
- `fund_invoice` calls the **same** helper and maps each outcome back to the
  exact error it returned before (`Stale → OracleDataStale`, `QuorumDisagreement
  → PayerUnverified`, `TimestampRegression → OracleTimestampRegression`,
  `DuplicateUpdate → OracleDuplicateUpdate`, `CircuitOpen → OracleCircuitOpen`,
  `Unverified → PayerUnverified`), so funding behaviour, error precedence, and
  its own event rollback are unchanged.

The new event is `OracleDataStaleRejected` (`oracle_stale_data_rejected`) with
`{ feed_type, token, oracle, data_timestamp, age_ledgers, max_age_ledgers }`;
the existing `OracleFeederDisagreement` (#1047) is now reachable on a committed
path. `check_oracle_verification` also records health, so it still drives the
Issue #1048 breaker recovery. See
[`oracle-integration.md`](oracle-integration.md#observing-gate-failures-issue-1049)
for the operator view and
[`benchmarks.md`](benchmarks.md#verification-diagnostic-events-cost-issue-1049)
for the measured per-call cost.

## Health and Quorum Exposure (Issue #1055)

The registry's *configuration* and *health* state was readable only by fanning
out across five entry points — `get_oracle_health`,
`is_oracle_circuit_tripped`, `get_circuit_recovery_state`,
`get_identity_oracles`, `get_identity_quorum` — and the **live health of the
multi-oracle Identity quorum (#1047) had no reader at all**. The quorum path
deliberately does not write the single-oracle `OracleHealth` key (it models *N*
oracles per feed + token, not one), so `get_oracle_health` returns `None` forever
once a feeder list is registered. A `/health` page therefore could not see
whether gating was healthy — the exact action item recorded in
[`game-day-exercise-execution.md`](game-day-exercise-execution.md).

#1055 adds two additive, **permissionless**, read-only views:

- `get_oracle_status(feed_type, token)` (Anyone) → `OracleStatusView`: the
  resolved oracle, the last recorded `OracleHealthStatus`, the breaker flag, the
  half-open `CircuitRecoveryState` (#1048), the Identity feeder list + threshold,
  and the price sources — composed from **local storage reads only** (no
  cross-contract calls, no writes), so it is cheap and safe to poll at high
  frequency, including while the contract is paused.
- `check_identity_quorum_health(feed_type, token, payer)` (Anyone) →
  `IdentityQuorumHealth`: one `FeederHealth` observation per registered feeder
  (`Verified` / `Unverified` / `Stale` / `Unreachable`), the `verified_count`, the
  `required_quorum`, and the resulting `quorum_met` verdict. This necessarily
  queries the feeders, and its fan-out is bounded by
  `MAX_IDENTITY_ORACLES_PER_FEED` (20).

Both views classify a feeder through the **single** shared predicate
`oracle_registry::classify_identity_feeder`, which the funding gate's
`check_identity_quorum` also calls, so the diagnostic can never disagree with the
gate about *why* a feeder did or didn't count. A feeder that reverts/traps/
returns an undecodable payload degrades to `FeederHealth::Unreachable` ("no
opinion") via the non-panicking `try_invoke_contract` rather than aborting the
probe. The change is purely additive: two new entry points and two new
`#[contracttype]` views, no new storage key, no change to any existing key/value
encoding, error code, or TTL constant. See
[`oracle-integration.md`](oracle-integration.md#exposing-health-and-quorum-issue-1055)
for the operator view and
[`benchmarks.md`](benchmarks.md#oracle-health--quorum-exposure-cost-issue-1055)
for the measured cost.

## Security Considerations

- The oracle is a **read-only dependency** — it cannot initiate token transfers or modify ILN state.
- The admin key that controls `set_price_oracle` is a single point of control. A compromised admin can register a malicious oracle. Governance timelock on oracle registration changes is recommended.
- Fail-open behaviour applies **only** when nothing is configured (no oracle → all pass), which preserves backwards compatibility for deployments that run without an oracle. Once **any** oracle is registered for a feed + token the gate is fail-closed (Issue #1056) — every failure to positively verify rejects funding — so the "fail-open" default is not a live risk for a configured feed, only for a deliberately oracle-free deployment.
- Oracle address changes take effect immediately. There is no delay between registering a new oracle and it being used for checks. Consider adding a timelock.
- The per-feed price-source list is **capped** (`MAX_PRICE_SOURCES_PER_FEED`, Issue #1064) so that neither a single `get_verified_price` read's CPU/storage cost nor its `PriceOutlierRejected` event fan-out (one per deviating source) can be grown without bound — a feed that grew large enough to exceed Soroban's per-invocation resource limit would otherwise fail the price path for every token on that feed. The cap is a defensive bound on unbounded series, not a limit on any realistic quorum.
- The per-feed Identity-feeder list is likewise **capped** (`MAX_IDENTITY_ORACLES_PER_FEED`, Issue #1047) for the same reason — the funding path makes one cross-contract call per feeder, so the cap bounds the worst-case `fund_invoice` cost, and it also bounds the `oracle_feeder_disagreement` payload's `total_oracles` field. As with price sources, this is a defensive bound, not a limit on a realistic quorum.
- The Identity quorum's default of **1** means registering feeders alone does not add disagreement safety — governance must raise `set_identity_quorum` above 1 for the protection to take effect. Until then the trust model is still single-oracle.

---

## Migration Plan for TWAP Feeds

As part of the rollout of TWAP oracle feeds, clients and the protocol must migrate in a controlled manner:

1. **Deployment**: Upgrade the `invoice_liquidity` contract with the TWAP features. TWAP is disabled by default, meaning all feeds continue to operate using spot/boolean logic.
2. **Data Seeding**: An automated bot or indexer must invoke `record_twap_sample` for the desired `feed_type` and `token`. The buffer must reach the `MIN_TWAP_OBSERVATIONS` limit (default 2) spanning the configured window.
3. **Activation**: The admin invokes `set_twap_enabled(env, feed_type, true)`. Once enabled, price reads (`get_twap_aware_price`) strictly enforce the TWAP bounds and sample counts. If the minimum observations aren't met, an `InsufficientTwapObservations` error will be returned, refusing to silently fall back to spot prices.
4. **Monitoring**: Operators should monitor for `TwapInsufficientData` events to detect stalling feeds and ensure data automation continues submitting samples.
