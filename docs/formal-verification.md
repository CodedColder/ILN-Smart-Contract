# Formal Verification Specification — Invoice Lifecycle & Governance

## 1. Overview

Cross-contract system invariants spanning `invoice_liquidity`, `iln_distribution`, and `insurance_pool` are specified separately in [`formal-verification-cross-contract.md`](formal-verification-cross-contract.md).

This document defines formal invariants, valid state transitions, and authorization properties for the protocol's core state machines. These specifications serve as the basis for formal verification, property-based testing, and audit review.

Two independent state machines are covered:

- **Part I** — the invoice lifecycle in the Invoice Liquidity contract (`contracts/invoice_liquidity/src/invoice.rs` and `contracts/invoice_liquidity/src/lib.rs`).
- **Part II** — the governance proposal lifecycle in the ILN Governance contract (`contracts/iln_governance/src/lib.rs`).

**Target Contracts:** `contracts/invoice_liquidity/src/invoice.rs`, `contracts/invoice_liquidity/src/lib.rs`, `contracts/iln_governance/src/lib.rs`

---

# Part I — Invoice Lifecycle

---

## 2. State Machine Specification

### 2.1 Invoice Status Enum

```rust
pub enum InvoiceStatus {
    Pending,         // Submitted, awaiting liquidity
    Funded,          // Fully funded by LP(s), freelancer paid out
    PartiallyFunded, // Partially funded, still awaiting remainder
    Paid,            // Payer settled in full
    Defaulted,       // Past due_date, unpaid
    Appealed,        // Payer contested the default ruling
    Disputed,        // Payer disputed the invoice before settlement
    Expired,         // Past due_date with no funding
    Cancelled,       // Freelancer cancelled before funding
}
```

### 2.2 Valid State Transition Table

| From State | Action | To State | Guards |
|---|---|---|---|
| `Pending` | `submit_invoice` | `Pending` | Caller is freelancer; terms valid |
| `Pending` | `fund_invoice` (full) | `Funded` | `amount_funded == amount` |
| `Pending` | `fund_invoice` (partial) | `PartiallyFunded` | `0 < amount_funded < amount` |
| `Pending` | `cancel_invoice` | `Cancelled` | Caller is freelancer |
| `Pending` | `expire_invoice` | `Expired` | `timestamp > due_date` |
| `Pending` | `update_invoice` | `Pending` | Caller is freelancer |
| `Pending` | `transfer_invoice` | `Pending` | Caller is freelancer |
| `Pending` | `dispute_invoice` | `Disputed` | Caller is payer |
| `PartiallyFunded` | `fund_invoice` (remainder) | `Funded` | `amount_funded == amount` |
| `PartiallyFunded` | `fund_invoice` (partial) | `PartiallyFunded` | `0 < amount_funded < amount` |
| `PartiallyFunded` | `cancel_invoice` | `Cancelled` | Refunds all funders |
| `PartiallyFunded` | `dispute_invoice` | `Disputed` | Caller is payer |
| `Funded` | `mark_paid` (full) | `Paid` | `amount_paid == amount` |
| `Funded` | `mark_paid` (partial) | `Funded` | `amount_paid < amount` |
| `Funded` | `claim_default` | `Defaulted` | `timestamp > due_date` |
| `Funded` | `dispute_invoice` | `Disputed` | Caller is payer |
| `Paid` | — | — | Terminal state |
| `Defaulted` | `appeal_default` | `Appealed` | Within appeal window |
| `Appealed` | `resolve_appeal(true)` | `Defaulted` | Admin only; score restored |
| `Appealed` | `resolve_appeal(false)` | `Defaulted` | Admin only |
| `Disputed` | `resolve_dispute(1)` | `Cancelled` | Admin; payer right → refund LPs |
| `Disputed` | `resolve_dispute(2)` | `Funded`/`PartiallyFunded`/`Pending` | Admin; freelancer right |
| `Disputed` | `auto_resolve_dispute` | `Funded`/`PartiallyFunded`/`Pending` | Timeout passed |
| `Expired` | — | — | Terminal state |
| `Cancelled` | — | — | Terminal state |

### 2.3 Prohibited Transitions (Enforced)

