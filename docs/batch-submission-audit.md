# Audit: `submit_invoices_batch` partial-failure behaviour

Scope: `contracts/invoice_liquidity/src/lib.rs` (`submit_invoices_batch`),
tests in `tests_batch_atomicity.rs`.

## Verified findings

| # | Finding | Status |
|---|---------|--------|
| 1 | **Atomic.** Any `Err` fails the whole invocation; Soroban discards all storage writes and events from earlier entries. No partial success is possible. | Verified (code + tests) |
| 2 | **Parity gap (fixed).** The batch path skipped three checks that `submit_invoice` enforces: `SelfInvoice`, the `MAX_DISCOUNT_RATE` cap, and the governance `max_invoice_amount` cap (Issue #655). A batch could therefore create invoices a single submit would reject, including above the staged-rollout cap. | Verified, fixed |
| 3 | **No idempotency key.** Replaying an identical batch creates new, distinct invoices (new IDs, reputation `invoices_submitted` incremented again). | Verified, documented, unchanged |
| 4 | Empty batch is a successful no-op returning `[]`. | Verified, unchanged |

## Behaviour by case

| Case | Result |
|------|--------|
| Normal | One ID per input entry, in input order, sequential from the next free ID. |
| Invalid entry (any position) | Whole call fails with that entry's error; no state change, IDs not consumed (no gaps). |
| Unauthorized | Every *distinct* `freelancer` must authorize the call; a missing authorization fails the whole batch. |
| Paused | `ContractPaused`, no side effects. |
| Oversized (> 10) | `BatchTooLarge`, checked before any work. |
| Repeated | Independent new invoices; see finding 3. |
| Stale (due date, caps) | Validated against the ledger time / caps at execution; a batch prepared earlier can fail `InvalidDueDate`/`DueDateTooSoon` and reverts fully. |
| Interrupted | Soroban transactions are atomic: a transaction that does not complete leaves no batch state. |

## Residual risk / assumptions

- A client that loses the response of a *submitted* transaction cannot tell
  whether it landed. Because of finding 3 it must query
  `list_invoices_by_submitter` before retrying, or it may create duplicates.
- Errors do not identify the failing index; clients should pre-validate each
  entry (or bisect) to locate it.
- Atomicity relies on the Soroban host's rollback semantics, not on contract code.
