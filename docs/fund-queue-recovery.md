# Fund Queue Delayed Resolution, Recovery and Bounds

Covers Issues #1019 (verify winning LP after delayed resolution), #1020
(near-expiry recovery tooling), #1021 (validate cancelled/expired invoice
records) and #1022 (queues across long ledger gaps).

Code paths: `invoice.rs` (queue helpers), `lib.rs` (`join_fund_queue`,
`resolve_fund_queue`, `recover_fund_queue`), `storage.rs` (`DataKey`).

## Before / after

| Behaviour | Before | After |
| --- | --- | --- |
| Winner selection after a delay | Used the LP score *snapshotted at join time*; a stale high snapshot could win | `resolve_fund_queue` re-reads each LP's **current** score; ties are still broken by the PRNG (#1019) |
| Resolve on a dead invoice | Locked in a winner even if the invoice was cancelled, expired, funded or past due | Fails closed with the invoice's status error (`AlreadyCancelled`, `InvoiceExpired`, …) (#1021) |
| Join on a Pending invoice past `due_date` | Allowed until `expire_invoice` was called | `InvoiceExpired` (#1021) |
| Stalled approved LP | Held an exclusive funding right until the invoice expired; nobody else could fund | Anyone may call `recover_fund_queue` after a grace period to evict them (#1020) |
| Queue size | Unbounded `Vec` in one persistent entry, O(n) scans | Capped at `MAX_FUND_QUEUE_LEN = 50`, else `QueueFull` (#1022) |
| Leftover queue state | `FundQueue`, `QueueResolution`, `FundQueueOpenedAt` stayed forever after cancel/expire/fund | `recover_fund_queue` deletes all four keys once the invoice is not fundable (#1021) |

## `recover_fund_queue(invoice_id) -> Option<Address>` (permissionless)

* **Invoice not fundable** (cancelled, expired, past due, funded, paid, …):
  purge all queue keys, emit `FundQueueRecovered { purged: true }`, return `None`.
* **Fundable, winner resolved, grace elapsed**: evict the winner from the queue,
  delete the resolution, restart the maturity window (`FundQueueOpenedAt = now`)
  for remaining LPs, emit `FundQueueRecovered { purged: false }`, return the
  evicted LP. The evicted LP may re-join.
* **Otherwise**: `QueueNotStalled`. Paused contract: `ContractPaused`.

Grace period: `QUEUE_RESOLUTION_TTL_LEDGERS` (720 ≈ 1h). When the invoice is
within `NEAR_EXPIRY_WINDOW_SECS` (24h) of `due_date` it shortens to
`QUEUE_DELAY_LEDGERS` (120), so a stalled winner cannot run the clock out.

Security properties: moves no funds; only removes an exclusive right and
queue storage; no caller-supplied inputs beyond `invoice_id`; ledger-sequence
based (not manipulable timestamps) except for the near-expiry window check.

## Operations

* Monitor `fund_queue_recovered` events; repeated evictions of the same LP
  indicate a griefing/offline LP. (Reputation penalties are intentionally not
  applied here — follow-up governance decision.)
* Keepers should call `recover_fund_queue` for cancelled/expired invoices to
  reclaim persistent storage and for resolved invoices approaching expiry.

## Migration / rollout / recovery

* Additive: new `DataKey::QueueResolvedAt(u64)`, errors 47/48, one new entry
  point, one new event. No existing key or error code changes; no storage
  migration needed.
* Queues resolved **before** the upgrade have no `QueueResolvedAt`; it reads as
  ledger 0, so they are immediately recoverable once the grace period relative
  to ledger 0 has passed (i.e. always) — operators may recover legacy stuck
  queues right after upgrade.
* Queues longer than 50 that exist before the upgrade keep working; only new
  joins are rejected.
* Rollback: the new key is ignored by older code; no cleanup required.
