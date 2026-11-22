# Oracle Integration Guide

This guide explains how third-party providers can deploy a compatible payer-verification oracle and register it with the Invoice Liquidity Network.

---

## Overview

The ILN payer-verification oracle is an optional on-chain component. When registered, the ILN contract calls it to check whether a payer's identity or creditworthiness has been verified off-chain before allowing invoice funding. See [oracle-design.md](./oracle-design.md) for the full design specification.

---

## Oracle Interface Specification

Any oracle contract must expose two Soroban entry-points:

```rust
// Returns the verification record for `payer`
fn get_verification(env: Env, payer: Address) -> VerificationResult

// Updates the verification status for `payer` (oracle-operator only)
fn update_verification(env: Env, payer: Address, verified: bool)
```

Where `VerificationResult` is a `#[contracttype]` struct:

```rust
#[contracttype]
pub struct VerificationResult {
    pub verified: bool,   // true = payer is verified
    pub timestamp: u64,   // Unix epoch seconds of last update
}
```

The `get_verification` method must never panic under normal conditions. If no record exists for a payer, return `VerificationResult { verified: false, timestamp: 0 }`.

`update_verification` is the oracle's **feeder** (operator) write path and **must** be authorized — see [Authorizing the Feeder](#authorizing-the-feeder-issue-1046) below. The ILN contract never calls this method directly.

---

## Authorizing the Feeder (Issue #1046)

`update_verification` is the single write path that can flip a payer from
unverified to verified. When the ILN is configured with
`require_oracle_verification = true`, that flip is exactly what unlocks funding
in `fund_invoice`. An **unrestricted** `update_verification` therefore lets
anyone mint funding eligibility for any payer — the disposition/credit gate is
only as strong as this one entry point.

A conforming oracle **MUST**:

1. persist a designated **feeder** (operator) address — set once by the deployer
   / operator at deployment;
2. require that address's signature (`feeder.require_auth()`) before mutating any
   verification state; and
3. reject the call when **no feeder has been configured** (fail closed — never
   fall back to "anyone may write").

The reference implementation in
[`contracts/tests/mocks/mock_oracle.rs`](../contracts/tests/mocks/mock_oracle.rs)
models exactly this: `set_feeder(address)` configures the operator,
`update_verification` calls `feeder.require_auth()` and panics with
`"mock oracle: feeder not configured"` when none is set, and `get_feeder()`
exposes the current operator. Verified end-to-end by
[`contracts/tests/oracle_integration_test.rs`](../contracts/tests/oracle_integration_test.rs)
(success, unconfigured, unauthorized-signer under real auth, and feeder
rotation) and by the funding-path tests in
`contracts/invoice_liquidity/src/tests_oracle_freshness.rs`.

### Before / after

| | Before #1046 | After #1046 |
|---|---|---|
| `update_verification` authorization | documented in this guide and in `oracle_interface.rs`, but **not enforced** — any caller could flip any payer to verified | requires the configured feeder's `require_auth()`; an unauthorized signer is rejected and the write does not land |
| No feeder configured | unrestricted write still succeeded | call is rejected (`feeder not configured`) |
| Funding impact (when `require_oracle_verification = true`) | anyone could open the funding gate for any payer | only the configured feeder can open the gate |
| ILN contract surface | unchanged — the ILN never calls `update_verification` | unchanged — authorization is enforced inside the oracle contract the registry points at |
| Feeder rotation | n/a | re-pointing the feeder immediately revokes the previous one; the old feeder's writes are rejected, the new one's accepted |

The ILN contract's own behavior is **unchanged** by this issue: the gate lives on
the oracle's write path, which the ILN never calls. What changes is that a
compliant oracle can no longer ship an unrestricted operator entry point.

---

## Handling Feeder Disagreement (Issue #1047)

A single Identity oracle is a single point of both trust and failure: one
mistaken, offline, or compromised feeder decides funding eligibility for every
payer. #1046 closes *who may write* the oracle; #1047 closes *how many oracles
must agree* before that write unlocks funding.

Governance may register several Identity feeders, and `fund_invoice` will then
require a configurable quorum of them to agree the payer is verified:

