# Approved-Token Allowlist Invariants

Issue **#1176**. A bounded audit of the governance **approved-token allowlist**
and its interaction with per-token **funded-volume caps** and **decimals**
across invoice submission, token conversion, batch submission and funding.

This document records **verified behavior** (what the code does today) and
separates it from **residual risk** (things that are true but are policy
choices or gaps, not necessarily defects). It does **not** assume a
vulnerability exists.

- Source: [`contracts/invoice_liquidity/src/lib.rs`](../contracts/invoice_liquidity/src/lib.rs)
  (`add_token`, `remove_token`, `is_approved_token`, `set_token_volume_cap`,
  `correct_token_decimals`), plus the volume-cap helpers in
  [`contracts/invoice_liquidity/src/invoice.rs`](../contracts/invoice_liquidity/src/invoice.rs).
- Tests: [`contracts/invoice_liquidity/src/tests_token_allowlist.rs`](../contracts/invoice_liquidity/src/tests_token_allowlist.rs) (#1176),
  [`contracts/invoice_liquidity/src/tests_token_safety.rs`](../contracts/invoice_liquidity/src/tests_token_safety.rs) (#1070–#1073).
- Related: [token-safety.md](token-safety.md) (SAC admission + freeze/clawback),
  [multi-token.md](multi-token.md) (operator/integration guide).

---

## 1. The allowlist source of truth

`is_approved_token(env, token) -> bool` (`lib.rs`) is the single read used by
every gate. It resolves in a fixed order:

1. **Explicit flag wins.** If `DataKey::ApprovedToken(token)` has ever been
   written, its value is returned verbatim. This is why `remove_token`'s
   explicit `false` does **not** fall through and re-approve a core token.
2. **Config fallback.** Only when no explicit flag has ever been recorded does
   it fall back to the tokens wired in `Config`
   (`usdc_sac_address` / `eurc_sac_address` / `xlm_sac_address`). This covers
   state that predates the explicit flag.
3. Otherwise `false`.

`initialize` writes `ApprovedToken = true` for all three bootstrap tokens, so
after a normal deployment every token on the allowlist has an explicit flag.

| Invariant | Statement |
|-----------|-----------|
| **INV-AL1** | A token is usable only if `is_approved_token` returns `true`. Once an explicit flag exists it is authoritative; `false` never falls through to the Config list. |
| **INV-AL2** | `add_token` is the only way to approve a *new* token; it is admin-only and rate-limited (`DEFAULT_RATE_LIMIT_LEDGERS` = 120). |
| **INV-AL3** | `remove_token` is admin-only and rate-limited; it sets `ApprovedToken = false` and prunes the token from `TokenList`. It removes the token from the allowlist for **new** money movement (see §3), but does not touch existing invoices or balances. |
| **INV-AL4** | Re-approving a removed token (`add_token` again) clears the explicit `false` back to `true`; the token becomes usable again. `TokenDecimals` survives removal. |

---

## 2. Where the allowlist is enforced

Every gate returns `ContractError::Unauthorized` when the token is not
approved. Order matters: for submission paths the allowlist check runs
**before** decimals/amount validation, so an unlisted token is reported as
`Unauthorized`, never `TokenDecimalsNotConfigured` or `InvalidAmount`.

| Entry point | Check | Error | Notes |
|-------------|-------|-------|-------|
| `submit_invoice` | `is_approved_token(token)` | `Unauthorized` | Runs before `validate_invoice_terms_with_token`. |
| `submit_invoices_batch` | `is_approved_token(params.token)` per item | `Unauthorized` | The whole batch rolls back on the first unapproved item. |
| `convert_invoice_token` | `is_approved_token(new_token)` | `Unauthorized` | Only the **target** token is checked; the old token's approval is not re-checked. |
| `correct_token_decimals` | `is_approved_token(token)` | `Unauthorized` | Requiring approval here keeps this from being a de-facto approval path. |
| `fund_invoice` | `is_approved_token(invoice.token)` | `Unauthorized` | Issue #19: a token removed *after* submission cannot receive **further** funding. |
| `mark_paid` / settlement | *(none)* | — | Settlement is **not** gated on the allowlist (see §3 and §6). |

Soroban rolls back **all** storage writes and events when an invocation
returns `Err`, so each rejection leaves the invoice, funder list, balances,
volume counter and allowlist byte-for-byte unchanged.

---

## 3. `remove_token` semantics (verified)

Removal is a **hard gate on new money movement in**, not a global kill switch:

- **Blocks** new submissions (`submit_invoice`, `submit_invoices_batch`,
  `convert_invoice_token` target) — `Unauthorized`.
- **Blocks** further funding of an already-submitted invoice
  (`fund_invoice`, Issue #19) — `Unauthorized`. A partially-funded invoice in
  a removed token is therefore frozen: it can no longer be completed, though
  it is not corrupted and its existing contributions remain recorded.
- **Does not block** `mark_paid` (settlement) of an invoice that was already
  **fully funded** before removal. `mark_paid` never consults the allowlist,
  so committed principal can still be repaid.

Residual nuance: an invoice left `PartiallyFunded` when its token is removed
has no on-chain path to completion or refund through the normal flow (admin
would re-`add_token`, or handle it via the existing dispute/default paths).

---

## 4. Per-token funded-volume caps

Storage: `DataKey::TokenVolumeCap(token) -> i128` (default `0`) and
`DataKey::TokenVolume(token) -> i128` (default `0`). Helper functions live in
`invoice.rs`; the cap is written by `set_token_volume_cap` (`lib.rs`,
admin-only, rate-limited with `ECONOMIC_PARAM_COOLDOWN_LEDGERS` = 360) and
read by `token_volume_cap(token)` (Anyone).

| Invariant | Statement |
|-----------|-----------|
| **INV-VC1** | `cap == 0` means **uncapped** (the default). A cap is enforced only when `cap > 0`. |
| **INV-VC2** | The cap is checked in `fund_invoice`: `existing_volume + fund_amount > cap` → `GlobalVolumeCapExceeded` (=45). Reaching the cap **exactly** is allowed. |
| **INV-VC3** | `TokenVolume` is incremented via `add_volume` with `saturating_add`, attributed by the token's actual SAC address — never by `TokenList` position (Issue #620). |
| **INV-VC4** | `set_token_volume_cap` takes effect immediately; no migration is needed because the check and the counter read the same key. |
| **INV-VC5** | `initialize` does not apply or check any volume cap (volume starts at 0 and is only accumulated by funding). |

Caps are orthogonal to the allowlist: an approved token may still be
volume-capped, and a removed token is rejected before the cap is consulted.

---

## 5. Decimals prerequisites

- `add_token` requires the declared `decimals` to equal the token's own
  `decimals()` and be `≤ MAX_TOKEN_DECIMALS` (18) — `TokenDecimalsUnavailable`
  / `InvalidTokenDecimals` (Issues #1070, #1069). See [token-safety.md](token-safety.md).
- Submission paths validate amounts through `validate_invoice_terms_with_token`
  using the token's **registered** decimals, failing closed with
  `TokenDecimalsNotConfigured` if none is registered.
- `remove_token` does **not** delete `TokenDecimals`; re-adding a token keeps
  or overwrites it via `add_token`.
- Because the allowlist check precedes decimals validation on submission, an
  unlisted token is always `Unauthorized` regardless of its decimals state.

---

## 6. Authorization and failure behavior

- `add_token`, `remove_token`, `set_token_volume_cap`,
  `correct_token_decimals` are **admin-only** (`require_admin`) and
  rate-limited. `add_token` / `remove_token` / `correct_token_decimals` share
  `DEFAULT_RATE_LIMIT_LEDGERS` (120); `set_token_volume_cap` uses
  `ECONOMIC_PARAM_COOLDOWN_LEDGERS` (360). A function's first-ever call is
  always allowed; only repeats within the cooldown are rejected with
  `RateLimited`.
- `is_approved_token`, `token_volume_cap`, `get_token_decimals` are
  **permissionless** reads.
- Events: `token_added` (`TokenAdded { token, decimals }`),
  `token_removed` (`TokenRemoved { token }`), and a `parameter_updated`
  (`ParameterUpdated`) for `set_token_volume_cap`. **Note:** the
  `set_token_volume_cap` event does **not** carry the target token in its
  topics (kept at 3 to match every other `parameter_updated`). Indexers must
  read the call arguments or poll `token_volume_cap(token)` rather than topic
  filtering. This is a deliberate, documented trade-off (see §7).
- Every rejection is a typed `ContractError`, so no partial state is possible.

---

## 7. Verified findings vs. residual risk

### Verified (observed behavior, covered by tests)

- The allowlist gate and its error (`Unauthorized`) are consistent across
  `submit_invoice`, `submit_invoices_batch`, `convert_invoice_token`,
  `fund_invoice` and `correct_token_decimals`.
- An explicit `ApprovedToken = false` is authoritative; removing a bootstrap
  (core) token really disables it (no Config fall-through).
- `remove_token` stops new submissions **and** further funding, but not
  settlement of an already-funded invoice.
- Volume caps are enforced per token at funding, with `0` = uncapped.
- Re-approval after removal works and preserves registered decimals.

### Residual risk / known limitations (policy or gaps, not confirmed defects)

1. **Orphaned + stale test modules.** `tests_multi_token.rs` and
   `tests_access_control.rs` exist but are **not declared** as `mod`s in
   `lib.rs`, so nothing in them runs. In particular,
   `tests_multi_token.rs::test_admin_removing_token_mid_flight_does_not_break_existing_invoice_settlement`
   asserts that `fund_invoice` *succeeds* after `remove_token` — which
   contradicts the Issue #19 gate at `lib.rs` (`fund_invoice` →
   `is_approved_token` → `Unauthorized`). If that module were registered as-is
   it would fail. The #1176 suite encodes the *verified* behavior instead.
2. **Settlement is not allowlist-gated.** This is intentional (committed
   principal must be able to settle), but it means a removed token can still
   produce settlement events/transfers for pre-existing funded invoices.
3. **Partial invoices in a removed token** have no normal completion/refund
   path until the token is re-added; operators must track them explicitly.
4. **Per-cap event has no token topic** (§6): a deliberate choice, but it
   forces indexers to reconcile caps out-of-band.
5. **Bootstrap tokens are approved with unvalidated decimals** at
   `initialize` (Issue #1069) — observable via `audit_token_decimals` and
   correctable via `correct_token_decimals`; see [token-safety.md](token-safety.md).

### Operator guidance

- Before `remove_token`, list invoices still open in that token
  (`PartiallyFunded`) and decide whether to complete, dispute or re-add.
- Prefer `set_token_volume_cap` for staged rollout over removal when the goal
  is to slow (not stop) a token.
- Do not rely on `set_token_volume_cap` events for per-token filtering; poll
  `token_volume_cap(token)`.