| From State | Action | Error |
|---|---|---|
| `Funded` | `fund_invoice` | `AlreadyFunded` |
| `Funded` | `update_invoice` | `AlreadyFunded` |
| `Funded` | `transfer_invoice` | `AlreadyFunded` |
| `Paid` | `fund_invoice` | `AlreadyPaid` |
| `Paid` | `mark_paid` | `AlreadyPaid` |
| `Paid` | `claim_default` | `AlreadyPaid` |
| `Defaulted` | `fund_invoice` | `InvoiceDefaulted` |
| `Defaulted` | `mark_paid` | `InvoiceDefaulted` |
| `Defaulted` | `claim_default` | `InvoiceDefaulted` |
| `Pending` | `mark_paid` | `NotFunded` |
| `Pending` | `claim_default` | `NotFunded` |
| `PartiallyFunded` | `mark_paid` | `NotFunded` |
| `PartiallyFunded` | `claim_default` | `NotFunded` |
| Any non-`Pending` | `update_invoice` | Varies by state |

---

## 3. Balance Invariants

### Invariant B1: `amount_funded <= amount`
**Property:** For every invoice, `invoice.amount_funded` must never exceed `invoice.amount`.

**Rationale:** Prevents overfunding that would break accounting.

**Enforcement:** `fund_invoice()` at `src/lib.rs:1153`:
```rust
if invoice.amount_funded + fund_amount > invoice.amount {
    return Err(ContractError::OverfundingRejected);
}
```

### Invariant B2: `amount_paid <= amount`
**Property:** For every invoice, `invoice.amount_paid` must never exceed `invoice.amount`.

**Rationale:** Prevents overpayment that would inflate LP payouts.

**Enforcement:** `mark_paid()` at `src/lib.rs:1524-1527`:
```rust
let remaining = invoice.amount - invoice.amount_paid;
if amount > remaining {
    return Err(ContractError::OverpaymentRejected);
}
```

### Invariant B3: Total LP Payout == Amount Paid - Protocol Fee
**Property:** When an invoice is fully paid, the sum of all LP payouts equals `invoice.amount - protocol_fee`.

**Enforcement:** Proportional distribution at `src/lib.rs:1607-1614`:
```rust
for i in 0..funders.len() {
    let (funder_addr, fund_amt) = funders.get(i).unwrap();
    let funder_share = distribute_amount.checked_mul(fund_amt).unwrap_or(0) / invoice.amount;
    // ...
}
```

### Invariant B4: No Double-Claim on Default
**Property:** An invoice can only transition to `Defaulted` once. Subsequent `claim_default` calls fail.

**Enforcement:** Status guard at `src/lib.rs:1733-1744` — only `Funded` invoices can transition to `Defaulted`.

---

## 4. Authorization Invariants

### Invariant A1: Freelancer Authorization
| Action | Authorized Caller | Enforcement |
|---|---|---|
| `submit_invoice` | Freelancer address | `require_submitter` |
| `update_invoice` | Freelancer of invoice | `require_submitter_by_id` |
| `cancel_invoice` | Freelancer of invoice | `require_submitter_by_id` |
| `transfer_invoice` | Freelancer of invoice | `require_submitter_by_id` |
| `convert_invoice_token` | Freelancer of invoice | `require_submitter_by_id` |

### Invariant A2: Payer Authorization
| Action | Authorized Caller |
|---|---|
| `mark_paid` | Payer of invoice |
| `appeal_default` | Payer of invoice |
| `dispute_invoice` | Payer of invoice |

### Invariant A3: LP Authorization
| Action | Authorized Caller |
|---|---|
| `fund_invoice` | LP (authenticated via `require_lp` + queue check) |
| `claim_default` | LP who funded the invoice |
| `claim_yield` | LP who funded the invoice |
| `join_fund_queue` | LP |
| `transfer_lp_position` | Current LP of invoice |

### Invariant A4: Admin Authorization
| Action | Guard |
|---|---|
| `set_admin` | `require_admin` |
| `update_fee_rate` | `require_admin` |
| `update_max_discount` | `require_admin` |
| `add_token` | `require_admin` |
| `remove_token` | `require_admin` |
| `pause` / `unpause` | `require_admin` |
| `set_distribution_contract` | `require_admin` |
| `set_price_oracle` | `require_admin` |
| `resolve_appeal` | `require_admin` |
| `resolve_dispute` | `require_admin` |
| `upgrade` | `require_admin` |

---

## 5. Proof Specification

### 5.1 Safety Properties

**SP1 — State determinism:** Given a starting state and a sequence of actions, there is exactly one reachable final state. The state machine is deterministic.

**SP2 — No stuck funds:** For every terminal state (`Paid`, `Defaulted`, `Expired`, `Cancelled`), all funds are either:
- Distributed to the intended recipient (freelancer/LP), or
- Returned to the funder (on default/cancellation), or
- Held by the contract as protocol fees.

