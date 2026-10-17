# ADR-005: Governance Timelock Length (No Timelock in v1)

**Date:** 2024-05-01  
**Status:** Accepted

## Context

After a governance proposal passes (quorum met, majority in favour), there is a
window between the vote closing and the proposal being executed on-chain. A
**timelock** is a mandatory delay in that window that gives the community time
to react — exit positions, raise objections, or prepare for the change — before
it takes effect.

The ILN governance contract (`iln_governance`) executes proposals via
`execute_proposal`, which cross-contract-calls the ILN contract to apply the
change atomically. The team had to decide how long the timelock delay should be.

Competing concerns:

- **Security** — a longer delay gives more time to detect and respond to a
  malicious or erroneous proposal.
- **Agility** — a shorter delay lets the protocol respond quickly to market
  conditions (e.g. adjusting fee rates during a crisis).
- **Admin veto as substitute** — the governance contract already has an
  emergency veto (`veto_proposal`) that can block any `Active` or `Passed`
  proposal before execution. This provides a safety net that partially
  substitutes for a timelock during the early protocol phase. **Update
  (Issue #642):** veto authority is now multisig-gated — `veto_proposal`
  requires a configurable threshold of `VetoSigners` to approve (via
  `configure_veto_multisig`) rather than a single admin key, closing the
  "admin single point of failure" finding from the threat model. The rest of
  this ADR's reasoning (veto as timelock substitute, disable-before-mainnet
  plan) is otherwise unchanged.
- **Protocol maturity** — at launch, the governance token distribution is
  concentrated; a long timelock would slow down necessary parameter tuning
  without providing meaningful decentralisation benefits.

## Decision

