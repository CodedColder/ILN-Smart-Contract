# MEV mitigation: ledger-placement behaviour of the fund queue

Scope: `join_fund_queue`, `resolve_fund_queue`, `recover_fund_queue`,
`fund_invoice` in `contracts/invoice_liquidity/src/lib.rs`. Tests:
`tests_mev_mitigation.rs` (section "Ledger-placement audit").

## Verified behaviour

| Situation | Result |
|-----------|--------|
| Two LPs join in the same ledger | Order inside the ledger is irrelevant; queue is sorted by score, winner chosen from *current* scores at resolution. |
| Join and resolve in the same ledger | Impossible: `QueueNotMature` until `QUEUE_DELAY_LEDGERS` after the **first** join. Later joins never reset the timer; a rejected duplicate join does not either. |
| Join after maturity, before anyone resolves | Accepted and competes. `resolve_fund_queue` is the cut-off, not the delay. |
| Join after resolution | `NotApprovedFunder`; winner unchanged. |
| Non-winner funds after resolution (same or later ledger) | `NotApprovedFunder`, no state change. |
| Score changes between join and resolve | Resolution re-reads current scores (stale snapshot cannot win). |
| Scores change after resolution | No effect; resolution is final and idempotent. |
| Rejected resolve (`QueueNotMature`) | No state written; queue stays open, timer unchanged. |
| Stalled winner | `recover_fund_queue` fails `QueueNotStalled` until `QUEUE_RESOLUTION_TTL_LEDGERS` after resolution, then evicts the winner and restarts the delay for the rest. |
| Paused | Join/resolve/recover rejected (`ContractPaused`); the delay is ledger-based, so it keeps elapsing. |
| Invoice expires while queued | Join/resolve rejected (`InvoiceExpired`); no winner is locked in. |
| Unauthorized join | LP must authorize; a call authorized by another address fails and leaves the queue untouched. |

## Residual risks (not changed here)

1. **No exclusivity before resolution.** Joining only registers intent. Until a
   winner is resolved, `fund_invoice` is first-come-first-served, so a
   non-queued LP can fund directly (pinned by
   `test_unresolved_queue_does_not_block_direct_funding`). The queue protects
   only invoices whose queue has been resolved.
2. **Resolution timing is caller-controlled.** Anyone may call resolve at the
   first mature ledger; an LP whose join lands after that call (or after a
   delayed call but before resolve) can win or lose purely on transaction
   ordering. The delay narrows, but cannot remove, this window.
3. **Equal scores use the network PRNG** (`env.prng()`), which is not a
   source of unpredictable randomness against a validator.
4. **Full-queue displacement (#1095) is unreachable.** `join_fund_queue` rejects
   with `QueueFull` at `MAX_FUND_QUEUE_LEN` (Issue #1022) *before* the later
   block that would displace the lowest-scored entry for a higher-scored joiner.
   Behaviour today is "full queue always rejects"; the displacement code and its
   `FundQueueEntryDisplaced` event cannot execute.

## Repair noted while working on this audit

`FundQueueTieBroken` and `FundQueueEntryDisplaced` were referenced in
`lib.rs` but not defined in `events.rs`, and `tied_count` was undefined in
`resolve_fund_queue`. Definitions were restored (matching the original
introducing commit) so the queue code compiles.
