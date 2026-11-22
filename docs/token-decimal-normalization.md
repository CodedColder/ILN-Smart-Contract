# Token decimal normalization invariants (Issue #1177)

This document records the **verified** decimal-normalization behavior of the
`invoice_liquidity` contract across every financial entry point that touches a
token amount: submission, funding, settlement and outbound payout. It is a
bounded review, not a claim that a defect exists. Verified observations are
separated from residual risk in [§6](#6-verified-findings-vs-residual-risk).

- **Scope file:** [`contracts/invoice_liquidity/src/token_safety.rs`](../contracts/invoice_liquidity/src/token_safety.rs)
  (decimals registration/validation) and the normalizers/call sites in
  `contracts/invoice_liquidity/src/lib.rs`.
- **Compatibility:** no API, storage key or error-variant change is introduced
  by this review. Everything below describes behavior that already ships.
- **Terminology:** *registered decimals* is the `u32` stored under
  `DataKey::TokenDecimals(token)`. *Native precision* is the integer scale the
  token's own `decimals()` reports (e.g. 7 for a classic-asset SAC, 6 for a
  6-decimal SEP-41 token).

---

## 1. Source of truth for precision

| Layer | Source | Enforced by |
| --- | --- | --- |
| Registered decimals | `DataKey::TokenDecimals(token)` | `token_safety::registered_token_decimals` (`token_safety.rs:64`) |
| On-chain decimals | token's `decimals()` | `token_safety::validate_token_decimals` (`token_safety.rs:44`) |
| Mismatch audit | both of the above | `token_safety::audit_token_decimals` (`token_safety.rs:107`) |

- `initialize` writes bootstrap decimals directly: USDC `6`, EURC `6`, XLM `7`
  (`lib.rs:175-207`). These values are **not** checked against the token's own
  `decimals()`.
- `add_token` requires `declared == token.decimals()`; a token that cannot
  report decimals is rejected with `TokenDecimalsUnavailable`, and an
  out-of-range or mismatched value with `InvalidTokenDecimals`
  (`token_safety.rs:44-60`). `MAX_TOKEN_DECIMALS = 18`.
- `registered_token_decimals` **fails closed** with
  `TokenDecimalsNotConfigured` when a token has no registered entry; there is no
  silent fallback to 6.

---

## 2. The normalizers

Three helper functions exist in `lib.rs`, selected by token identity:

| Selector | Matcher | Normalizer | Definition |
| --- | --- | --- | --- |
| XLM | `is_xlm_token` (`lib.rs:4293`) | `normalize_xlm_amount` (`lib.rs:4302`) | `amount` |
| EURC | `is_eurc_token` (`lib.rs:4321`) | `normalize_eurc_amount` (`lib.rs:4330`) | `amount` |
| default | fallthrough | `normalize_usdc_amount` (`lib.rs:4316`) | `amount` |

`is_xlm_token` / `is_eurc_token` / `is_usdc_token` compare the address against
`Config::xlm_sac_address` / `eurc_sac_address` / `usdc_sac_address`
respectively, returning `false` when no config is set.

**INV-DN1 (identity):** all three normalizers are the identity function. No
scaling, shifting or truncation occurs. Amounts are therefore stored, accounted
and transferred in the token's **native precision**; registered decimals are
never used to rescale an amount.

