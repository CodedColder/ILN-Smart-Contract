# Adversarial fund-queue race coverage (#1174)

Scope: [`contracts/invoice_liquidity/src/lib.rs`](../contracts/invoice_liquidity/src/lib.rs)
— `join_fund_queue`, `resolve_fund_queue`, `require_queueable`,
`recover_fund_queue` — plus their interaction with `cancel_invoice` and
`expire_invoice`.

Test module:
[`contracts/invoice_liquidity/src/tests_fund_queue_races.rs`](../contracts/invoice_liquidity/src/tests_fund_queue_races.rs).

This is a verification-only change: no contract behavior, storage layout, or
public API is modified. Its purpose is to make the queue's behavior at ledger
and transaction boundaries explicit and regression-locked.

## Non-goals

- Reputation scoring, tie-breaking fairness, or the PRNG (covered by
  `tests_tie_breaking.rs`).
- The maturity delay itself (`tests_mev_mitigation.rs`) and queue
  archival/TTL (`tests_queue_archival.rs`) in isolation.
- Any cross-repository SDK/indexer work.

## Model

The fund queue is an *intent* layer laid over the invoice lifecycle:

1. `join_fund_queue(lp, id)` snapshots `lp_score` and inserts the LP in
   descending-score order. It records `FundQueueOpenedAt` on the first join
   only.
2. A **maturity window** of `QUEUE_DELAY_LEDGERS` (120) ledger sequences must
   elapse before `resolve_fund_queue` may lock a winner. Resolution re-reads
   each LP's *current* score and selects the current maximum (ties broken by
   PRNG).
3. The winner is stored in `QueueResolution` + `QueueResolvedAt`. Only the
   approved LP may then call `fund_invoice`.
4. `QueueResolution` grants no other privilege. Routing around it is
   permissionless: `recover_fund_queue` evicts a stalled winner after
   `QUEUE_RESOLUTION_TTL_LEDGERS` (720) — shortened to
   `QUEUE_DELAY_LEDGERS` within `NEAR_EXPIRY_WINDOW_SECS` (86 400 s) of the due
   date — or purges all queue state once the invoice is no longer fundable.

Cancellation and expiry never touch queue state; that state is reclaimed lazily
by `recover_fund_queue`.

### Entry points and authorization

| Entry point | Caller | Notes |
|---|---|---|
| `join_fund_queue` | any LP (`require_lp` → `require_auth`) | resolution gate precedes status gate |
| `resolve_fund_queue` | anyone | permissionless; maturity + `require_queueable` gated |
| `recover_fund_queue` | anyone | permissionless; moves no funds |
| `cancel_invoice` | freelancer (`require_submitter_by_id`) | `Pending` / `PartiallyFunded` only |
| `expire_invoice` | anyone | `Pending` only, strict `now > due_date` |
| `fund_invoice` | LP | resolution gate, then allowlist/oracle, then status gate |

### Data sources

All assertions read live contract state through the test client or through
`env.as_contract(...)` (`get_fund_queue`, `get_queue_resolution`,
`get_fund_queue_opened_at`, `get_queue_resolved_at`, `get_invoice_funders`,
`get_lp_invoices`). Ledger sequence and timestamp are driven explicitly with
`env.ledger().set(...)`.

## Failure behavior

| Condition | Result |
|---|---|
| join when queue has ≥ `MAX_FUND_QUEUE_LEN` entries | bounded: displace tail or `QueueFull` (INV-FQR8) |
| join when `QueueResolution` present | `NotApprovedFunder` |
| join when invoice terminal | `AlreadyCancelled` / `InvoiceExpired` / … |
| join when `now > due_date` | `InvoiceExpired` |
| resolve before maturity | `QueueNotMature` |
| resolve when `QueueResolution` present | returns cached winner |
| resolve on non-fundable invoice | `AlreadyCancelled` / `InvoiceExpired` / … |
| resolve on empty queue | `NotFunded` |
| resolve when `FundQueueOpenedAt` missing | `QueueStateArchived` |
| recover with no queued/resolved state | `QueueNotStalled` |
| recover when invoice not fundable | purges, returns `Ok(None)` |
| recover before grace elapsed | `QueueNotStalled` |

## Invariants

- **INV-FQR1** — an LP holds at most one slot in a queue; duplicate joins never
  grow or reorder it.
- **INV-FQR2** — in `join_fund_queue`, the `QueueResolution` gate is evaluated
  before the invoice-status gate.
- **INV-FQR3** — with no resolution, `join_fund_queue` reports the terminal
  status directly.
- **INV-FQR4** — the past-due join gate is strict: `now == due_date` is
  joinable; `now > due_date` is not.
- **INV-FQR5** — a rejected call (`join`, `resolve`, or `fund`) is atomic: it
  leaves no partial queue, resolution, accounting, or index state.
- **INV-FQR6** — `QueueResolution` is idempotent; repeat resolves return the
  cached winner and never re-anchor `QueueResolvedAt`.
- **INV-FQR7** — recovery either purges all queue state (un-fundable invoice) or
  re-anchors `FundQueueOpenedAt` to the recovery ledger (restarting maturity);
  repeated recovery after a purge is a no-op.
- **INV-FQR8** — the queue is bounded: joins attempted at or over
  `MAX_FUND_QUEUE_LEN` never grow it past the cap and leave it well-formed and
  resolvable, whether the capacity path admits a displacing entry or rejects.

## Verified findings

- **VF-FQR1** — resolution uses each LP's *current* score, not the join-time
  snapshot. An LP who joins after the maturity window has already elapsed (so
  resolution is immediately callable) wins outright if their reputation is the
  highest. Late entry is bounded only by the due-date gate, not the maturity
  window.
