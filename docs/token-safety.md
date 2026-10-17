# Token Configuration and SAC Safety

Issues #1069, #1070, #1071, #1072, #1073. Covers how `invoice_liquidity`
admits tokens, and how it behaves when a Stellar Asset Contract (SAC)
issuer freezes an account or claws back funds.

Source: [`contracts/invoice_liquidity/src/token_safety.rs`](../contracts/invoice_liquidity/src/token_safety.rs).
Tests: [`contracts/invoice_liquidity/src/tests_token_safety.rs`](../contracts/invoice_liquidity/src/tests_token_safety.rs),
[`contracts/invoice_liquidity/src/tests_token_decimals_audit.rs`](../contracts/invoice_liquidity/src/tests_token_decimals_audit.rs) (#1069).

---

## Summary of controls

| Issue | Risk (audit path) | Control |
|-------|-------------------|---------|
| #1070 | EURC approved in `initialize` with no `TokenDecimals` entry; `add_token` trusted the admin-supplied `decimals`; submit validation silently fell back to 6 (`lib.rs` initialize, `add_token`, `validate_invoice_terms_with_token`). A wrong precision mis-scales the minimum invoice amount by 10× per digit. | `add_token` rejects tokens that do not answer `decimals()` (`TokenDecimalsUnavailable`), values above 18 or that differ from the token's own `decimals()` (`InvalidTokenDecimals`). `initialize` registers EURC decimals. Submit, batch submit and `convert_invoice_token` fail with `TokenDecimalsNotConfigured` instead of falling back. `migrate` backfills legacy state. |
| #1073 | `add_token` accepted any contract with `transfer`/`balance`; a token whose freeze state cannot be read defeats every downstream safety check. | `add_token` requires the SAC `authorized()` interface (`TokenAuthorizationUnavailable`) and requires that both this contract and the admin are authorized holders (`AccountFrozen`). Covers `AUTH_REQUIRED` assets where the contract must be explicitly authorized first. |
| #1072 | A frozen trustline anywhere in a multi-recipient payout made the whole call trap. One frozen funder blocked the payer's settlement and every other funder's payout. The SAC error code also surfaced under a colliding ILN code (see Before/after below). | Frozen *initiators* (funding LP, paying payer) get `AccountFrozen` before any transfer. Funding that would complete an invoice is rejected if the freelancer is frozen, before the LP pays anything. Frozen *third-party* recipients (funders, fee recipient, refunded payer) have their share **escrowed** as a pending payout, claimable with `claim_pending_payout` once re-authorized. |
| #1071 | Issuer clawback against the contract's balance left it unable to cover refunds/payouts; the failure surfaced as an unrelated ILN error. | Every outbound payout checks the contract's *unreserved* balance (balance minus outstanding escrow) and fails with `InsufficientContractBalance`. Escrowed funds can't be spent by other payouts. |
| #1069 | `initialize`'s three bootstrap tokens are registered with a fixed, unvalidated decimals value (USDC 6, EURC 6, XLM 7) — unlike `add_token`, which always validates. On Stellar Mainnet, classic-asset SACs (including real USDC/EURC) report 7 decimals, so a real deployment is very likely already registering the wrong value for USDC/EURC specifically, silently under-scaling the minimum invoice amount. | `audit_token_decimals` (read-only) reports the registered value next to what the token reports *right now*, so the divergence is observable instead of assumed away. `correct_token_decimals` (admin-only) re-validates a new value against the token's own `decimals()` — exactly like `add_token` — before overwriting the registered one. `initialize` itself is unchanged; this is opt-in, not automatic. |

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

No new error code was needed for #1069: `correct_token_decimals` reuses
`Unauthorized` (not admin, or token not approved), `InvalidTokenDecimals`
and `TokenDecimalsUnavailable` (both via the same `validate_token_decimals`
`add_token` already uses).

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
- **Issue #1069:**
  - `audit_token_decimals(token) -> TokenDecimalsAudit { registered: Option<u32>, on_chain: Option<u32> }`:
    read-only, Access: Anyone. `registered` is `None` if `token` was never
    approved; `on_chain` is `None` if the token can't currently answer
    `decimals()` (including a non-contract address). Neither side being
    `None` is itself evidence of a problem — only `Some(a) != Some(b)` is.
  - `correct_token_decimals(token, decimals) -> Result<(), ContractError>`:
    Admin only. Re-validates `decimals` against `token`'s own `decimals()`
    (same check as `add_token`) before overwriting the registered value for
    an *already-approved* token. Does not approve a new token or move any
    funds.
  - Event: `token_decimals_corrected` (`TokenDecimalsCorrected { token, old_decimals, new_decimals, updated_by }`).

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

### After `initialize`: verify the bootstrap tokens' decimals (Issue #1069)

`initialize` registers USDC and EURC at 6 decimals and XLM at 7 without
checking either against the token — see "Known limitations" above for why
that's deliberate, not an oversight left unfixed.

1. Right after `initialize`, call `audit_token_decimals(usdc_token)`,
   `audit_token_decimals(eurc_token)` and `audit_token_decimals(xlm_token)`.
2. For each, compare `registered` and `on_chain`. Classic Stellar assets
   wrapped as a SAC (as real mainnet USDC/EURC are) always report **7** —
   so expect `audit.matches() == false` for USDC/EURC on a real deployment,
   and `== true` for XLM.
3. If the two genuinely differ, decide deliberately whether to correct it:
   `correct_token_decimals(token, on_chain_value)` (admin only) re-validates
   the new value against the token's own `decimals()` and overwrites the
   registered one. This changes the minimum invoice amount applied to
   invoices submitted *after* the call, in that token only — it does not
   touch any already-submitted invoice's stored `amount`.
4. Re-run `audit_token_decimals` to confirm `matches() == true` before
   advertising the token to LPs/payers.

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

- **Resolved by Issue #1069** (was: "decimals for `initialize` bootstrap
  tokens are fixed and not checked against the token... needs its own
  follow-up issue"). `initialize` itself is deliberately still unchanged —
  it still registers USDC/EURC at 6 and XLM at 7 without checking, because
  test and legacy deployments pass placeholder (non-contract) addresses for
  `eurc_token`, and because silently switching to the token's real
  `decimals()` at `initialize` time would retroactively change the minimum
  invoice amount for every existing deployment and test, not just fix a
  bug. Instead, `audit_token_decimals` makes the divergence observable, and
  `correct_token_decimals` lets an admin who has confirmed the real value
  apply it deliberately, fully re-validated. See the operator runbook
  entry below.
- Issuer flags (`auth_revocable`, `auth_clawback_enabled`) are not readable
  through the SAC interface. They are an operator check, not an on-chain one.
