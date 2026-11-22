# Failed-transfer rollback on funded payout paths (Issue #1178)

This document records the verified behaviour of the contract when a Soroban
token transfer that forms part of a **funded payout** fails (the token
contract panics/traps, or a token-side error propagates). The property under
review is simple and economic:

> A failed token transfer must leave the invoice, the funder list, the
> per-token volume counter and every token balance exactly as they were
> before the invocation.

Nothing here asserts that a vulnerability exists. It records what the code
and the Soroban host actually do, separating **verified findings** from
**residual risk**, and is backed by the regression suite in
`contracts/invoice_liquidity/src/tests_failed_payout_rollback.rs`.

## 1. Why a failed transfer is possible at all

Every payout this contract makes is an ordinary SEP-41/SAC `transfer` call to
a token contract it does not control. That call can fail in ways the ILN
contract cannot predict:

- a token whose transfer logic traps (`panic!`) or returns a host error;
- a token-side assertion (e.g. an internal invariant) failing;
- the host enforcing an invocation resource limit mid-call.

The ILN contract is not permitted to (and does not) assume the token call
succeeds. The question is what the surrounding state looks like when it does
not.

## 2. The mechanism that provides the guarantee

The guarantee does **not** come from hand-written compensating code. It comes
from two facts:

1. **Soroban invocation atomicity.** A `transfer` that traps aborts the
   *entire* top-level invocation, rolling back every storage write and event
   emitted during it — including the caller's own writes and the token
   contract's writes. There is no partial commit.
2. **Checks-Effects-Interactions (CEI) ordering.** The payout entry points
   deliberately write their status/accounting changes *before* the external
   transfer loop (see the `// CEI:` comments in `lib.rs`). Combined with
   atomicity this means the pre-transfer writes are rolled back together with
   the failed transfer, rather than left dangling.

Source of truth for the payout primitives: `token_safety.rs`
(`transfer_out_strict`, `pay_or_escrow`, `claim_pending`). Every funded
payout path in `lib.rs` funnels through one of those three.

## 3. Funded payout paths and their transfer ordering

Transfer ordinals below are **per invocation** and assume the "armed failure"
mock counts a `transfer` call. Inbound funding transfers count too, which is
why some rows start at `#2`.

| Entry point | Payout being made | Transfer ordinal | Primitive used | State written before the transfer |
|---|---|---|---|---|
| `fund_invoice` (full funding) | freelancer advance | `#2` (`#1` = LP → contract) | `transfer_out_strict` | none yet — `save_invoice` is *after* the payout |
| `mark_paid` (full payment, no fee) | funder distribution | `#2` (`#1` = payer → contract) | `pay_or_escrow` | `amount_paid += amount`; `status = Paid` + `save_invoice` before the funder loop |
| `mark_paid` (full payment, fee > 0) | protocol fee, then funder share | `#2` = admin fee, `#3` = funder share | `pay_or_escrow` | same as above |
| `cancel_invoice` (PartiallyFunded) | funder refund | `#2` | `pay_or_escrow` | `status = Cancelled` + `save_invoice` before the refund loop |
| `claim_default` | funder principal refund | `#3` (funding used `#1`–`#2`) | `pay_or_escrow` | `status = Defaulted` + `save_invoice` + NFT sync before the refund loop |
| `resolve_dispute` (resolution 1, upheld) | LP refund, then payer refund | `#3` (funding used `#1`–`#2`) | `pay_or_escrow` | `status = Cancelled` + `save_invoice` before the refunds |
| `claim_pending_payout` | escrowed payout release | `#2` (escrow was created without a transfer) | `claim_pending` | `set_pending(recipient, 0, …)` **before** the transfer |

Notes:

- The fee-only case (`protocol_fee > 0`) is exercised by `update_fee_rate`.
  With the default fee rate of `0`, `mark_paid` makes no fee transfer and the
  funder share is the first outbound transfer.
- `cancel_invoice`, `claim_default` and `resolve_dispute` all write their new
  terminal status **before** iterating the refund loop. A failed refund must
  therefore un-write that status.

## 4. Assumptions and data sources

