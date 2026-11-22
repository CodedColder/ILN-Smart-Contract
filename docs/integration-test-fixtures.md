# Integration Test Fixtures: Setup and Cleanup Audit

Issue #1161. Scope: [`contracts/tests/full_protocol_lifecycle_test.rs`](../contracts/tests/full_protocol_lifecycle_test.rs),
the five-contract integration suite (ILN, governance, distribution,
reputation bonus, insurance pool). Goal: every scenario starts from a known
state, leaves nothing behind, and gets its balances without any testnet.

```sh
cargo test -p invoice_liquidity --test full_protocol_lifecycle_test
```

## 1. Fixture contract

`Protocol::new()` is the only way a scenario obtains contracts.

| Property | How it is guaranteed |
|---|---|
| Reset between scenarios | A fresh `Env` per `Protocol`. Storage, auth records, events and address generators live in the `Env`; nothing is `static` or shared. |
| Deterministic start | Ledger timestamp/sequence are pinned (`T0`, `SEQ0`) before the first contract call; setup advances are named constants. Two builds give identical state. |
| Testnet-independent balances | Payment token is the in-tree `MockToken`. The only balance sources are the `mint` calls in `Protocol::new` / `enroll_lp`. No network, faucet, env var or wall clock is read (`grep` of `contracts/tests` finds none). |
| Closed supply | `Protocol::holders()` lists every address that can hold the token, so `total_supply()` can be compared with the mint constants at any step. |
| No on-disk residue | `capture_snapshot_at_drop: false`, so no `test_snapshots/*.json` is written on drop. |

## 2. Observable behaviour by case

| Case | Scenario test | Result |
|---|---|---|
| Normal | `test_full_protocol_lifecycle_across_all_five_contracts` | Each step moves exactly the expected tokens (LP pays `FUNDING_COST`, freelancer receives it, payer pays the invoice once, LP receives the full amount, ILN ends at 0); supply constant; accruals, governance and pool state consistent. |
| Pristine start | `fixture_initial_state_is_pinned_and_testnet_independent` | Pinned ledger; only payer and LP funded; every counter and accrual zero; pool empty and LP not enrolled. |
| Isolation / reset | `fixtures_share_no_state_and_reset_deterministically` | A full journey in one fixture does not change a sibling; a fixture built afterwards equals one built before; first invoice id is identical across fixtures. |
| No residue | `fixture_writes_no_snapshot_file` | Dropping a fixture leaves no snapshot file. |
| Repeated / stale | `replayed_steps_after_completion_change_nothing` | `mark_paid` -> `AlreadyPaid`; funding a paid invoice fails; second reward claim returns 0; re-executing a proposal -> `ProposalAlreadyExecuted`; balances, accruals and parameters unchanged. |
| Interrupted | `early_governance_execution_is_rejected_then_succeeds_after_window` | Execution during voting -> `VotingOngoing` with no side effects; completes normally afterwards. |
| Unauthorized | not covered | Auth is mocked (`mock_all_auths_allowing_non_root_auth`) so cross-contract calls can be driven; access control is covered in `docs/access-control.md` suites. |

## 3. Findings

**Verified and fixed**

* **F-1 — The suite rewrote a tracked, stale snapshot on every run.** `Env::default()`
  captures a snapshot on drop, so the test regenerated
  `contracts/invoice_liquidity/test_snapshots/test_full_protocol_lifecycle_across_all_five_contracts.1.json`
  (100 KB, committed). The committed copy had already drifted: it records a
  `set_admin` call the current test never makes. The fixture now disables
  capture and the stale file is removed.
* **F-2 — Ledger was only partly pinned.** The timestamp was set *after*
  `initialize` and the sequence was left at the default. Both are now set
  before the first call, with the setup advances named.
* **F-3 — One scenario, no balance checks.** The suite asserted accruals and
  governance state but never token balances, so a leak or double payment would
  pass. Token-ledger and supply-conservation assertions were added, plus the
  isolation, replay and interrupted scenarios above.
* **F-4 — Deprecated registration API.** `register_contract` replaced by
  `register`; no behaviour change.

**Residual risk (reviewed, not changed here)**

1. Other integration targets (`multi_token_test`, `oracle_integration_test`,
   `governance_main_integration_test`, `governance_cross_contract_auth_test`,
   `audit_regression_test`, `test_context`) still use the default `Env` and so
   still write/rewrite tracked snapshots. They should adopt the same
   `EnvTestConfig`, in a separate change, because their snapshots are shared
   with other tests' history.
2. `MockToken` models SEP-41 balances and one-shot failure, not SAC trustline
   or clawback behaviour; that is covered by `tests_token_safety.rs`.
3. Isolation is guaranteed by `Env` ownership, and checked by the scenario
   above; a future fixture that introduces a `static` or file-backed value
   would not be caught by type checking.
