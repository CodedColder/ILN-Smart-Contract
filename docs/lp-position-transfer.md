# LP-position transfer ownership & balance changes (#1175)

Scope: [`contracts/invoice_liquidity/src/lib.rs`](../contracts/invoice_liquidity/src/lib.rs)
— `transfer_lp_position` (lines [3028–3090](../contracts/invoice_liquidity/src/lib.rs#L3028-L3090))
— plus the downstream claims that must follow the new owner: `claim_yield`
([3421–3453](../contracts/invoice_liquidity/src/lib.rs#L3421-L3453)), the settlement
payout loop in `mark_paid`
([3360–3375](../contracts/invoice_liquidity/src/lib.rs#L3360-L3375)), and
`nft::sync_nft_state`
([279–357](../contracts/invoice_liquidity/src/nft.rs#L279-L357)).

Test module:
[`contracts/invoice_liquidity/src/tests_lp_position_transfer.rs`](../contracts/invoice_liquidity/src/tests_lp_position_transfer.rs).

This is a verification-only change: no contract behavior, storage layout, or
public API is modified. Its purpose is to make the ownership handoff of a
funded LP position explicit and regression-locked.

## Non-goals

- The fund queue / MEV mitigation around *who funds first*
  (`tests_mev_mitigation.rs`, `tests_lp_priority_queue.rs`).
- NFT minting/transfer/burn mechanics in isolation (`tests_nft_event_emission.rs`).
- Reputation scoring policy (≤100 cap, baselines) — only its non-transfer is pinned here.
- Any cross-repository SDK/indexer work.

## Model

`transfer_lp_position` reassigns the position of an **already fully funded**
invoice. It is a pure re-indexing: no tokens are moved, no amount changes.

The transfer rewrites the ownership triad:

1. **Primary pointer** — `invoice.funder` (`Some(current) → Some(new)`).
2. **Index rows** — every row of `InvoiceFunders(id)` whose address equals the
   current holder is re-pointed to the new holder, **amount and order preserved**.
3. **Reverse index + NFT** — `LpInvoices(current)` drops the id,
   `LpInvoices(new)` gains it (de-duplicated), and `sync_nft_state` transfers the
   invoice NFT to the new owner.

The settlement payout and the yield claim both key off this state, so they
follow the transferee automatically. The write sequence is all-or-nothing: any
`Err` (or host panic) rolls the whole invocation back under Soroban.

### Entry points and authorization

| Entry point | Caller | Notes |
|---|---|---|
| `transfer_lp_position` | current holder (`invoice.funder.require_auth`) | paused-gated; transferee auth **not** required (VF-LPT3) |
| `claim_yield` | current funder (`require_lp_by_id(invoice.funder)`) | returns 0 until `Paid` |
| `mark_paid` | payer | pays each `InvoiceFunders` row its share |
| `claim_default` | a listed funder | refunds each `InvoiceFunders` row |

`transfer_lp_position` is valid **only** in `Funded`. Every other status returns
a typed error (see below); `Funded` is the only status in which a single
`invoice.funder` pointer plus a complete obligation coexist.

### Data sources

All assertions read live contract state: `get_invoice` (`.funder`, `.status`),
`query_nft_owner`, and, through `env.as_contract(...)`,
`get_invoice_funders`, `get_lp_invoices`, `get_lp_score`. Token balances are read
via the SAC `TokenClient`.

## Failure behavior

| Condition | Result |
|---|---|
| paused | `ContractPaused` |
| unknown invoice | `InvoiceNotFound` |
| `Pending` | `NotFunded` |
| `PartiallyFunded` | `NotFunded` |
| `Funded` | success |
| `Paid` | `AlreadyPaid` |
| `Defaulted` | `InvoiceDefaulted` |
| `Appealed` | `InvoiceAppealed` |
| `Disputed` | `InvoiceDisputed` |
| `Expired` | `InvoiceExpired` |
| `Cancelled` | `AlreadyCancelled` |
| caller != `invoice.funder` | host auth failure |
| `new_lp == current_lp` | `Unauthorized` |

## Invariants

- **INV-LPT1** — a successful transfer re-points *every* ownership index
  (`invoice.funder`, `InvoiceFunders`, NFT holder, `LpInvoices`) at the
  transferee within one invocation.
- **INV-LPT2** — the transfer is a slot replacement: funded amounts and row
  ordering of `InvoiceFunders` are unchanged; only the matching row's address
  changes.
- **INV-LPT3** — chained transfers (A→B→C, and A→B→A) converge on exactly one
  canonical owner with no stale rows or dangling reverse-index entries.
- **INV-LPT4** — the reverse-index rewrite is scoped to the one invoice; other
  positions of either party are untouched.
- **INV-LPT5** — the yield claim right follows the current `invoice.funder`; the
  previous owner loses it the moment the transfer lands.
- **INV-LPT6** — the settlement payout is driven by the `InvoiceFunders` index,
  so the transferee receives the position payout and the previous owner receives
  nothing.
- **INV-LPT7** — the NFT holder tracks the funder pointer (transferred on
  transfer; burned once the invoice leaves the fundable set).
- **INV-LPT8** — the transfer moves no tokens; balances of both parties and of
  the contract are unchanged.
- **INV-LPT9** — exactly one `lp_position_transferred` event is emitted.
- **INV-LPT10** — only the current holder's authorization is accepted; an
  unauthorised caller or a self-transfer is rejected and mutates no state.
- **INV-LPT11** — only `Funded` is transferable; every terminal/outcome status
  surfaces its typed error and leaves state untouched.
- **INV-LPT12** — an unknown invoice and a paused contract are rejected before
  any state change.

## Verified findings

- **VF-LPT1** — the `InvoiceFunders` rewrite is *per matching row* and does not
  coalesce. If the transferee is already a partial funder of the same invoice,
  the address ends up **listed twice**, each row carrying an independent amount.
  Observed through a two-funder route (A partial, B completes, transfer B→A):
  the list holds `[(A, x), (A, y)]`. Settlement pays each row its own share, so
  the total is correct, but the index is no longer one-row-per-address. The
  reverse index (`LpInvoices`) still de-duplicates the id, so this asymmetry
  exists only in the funder-row list.
- **VF-LPT2** — reputation is **not** part of the transferred position. The
  funding-time score credit stays with the funder of record; the transferee
  gains none.
- **VF-LPT3** — the transferee's authorization is **not** required. The current
  holder can unilaterally assign the position — and therefore the future
  settlement payout and yield — to any address (only `current_lp.require_auth()`
  is checked).

## Residual risks

- **RR-LPT1** — VF-LPT1's duplicate rows are not coalesced anywhere. An off-chain
  consumer that assumes one row per address, or that both iterates rows *and*
  keys a map by address, can mis-render "funders" for such an invoice.
- **RR-LPT2** — because VF-LPT2 leaves reputation with the funder of record, the
  current holder's LP score is not a reliable signal of the position's quality.
- **RR-LPT3** — a `PartiallyFunded` position **cannot** be reassigned; it must
  first be completed to `Funded`. There is no partial-position handoff path.
- **RR-LPT4** — `transfer_lp_position` does not take the reentrancy lock that
  `cancel_invoice` / `mark_paid` do. It makes no external token transfer today
  (only `require_auth` and event emission), so there is no reentrancy surface,
  but the asymmetry is a maintenance hazard if a future change adds a call.
- **RR-LPT5** — `transfer_lp_position` uses the panicking `load_invoice` after a
  separate `invoice_exists` check, rather than the checked `try_load_invoice`
  style adopted for other reads in #1167. Safe today (the guard precedes the
  load) but not the hardened pattern.
- **RR-LPT6** — pre-existing (not caused by transfer): the settlement event's
  `primary_lp` / `lp_earned` fields derive from `InvoiceFunders[0]`, while the
  NFT lead-LP derives from the maximum-amount row. With multi-funder rows these
  can name different addresses. A transfer can move the position to a non-zero
  index but does not itself resolve this divergence.

## Test mapping

| Test | Invariant / finding |
|---|---|
| `test_transfer_updates_ownership_triad_atomically` | INV-LPT1 |
| `test_transfer_replaces_index_slot_without_changing_amount_or_order` | INV-LPT2 |
| `test_chained_transfer_keeps_single_canonical_owner` | INV-LPT3 |
| `test_transfer_to_prior_partial_funder_duplicates_index_rows` | VF-LPT1 |
| `test_transfer_only_reindexes_this_invoice` | INV-LPT4 |
| `test_old_lp_loses_yield_claim_after_transfer` | INV-LPT5 |
| `test_new_lp_controls_yield_after_settlement` | INV-LPT5 |
| `test_claim_yield_zero_while_still_funded_after_transfer` | INV-LPT5 |
| `test_settlement_pays_new_owner_only` | INV-LPT6 |
| `test_nft_owner_follows_transfer_then_burns_on_settlement` | INV-LPT7 |
| `test_transfer_moves_no_tokens` | INV-LPT8 |
| `test_transfer_does_not_move_reputation_score` | VF-LPT2 |
| `test_transfer_emits_single_position_event` | INV-LPT9 |
| `test_transfer_requires_current_lp_authorization` | INV-LPT10 |
| `test_same_lp_transfer_rejected_and_state_untouched` | INV-LPT10 |
| `test_transfer_rejected_for_pending_and_partial_states` | INV-LPT11 |
| `test_transfer_rejected_for_paid_expired_and_cancelled` | INV-LPT11 |
| `test_transfer_rejected_for_defaulted_position` | INV-LPT11 |
| `test_transfer_unknown_invoice_rejected` | INV-LPT12 |
| `test_transfer_blocked_when_paused` | INV-LPT12 |
| `test_position_can_be_transferred_back_to_original_lp` | INV-LPT3 |

## Compatibility

No `DataKey` variants, storage encodings, event names, or entry-point signatures
are added or changed by this change; it adds tests and this document only. It is
therefore safe to land alongside other `invoice_liquidity` changes without a
migration.