**INV-DN2 (selection is observationally inert today):** because every branch is
identity, the `is_xlm_token` → `else if is_eurc_token` → `else` ladder produces
the same result regardless of which branch is taken. Note the `else` branch
covers **both** USDC and every custom token; see [RR-2](#6-verified-findings-vs-residual-risk).

---

## 3. Amount floor (the one place decimals are consumed)

`validate_invoice_terms_with_token` (`lib.rs:4347`) computes

```
min_amount = ten_pow(registered_token_decimals(token))
```

and rejects `amount < min_amount` with `InvalidAmount`. `ten_pow`
(`lib.rs:4281`) is `10^exp` with saturating multiplication.

- 6-decimal token → floor `1_000_000` (= 1 whole unit at 6 decimals).
- 7-decimal token → floor `10_000_000` (= 1 whole unit at 7 decimals).

**INV-DN4 (floor scaling):** the invoice amount floor is exactly `10^registered
decimals`, and an unregistered token fails closed with
`TokenDecimalsNotConfigured` before any amount comparison.

Call sites: `submit_invoice` (`lib.rs:1989`), `update_invoice` (`lib.rs:2105`),
`submit_invoices_batch` (`lib.rs:2230`). This is the only entry point that
consumes decimals for an amount comparison; the former decimals-blind helper
with a hardcoded 6-decimal floor was removed in Issue #1051.

---

## 4. Entry-point map

| Entry point | Amount handling | Decimals used? |
| --- | --- | --- |
| `submit_invoice` | floor via `validate_invoice_terms_with_token`; stored as-is | floor only |
| `update_invoice` | same floor; stored as-is | floor only |
| `submit_invoices_batch` | same floor per item; stored as-is | floor only |
| `convert_invoice_token` | target token must be approved; amount unchanged | no |
| `fund_invoice` | transfer `cost = normalized(fund_amount) - discount`; ledger fields, funder list and `add_volume` record raw `fund_amount` | no (identity) |
| `mark_paid` | transfer `normalized(amount)`; `amount_paid` records raw `amount` | no (identity) |
| `token_safety::transfer_out_strict` / `pay_or_escrow` / `claim_pending` | transfer the exact `amount` argument | no |
| `add_token` | fee-on-transfer probe at fixed `1_000_000`; validates `declared == decimals()` | validation only |

**INV-DN3 (accounting parity):** along every funding, settlement and payout
path, the number of base units moved equals the number of base units recorded.
This holds *because* the normalizers are identity; it is asserted, not assumed,
by the tests in §5.

**INV-DN5 (payouts unscaled):** `token_safety.rs` performs no scaling at all —
`transfer_out_strict` (`token_safety.rs:240`), `pay_or_escrow`
(`token_safety.rs:259`) and `claim_pending` (`token_safety.rs:305`) transfer the
exact amount they are given, and `pending_payout` bookkeeping stores that same
value. Any rescaled amount handed to them would be paid out verbatim.

---

## 5. Tests

`contracts/invoice_liquidity/src/tests_token_decimal_normalization.rs` (Issue
#1177) pins the invariants above:

- `normalizers_are_identity_for_every_token_class` — INV-DN1 for XLM, EURC, USDC
  and a token that is none of the three.
- `six_decimal_token_floor_is_one_whole_unit` / `seven_decimal_token_floor_is_one_whole_unit`
  — INV-DN4 at the exact boundary (below-floor rejected, at-floor accepted).
- `unregistered_token_fails_closed` — `TokenDecimalsNotConfigured`.
- `funding_transfers_exactly_amount_minus_discount_in_native_units` — INV-DN3
  end-to-end across a full fund.
- `settlement_transfers_exactly_amount_in_native_units` — INV-DN3 across
  `mark_paid`.
- `custom_token_amount_is_not_rescaled` — a custom (non-USDC/EURC/XLM) token's
  amount passes through the `else` branch unchanged.

These tests document observed behavior; they do not assert a vulnerability.

---

## 6. Verified findings vs. residual risk

### Verified findings (observed, no defect inferred)

- **VF-1 — Normalization is currently a no-op.** All three normalizers are
  identity (INV-DN1). Amounts are native-precision throughout.
- **VF-2 — Decimals are used only for the submission/update floor.** No other
  code path rescales an amount by registered decimals.
- **VF-3 — Parity holds today.** Transferred base units equal accounted base
  units in funding, settlement and payouts (INV-DN3, INV-DN5).
- **VF-4 — Unregistered tokens fail closed.** `registered_token_decimals`
  returns `TokenDecimalsNotConfigured`; there is no silent 6-decimal default.

### Residual risk (latent; not a live vulnerability)

- **RR-1 — Dual accounting of the funding amount.** In `fund_invoice` the
  transferred `cost` is derived from `normalized_fund_amount`
  (`lib.rs:2789-2793`) while `invoice.amount_funded`, the funder list and
  `add_volume` all use the raw `fund_amount` (`lib.rs:2814-2865`). Identical
  today only because normalization is identity. If any normalizer were ever made
  non-identity, the principal moved and the principal accounted would diverge.
  `mark_paid` has the same shape: it transfers `normalized_amount`
  (`lib.rs:3236`) but adds raw `amount` to `amount_paid` (`lib.rs:3227-3230`).
- **RR-2 — `add_volume` uses the raw amount.** Volume attribution
  (`invoice.rs:1087`) receives `fund_amount`, not the normalized value, so any
  future scaling would silently desynchronize the volume cap counter from
  principal.
- **RR-3 — Custom tokens share the USDC branch.** The `else` branch maps every
  token that is not XLM or EURC — including custom SEP-41 tokens — onto
  `normalize_usdc_amount`. Harmless while identity, but the name invites a future
  maintainer to make it USDC-specific and thereby mis-scale custom tokens.
- **RR-4 — Bootstrap decimals are unvalidated and may already diverge.** USDC
  and EURC are registered as `6` in `initialize` without probing the token.
  Classic-asset SACs (including real USDC/EURC on Mainnet) report `7`, so the
  stored floor can be one decade too small for those tokens. This is observable
  on-chain via `audit_token_decimals` / `TokenDecimalsAudit::matches` and by an
  off-chain monitor; it is documented as a known limitation in
  `docs/token-safety.md`. Because normalization is identity, a wrong registered
  value affects **only** the minimum-amount floor, not the transferred amount.
- **RR-5 — Stale parameter documentation.** The `InvoiceParams::amount` doc
  comment in `invoice.rs:99` describes the amount as "stroops
  (1 USDC = 10_000_000)", i.e. 7 decimals, which contradicts the bootstrap 6
  decimals recorded for USDC. Off-chain integrators should rely on the
  token's own `decimals()`, not this comment.

### Assumptions and data sources

- Amounts are signed `i128` base units in the invoice's `token`.
- Registered decimals are assumed stable for a token once written; removing a
  token does not clear its `TokenDecimals` entry (`lib.rs:1052-1082`).
- Authorization for every entry point above is unchanged by this review: funding
  and settlement require the caller's `require_auth`; `add_token` /
  `remove_token` / `set_token_volume_cap` are admin-only.

---

## 7. Operator guidance

1. After `initialize`, call `audit_token_decimals` for each bootstrap token and
   reconcile any divergence with the values the SACs actually report before
   relying on the amount floor.
2. Do not assume the on-chain amount is rescaled; it is always in the token's
   native precision.
3. Treat RR-1..RR-3 as review gates: any change that makes a normalizer
   non-identity must also normalize every accounting write and the `add_volume`
   call, or parity breaks.
