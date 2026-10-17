# Cross-Contract Proof Obligations

**Issue:** #1100 — publish cross-contract proof obligations
**Specification:** [`formal-verification-cross-contract.md`](formal-verification-cross-contract.md)
**Machine-readable registry:** [`proof-obligations.json`](proof-obligations.json)
**Harness:** `contracts/tests/cross_contract_invariants_test.rs`

This document publishes the **system-level** proof obligations for the ILN
contract set (`invoice_liquidity` ↔ `iln_distribution` ↔ `insurance_pool`) in a
form that is both human-reviewable and machine-checkable. The narrative proof
sketches live in
[`formal-verification-cross-contract.md`](formal-verification-cross-contract.md);
this file is the binding **registry** — the source of truth for *which
obligation is discharged by which test*.

Every obligation carries:

- a stable `id` (`X1`–`X5`),
- the invariant `property`,
- the executable `source_file` / `source_symbols` it constrains,
- the `test_file` / `verified_by` integration test that discharges it.

## How the registry is checked

The registry is validated by two independent mechanisms so it cannot silently
drift:

1. **CI script** — [`../scripts/check-proof-obligations.py`](../scripts/check-proof-obligations.py)
   (wired into the `formal-verification` workflow, Issue #1101).
2. **Rust test** — `proof_obligations_registry_is_consistent` in
   `contracts/iln_governance/src/tests_proof_obligations.rs`, runnable via
   `cargo test -p iln_governance --lib`.

Both check the same invariants:

- every `source_file` and `test_file` exists in the tree;
- every `source_symbols` entry actually occurs in its `source_file`
  (the obligation still maps to live code);
- every `verified_by` name occurs as an `fn` in the cited `test_file`;
- every `#[test]` in each cited `test_file` is claimed by exactly one
  obligation — **no orphan obligations, no unclaimed tests**.

## Registry

| ID | Obligation | Code path (`source_symbols`) | Verified by |
|----|-----------|------------------------------|-------------|
| **X1** | Funding notifies distribution | `set_distribution_contract`, `fund_invoice`, `notify_distribution_funding` | `x1_funding_increases_lp_distribution_accrual` |
| **X2** | Settlement notifies distribution | `mark_paid`, `notify_distribution_settlement` | `x2_settlement_increases_freelancer_distribution_accrual` |
| **X3** | Insurance payout bounded by pool + enrollment | `claim_default`, `get_insurance_pool`, `set_insurance_pool` | `x3_default_claim_reduces_insurance_pool_by_payout` |
| **X4** | Escrow + reserves ≤ tracked inflows (conservation) | `fund_invoice`, `claim_default` | `x4_payment_token_conserved_across_iln_and_insurance` |
| **X5** | Insurance compensation single-shot per invoice | `claim_default` | `x5_insurance_compensation_not_paid_twice_for_same_invoice` |

## Before / after

| | Before (#1100) | After |
|---|---|---|
| Obligation ↔ test mapping | narrative only, in prose | machine-readable registry (`proof-obligations.json`) |
| Drift detection | none — a renamed/removed test or symbol was invisible | CI + Rust test fail if a cited symbol/test disappears or a test is unclaimed |
| Review surface | two documents, no cross-check | one registry, checked against the tree on every run |

## Rollout, recovery, and upgrade impact

- **No on-chain state.** This change adds documentation, a registry, a Python
  checker, and a Rust test. There is no contract storage or WASM change, so
  there is no migration or upgrade action.
- **Rollout.** Merge with the accompanying tests; the checker runs in CI and
  can also be run locally (`python3 scripts/check-proof-obligations.py`).
- **Recovery.** If an obligation is intentionally retired, remove it from both
  the registry and the spec in the same PR; the orphan check will flag any test
  left unclaimed.
- **Residual risk.** X1–X5 are discharged by integration tests that execute
  real contracts under `soroban-sdk` testutils; they are evidence, not a
  mechanised proof. See §7 of
  [`formal-verification-cross-contract.md`](formal-verification-cross-contract.md)
  for the residual-risk discussion.

## See also

- [`formal-verification-cross-contract.md`](formal-verification-cross-contract.md) — invariant specifications X1–X5
- [`formal-verification.md`](formal-verification.md) — per-contract lifecycle & governance invariants
- [`storage-layout.md`](storage-layout.md) — storage keys referenced by the code paths above
