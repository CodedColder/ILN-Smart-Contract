# ADR-007: NFT Invoice Representation

**Date:** 2026-07-26
**Status:** Accepted

## Context

An invoice funded by an LP is, economically, a claim on a future payment —
the LP has effectively bought a discounted receivable. The team had to decide
how to represent ownership of that claim on-chain: as an implicit field on
the `Invoice` record (e.g. a `funder`/`owner` address updated in place), or as
an explicit, transferable token.

Motivating factors:

- **Secondary markets.** LPs may want to exit a position before an invoice's
  due date — sell their claim to another LP at a discount — rather than
  waiting for `mark_paid` or a default. That requires a transferable
  representation of "who currently owns this claim," independent of the
  invoice's internal funding fields.
- **Composability.** A standard token-like object (mint / transfer / burn,
  queryable metadata and ownership) can be referenced by other contracts —
  future collateralized-lending or marketplace contracts — without those
  contracts needing to understand the full `Invoice` state machine.
- **Auditability.** A dedicated NFT lifecycle (minted on submission,
  transferred on funding, burned on settlement) gives a clean, independently
  verifiable event trail for who held a claim at any point in time, separate
  from the invoice's own status transitions.
- **Incremental delivery.** As with the insurance pool (see
  [ADR-006](ADR-006-insurance-pool-design.md)), the team chose to land the
  NFT data model and query surface first, proven by tests, before wiring the
  mint/transfer/burn lifecycle into the invoice state machine's write paths.

## Decision

Represent each invoice as a **soulbound-until-funded NFT**, modeled in
`contracts/invoice_liquidity/src/nft.rs` as `InvoiceNftMetadata`:

```rust
pub struct InvoiceNftMetadata {
    pub invoice_id: u64,
    pub amount: i128,
    pub due_date: u32,
    pub discount_rate: u32,
    pub token: Address,
    pub owner: Address,
    pub minted_at: u32,
}
```

Storage and lifecycle are keyed per invoice — `DataKey::InvoiceNft(invoice_id)`
for metadata, `DataKey::InvoiceNftOwner(invoice_id)` for a lightweight
ownership lookup — rather than using a general-purpose token ID space, since
the invoice ID already uniquely identifies each NFT and there is no need for
multiple NFTs per invoice.

The module exposes four lifecycle operations plus two read-only queries:

| Function | Intended trigger | Effect |
|----------|-------------------|--------|
| `mint_invoice_nft` | Invoice submission | Creates the NFT, owned by the submitting freelancer. |
| `transfer_invoice_nft` | Invoice funding | Reassigns ownership from freelancer to the funding LP. |
| `burn_invoice_nft` | Invoice paid | Destroys the NFT once the underlying claim is settled. |
| `invoice_nft_exists` | — | Existence check without loading metadata. |
| `query_nft_metadata` (public) | — | Returns full metadata, or `None`. |
| `query_nft_owner` (public) | — | Returns current owner, or `None`. |

Each lifecycle operation emits a corresponding event
(`InvoiceNftMinted` / `InvoiceNftTransferred` / `InvoiceNftBurned`) for
off-chain indexing.

Ownership is enforced at the module level: `transfer_invoice_nft` and
`burn_invoice_nft` both verify the caller-supplied `from`/`owner` argument
matches the stored owner, returning `ContractError::Unauthorized` otherwise.

## Alternatives Considered

| Alternative | Why rejected |
|-------------|--------------|
| **Track ownership as a plain field on `Invoice` (no separate NFT module)** | Works for the current single-owner-at-a-time model, but gives no standard mint/transfer/burn interface for other contracts (marketplaces, collateralized lending) to build against, and mixes claim-ownership concerns into the invoice state machine. |
| **General-purpose token ID space (arbitrary `token_id`, not tied to `invoice_id`)** | Adds an indirection layer with no benefit here — invoices are 1:1 with their NFT and already have a unique `u64` ID, so a separate ID space would only add a lookup table to maintain. |
| **Full SEP-41-style fungible/semi-fungible token standard** | Invoices are inherently non-fungible (each has a unique amount, due date, and discount rate) and single-supply; a fungible token standard adds interface surface (allowances, decimals) that doesn't apply. |
| **Wire the full mint/transfer/burn lifecycle into `submit_invoice`/`fund_invoice`/`mark_paid` in this iteration** | The data model, storage layout, and query API needed to be validated and tested first (see `tests_nft_query.rs`) before coupling NFT side-effects to the core lending write paths, which already carry significant complexity (escrow, discounting, reputation, disputes). |

## Consequences

**Positive:**
- A transferable NFT per invoice is a prerequisite for secondary-market
  trading of funded claims and for future composability with other
  contracts.
- The metadata (`amount`, `due_date`, `discount_rate`, `token`) is
  self-contained, so a marketplace or lending contract can price a claim
  without cross-calling back into `invoice_liquidity` for invoice details.
- Ownership checks and event emission are centralized in `nft.rs`, giving one
  place to audit the NFT invariants rather than scattering them across the
  invoice lifecycle handlers.
- Read-only queries (`query_nft_metadata`, `query_nft_owner`) are already
  wired into the public contract API and exposed through the SDK
  (`getNftMetadata`), so integrators can build against the data model today.

**Negative / Trade-offs:**
- **The mint/transfer/burn lifecycle is not currently invoked from
  `submit_invoice`, `fund_invoice`, or `mark_paid`.** As of this writing, no
  code path in `lib.rs` calls `nft::mint_invoice_nft`,
  `nft::transfer_invoice_nft`, or `nft::burn_invoice_nft` — only the
  query functions are wired in. `query_nft_metadata`/`query_nft_owner`
  therefore return `None` for every invoice today, and `tests_nft_query.rs`
  only exercises the not-found paths. Wiring the lifecycle calls into the
  three invoice-state transitions is required before this feature is
  functionally complete.
- Once wired, transferring the NFT independently of the invoice's `funder`
  field (used internally for escrow accounting) creates two sources of truth
  for "who holds this claim" that must be kept in sync — a secondary-market
  transfer of the NFT would need to also update `Invoice.funder`, or the two
  must be reconciled at read time.
- Persistent storage per invoice (metadata + owner key) adds rent/TTL
  management overhead on top of the existing `Invoice` record.
- No `approve`/`transfer_from` pattern exists yet, so a marketplace contract
  cannot escrow-and-swap an NFT on a seller's behalf without either the
  seller directly calling `transfer_invoice_nft` or a future extension to
  the ownership model.

## Status update (Issue #854)

The wiring described below as follow-up work has since landed:
`nft::sync_nft_state` is now called from every invoice write path in
`contracts/invoice_liquidity/src/lib.rs`, maintaining the invariant "NFT
exists iff invoice is Funded / PartiallyFunded / Defaulted / Appealed /
Disputed, holder = funder (or lead LP)". See the consolidated walkthrough in
[`docs/reputation-model.md`](../reputation-model.md#full-lifecycle-guide-issues-854--single-cross-referenced-narrative).

## Follow-up work

- ✅ Done: lifecycle calls are wired via `sync_nft_state` (see status update
  above) — `mint` on funding, `transfer` on holder change, `burn` on
  settlement/cancel/expiry.
- Reconcile `InvoiceNftMetadata.owner` with `Invoice.funder` once the
  lifecycle is wired, so the two cannot drift.
- Consider an `approve`/`transfer_from`-style extension if a marketplace
  contract needs to move NFTs on behalf of their owner.
