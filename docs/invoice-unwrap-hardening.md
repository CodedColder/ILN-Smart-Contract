# Invoice Contract Unwrap Hardening (Issue #1167)

This document records the audit of unguarded `unwrap`/`get` assumptions on
contract state in `contracts/invoice_liquidity` and the conversion of
*predictable missing-state* reads into typed `ContractError` values.

It is deliberately conservative: the goal is to remove host-trapping reads on
paths that can legitimately observe absent state, **not** to change any
happy-path behavior or the external contract API.

---

## 1. Background

Two families of state reads assumed presence:

1. **Invoice payloads.** The helper `invoice::load_invoice(env, id)` read the
   legacy `StorageKey::Invoice(id)` key through
   [`expect("invoice not found")`](../contracts/invoice_liquidity/src/invoice.rs).
   If the key was missing, the invocation *trapped* (host panic) instead of
   returning a contract error. The same helper in
   [`storage.rs`](../contracts/invoice_liquidity/src/storage.rs) carries the
   identical `expect`.

2. **The admin address.** Several entry points read `DataKey::Admin` with
   `.unwrap()`, trapping if authority state was absent.

A host trap aborts the whole invocation and rolls back its writes, but it does
so **without a typed error**: operators, indexers and the SDK cannot
distinguish "missing invoice" from an unexpected contract failure. The modern
idiom already used elsewhere in the contract is a single checked read:

```rust
let invoice = try_load_invoice(&env, invoice_id).ok_or(ContractError::InvoiceNotFound)?;
```

## 2. What changed

All conversions preserve the happy path and the external API; only the
missing-state behavior changes (trap → `ContractError`).

### 2.1 Checked invoice reads (`lib.rs`)

Every `load_invoice(&env, invoice_id)` call in
[`lib.rs`](../contracts/invoice_liquidity/src/lib.rs) was replaced with
`try_load_invoice(&env, invoice_id).ok_or(ContractError::InvoiceNotFound)?`.
This covers `update_invoice`, `convert_invoice_token`, `join_fund_queue`,
`transfer_invoice`, `transfer_lp_position`, `cancel_invoice`,
`expire_invoice`, `claim_yield`, `claim_default`, `appeal_default`,
`resolve_appeal`, `dispute_invoice`, `attach_invoice_evidence`,
`resolve_dispute`, `auto_resolve_dispute` and `get_invoice`.

The same conversion was applied to the shared access helpers in
[`access.rs`](../contracts/invoice_liquidity/src/access.rs)
(`require_submitter_by_id`, `require_payer_by_id`).

### 2.2 Paginated views fail soft (`lib.rs`)

`list_invoices_by_submitter` and `list_invoices_by_lp` previously indexed
their own id list and called the panicking `load_invoice`. These views return
`Vec<Invoice>` (no error channel), so a dangling index entry would have
trapped the *entire* query. They now use `try_load_invoice` and **skip** an id
whose payload is absent:

```rust
if let Some(id) = invoice_ids.get(i) {
    if let Some(invoice) = try_load_invoice(&env, id) {
        result.push_back(invoice);
    }
}
```

### 2.3 Checked admin reads (`lib.rs`)

The `.unwrap()` reads of `DataKey::Admin` in `set_admin`, `add_token` and
`mark_paid` were replaced with
`get_admin(&env).ok_or(ContractError::Unauthorized)?`, matching the idiom used
across the rest of the contract. `mark_paid`'s read is the only one *not*
already shielded by a prior `require_admin`, so this closes a genuine gap
mid-settlement.

### 2.4 NFT owner lookup (`nft.rs`)

`sync_nft_state` resolved the current owner with
`get_invoice_nft_owner(...).unwrap()` when the NFT-exists flag was set but the
invoice payload was gone. It now uses
`.ok_or(ContractError::InvoiceNotFound)?`, consistent with the sibling lookup
already used later in the same function.

## 3. Authorization and failure behavior

- No authorization rule changed. The converted reads occur **after** the
  relevant `require_admin` / `require_submitter_by_id` / `require_payer_by_id`
  / `require_lp` gate, so a caller cannot reach them without the same
  authority as before.
