# Cross-Contract Accounting Consistency Audit

Issue #1160. Scope: conservation of token balances and obligations across
`invoice_liquidity` (ILN), `iln_distribution`, `insurance_pool` and the payment
token. Harness:
[`contracts/tests/cross_contract_invariants_test.rs`](../contracts/tests/cross_contract_invariants_test.rs)
(real contracts, `MockToken` payment token).

```sh
cargo test -p invoice_liquidity --test cross_contract_invariants_test
python3 scripts/check-proof-obligations.py
```

Single-contract lifecycle invariants (`[ACC-*]`, `[SET-*]`, `[RB-*]`, `[NE-*]`)
stay in [`invoice-lifecycle-invariants.md`](invoice-lifecycle-invariants.md).
This document adds the **cross-contract** layer and links to those IDs rather
than restating them.

## 1. Assumptions and data sources

* The payment token is SEP-41 and moves exactly the requested amount ([NE-9]).
  `MockToken` is used; fee-on-transfer and rebasing tokens are out of scope.
* Authorization is mocked (`mock_all_auths`, root-only). The suite proves
  accounting and atomicity; **who may call what** is covered by the access
  control suites (`docs/access-control.md`), not here.
* ILN is the sole caller of `iln_distribution.accrue_*` and
  `insurance_pool.claim`; a distribution contract wired to a different ILN
  rejects the call, which the rollback tests use as their failure injection.
* Amounts are 7-decimal USDC-style units; `normalize_usdc_amount` is the
  identity. Discount and fee use floor division, so remainders ("dust") exist.

## 2. Token-flow model

`cost(f) = f - floor(f * rate / 10_000)` for a tranche `f`.

| Operation | Token movements | Net effect on ILN balance |
|---|---|---|
| `fund_invoice` (tranche `f`) | LP -> ILN: `cost(f)`. When this completes the invoice: ILN -> freelancer `amount - discount(amount)` | `+cost(f)` until full; at full: `+ dust`, `0 <= dust < #funders` |
| `mark_paid` partial | payer -> ILN: amount | `+amount` held |
| `mark_paid` completing | payer -> ILN: amount; ILN -> admin: fee; ILN -> each LP: `floor((amount-fee) * f / amount)` | `+dust`, `0 <= dust < #funders` |
| `cancel_invoice` (PartiallyFunded) | ILN -> each LP: `cost(f)` | `-` exactly the deposits |
| `claim_default` | ILN -> each LP: `cost(f)` from ILN's **pooled** balance; pool -> enrolled LP: `payout` | `-sum(cost)` ([NE-2]); pool `-payout` |
| `resolve_dispute(1)` | ILN -> each LP `cost(f)`; ILN -> payer `amount_paid` | `-sum(cost) - amount_paid + (held)` |
| `deposit_premium` | LP -> pool: amount | pool `+amount` |

## 3. Cross-contract invariants (registry IDs)

| ID | Statement | Tests |
|---|---|---|
| X1-X5 | Distribution/insurance notifications and payouts (see [`formal-verification-cross-contract.md`](formal-verification-cross-contract.md)) | `x1_…` – `x5_…` |
| X4 | Supply is conserved at **every step** of a mixed lifecycle across LPs, payer, freelancer, admin, ILN and pool | `x4_payment_token_conserved_across_iln_and_insurance`, `xc_end_to_end_accounting_is_conserved` |
| X9 | A failing distribution hook or token transfer rolls back funding/settlement in all contracts; success applies and accrues once | `xc_funding_rolls_back_…`, `xc_settlement_rolls_back_…`, `xc_token_failure_rolls_back_…` |
| X10 | A failing refund rolls back default/dispute and leaves the pool untouched; a failing insurance claim never blocks a default | `xc_failed_default_…`, `xc_insurance_failure_…`, `xc_insolvent_dispute_…` |
| X11 | Per-actor flows in §2 are exact; dust is bounded; cancel/dispute refund exactly what was deposited/paid | `xc_funding_and_settlement_ledger_…`, `xc_partial_funding_cancel_…`, `xc_upheld_dispute_ledger_…` |
| X12 | With no reserve, ILN balance = refundable deposits + partial payments, and returns to `0` when all are discharged; pool accounting = pool token balance | `xc_iln_balance_equals_outstanding_obligations`, `xc_insurance_pool_token_balance_…` |

