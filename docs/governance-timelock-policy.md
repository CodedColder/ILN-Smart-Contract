# Governance Timelock Policy (Issues #981, #982)

Scope: `contracts/iln_governance/src/lib.rs` — `set_execution_delay`,
`execute_proposal`, `create_proposal`, `disable_veto_power`.
Decision record: [ADR-005](adr/ADR-005-governance-timelock.md) (amended).

## 1. Problem (audit baseline)

| # | Before | Risk |
|---|--------|------|
| 981 | `disable_veto_power()` only checked `iln_contract.require_auth()`. With the default `execution_delay = 0` it could retire the veto while no timelock existed, and `set_execution_delay` could later drop the delay back to `0`. | ADR-005's "timelock before veto disablement" requirement was process-only. A passed proposal could execute in the same ledger it was queued with no brake at all. |
| 981 | `set_execution_delay` stored the caller as `Admin` if none was set. | Pre-`initialize` first-caller admin capture on the timelock control. |
| 982 | `eta_ledger` was computed from whatever `execution_delay` was in force at the moment the proposal was queued; nothing recorded which policy applied. | Lowering the delay while a proposal was voting (or after it was queued, with veto retirement) shortened its execution window; reviewers could not tell which policy a proposal ran under. |

## 2. Behavior after the change

### 2.1 Active-timelock gate (#981)

- `MIN_ACTIVE_TIMELOCK_LEDGERS = 17_280` (~24 h at ~5 s/ledger), exported as a
  public constant.
- `disable_veto_power()` → `TimelockNotActive` (error 29) unless
  `execution_delay >= MIN_ACTIVE_TIMELOCK_LEDGERS`. On failure no state changes.
- After the veto is disabled, `set_execution_delay(d)` with
  `d < MIN_ACTIVE_TIMELOCK_LEDGERS` → `TimelockNotActive`. Raising is allowed.
- While the veto is enabled, the delay may still be set freely (including 0) —
  unchanged early-phase behavior.
- `set_execution_delay` returns `NotInitialized` before `initialize` (no more
  first-caller admin capture).

Invariant: at every reachable state,
`VetoPowerEnabled || execution_delay >= MIN_ACTIVE_TIMELOCK_LEDGERS`.

### 2.2 Timelock policy versioning (#982)

- `TimelockPolicyVersion` (instance storage) is `1` after `initialize` and is
  incremented by an effective (value-changing) `set_execution_delay` call and
  by `disable_veto_power`. Each bump emits
  `timelock_policy_updated { version, execution_delay, veto_enabled }`.
- `create_proposal` writes `ProposalTimelockPolicy(id)` (persistent, one
  record per proposal — growth is bounded 1:1 with `Proposal(id)`):
  `created_version`, `created_delay`, `created_veto_enabled`.
- Queueing (`Active → Passed`) applies
  `applied_delay = max(created_delay, current_delay)`, sets
  `eta_ledger = current_ledger + applied_delay`, and records `queued_version`,
  `queued_ledger`, `applied_delay`.
- Executing (`Passed → Executed`) requires `current_ledger >= effective_eta`:
  - same policy version as at queue time → `effective_eta = eta_ledger`;
  - policy changed since queueing → `max(eta_ledger, queued_ledger + current_delay)`.
- Read APIs: `get_timelock_policy_version()`,
  `get_proposal_timelock_policy(id) -> Option<ProposalTimelockPolicy>`.

Invariant: a timelock policy change can lengthen but never shorten an
in-flight proposal's timelock. Retiring the veto re-applies the active
timelock to proposals that were queued under a zero delay.

The snapshot is a separate storage key rather than new fields on
`GovernanceProposal`, so proposals persisted by an earlier build still decode.

## 3. Before / after measurement

`cargo test -p iln_governance` on `main` did not compile (pre-existing `?` in
the bool-returning deposit settle helpers); with only that compile fix applied
the baseline is the "before" column.

| Metric | Before | After |
|--------|--------|-------|
| `cargo test -p iln_governance` | 113 passed, 1 failed | 132 passed, 1 failed |
| Failing test | `test_max_delegation_depth_cap_exceeded_panics` (pre-existing, unrelated) | same, unchanged |
| `disable_veto_power` with delay 0 | succeeds | `TimelockNotActive` |
| Delay 100 at creation, lowered to 0 before queueing | executes at queue ledger | executes at queue ledger + 100 |
| Queued with delay 0, then veto retired | executable immediately | executable at queue ledger + 17,280 |
| Extra storage per proposal | — | one `ProposalTimelockPolicy` entry |

## 4. Rollout

1. Upgrade the governance contract WASM.
2. Before any proposal to disable the veto is voted, set
   `execution_delay >= MIN_ACTIVE_TIMELOCK_LEDGERS` via `set_execution_delay`.
   A `disable_veto_power` call without it now fails and the proposal stays
   `Passed` (retryable once the delay is raised).
3. Monitor `timelock_policy_updated` events; the indexer can surface
   `get_proposal_timelock_policy` for proposal pages.

## 5. Upgrade / migration impact

- Contracts upgraded from a pre-#982 build have no stored policy version, so
  `get_timelock_policy_version()` reads `0` until the first policy change.
- Proposals created before the upgrade and still `Active` have no snapshot;
  they are queued under the current policy (pre-#982 behavior) and receive a
  snapshot at queue time.
- Proposals already `Passed` before the upgrade have no recorded queue ledger
  and fail closed to `eta_ledger + current_delay`. With the default delay of 0
  this is identical to the old behavior; with a non-zero delay they wait one
  extra delay period.
- No changes to `GovernanceProposal`'s layout, existing events, or error codes
  1–28.

## 6. Recovery

- `disable_veto_power` blocked by `TimelockNotActive`: raise the delay with
  `set_execution_delay(admin, ≥17_280)` and retry `execute_proposal`.
- Delay set too high after veto retirement: it can be lowered with
  `set_execution_delay`, but not below the floor; proposals already queued
  under the higher value keep their recorded eta.
- The veto disablement remains a one-way switch; there is no on-chain path
  to re-enable it (see ADR-012 for the multisig handoff).

## 7. Tests

`contracts/iln_governance/src/test.rs`:

- #981: `test_disable_veto_power_rejected_without_timelock`,
  `test_disable_veto_power_rejected_one_ledger_below_floor`,
  `test_disable_veto_power_succeeds_exactly_at_floor`,
  `test_disable_veto_power_requires_iln_contract_auth`,
  `test_execution_delay_cannot_drop_below_floor_after_veto_disabled`,
  `test_execution_delay_can_be_lowered_while_veto_enabled`,
  `test_set_execution_delay_rejects_non_admin`,
  `test_set_execution_delay_before_initialize_fails_closed`.
- #982: `test_policy_version_starts_at_one_and_bumps_on_change`,
  `test_rejected_policy_change_does_not_bump_version`,
  `test_proposal_persists_creation_policy`, `test_queue_records_policy_and_eta`,
  `test_delay_reduction_during_voting_does_not_shorten_timelock`,
  `test_delay_reduction_after_queue_does_not_shorten_timelock`,
  `test_veto_disablement_extends_already_queued_proposal`,
  `test_delay_increase_after_queue_extends_timelock`,
  `test_legacy_proposal_without_snapshot_is_queued_under_current_policy`,
  `test_legacy_queued_proposal_fails_closed_to_eta_plus_delay`,
  `test_policy_update_emits_event`.
