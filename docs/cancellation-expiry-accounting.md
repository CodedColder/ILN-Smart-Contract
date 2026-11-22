# Cancellation & Expiry Accounting (Issue #1173)

Scope: the two terminal paths in `contracts/invoice_liquidity` that finish an
invoice *without* a settlement or a default — `cancel_invoice` (freelancer) and
`expire_invoice` (permissionless). The audit verifies they cannot retain funded
obligations or leave queue / reputation state inconsistent.

This audit is **observational**: it documents the behavior on `dev` and pins it
with tests. No contract logic changed.

Suggested companion tests: `contracts/invoice_liquidity/src/tests_cancel_expire_accounting.rs`.

## Non-goals

- `mark_paid` / `claim_default` / dispute refund accounting (separate audits).
- Storage-layout or migration changes. Both paths already ship with their own keys.
- Any change to the fund-queue lifecycle; `recover_fund_queue` (Issues
  #1020/#1021) is treated as-is.

## Entry points & authorization

| Entry point | Authorized caller | State precondition | Time precondition |
| --- | --- | --- | --- |
| `cancel_invoice(invoice_id)` | **freelancer / submitter** (`require_submitter_by_id`) | `status ∈ {Pending, PartiallyFunded}` | none |
| `expire_invoice(invoice_id)` | **anyone** (no auth) | `status == Pending` | `now > due_date` (strict) |

`cancel_invoice` takes the reentrancy lock; `expire_invoice` does not.

## Accounting model

### Cancellation

| From state | Behavior | Refund |
| --- | --- | --- |
| `Pending` | → `Cancelled`; emit `InvoiceCancelled` | none (no funders) |
| `PartiallyFunded` | → `Cancelled` (CEI: state saved before transfers); emit `InvoiceCancelled` | each LP refunded `fund_amt − fund_amt·rate/10_000` = exactly the principal it paid |
| `Funded` | rejected (`AlreadyFunded`) | — |
| `Paid` / `Defaulted` / `Appealed` / `Disputed` / `Expired` / `Cancelled` | rejected with the matching typed error | — |

The refund equals the LP's original outlay because the contract funded with
`cost = normalized_amount − normalized_amount·rate/10_000` and the normalizers
are identity (`normalize_{xlm,usdc,eurc}_amount`). Cancellation is therefore
**value-neutral**: the LP's balance and the contract's token balance both return
to their pre-funding values.

### Expiry

Only `Pending` invoices can expire. Because a `Pending` invoice has never been
funded, expiry moves no funds and has no LP obligations to unwind. A
`Pending` invoice that is funded *after* its due date is transitioned to
`Expired` inside `fund_invoice` and the funding call fails with
`InvoiceExpired`.

## Failure & rollback behavior

| Condition | Entry point | Error |
| --- | --- | --- |
| `status == Funded`, or `PartiallyFunded` for expiry | both | `AlreadyFunded` |
| `status == Paid` | both | `AlreadyPaid` |
| `status == Defaulted` | cancel | `InvoiceDefaulted` |
| `status == Appealed` | cancel | `InvoiceAppealed` |
| `status == Disputed` | cancel | `InvoiceDisputed` |
| `status == Expired` | both | `InvoiceExpired` |
| `status == Cancelled` | cancel | `AlreadyCancelled` |
| `now <= due_date` | expire | `NotYetDefaulted` |
| not the freelancer | cancel | auth failure |
| paused | cancel | `ContractPaused` |
| invoice missing | both | `InvoiceNotFound` |

All rejections return `Err`, so Soroban rolls back any partial state (and the
reentrancy lock) atomically.

## Invariants

- **INV-CX1** — `cancel_invoice` requires the freelancer/submitter's authorization.
- **INV-CX2** — `cancel_invoice` accepts only `Pending` and `PartiallyFunded`;
  a `Funded` invoice (a live obligation) is never cancellable away.
- **INV-CX3** — cancelling a `PartiallyFunded` invoice refunds every funder
  exactly the principal it paid (net of the discount); the operation is
  value-neutral for both the LPs and the contract.
- **INV-CX4** — `expire_invoice` is permissionless, accepts only `Pending`, and
  requires `now > due_date` (strict `>`); `now == due_date` is rejected with
  `NotYetDefaulted`.
- **INV-CX5** — neither cancel nor expire mutates reputation counters; an
  unfunded expiry is no-fault (never counted as a default) and a voluntary
  cancellation is neutral.
- **INV-CX6** — neither cancel nor expire implicitly clears fund-queue state
  (queue, resolution, resolved-at, opened-at). Reclamation is the explicit,
  permissionless job of `recover_fund_queue`.

## Verified findings

- **VF-CX1 — cancellation leaves stale funded metadata.** Cancelling a
  `PartiallyFunded` invoice refunds the principal but does **not** reset
  `amount_funded`, clear the `InvoiceFunders` list, or remove the invoice from
  each LP's `LpInvoices` index. The invoice is terminal, so this is
  stale-but-inert state (no double-refund path exists), but any consumer that
  reads `amount_funded` / LP portfolios without checking `status` will see a
  cancelled invoice as still "funded".
- **VF-CX2 — queue state is not cleared by cancel/expire.** A `Pending` or
  `PartiallyFunded` invoice can carry a non-empty fund queue and a resolved
  `QueueResolution`; cancelling or expiring it leaves that storage in place
  until `recover_fund_queue` is called. `recover_fund_queue` detects the
  non-fundable status and purges all queue state (returns `Ok(None)`).
- **VF-CX3 — no expiry for partially funded invoices (RR-CX1).** A
  `PartiallyFunded` invoice past its due date is neither expirable
  (`AlreadyFunded`) nor deadline-bound — it keeps accepting funding until it
  fills or the freelancer cancels.

## Residual risks

- **RR-CX1** — Partial funding has no deadline: a `PartiallyFunded` invoice can
  sit past its due date and still be topped up (see VF-CX3). This is by design
  (the Pending-only expiry check) but means "due date" is not a hard funding
  cutoff for partially funded invoices.
- **RR-CX2** — `LpInvoices` is an append/remove index that is only pruned on
  `transfer_lp_position`; terminal states other than a transfer (cancel, expiry,
  settlement) leave the entry, so consumers must filter by invoice status.
- **RR-CX3** — Stale `amount_funded` / funder list on a cancelled invoice
  (VF-CX1) is a UI/operator-integration hazard; no on-chain normalization
  exists.
- **RR-CX4** — Queue reclamation depends on someone calling
  `recover_fund_queue`; until then the queue storage lingers (TTL-bounded) for
  cancelled/expired invoices.

## Test mapping

| Test | Covers |
| --- | --- |
| `test_cancel_requires_freelancer_authorization` | INV-CX1 |
| `test_cancel_pending_invoice_succeeds` | INV-CX2 |
| `test_cancel_funded_invoice_rejected` | INV-CX2 |
| `test_cancel_paid_invoice_rejected` | failure table |
| `test_cancel_defaulted_invoice_rejected` | failure table |
| `test_cancel_appealed_invoice_rejected` | failure table |
| `test_cancel_disputed_invoice_rejected` | failure table |
| `test_cancel_expired_invoice_rejected` | failure table |
| `test_cancel_twice_rejected` | failure table |
| `test_cancel_missing_invoice_rejected` | failure table |
| `test_cancel_blocked_when_paused` | failure table |
| `test_cancel_partially_funded_refunds_principal_value_neutral` | INV-CX3 |
| `test_cancel_partially_funded_refunds_every_funder` | INV-CX3 |
| `test_cancel_partially_funded_leaves_stale_funded_metadata` | VF-CX1 |
| `test_expire_is_permissionless` | INV-CX4 |
| `test_expire_requires_past_due_date` | INV-CX4 |
| `test_expire_partially_funded_rejected` | INV-CX4 |
| `test_expire_funded_rejected` | INV-CX4 |
| `test_expire_paid_rejected` | failure table |
| `test_expire_twice_rejected` | failure table |
| `test_expire_missing_invoice_rejected` | failure table |
| `test_expire_does_not_penalize_reputation` | INV-CX5 |
| `test_cancel_does_not_penalize_reputation` | INV-CX5 |
| `test_cancel_does_not_clear_fund_queue_resolution` | INV-CX6 / VF-CX2 |
| `test_expire_leaves_fund_queue_until_recovered` | INV-CX6 / VF-CX2 |
| `test_partially_funded_past_due_remains_fundable` | RR-CX1 / VF-CX3 |

## Compatibility

- No `DataKey`, `InvoiceStatus`, event, or entry-point signature changes.
- Additions are limited to a `#[cfg(test)]` module
  (`tests_cancel_expire_accounting`) and this document, so existing deployments
  and SDK bindings are unaffected.
