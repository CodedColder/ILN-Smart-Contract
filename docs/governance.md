# ILN Governance

This document reflects the governance system as actually implemented in
`contracts/iln_governance/src/lib.rs`.  It covers proposal creation, voting
mechanics, quorum rules, execution, admin veto, and security considerations.

---

## Table of contents

1. [Overview](#1-overview)
2. [Governance token and voting power](#2-governance-token-and-voting-power)
3. [Governable parameters](#3-governable-parameters)
4. [Proposal lifecycle](#4-proposal-lifecycle)
5. [Worked example — end-to-end proposal](#5-worked-example--end-to-end-proposal)
6. [Quorum and majority rules](#6-quorum-and-majority-rules)
7. [Execution mechanics](#7-execution-mechanics)
8. [Admin veto power](#8-admin-veto-power)
9. [Security considerations](#9-security-considerations)
10. [Past governance decisions](#10-past-governance-decisions)

---

## 1. Overview

ILN governance lets token holders propose and vote on protocol changes on-chain.
The `GovContract` (`contracts/iln_governance`) orchestrates voting; when a
proposal passes it cross-contract-calls the `InvoiceLiquidityContract` to apply
the change atomically.

There is currently **no timelock delay** between a proposal passing and its
execution.  `execute_proposal` is callable by anyone once the voting period ends
and quorum/majority criteria are met.

During the early protocol phase an **admin veto power** exists as an emergency
brake.  This power is time-limited and can be permanently disabled by a
governance vote before mainnet launch (see [Section 8](#8-admin-veto-power)).

---

## 2. Governance token and voting power

Voting power is read from per-proposal checkpoints, not the live balance at vote
call time. When a proposal is created, the proposer's current governance-token
balance is recorded as the initial checkpoint. The contract also maintains a
proposal-scoped checkpoint for each voter; if no checkpoint exists yet, the
first vote records that voter's current balance and reuses it for the duration
of the proposal.

| Property | Value |
|----------|-------|
| Token | Address supplied to `initialize(gov_token)` |
| Unit of power | 1 token = 1 vote (raw balance in stroops) |
| Snapshot | Per-proposal voter checkpoint stored in contract storage |
| Minimum power | Must be > 0 (0-balance callers are rejected with `GovernanceError::NoVotingPower`) |

---

## 3. Governable parameters

All on-chain actions are defined by the `ProposalAction` enum.

| Variant | Target function | Description |
|---------|----------------|-------------|
| `UpdateFeeRate(u32)` | `update_fee_rate` | Set protocol fee in basis points (0–10 000) |
| `UpdateMaxDiscountRate(u32)` | `update_max_discount` | Cap on invoice discount rates in basis points |
| `AddToken(Address)` | `add_token` | Whitelist a new payment token |
| `RemoveToken(Address)` | `remove_token` | Delist an existing payment token |
| `RegisterOracle(OracleFeedType, Address)` | `register_oracle` | Register (or update) the default oracle for a feed type |
| `RemoveOracle(OracleFeedType)` | `remove_oracle` | Remove the default oracle for a feed type |

> **Before proposing a `RegisterOracle` action:** see
> [Oracle Provider Vetting](oracle-provider-vetting.md) for the disclosure
> criteria (track record, data-source decentralization, freshness SLA,
> incident history) a proposal must include, and the required proposal
> template.

Admin-only functions that are **not** governable via proposals:
`set_admin`, `set_distribution_contract`.

Governance **can** disable the admin veto power by passing a proposal that
cross-contract-calls `disable_veto_power()` on the governance contract (via the
ILN contract).

---

## 4. Proposal lifecycle

```
                ┌─────────────┐
                │   Created   │  create_proposal() called
                └──────┬──────┘
                       │  voting opens immediately
                ┌──────▼──────┐
                │   Active    │  voters call cast_vote(support=true/false)
                └──────┬──────┘
          ┌────────────┼────────────┬──────────────────────┐
          │            │            │                      │
          │    end_time reached     │               admin calls
          │                        │               veto_proposal()
   ┌──────▼──────┐          ┌──────▼──────┐        ┌──────▼──────┐
   │  Rejected   │          │   Passed    │        │   Vetoed    │
   │(no quorum / │          └──────┬──────┘        └─────────────┘
   │  against)   │                 │  execute_proposal() called
   └─────────────┘          ┌──────▼──────┐
                            │  Executed   │  ILN contract updated
                            └─────────────┘
```

### Proposal statuses

| Status | Description |
|--------|-------------|
| `Active` | Voting is open |
| `Passed` | Voting closed, quorum and majority met — awaiting execution |
| `Rejected` | Voting closed, quorum not met or majority against |
| `Executed` | Proposal passed and the on-chain action was applied |
| `Vetoed` | Admin used emergency veto before the proposal was executed |

### Voting window

Each proposal has a fixed **3-day (259 200 second)** voting window starting
from the ledger timestamp at the moment `create_proposal` is invoked.

### Double-vote prevention

A `HasVoted(proposal_id, voter_address)` key is stored in Soroban temporary
storage when a vote is cast. The receipt is only needed through the proposal's
3-day voting window, so it is extended to 69,120 ledgers: approximately 4 days
at 5 seconds per ledger. This covers the full voting period plus a 1-day buffer
for boundary reads and indexers while allowing automatic expiry. Attempting to
vote again while the receipt is live returns `AlreadyVoted`.

---

## 5. Worked example — end-to-end proposal

Suppose the community wants to raise the protocol fee from 0 to 50 bps (0.5%).

```
Step 1 — Create the proposal
────────────────────────────
Caller: any address (no minimum token balance required to propose)
Function: GovContract::create_proposal(creator, ProposalAction::UpdateFeeRate(50), hash, 50)
Result: proposal_id = 1
        voting_end  = now + 259_200

Step 2 — Vote
─────────────
During the 3-day window, token holders call:
  GovContract::cast_vote(voter_addr, proposal_id=1, support=true)   // For
  GovContract::cast_vote(voter_addr, proposal_id=1, support=false)  // Against

Each call uses the stored checkpoint weight for that proposal/voter pair and
adds it to votes_for or votes_against.

Step 3 — Execute (after voting_end)
────────────────────────────────────
Anyone calls: GovContract::execute_proposal(proposal_id=1, total_supply)

The contract checks:
  total_votes = votes_for + votes_against
  quorum      = total_supply * min_quorum_bps / 10_000  (default 10%)

  If total_votes < quorum      → status = Rejected, deposit forfeited,
                                 ProposalRejected{reason: QuorumNotReached} emitted, call returns Ok
  If votes_for > votes_against → status = Passed, then:
    invoke_contract(iln_contract, "update_fee_rate", [50])
    status = Executed (if the call fails: ProposalExecutionFailed emitted,
                       call returns Ok, status stays Passed and is retryable)
  Else                         → status = Rejected, deposit forfeited,
                                 ProposalRejected{reason: MajorityAgainst} emitted, call returns Ok

Emergency path — Admin veto before execution
─────────────────────────────────────────────
If the admin decides the proposal is harmful, at any point while it is
Active or Passed they can call:
  GovContract::veto_proposal(proposal_id=1, reason_hash)
  → status = Vetoed; ProposalVetoed event emitted; proposal cannot be executed
```

---

## 6. Quorum and majority rules

| Parameter | Value | Source |
|-----------|-------|--------|
| Quorum threshold | Configurable, default 10% of `total_supply` | `min_quorum_bps` (1 000 bps) |
| Majority rule | Simple majority (`votes_for > votes_against`) | Strict `>` |
| Abstain option | Not supported; every vote is For or Against | — |

> **Note:** `total_supply` is a caller-supplied argument to `execute_proposal`,
> not read from the token contract.  An incorrect value will distort the quorum
> check.  Future governance iterations should read supply on-chain.

---

## 7. Execution mechanics

`execute_proposal` uses `env.invoke_contract` to call the ILN contract
synchronously.  If the cross-contract call reverts, `execute_proposal`
also reverts, leaving the proposal status unchanged (`Passed`).  It can be
retried once the root cause is fixed.

There is **no timelock delay** — execution happens in the same transaction as
the `execute_proposal` call, immediately after the voting window closes.

---

## 8. Admin veto power

### Purpose

In the early protocol phase before full decentralisation the admin holds an
emergency veto power.  This allows blocking a governance proposal that would
cause irreparable harm (e.g. setting fees to 100%) before the proposal is
executed on-chain.

### Function: `veto_proposal(proposal_id, reason_hash)`

| Property | Detail |
|----------|--------|
| Callable by | Admin only (`admin.require_auth()`) |
| Vetoable statuses | `Active`, `Passed` |
| Non-vetoable statuses | `Executed`, `Rejected`, `Vetoed` (returns `NotVetoable`) |
| Result | Proposal status set to `Vetoed`; `ProposalVetoed` event emitted |
| `reason_hash` | `BytesN<32>` — SHA-256 hash of an off-chain document explaining the reason |

### Function: `disable_veto_power()`

| Property | Detail |
|----------|--------|
| Callable by | ILN contract (`iln_contract.require_auth()`) — triggered by a passed governance proposal |
| Effect | Sets `VetoPowerEnabled = false` permanently (one-way switch) |
| After disabling | Any call to `veto_proposal` returns `VetoPowerDisabled` |

### Function: `is_veto_power_enabled() → bool`

Read-only getter.  Returns `true` while the veto power is active.

### Initialisation

`initialize(iln_contract, gov_token, admin)` now requires an `admin` address.
`VetoPowerEnabled` is set to `true` at initialisation time.

### Intended removal timeline

`disable_veto_power()` **must be called via governance vote before mainnet
launch**.  Until it is called, the admin retains unilateral veto authority.

---

## 9. Security considerations

### Admin veto abuse

The admin veto is a centralisation risk.  Mitigations:
- Veto is **emit-only** — the admin cannot execute arbitrary code, only stop a proposal.
- A `reason_hash` is recorded on-chain so the rationale is publicly verifiable off-chain.
- Governance can permanently remove the power via `disable_veto_power()`.
- All vetoes emit `ProposalVetoed` events providing a full audit trail.

### Quorum attacks

An attacker with > 10% of supply can reach quorum alone.  Mitigations:
- Increase the quorum threshold via a governance proposal.
- Introduce a minimum proposal delay so the community can react.

### Flash-loan / balance manipulation

Voting power is pinned to proposal-scoped checkpoints, so later balance changes
cannot inflate a voter's weight during an active proposal.

### Delegation

Transitive vote delegation is implemented (Issue #64).  Cycle detection and a
maximum depth of 10 hops prevent infinite loops.

### Double-proposal spam

There is no minimum token balance or deposit required to create a proposal.
A future `min_proposal_deposit` guard is recommended.

---

## 10. Past governance decisions

*This section serves as a historical record of community decisions.*

- *(Currently empty — no proposals have been executed yet.)*