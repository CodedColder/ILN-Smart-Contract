# Invoice Lifecycle & Accounting Invariants

Issue #1159. This document is the source of truth for the `invoice_liquidity`
invoice state machine as implemented in
[`contracts/invoice_liquidity/src/lib.rs`](../contracts/invoice_liquidity/src/lib.rs)
and [`invoice.rs`](../contracts/invoice_liquidity/src/invoice.rs). It lists only
states and transitions that exist in code. Every invariant has an ID (e.g.
`[ACC-1]`) that is referenced from the doc comments on the entrypoints and from
the tests that check it:

| Test file | Scope |
|---|---|
| `contracts/invoice_liquidity/src/tests_lifecycle_invariants.rs` | Unit + property tests (single contract, real auth checks) |
| `contracts/tests/cross_contract_invariants_test.rs` | Cross-contract rollback with real `iln_distribution` + `insurance_pool` |

```sh
cargo test -p invoice_liquidity --lib tests_lifecycle_invariants
cargo test -p invoice_liquidity --test cross_contract_invariants_test
```

---

## 1. States

| State | Meaning | Kind |
|---|---|---|
| `Pending` | Submitted, no funding yet | initial |
| `PartiallyFunded` | `0 < amount_funded < amount` | active |
| `Funded` | `amount_funded == amount`; freelancer has been paid out | active |
| `Disputed` | Payer opened a dispute before settlement | active (admin / timeout exit) |
| `Defaulted` | Funded, unpaid at/after `due_date`; LPs refunded | final except for one appeal |
| `Appealed` | Payer contested the default | active (admin exit) |
| `Paid` | Payer settled in full; LPs paid out | **terminal** |
| `Expired` | Never funded, past `due_date` | **terminal** |
| `Cancelled` | Cancelled by freelancer, or dispute upheld | **terminal** |

`Defaulted` can only ever move to `Appealed`, and only once (the appeal record
is never cleared), so after an appeal is resolved `Defaulted` is effectively
terminal.

## 2. State-transition table

Roles: **freelancer** = `invoice.freelancer`; **payer** = `invoice.payer`;
**LP** = the `funder` argument (must sign for its own funds); **recorded
funder** = an address in the invoice's contributor list; **current funder** =
`invoice.funder` (the LP whose tranche completed funding, or its transferee);
**admin** = stored `Admin`; **anyone** = no signature.

"P" in the *Paused?* column means the entrypoint returns `ContractPaused` while
the contract is paused.