## 4. Observable behaviour by case

| Case | Behaviour | Evidence |
|---|---|---|
| Normal | Each ledger moves exactly as in §2; supply conserved after every step | X4, X11, X12 |
| Repeated | Second `mark_paid` -> `AlreadyPaid`; second `claim_default` -> `InvoiceDefaulted`; second `cancel_invoice` -> `AlreadyCancelled`; second pool `claim(invoice)` rejected. Nothing changes. | X5, X9, X11 |
| Stale | Funding/settling/defaulting a terminal invoice is rejected with the status error and no state change | X5, X9 |
| Interrupted | Token or hook failure aborts the whole invocation; ledgers equal their pre-call snapshot. A failed insurance call is tolerated and leaves no claimed marker. | X9, X10 |
| Unauthorized | Not provable here (auth mocked); see access-control suites | residual §6 |

## 5. Findings

**Verified**

* **F-1 — Proof-obligation registry drifted from the tree (fixed).**
  `proof-obligations.json` named five tests (`x1_…`–`x5_…`) that did not exist,
  and seven existing `xc_*` tests were unclaimed, so both
  `scripts/check-proof-obligations.py` and
  `proof_obligations_registry_is_consistent` failed on a clean tree. X1-X5 are
  now implemented, the `xc_*` tests are claimed by X9-X12 (X6-X8 are
  intentionally skipped, see next). Related: `formal-verification-cross-contract.md`
  and `formal-verification.md` cited `x6_…`/`x7_…`/`x8_…` governance tests that
  exist nowhere in the repository; those references now say so. Governance
  timelock/veto/authorization is **not** covered by this harness.
* **F-2 — Reward volume is not reversed by a refund (open, behaviour pinned).**
  `fund_invoice` calls `accrue_lp` per tranche; `iln_distribution` only ever adds
  to `LpFundedVolume`, and `cancel_invoice` / `resolve_dispute` never reverse it.
  A funder whose deposit is fully refunded keeps the volume
  (`xc_refunded_funding_volume_still_accrues_lp_reward`). The freelancer
  controls `cancel_invoice`, so an LP that also submits invoices can cycle
  fund -> cancel at zero principal cost to build claimable reward volume
  (`floor(volume / 100 USDC) * rate`). Severity depends on the value of the
  governance reward; no principal is at risk. Not fixed here (it needs a
  distribution API change); recommended follow-up: a reversal hook
  (`reverse_lp`) on cancel/upheld dispute, or accruing only on full funding.
* **F-3 — Rounding dust is retained by ILN (benign).** Per-tranche discount
  and per-LP share use floor division, so ILN keeps `< #funders` units per
  invoice at funding and again at settlement. It never pays out more than it
  received (`dust >= 0`), asserted exactly in X11. No stranded dust exists for
  single-funder invoices (X12 returns to exactly `0`).

**Known, documented elsewhere**

* [NE-2] default/dispute refunds draw on ILN's pooled balance, not a
  per-invoice escrow; X11 pins the dispute case (reserve absorbs the
  freelancer payout).

## 6. Residual risk

1. Authorization is mocked, so unauthorized-caller behaviour of
   `accrue_*`/`claim` is not exercised cross-contract.
2. Conservation is proven for `MockToken`, not every token implementation.
3. Pool backing (X12) assumes the default backstop configuration path; a
   backstop top-up from outside `deposit_premium` is not modelled.
4. Multi-invoice interference through the pooled balance ([NE-2]) is
   documented, not eliminated: X12 holds only while no default/dispute refund
   is drawn from the pooled balance.
5. Tests are example-based; `tests_lifecycle_invariants.rs` carries the
   property-based coverage for the single-contract layer.
