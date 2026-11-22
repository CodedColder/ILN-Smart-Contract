# Insurance Pool — Enrollment, Premium, and Claim Authorization

Issue **#1194** — audit of enrollment, premium, claim, repeated-claim, and
unauthorized-payout behaviour in `contracts/insurance_pool/src/lib.rs`.

This document records the *observed* behaviour of the shipped implementation,
the authorization rules that gate each state transition, the failure mode of
each rejection, and a split between **verified findings (VF)** and **residual
risk (RR)**. It does not assume a vulnerability exists: every statement below
was read from the source, not inferred.

Related design references:

* `docs/insurance-pool-design.md` — integration design and follow-up work.
* `docs/insurance-pool-launch-parameters.md` — launch parameter envelopes.
* `docs/formal-verification-insurance.md` — formal/invariant coverage.
* `docs/adr/ADR-013-capital-backstop.md`, `docs/adr/ADR-014-pair-default-tracking.md`,
  `docs/adr/ADR-015-reputation-premium-integration.md`.

---

## 1. Scope

In scope: the pool contract's public entry points that move membership,
accounting, or funds:

| Entry point | Module | Mutates |
| --- | --- | --- |
| `initialize` | `lib.rs` | `Admin`, `Balance`, `Coverage`, `TokenAddress`, `InitializedAt` |
| `enroll` | `lib.rs` (`InsurancePoolInterface`) | `Enrolled(lp)`, `EnrolledCount` |
| `deposit_premium` | `lib.rs` (`InsurancePoolInterface`) | `Premiums(lp)`, `Balance`, `BackstopBalance` |
| `claim` | `lib.rs` (`InsurancePoolInterface`) | `Balance`, `BackstopBalance`, `Claimed(invoice_id)` |