**SP3 — Auth enforcement:** Every state-mutating action requires the caller to pass an explicit authorization guard. No action can be invoked without satisfying its auth predicate.

### 5.2 Liveness Properties

**LP1 — Funding completion:** If `amount_funded == amount`, the invoice transitions to `Funded` and the freelancer receives `amount - discount`.

**LP2 — Settlement:** If a `Funded` invoice is paid before `due_date`, the invoice transitions to `Paid` and LPs receive principal + yield.

**LP3 — Default resolution:** If a `Funded` invoice remains unpaid past `due_date`, any LP may invoke `claim_default` to receive their principal back (minus discount).

### 5.3 Invariant Enforcement in Code

The function `check_invariants()` in `tests_invariants.rs` programmatically asserts:
- `Pending` → `funder.is_none()` && `amount_funded == 0`
- `PartiallyFunded` → `0 < amount_funded < amount`
- `Funded` → `funder.is_some()` && `funded_at.is_some()` && `amount_funded == amount`
- All invoice IDs are loadable from storage

---

## 6. Storage Isolation Guarantee

**Property:** Each invoice occupies an independent storage key (`DataKey::Invoice(id)`). Operations on invoice `i` never modify the storage of invoice `j` for `i ≠ j`.

**Enforcement:** All invoice mutations operate on a single invoice loaded by ID. Cross-invoice state isolation is verified by `test_storage_isolation_adjacent_invoice_ids()` in `tests_security.rs`.

---

## 7. Coverage

| Property | Verified By |
|---|---|
| State machine valid transitions | `tests_state_machine.rs` — 15+ test cases |
| State machine invalid transitions | `tests_state_machine.rs` — 10+ test cases |
| Balance invariants | `tests_security.rs` — overflow/underflow tests |
| Authorization invariants | `tests_access_control.rs`, `tests_auth.rs` |
| Storage isolation | `tests_security.rs` — adjacency test |
| Cross-invoice independence | `tests_invariants.rs` — `check_invariants` |
| Admin function guards | `tests_access_control.rs` |

---

# Part II — Governance Proposal Lifecycle

## 8. State Machine Specification

### 8.1 ProposalStatus Enum

```rust
pub enum ProposalStatus {
    Active,    // Voting window open; votes_for/votes_against accumulating
    Passed,    // Quorum reached and votes_for > votes_against; awaiting timelock
    Rejected,  // Quorum not reached, or votes_against >= votes_for
    Executed,  // Cross-contract action applied successfully
    Vetoed,    // Blocked by admin via veto_proposal()
}
```

Unlike the invoice lifecycle (one enum field on a mutable record), a `GovernanceProposal` additionally carries `votes_for`, `votes_against`, `voting_end` (unix timestamp) and `eta_ledger` (a ledger-sequence timelock), both of which gate the transitions below alongside `status`.

**Target Contract:** `contracts/iln_governance/src/lib.rs`

### 8.2 Valid State Transition Table