```typescript
// Register two more Identity feeders (admin; rate-limited).
await ilnClient.add_identity_oracle({ feed_type: "Identity", oracle: feederB });
await ilnClient.add_identity_oracle({ feed_type: "Identity", oracle: feederC });

// Require 2 of the 3 registered feeders to agree (admin; must be in [1, 20]).
await ilnClient.set_identity_quorum({ quorum: 2 });
```

When the list is non-empty, `fund_invoice` queries **every** registered feeder
(via `get_payer_data`) and counts the responses that are both `is_verified`
**and** fresh (same ledger-age rule as the single-oracle path — see
[Staleness Policy](#staleness-policy)). If fewer than the configured quorum
agree, funding **fails closed** with `PayerUnverified` and an
`oracle_feeder_disagreement` event is emitted carrying `verified_count`,
`required_quorum`, and `total_oracles` so the disagreement is observable
off-chain.

A conforming feeder therefore only has to implement the **existing**
`OracleInterface` (`get_payer_data`) — there is no new method, no new version,
and no ABI change; each feeder is an independent deployment. The safety property
is *disagreement fails closed*: a feeder that reverts, returns unverified, or
returns stale data simply contributes no agreement, so it can never **force**
verification through. Note this cuts both ways — a **quiet** feeder (one that is
down or stale) also withholds its agreement, so size the quorum to your
tolerance for feeder downtime (e.g. `2-of-3`, not `3-of-3`, if one feeder may be
offline).

### Before / after

| | Before #1047 | After #1047 |
|---|---|---|
| Identity resolution | one resolved oracle (`resolve_oracle_for_verification`) | governance registers up to `MAX_IDENTITY_ORACLES_PER_FEED` (20) feeders; `fund_invoice` queries all of them when the list is non-empty |
| Decision rule | that one oracle's `is_verified` + freshness | `N`-of-`M` agreement, `N` = `get_identity_quorum()` (default 1) |
| Feeder disagreement | not consultable, not observable | fails closed (`PayerUnverified`) + `oracle_feeder_disagreement` event |
| Bad feeder impact | a single bad feeder decides the gate | a reverting / unverified / stale feeder contributes **no** agreement (cannot force verification) |
| Empty list (default) | — | `NotConfigured` → the **unchanged** single-oracle path runs (regression/duplicate/freshness/circuit checks intact) |
| Interface | `OracleInterface` v1 | unchanged — feeders are independent `get_payer_data` deployments |

Feeder-list and quorum changes are admin-only and rate-limited
(`add_identity_oracle`, `remove_identity_oracle`, `set_identity_quorum`); the
read accessors `get_identity_oracles` / `get_identity_quorum` are public. The
list is capped at 20 (`TooManyIdentityOracles` past the cap; adding an existing
oracle is a no-op) and the quorum is validated to `[1, 20]`
(`InvalidIdentityQuorum` otherwise) — see [access-control.md](access-control.md).

---

## Recovering After a Feeder Outage (Issue #1048)

The per-feed circuit breaker (see [Failure Handling](#failure-handling)) trips
after `MAX_CONSECUTIVE_STALE_QUERIES` (3) consecutive stale responses from the
same oracle, and `fund_invoice`'s gated path then stops resolving that oracle.
Before #1048 the breaker was **sticky**: it could only be cleared by the
governance-gated `reset_oracle_circuit`, so a *transient* feeder outage jammed
oracle-gated funding even after the feeder came back — with no on-chain way to
tell "still broken" from "recovered but still latched".

Since #1048 the breaker is **half-open** and self-heals: the permissionless
keeper path, `check_oracle_health`, auto-clears it once the feeder that caused
the trip serves a **sustained** run of healthy observations.

```typescript
// A keeper already polls this to track staleness; it now also drives recovery.
await ilnClient.check_oracle_health({ feed_type: "Identity", token, payer });

// Observe in-progress recovery (anyone): None when nothing is recovering.
const rec = await ilnClient.get_circuit_recovery_state({ feed_type: "Identity", token });
// rec => { tripped_oracle, consecutive_healthy, first_healthy_ledger }
```

Recovery requires **all** of:

- the observations come from the oracle recorded as having *caused* the trip
  (`CircuitRecoveryState.tripped_oracle`) — a different, even healthy, oracle
  cannot clear this breaker;
- `CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS` (3) **consecutive** healthy
  observations, where "healthy" means fresh (per the [Staleness
  Policy](#staleness-policy)) **and** timestamp-advancing versus that oracle's
  previous snapshot (a frozen/replayed value never counts); and
- `CIRCUIT_RECOVERY_MIN_LEDGERS` (120) ledgers elapsed since the streak began.

Any stale or non-advancing observation resets the streak to 0. A single fresh
query advances it to 1 but **never** recovers — the anti-flapping guarantee is
preserved. When the thresholds are met the breaker is cleared and an
`oracle_circuit_recovered` event
(`{ feed_type, token, healthy_observations, elapsed_ledgers }`) is emitted.

Recovery is **unprivileged to trigger**: keepers/monitors running
`check_oracle_health` on their normal cadence will un-jam the gate automatically.
It is driven by that path and **not** by `fund_invoice`, because an open breaker
makes `fund_invoice` reject *before* it queries the oracle. The governance fast
path `reset_oracle_circuit` remains — it is still admin-only and rate-limited,
and now also clears any in-progress recovery bookkeeping, so an operator can
force the gate closed immediately if needed.

### Before / after

| | Before #1048 | After #1048 |
|---|---|---|
| Tripped breaker | cleared only by admin `reset_oracle_circuit`; a genuinely recovered feeder stayed latched | **half-open** — auto-closes after a sustained healthy window |
| Recovery trigger | governance action | permissionless `check_oracle_health` keeper path (no privilege) |
| Recovery threshold | n/a | 3 consecutive healthy observations **and** ≥ 120 ledgers elapsed |
| "Healthy" | n/a | fresh **and** timestamp-advancing (frozen/replayed data does not count) |
| Single fresh query | did not clear it (sticky) | still does not clear it (streak reaches 1) — anti-flapping preserved |
| Different oracle | n/a | cannot clear the breaker; only the tripping oracle can |
| No recovery record | n/a | fails safe — admin `reset_oracle_circuit` still required |
| Event | `oracle_circuit_reset` | plus `oracle_circuit_recovered` |
| Observability | `is_oracle_circuit_tripped` | plus `get_circuit_recovery_state` (anyone) |

Runtime tuning is by contract upgrade of the two constants
(`oracle_registry::CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS`,
`CIRCUIT_RECOVERY_MIN_LEDGERS`); there is no admin setter, mirroring the
`MAX_CONSECUTIVE_STALE_QUERIES` trip threshold.

---

## Observing Gate Failures (Issue #1049)

`fund_invoice`'s Identity gate reports *every* rejection — stale data, feeder
disagreement, a regressed or duplicated timestamp, an open breaker, an unverified
payer — by returning `Err`. Soroban rolls back all events (and storage writes) of
an invocation that returns an error, so those failures leave **no on-chain
trace**: from a rejected funding transaction you can see that it failed, but not
*why*. This is exactly the rollback problem [ADR-010](adr/ADR-010-oracle-registry.md)
described for staleness monitoring, and it is why a failure event needs a
committed path to land on.

Since #1049 the gate's decision is exposed through a **permissionless,
never-erroring diagnostic** that returns the outcome instead of an error, so its
failure events commit:

```typescript
// Anyone may call this. It runs the *same* gate fund_invoice runs
// (quorum → circuit → monotonicity → duplicate → freshness → verified) and
// returns a structured outcome instead of an error, so its events survive.
const outcome = await ilnClient.check_oracle_verification({
  feed_type: "Identity",
  token,
  payer,
});
// outcome is one of:
//   "Verified"            — the payer passes (also the legacy fail-open case)
//   "Stale"               — fresh data rejected; emits OracleDataStaleRejected
//   "QuorumDisagreement"  — feeders disagree; emits OracleFeederDisagreement
//   "TimestampRegression" — replayed/rolled-back timestamp (Issue #1052)
//   "DuplicateUpdate"     — repeated identical timestamp past the cap (#1053)
//   "CircuitOpen"         — every candidate oracle is circuit-tripped
//   "Unverified"          — the oracle reports the payer is not verified
```

Unlike `check_oracle_health` (which only observes the single resolved oracle),
`check_oracle_verification` runs the **full** identity gate — including the
multi-oracle quorum — and emits the failure events:

- `oracle_stale_data_rejected` →
  `OracleDataStaleRejected { feed_type, token, oracle, data_timestamp,
  age_ledgers, max_age_ledgers }` (new in #1049), and
- `oracle_feeder_disagreement` →
  `OracleFeederDisagreement { feed_type, token, verified_count,
  required_quorum, total_oracles }` (Issue #1047 — previously only reachable on
  a `fund_invoice` that immediately rolled it back).

It also records oracle health exactly like the gate, so it still drives the
Issue #1048 circuit-breaker recovery. `fund_invoice` itself is unchanged in
behaviour: it keeps returning the same errors (and rolling its own events back),
so existing integrations need no changes.

### Before / after

| | Before #1049 | After #1049 |
|---|---|---|
| A rejected `fund_invoice` | failure reason invisible (events rolled back) | unchanged — but the reason is now observable via the diagnostic |
| Stale-data signal | none | `oracle_stale_data_rejected` (permissionless, commits) |
| Disagreement signal | `oracle_feeder_disagreement` only reachable on a rolled-back path | same event, now committed via the diagnostic |
| New entry point | — | `check_oracle_verification(feed_type, token, payer)` → `IdentityVerificationOutcome` (Anyone) |
| Gate implementation | inline in `fund_invoice` | shared `oracle_registry::evaluate_identity_verification`; funding maps the outcome back to the same error |
| Authorization | n/a | none — the diagnostic has no `require_auth`, mirroring `check_oracle_health` |
| Health recording | funding path only | diagnostic also records health (drives #1048 recovery) |

Wire `check_oracle_verification` into your monitoring cadence alongside
`check_oracle_health`: alert on a run of `Stale` / `QuorumDisagreement` outcomes
(a feeder is stale, misconfigured, or compromised and funding is failing closed)
and on `oracle_stale_data_rejected` / `oracle_feeder_disagreement` events.

---

## Exposing Health and Quorum (Issue #1055)

Answering "is oracle gating for this feed + token healthy?" used to require an
off-chain probe to know and fan out across five separate read entry points —
`get_oracle_health`, `is_oracle_circuit_tripped`, `get_circuit_recovery_state`,
`get_identity_oracles`, `get_identity_quorum`. Worse, under the #1047 multi-oracle
Identity quorum the *live* health had no reader at all: `check_identity_quorum`
never writes the single-oracle `OracleHealth` key, so `get_oracle_health` returns
`None` forever once a quorum list is registered.

Since #1055 that state is exposed through two **permissionless** views:

```typescript
// One-call aggregate of *recorded* state. No cross-contract calls, no writes —
// cheap enough for a high-frequency /health probe and safe while paused.
const status = await ilnClient.get_oracle_status({ feed_type: "Identity", token });
// status.resolved_oracle  — the oracle the priority chain resolves to (or null)
// status.health           — last recorded OracleHealthStatus (or null; see caveat)
// status.circuit_tripped  — is the breaker open for this feed + token?
// status.recovery         — in-progress half-open recovery, or null (Issue #1048)
// status.identity_oracles — registered Identity quorum feeders, in order
// status.identity_quorum  — the N of the N-of-M threshold (protocol-wide)
// status.price_sources    — registered price sources for Price feeds

// Live per-feeder health + the resulting quorum verdict, for one payer. This
// *does* query the feeders (bounded by MAX_IDENTITY_ORACLES_PER_FEED = 20).
const q = await ilnClient.check_identity_quorum_health({
  feed_type: "Identity",
  token,
  payer,
});
// q.feeders            — registered feeders, in order (empty ⇒ single-oracle chain governs)
// q.observations       — one FeederHealth per feeder, index-aligned:
//                          "Verified"    — fresh AND is_verified (counts toward quorum)
//                          "Unverified"  — fresh, but is_verified = false (disagrees)
//                          "Stale"       — responded but stale/future-dated
//                          "Unreachable" — reverted/trapped/undecodable ("no opinion")
// q.verified_count     — feeders that agreed
// q.required_quorum    — the N of N-of-M
// q.quorum_met         — verified_count >= required_quorum (false when no quorum is in use)
```

`check_identity_quorum_health` classifies each feeder through the **same**
predicate the funding gate uses (`oracle_registry::classify_identity_feeder`), so
it can never disagree with `fund_invoice` about *why* a feeder did or didn't
count; a reverting feeder degrades to `Unreachable` rather than aborting the
probe. Use `get_oracle_status` as the cheap periodic `/health` probe and
`check_identity_quorum_health` when you need to know *which* feeder is holding a
quorum back — both are read-only (no `require_auth`, no write side effects) and
therefore safe to call from any monitoring role.

### Before / after

| | Before #1055 | After #1055 |
|---|---|---|
| Configuration + health read | five separate entry points to know and call | one aggregate `get_oracle_status(feed_type, token)` → `OracleStatusView` |
| Live quorum health | **no reader** — `OracleHealth` is never written in quorum mode, so `get_oracle_health` is permanently `None` | `check_identity_quorum_health(feed_type, token, payer)` → `IdentityQuorumHealth` with a per-feeder `FeederHealth` + `quorum_met` |
| Gate / diagnostic agreement | n/a | both share `classify_identity_feeder`, so they cannot disagree |
| Cross-contract calls / writes | n/a | `get_oracle_status`: none (local reads only); `check_identity_quorum_health`: live fan-out, bounded by `MAX_IDENTITY_ORACLES_PER_FEED` |
| Authorization | n/a | both permissionless, mirroring `check_oracle_health`; no write side effect |

For a `/health` endpoint, poll `get_oracle_status` per (feed, token) and alert on
`circuit_tripped` or a stale `health`; where an Identity quorum is configured,
additionally poll `check_identity_quorum_health` and alert whenever `quorum_met`
is `false` while the feeder list is non-empty. See
[`audit-evidence-remediation-map.md`](audit-evidence-remediation-map.md#18-expose-oracle-health-and-quorum-issue-1055)
and [`benchmarks.md`](benchmarks.md) for the before/after and cost.

---

## Staleness Policy

`fund_invoice`'s `require_oracle_verification` path (the live enforcement point) checks freshness against `max_oracle_age_ledgers`, a per-protocol setting in **ledgers**, not seconds — default `DEFAULT_MAX_ORACLE_AGE_LEDGERS` = 17 280 ledgers, ≈ **24 hours** at 5 seconds/ledger. It's read/set via `get_max_oracle_age()` / `set_max_oracle_age()` (admin-gated, rate-limited to one call per ~10 minutes). Oracle operators must refresh records more often than this window to keep payers passing verification.

`oracle_interface.rs` also defines `ORACLE_STALENESS_THRESHOLD_SECS` (7 days) and an `is_fresh()` helper — this is **legacy/unused**: nothing in `fund_invoice` or any other live code path calls `is_fresh()`. Don't rely on the 7-day figure; it does not reflect enforced behavior. See [oracle-attack-economics.md](oracle-attack-economics.md) for why the actual enforced window matters to attack-cost modeling.

---

## Deploying a Custom Oracle

### Step 1 — Implement the interface

Create a Soroban contract that implements `get_verification` and `update_verification` as shown above. A minimal reference implementation is available in `contracts/tests/mocks/mock_oracle.rs`.

### Step 2 — Deploy to the target network

```sh
stellar contract deploy \
  --wasm oracle.wasm \
  --source <operator-keypair> \
  --network <testnet|mainnet>
```

Note the deployed contract address.

### Step 3 — Register with ILN

The ILN admin must call `set_price_oracle` with the oracle contract address. This is a **separate transaction** after `initialize()`:

```typescript
// Using the Soroban TypeScript SDK
await ilnClient.set_price_oracle({
  oracle: oracleContractAddress,
}, { fee: 100 });
```

Once registered, all subsequent payer-verification checks use the new oracle. There is no delay — the oracle takes effect immediately.

### Step 4 — Populate verification data

The oracle **feeder** (operator) calls `update_verification` for each verified
payer, signing with the feeder key configured at deployment (Issue #1046 — see
[Authorizing the Feeder](#authorizing-the-feeder-issue-1046)). A call not signed
by the configured feeder is rejected:

```typescript
await oracleClient.update_verification({
  payer: payerAddress,
  verified: true,
}, { fee: 100 });
```

Refresh records more often than `max_oracle_age_ledgers` (≈24h by default) to
stay within the enforced staleness window — see [Staleness Policy](#staleness-policy).

---

## Failure Handling

**Once any oracle is configured for a feed + token, funding is fail-closed**
(Issue #1056): a payer funds only on a positive, fresh verification, and every
other outcome rejects the call. The only fail-open path is the unconfigured case.
The gate reports the first matching condition in the order
`OracleCircuitOpen` → `OracleTimestampRegression` → `OracleDuplicateUpdate` →
`OracleDataStale` → `PayerUnverified`.

| Situation | ILN Response |
|---|---|
| No oracle configured for the feed + token | All payers pass (fail-open). The **only** fail-open path |
| Oracle returns `is_verified = false` | Funding fails with `PayerUnverified` |
| Oracle data is stale — age `>= max_oracle_age_ledgers` (default 17 280 ledgers ≈ 24h, **not** 7 days), or the timestamp is future-dated | Funding fails with `OracleDataStale` (Issue #1050) |
| Oracle re-serves an older timestamp than it last reported | Funding fails with `OracleTimestampRegression` (Issue #1052) |
| Oracle re-serves an identical timestamp past the duplicate cap (3) | Funding fails with `OracleDuplicateUpdate` (Issue #1053) |
| Oracle served 3 consecutive stale responses | The circuit breaker trips; funding fails with `OracleCircuitOpen` until recovery (Issue #1048) |
| Oracle contract panics / traps | The cross-contract call traps and the funding invocation reverts (fail-closed) |
| Identity feeders disagree (fewer than the quorum agree) | Funding fails with `PayerUnverified`; an `oracle_feeder_disagreement` event is emitted (Issue #1047) |
| A single Identity feeder reverts / is stale | It contributes no agreement ("no opinion"); the remaining feeders can still satisfy the quorum (Issue #1047) |

A rejected `fund_invoice` returns `Err`, so everything it wrote is rolled back —
the invoice stays `Pending` and no health snapshot is persisted. To *observe* a
rejection (and its reason) you must use the permissionless
`check_oracle_verification` diagnostic, which runs the identical gate but
returns the outcome so its events commit — see
[Observing Gate Failures](#observing-gate-failures-issue-1049). Oracle state is
also never a substitute for the funder's authorization: `fund_invoice` still
requires the funder's `require_auth` regardless of oracle configuration. See
[`oracle-design.md` § Fail-Closed Semantics](oracle-design.md#fail-closed-semantics-issue-1056)
for the full outcome-to-error mapping.

---

## Testing with MockOracle

The mock oracle in `contracts/tests/mocks/mock_oracle.rs` lets you test oracle-dependent behaviour deterministically.

### Basic usage

```rust
// In a test:
#[path = "path/to/mocks/mock_oracle.rs"]
mod mock_oracle;
use mock_oracle::{MockOracle, MockOracleClient};

let env = Env::default();
env.mock_all_auths();
let oracle_id = env.register(MockOracle, ());
let oracle = MockOracleClient::new(&env, &oracle_id);

// Mark a payer as verified
oracle.set_verified(&payer_address, &true);

// Set the oracle timestamp (simulate fresh data)
oracle.set_timestamp(&env.ledger().timestamp());
```

### Simulating stale data

```rust
// Oracle last updated 30 days ago
oracle.set_timestamp(&(env.ledger().timestamp() - 30 * 24 * 60 * 60));
oracle.set_verified(&payer, &true);

// ILN will treat this as unverified because timestamp is stale
let result = iln_client.check_something(&payer);
```

### Simulating oracle failure

```rust
// Arm a one-shot panic
oracle.set_should_panic();

// The next get_verification call panics; ILN treats payer as unverified
```

See `contracts/tests/oracle_integration_test.rs` for complete test examples.

---

## Security Checklist

- [ ] Oracle `update_verification` requires the **configured feeder's** signature (`feeder.require_auth()`), and rejects calls when no feeder is set (Issue #1046)
- [ ] Feeder key is a dedicated operator key — not the ILN admin key — and is rotatable (`set_feeder`) without redeploying the oracle
- [ ] The ILN admin key that calls `set_price_oracle` is protected (hardware wallet / multisig)
- [ ] Consider a governance timelock on oracle registration changes (see [governance.md](./governance.md))
- [ ] Refresh verification records before they exceed 7 days
- [ ] Monitor oracle contract for unexpected upgrades or admin key changes
- [ ] For disagreement safety, register more than one Identity feeder (`add_identity_oracle`) and raise `set_identity_quorum` above 1 (e.g. `2-of-3`) — an empty list or a quorum of 1 keeps the single-oracle trust model (Issue #1047)
- [ ] Monitor the `oracle_feeder_disagreement` event; repeated disagreements mean a feeder is stale, misconfigured, or compromised and funding is failing closed