- **VF-FQR2** — a stored resolution is never erased by cancellation or expiry;
  `resolve_fund_queue` short-circuits on it before `require_queueable`. A
  cancelled invoice with a resolved queue keeps returning its winner.
- **VF-FQR3** — without a stored resolution, `resolve_fund_queue` on a
  cancelled invoice returns `AlreadyCancelled` (the resolution was never
  finalized).
- **VF-FQR4** — recovery re-opens the queue for the remaining LPs and restarts
  the maturity window from the recovery ledger; the survivor must wait a fresh
  `QUEUE_DELAY_LEDGERS` before resolving again.
- **VF-FQR5** — cancelling a `Pending` invoice that already has a resolved queue
  succeeds and leaves the resolution in place; the approved LP cannot fund
  (status gate) while a non-approved LP is stopped earlier by the resolution
  gate. Only `recover_fund_queue` reclaims the state.
- **VF-FQR6** — the same holds for a `PartiallyFunded` invoice: cancellation
  refunds the LP and the resolution persists until recovery.
- **VF-FQR7** — expiring a `Pending` invoice with a resolved queue leaves the
  resolution behind; the approved LP can no longer fund and recovery purges all
  four queue keys.
- **VF-FQR8** — `expire_invoice` refuses a `PartiallyFunded` invoice
  (`AlreadyFunded`), and a past-due `PartiallyFunded` invoice cannot be resolved
  (`require_queueable` fails closed on `now > due_date`).
- **VF-FQR9** — the past-due guard that blocks resolution for a `PartiallyFunded`
  invoice does **not** block direct funding: `fund_invoice` auto-expires only the
  `Pending` case, so a past-due partially funded invoice can still be completed
  by a direct funder (queue and direct paths diverge on the expiry boundary).
- **VF-FQR10** — near the due date, recovery shortens the stalled winner's
  grace to `QUEUE_DELAY_LEDGERS`, so a stalled winner cannot run the full TTL
  down near the deadline.

## Residual risks

- **RR-FQR1** — because a resolution survives cancellation/expiry until
  `recover_fund_queue` runs, `QueueResolution`/`QueueResolvedAt` can sit in
  storage for an un-fundable invoice indefinitely if no one calls recovery
  (bounded only by persistent-entry TTL).
- **RR-FQR2** — the divergence in VF-FQR9 means off-chain consumers cannot use
  a single "past due ⇒ unfundable" rule; a `PartiallyFunded` invoice past due
  remains directly fundable until someone completes it.
- **RR-FQR3** — `resolve_fund_queue` has no on-chain liveness requirement: a
  mature, non-empty queue may remain unresolved forever if nobody calls it (no
  automatic resolution), so maturity is necessary but not sufficient.
- **RR-FQR4** — the capacity path's branch (a displacing admission vs. a
  `QueueFull` rejection) is not fixed by this change; INV-FQR8 pins only the
  safety property (the cap is never exceeded), not which branch executes.
- **RR-FQR5** — recovery's grace shortening is evaluated at call time only;
  between `resolve` and the next recovery call, `QueueResolvedAt` is not
  re-derived, so an invoice that drifts into the near-expiry window after
  resolution still observes the shortened grace on the next recovery (favorable
  to liveness, but not announced at resolution time).

## Test mapping

| Test | Invariant / finding |
|---|---|
| `race_double_join_same_lp_yields_single_entry` | INV-FQR1 |
| `race_join_after_resolution_precedes_status_gate` | INV-FQR2 |
| `race_join_cancelled_invoice_without_resolution` | INV-FQR3 |
| `race_join_expired_invoice_without_resolution` | INV-FQR3 |
| `race_join_at_due_date_boundary_then_one_second_past` | INV-FQR4 |
| `race_last_minute_join_outranks_earlier_joiner` | VF-FQR1 |
| `race_same_ledger_resolve_rejected_then_matures` | INV-FQR5 |
| `race_duplicate_resolution_is_idempotent_and_keeps_clock` | INV-FQR6 |
| `race_resolve_after_cancel_returns_cached_winner` | VF-FQR2 |
| `race_resolve_after_cancel_without_resolution_rejected` | VF-FQR3 |
| `race_recover_blocked_until_ttl_boundary` | INV-FQR6 |
| `race_recover_reopens_queue_and_restarts_maturity` | VF-FQR4, INV-FQR7 |
| `race_cancel_pending_with_resolved_queue_needs_recovery` | VF-FQR5 |
| `race_cancel_partially_funded_resolved_queue_persists_until_recovered` | VF-FQR6 |
| `race_expire_pending_with_resolved_queue_needs_recovery` | VF-FQR7 |
| `race_expire_and_resolve_rejected_for_partially_funded_past_due` | VF-FQR8 |
| `race_partially_funded_past_due_still_directly_fundable` | VF-FQR9 |
| `race_resolve_returns_cached_winner_after_funding_and_expiry` | INV-FQR6 |
| `race_queue_stays_bounded_under_capacity_burst` | INV-FQR8 |
| `race_duplicate_join_does_not_grow_or_reorder_queue` | INV-FQR1 |
| `race_rejected_resolve_leaves_state_untouched` | INV-FQR5 |
| `race_rejected_fund_leaves_invoice_untouched` | INV-FQR5 |
| `race_repeated_recovery_after_purge_is_noop` | INV-FQR7 |
| `race_recover_shortens_grace_near_expiry` | VF-FQR10 |

## Compatibility

No `DataKey` variants, storage encodings, event names, or entry-point
signatures are added or changed by this change; it adds tests and this document
only. It is therefore safe to land alongside other `invoice_liquidity` changes
without a migration.