Out of scope: the risk-price curve, capital backstop economics, collusion
heuristics, and reputation integration — those carry their own issues
(#528/#827/#829/ADR-015) and are only touched here where they intersect the
authorization surface.

The pool is a **pre-mainnet design-forward stub**. There is no deployed storage,
so the storage-layout fix in §3 requires no migration (see §10).

---

## 2. Actors and trust model

| Actor | Authority in this contract |
| --- | --- |
| **LP** | Signs `enroll` and `deposit_premium` for *its own* address only. Cannot trigger a payout. |
| **Admin** | The address set at `initialize`. In production this is the `invoice_liquidity` contract acting as the configured admin. Signs `claim`; also signs all governance setters (`set_*`, `propose_*`). |
| **Anyone** | All read-only getters (`is_enrolled`, `get_pool_balance`, `get_pool_health`, `get_premiums_paid`, `is_claimed`, …). |

The security-critical asymmetry: **premiums in are LP-signed, payouts out are
admin-signed.** An LP cannot self-serve a payout, and the admin cannot move an
LP's funds *into* the pool without that LP's signature.

---

## 3. Data sources and storage keys

All keys are `DataKey` variants. Instance storage holds pool-wide scalars;
persistent storage holds per-LP / per-invoice state.

| Key | Storage | Written by | Meaning |
| --- | --- | --- | --- |
| `Admin` | instance | `initialize` | Address authorized to `claim` |
| `Balance` | instance | `initialize`, `deposit_premium`, `claim` | Liquid claim reserve |
| `Coverage` | instance | `initialize` | Flat per-claim cap (tiered from this) |
| `TokenAddress` | instance | `initialize` | Settlement token |
| `Enrolled(lp)` | persistent | `mark_enrolled` | Membership flag |
| `EnrolledCount` | instance | `mark_enrolled` | Distinct-enrolled counter (never derived) |
| `Premiums(lp)` | persistent | `deposit_premium` | Lifetime premium paid |
| `Claimed(invoice_id)` | persistent | `claim` | Per-invoice idempotency key |
| `BackstopBalance` | instance | `deposit_premium`, `claim` | Capital backstop accounting balance |
| `BackstopFundingBps` | instance | `set_backstop_funding_bps` | Premium share diverted to backstop |

**Verified finding (VF-1) — duplicate `DataKey` variants (build blocker).**
`PendingRiskMultiplierNumerator`, `PendingRiskMultiplierDenominator`, and
`RiskMultiplierEta` were each declared **twice** in the `DataKey` enum (once
grouped with the other risk-multiplier keys, and again after
`ReputationContract`). Two variants with the same name are a hard Rust error
(`E0428`), so the `insurance_pool` crate — and every crate that depends on it,
including `invoice_liquidity` — could not compile on `dev`. The duplication was
introduced by the merge that combined commit `a6e8f82` ("restore build broken
by risk-multiplier timelock merge", which added the trailing set) with a branch
carrying the grouped set.

This change removes the **trailing** duplicate set. The three keys remain at
their grouped positions (adjacent to `RiskMultiplierNumerator` /
`RiskMultiplierDenominator`), every other variant keeps its existing index, and
no variant is renamed — so there is no API or storage-shape change beyond
deleting the illegal duplicate (see §10 and §11).

---

## 4. Authorization matrix

| Entry point | Required signature | Enforced by | Notes |
| --- | --- | --- | --- |
| `initialize` | `admin` | `admin.require_auth()` | Also rejects re-init (`AlreadyInitialized`) |
| `enroll(lp)` | `lp` | `lp.require_auth()` | Free; no payment required |
| `deposit_premium(lp, amount)` | `lp` | `lp.require_auth()` | Auto-enrolls; `amount > 0` |
| `claim(invoice_id, lp)` | `Admin` | `require_admin` → `admin.require_auth()` | Idempotent per invoice |

`require_admin` reads `DataKey::Admin` from instance storage and calls
`require_auth()` on the *stored* admin. If the pool is uninitialized it panics
with `NotInitialized` instead of silently succeeding.

**VF-2 — no authorization gap found.** Enrollment and premium deposit are
gated on the LP's own signature; payout is gated on the admin's signature. The
two directions (money in vs money out) require different, non-interchangeable
signers. This is confirmed by the new tests in §9.

---

## 5. Enrollment behaviour (`enroll`)

1. `lp.require_auth()` — the caller must be the LP being enrolled.
2. `mark_enrolled(&env, &lp)`:
   * reads `Enrolled(lp)` (persistent);
   * if already `true`, returns without touching `EnrolledCount`;
   * otherwise sets `Enrolled(lp) = true` and increments `EnrolledCount` by 1.
3. Emits `("enrolled", lp)`.

Observed properties:

* **Self-only.** A third party cannot enroll someone else; the signature check
  fails first.
* **Idempotent.** Repeated `enroll` calls are a no-op and do not inflate the
  enrolled counter.
* **Free.** Enrollment requires no premium payment. Membership alone grants
  *claim eligibility*, but not a payout — every payout still requires the
  admin-signed `claim` (§7).
* **Failure behaviour.** A missing/incorrect signature surfaces as an auth
  error from `try_enroll`; no storage is written (the check precedes the first
  write).

---

## 6. Premium behaviour (`deposit_premium`)

Order of operations:

1. `lp.require_auth()`.
2. Reject `amount <= 0` → `InvalidAmount` (before any state change).
3. `mark_enrolled` (auto-enroll on first premium).
4. `Premiums(lp) = Premiums(lp) + amount` (checked; `ArithmeticOverflow`).
5. Split the deposit: `backstop_share = amount * BackstopFundingBps / 10_000`,
   `liquid_share = amount - backstop_share`.
6. `Balance += liquid_share` (checked; `ArithmeticOverflow`); if a `BalanceCap`
   is set and would be exceeded → `BalanceCapExceeded`.
7. If `backstop_share > 0`, `BackstopBalance += backstop_share` and emit
   `("back_top", …)`.
8. `token.transfer(lp → pool, amount)`.
9. Emit `("premium", lp, amount)`.

Observed properties:

* **Self-only.** Only the LP can pay its own premium; the signature check is
  the first statement, so an impostor cannot move another LP's tokens.
* **Checks-effects-interactions.** All storage writes (steps 4–7) complete
  before the external token transfer (step 8). A revert in the transfer rolls
  back the whole invocation, so accounting can never diverge from token
  balances.
* **Failure behaviour.** `InvalidAmount` (non-positive), `ArithmeticOverflow`
  (accumulator overflow), `BalanceCapExceeded` (governance cap). All are
  raised by panic-with-error *before* the transfer, so none can strand funds.

---

## 7. Claim behaviour (`claim`)

Order of operations:

1. `require_admin(&env)` — only the configured admin may report a confirmed
   default. This is the **first** statement: an unauthorized caller fails here,
   before any read of accounting or any write.
2. `is_claimed(invoice_id)` → `AlreadyClaimed` if already paid.
3. `gate_solvency_circuit` — if the sticky breaker is open, reject with
   `SolvencyCircuitOpen` (before any write).
4. `enforce_claim_review_gate(invoice_id)` — when a review window is
   configured (`> 0`): require submitted evidence (`EvidenceRequired`) and an
   elapsed window (`ReviewWindowNotElapsed`); a `0` window disables the gate.
5. `available = Balance + BackstopBalance`. If `available == 0` → `PoolEmpty`.
6. `coverage = get_tiered_coverage(lp)` — 50 % / 75 % / 100 % / 150 % of
   `Coverage` by the LP's premium band.
7. `payout = min(coverage, available)`; draw from liquid `Balance` first, then
   the backstop (writes `Balance` / `BackstopBalance`).
8. Set `Claimed(invoice_id) = true` (idempotency key) — **before** the transfer.
9. `token.transfer(pool → lp, payout)`.
10. Emit `("claimed", invoice_id, payout)`.
11. `trip_circuit_if_breached` — re-evaluate the reserve ratio *after* the
    payout so the breaker flip persists on the success path.

Observed properties:

* **Admin-only payout.** No LP, and no unrelated address, can initiate a
  payout. This is the primary defence against unauthorized payouts.
* **Idempotent per invoice.** `Claimed(invoice_id)` is written before the
  transfer, so a replayed claim is rejected with `AlreadyClaimed` and cannot
  double-pay.
* **Bounded payout.** `payout` can never exceed the available reserve
  (`Balance + BackstopBalance`) nor the tiered coverage cap.
* **Checks-effects-interactions.** Accounting and the idempotency flag are
  written before the external call; a failed transfer reverts everything.
* **Failure behaviour.** `AlreadyClaimed`, `SolvencyCircuitOpen`,
  `EvidenceRequired`, `ReviewWindowNotElapsed`, `PoolEmpty` — all raised with
  no partial state on the rejection paths.

---

## 8. Repeated claim and unauthorized payout

**Repeated claim.** The second `claim` for the same `invoice_id` hits step 2 of
§7 and panics with `AlreadyClaimed`, before any accounting is read or written.
Verified by `claim_is_idempotent_per_invoice` and
`repeated_claim_moves_no_tokens` (the latter also asserts the LP balance and
pool balance are unchanged across the rejected second call).

**Unauthorized payout.** Every payout path begins with `require_admin`, whose
`admin.require_auth()` is evaluated against the *stored* admin. A caller that
is not that admin — including an enrolled LP, or an arbitrary "impostor" —
fails the signature check first, so no balance is read, no payout is computed,
and `Claimed(invoice_id)` is never set. Verified by
`claim_requires_admin_authorization` and `enrolled_lp_cannot_claim_for_itself`.

---

## 9. Test coverage map

New tests (all in `contracts/insurance_pool/src/test.rs`, Issue #1194 block):

| Test | Verifies |
| --- | --- |
| `enroll_requires_lp_authorization` | An impostor cannot enroll an LP; no state written |
| `enroll_succeeds_with_lp_authorization` | The LP's own signature is accepted |
| `enroll_is_idempotent_and_does_not_inflate_enrolled_count` | Repeated enroll is a no-op; counter stays 1 |
| `deposit_premium_requires_lp_authorization` | An impostor cannot pay an LP's premium; no accounting movement |
| `claim_requires_admin_authorization` | An impostor cannot trigger a payout; invoice not marked claimed |
| `enrolled_lp_cannot_claim_for_itself` | An LP's own signature does not satisfy `claim`'s admin gate |
| `admin_authorized_claim_passes_gate_and_empty_pool_is_rejected` | The admin gate is cleared with admin auth; only `PoolEmpty` then blocks |
| `repeated_claim_moves_no_tokens` | A second claim is rejected and moves no tokens |

Existing tests that already cover adjacent behaviour: `enroll_marks_lp_enrolled`,
`deposit_premium_increases_balance_and_transfers_tokens`,
`deposit_premium_rejects_non_positive_amount`,
`claim_is_idempotent_per_invoice`, `claim_rejects_when_pool_empty`,
`deposit_premium_at_i128_max_overflows`, `deposit_premium_enforces_balance_cap`,
and the solvency / backstop / review-window suites.

> The new authorization tests deliberately avoid `mock_all_auths()` (unlike the
> pre-existing `setup()`), so `require_auth()` is enforced for real. See
> `setup_no_mock_auths` in the test module.

---

## 10. Failure and rejection summary

| Condition | Entry point | Error | State effect |
| --- | --- | --- | --- |
| Pool already initialized | `initialize` | `AlreadyInitialized` | none |
| Coverage ≤ 0 | `initialize` | `InvalidAmount` | none |
| Missing/invalid `lp` signature | `enroll`, `deposit_premium` | auth error | none |
| Missing/invalid `Admin` signature | `claim` | auth error | none |
| `amount <= 0` | `deposit_premium` | `InvalidAmount` | none |
| Premium accumulator overflow | `deposit_premium` | `ArithmeticOverflow` | none |
| Would exceed balance cap | `deposit_premium` | `BalanceCapExceeded` | none |
| Invoice already paid | `claim` | `AlreadyClaimed` | none |
| Sticky breaker open | `claim` | `SolvencyCircuitOpen` | none |
| No evidence submitted | `claim` | `EvidenceRequired` | none |
| Review window not elapsed | `claim` | `ReviewWindowNotElapsed` | none |
| Available reserve is 0 | `claim` | `PoolEmpty` | none |

---

## 11. Verified findings vs residual risk

**Verified findings (VF)**

* **VF-1 — Duplicate `DataKey` variants (`E0428`) blocked compilation.** Fixed
  in this change by deleting the trailing duplicate set (§3).
* **VF-2 — No authorization gap.** Enrollment/premium are LP-signed,
  payout is admin-signed; the two are non-interchangeable (§4, §5, §6, §7).
* **VF-3 — Payout is idempotent per invoice.** `Claimed(invoice_id)` is written
  before the transfer (§7, §8).
* **VF-4 — Unauthorized `claim` mutates nothing.** `require_admin` is the first
  statement, ahead of all reads/writes (§7, §8).
* **VF-5 — Checks-effects-interactions holds on both token-moving paths.**
  Accounting is written before `token.transfer` in `deposit_premium` and
  `claim` (§6, §7).

**Residual risk (RR)**

* **RR-1 — Enrollment is free.** Membership alone is not economically gated;
  it only becomes a payout when the admin-signed `claim` runs. Acceptable
  because payouts are admin-gated, but it means "enrolled" is not a bond.
* **RR-2 — The admin must be trusted to report defaults truthfully.** The pool
  cannot verify that `invoice_id` defaulted or that `lp` funded it; it trusts
  the admin (the liquidity contract) exactly as documented in the interface.
* **RR-3 — `claim` does not require `lp` to be enrolled.** An admin can pay an
  address that never enrolled. Not reachable without admin auth, so it is an
  operator/design risk rather than an authorization gap.
* **RR-4 — No per-LP or global payout cap beyond tiered coverage.** Repeated
  claims across distinct `invoice_id`s can drain the reserve; the solvency
  circuit breaker (`SolvencyCircuitOpenFlag`) is the backstop against this.
* **RR-5 — Review-window gate is evidence-based.** With a non-zero window and
  no evidence submitted, all claims are blocked (`EvidenceRequired`); operators
  must submit evidence through `submit_claim_evidence` (admin-signed).

---

## 12. Operator guidance

* **Bring-up order.** `initialize` (admin) → LPs self-`enroll` / `deposit_premium`
  → governance setters (`set_min_reserve_ratio_bps`, `set_backstop_funding_bps`,
  `set_balance_cap`, `set_review_window_seconds`) as needed.
* **Payouts.** Only the admin (the liquidity contract) calls `claim`; the pool
  never pays on an LP's request. If a review window is enabled, submit evidence
  first and wait out the window.
* **Monitoring.** Watch `get_pool_health` (balance, enrolled count, projected
  claim rate) and `is_solvency_circuit_open`; a tripped breaker is sticky and
  requires `reset_solvency_circuit` (admin) after the reserve is restored.
* **Storage layout.** Removing the duplicate `DataKey` variants (§3) does not
  change any surviving variant's name; because the contract is pre-mainnet
  with no deployed storage, no migration is required. Should the pool ever ship
  to mainnet, treat the `DataKey` ordering as frozen.