- Failure behavior for a **missing invoice** is now uniformly
  `ContractError::InvoiceNotFound` (code `1`) at every converted site.
- Failure behavior for a **missing admin** on the converted sites is
  `ContractError::Unauthorized` (code `5`). `set_admin`/`add_token` read the
  admin only after `require_admin` succeeded, so the new arm is unreachable
  in practice; `mark_paid` can reach it only if the fee recipient is absent.
- Because a returned `Err` rolls back the invocation, a skipped view read
  (2.2) leaves storage untouched — the dangling index entry is preserved for
  later reconciliation.

## 4. Invariants

| ID | Statement |
|----|-----------|
| INV-NOUNWRAP1 | Reading an absent invoice returns `ContractError::InvoiceNotFound`, never a host trap. |
| INV-NOUNWRAP2 | Paginated views tolerate a dangling index entry and omit it. |
| INV-NOUNWRAP3 | A dangling-entry read does not mutate the submitter/LP index. |
| INV-NOUNWRAP4 | A missing admin on a converted path returns `ContractError::Unauthorized`; no authorization rule is relaxed. |

## 5. Verified findings vs residual risk

**Verified / fixed (VF):**

- VF-1 — `load_invoice`'s `expect("invoice not found")` was reachable as a trap
  from the two paginated views (no `invoice_exists` pre-check). Now fail-soft.
- VF-2 — `mark_paid` read `DataKey::Admin` with `.unwrap()` outside any
  `require_admin` shield. Now `Unauthorized`.
- VF-3 — `sync_nft_state` read the NFT owner with `.unwrap()` under a
  possibly-stale exists flag. Now `InvoiceNotFound`.
- VF-4 — All `Result`-returning invoice entry points now agree with their
  `invoice_exists` probe on a single typed `InvoiceNotFound`.

**Residual risk (RR):**

- RR-1 — `invoice::load_invoice` / `storage::load_invoice` still carry
  `expect`. They are now called only from paths already guarded by a checked
  read, and `storage::load_invoice` has no production caller; the panicking
  helper is left in place to avoid an API/signature change. A future edit that
  reintroduces a bare `load_invoice` call would re-open the trap.
- RR-2 — `due_date` / timestamp conversions use `try_into().unwrap()`
  (`u64 → u32`). The configured duration bounds keep these well under
  `u32::MAX`; a ledger timestamp only exceeds `u32::MAX` after ~2106. Left
  as-is (out of scope for this issue) and noted for completeness.
- RR-3 — `Vec::get(i).unwrap()` inside `for i in 0..len()` loops (fee tiers,
  fund queues, funders) is provably in-bounds and was intentionally left
  unchanged.
- RR-4 — `list_invoices_by_submitter`/`..._by_lp` now *skip* a dangling id,
  so a consumer sees a shorter page rather than an error. This is the
  intended fail-soft direction for a read-only view, but it does mean index
  drift is silent at read time; reconciliation remains the indexer's job.

## 6. Tests

[`tests_unwrap_hardening.rs`](../contracts/invoice_liquidity/src/tests_unwrap_hardening.rs)
adds focused coverage:

- `missing_invoice_reads_return_invoice_not_found` — INV-NOUNWRAP1 across
  `get_invoice`, `update_invoice`, `convert_invoice_token`,
  `transfer_invoice`, `cancel_invoice`, `expire_invoice`, `dispute_invoice`.
- `list_submitter_skips_dangling_index_entry` — INV-NOUNWRAP2/3: a removed
  payload behind a live index entry no longer traps the view and leaves the
  index intact.

## 7. Compatibility

- No `DataKey` variant was added, reordered or removed; storage layout is
  unchanged.
- No external entry point signature changed; `get_invoice` still returns
  `Result<Invoice, ContractError>` and the paginated views still return
  `Vec<Invoice>`.
- Error codes are unchanged (`InvoiceNotFound = 1`, `Unauthorized = 5`).
