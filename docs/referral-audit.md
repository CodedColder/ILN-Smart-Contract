# Audit: referral code validation and replay

Scope: `submit_invoice`, `submit_invoices_batch`, `record_referral`,
`get_referral_stats` in `contracts/invoice_liquidity/src/lib.rs`; tests in
`tests_referral.rs` (section "Audit: referral code validation and replay").

## Data model and sources

- `ReferralCode` (`None | Present(BytesN<32>)`) is supplied by the submitter and
  stored on the invoice (cold-path `InvoiceMetadata`).
- `DataKey::ReferralCount(BytesN<32>)` is a `u64` counter in persistent storage
  (`get_referral_stats`, read-only, anyone).
- The contract contains **no reward logic**: nothing pays, mints or ranks by
  referral. Counts are informational, consumed off-chain.

## Verified findings

| # | Finding | Status |
|---|---------|--------|
| 1 | **No "update" path exists.** The count is written in exactly one place (`record_referral`), once per successfully created invoice. `update_invoice`, `convert_invoice_token`, `cancel_invoice`, `fund_invoice`, `mark_paid` and the default/expiry flows never touch a count or an invoice's `referral_code`. So malformed/duplicate/unauthorized *updates* cannot alter accounting. | Verified (code + tests) |
| 2 | **Malformed codes are rejected by the ABI.** `BytesN<32>` is fixed-size; a wrong-length value cannot be decoded by the host, so no in-contract length check is needed. | Verified by type |
| 3 | **Unauthorized submissions cannot count.** `require_submitter` runs before any state write; a failed auth, pause, or any validation error aborts the invocation and rolls back the increment. | Verified (tests) |
| 4 | **Batch is atomic for counts.** A failing entry reverts increments from earlier entries; batch and single submit share one helper and one counter. | Verified (tests) |
| 5 | **Duplication cleaned up.** The increment was duplicated inline in both submit paths with an unchecked `+ 1`. It now lives in `record_referral` and fails closed with `ArithmeticOverflow` (unreachable for `u64`; the workspace also sets `overflow-checks = true`). | Fixed, behaviour-neutral |
| 6 | **Docs drift.** `docs/storage-layout.md` listed the counter as `u32`; it is `u64`. | Fixed |

## Behaviour by case

| Case | Result |
|------|--------|
| Normal | Count of the code increases by 1 per created invoice. |
| Repeated (replay) | Identical resubmission is a new invoice and increments again; there is no per-referee or per-freelancer dedupe. |
| Unauthorized | Fails; no count change. |
| Stale / interrupted | A transaction that fails or does not complete leaves counts unchanged. A cancelled/expired/defaulted invoice **keeps** its count (counts measure submissions, not outcomes). |
| Malformed | Not representable (finding 2). |

## Residual risks (not changed here, no migration)

1. **Codes are unauthenticated.** Anyone can submit with any code and inflate it
   (`test_any_submitter_can_inflate_any_code_count`). Counts must not gate
   rewards by themselves; reward logic should require funded/paid invoices,
   distinct submitters, and an off-chain code owner registry.
2. **All-zero code is valid.** `[0;32]` is an ordinary code, not "no referral".
   Off-chain consumers must use `ReferralCode::None` for absence.
3. **No TTL extension on `ReferralCount`.** The counter is not bumped on write,
   so a long-idle code's counter can reach archival, unlike invoice records.
   Fixing this changes resource usage and is left to a separate change.
4. **Counts include never-funded invoices.** Spam invoices from many freelancer
   addresses cost only the (small) submission cost.