| Entrypoint | From | To | Required authorization | Guards | Accounting effect |
|---|---|---|---|---|---|
| `submit_invoice` / `submit_invoices_batch` | — | `Pending` | freelancer | P; freelancer ≠ payer; token approved; amount/discount/due-date valid; ≤ max invoice amount | Creates invoice with `amount_funded = amount_paid = 0`; submitter index, counters, reputation `invoices_submitted` |
| `update_invoice` | `Pending` | `Pending` | freelancer (signs **and** must equal `invoice.freelancer`) | P; `now < due_date`; terms valid | Rewrites `amount`, `due_date`, `discount_rate` (only possible while nothing is funded) |
| `convert_invoice_token` | `Pending` | `Pending` | freelancer (as above) | P; `now < due_date`; new token approved | Changes `token` |
| `transfer_invoice` | `Pending` | `Pending` | freelancer | P | Changes `freelancer`, moves submitter index |
| `join_fund_queue` | `Pending`, `PartiallyFunded` | unchanged | LP | queue not yet resolved; not already queued | Queue entry only |
| `resolve_fund_queue` | any (exists) | unchanged | anyone | queue non-empty; maturity delay elapsed | Locks the approved funder; later `fund_invoice` calls by other LPs fail `NotApprovedFunder` |
| `fund_invoice` (partial) | `Pending`, `PartiallyFunded` | `PartiallyFunded` | LP | P; `fund_amount > 0`; `amount_funded + fund_amount < amount`; `now ≤ due_date` if `Pending`; approved-funder / token / payer-reputation / oracle / volume-cap checks | LP pays `fund_amount − ⌊fund_amount·rate/10⁴⌋` into ILN; contributor list += `fund_amount`; `amount_funded += fund_amount`; LP index; token volume; distribution `accrue_lp` |
| `fund_invoice` (completing) | `Pending`, `PartiallyFunded` | `Funded` | LP | as above with `amount_funded + fund_amount == amount` | As above, plus ILN pays freelancer `amount − ⌊amount·rate/10⁴⌋`; `funder`/`funded_at` set; LP score +1; `total_funded` +1 |
| `transfer_lp_position` | `Funded` | `Funded` | current funder | P; `new_lp ≠ current funder` | `funder` and the current funder's contributor entries renamed to `new_lp`; LP index moved. Amounts unchanged |
| `cancel_invoice` | `Pending` | `Cancelled` | freelancer | P | None |
| `cancel_invoice` | `PartiallyFunded` | `Cancelled` | freelancer | P | Each contributor refunded `amt − ⌊amt·rate/10⁴⌋` (exactly what it paid). `amount_funded` and the contributor list are kept as history |
| `expire_invoice` | `Pending` | `Expired` | anyone | P; `now > due_date` | None |
| `mark_paid` (partial) | `Funded` | `Funded` | payer | P; `0 < amount ≤ amount − amount_paid` | Payer → ILN `amount`; `amount_paid += amount` |
| `mark_paid` (completing) | `Funded` | `Paid` | payer | as above with `amount_paid + amount == amount` | As above, then fee `⌊amount·fee/10⁴⌋` → admin; each contributor receives `⌊(amount − fee)·amt/amount⌋`; `total_paid` +1; payer score +1; `invoices_paid` +1 (payer, freelancer); distribution `accrue_settlement` |
| `claim_yield` | any with `funder` set | unchanged | current funder (must also be a recorded funder) | — | **View only, moves no funds.** `Paid` → `⌊amount·rate/10⁴⌋`; `Pending`/`PartiallyFunded`/`Funded` → 0; other states → error; no `funder` → `NothingToClaim` |
| `claim_default` | `Funded` | `Defaulted` | LP that is a recorded funder | P; `now ≥ due_date` | Every contributor refunded `amt − ⌊amt·rate/10⁴⌋` from ILN's balance; if the caller is enrolled in the insurance pool, `pool.claim` is attempted (failure tolerated); payer score −5 (snapshot kept); `invoices_defaulted` +1 |
| `appeal_default` | `Defaulted` | `Appealed` | payer | P; no prior appeal; `now ≤ due_date + 30 days` | Appeal record only |
| `resolve_appeal(true)` | `Appealed` | `Defaulted` | admin | — | Payer score restored to pre-default snapshot; `invoices_defaulted` −1. No funds move |
| `resolve_appeal(false)` | `Appealed` | `Defaulted` | admin | — | None |
| `dispute_invoice` | `Pending`, `PartiallyFunded`, `Funded` | `Disputed` | payer | P; no prior dispute | Dispute record only |
| `resolve_dispute(_, _, 1)` (upheld) | `Disputed` | `Cancelled` | admin | — | Each contributor refunded `amt − ⌊amt·rate/10⁴⌋`; payer refunded `amount_paid` (if any). `amount_funded`/`amount_paid` kept as history |
| `resolve_dispute(_, _, 2)` (rejected) | `Disputed` | `Pending` / `PartiallyFunded` / `Funded` per `amount_funded` | admin | — | None |
| `resolve_dispute(_, _, other)` | `Disputed` | error | admin | — | None (rejected with `Unauthorized`) |
| `auto_resolve_dispute` | `Disputed` | as `resolve_dispute(2)` | anyone | `ledger ≥ disputed_at + dispute_timeout_ledgers` | None |

There are no other transitions: `Paid`, `Expired` and `Cancelled` accept no
state-changing call, and every call that does not match a row above returns a
distinct `ContractError` with no side effect.

### Failed-transaction rollback

Soroban executes each invocation atomically: if an entrypoint returns `Err` or
any nested call (token transfer, distribution hook, …) traps, **every** storage
write and token movement of that invocation — in every contract it touched —
is discarded. Several entrypoints rely on this rather than on explicit cleanup:

* lock-then-error paths in `fund_invoice`, `cancel_invoice`, `mark_paid`,
  `claim_default`, `resolve_dispute` return without releasing the reentrancy
  lock;
* `fund_invoice` / `update_invoice` / `convert_invoice_token` write
  `Expired` and then return `Err(InvoiceExpired)` (see `[NE-4]`);
* `fund_invoice` and `mark_paid` call the distribution contract *after* the
  token transfers; a failing hook must undo those transfers.

`claim_default` is the one place where a cross-contract failure is tolerated:
the insurance call uses `try_*`, so an empty / paused / unreachable pool leaves
the default (and the pool) intact instead of reverting it.

## 3. Invariants and where they are checked

### Accounting

