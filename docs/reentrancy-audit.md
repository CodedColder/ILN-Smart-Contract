# Cross-Contract Reentrancy Audit

Issue #698. Scope: repeated external calls during **funding**, **insurance
claims** and **distribution settlement**. Tests live in
[`contracts/tests/cross_contract_reentrancy_test.rs`](../contracts/tests/cross_contract_reentrancy_test.rs):

```sh
cargo test -p invoice_liquidity --test cross_contract_reentrancy_test
```

This document separates **verified** behaviour (covered by a test) from
**residual risk** (reviewed, not provable by these tests). It does not claim
a vulnerability: no exploitable reentrancy was found.

## 1. Protection layers

| # | Layer | Where | Reachable through a hostile callee? |
|---|-------|-------|-------------------------------------|
| 1 | Soroban host refuses to invoke a contract that is already on the call stack (default reentry mode). Applies to reads as well as writes. | Host | **Yes** — fires first |
| 2 | `ReentrancyLock` instance-storage flag; `lock_reentrancy` returns `ContractError::Reentrancy` (37) if held. | [`access.rs`](../contracts/invoice_liquidity/src/access.rs) | No, while layer 1 holds. Tested by forcing the lock. |
| 3 | Checks-effects-interactions: invoice status / `amount_funded` are saved before most outgoing transfers. | [`lib.rs`](../contracts/invoice_liquidity/src/lib.rs) | n/a |

Layer 2 is defense-in-depth. Because layer 1 rejects a hostile re-entry before
ILN code runs, hostile-callee tests assert the *outcome* and intentionally do
not assert that error code 37 was produced.

## 2. External call sites

| ILN entry point | Lock | External calls made while it runs | Tolerated on failure? |
|---|---|---|---|
| `fund_invoice` | yes | token `transfer` x2, distribution `accrue_lp` | No — whole call reverts [RB-2] |
| `mark_paid` | yes | token `transfer`s, distribution `accrue_settlement` | No — whole call reverts [RB-2] |
| `claim_default` | yes | token `transfer`s, pool `is_enrolled`, pool `claim` | Pool calls only (`try_*`); refund failure reverts [RB-3] |
| `cancel_invoice`, `resolve_dispute`, `claim_pending_payout` | yes | token `transfer`s | No |

## 3. Verified behaviour

Each row is a test in the file above. "Hostile callee" is a contract installed
as the distribution contract or insurance pool that, from inside the hook,
tries to call `fund_invoice`, `mark_paid`, `claim_default`, `cancel_invoice`
and `claim_pending_payout` on ILN (non-root auth is mocked so authorization can
never be the reason a re-entry fails).

| Case | Test | Observable result |
|---|---|---|
| Normal / repeated: funding hook re-enters with a competing tranche | `hostile_distribution_cannot_reenter_funding` | 0 of 5 attempts succeed per hook; `amount_funded` and funder list reflect only the real tranche; `total_funded` increments once; tokens conserved; extra funding of a `Funded` invoice is rejected unchanged |
| Normal / repeated: settlement hook re-enters | `hostile_distribution_cannot_reenter_settlement` | `Paid` once; `total_paid` +1; payer score +1; tokens conserved; second `mark_paid` -> `AlreadyPaid`, no state change |
| Normal / repeated: insurance `claim` re-enters | `hostile_insurance_pool_cannot_reenter_claim_default` | `Defaulted` once; LP refunded exactly `amount - discount`; payer penalised once; second claim -> `InvoiceDefaulted`, pool not called again |
| Interrupted: pool re-enters then aborts | `aborting_insurance_pool_does_not_block_or_double_apply_default` | Default still completes once (pool call is `try_*`); lock released |
| Unauthorized: non-funder with hostile pool wired | `non_funder_cannot_claim_default_even_with_hostile_pool_wired` | `Unauthorized`; pool never reached; no state change |
| Lock held (layer 2) | `locked_guard_rejects_every_guarded_entry_point_without_side_effects` | All six guarded entry points return `Reentrancy`; no state change; lock stays held; releasing it restores normal operation |
| Stale / failed calls | `failed_calls_never_leave_the_lock_held` | Early error returns that skip `unlock_reentrancy` still leave the lock free, because Soroban reverts the whole invocation including the lock write |

## 4. Failure behaviour

* A failing token transfer or distribution hook aborts the **entire** ILN
  invocation; storage (including `ReentrancyLock`) reverts. A failed call can
  therefore never wedge the protocol.
* A failing or hostile **insurance pool** never blocks a default: its calls use
  `try_*`, the LP principal refund and status change are unaffected, and no
  pool marker is written by ILN.
* A hostile **distribution contract** can block funding or settlement by
  aborting (calls use `invoke_contract`, not `try_*`). This is an admin trust
  assumption (`set_distribution_contract` is admin-only and rate limited), not
  a reentrancy issue.

## 5. Residual risk (reviewed, not proven by these tests)

1. **Lock coverage.** Only the six functions in §2 take the lock. Other
   state-changing entry points (queue, appeal, expiry, transfer) rely on layer
   1 and checks-effects-interactions alone. They were not in scope here.
2. **Partial `mark_paid` ordering.** On a partial payment the invoice is saved
   *after* the payer's token transfer (the full-payment path saves first).
   With layers 1 and 2 this is not exploitable, but it is not strict CEI and
   would matter if layer 1 ever changed.
3. **Post-hook writes in `claim_default`.** Payer score and defaulted counters
   are written after the pool call. Safe for the same reason as (2).
4. **Custom tokens.** A SEP-41 token with transfer hooks is also blocked from
   re-entering ILN by layer 1, but only the SAC and `MockToken` are tested.
5. **Host rule dependence.** Layer 1 is a Soroban host default, not ILN code;
   a future protocol change to reentry mode would leave layer 2 as the only
   barrier, and layer 2 is covered only for the six guarded functions.
