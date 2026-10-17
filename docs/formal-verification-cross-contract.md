# Formal Verification Specification — Cross-Contract System Invariants

## 1. Overview

Per-contract specs cover local safety:

- [`formal-verification.md`](formal-verification.md) — `invoice_liquidity` lifecycle & governance
- [`formal-verification-insurance.md`](formal-verification-insurance.md) — `insurance_pool` solvency
- [`formal-verification-distribution.md`](formal-verification-distribution.md) — `iln_distribution` reward accounting

This document specifies **system-level** invariants that only make sense across
`invoice_liquidity` (ILN), `iln_distribution`, and `insurance_pool` together.
They are verified by integration tests under `contracts/tests/` (wired as
`[[test]]` targets on `invoice_liquidity`), not by single-contract `proptest`
suites — those cannot see foreign contract storage.

**Harness:** `contracts/tests/cross_contract_invariants_test.rs`  
**Run:** `cargo test -p invoice_liquidity --test cross_contract_invariants_test`

**Published registry (Issue #1100):** the binding obligation↔test mapping is
machine-readable in [`proof-obligations.json`](proof-obligations.json), described
in [`proof-obligations.md`](proof-obligations.md), and validated against the tree
by `scripts/check-proof-obligations.py` and
`contracts/iln_governance/src/tests_proof_obligations.rs`. Any change to the
invariants below must update the registry too (the check fails otherwise).

---

## 2. Invariant X1 — Funding notifies distribution (when wired)

**Property:** After `set_distribution_contract` and a successful `fund_invoice`,
`iln_distribution.get_accrual(lp)` is strictly greater than its pre-fund value.

**Why cross-contract:** Accrual lives in `iln_distribution`; the trigger is an
ILN hook (`notify_distribution_funding`). A per-contract ILN test cannot see
the accrual store; a per-contract distribution test cannot see `fund_invoice`.

**Verified by:** `x1_funding_increases_lp_distribution_accrual`

---

## 3. Invariant X2 — Settlement notifies distribution (when wired)

**Property:** After a successful on-time `mark_paid`, freelancer (and payer,
when rates are non-zero) distribution accruals are strictly greater than their
pre-settlement values.

**Why cross-contract:** Settlement state transitions are ILN-local; reward
accrual is distribution-local. The composition is the product guarantee.

**Verified by:** `x2_settlement_increases_freelancer_distribution_accrual`

---

## 4. Invariant X3 — Insurance claim payout is bounded by pool and enrollment

**Property:** For a defaulted, insurance-wired invoice whose LP is enrolled with
a solvent pool: `claim_default` succeeds, and afterward
`insurance_pool.get_pool_balance()` equals the pre-claim balance minus the
paid compensation (or remains unchanged if compensation is zero / not enrolled).
Pool balance never goes negative (composes with insurance S1).

**Why cross-contract:** `claim_default` in ILN invokes `insurance_pool.claim`;
neither side alone proves the paired state update.

**Verified by:** `x3_default_claim_reduces_insurance_pool_by_payout`

---

## 5. Invariant X4 — Escrow + insurance reserves do not exceed tracked inflows

**Property:** In the test harness, let `inflows` be the sum of all payment-token
`mint` amounts credited to actors for the scenario. After any sequence of fund /
premium-deposit / settle / claim operations in the harness:

```text
token.balance(iln) + insurance_pool.get_pool_balance() + Σ user_balances(tracked)
  == inflows
```

i.e. ILN escrow plus insurance reserves plus remaining user balances conserve
minted supply — the pool and escrow cannot manufacture tokens.

**Why cross-contract:** TVL is the sum of balances across contracts and users;
no single contract stores the global conservation predicate.

**Verified by:** `x4_payment_token_conserved_across_iln_and_insurance`

---

## 6. Invariant X5 — Cross-module default compensation is single-shot per invoice

**Property:** After a successful `claim_default` that pulled insurance
compensation for `invoice_id`, a second `claim_default` for the same id fails
(or is a no-op that does not reduce the pool again), and
`insurance_pool` rejects a second `claim(invoice_id)` with already-claimed
semantics when invoked consistently with ILN’s integration.

**Why cross-contract:** Idempotency is specified for the pool (S3) and for ILN
default handling separately; the system property is that the *pair* cannot pay
insurance twice for one invoice.

**Verified by:** `x5_insurance_compensation_not_paid_twice_for_same_invoice`

---

---

## 7. Invariant X6 — Governance execution strictly not before ETA timelock (Issue #1087)

**Property:** A passed governance proposal targeting downstream contracts (`invoice_liquidity`, `iln_distribution`, `insurance_pool`, or `reputation_bonus`) cannot execute before both `now >= voting_end` (voting window guard) and `current_ledger >= eta_ledger` (timelock guard) are satisfied in order:
```text
execute_proposal(id) -> Executed  iff  (now >= voting_end) ∧ (status == Passed) ∧ (current_ledger >= eta_ledger)
```
At any ledger sequence `current_ledger < eta_ledger` (including boundary `eta_ledger - 1`), execution is rejected with `TimelockNotExpired` and no downstream cross-contract calls are dispatched.

**Why cross-contract:** The timelock is tracked in `iln_governance` storage, but its guarantee protects parameter integrity across the target contracts. If governance could bypass `eta_ledger`, timelocked protections on economic parameters (such as `update_fee_rate`, `update_max_discount`, or `upgrade`) would be void.

**Verified by:** `x6_governance_execution_not_before_eta_invariant`

---

## 8. Invariant X7 — Model veto transition safety (Issue #1088)

**Property:** While `VetoPowerEnabled == true`, authorized veto signers (`VetoSigners`) can transition an `Active` or `Passed` proposal directly to `Vetoed`. Once in `Vetoed`:
1. The status is permanently terminal: subsequent votes fail (`ProposalNotActive`) and execution fails (`AlreadyResolved`).
2. Downstream cross-contract actions are never dispatched, even if the proposal previously passed and its timelock elapsed.
3. Terminal states (`Executed`, `Rejected`, `Vetoed`) reject veto calls (`NotVetoable`).
4. Once `disable_veto_power` is executed via the `iln_contract`, subsequent veto attempts fail closed with `VetoPowerDisabled`.

**Why cross-contract:** Veto power acts as an emergency stop for malicious or flawed proposals targeting downstream protocol contracts. Veto safety guarantees that once a proposal is blocked, its actions can never reach the downstream contracts regardless of subsequent ledger progression.

**Verified by:** `x7_governance_veto_transition_safety`

---

## 9. Invariant X8 — Proposal lifecycle authorization (Issue #1089)

**Property:** Every proposal lifecycle state mutation strictly enforces its required authorization predicate:
1. `create_proposal`: Caller requires `proposer.require_auth()` and `proposer_balance >= min_proposal_balance`; otherwise rejected with `InsufficientProposerBalance`.
2. `cast_vote`: Caller requires `voter.require_auth()`, has not already voted on `proposal_id` (rejects with `AlreadyVoted`), and holds an aged balance checkpoint predating the proposal creation by at least `MIN_VOTE_HOLD_LEDGERS` (rejects with `InsufficientHoldingPeriod`).
3. Gated configuration entrypoints (`set_min_quorum_bps`, `set_min_proposal_balance`, `disable_veto_power`): Require `iln_contract.require_auth()` and validate parameter bounds (`1 <= min_quorum_bps <= 10_000`); unauthorized callers cannot tamper with governance parameters.
4. `veto_proposal`: Only authorized signers registered in `VetoSigners` can submit veto approvals.

**Why cross-contract:** Governance parameters and proposal execution govern the configuration of all five protocol contracts. Enforcing authorization across each lifecycle transition ensures malicious external callers cannot trigger unauthorized cross-contract configuration mutations.

**Verified by:** `x8_governance_proposal_lifecycle_authorization`

---

## 10. Residual risks

- Distribution rates of zero trivially satisfy X1/X2 with “no increase”; tests use positive default rates after `initialize`.
- X4 is harness-level conservation for the payment token mock/SAC used in tests, not a consensus proof over every possible token implementation.
- X6/X7/X8 model governance lifecycle invariants and cross-contract parameter execution against `invoice_liquidity`, composing with Part II of `formal-verification.md`.

## 11. Coverage

| Invariant | Verified by | Issue |
|-----------|-------------|-------|
| X1 Funding → LP accrual | `x1_funding_increases_lp_distribution_accrual` | #855 |
| X2 Settlement → freelancer accrual | `x2_settlement_increases_freelancer_distribution_accrual` | #855 |
| X3 Default claim vs pool balance | `x3_default_claim_reduces_insurance_pool_by_payout` | #855 |
| X4 Token conservation | `x4_payment_token_conserved_across_iln_and_insurance` | #855 |
| X5 No double insurance pay | `x5_insurance_compensation_not_paid_twice_for_same_invoice` | #855 |
| X6 Governance execution not before ETA | `x6_governance_execution_not_before_eta_invariant` | #1087 |
| X7 Model veto transition safety | `x7_governance_veto_transition_safety` | #1088 |
| X8 Proposal lifecycle authorization | `x8_governance_proposal_lifecycle_authorization` | #1089 |