**Phase 3 Timelock Implementation and Hardening (Issues #985, #987, #989, #990).**

While v1 initially launched without an execution delay, ADR-012 Phase 3 established a governance execution timelock with the following critical security controls:

1. **Minimum Production Delay Bounds (Issue #985):**
   `set_execution_delay(delay)` enforces bounds to prevent governance deadlocks (`MAX_EXECUTION_DELAY_LEDGERS = 518_400` ~ 30 days) and guarantees that once admin veto power is disabled (`!is_veto_power_enabled()`), execution delay cannot be reduced below `MIN_PRODUCTION_EXECUTION_DELAY_LEDGERS` (17,280 ledgers ~ 24 hours), resolving the decentralized transition risk.
2. **Rollover-Safe Eta Comparisons (Issue #987):**
   Proposal scheduling and execution timelock checks utilize RFC 1982 serial number arithmetic (`is_eta_reached`) and wrapping addition (`wrapping_add`), preventing execution stalls or bypasses across ledger sequence boundaries or rollover at `u32::MAX`.
3. **Veto Transition State Machine (Issue #989):**
   The multisig veto mechanism implements an explicit finite state machine (`VetoStatus`: `Uninitiated -> Pending -> Executed`), emitting `VetoStateTransition` events and exposing `get_veto_state(proposal_id)` for transparent on-chain auditing.
4. **Veto-Disable Authorization and Reason Recording (Issue #990):**
   Permanent disablement of admin veto power (`disable_veto_power` / `disable_veto_power_with_reason`) records caller authorization, ledger sequence, and reason hash in `VetoDisableRecord`, prevents redundant disable calls (`VetoPowerAlreadyDisabled`), and emits structured `VetoDisableRecorded` events.

5. **Legacy Zero-Delay Migration (Issue #992):**
   A stored delay of `0` (never set) and `Passed` proposals with `eta_ledger == 0` predate the timelock. Once veto power is retired the effective delay is clamped to `MIN_PRODUCTION_EXECUTION_DELAY_LEDGERS` (`get_effective_execution_delay`), `migrate_legacy_execution_delay` (admin, idempotent) persists the floor with an audit record, and a legacy `Passed` proposal must be re-stamped with `rebase_legacy_proposal_eta` (permissionless, can only postpone) before it can execute. A computed eta that wraps onto `0` is moved to `1` so it can never be mistaken for the legacy sentinel.
6. **Proposer Identity Binding (Issue #998):**
   `create_proposal` stores the proposer under `ProposalProposer(id)`. `execute_proposal` (queue and execute) and `veto_proposal` refuse with `ProposerIdentityMismatch` if the stored proposal disagrees; pre-binding proposals are backfilled once.
7. **Checkpoints Bound to the Snapshot Ledger (Issue #1001):**
   A vote uses the newest checkpoint recorded at least `MIN_VOTE_HOLD_LEDGERS` before the proposal's snapshot ledger. `checkpoint_balance` demotes the replaced checkpoint into one prior slot (two slots per voter, no unbounded growth), so refreshing after a snapshot no longer disenfranchises the voter. A proposal without a snapshot ledger fails with `SnapshotLedgerMissing` instead of silently using the current ledger.

Threat model: [`docs/timelock-threat-model.md`](../timelock-threat-model.md).

## Alternatives Considered

| Alternative | Why rejected |
|-------------|--------------|
| **24-hour timelock** | Adds friction without meaningful security benefit while the admin veto exists; a 24-hour window is too short for large token holders to coordinate a response anyway. |
| **48-hour timelock** | Same reasoning as 24-hour; the admin veto already covers the early phase. |
| **7-day timelock (Compound-style)** | Appropriate for a mature, decentralised protocol, but excessive for a protocol in active parameter tuning. Would slow down legitimate fee adjustments. |
| **Configurable timelock (governance sets the delay)** | Implemented in Phase 3 with strict production delay bounds and bounds enforcement upon veto retirement (Issue #985). |
| **Optimistic execution with challenge period** | Complex to implement correctly; requires a separate challenge mechanism and bond. Out of scope for v1. |

## Consequences

**Positive:**
- Proposals are safely timelocked with bounded delays (`17_280..=518_400` ledgers), ensuring community reaction windows.
- Eta comparisons remain sound and verified even across ledger rollover boundaries.
- Veto approvals follow a deterministic finite state machine with queryable state and event traces.
- Veto retirement permanently audits the authorization and justification on-chain.

**Negative / Trade-offs:**
- Without a timelock, a proposal that passes cannot be stopped by the community
  once the admin veto is disabled. This is a significant risk for a mature,
  decentralised protocol.
- The absence of a timelock is a known deviation from DeFi governance best
  practices (MakerDAO, Compound, Uniswap all use timelocks of 2–7 days).
- **Action required:** A timelock must be implemented and activated via a
  governance upgrade before `disable_veto_power()` is called. Failure to do
  so leaves the protocol without any execution delay after full decentralisation.
- The `TimelockNotExpired` error is reserved but unimplemented; contributors
  must not assume it is enforced.

## Addendum (#995, #999, #1002, #1005): implemented timelock invariants

The "reserved but unimplemented" wording above is superseded for the current
`iln_governance` contract. `execute_proposal` is a two-step flow and
`TimelockNotExpired` is enforced:

1. Call 1 (`Active -> Passed`) sets `eta_ledger = ledger.sequence + ExecutionDelay`
   (`checked_add`; overflow returns `InvalidTimelock`). `ExecutionDelay`
   defaults to `0`, so default behaviour is unchanged.
2. Call 2 (`Passed -> Executed`) goes through `assert_executable`, which fails
   closed unless status is `Passed`, the voting window has closed **and**
   `ledger.sequence >= eta_ledger`.

Invariants (all covered by `contracts/iln_governance/src/test.rs`):

| # | Invariant |
|---|-----------|
| I1 | A proposal is never `Executed` at a ledger below its `eta_ledger`. |
| I2 | `eta_ledger` is frozen when the proposal becomes `Passed`; later `set_execution_delay` calls cannot shorten or extend it. |
| I3 | Status only moves forward: `Active -> {Passed, Rejected, Vetoed}`, `Passed -> {Executed, Vetoed}`. `Rejected`, `Executed`, `Vetoed` are terminal. Enforced by the single writer `transition_status`. |
| I4 | A vote receipt outlives the voting window (compile-time assertion), tallies are immutable once the window closes, and each deposit is settled exactly once (refund xor forfeit). |
| I5 | `set_execution_delay` requires the stored admin's auth, fails with `NotInitialized` before `initialize()`, and rejects delays above 518,400 ledgers (~30 days). |

Operational note: the delay is an admin-key parameter, not a governance-gated
one. Moving it behind the ILN-contract authority is the remaining step before
`disable_veto_power()` (see ADR-012).
