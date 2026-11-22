# Dispute Submission & Resolution Transitions (Issue #1172)

Scope: the payer-initiated **dispute** mechanism in `contracts/invoice_liquidity` —
`dispute_invoice`, `resolve_dispute`, `auto_resolve_dispute` — and the
"evidence window" they rely on (`dispute_timeout_ledgers` + the stored
`DisputeRecord`).

This audit is **observational**: it documents the behavior that exists on the
`dev` branch and pins it with tests. No contract logic changed.

Suggested companion tests: `contracts/invoice_liquidity/src/tests_dispute_transitions.rs`.

## Non-goals

- The default/appeal path (`claim_default` / `appeal_default` / `resolve_appeal`)
  is covered separately (see the appeal/authorization audits).
- Off-chain evidence storage/verification. The contract only ever sees a
  `BytesN<32>` commitment.
- Any storage migration. The dispute feature already ships with its own keys.

## Entry points & authorization

| Entry point | Authorized caller | State precondition | Evidence window |
| --- | --- | --- | --- |
| `dispute_invoice(invoice_id, reason_hash)` | **payer** (`invoice.payer.require_auth()`) | `status ∈ {Pending, PartiallyFunded, Funded}` | none — only bounded by state |
| `resolve_dispute(invoice_id, resolution_hash, resolution)` | **admin** (`require_admin`) | `status == Disputed`, `resolution ∈ {1,2}` | n/a |
| `auto_resolve_dispute(invoice_id)` | **anyone** (permissionless, no auth) | `status == Disputed` **and** `now_ledger ≥ disputed_at + dispute_timeout_ledgers` | ledgers, measured from filing |

`resolve_dispute` takes the reentrancy lock and records an admin-action audit
entry after `require_admin` succeeds.

## State model

`InvoiceStatus::Disputed` is entered only from a non-terminal, pre-settlement
state and is left only by an explicit resolution:

```
Pending ─────┐
PartiallyFunded ─┼─► Disputed ─► Cancelled              (resolve_dispute(1), upheld)
Funded ──────┘            ├─► Funded                   (resolve_dispute(2), rejected — fully funded)
                          ├─► PartiallyFunded          (resolution (2) — partially funded)
                          └─► Pending                  (resolution (2) — unfunded, or auto_resolve timeout)
```

Terminal states (`Paid`, `Expired`, `Cancelled`) and the default/appeal states
(`Defaulted`, `Appealed`) **reject** `dispute_invoice` with a distinct typed
error (see the failure table below).

## Evidence window model

There is no separate "evidence submission" transaction. The payer commits the
evidence commitment (`reason_hash`) **at filing time**, and the only window is
the administrative resolution deadline:

- `DisputeRecord { reason_hash, disputed_at }` is written at filing.
  `disputed_at` is a **ledger sequence** (used by `auto_resolve_dispute`).
- The `InvoiceDisputed` event records `disputed_at` as a **timestamp** (see
  VF-DT1).
- After `dispute_timeout_ledgers` elapse, anyone may call
  `auto_resolve_dispute`, which rules for the freelancer (code `2`) with an
  empty resolution hash — the payer's evidence is discarded by design, to
  prevent a denial-of-service lock on the invoice.

## Failure & rollback behavior

All three entry points return a typed `ContractError`. Because Soroban rolls
back the whole invocation on `Err`, a rejected call leaves **no** partial state:

| Condition | Entry point | Error |
| --- | --- | --- |
| `status == Paid` | `dispute_invoice` | `AlreadyPaid` |
| `status == Defaulted` | `dispute_invoice` | `InvoiceDefaulted` |
| `status == Appealed` | `dispute_invoice` | `InvoiceAppealed` |
| `status == Expired` | `dispute_invoice` | `InvoiceExpired` |
| `status == Cancelled` | `dispute_invoice` | `AlreadyCancelled` |
| `status == Disputed` / record exists | `dispute_invoice` | `AlreadyDisputed` |
| not the payer | `dispute_invoice` | auth failure |
| paused | `dispute_invoice` | `ContractPaused` |
| invoice missing | any | `InvoiceNotFound` |
| `status != Disputed` | `resolve_dispute` / `auto_resolve_dispute` | `NotDisputed` |
| `resolution ∉ {1,2}` | `resolve_dispute` | `Unauthorized` |
| not the admin | `resolve_dispute` | auth failure |
| before the timeout | `auto_resolve_dispute` | `Unauthorized` |

## Invariants

- **INV-DT1** — `dispute_invoice` requires the invoice payer's authorization.
- **INV-DT2** — `resolve_dispute` requires the admin's authorization.
- **INV-DT3** — `dispute_invoice` is accepted **only** from
  `Pending`, `PartiallyFunded`, or `Funded`; every other state returns a
  distinct typed error.
- **INV-DT4** — `resolve_dispute` is accepted **only** from `Disputed` and only
  for `resolution ∈ {1,2}`; any other code returns `Unauthorized` without
  mutating state.
- **INV-DT5** — `resolve_dispute(1)` (upheld) transitions to `Cancelled`,
  refunding the LP principal (net of discount) and any partial payer payment.
- **INV-DT6** — an invoice accrues **at most one** `DisputeRecord` for its
  lifetime; the record is never cleared, so a resolved/auto-resolved invoice
  can never be disputed again (`AlreadyDisputed`).
- **INV-DT7** — `auto_resolve_dispute` is permissionless but fires only when
  `now_ledger ≥ disputed_at + dispute_timeout_ledgers` (inclusive boundary);
  earlier calls return `Unauthorized` and change nothing.
- **INV-DT8** — `auto_resolve_dispute` always applies resolution code `2` with
  an empty resolution hash and restores the funding-derived status.