- **Authorization.** Payout entry points require the same auth as their happy
  path: `fund_invoice` needs the funder (LP) auth; `mark_paid` needs the payer
  auth; `cancel_invoice` needs the submitter (freelancer) auth;
  `claim_default` needs the funder auth; `resolve_dispute` is admin-only;
  `claim_pending_payout` needs `recipient.require_auth()`. Authorization is
  checked *before* any transfer, so a failed transfer is never also an
  authorization bypass.
- **Balance pre-check.** `ensure_spendable` (in `token_safety.rs`) rejects a
  payout with the typed `InsufficientContractBalance` *before* calling the
  token, using the contract's balance minus currently-escrowed totals as the
  reserved amount. This is a typed error path, distinct from a token trap.
- **Escrow path.** When a *third-party* recipient is frozen, `pay_or_escrow`
  escrows instead of transferring (no token call), so a frozen recipient
  cannot turn into a failed-transfer rollback. A frozen *initiator* is
  rejected earlier with `AccountFrozen`.
- **Data sources.** Invoice state via `get_invoice`; funder list via
  `get_invoice_funders`; per-token cumulative volume via `get_token_volume`;
  balances via the token client. The regression suite snapshots each of these
  immediately before the failing call and compares immediately after.

## 5. Verified findings

These are observed, reproduced behaviours (see the test names in
`tests_failed_payout_rollback.rs`), not hypotheses:

- **VF-1** — A failed freelancer payout during full funding leaves the invoice
  `Pending` with `amount_funded == 0`, an empty funder list, unchanged
  per-token volume, and unchanged LP/contract token balances. The LP's inbound
  transfer is rolled back with the failed outbound one.
- **VF-2** — A failed funder distribution during settlement leaves the invoice
  `Funded` with `amount_paid == 0`, and all balances unchanged.
- **VF-3** — A settlement that makes the protocol-fee transfer successfully and
  *then* fails the funder-share transfer rolls back the fee transfer too: the
  admin balance is unchanged. This exercises the multi-transfer rollback, not
  just a single failure.
- **VF-4** — A failed partial-funding refund in `cancel_invoice` leaves the
  invoice `PartiallyFunded` (not `Cancelled`).
- **VF-5** — A failed refund in `claim_default` leaves the invoice `Funded`
  (not `Defaulted`).
- **VF-6** — A failed LP refund in `resolve_dispute` (upheld) leaves the
  invoice `Disputed` (not `Cancelled`).
- **VF-7** — A failed release in `claim_pending_payout` leaves the escrow
  entry intact (`get_pending_payout` is unchanged). `claim_pending` clears the
  escrow *before* transferring, so this directly proves the pre-transfer write
  is rolled back.

## 6. Residual risk

- **RR-1 — non-trapping token with silent misbehaviour.** The guarantee covers
  a token that traps or errors. A token that *returns success* while moving no
  funds (or the wrong amount) is outside what the host can detect; the ILN
  contract trusts the return of `transfer`. This is the same trust model
  documented in `docs/token-safety.md` for fee-on-transfer tokens.
- **RR-2 — `try_*` graceful paths intentionally do not roll back.**
  `claim_default`'s optional insurance-pool top-up and similar best-effort
  cross-contract calls use `try_*` and degrade gracefully; a failure there is
  *by design* not atomic with the principal refund. Those are not "funded
  payout" paths and are called out here so the atomicity claim is not
  over-read.
- **RR-3 — escrow is not loss-free, only non-blocking.** If a recipient stays
  frozen forever the escrowed payout is never released; that is a liveness
  property of the escrow design, not a rollback concern.
- **RR-4 — rollback is only as strong as the host.** The guarantee is the
  Soroban host's atomicity guarantee. It is asserted here at the SDK test-host
  level; it is the same guarantee relied upon by `tests_resource_limit_failure.rs`
  (Issue #1067).

## 7. Operator guidance

- A trapped payout leaves **no** observable state change. Re-submitting the
  same operation after the underlying token problem is resolved is safe: there
  is nothing half-applied to reconcile.
- Because the invoice status is also rolled back, indexers should not emit a
  terminal transition (`Cancelled`/`Defaulted`/`Paid`) on the strength of a
  submitted transaction alone — they must be driven by the emitted event, which
  is present only on success.
- Pending-payout escrow entries (`get_pending_payout`) survive a failed claim,
  so an operator can safely retry `claim_pending_payout` once the recipient's
  trustline is authorized again and the token call succeeds.