| ID | Invariant | Enforced by | Tests |
|---|---|---|---|
| ACC-1 | `0 ≤ amount_funded ≤ amount` | `fund_invoice`: `fund_amount > 0`, `checked_add`, `OverfundingRejected` | `acc_overfunding_rejected_at_every_tranche`, `acc_overflowing_fund_amount_rejected`, `prop_*` |
| ACC-2 | `0 ≤ amount_paid ≤ amount`; a payer never pays more than the obligation | `mark_paid`: `InvalidAmount`, `OverpaymentRejected`, `checked_add` | `acc_overpayment_rejected_and_paid_never_exceeds_amount`, `prop_*` |
| ACC-3 | Contributor amounts sum to `amount_funded` | `fund_invoice` updates both together | `acc_multi_lp_partial_funding_is_cumulative`, `prop_*` |
| ACC-4 | Every recorded contribution is `> 0` | `fund_invoice` `fund_amount > 0` guard | `acc_zero_and_negative_fund_amount_rejected`, `prop_*` |
| ACC-5 | `PartiallyFunded ⇒ 0 < amount_funded < amount`; `Funded ⇒ amount_funded == amount`; dispute rejection restores the status implied by `amount_funded` | `fund_invoice`, `resolve_dispute(2)`, `auto_resolve_dispute` | `acc_dispute_rejection_restores_status_from_cumulative_funding`, `prop_*` |
| ACC-6 | Tokens are conserved across all actors, ILN and the insurance pool | transfers only | `prop_*`, `xc_failed_default_leaves_insurance_untouched_and_success_is_once`, `xc_end_to_end_accounting_is_conserved` |

### Status consistency

| ID | Invariant | Tests |
|---|---|---|
| ST-1 | `Pending ⇒ amount_funded == 0 ∧ amount_paid == 0 ∧ funder == None` | `assert_accounting` (all tests) |
| ST-2 | `Paid ⇔ amount_paid == amount` (and `amount_funded == amount`) | `assert_accounting`, `prop_*` |
| ST-3 | `Defaulted`/`Appealed ⇒ amount_funded == amount ∧ amount_paid < amount` | `assert_accounting`, `set_default_requires_due_date_and_full_funding` |
| ST-4 | `Expired ⇒ amount_funded == 0` (only `Pending` expires) | `auth_permissionless_transitions_are_state_gated`, `prop_*` |
| ST-5 | Terminal states (`Paid`, `Expired`, `Cancelled`) are immutable; every observed transition is a row of §2 | `prop_lifecycle_and_accounting_invariants` (`allowed()` encodes §2) |

### Authorization

All negative tests use real auth enforcement (`mock_auths` with a specific
non-authorized signer, and `set_auths(&[])`), assert the call fails with no
state change, then assert via `env.auths()` that exactly the expected role
signed the successful call.

| ID | Transition(s) | Required signer | Tests |
|---|---|---|---|
| AUTH-1 | `cancel_invoice`, `update_invoice`, `transfer_invoice` | freelancer | `auth_cancel_requires_freelancer`, `auth_update_and_transfer_invoice_require_freelancer` |
| AUTH-2 | `fund_invoice` | the funding LP | `auth_fund_requires_the_funding_lp` |
| AUTH-3 | `mark_paid` | payer | `auth_mark_paid_requires_payer` |
| AUTH-4 | `dispute_invoice`, `appeal_default` | payer | `auth_dispute_and_appeal_require_payer` |
| AUTH-5 | `resolve_dispute`, `resolve_appeal` | admin | `auth_dispute_and_appeal_resolution_require_admin` |
| AUTH-6 | `claim_default` | a recorded funder | `auth_claim_default_requires_a_recorded_funder` |
| AUTH-7 | `transfer_lp_position`, `claim_yield` | current funder | `auth_lp_position_and_yield_require_current_funder` |
| AUTH-8 | `initialize` cannot be re-run to replace the admin | `AlreadyInitialized` guard on `Admin` | `auth_reinitialize_cannot_replace_admin` |
| — | `expire_invoice`, `auto_resolve_dispute` are permissionless but purely state/time gated | — | `auth_permissionless_transitions_are_state_gated`, `acc_auto_resolve_requires_timeout` |

### Settlement, default and yield

| ID | Invariant | Tests |
|---|---|---|
| SET-1 | Settlement is applied at most once; after `Paid` no payment, default, dispute, cancel or funding is accepted | `set_settlement_cannot_be_applied_twice`, `xc_settlement_rolls_back_when_distribution_settlement_fails`, `prop_*` |
| SET-2 | Default is applied (and LPs refunded, insurance claimed) at most once, including after an appeal | `set_default_cannot_be_claimed_twice_or_after_appeal`, `xc_failed_default_leaves_insurance_untouched_and_success_is_once`, `prop_*` |
| SET-3 | Settlement payouts ≤ `amount − fee`, and never draw on ILN's pre-existing balance | `set_settlement_with_fee_never_distributes_more_than_received` |
| SET-4 | Only a fully funded *and* fully paid invoice reports non-zero yield | `set_yield_only_from_legitimately_funded_and_settled_obligation`, `prop_*` |

### Rollback

