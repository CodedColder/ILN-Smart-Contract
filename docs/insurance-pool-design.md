# Insurance Pool Design — Default Protection for LPs (Issue #123)

**Status:** Real token settlement implemented (Issue #824); claim payout prioritization added (Issue #825)
**Crate:** `contracts/insurance_pool`

## Motivation

Liquidity providers (LPs) who fund invoices bear the risk that a payer
*defaults*. Before mainnet, ILN should offer an **optional** insurance pool that
LPs can buy into for protection: they pay periodic premiums, and if an invoice
they funded defaults, the pool compensates them out of accumulated premiums.

This document describes the contract interface, the stub implementation shipped
in this PR, and the integration with the main `invoice_liquidity` contract.

## Interface

Defined in [`contracts/insurance_pool/src/insurance_interface.rs`] as the
`InsurancePoolInterface` trait (a typed `InsurancePoolInterfaceClient` is
generated for cross-contract calls):

| Method | Auth | Description |
|--------|------|-------------|
| `enroll(lp)` | `lp` | Opt an LP into the program. |
| `is_enrolled(lp) -> bool` | — | Whether `lp` is enrolled. |
| `deposit_premium(lp, amount)` | `lp` | Pay a premium; increases pool balance; auto-enrolls. |
| `claim(invoice_id) -> i128` | admin | Compensate for a defaulted invoice; returns payout. Idempotent per invoice. |
| `get_pool_balance() -> i128` | — | Total pool balance (premiums − payouts). |

Auxiliary views on the contract: `get_premiums_paid(lp)`, `get_coverage()`,
`is_claimed(invoice_id)`, plus `initialize(admin, coverage)`.

The five issues shipped with this iteration add the following **non-interface**
methods on the full `InsurancePoolClient` (the `InsurancePoolInterface` and its
`INSURANCE_INTERFACE_VERSION = 1` are deliberately **unchanged**, so existing
`invoice_liquidity` integration keeps working):

| Method | Auth | Issue | Description |
|--------|------|-------|-------------|
| `set_min_reserve_ratio_bps(bps)` | admin | #826 | Set minimum pool reserve ratio (bps, 0..=10_000) below which claims pause. `0` disables the breaker. |
| `get_min_reserve_ratio_bps() -> u32` | — | #826 | Current breaker threshold (bps). |
| `get_reserve_ratio_bps() -> u32` | — | #826 | Current reserve ratio: `(balance + backstop) / coverage` in bps. |
| `get_total_reserve() -> i128` | — | #826/#827 | Liquid balance + capital backstop. |
| `is_solvency_circuit_open() -> bool` | — | #826 | Whether the sticky breaker is currently open. |
| `reset_solvency_circuit()` | admin | #826 | Clear the breaker trip and emit `SolvencyCircuitReset`. |
| `set_backstop_funding_bps(bps)` | admin | #827 | Share (bps) of each premium diverted to the backstop. `0` disables. |
| `get_backstop_funding_bps() -> u32` | — | #827 | Config backstop funding share (bps). |
| `get_backstop_balance() -> i128` | — | #827 | Current backstop balance. |
| `top_up_backstop(from, amount)` | admin | #827 | Book `amount` from `from` into the backstop (real token transfer). |
| `submit_claim_evidence(invoice_id, hash)` | admin | #828 | Attach a 32-byte evidence hash to a claim; records timestamp. |
| `get_claim_evidence(invoice_id) -> Option<ClaimEvidence>` | — | #828 | Evidence hash + submission time for an invoice. |
| `set_review_window_seconds(secs)` | admin | #828 | Require evidence + this delay after submission before payout. `0` disables. |
| `get_review_window_seconds() -> u64` | — | #828 | Config review window (seconds). |
| `record_pair_default(lp, payer)` | admin | #829 | Record a default against a specific (lp, payer) pair; bumps LP/pool rollups. |
| `get_pair_default_count(lp, payer) -> u32` | — | #829 | Raw per-pair default count. |
| `get_pair_collusion_flag(lp, payer) -> bool` | — | #829 | Heuristic: pair flagged when ≥ `MIN_PAIR_DEFAULTS_TO_FLAG` defaults and ≥ 50% of the LP's defaults concentrate on one payer. |

### Timelocked admin actions (Issue #542)

Coverage cap changes and admin transfers are sensitive to LPs, so they are
queued behind a `TIMELOCK_DELAY_SECONDS` (3 days) delay rather than applying
immediately:

| Method | Auth | Description |
|--------|------|-------------|
| `propose_coverage_change(new_coverage) -> u64` | admin | Queue a new coverage cap; returns the ledger timestamp (ETA) at which it becomes executable. |
| `execute_coverage_change()` | — | Apply the pending coverage change once its ETA has passed. Callable by anyone. |
| `cancel_coverage_change()` | admin | Cancel a pending coverage change before it executes. |
| `propose_admin_transfer(new_admin) -> u64` | admin | Queue an admin transfer; returns the ETA. |
| `execute_admin_transfer()` | Pending admin | Apply the pending admin transfer once its ETA has passed. The pending admin must authorize it (acceptance step, Issue #1265), so a transfer can never install an address that cannot sign. |
| `cancel_admin_transfer()` | admin | Cancel a pending admin transfer before it executes. |
| `get_pending_coverage() -> Option<(i128, u64)>` | — | View the pending coverage proposal, if any. |
| `get_pending_admin() -> Option<(Address, u64)>` | — | View the pending admin proposal, if any. |

Each proposal overwrites any previously pending proposal of the same kind.
`execute_*` is intentionally open to any caller (like `execute_proposal` in
`iln_governance`) since the timelock itself — not caller identity — is the
security boundary once a change has been proposed by the admin.

## Implementation status

The implementation in `contracts/insurance_pool/src/lib.rs` is **fully featured** with both
token settlement and payout prioritization:

- **Accounting, not custody for premiums; real transfers for payouts.**
  `deposit_premium` moves real SAC tokens from the LP into the pool and
  records the amount as pool balance; `claim` transfers real tokens back out.
- **Tiered coverage cap (Issue #528).** `claim` pays
  `min(tiered_coverage(lp), available_reserve)`, where `tiered_coverage(lp)`
  scales the configured flat `coverage` cap by how much the LP has paid in
  premiums over the pool's lifetime — see
  [Tiered coverage boundaries](#tiered-coverage-boundaries-issue-528) below.
  `available_reserve` is the liquid balance **plus** the capital backstop
  (Issue #827), drawn liquid-first then from the backstop.
- **Idempotency & auth.** Each `invoice_id` can be claimed once; `claim`
  requires the configured admin (the liquidity contract in production).

### Solvency circuit breaker (Issue #826)

Governance sets a minimum reserve ratio (`MinReserveRatioBps`, default `0` =
disabled) against the per-claim coverage cap. Each claim sequence:

- **Gate:** if the sticky `SolvencyCircuitOpenFlag` is already open, `claim`
  panics with `SolvencyCircuitOpen` *before* any storage write (rejections
  never have state to roll back).
- **Trip:** a payout that leaves the pool at/below the threshold — measured as
  `(liquid_balance + backstop) / coverage` — trips the breaker *after* the
  payout completes. This ordering is deliberate: in Soroban, a storage write
  followed by a panic in the same invocation is rolled back (no partial
  commit), so the flag could never persist if it were set on a rejection path.
  The tripping claim therefore pays out its final bound payout (emits
  `claimed`), then emits `SolvencyCircuitTripped { ratio_bps, reserve }` and
  sets the sticky flag.
- **Sticky pause:** while the flag is open, **all** subsequent claims are
  rejected with `SolvencyCircuitOpen` — even if a later deposit restored the
  ratio — until governance calls `reset_solvency_circuit()` (emits
  `SolvencyCircuitReset`). This sticky design intentionally requires an
  explicit human resume after a solvency event.
- **Enrollments and premium deposits are never blocked** by the breaker, so
  the pool can be recapitalized while payouts are paused.

The breaker is purely additive and off by default, so pools that never arm it
behave exactly as before. Note that because `claim()` derives payout from
tiered coverage capped by available reserve, the breaker is a *policy* guard
for LPs/auditors — the protocol's own solvency invariant (never overpay) was
already guaranteed by the `min()` bounding.

### Capital backstop (Issue #827)

The pool holds a second accounting balance, the **backstop**, separate from the
liquid claim `Balance`:

- **Funding sources**: a governance-led `top_up_backstop(from, amount)` (real
  token transfer from `from`), plus an optional automatic share of every
  premium deposit (`set_backstop_funding_bps`, default `0` = disabled).
- **Payout order**: `claim()` computes `payout = min(tiered_coverage(lp),
  liquid_balance + backstop)` and draws liquid balance **first**, then the
  backstop. The claim event payload remains the total payout; the internal
  split is observable via `get_backstop_balance()` after the claim.
- **Semantics**: the backstop is a reserve buffer, not LP-owned deposits —
  it extends the pool's claim-paying capacity beyond the liquid premium pool.
  The chosen approach and the rejected alternatives are recorded in
  [ADR-013](adr/ADR-013-capital-backstop.md).

### Claim evidence & review window (Issue #828)

Two opt-in mechanisms around claims, both **disabled by default** so the
automatic flow is unchanged:

- **Evidence attachment (advisory, always available):**
  `submit_claim_evidence(invoice_id, hash)` records a 32-byte digest (e.g. an
  IPFS CID or document hash) plus a ledger timestamp, queryable via
  `get_claim_evidence()`. This is purely additive — an auditable trail of
  what documentation backs a claim, attachable before or after payout.
- **Review-window gate (opt-in per risk tier):** when governance sets
  `set_review_window_seconds(n) > 0`, `claim()` additionally requires that
  evidence was submitted and that `timestamp >= submitted_at + n` before
  paying out. Without evidence it panics `EvidenceRequired`; too early it
  panics `ReviewWindowNotElapsed`. Because `invoice_liquidity` calls `claim`
  via `try_claim` and treats failure as `compensated: false`, enabling the
  gate for a risk tier simply defers compensation until a follow-up payout
  invocation after the window — compatible with the existing graceful
  degradation, no timelock-queue changes needed.

### Per-pair default tracking & collusion heuristic (Issue #829)

`record_pair_default(lp, payer)` counts defaults per (lp, payer) pair while
keeping the existing per-LP and pool-wide counters (Issue #528) in sync.
The raw `get_pair_default_count(lp, payer)` is surfaced for off-chain
monitoring. `get_pair_collusion_flag(lp, payer)` applies the documented
heuristic on-chain: a pair is flagged once it has at least
`MIN_PAIR_DEFAULTS_TO_FLAG` (3) defaults *and* those defaults represent at
least `COLLUSION_PAIR_SHARE_FLAG_BPS` (50%) of the LP's total defaults —
i.e. one payer absorbing a disproportionate share of a single LP's losses.
The flag is deliberately conservative (threshold + concentration) to avoid
false-positive alerts while still catching serial-collusion patterns; the
design and its limits are discussed in [ADR-014](adr/ADR-014-pair-default-tracking.md).

The crate's test suite (`cargo test -p insurance_pool`) covers initialization,
enrollment, premium accumulation, tiered and balance-capped payouts,
idempotency, the empty-pool and invalid-amount rejection paths, risk-priced
premiums, timelocked admin actions, pool-health estimation, and the
scale/precision boundary checks described below.

### Tiered coverage boundaries (Issue #528)

`get_tiered_coverage(lp)` scales the flat `coverage` cap (set at `initialize`,
adjustable via the timelocked `propose_coverage_change` or the no-timelock
`set_coverage_via_governance`) by how much premium `lp` has paid **over the
pool's lifetime** (`get_premiums_paid(lp)`, cumulative, never decays):

| LP's cumulative premiums paid (as % of `coverage`) | Coverage multiplier | Rationale |
|---|---|---|
| < 10% | 50% | Minimal stake in the pool — reduced protection discourages depositing just enough to qualify. |
| 10% – 25% | 75% | Moderate, ongoing commitment. |
| 25% – 50% | 100% | Full flat-cap protection — the "baseline" tier most LPs should target. |
| ≥ 50% | 150% | Heavy contributors are over-protected relative to the flat cap, since their premiums have materially built up the pool's own solvency. |

Implementation (`contracts/insurance_pool/src/lib.rs::get_tiered_coverage`):
boundaries are computed as `coverage / 10`, `coverage / 4`, `coverage / 2`,
and each branch uses `>=`, so a premium total sitting *exactly* on a
threshold already belongs to the tier above it, not the one below.

This uses **premiums paid**, not pool balance or claim history, as the proxy
for an LP's stake — deliberately simple for the stub. A production version
would likely also weight remaining pool solvency (see
[Follow-up work](#follow-up-work-before-mainnet)) so tier eligibility can't
outrun what the pool can actually pay out.

#### Verified at scale (precision and i128 boundaries)

`tiered_coverage_low/medium/high/very_high_premiums` exercise the four tiers
at the crate's small test-fixture coverage (1_000_000_000 stroops). Three
additional tests confirm the same boundary logic holds at magnitudes the
fixture doesn't reach:

- **`tiered_coverage_boundaries_hold_at_realistic_mainnet_scale`** — re-runs
  all four tiers, and each exact threshold ± 1, across coverage caps from
  $1,000 to $10,000,000 equivalent (assuming 7-decimal stroops, i.e.
  1e10–1e14), including one non-round value, confirming no integer-division
  truncation issue distorts a boundary at realistic magnitudes.
- **`tiered_coverage_resolves_top_tier_with_i128_scale_premiums`** — an LP
  with cumulative premiums near `i128::MAX` (mirroring
  `deposit_premium_at_i128_max_overflows`'s technique) against a
  $10,000,000-equivalent coverage cap still resolves cleanly to the top tier;
  the `>=` threshold comparison itself has no overflow risk regardless of how
  large `premiums_paid` grows, since comparison isn't arithmetic.
- **`tiered_coverage_overflows_past_the_i128_safe_bound`** — the top tier's
  payout, `(coverage * 150) / 100`, requires the intermediate product
  `coverage * 150` to fit in `i128`. That means `coverage` itself must stay
  below `i128::MAX / 150` (≈ 1.13 × 10³⁶ stroops, ≈ 1.13 × 10²⁹ dollars at
  7-decimal stroops) for `get_tiered_coverage` to be computable at all — about
  10²² times the $10,000,000 ceiling exercised above, so this is a
  defensive/theoretical bound rather than an operational concern under any
  sane governance-set coverage cap. Soroban's checked arithmetic panics on
  overflow, and the generated client's `try_*` methods surface that as a
  trapped `Err` rather than corrupting state or silently wrapping — the test
  confirms both that the bound is exactly where the math says it should be
  (safe at `i128::MAX / 150`, erroring just past it) and that governance is
  not expected to enforce an explicit upper bound on `coverage` today.

## Aggregate exposure invariant (Issue #662)

`insurance_pool` and `invoice_liquidity` interact via
[`tests_insurance_integration.rs`](../contracts/invoice_liquidity/src/tests_insurance_integration.rs),
but nothing in either contract tracks the *aggregate* exposure the pool has
implicitly committed to across every enrolled LP. This section makes that
explicit.

### What "exposure" means here

An LP becomes "exposed" coverage the moment they're enrolled: if the invoice
they funded defaults, they're eligible for up to `get_tiered_coverage(lp)`
(their tier-scaled cut of the flat `coverage` cap — see
[Tiered coverage boundaries](#tiered-coverage-boundaries-issue-528) above).
Summed across every enrolled LP, that's the pool's *aggregate nominal
exposure* — what it would owe if every enrolled LP's funded invoice defaulted
in the same ledger.

### Chosen policy: best-effort, pro-rata by claim order — no enrollment cap

The pool does **not** cap total enrolled coverage against its balance. There
is no check at `enroll()` / `deposit_premium()` time that aggregate nominal
exposure stays under `pool_balance` (or under `BalanceCap`, which bounds
balance *growth*, not exposure). Coverage is explicitly **best-effort**:

- Each individual `claim()` is capped by `min(tiered_coverage(lp),
  pool_balance)` — already covered by `claim_pays_coverage_capped_by_balance_and_transfers_tokens`
  in `contracts/insurance_pool/src/test.rs` and by
  [Invariant S1](formal-verification-insurance.md#2-invariant-s1--pool-balance-is-never-negative)
  in the formal verification spec.
- Under simultaneous defaults whose combined tiered coverage exceeds the
  pool's balance, claims are paid **in call order, first-come-first-served,
  each capped by whatever balance remains** — not pro-rata by percentage.
  The first claim(s) processed can receive their full tiered coverage while
  later claims in the same batch receive a partial payout, or nothing, once
  the balance is exhausted. This is a direct consequence of `claim()`'s
  per-call `min(coverage, balance)` logic and how `invoice_liquidity`'s
  `claim_default` invokes it once per invoice as defaults are reported — it
  has no batching or ordering logic of its own.
- **This is a deliberate simplification, not a bug.** Implementing a true
  reservation/allocation system (tracking committed-but-unclaimed exposure
  per LP and admitting new enrollments or premium tiers only while aggregate
  exposure stays under balance) is real design and implementation work,
  tracked as follow-up below rather than attempted here.

### Why not cap enrollment instead

An enrollment-time cap (reject `deposit_premium` / tier upgrades once
aggregate exposure would exceed balance) was considered and rejected for this
iteration:

- Tier eligibility today is a pure function of the calling LP's own premiums
  paid (`get_tiered_coverage`); computing "aggregate exposure so far" would
  require a new running total updated on every enrollment, tier change, and
  coverage-cap change — extra state and extra invariants to keep consistent,
  for a stub contract already flagged as pre-mainnet.
- A hard cap would let an early wave of LPs lock out later ones from
  enrolling at all once nominal exposure hits balance, even though actual
  simultaneous-default risk is typically far lower than nominal exposure.
- Pro-rata-by-balance at claim time (this section's chosen policy) already
  guarantees the pool itself can never overpay ([Invariant
  S1](formal-verification-insurance.md#2-invariant-s1--pool-balance-is-never-negative)),
  which is the property that actually matters for solvency; capping
  enrollment only changes *whether* a claim gets a full or partial payout,
  not whether the pool stays solvent.

### Stress test

`contracts/invoice_liquidity/src/tests_insurance_integration.rs::test_insurance_pool_degrades_gracefully_under_simultaneous_default_stress`
enrolls several LPs whose combined tiered coverage exceeds the pool's
balance, defaults all of their funded invoices in the same test (simulating
simultaneous defaults), and asserts:

- No panic — every `claim_default` call completes.
- `get_pool_balance()` never goes negative and ends at exactly `0` once the
  balance is exhausted (no claim can ever pay out more than what remains).
- The sum of all insurance payouts across every LP never exceeds the pool's
  starting balance.
- At least one claim (a later one, once balance is exhausted) receives less
  than its full tiered coverage, or `0` — confirming the graceful,
  first-come-first-served degradation this section describes, not an
  incorrect (over-)payout.

### Follow-up

Tracked in [Follow-up work](#follow-up-work-before-mainnet) below: a real
reservation/allocation system, if aggregate nominal exposure ever needs to be
bounded rather than left best-effort.

## Integration with `invoice_liquidity` (Issue #529)

The compensation hook lives on the liquidity contract's default-handling path
(`claim_default`), implemented directly in
`contracts/invoice_liquidity/src/lib.rs`. The design:

1. `invoice_liquidity` depends on the `insurance_pool` crate directly (a
   regular Cargo dependency, not just dev-only) so it can use the generated
   `InsurancePoolInterfaceClient` for typed cross-contract calls. The deployed
   pool address is stored as a new `DataKey::InsurancePool` instance key, set
   via the admin-gated `set_insurance_pool(pool)` / read via
   `get_insurance_pool()`.
2. After a default is confirmed for `invoice_id` (invoice marked `Defaulted`,
   funders refunded their principal), `claim_default` checks whether the
   *claiming* LP (the caller) is enrolled and, if so, attempts to claim on
   their behalf:

```rust
// inside claim_default(), after the principal refund loop:
if let Some(pool_addr) = crate::storage::get_insurance_pool(&env) {
    let pool_client = InsurancePoolInterfaceClient::new(&env, &pool_addr);
    let enrolled = matches!(pool_client.try_is_enrolled(&funder), Ok(Ok(true)));
    if enrolled {
        let (compensated, payout) = match pool_client.try_claim(&invoice_id, &funder) {
            Ok(Ok(payout)) => (true, payout),
            _ => (false, 0),
        };
        env.events().publish(
            (Symbol::new(&env, "insurance_claim_attempted"), invoice.id, funder.clone()),
            InsuranceClaimAttempted { invoice_id: invoice.id, lp: funder.clone(), compensated, payout },
        );
    }
}
```

3. The pool is configured with the liquidity contract's own address as its
   `admin`, so only a genuine confirmed default (the liquidity contract
   authorizing itself) can trigger `claim`. `claim()` transfers the payout
   directly from the pool's balance to the LP — `invoice_liquidity` never
   holds or forwards insurance funds itself.
4. **Graceful degradation**: the pool calls use `try_is_enrolled` /
   `try_claim` rather than the panicking variants. If the pool is paused,
   empty, unreachable, or the invoice was already claimed, `claim_default`
   still completes successfully (the principal refund and status update
   already happened, in the same atomic invocation) — it just reports
   `compensated: false` instead of reverting the whole default over an
   optional insurance top-up.

Tests covering this integration (using the real `insurance_pool` contract,
not a mock) are in `contracts/invoice_liquidity/src/tests_insurance_integration.rs`.

## SDK Integration

The `@iln/sdk` TypeScript package provides convenience methods to interact with the insurance pool:

### Querying pool status

```typescript
import { ILNClient } from "@iln/sdk";
import { Networks } from "@stellar/stellar-sdk";

const client = ILNClient.testnet(mySigner);

const poolBalance = await client.getPoolBalance(
  client.rpc,
  insurancePoolAddress
);

const coverage = await client.getCoverage(
  client.rpc,
  insurancePoolAddress
);

const isEnrolled = await client.isEnrolled(
  client.rpc,
  insurancePoolAddress,
  lpAddress
);

const premiumsPaid = await client.getPremiumsPaid(
  client.rpc,
  insurancePoolAddress,
  lpAddress
);
```

### Convenience methods

The SDK provides shorter method names for common queries:

```typescript
// Convenience wrapper for isEnrolled(...)
const enrolled = await client.isInsuranceEnrolled(
  client.rpc,
  insurancePoolAddress,
  lpAddress
);

// Convenience wrapper for getPremiumsPaid(...)
const premiums = await client.getInsurancePremiums(
  client.rpc,
  insurancePoolAddress,
  lpAddress
);
```

### Querying LP pool info

Fetch enrollment status, pool balance, coverage cap, and premiums paid in one call:

```typescript
const poolInfo = await client.getInsurancePoolInfo(
  client.rpc,
  insurancePoolAddress,
  lpAddress
);

console.log(`
  Enrolled: ${poolInfo.isEnrolled}
  Premiums paid: ${poolInfo.premiumsPaid}
  Pool balance: ${poolInfo.poolBalance}
  Coverage cap: ${poolInfo.coverage}
`);
```

### Enrolling in the pool

```typescript
import { Keypair } from "@stellar/stellar-sdk";

const lp = Keypair.fromSecret(lpSecretKey);
const sourceAccount = await client.rpc.getAccount(lp.publicKey());

const { txHash } = await client.enrollInsurancePool(
  client.rpc,
  insurancePoolAddress,
  lp.publicKey(),
  sourceAccount,
  (tx) => {
    tx.sign(lp);
    return tx;
  }
);

console.log(`Enrolled in insurance pool: ${txHash}`);
```

### Depositing premiums

Auto-enrolls the LP on first payment.

```typescript
const { txHash } = await client.depositInsurancePremium(
  client.rpc,
  insurancePoolAddress,
  lpAddress,
  premiumAmount,
  sourceAccount,
  (tx) => {
    tx.sign(lp);
    return tx;
  }
);

console.log(`Premium deposited: ${txHash}`);
```

### Filing a claim (admin-only)

In production, the `invoice_liquidity` contract is the pool admin and files claims automatically on confirmed defaults. For testing or standalone use:

```typescript
// Only the pool admin can call claim
const adminKeypair = Keypair.fromSecret(adminSecretKey);
const adminAccount = await client.rpc.getAccount(adminKeypair.publicKey());

const { txHash, payout } = await client.claimInsurance(
  client.rpc,
  insurancePoolAddress,
  invoiceId,
  adminAccount,
  (tx) => {
    tx.sign(adminKeypair);
    return tx;
  }
);

console.log(`Claim filed for invoice ${invoiceId}: payout ${payout} stroops`);
```

---

## Completed work

- ~~Real SAC token custody for premiums and payouts.~~ Done (Issue #527).
- ~~Risk-priced premiums and coverage (vs. flat cap).~~ Done (Issue #528) —
  see [Tiered coverage boundaries](#tiered-coverage-boundaries-issue-528).
- ~~Solvency circuit breaker.~~ Done (Issue #826) — a governance-configurable
  minimum reserve ratio pauses new claims when breached and requires an
  explicit `reset_solvency_circuit` to resume; enrollments and premiums are
  never blocked.
- ~~Capital backstop fund.~~ Done (Issue #827) — protocol-owned backstop
  balance funded by a configurable premium share and/or governance top-ups,
  drawn after liquid balance on claims; see [ADR-013](adr/ADR-013-capital-backstop.md).
- ~~Claim evidence hashing + review window.~~ Done (Issue #828) — advisory
  evidence attachment always available; a review-wave gate is opt-in per risk
  tier via `set_review_window_seconds`.
- ~~Per-pair default tracking for collusion detection.~~ Done (Issue #829) —
  raw per-(lp, payer) counters plus an on-chain heuristic flag; see
  [ADR-014](adr/ADR-014-pair-default-tracking.md).
- Pool solvency guards and payout prioritization across simultaneous defaults
  — tier eligibility (above) is based on premiums paid only and doesn't yet
  weight remaining pool balance, so a pool near-depleted by prior claims could
  still nominally owe a top-tier LP more than it can pay (`claim` does clamp
  the actual payout to `pool_balance`, but tier *eligibility* itself doesn't
  account for solvency). See [Aggregate exposure invariant (Issue
  #662)](#aggregate-exposure-invariant-issue-662) for the chosen best-effort
  policy and stress-test coverage of this exact scenario — a real
  reservation/allocation system that bounds aggregate exposure remains a
  follow-up, not attempted here. The new solvency breaker (Issue #826) is a
  *complementary* guard: it pauses payouts when the reserve ratio is too thin,
  rather than changing payout prioritization.
- Governance parameters (premium schedule, coverage ratio).
- End-to-end integration tests across `invoice_liquidity` ⇄ `insurance_pool`.
