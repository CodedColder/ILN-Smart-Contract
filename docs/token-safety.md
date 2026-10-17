# Token Configuration and SAC Safety

Issues #1070, #1071, #1072, #1073. Covers how `invoice_liquidity` admits
tokens, and how it behaves when a Stellar Asset Contract (SAC) issuer
freezes an account or claws back funds.

Source: [`contracts/invoice_liquidity/src/token_safety.rs`](../contracts/invoice_liquidity/src/token_safety.rs).
Tests: [`contracts/invoice_liquidity/src/tests_token_safety.rs`](../contracts/invoice_liquidity/src/tests_token_safety.rs).

---

## Summary of controls

| Issue | Risk (audit path) | Control |
|-------|-------------------|---------|
| #1070 | EURC approved in `initialize` with no `TokenDecimals` entry; `add_token` trusted the admin-supplied `decimals`; submit validation silently fell back to 6 (`lib.rs` initialize, `add_token`, `validate_invoice_terms_with_token`). A wrong precision mis-scales the minimum invoice amount by 10× per digit. | `add_token` rejects tokens that do not answer `decimals()` (`TokenDecimalsUnavailable`), values above 18 or that differ from the token's own `decimals()` (`InvalidTokenDecimals`). `initialize` registers EURC decimals. Submit, batch submit and `convert_invoice_token` fail with `TokenDecimalsNotConfigured` instead of falling back. `migrate` backfills legacy state. |
| #1073 | `add_token` accepted any contract with `transfer`/`balance`; a token whose freeze state cannot be read defeats every downstream safety check. | `add_token` requires the SAC `authorized()` interface (`TokenAuthorizationUnavailable`) and requires that both this contract and the admin are authorized holders (`AccountFrozen`). Covers `AUTH_REQUIRED` assets where the contract must be explicitly authorized first. |
| #1072 | A frozen trustline anywhere in a multi-recipient payout made the whole call trap. One frozen funder blocked the payer's settlement and every other funder's payout. The SAC error code also surfaced under a colliding ILN code (see Before/after below). | Frozen *initiators* (funding LP, paying payer) get `AccountFrozen` before any transfer. Funding that would complete an invoice is rejected if the freelancer is frozen, before the LP pays anything. Frozen *third-party* recipients (funders, fee recipient, refunded payer) have their share **escrowed** as a pending payout, claimable with `claim_pending_payout` once re-authorized. |
| #1071 | Issuer clawback against the contract's balance left it unable to cover refunds/payouts; the failure surfaced as an unrelated ILN error. | Every outbound payout checks the contract's *unreserved* balance (balance minus outstanding escrow) and fails with `InsufficientContractBalance`. Escrowed funds can't be spent by other payouts. |

All failures return a typed error, so Soroban rolls back every storage write
in the invocation. The tests snapshot the invoice, the funder list and all
balances around each failing call and assert that nothing changed.

## New error codes

| Code | Variant | Meaning |
|------|---------|---------|
| 68 | `TokenDecimalsUnavailable` | Token does not implement `decimals()`. |
| 69 | `InvalidTokenDecimals` | Declared decimals > 18 or ≠ the token's `decimals()`. |
| 70 | `TokenDecimalsNotConfigured` | Approved token has no registered decimals (legacy state). |
| 71 | `AccountFrozen` | Trustline/balance is deauthorized or missing. |
| 72 | `TokenAuthorizationUnavailable` | Token has no SAC `authorized()` interface. |
| 73 | `InsufficientContractBalance` | Contract's unreserved balance can't cover a payout (e.g. after clawback). |

## New entry points, storage and events

- `get_pending_payout(recipient, token) -> i128`: read-only.
- `claim_pending_payout(recipient, token) -> i128`: requires `recipient`
  auth. Blocked while paused. Fails with `AccountFrozen` while still frozen,
  or `InsufficientContractBalance` after an uncovered clawback; in both cases
  the escrow entry is left intact.
- Storage (persistent, TTL bumped to 2M ledgers on write):
  - `PendingPayout(recipient, token) -> i128`: one entry per recipient and
    token. Repeated escrows accumulate into it; it never grows per invoice.
  - `PendingPayoutTotal(token) -> i128`: reserved balance.
- Events: `payout_escrowed` (`PayoutEscrowed`) and `pending_payout_claimed`
  (`PendingPayoutClaimed`).

## Before / after

Measured on `dev` @ `4bd3a14` (with the build repairs on this branch)
versus this branch.

