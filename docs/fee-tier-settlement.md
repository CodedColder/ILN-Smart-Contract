# Fee tiers and settlement rounding (Issue #1179)

This document records the **verified** fee behavior of the `invoice_liquidity`
contract: how the effective protocol fee rate is chosen, how the settled amount
is split between the protocol and the funding LPs, and where integer division
rounds. It is a bounded review, not a claim that a defect exists. Verified
observations are separated from residual risk in [§7](#7-verified-findings-vs-residual-risk).

- **Scope file:** [`contracts/invoice_liquidity/src/lib.rs`](../contracts/invoice_liquidity/src/lib.rs)
  — `update_fee_rate` (`lib.rs:284`), `update_fee_tiers` (`lib.rs:1754`),
  `effective_fee_rate` (`lib.rs:1789`), and the settlement split in `mark_paid`
  (`lib.rs:3260-3328`).
- **Compatibility:** no API, storage key or error-variant change is introduced
  by this review. Everything below describes behavior that already ships.
- **Terminology:** *bps* = basis points; `10_000` bps = 100 %. The *flat rate* is
  the single `StorageKey::FeeRate` value; *tiers* is the
  `StorageKey::FeeTiers` list of `(min_amount, rate_bps)` pairs.

---

## 1. Where the fee comes from

Two independent knobs feed the settlement fee:

| Knob | Entry point | Storage | Access |
| --- | --- | --- | --- |
| Flat rate | `update_fee_rate(rate)` | `StorageKey::FeeRate` (`u32`) | admin, rate-limited (`ECONOMIC_PARAM_COOLDOWN_LEDGERS`) |
| Tier table | `update_fee_tiers(tiers)` | `StorageKey::FeeTiers` (`Vec<(i128, u32)>`) | admin, rate-limited, capped at `MAX_FEE_TIERS` (20) |

`initialize` seeds the flat rate to `0` (`lib.rs:148`). The tier table is absent
by default (`get_fee_tiers` returns an empty `Vec`).

Both are also settable through the multisig governance flow
(`propose_set_fee_rate`, `multisig::AdminAction::SetFeeRate`).

**INV-FEE1 (precedence):** `effective_fee_rate` uses the tier table **whenever it
is non-empty**, and falls back to the flat rate only when the tier table is
empty. A configured tier table therefore fully overrides the flat rate — the
flat rate is *not* a floor and is *not* consulted for any amount once any tier
exists.

---

## 2. Tier selection semantics

`effective_fee_rate(env, invoice_amount)` walks the stored tier vector in
**storage order** and keeps the rate of the *last* entry whose `min_amount` is
`<= invoice_amount`:

```
best_rate = 0
for (min_amount, rate) in tiers:          // storage order
    if invoice_amount >= min_amount:
        best_rate = rate
return best_rate
```

**INV-FEE2 (last matching entry wins):** the selected rate is the last entry in
storage order that the amount satisfies, **not** the entry with the greatest
`min_amount`. The in-code comment claims tiers are "stored sorted ascending by
min_amount", but `update_fee_tiers` performs **no sorting** — it stores the
caller's vector verbatim. For a correctly ascending table the two interpretations
coincide; for an unsorted table they do not (see `tests_fee_tiers_rounding.rs`,
`tier_walk_follows_storage_order_not_sorted_order`).

**INV-FEE3 (below the lowest bracket → zero fee):** `best_rate` is initialised to
`0` and only ever replaced by a matching tier. If `invoice_amount` is smaller
than every tier's `min_amount` (no entry matches), the effective rate is **0**,
so the protocol charges **no** fee. In particular, the smallest configured tier's
rate is *not* used as a floor.

**INV-FEE4 (bounded walk):** the number of tiers is bounded by `MAX_FEE_TIERS`
(20); `update_fee_tiers` rejects a larger table with `TooManyFeeTiers`
(checked after `require_admin`, so authorization takes precedence). This bounds
the per-settlement walk to a fixed cost (Issue #1061).

---

## 3. The settlement split (`mark_paid`, full payment)

On the final payment that brings `amount_paid` up to `invoice.amount`, settlement
computes (`lib.rs:3260-3328`):

```
fee_rate       = effective_fee_rate(env, invoice.amount)
protocol_fee   = invoice.amount * fee_rate / 10_000        // 0 on i128 overflow
distribute     = invoice.amount - protocol_fee              // saturating
funder_share_i = distribute * fund_amount_i / invoice.amount // truncated, per funder
```

- `protocol_fee`, when `> 0`, is paid to the admin via
  `token_safety::pay_or_escrow` (a frozen admin is escrowed, not fatal).
- `distribute` is paid to the funders in `get_invoice_funders` order, one
  `pay_or_escrow` per funder.
- The primary LP's `primary_lp_payout` / `lp_earned` are computed with the same
  formula and are emitted on the `paid` event (`lib.rs:3344-3364`); the LP is
  paid through the *funder loop*, not a separate transfer.

**INV-FEE5 (fee ceiling per invoice is the settled amount):**
`protocol_fee = floor(amount * rate / 10_000)`. For any `rate <= 10_000`
(≤ 100 %) the fee never exceeds the amount, and `distribute = amount - fee >= 0`.

**INV-FEE6 (fee conservation, single funder):** with one funder holding the whole
`invoice.amount`, `funder_share = floor(distribute * amount / amount) =
distribute`, so `protocol_fee + funder_share == invoice.amount` exactly and the
contract's balance is unchanged by settlement.

---

## 4. Rounding

Every division in the split is integer division on `i128`, which truncates toward
zero. Amounts are positive, so this is a floor.

- **INV-FEE7 (fee rounds down):** `protocol_fee = floor(amount * rate / 10_000)`.
  A sub-unit remainder is discarded, never rounded up.
- **INV-FEE8 (shares round down, dust is retained):** each
  `funder_share_i = floor(distribute * fund_amount_i / amount)`. Truncation can
  make `sum(funder_share_i) < distribute`. The difference — **dust** — is left in
  the contract; it is not redistributed and not refunded. With `N` funders the
  dust is `>= 0` and strictly less than `N` (each share loses `< 1` base unit).

Neither the fee nor any share uses rounding-to-nearest or a running remainder.

---

## 5. Entry points and authorization

| Entry point | Authorization | Fee effect |
| --- | --- | --- |
| `mark_paid` | payer (`require_payer_by_id`) | computes and pays the fee + distribution |
| `update_fee_rate` | admin | sets the flat rate |
| `update_fee_tiers` | admin | replaces the tier table (≤ `MAX_FEE_TIERS`) |
| `propose_set_fee_rate` | multisig proposer → governance | sets the flat rate on execution |
| `get_fee_tiers` | anyone | read-only view of the tier table |

The discount rate on an invoice (`discount_rate`, bounded by `MAX_DISCOUNT_RATE`
= 5000 in `submit_invoice`) is a **separate** parameter: it sets the funding cost
`fund_amount - floor(fund_amount * discount_rate / 10_000)` and the freelancer's
upfront payout at funding (`lib.rs:2789-2841`). It is not the protocol fee.

---

## 6. Tests

`contracts/invoice_liquidity/src/tests_fee_tiers_rounding.rs` (Issue #1179) pins
the invariants above:

- `flat_fee_truncates_and_settles_exactly` — INV-FEE7: `3_333_333 * 100 / 10_000`
  is `33_333` (remainder `3_300` discarded).
- `tier_selection_uses_highest_applicable_bracket` — INV-FEE1/INV-FEE2 for an
  ascending table.
- `amount_below_lowest_tier_pays_zero_fee` — INV-FEE3.
- `tier_walk_follows_storage_order_not_sorted_order` — INV-FEE2 for an unsorted
  table.
- `empty_tiers_fall_back_to_flat_fee_rate` — INV-FEE1 fallback.
- `funder_share_truncation_retains_dust_in_contract` — INV-FEE8 with two funders.
- `settlement_conserves_value_between_fee_and_lp` — INV-FEE6.
- `fee_rate_above_ten_thousand_saturates_lp_distribution_to_zero` — the RR-1 edge.
- `update_fee_tiers_rejects_more_than_cap` — INV-FEE4.

These tests document observed behavior; they do not assert a vulnerability.

---

## 7. Verified findings vs. residual risk

### Verified findings (observed, no defect inferred)

- **VF-1 — Tier table overrides the flat rate.** Non-empty tiers are the sole
  source of the effective rate (INV-FEE1).
- **VF-2 — Last storage-order entry wins.** Selection is a linear scan keeping
  the last matching entry, not a max-by-threshold (INV-FEE2).
- **VF-3 — Below the lowest bracket the fee is zero.** `best_rate` starts at 0
  and the lowest tier's rate is not used as a minimum (INV-FEE3).
- **VF-4 — The fee rounds down.** Truncating integer division (INV-FEE7).
- **VF-5 — Share dust stays in the contract.** Truncation of per-funder shares is
  not redistributed (INV-FEE8).

### Residual risk (latent; not a live vulnerability)

- **RR-1 — No on-chain ceiling on the fee rate.** Neither `update_fee_rate` nor
  `update_fee_tiers` bounds `rate`. A rate above `10_000` bps makes
  `protocol_fee > amount`, so `distribute` saturates to `0` and the funding LPs
  receive nothing while the admin is paid out of the contract's own balance. This
  is admin/governance-controlled and rate-limited, but there is no on-chain
  maximum; operators should keep the rate well below `10_000` and monitor the
  `parameter_updated` / `fee_tiers_updated` events.
- **RR-2 — Tier table is not sorted or validated.** `update_fee_tiers` stores the
  caller's vector unchanged and does not validate ordering, uniqueness of
  `min_amount`, or that adjacent thresholds increase. INV-FEE2 then depends on
  storage order. Governance should supply an ascending, de-duplicated table; a
  future hardening could sort/validate on write (a storage-compatible change).
- **RR-3 — Overflow paths are silent-zeroed, inconsistently.** `protocol_fee`
  uses `checked_mul(...).unwrap_or(0)` (an overflow would silently charge **no**
  fee), while the primary LP payout uses `checked_mul(...).ok_or(ArithmeticOverflow)?`
  (an overflow errors). With realistic `amount`/`rate` magnitudes neither can
  overflow `i128`, so this is latent, not live.
- **RR-4 — Dust is unrecoverable by design.** The sub-unit remainder retained by
  truncation (INV-FEE8) accumulates in the contract with no sweep entry point. It
  is economically negligible (≤ N-1 base units per settlement) but is not
  accounted to any party.

### Assumptions and data sources

- Amounts are signed `i128` base units in the invoice's `token`, always positive
  at settlement (`amount > 0` enforced in `mark_paid`).
- `effective_fee_rate` reads instance storage; tier tables are assumed written by
  governance and stable between settlements.
- Authorization for every entry point above is unchanged by this review:
  settlement requires the payer's `require_auth`; the fee knobs are admin-only
  and rate-limited.

---

## 8. Operator guidance

1. Keep the effective rate (flat or tier) well below `10_000` bps; there is no
   on-chain maximum (RR-1).
2. Supply an **ascending, de-duplicated** tier table — selection follows storage
   order, not threshold order (RR-2, INV-FEE2).
3. Expect funder payouts to sum to slightly **less** than `amount - fee`; the
   difference is truncation dust retained in the contract, not an accounting
   error (INV-FEE8).
4. Remember a tier table with no bracket at or below the invoice amount charges
   **zero** fee for that invoice (INV-FEE3).