| ID | Invariant | Tests |
|---|---|---|
| RB-1 | Any failed lifecycle call leaves invoice, contributor list, token balances and payer score unchanged | every negative test; `prop_*` checks it after every failed step |
| RB-2 | A failing distribution hook or token transfer reverts funding / settlement across ILN, LPs, payer and distribution | `xc_funding_rolls_back_when_distribution_accrual_fails`, `xc_settlement_rolls_back_when_distribution_settlement_fails`, `xc_token_failure_rolls_back_funding_and_accrual` |
| RB-3 | A failing refund in `claim_default` / `resolve_dispute(1)` reverts the status change and leaves the insurance pool untouched; a failing insurance claim leaves the pool untouched without blocking the default | `xc_failed_default_leaves_insurance_untouched_and_success_is_once`, `xc_insurance_failure_does_not_corrupt_pool_or_block_default`, `rb_insolvent_dispute_refund_reverts_status_and_balances`, `xc_insolvent_dispute_resolution_reverts_atomically` |
| RB-4 | Error paths do not leave the reentrancy lock set | `rb_failed_calls_do_not_leave_reentrancy_lock_set` |

### Property tests

`prop_lifecycle_and_accounting_invariants` runs random sequences (up to 40
steps, 64 cases) of funding (including 0, negative, tiny and over-sized
amounts), "fund the rest", partial/complete payment, cancel, expire, time
travel, default (by any LP), appeal, appeal resolution, dispute, every
dispute ruling, timeout resolution, yield queries and LP-position transfers.
After every step it checks ACC-*, ST-*, RB-1, the §2 table, per-entrypoint
accounting deltas, payer outflow and SET-1/SET-2.
`prop_partial_funding_then_settle_or_default` focuses on many small tranches
followed by settlement or default + appeal.

## 4. Invariants that are NOT mechanically enforced

These are properties a reader might expect but that the contract does not
guarantee today. They are documented, not fixed, by Issue #1159.

* **NE-1 — Cumulative fields are history, not net position.** After
  `cancel_invoice` (from `PartiallyFunded`) or an upheld dispute, LPs and the
  payer are refunded but `amount_funded`, `amount_paid` and the contributor
  list are left unchanged. Off-chain consumers must use `status` to interpret
  them.
* **NE-2 — No per-invoice escrow / solvency.** On full funding the LPs'
  principal is forwarded to the freelancer. `claim_default` and an upheld
  `resolve_dispute(1)` on a `Funded` invoice nevertheless refund that
  principal to the LPs **from ILN's pooled token balance**. Nothing ties those
  refunds to funds held for the same invoice, so they can consume other
  invoices' partial-funding deposits or partial payments, or simply fail
  (and revert) if the balance is insufficient
  (`rb_insolvent_dispute_refund_reverts_status_and_balances`). The tests mint
  an explicit treasury reserve for this reason.
* **NE-3 — Partial payments on default.** If the payer made partial payments
  (`amount_paid > 0`) before `claim_default`, those tokens stay in ILN: they
  are neither distributed to LPs nor refunded to the payer.
* **NE-4 — Lazy expiry is not durable.** `fund_invoice`, `update_invoice` and
  `convert_invoice_token` set `status = Expired` and then return
  `Err(InvoiceExpired)`; the write is rolled back with the error
  (`rb_lazy_expiry_on_error_path_is_rolled_back`). Only `expire_invoice`
  persists `Expired`. The boundaries also differ: `update_invoice` /
  `convert_invoice_token` treat `now ≥ due_date` as expired, `fund_invoice` and
  `expire_invoice` use `now > due_date`, and `claim_default` allows
  `now ≥ due_date`.
* **NE-5 — Late payment vs. default race.** `mark_paid` has no due-date
  check, so a payer can settle after `due_date` until an LP calls
  `claim_default`; whichever lands first wins.
* **NE-6 — `claim_yield` is informational.** It moves no funds and reports
  `amount·rate/10⁴` for the whole invoice, ignoring the protocol fee and the
  multi-LP split. Actual LP proceeds are paid by `mark_paid`.
* **NE-7 — Not all lifecycle entrypoints honour pause.** `resolve_dispute`,
  `resolve_appeal`, `auto_resolve_dispute`, `claim_yield`, `join_fund_queue`
  and `resolve_fund_queue` work while paused.
* **NE-8 — LP-position transfer covers only the completing LP.**
  `transfer_lp_position` moves `invoice.funder`'s position; other partial
  contributors are unaffected. If `new_lp` already contributed, the
  contributor list holds two entries for the same address (sums are still
  correct).
* **NE-9 — Token behaviour is trusted.** Accounting assumes a SEP-41 token
  that rejects negative amounts and moves exactly the requested amount; the
  fee-on-transfer probe runs only in `add_token`.
