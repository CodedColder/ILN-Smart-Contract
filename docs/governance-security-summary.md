# Governance Security Summary

**Status:** Reviewer-facing synthesis — prepared for the external security auditor and the SCF application.
**Scope:** Consolidates the decentralization- and governance-hardening work on `iln_governance` and the governance-controlled surface of `invoice_liquidity` into a single honest picture of what is resolved versus what is an accepted risk at launch. It does not re-derive the analyses — each row links to the primary source (an ADR, the threat model, or `governance.md`).

This document mirrors [`oracle-attack-economics.md`](oracle-attack-economics.md)'s approach for economic security: quantify the attack, state the current parameter values as shipped, and be explicit about the gap between the designed end-state and what is live today.

---

## 1. What "governance" means here, and where authority actually sits

ILN governance lets token holders propose and vote on protocol changes on-chain. `GovContract` (`contracts/iln_governance`) orchestrates voting; a passing proposal cross-contract-calls `invoice_liquidity` to apply the change atomically ([`governance.md` §1](governance.md#1-overview)).

The **real ordering of authority today** ([ADR-012 §"Where authority actually sits"](adr/ADR-012-governance-multisig-handoff.md)):

```
contract-level multisig  (designed in ADR-008, NOT wired into lib.rs)
        ↓
single admin key         (live — every require_admin call; ideally a Stellar
                          account-level multisig, currently an empty signer list
                          per .github/mainnet-admin-signers.json)
        ↓
admin veto               (live — veto_proposal; multisig-gated per Issue #642;
                          one-way disable via disable_veto_power())
        ↓
governance               (live — controls a subset of parameters, NO timelock)
```

Every mitigation available *today* against a compromised admin is an **off-chain, Stellar-account-level** control — the contract has no concept of "signers" beyond `require_admin` seeing "the admin account authorized this call" or not ([disaster-recovery-multisig-signers.md §1–2](disaster-recovery-multisig-signers.md)).

---

## 2. Findings by governance-hardening area

| # | Area | Outcome | Resolved? | Primary source |
|---|------|---------|-----------|----------------|
| 1 | **Quadratic voting analysis** | `QuadraticVotingEnabled` flag (default `false`), governance-toggled. When on, `cast_vote` weight is `isqrt(own_balance + delegated_weight)` — floor integer sqrt via binary search over `i128`, `#![no_std]`-safe. Modeling against a synthetic power-law holder distribution showed it compresses a ~50% whale dominance to ~13% without disenfranchising large holders. **Recommendation: enable at mainnet launch** given the concentrated early token supply. Sybil-plus-flash-loan vote-splitting modeled in §6 (Issue #809): splitting `B` across `N` addresses multiplies weight by ~`sqrt(N)`, but every Sybil now needs its own aged pre-proposal checkpoint (Issue #805), so flash-funded swarms cannot materialise — no extra protocol cap added (documented decision). | ✅ Implemented; ⚠️ off by default — enabling is itself a governance action | [ADR-009](adr/ADR-009-quadratic-voting.md) |
| 2 | **Delegation depth & cost bounds** | Transitive delegation is implemented (Issue #64). **Cycle detection and a hard maximum depth of 10 hops** prevent infinite loops and bound the per-vote traversal cost. Quadratic mode sums own + delegated balance *before* the square root (not sqrt-per-component), which removes the incentive to split power across many delegate chains — sqrt is concave, so splitting-then-summing would otherwise inflate weight. | ✅ Bounded (depth 10, cycle-checked) | [`governance.md` §9 "Delegation"](governance.md#9-security-considerations); [ADR-009 "Alternatives Considered"](adr/ADR-009-quadratic-voting.md#alternatives-considered) |
| 3 | **Snapshot timing guarantees** | Voting power is pinned to **pre-proposal checkpoints** (Issue #805). Each voter holds a `BalanceCheckpoint { balance, ledger }` (`checkpoint_balance`, auto-seeded for proposers at `create_proposal`); a vote on a proposal created at ledger `C` requires `checkpoint.ledger + 10 <= C` and carries `min(checkpoint.balance, current_balance)`. The proposer's creation-time snapshot is still honoured (real funds must clear the proposer gate and escrow there). The proposal creation ledger is stored per proposal. Double-vote is blocked by a `HasVoted(proposal_id, voter)` receipt in temporary storage (TTL ≈ 4 days — the 3-day window plus a 1-day buffer) **and** a durable persistent `VoteReceipt(proposal_id, voter)` mirror (Issue #1099) so a host-archived temporary receipt cannot reopen the double-vote window; the guard fails closed on presence in either store. The applied weight (linear or quadratic) is recorded per-voter-per-proposal in an `AppliedVoteWeight` receipt for after-the-fact auditability (its durable `VoteReceipt` mirror is the archival fallback). Veto multisig approvals are similarly durable per signer (`VetoApprovalReceipt`), so an archived approval list cannot reset a partially-collected veto. Delegation entries draw on the same proven balance; un-delegation always succeeds so tallies cannot get stuck. | ✅ Same-transaction flash-loan voting rejected (`InsufficientHoldingPeriod`); post-vote inflation impossible; ⚠️ residual — multi-ledger (non-flash) borrows, proposer-snapshot liveness — see §3.2 | [`governance.md` §2, §4](governance.md#2-governance-token-and-voting-power); [threat-model.md §E3](threat-model.md) |
| 4 | **Spam resistance (proposals & votes)** | **Vote** spam: bounded by the per-voter double-vote receipt and by needing non-zero voting power (`NoVotingPower` rejects 0-balance callers). **Proposal** spam: the static `MinProposalBalance` holding gate plus a forfeitable `MinProposalDeposit` escrow (Issue #814, default `0` = disabled for backwards compatibility, governance-settable via `set_min_proposal_deposit`). `create_proposal` escrows the deposit, `execute_proposal` refunds it on `Passed`/`Executed` and forfeits it on `Rejected`/expired-without-quorum, `veto_proposal` forfeits on `Vetoed`. Forfeits go to the governance-configurable `ProposalDepositSink` treasury address (not the insurance pool — spam penalties are treasury revenue, mixing them would distort pool coverage accounting); when unset, forfeits stay locked in the governance contract. Events `ProposalDepositEscrowed` / `ProposalDepositRefunded` / `ProposalDepositForfeited`; settlement is idempotent via `ProposalDepositSettled` (double-refund safe). | ✅ Implemented (deposit `0` by default — governance must set a non-zero value to activate); ⚠️ residual — a wallet above the static gate can still spam while the deposit is `0` | [`governance.md` §9 "Double-proposal spam"](governance.md#9-security-considerations); [ADR-009 "set_min_proposal_balance"](adr/ADR-009-quadratic-voting.md) |
| 5 | **Quorum consistency** | Quorum = `stored_total_supply * min_quorum_bps / 10_000` (default 10%, governance-configurable via `min_quorum_bps`). **`total_supply` is no longer caller-supplied**: `execute_proposal(proposal_id)` reads the contract-stored `GovTokenTotalSupply` (seeded at `initialize`, readable via `get_gov_token_total_supply`, updatable only by the ILN contract via `set_gov_token_total_supply`). A live SAC `total_supply()` query does not exist in `soroban-sdk` 21.x's SEP-41 interface, so the tracked counter is the on-chain source of truth (Issue #808). An attacker holding >10% of supply can still reach quorum alone. | ✅ Caller-supply manipulation closed; ⚠️ residual — tracked-supply staleness vs real mints/burns, see §3.3 | [`governance.md` §6 (note)](governance.md#6-quorum-and-majority-rules); [`governance.md` §9 "Quorum attacks"](governance.md#9-security-considerations) |
| 6 | **Veto sunset roadmap** | The admin veto (`veto_proposal`, now multisig-gated per Issue #642) is an emergency brake for the early phase. `disable_veto_power()` is a **one-way switch** callable only by the ILN contract (i.e. via a passed governance proposal); after it, `veto_proposal` returns `VetoPowerDisabled`. ADR-012 sequences the retirement: **Phase 1** wire the contract-level multisig (ADR-008) → **Phase 2** expand governance's parameter authority → **Phase 3** implement and activate a timelock on `execute_proposal` → **Phase 4** retire the veto (requires Phases 1–3 done, the multisig proven as the operative emergency mechanism, and a governance vote). | ✅ **Phase 3 Complete** — timelock implemented, handoff is a documented plan with no phase complete; `disable_veto_power()` **must be called via governance vote before mainnet** but the prerequisites for doing so safely are not yet met | [`governance.md` §8](governance.md#8-admin-veto-power); [ADR-005](adr/ADR-005-governance-timelock.md); [ADR-012 §"Decision" (phases)](adr/ADR-012-governance-multisig-handoff.md) |

---

## 3. Accepted risks at launch (explicit)

These are known, documented, and consciously carried into mainnet. They are listed here so a reviewer does not have to reconstruct them from six sources.

### 3.1 Execution timelock (partially implemented)

> **Update (#995/#999/#1002/#1005):** the contract now enforces an `eta_ledger` timelock and monotonic proposal states — see the [ADR-005 addendum](adr/ADR-005-governance-timelock.md#addendum-995-999-1002-1005-implemented-timelock-invariants). The default `ExecutionDelay` is still `0`, so mainnet must set a non-zero delay (admin `set_execution_delay`, capped at 518,400 ledgers) before the veto is retired. **Rollout:** no storage migration (no struct changes; new error codes 29–31 are additive). **Recovery:** if a bad delay is set, lower it with `set_execution_delay`; already-`Passed` proposals keep their original eta, and any `Passed` proposal can still be vetoed while the veto is enabled.
>
> **Known follow-ups, not fixed here:** (a) `execute_proposal` returns `Err` for quorum/majority rejection, and Soroban rolls back state on `Err`, so `Rejected` status and the deposit forfeit do not persist on-chain (tests bypass this with `as_contract`); (b) `delegate_votes` after voting can let the same balance be counted by the delegate; (c) the delay is admin-key controlled.

Original accepted-risk text (pre-timelock):


`execute_proposal` runs in the same transaction as the call that triggers it, immediately after the voting window closes ([ADR-005](adr/ADR-005-governance-timelock.md), [`governance.md` §7](governance.md#7-execution-mechanics)). The stated substitute is the admin veto, which can block any `Active`/`Passed` proposal. **Accepted because:** at launch the token distribution is concentrated enough that a long timelock would slow necessary parameter tuning without a real decentralization benefit, and the veto covers the "malicious proposal" case. **Exit:** ADR-012 Phase 3 implements and activates a real timelock via governance upgrade *before* the veto is retired.

### 3.2 Flash-loan vote manipulation (fixed — Issue #805)

The old lazy snapshot ([§2, finding 3](#2-findings-by-governance-hardening-area) before this fix) checkpointed a voter's balance at *their first vote*, not at proposal creation, so a flash-borrow + first-vote + repay inside one transaction permanently locked in the inflated amount ([threat-model.md §E3](threat-model.md)). **Fix shipped:** votes now draw on a `BalanceCheckpoint` that must predate the proposal's creation ledger by `MIN_VOTE_HOLD_LEDGERS` (10 ledgers, ~50 s) and carry `min(checkpoint, current)`; same-transaction voting is rejected with `InsufficientHoldingPeriod`, and `delegate_votes` entries are gated identically. **Residual (accepted):** (a) a *multi-ledger* (non-flash) borrow held past the holding period is indistinguishable from owned tokens — at that point the attacker pays real borrow cost and duration risk, which is the intended economic deterrent; (b) the proposer's own creation-time snapshot still uses the live balance — the proposer must still clear the balance gate and deposit escrow with real funds, and their vote still needs quorum + majority. **Exit if the residual ever bites:** move to staking-based governance (lock tokens in escrow for the proposal's duration) or a historical-balance oracle.

### 3.3 Tracked (not caller-supplied) `total_supply` in the quorum check

See [§2, finding 5](#2-findings-by-governance-hardening-area). **Fixed (Issue #808):** `execute_proposal` takes no supply argument; quorum reads the ILN-gated stored counter, so no caller can inflate or deflate the denominator. **Residual (accepted):** the counter can go stale between real token mints/burns and the corresponding `set_gov_token_total_supply` sync — an indexer/keeper must keep it fresh, and a stale-low supply lowers quorum while a stale-high supply can gridlock execution. It is still *not* acceptable to retire the veto (§3.4) while supply sync is an off-chain process; a future `soroban-sdk` with a SEP-41 `total_supply()` query (or a SAC wrapper exposing one) would let the contract read it live and retire this residual.

### 3.4 Veto and multisig handoff incomplete

Per [§2, finding 6](#2-findings-by-governance-hardening-area) and [ADR-012](adr/ADR-012-governance-multisig-handoff.md): the contract-level multisig (ADR-008) is designed but not wired into `lib.rs`; there is no on-chain break-glass recovery key ([disaster-recovery-multisig-signers.md §2](disaster-recovery-multisig-signers.md)); the production admin's Stellar-account signer list is empty. **Accepted for testnet.** For mainnet these are prerequisites, not a checklist to run the day something breaks — a majority-loss lockout with no prior preparation is unrecoverable on the deployed contract.

---

## 4a. Rejected-transition events and cross-contract auth (Issues #1007 / #1008)

* `execute_proposal` commits `Rejected` and emits `proposal_rejected` (reason:
  `QuorumNotReached` / `MajorityAgainst`) instead of returning an `Err` that
  Soroban rolls back; a failed cross-contract execution commits
  `proposal_execution_failed` and leaves the proposal `Passed`.
* A rejection is terminal: re-calling returns `AlreadyResolved`, the deposit is
  forfeited exactly once, and a rejected proposal can no longer be voted on or
  vetoed. No new storage keys; no unbounded growth.
* `contracts/tests/governance_cross_contract_auth_test.rs` proves, with no
  mocked auths on the execution path, that only a governance contract that is
  ILN's `Admin` can drive ILN's admin setters, that a mis-wired deployment fails
  closed, and that the timelock boundary (`eta - 1` refused, `eta` accepted)
  holds across the contract boundary.
* **Rollout:** deploy the new governance wasm, then update indexers/alerts to
  consume `proposal_rejected` / `proposal_execution_failed`. **Recovery:** the
  change is wasm-only (no storage migration); rolling back restores the old
  `Err` returns. Proposals already `Active` past their window are rejected with
  the new behaviour on the next `execute_proposal` call.

## 5. Adversarial Test Coverage (Issue #813)

A dedicated test suite combines all governance attack vectors to verify the system rejects or economically deters combined attacks, not just isolated ones:

### Test Cases Implemented

| Test | Attack Vector | Outcome | Issue |
|---|---|---|---|
| `test_first_vote_without_aged_checkpoint_rejected()` | Flash-loan borrow, first-vote in same transaction, repay | ✅ Rejected with `InsufficientHoldingPeriod`: no pre-proposal checkpoint | #805 |
| `test_aged_checkpoint_bounds_post_checkpoint_inflation()` | Checkpoint dust, inflate to 100×, vote | ✅ Bounded: weight pinned to proven checkpoint (`min`), snapshot stores 1_000 not 100_000 | #805 |
| `test_delegate_without_aged_checkpoint_rejected()` | Flash-funded address inflates a terminal's `DelegatedToMe` tally | ✅ Rejected: delegation entries are checkpoint-gated; no tally written | #805 |
| `test_quorum_uses_stored_total_supply_across_supply_changes()` | Mid-window mint/burn moves the quorum denominator | ✅ Mid-window supply expansion rejects (quorum rises); contraction passes — both read the stored counter, never caller input | #808 |
| `test_quadratic_sybil_split_needs_aged_checkpoint_per_address()` | Split 10_000 across 2 Sybils under quadratic voting | ✅ Only the aged Sybil counts (70 = `isqrt(5_000)`); the fresh Sybil is rejected | #809 |
| `test_sybil_proposal_spam()` | Create many Sybil addresses, spam proposals | ✅ Rejected: MinProposalBalance gate prevents spam | #813 |
| `test_delegation_cycle_prevention()` | Create delegation cycles (A→B→C→A) to confuse vote tallies | ✅ Rejected: cycle detection guard in `delegate_votes` | #813 |
| `test_max_delegation_depth_cap_exceeded_panics()` / `test_delegation_depth_bound()` | Create deep delegation chain (> cap) to increase vote resolution cost, including appending at the leaf | ✅ Rejected: `MaxDelegationDepth` enforced against the delegator's full inbound depth for every insertion order (reverse adjacency, #1098) | #813, #1098 |
| `test_combined_adversarial_attack()` | Flash-loan + Sybil + delegation + voting in sequence | ✅ Rejected: checkpoint gating + quorum mechanics | #813 |

**Test location:** [`contracts/iln_governance/src/test.rs`](../contracts/iln_governance/src/test.rs) (compiled unit tests). The older [`contracts/tests/tests_adversarial_governance.rs`](../contracts/tests/tests_adversarial_governance.rs) scenario suite predates the current `initialize`/`create_proposal` API and is not compiled; its scenarios are superseded by the rows above (follow-up: delete or port it).

**Coverage:** Each test demonstrates both the attack mechanism and the mitigation that prevents it, with comments explaining why the attack fails.

---

## 6. Execution & finalization hardening (Issues #1000, #1003, #1004, #1006)

Audit path: `contracts/iln_governance/src/lib.rs` (`execute_proposal`, `cast_vote`, `veto_proposal`, `get_proposal`); ADR-005 (no-timelock v1 execution model).

| Issue | Before | After (fail-closed) |
|-------|--------|---------------------|
| #1000 | A `Vetoed` proposal returned the generic `AlreadyResolved` (or `VotingOngoing` pre-deadline). A stale `Passed` proposal could still execute after a newer same-kind proposal had executed, overwriting the newer outcome. | `Vetoed` -> `ProposalCancelled` at any time. `Passed` proposal created before the last executed same-kind proposal -> `ProposalSuperseded`. One `LastExecuted(kind)` entry per action kind (19 max) — bounded state. |
| #1003 | Missing/zero `GovTokenTotalSupply` made `quorum = 0`, so any single vote passed; missing `MinQuorumBps` silently defaulted. | Unset/non-positive supply or unset/out-of-range bps -> `QuorumDataMissing`; no status change. |
| #1004 | Tallies were combined with `saturating_add`; negative/overflowing tallies were not detected. | Negative or overflowing `votes_for`/`votes_against` -> `InvalidVoteAccounting`. (A `tally <= total_supply` check was deliberately **not** added: supply is a static seed and legitimate mints exceed it.) |
| #1006 | A missing proposal entry was indistinguishable from a never-created id (`ProposalNotFound`). | Ids `1..=ProposalCount` with a missing entry or mismatched `id` -> `ProposalStorageCorrupt`. Never-issued ids (0, > count) stay `ProposalNotFound`. Applies to `get_proposal`, `cast_vote`, `execute_proposal`, `veto_proposal`. `list_proposals` still skips missing entries (read-only view). |

**Rollout / upgrade.** Additive: new error codes 29-33 and one new storage key; no existing storage layout changes. Existing `Passed` proposals remain executable unless a newer same-kind proposal has executed. Before upgrading, confirm `get_gov_token_total_supply() > 0` and `get_min_quorum_bps()` in 1..=10,000 on the live deployment, otherwise active proposals will not finalize.

**Recovery.** `QuorumDataMissing`: restore the value through the ILN-gated setters, then re-call `execute_proposal` (proposal remains `Active`). `ProposalStorageCorrupt` / `InvalidVoteAccounting`: the proposal is frozen (cannot execute); restore the entry from the persistent-storage archive/TTL restore, or re-propose. `ProposalSuperseded`: re-propose.

**Tests.** `test_1000_*`, `test_1003_*`, `test_1004_*`, `test_1006_*` in `contracts/iln_governance/src/test.rs`.

---

## 4. Cross-references

- [`docs/governance.md`](governance.md) — full mechanics: proposal lifecycle, voting window, quorum/majority rules, veto functions, past decisions.
- [`docs/adr/ADR-005-governance-timelock.md`](adr/ADR-005-governance-timelock.md) — the "no timelock in v1" decision and its reasoning.
- [`docs/adr/ADR-009-quadratic-voting.md`](adr/ADR-009-quadratic-voting.md) — quadratic weight calculation, `isqrt`, the `AppliedVoteWeight` receipt, and the launch recommendation.
- [`docs/adr/ADR-012-governance-multisig-handoff.md`](adr/ADR-012-governance-multisig-handoff.md) — the four-phase authority handoff plan and its exit criteria.
- [`docs/adr/adr-008-multisig-admin.md`](adr/adr-008-multisig-admin.md) — the contract-level M-of-N multisig design (`multisig.rs`), not yet wired.
- [`docs/threat-model.md`](threat-model.md) — §E (governance): flash-loan analysis (E3), reentrancy notes, parameter-validation findings. Includes reviewer sign-off (Issue #811).
- [`docs/formal-verification.md`](formal-verification.md) — §14–15 governance snapshot & delegation invariants (Issue #810) and test coverage matrix.
- [`docs/disaster-recovery-multisig-signers.md`](disaster-recovery-multisig-signers.md) — recovery path for lost/compromised admin signer majority.
- [`docs/oracle-attack-economics.md`](oracle-attack-economics.md) — the economic-security counterpart this document mirrors in structure.
- [`docs/incident-response-runbook.md`](incident-response-runbook.md) — the operational procedure for acting on a governance takeover attempt in real time (invoking `veto_proposal` / `pause()`).
