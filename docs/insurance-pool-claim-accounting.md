# Insurance Pool — Claim Accounting Conservation

Scope: `contracts/insurance_pool` — the `claim` payout path and the counters it
moves. This document records the assumptions, data sources, authorization
rules, and failure behaviour behind the conservation tests added for Issue
#1195, and separates verified findings (VF) from residual risk (RR).

The change is test-only: `contracts/insurance_pool/src/test.rs`. No API,
storage key, or storage layout is modified, so no migration is required.

## 1. Entry point under test

`claim(env, invoice_id: u64, lp: Address) -> i128` (lib.rs). It is the only
function that pays a claim out of the pool.

| Step | Action | On failure |
| --- | --- | --- |
| 1 | `require_admin` (→ `admin.require_auth()`) | host auth error |
| 2 | `is_claimed(invoice_id)` | `AlreadyClaimed` (2) |
| 3 | `gate_solvency_circuit` | `SolvencyCircuitOpen` (10) |
| 4 | `enforce_claim_review_gate` | `EvidenceRequired` / `ReviewWindowNotElapsed` |
| 5 | read `Balance`, `BackstopBalance`; `available = balance + backstop` | if `available == 0` → `PoolEmpty` (4) |
| 6 | `coverage = get_tiered_coverage(lp)`; `payout = min(coverage, available)` | — |
| 7 | `from_balance = min(payout, balance)`; `from_backstop = payout - from_balance` | — |
| 8 | write `Balance`, `BackstopBalance`, `Claimed(invoice_id) = true` | — |
| 9 | `token.transfer(pool → lp, payout)` | host reverts whole invocation |
| 10 | emit `claimed`; `trip_circuit_if_breached` | — |

Steps 1–4 run **before any storage write**, so every rejection is state-free by
construction. Steps 8–9 follow checks-effects-interactions: the counters are
written before the external token call.

## 2. Actors and authorization

- **Admin** — the only caller allowed to invoke `claim`. `require_admin`
  resolves the configured admin and calls `admin.require_auth()`. In production
  the admin is the liquidity contract (it reports confirmed defaults).
- **`lp` argument** — the payout recipient and the key used to select a coverage
  tier. It is **not** authenticated on this path; the admin is trusted to pass
  the genuine defaulted LP.
- **LP** — authenticates on `enroll` / `deposit_premium` (see
  `docs/insurance-pool-enrollment-claims.md`), not on `claim`.

## 3. Data sources (storage keys)

| Key | Storage | Meaning |
| --- | --- | --- |
| `Balance` | instance | liquid reserve available to claims |
| `BackstopBalance` | instance | capital backstop, drawn only after liquid |
| `Claimed(u64)` | persistent | per-invoice idempotency flag |
| `Premiums(lp)` | persistent | cumulative premiums paid → tier selection |
| coverage cap | instance | flat per-claim cap set at `initialize` |

`get_tiered_coverage(lp)` maps cumulative `Premiums(lp)` against the coverage cap
to 50% / 75% / 100% / 150% of the cap (tier 1–4). It is the *allocated coverage*
bound.

## 4. Conservation properties (invariants)

- **INV-CLAIM1 — coverage cap.** `payout <= tiered_coverage(lp)`.
- **INV-CLAIM2 — reserve cap.** `payout <= balance_before + backstop_before`.
- **INV-CLAIM3 — no value created.** `balance_after + backstop_after + payout
  == balance_before + backstop_before`.
- **INV-CLAIM4 — draw order.** `from_balance = min(balance, payout)`, so liquid
  is fully drained before the backstop absorbs any shortfall; the backstop
  decreases by `payout - balance_before` at most.
- **INV-CLAIM5 — rejection is state-free.** On any rejection, `Balance`,
  `BackstopBalance`, and `Claimed(invoice_id)` are unchanged.

## 5. Verified findings (VF)

- **VF-1** Payouts are bounded by both the allocated tiered coverage *and* the
  total reserve; whichever is smaller wins. (tests:
  `claim_payout_is_capped_by_tiered_coverage_when_pool_is_larger`,
  `claim_payout_is_capped_by_total_reserve_when_coverage_exceeds_it`.)
- **VF-2** Value is conserved across the payout: reserve drops by exactly the
  payout and never goes negative. (tests: both VF-1 tests plus
  `claim_draws_liquid_first_then_backstop_conserving_counters`.)
- **VF-3** Liquid reserve is drained before the backstop is touched; the
  backstop only absorbs the shortfall. (test:
  `claim_draws_liquid_first_then_backstop_conserving_counters`.)
- **VF-4** Rejections (`AlreadyClaimed`, `PoolEmpty`, `SolvencyCircuitOpen`)
  occur before any counter write, leaving `Balance`, `BackstopBalance`, and
  `Claimed` untouched. (tests:
  `already_claimed_rejection_leaves_counters_unchanged`,
  `pool_empty_claim_rejection_leaves_counters_unchanged`,
  `circuit_gated_claim_rejection_leaves_counters_unchanged`.)

## 6. Residual risk (RR)

- **RR-1 — unauthenticated recipient.** `claim` authenticates the admin but not
  the `lp` recipient. A compromised or buggy admin could route a payout to an
  arbitrary address. Bounded by the production assumption that the admin is the
  liquidity contract.
- **RR-2 — forced transfer failure not exercisable in-tree.** The "failed payout
  rolls back counters" property relies on Soroban host atomicity (a trap reverts
  the entire invocation) plus the checks-effects-interactions ordering. The
  in-tree mock Stellar Asset Contract offers no supported way to make
  `transfer` trap deterministically (no clawback/freeze lever exposed on this
  path), so this is asserted structurally rather than by a forced failure.
- **RR-3 — counters are not reconciled to token balance.** `Balance` /
  `BackstopBalance` are accounting counters. There is no sweep/reconciliation
  entrypoint, so tokens transferred directly to the contract address would not
  be credited and the counters would drift from the real token balance.
- **RR-4 — tier is derived from recorded premiums only.** `get_tiered_coverage`
  reads the cumulative `Premiums(lp)` recorded at deposit time; it does not
  re-derive from any external source.

## 7. Test coverage map (Issue #1195)

| Test | Property |
| --- | --- |
| `claim_payout_is_capped_by_tiered_coverage_when_pool_is_larger` | INV-CLAIM1, INV-CLAIM3 (VF-1, VF-2) |
| `claim_payout_is_capped_by_total_reserve_when_coverage_exceeds_it` | INV-CLAIM2, INV-CLAIM3 (VF-1, VF-2) |
| `claim_draws_liquid_first_then_backstop_conserving_counters` | INV-CLAIM3, INV-CLAIM4 (VF-2, VF-3) |
| `already_claimed_rejection_leaves_counters_unchanged` | INV-CLAIM5 (VF-4) |
| `pool_empty_claim_rejection_leaves_counters_unchanged` | INV-CLAIM5 (VF-4) |
| `circuit_gated_claim_rejection_leaves_counters_unchanged` | INV-CLAIM5 (VF-4) |

These complement the pre-existing property test
`prop_claims_never_exceed_deposits` (INV: `sum(claims) <= sum(premiums)`) and
`prop_claim_idempotent_per_invoice`.
