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

**No timelock delay in v1.** `execute_proposal` is callable by anyone
immediately after the 3-day voting window closes, provided quorum and majority
criteria are met. The admin veto power serves as the emergency brake during the
early phase.

The `TimelockNotExpired` error code is reserved in the error enum for a future
timelock implementation.

A timelock **must be introduced before the admin veto power is disabled**
(which must happen before mainnet launch per the governance documentation).

## Alternatives Considered

| Alternative | Why rejected |
|-------------|--------------|
| **24-hour timelock** | Adds friction without meaningful security benefit while the admin veto exists; a 24-hour window is too short for large token holders to coordinate a response anyway. |
| **48-hour timelock** | Same reasoning as 24-hour; the admin veto already covers the early phase. |
| **7-day timelock (Compound-style)** | Appropriate for a mature, decentralised protocol, but excessive for a protocol in active parameter tuning. Would slow down legitimate fee adjustments. |
| **Configurable timelock (governance sets the delay)** | Correct long-term design, but adds implementation complexity at v1. Deferred to a future upgrade. |
| **Optimistic execution with challenge period** | Complex to implement correctly; requires a separate challenge mechanism and bond. Out of scope for v1. |

## Consequences

**Positive:**
- Proposals can be executed immediately after voting closes, enabling rapid
  response to protocol conditions.
- Implementation is simpler — no timelock storage, no expiry checks on
  execution.
- The admin veto provides equivalent protection during the early phase when
  token distribution is concentrated.

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

## Addendum (Issues #1007 / #1008): observable rejected transitions

The "no timelock, veto as brake" decision above is unchanged. Two audit
findings on the execution path were fixed without changing that decision:

1. **Rejected transitions were not observable.** `execute_proposal` wrote
   `Rejected`, forfeited the deposit and then returned
   `Err(QuorumNotReached | ProposalRejected)`; on Soroban an `Err` rolls back
   the whole call, so on-chain the proposal stayed `Active`, the deposit was
   never forfeited and no event existed. (The old unit tests hid this by
   calling the function through `as_contract`, which does not roll back.) It
   now returns `Ok`, commits `Rejected`, and emits `proposal_rejected` with a
   `RejectionReason`. The same applies to a failed cross-contract execution:
   `proposal_execution_failed` is now committed and the proposal stays
   `Passed` (retryable).
2. **Cross-contract authorization is now tested against the real ILN
   contract** (`contracts/tests/governance_cross_contract_auth_test.rs`) with
   mocked auths dropped: governance only controls ILN when ILN's `Admin` *is*
   the governance contract address; if it is not, execution is rejected by
   ILN's `require_auth`, ILN is untouched and the failure event is emitted.

Guards that reject without any state change (`NotVetoSigner`, `NotVetoable`,
`VetoAlreadyApproved`, `VetoPowerDisabled`, `AlreadyResolved`, ...) still
return `Err`: there is nothing to persist, and emitting events from a reverting
call is impossible on Soroban. Callers see them as transaction errors.

**Migration.** Integrators that treated `Err(QuorumNotReached)` /
`Err(ProposalRejected)` / `Err(ExecutionFailed)` from `execute_proposal` as the
rejection signal must switch to the `proposal_rejected` /
`proposal_execution_failed` events (or read `get_proposal(id).status`). The
error variants remain in the enum so error codes do not shift.