## Verified findings

- **VF-DT1 — unit mismatch between stored record and event.** `DisputeRecord.
  disputed_at` is the **ledger sequence**, while the `InvoiceDisputed` event's
  `disputed_at` is the **timestamp**. Off-chain consumers that treat the two as
  the same value will misread one of them.
- **VF-DT2 — disputes are once-per-invoice.** `resolve_dispute` and
  `auto_resolve_dispute` do not remove the `DisputeRecord`; a subsequent
  `dispute_invoice` is rejected with `AlreadyDisputed` even after a *rejected*
  resolution returned the invoice to `Funded`.
- **VF-DT3 — invalid resolution codes collapse to `Unauthorized`.** Codes `0`
  and `>2` share the authorization error, so callers cannot distinguish "bad
  code" from "not admin" by error variant.
- **VF-DT4 — timeout-not-reached borrows `Unauthorized`.** `auto_resolve_dispute`
  reuses `Unauthorized` for the "window still open" case rather than a
  `TimeoutNotReached`-style error.
- **VF-DT5 — `auto_resolve_dispute` is permissionless.** Any address can close
  an overdue dispute without authorization; the design intent (anti-DoS) is
  documented, but the entry point grants no reward and emits no distinguishing
  caller identity.
- **VF-DT6 — `dispute_invoice` performs no evidence validation.** A zeroed or
  arbitrary `reason_hash` is accepted; there is no on-chain check that evidence
  was actually provided.
- **VF-DT7 — `resolve_dispute` early returns skip `unlock_reentrancy`.**
  The `NotDisputed` and invalid-resolution paths return `Err` without unlocking,
  while the missing-invoice path explicitly unlocks. Correctness depends on
  Soroban rolling back the lock write on `Err` (see RR-DT5).

## Residual risks

- **RR-DT1** — There is no on-chain dispute **submission** deadline: an invoice
  can be disputed at any point before settlement (bounded only by status). A
  payer can dispute a still-`Pending` (unfunded) invoice and strand it until
  admin action or timeout.
- **RR-DT2** — Evidence is a blind hash; the contract cannot confirm the
  commitment resolves to real evidence.
- **RR-DT3** — Auto-resolution discards payer-supplied evidence and always rules
  for the freelancer.
- **RR-DT4** — The record/event unit mismatch (VF-DT1) is a documented
  integration hazard for indexers; no on-chain normalization exists.
- **RR-DT5** — `resolve_dispute`'s reentrancy-lock cleanup relies on transaction
  atomicity for its `Err` paths (VF-DT7). Any future refactor that converts an
  early `Err` into an `Ok` would leak the lock.
- **RR-DT6** — `auto_resolve_dispute` measures elapsed ledgers from
  `disputed_at`; if a ledger sequence is ever reset (local/network reset), the
  measured window is relative to the stored sequence.

## Test mapping

| Test | Covers |
| --- | --- |
| `test_dispute_requires_payer_authorization` | INV-DT1 |
| `test_resolve_requires_admin_authorization` | INV-DT2 |
| `test_auto_resolve_is_permissionless` | VF-DT5 |
| `test_dispute_pending_invoice_succeeds` | INV-DT3 |
| `test_dispute_partially_funded_invoice_succeeds` | INV-DT3 |
| `test_dispute_funded_invoice_succeeds` | INV-DT3 |
| `test_dispute_paid_invoice_rejected` | INV-DT3 |
| `test_dispute_defaulted_invoice_rejected` | INV-DT3 |
| `test_dispute_appealed_invoice_rejected` | INV-DT3 |
| `test_dispute_expired_invoice_rejected` | INV-DT3 |
| `test_dispute_cancelled_invoice_rejected` | INV-DT3 |
| `test_double_dispute_rejected` | INV-DT3, INV-DT6 |
| `test_dispute_blocked_when_paused` | failure table |
| `test_dispute_missing_invoice_rejected` | failure table |
| `test_redispute_after_rejected_resolution_rejected` | INV-DT6 / VF-DT2 |
| `test_redispute_after_auto_resolve_rejected` | INV-DT6 / VF-DT2 |
| `test_dispute_record_ledger_vs_event_timestamp_units` | VF-DT1 |
| `test_resolve_requires_disputed_state` | INV-DT4 |
| `test_resolve_missing_invoice_rejected` | failure table |
| `test_resolve_invalid_resolution_zero_rejected` | INV-DT4 / VF-DT3 |
| `test_resolve_invalid_resolution_high_rejected` | INV-DT4 / VF-DT3 |
| `test_resolve_upheld_cancels_and_refunds_funder` | INV-DT5 |
| `test_resolve_upheld_refunds_partial_payer_and_emits_event` | INV-DT5 |
| `test_resolve_rejected_restores_funded` | INV-DT8 |
| `test_resolve_rejected_restores_partially_funded` | INV-DT8 |
| `test_resolve_rejected_restores_pending` | INV-DT8 |
| `test_resolve_emits_dispute_resolved_event_with_code` | INV-DT4 |
| `test_auto_resolve_before_timeout_rejected` | INV-DT7 / VF-DT4 |
| `test_auto_resolve_at_timeout_boundary_succeeds` | INV-DT7 |
| `test_auto_resolve_requires_disputed_state` | INV-DT7 |
| `test_auto_resolve_missing_invoice_rejected` | failure table |
| `test_auto_resolve_emits_rejected_resolution_with_empty_hash` | INV-DT8 |

## Compatibility

- No `DataKey`, `InvoiceStatus`, event, or entry-point signature changes.
- Additions are limited to a `#[cfg(test)]` module
  (`tests_dispute_transitions`) and this document, so existing deployments and
  SDK bindings are unaffected.