| Scenario | Before | After |
|----------|--------|-------|
| `get_token_decimals(EURC)` after `initialize` | `None` (silent fallback to 6) | `Some(6)` |
| `add_token(SAC, 6)`: SAC reports 7 | accepted, min amount scaled 10× too low | `InvalidTokenDecimals` |
| `add_token` on a token with no `decimals()` | accepted | `TokenDecimalsUnavailable` |
| `add_token` on a token with no `authorized()` | accepted | `TokenAuthorizationUnavailable` |
| `mark_paid` with one of two funders frozen | traps; client sees **`NotYetDefaulted` (#11)**, which is the SAC's own error code colliding with ILN's enum; invoice stuck `Funded` | succeeds; healthy funder paid, frozen funder's share escrowed |
| `cancel_invoice` after issuer clawback of escrow | traps; client sees **`NothingToClaim` (#10)** (collision) | `InsufficientContractBalance`; state unchanged |

Resource cost (host CPU instructions, single funder, from
`measure_fund_and_settle_cost`):

| Call | Before | After | Δ |
|------|--------|-------|---|
| `fund_invoice` (completing) | 1,309,908 | 1,640,021 | +25% |
| `mark_paid` (full) | 1,478,456 | 1,710,893 | +16% |

Each outbound payout adds one `balance` and one `authorized` call, and each
inbound transfer adds one `authorized` call. Both calls stay orders of
magnitude below the per-transaction CPU limit. `mark_paid` cost grows
linearly with the number of funders, as before.

---

## Operator runbook

### Before listing a token (`add_token`)

1. Confirm it is a SAC (or implements `decimals()` and `authorized()`).
   Otherwise `add_token` rejects it.
2. Read the token's decimals and pass exactly that value. Classic Stellar
   assets wrapped as a SAC always report **7**.
3. Check the issuer's account flags on Horizon (`/accounts/{issuer}` →
   `flags`):
   - `auth_revocable`: the issuer can freeze any holder, including this
     contract. Freezes are handled (escrow / `AccountFrozen`), but a frozen
     *contract* balance halts all flows in that token.
   - `auth_clawback_enabled`: the issuer can claw back from the contract's
     balance. Treat this as a solvency risk for that token, and consider
     setting a per-token volume cap (`set_token_volume_cap`) during rollout.
   - `auth_required`: the issuer must authorize the contract's balance
     **before** `add_token`, or it fails with `AccountFrozen`.
4. Fund the admin account with ≥ 1,000,000 units for the fee-on-transfer
   probe, and make sure the admin's trustline is authorized.

### A participant is frozen

- No action is needed to keep the protocol running. Settlement, cancels,
  default claims and dispute refunds escrow the frozen party's share and
  emit `payout_escrowed`.
- Track `payout_escrowed` events, and `get_pending_payout` per recipient.
- Once the issuer re-authorizes the account, the recipient calls
  `claim_pending_payout(recipient, token)`.
- A frozen LP or payer can't fund or pay (`AccountFrozen`). If the freelancer
  is frozen, an LP can't make the funding that would complete the invoice;
  partial funding is still allowed.

### This contract is frozen, or clawed back

- Contract frozen: every flow in that token fails atomically, and no state
  is corrupted. Consider `remove_token` to stop new invoices in that token,
  and coordinate with the issuer.
- Clawback: affected payouts fail with `InsufficientContractBalance` and
  leave state unchanged. Recovery is to restore the shortfall (transfer the
  missing amount to the contract) and retry. Escrowed amounts are reserved,
  so they are restored before any other payout can succeed.
- To reconcile, compare `token.balance(contract)` with the LP contributions
  held for partially-funded invoices plus the `PendingPayoutTotal(token)`
  storage entry. The balance should be at least that sum.

---

## Upgrade and migration

- `CURRENT_STORAGE_VERSION` is now **3**.
- Submit, batch submit and convert no longer fall back to 6 decimals. On a
  deployment initialized before this change, EURC (and any other approved
  token with no decimals entry) fails with `TokenDecimalsNotConfigured`
  until migrated.
- **Run `migrate()` immediately after `upgrade()`.** The v3 step backfills
  `TokenDecimals` for the bootstrap tokens with the precision they were
  effectively using (USDC 6, EURC 6, XLM 7). It only writes *missing*
  entries and never overwrites an existing value. It is idempotent and
  version-gated.
- Tokens added through `add_token` before this change already have
  registered decimals and are unaffected. They were not re-validated against
  their on-chain `decimals()`; review them with `get_token_decimals`.
- Integrators: handle error codes 68–73. `claim_pending_payout` and
  `get_pending_payout` are additive. Existing entry-point signatures are
  unchanged.
- Bootstrap tokens passed to `initialize` must expose `authorized()`, which
  every SAC does. With a custom token that does not, funding and payment fail
  closed with `TokenAuthorizationUnavailable`.

### Rollback

Rolling back the WASM is safe for invoice state. Two things to know first:

- Escrowed funds stay in the contract. The old code has no
  `claim_pending_payout`, so drain open escrows (`payout_escrowed` events
  without a matching `pending_payout_claimed`) before rolling back, or
  re-upgrade to release them.
- The v3 decimals backfill is harmless under v2 because it matches v2's
  fallback.

## Known limitations

- Decimals for `initialize` bootstrap tokens are fixed (USDC 6, EURC 6,
  XLM 7) and are not checked against the token, because test and legacy
  deployments pass placeholder addresses there. On mainnet, the USDC and
  EURC SACs report 7. This divergence predates this change, is out of scope
  here, and needs its own follow-up issue.
- Issuer flags (`auth_revocable`, `auth_clawback_enabled`) are not readable
  through the SAC interface. They are an operator check, not an on-chain one.