| From State | Action | To State | Guards |
|---|---|---|---|
| — | `create_proposal` | `Active` | Caller authorized; `proposer_balance >= min_proposal_balance`; `voting_end = now + VOTING_PERIOD_SECS` |
| `Active` | `cast_vote` | `Active` | `now < voting_end`; voter has not already voted; `weight > 0` |
| `Active` | `delegate_votes` / `undelegate_votes` | `Active` | Does not itself touch proposal status; adjusts `DelegatedToMe` tallies used by subsequent `cast_vote` calls |
| `Active` | `execute_proposal` (quorum not reached) | `Rejected` | `now >= voting_end`; `votes_for + votes_against < quorum(total_supply, min_quorum_bps)` |
| `Active` | `execute_proposal` (majority not reached) | `Rejected` | `now >= voting_end`; quorum reached; `votes_for <= votes_against` |
| `Active` | `execute_proposal` (passes) | `Passed` | `now >= voting_end`; quorum reached; `votes_for > votes_against`; sets `eta_ledger = current_ledger + execution_delay` |
| `Active` | `veto_proposal` | `Vetoed` | Caller is stored admin; `VetoPowerEnabled == true` |
| `Passed` | `execute_proposal` (timelock elapsed, call succeeds) | `Executed` | `current_ledger >= eta_ledger`; cross-contract call to the target contract returns `Ok(())` |
| `Passed` | `execute_proposal` (timelock elapsed, call fails) | `Passed` | `current_ledger >= eta_ledger`; cross-contract call traps or errors — status/`eta_ledger` unchanged so a retry is possible (Issue #531) |
| `Passed` | `veto_proposal` | `Vetoed` | Caller is stored admin; `VetoPowerEnabled == true` |
| `Rejected` | — | — | Terminal state |
| `Executed` | — | — | Terminal state |
| `Vetoed` | — | — | Terminal state |

### 8.3 Prohibited Transitions (Enforced)

| From State | Action | Error |
|---|---|---|
| `Active` | `execute_proposal` before `voting_end` | `VotingOngoing` |
| `Active` | `cast_vote` after `voting_end` | `VotingEnded` |
| Any non-`Active` | `cast_vote` | `ProposalNotActive` |
| `Active`/`Passed`/`Rejected`/`Executed`/`Vetoed` | `cast_vote` twice by same voter on same proposal | `AlreadyVoted` |
| `Passed` | `execute_proposal` before `eta_ledger` | `TimelockNotExpired` |
| `Rejected`, `Executed`, `Vetoed` | `execute_proposal` | `AlreadyResolved` |
| `Rejected`, `Executed`, `Vetoed` | `veto_proposal` | `NotVetoable` |
| Any vetoable state | `veto_proposal` when `VetoPowerEnabled == false` | `VetoPowerDisabled` |
| — | `create_proposal` with `proposer_balance < min_proposal_balance` | `InsufficientProposerBalance` |
| — | `delegate_votes(self, self)` | `CannotDelegateToSelf` |
| — | `delegate_votes` closing a cycle | `DelegationCyclePrevented` |
| — | `delegate_votes` chain exceeding `MaxDelegationDepth` | `MaxDelegationDepthExceeded` |

---

## 9. Vote & Weight Invariants

### Invariant V1: Vote weight is fixed at cast time
**Property:** `cast_vote` uses `own_balance` (snapshotted at `create_proposal` time for the proposer, or checkpoint-proven on first vote for others: requires a `BalanceCheckpoint` predating the proposal's creation ledger by `MIN_VOTE_HOLD_LEDGERS`, carrying `min(checkpoint, current)` — Issue #805) plus the caller's current `DelegatedToMe` tally; once recorded, `AppliedVoteWeight(proposal_id, voter)` never changes for that voter/proposal pair.

**Enforcement:** `cast_vote`'s snapshot branch (proposer fast path) and `proven_own_balance` gate (first-vote path) in `contracts/iln_governance/src/lib.rs`, plus the vote receipt write to temporary storage.

### Invariant V2: One vote per address per proposal
**Property:** A given `(proposal_id, voter)` pair can only increment `votes_for`/`votes_against` once.

**Enforcement:** `HasVoted(proposal_id, voter)` guard at `src/lib.rs:823-826`, checked before any tally mutation.

### Invariant V3: Delegation is acyclic and depth-bounded
**Property:** The `Delegation` forward-pointer graph never contains a cycle, and no delegation chain exceeds `MaxDelegationDepth` hops.

**Enforcement:** Forward-walk cycle check at `src/lib.rs:712-727` in `delegate_votes`, executed before the new edge is stored.

### Invariant V4: `DelegatedToMe` tallies stay consistent under re-delegation
**Property:** When a delegator re-delegates or undelegates, the old terminal node's `DelegatedToMe` tally is decremented by exactly the delegator's balance and the new terminal's tally is incremented by the same amount — the sum of all `DelegatedToMe` values attributable to a single delegator's balance is invariant across delegation changes.

**Enforcement:** `adjust_delegated_to_me()` at `src/lib.rs:1444-1453`, called symmetrically on the old and new terminal in `delegate_votes` (`src/lib.rs:733-746`) and `undelegate_votes` (`src/lib.rs:771-779`).

### Invariant V5: Quorum denominator is not caller-controlled
**Property:** `GovTokenTotalSupply`, the denominator used to compute the quorum threshold in `execute_proposal`, can only be set at `initialize` or via `set_gov_token_total_supply` (ILN-contract-gated) — never as an `execute_proposal` argument.

**Rationale:** Prevents a caller from inflating or deflating the effective quorum bar (Issue #622).

**Enforcement:** `execute_proposal` reads `GovTokenTotalSupply` from instance storage only (`src/lib.rs:980-984`); the setter requires `iln_contract.require_auth()` (`src/lib.rs:437-464`).

### Invariant V6: A failed execution cannot silently finalize
**Property:** If the cross-contract call dispatched from `execute_proposal` on a `Passed` proposal fails (trap or `Err`), the proposal's `status` and `eta_ledger` are left unchanged — it remains `Passed`, not `Executed`.

**Rationale:** Guarantees `execute_proposal` is safely retriable and that `Executed` status is a reliable signal that the action actually took effect (Issue #531).

**Enforcement:** `invoke_and_check()` (`src/lib.rs:1396-1408`) uses `try_invoke_contract`; the `!succeeded` branch (`src/lib.rs:1186-1198`) returns `ExecutionFailed` without touching persistent proposal storage.

---

## 10. Authorization Invariants

### Invariant GA1: Proposer Authorization
| Action | Authorized Caller | Enforcement |
|---|---|---|
| `create_proposal` | `proposer` (self-authorized) | `proposer.require_auth()` |

### Invariant GA2: Voter Authorization
| Action | Authorized Caller |
|---|---|
| `cast_vote` | `voter` (self-authorized via `require_auth`) |
| `delegate_votes` | `delegator` (self-authorized) |
| `undelegate_votes` | `delegator` (self-authorized) |

### Invariant GA3: Admin Authorization
| Action | Guard |
|---|---|
| `veto_proposal` | Caller must equal stored `Admin`; `VetoPowerEnabled` must be `true` |
| `set_execution_delay` | Caller must equal stored `Admin` (or become admin on first call if unset) |

### Invariant GA4: ILN-Contract-Gated Authorization
These parameters can only be changed by the configured `IlnContract` address (i.e. routed through a passed governance proposal on the ILN contract itself, or an equivalent trusted caller) — never by an arbitrary account:

| Action | Guard |
|---|---|
| `set_min_quorum_bps` | `iln_contract.require_auth()` |
| `set_min_proposal_balance` | `iln_contract.require_auth()` |
| `set_gov_token_total_supply` | `iln_contract.require_auth()` |
| `set_quadratic_voting_enabled` | `iln_contract.require_auth()` |
| `set_max_delegation_depth` | `iln_contract.require_auth()` |
| `disable_veto_power` | `iln_contract.require_auth()` (one-way switch; cannot be re-enabled) |

### Invariant GA5: Execution Authorization
`execute_proposal` itself has no caller restriction — it is permissionless (any account may trigger the transition once the guards in §8.2 are met). Authorization instead lives in the *target* of the cross-contract call: e.g. `UpdateReputationBonusParams` passes `env.current_contract_address()` as the `caller` argument, so the downstream contract's own admin check must recognize the governance contract's address (`src/lib.rs:1153-1164`).

---

## 11. Proof Specification

### 11.1 Safety Properties

**GSP1 — State determinism:** Given a proposal's starting status and a sequence of actions, there is exactly one reachable final status. `execute_proposal` is idempotent on already-terminal states (`Rejected`/`Executed`/`Vetoed` always return `AlreadyResolved`).

**GSP2 — No premature execution (Issue #1087):** A proposal cannot reach `Executed` before both `now >= voting_end` (voting-window guard) and `current_ledger >= eta_ledger` (timelock guard) have been satisfied in that order. At any sequence `current_ledger < eta_ledger`, execution attempts return `TimelockNotExpired` and no cross-contract state mutations occur.

**GSP3 — No double execution:** Once a proposal reaches `Executed`, no further call can change its `action_type` effects — `execute_proposal` on an `Executed` proposal returns `AlreadyResolved` without re-invoking the cross-contract call.

**GSP4 — Veto is a strict override & transition safety (Issue #1088):** `veto_proposal` can transition `Active` or `Passed` directly to `Vetoed`, bypassing quorum/majority checks entirely, but only while `VetoPowerEnabled == true`; once `disable_veto_power` runs, this path is permanently closed (Invariant GA4). `Vetoed` is a terminal state that permanently disables execution. Non-vetoable states (`Executed`, `Rejected`, `Vetoed`) reject veto attempts with `NotVetoable`.

**GSP5 — Auth enforcement across proposal lifecycle (Issue #1089):** Every state-mutating action requires the caller (or a designated authority — proposer, voter, admin/veto signer, or the configured ILN contract) to pass an explicit `require_auth()` check. No action can be invoked without satisfying its auth predicate and parameter bounds.

### 11.2 Liveness Properties

**GLP1 — Voting resolution:** Every `Active` proposal is resolvable: once `now >= voting_end`, the next `execute_proposal` call deterministically moves it to either `Passed` or `Rejected` based on quorum and vote tally, with no state in which it can be left permanently `Active` past its `voting_end`.

**GLP2 — Retry-to-completion:** A `Passed` proposal whose cross-contract execution call fails remains `Passed` (Invariant V6) — `execute_proposal` can be called again indefinitely, and the proposal reaches `Executed` as soon as the underlying failure condition (e.g. target contract paused) is resolved.

**GLP3 — Delegation reachability:** For any delegator with a non-cyclic delegation chain of length `<= MaxDelegationDepth`, `resolve_terminal()` terminates and returns a well-defined terminal address whose `DelegatedToMe` tally includes the delegator's balance.

### 11.3 Invariant Enforcement in Code

There is no single `check_invariants()` helper analogous to the invoice contract's `tests_invariants.rs`; the governance invariants above are instead enforced inline at each mutation site (guard clauses cited per invariant in §8–§10) and exercised by the integration/lifecycle test suites listed in §13.

---

## 12. Storage Isolation Guarantee

**Property:** Each proposal occupies an independent storage key (`StorageKey::Proposal(id)`), each vote receipt an independent key (`StorageKey::HasVoted(proposal_id, voter)` / `StorageKey::AppliedVoteWeight(proposal_id, voter)`), and each delegation edge an independent key (`StorageKey::Delegation(address)` / `StorageKey::DelegatedToMe(address)`). Operations on proposal `i` never modify the storage of proposal `j` for `i ≠ j`, and voting on one proposal never touches another proposal's tallies.

**Enforcement:** All proposal mutations load and store a single `GovernanceProposal` by `id`; all vote/delegation storage keys are keyed by `(proposal_id, voter)` or `address` respectively, giving each pair disjoint storage slots.

---

## 13. Coverage

| Property | Verified By |
|---|---|
| State machine valid transitions (create → vote → execute) | `contracts/tests/governance_lifecycle_test.rs`, `contracts/tests/governance_main_integration_test.rs` |
| Quorum / majority rejection paths | `contracts/iln_governance/src/test.rs` |
| Timelock (`eta_ledger`) enforcement & execution-not-before-eta (Issue #1087) | `contracts/iln_governance/src/test.rs`, `contracts/tests/cross_contract_invariants_test.rs::x6_governance_execution_not_before_eta_invariant` |
| Execution retry on cross-contract failure (Issue #531) | `contracts/iln_governance/src/test.rs` |
| Veto transition safety and `disable_veto_power` one-way switch (Issue #68, #1088) | `contracts/iln_governance/src/test.rs`, `contracts/tests/cross_contract_invariants_test.rs::x7_governance_veto_transition_safety` |
| Proposal lifecycle authorization across all actors (Issue #1089) | `contracts/iln_governance/src/test.rs`, `contracts/tests/cross_contract_invariants_test.rs::x8_governance_proposal_lifecycle_authorization` |
| Delegation cycle/depth prevention (Issue #64) | `contracts/iln_governance/src/test.rs` |
| Quadratic voting weight transform (Issue #530) | `contracts/iln_governance/src/test.rs` |
| Access control (admin / ILN-contract-gated setters) | `contracts/iln_governance/src/test.rs` |
| Benchmarks / gas bounds | `contracts/iln_governance/src/tests_benchmarks.rs` |

---

## 14. Governance Snapshot & Delegation Invariants

This section extends the governance formal verification to explicitly specify snapshot integrity and delegation safety properties, addressing Issue #810.

### 14.1 Vote-Weight Snapshot Invariants

**Invariant SN1: Snapshot monotonicity for a given proposal**
**Property:** For a given `proposal_id`, once a `voter` address has cast a vote (checked via `HasVoted(proposal_id, voter)`), that voter's recorded vote weight (`AppliedVoteWeight(proposal_id, voter)`) never changes for the duration of that proposal's lifetime (through terminal status).

**Enforcement:**
- Snapshot read at first vote: `cast_vote` loads `own_balance` at proposal creation (proposer) or via the Issue #805 checkpoint gate for other voters — a `BalanceCheckpoint` predating the proposal's creation ledger by `MIN_VOTE_HOLD_LEDGERS`, carrying `min(checkpoint, current)`; otherwise `InsufficientHoldingPeriod`
- Write-once guarantee: `AppliedVoteWeight` is written only on the first `cast_vote` call for a (proposal, voter) pair
- Immutability: Subsequent balance changes to the `voter` do not affect the recorded weight

**Specification:**
```rust
let proposal = get_proposal(proposal_id);
for voter in voters_who_cast_vote_on(proposal_id) {
    let recorded_weight = get_applied_vote_weight(proposal_id, voter);
    let current_balance = get_token_balance(voter);
    // recorded_weight was fixed at voter's first vote, independent of current_balance
    assert!(recorded_weight == apply_weight_fn(voter, proposal_id, balance_at_first_vote));
    assert_invariant!(recorded_weight does not depend on current_balance);
}
```

**Rationale:** Prevents post-vote balance inflation from changing voting outcomes.

---

### 14.2 Vote-Count Invariant: No Double-Voting

**Invariant SN2: One vote per address per proposal**
**Property:** For a given `(proposal_id, voter)` pair, the vote tallies (`votes_for` / `votes_against` on the proposal) can only be incremented once by that voter across the proposal's lifetime.

**Enforcement:**
- Guard check: `HasVoted(proposal_id, voter)` is checked before any tally mutation (`src/lib.rs:823-826`)
- Idempotence: Once `HasVoted(proposal_id, voter)` is written, subsequent `cast_vote` calls by the same voter on the same proposal return `AlreadyVoted` without changing tallies

**Specification:**
```rust
let proposal = get_proposal(proposal_id);
for voter in all_addresses {
    let has_voted = check_has_voted(proposal_id, voter);
    let vote_count_for = count_votes_by_voter_for(proposal_id, voter, FOR);
    let vote_count_against = count_votes_by_voter_against(proposal_id, voter, AGAINST);
    
    // If not voted, both counts are 0
    if !has_voted {
        assert_eq!(vote_count_for, 0);
        assert_eq!(vote_count_against, 0);
    }
    // If voted, exactly one count is 1, the other is 0 (cannot vote twice or both for/against)
    if has_voted {
        assert!((vote_count_for == 1 && vote_count_against == 0) 
                || (vote_count_for == 0 && vote_count_against == 1));
    }
}
```

**Rationale:** Prevents double-voting and ensures voting power is counted exactly once.

---

### 14.3 Delegation Graph Invariants

**Invariant DEL1: Delegation graph is acyclic**
**Property:** The directed graph formed by delegation edges (`address` → `delegated_to`) contains no cycles. If `A` delegates to `B`, then `B` cannot (directly or transitively) delegate to `A`.

**Enforcement:**
- Forward-walk cycle check: `src/lib.rs:712-727` in `delegate_votes` performs a depth-bounded traversal to detect cycles before storing the new edge
- Storage: Each delegation is stored in `Delegation(address)` as a forward pointer (`delegated_to: Option<Address>`)

**Specification:**
```rust
fn has_cycle(delegations: HashMap<Address, Address>) -> bool {
    for address in delegations.keys() {
        let mut visited = HashSet::new();
        let mut current = address;
        while let Some(next) = delegations.get(&current) {
            if visited.contains(&next) {
                return true; // Cycle detected
            }
            visited.insert(&next);
            current = next;
        }
    }
    false
}

assert!(!has_cycle(all_delegations()));
```

**Rationale:** Prevents infinite traversal loops during vote resolution.

---

### 14.4 Delegation Depth Bound

**Invariant DEL2: Delegation chain depth ≤ MaxDelegationDepth**
**Property:** For any address `A`, the chain of delegations from `A` (following `delegated_to` pointers until reaching a non-delegating address) contains at most `MaxDelegationDepth` edges (default: 10).

**Enforcement:**
- Depth check on store: `src/lib.rs:712-727` counts hops during the forward-walk cycle check and rejects if depth would exceed `MaxDelegationDepth`
- Rejection code: `src/lib.rs:728-730` returns `MaxDelegationDepthExceeded`

**Specification:**
```rust
fn delegation_depth(address: Address, delegations: HashMap<Address, Address>) -> u32 {
    let mut depth = 0;
    let mut current = address;
    while let Some(next) = delegations.get(&current) {
        depth += 1;
        current = next;
    }
    depth
}

for address in all_addresses {
    assert!(delegation_depth(address, all_delegations()) <= MaxDelegationDepth);
}
```

**Rationale:** Bounds per-vote traversal cost and prevents denial-of-service via deep delegation chains.

---

### 14.5 Delegated-To-Me Tally Consistency

**Invariant DEL3: DelegatedToMe tallies accurately reflect delegators**
**Property:** For a given address `terminal`, the `DelegatedToMe(terminal)` tally equals the sum of `own_balance` of all addresses whose delegation chain terminates at `terminal`.

**Enforcement:**
- Tally updates: `adjust_delegated_to_me()` (`src/lib.rs:1444-1453`) is called symmetrically when a delegator changes their `Delegation(delegator)` pointer
  - Old terminal's `DelegatedToMe` is decremented by `delegator.own_balance`
  - New terminal's `DelegatedToMe` is incremented by `delegator.own_balance`
- Invariant: sum is preserved across delegations changes (zero-sum property)

**Specification:**
```rust
fn compute_delegated_to_me_tally(terminal: Address, delegations: HashMap<Address, Address>, balances: HashMap<Address, i128>) -> i128 {
    delegations.iter()
        .filter(|(address, delegated_to)| resolve_terminal(address, delegations) == terminal)
        .map(|(address, _)| balances[address])
        .sum()
}

for terminal in all_addresses {
    let stored_tally = get_delegated_to_me(terminal);
    let computed_tally = compute_delegated_to_me_tally(terminal, all_delegations(), all_balances());
    assert_eq!(stored_tally, computed_tally);
}
```

**Rationale:** Ensures vote-weight tallies remain accurate as delegation edges change.

---

### 14.6 Vote Weight Computation with Delegation

**Invariant VW1: Vote weight includes both own balance and delegated balance**
**Property:** When a voter casts a vote, the recorded `AppliedVoteWeight(proposal_id, voter)` equals the result of the weight function (linear or quadratic, depending on `QuadraticVotingEnabled`) applied to:
```
vote_weight_input = own_balance + DelegatedToMe(voter)
```

**Enforcement:**
- Weight calculation: `src/lib.rs:879-885` reads both `own_balance` (from snapshot) and `DelegatedToMe(voter)` (current) and applies the weight function
- Recorded as: `AppliedVoteWeight(proposal_id, voter) = weight_fn(vote_weight_input)`

**Specification (Linear Mode):**
```rust
if !quadratic_voting_enabled {
    let own = get_own_balance(voter, proposal_id);
    let delegated = get_delegated_to_me(voter);
    let recorded_weight = get_applied_vote_weight(proposal_id, voter);
    assert_eq!(recorded_weight, own + delegated);
}
```

**Specification (Quadratic Mode):**
```rust
if quadratic_voting_enabled {
    let own = get_own_balance(voter, proposal_id);
    let delegated = get_delegated_to_me(voter);
    let input = own + delegated;
    let recorded_weight = get_applied_vote_weight(proposal_id, voter);
    assert_eq!(recorded_weight, isqrt(input));
}
```

**Rationale:** Ensures delegated votes contribute to the voter's weight and quadratic dampening is correctly applied to the total.

---

### 14.7 Sum of Vote Weights ≤ Total Supply

**Invariant VW2: Total vote weight on a proposal never exceeds the governance token total supply**
**Property:** For a given `proposal_id`, the sum of all `AppliedVoteWeight(proposal_id, voter)` across all voters who cast votes is ≤ `GovTokenTotalSupply`.

**Enforcement:**
- Invariant enforcement: implicit in the vote model — each voter's weight is derived from their token balance (own + delegated), and the sum of all balances ≤ total supply
- Formal check: not explicitly guarded in code, but verified by property-based testing

**Specification:**
```rust
let proposal = get_proposal(proposal_id);
let total_supply = get_gov_token_total_supply();
let total_weight: i128 = voters_on_proposal(proposal_id)
    .map(|voter| get_applied_vote_weight(proposal_id, voter))
    .sum();
    
assert!(total_weight <= total_supply);
```

**Rationale:** Prevents artificial vote inflation if the implementation allowed a voter to record more weight than tokens they own.

---

## 15. Test Coverage for Snapshot & Delegation

| Property | Verified By | Issue |
|---|---|---|
| Snapshot monotonicity — balance change post-vote does not affect weight | `contracts/iln_governance/src/test.rs`, `governance_main_integration_test.rs` | #810 |
| Double-vote rejection | `contracts/iln_governance/src/test.rs` | #810 |
| Delegation cycle detection and rejection | `contracts/iln_governance/src/test.rs` | #64, #810 |
| Delegation depth bound enforcement | `contracts/iln_governance/src/test.rs` | #64, #810 |
| DelegatedToMe tally consistency under re-delegation | `contracts/iln_governance/src/test.rs` | #810 |
| Vote weight = own_balance + DelegatedToMe (linear) | `contracts/iln_governance/src/test.rs` | #810 |
| Vote weight = isqrt(own_balance + DelegatedToMe) (quadratic) | `contracts/iln_governance/src/test.rs` | #810 |
| Sum of vote weights ≤ total supply | `contracts/iln_governance/src/test.rs` | #810 |
| Combined adversarial attack suite (flash-loan + Sybil + delegation + voting) | `contracts/tests/tests_adversarial_governance.rs` | #813 |
