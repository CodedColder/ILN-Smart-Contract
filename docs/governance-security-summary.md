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
| 1 | **Quadratic voting analysis** | `QuadraticVotingEnabled` flag (default `false`), governance-toggled. When on, `cast_vote` weight is `isqrt(own_balance + delegated_weight)` — floor integer sqrt via binary search over `i128`, `#![no_std]`-safe. Modeling against a synthetic power-law holder distribution showed it compresses a ~50% whale dominance to ~13% without disenfranchising large holders. **Recommendation: enable at mainnet launch** given the concentrated early token supply. | ✅ Implemented; ⚠️ off by default — enabling is itself a governance action | [ADR-009](adr/ADR-009-quadratic-voting.md) |
| 2 | **Delegation depth & cost bounds** | Transitive delegation is implemented (Issue #64). **Cycle detection and a hard maximum depth of 10 hops** prevent infinite loops and bound the per-vote traversal cost. Quadratic mode sums own + delegated balance *before* the square root (not sqrt-per-component), which removes the incentive to split power across many delegate chains — sqrt is concave, so splitting-then-summing would otherwise inflate weight. | ✅ Bounded (depth 10, cycle-checked) | [`governance.md` §9 "Delegation"](governance.md#9-security-considerations); [ADR-009 "Alternatives Considered"](adr/ADR-009-quadratic-voting.md#alternatives-considered) |
| 3 | **Snapshot timing guarantees** | Voting power is pinned to **per-proposal checkpoints**. The proposer's balance is checkpointed at `create_proposal`; every other voter's balance is checkpointed on their *first* vote for that proposal and reused for its duration (lazy snapshot, Issue #738). Later balance changes cannot inflate weight on an already-checkpointed proposal. Double-vote is blocked by a `HasVoted(proposal_id, voter)` receipt in temporary storage (TTL ≈ 4 days — the 3-day window plus a 1-day buffer). The applied weight (linear or quadratic) is recorded per-voter-per-proposal in an `AppliedVoteWeight` receipt for after-the-fact auditability. | ✅ Checkpoints prevent post-vote inflation; ⚠️ **residual flash-loan risk** — see §3 | [`governance.md` §2, §4](governance.md#2-governance-token-and-voting-power); [threat-model.md §E3](threat-model.md); [ADR-009 "Consequences"](adr/ADR-009-quadratic-voting.md#consequences) |
| 4 | **Spam resistance (proposals & votes)** | **Vote** spam: bounded by the per-voter double-vote receipt and by needing non-zero voting power (`NoVotingPower` rejects 0-balance callers). **Proposal** spam: the static `MinProposalBalance` holding gate plus a forfeitable `MinProposalDeposit` escrow (Issue #814, default `0` = disabled for backwards compatibility, governance-settable via `set_min_proposal_deposit`). `create_proposal` escrows the deposit, `execute_proposal` refunds it on `Passed`/`Executed` and forfeits it on `Rejected`/expired-without-quorum, `veto_proposal` forfeits on `Vetoed`. Forfeits go to the governance-configurable `ProposalDepositSink` treasury address (not the insurance pool — spam penalties are treasury revenue, mixing them would distort pool coverage accounting); when unset, forfeits stay locked in the governance contract. Events `ProposalDepositEscrowed` / `ProposalDepositRefunded` / `ProposalDepositForfeited`; settlement is idempotent via `ProposalDepositSettled` (double-refund safe). | ✅ Implemented (deposit `0` by default — governance must set a non-zero value to activate); ⚠️ residual — a wallet above the static gate can still spam while the deposit is `0` | [`governance.md` §9 "Double-proposal spam"](governance.md#9-security-considerations); [ADR-009 "set_min_proposal_balance"](adr/ADR-009-quadratic-voting.md) |
| 5 | **Quorum consistency** | Quorum = `total_supply * min_quorum_bps / 10_000` (default 10%, governance-configurable via `min_quorum_bps`). **`total_supply` is a caller-supplied argument to `execute_proposal`, not read on-chain from the token contract.** An incorrect (or adversarially low) value distorts the quorum check. An attacker holding >10% of supply can also reach quorum alone. | ⚠️ **Accepted risk at launch** — mitigations are "raise `min_quorum_bps` via proposal" and "future iterations should read supply on-chain"; the admin veto is the backstop against a proposal passed under a falsified quorum | [`governance.md` §6 (note)](governance.md#6-quorum-and-majority-rules); [`governance.md` §9 "Quorum attacks"](governance.md#9-security-considerations) |
| 6 | **Veto sunset roadmap** | The admin veto (`veto_proposal`, now multisig-gated per Issue #642) is an emergency brake for the early phase. `disable_veto_power()` is a **one-way switch** callable only by the ILN contract (i.e. via a passed governance proposal); after it, `veto_proposal` returns `VetoPowerDisabled`. ADR-012 sequences the retirement: **Phase 1** wire the contract-level multisig (ADR-008) → **Phase 2** expand governance's parameter authority → **Phase 3** implement and activate a timelock on `execute_proposal` → **Phase 4** retire the veto (requires Phases 1–3 done, the multisig proven as the operative emergency mechanism, and a governance vote). | ⚠️ **Not started** — veto is live, no timelock, handoff is a documented plan with no phase complete; `disable_veto_power()` **must be called via governance vote before mainnet** but the prerequisites for doing so safely are not yet met | [`governance.md` §8](governance.md#8-admin-veto-power); [ADR-005](adr/ADR-005-governance-timelock.md); [ADR-012 §"Decision" (phases)](adr/ADR-012-governance-multisig-handoff.md) |

---

## 3. Accepted risks at launch (explicit)

These are known, documented, and consciously carried into mainnet. They are listed here so a reviewer does not have to reconstruct them from six sources.

### 3.1 Execution timelock (partially implemented)

> **Update (#995/#999/#1002/#1005):** the contract now enforces an `eta_ledger` timelock and monotonic proposal states — see the [ADR-005 addendum](adr/ADR-005-governance-timelock.md#addendum-995-999-1002-1005-implemented-timelock-invariants). The default `ExecutionDelay` is still `0`, so mainnet must set a non-zero delay (admin `set_execution_delay`, capped at 518,400 ledgers) before the veto is retired. **Rollout:** no storage migration (no struct changes; new error codes 29–31 are additive). **Recovery:** if a bad delay is set, lower it with `set_execution_delay`; already-`Passed` proposals keep their original eta, and any `Passed` proposal can still be vetoed while the veto is enabled.
>
> **Known follow-ups, not fixed here:** (a) `execute_proposal` returns `Err` for quorum/majority rejection, and Soroban rolls back state on `Err`, so `Rejected` status and the deposit forfeit do not persist on-chain (tests bypass this with `as_contract`); (b) `delegate_votes` after voting can let the same balance be counted by the delegate; (c) the delay is admin-key controlled.

Original accepted-risk text (pre-timelock):


`execute_proposal` runs in the same transaction as the call that triggers it, immediately after the voting window closes ([ADR-005](adr/ADR-005-governance-timelock.md), [`governance.md` §7](governance.md#7-execution-mechanics)). The stated substitute is the admin veto, which can block any `Active`/`Passed` proposal. **Accepted because:** at launch the token distribution is concentrated enough that a long timelock would slow necessary parameter tuning without a real decentralization benefit, and the veto covers the "malicious proposal" case. **Exit:** ADR-012 Phase 3 implements and activates a real timelock via governance upgrade *before* the veto is retired.

### 3.2 Flash-loan vote manipulation if the governance token becomes composable

The lazy snapshot ([§2, finding 3](#2-findings-by-governance-hardening-area)) checkpoints a voter's balance at *their first vote*, not at proposal creation. If a Stellar lending protocol offers flash loans of the governance token, an attacker can borrow a large amount, `cast_vote` in the same transaction, have the inflated balance permanently recorded as their proposal weight, and repay — all atomically ([threat-model.md §E3](threat-model.md)). Quadratic voting reduces but does not eliminate this. **Accepted because:** the governance token is not currently flash-loanable anywhere, and Soroban lacks a native historical-balance proof to fix it cleanly. **Exit:** if the token becomes widely flash-loanable, the protocol must move to staking-based governance (lock tokens in escrow for the proposal's duration) or integrate a historical-balance oracle — this is a documented trigger, not an open-ended TODO.

### 3.3 Caller-supplied `total_supply` in the quorum check

See [§2, finding 5](#2-findings-by-governance-hardening-area). **Accepted because:** the practical exposure is a proposal passing under an understated quorum, which the admin veto can still block during the `Passed` state; reading supply on-chain is deferred to a future governance iteration. It is *not* acceptable to retire the veto (§3.4) while this is open.

### 3.4 Veto and multisig handoff incomplete

Per [§2, finding 6](#2-findings-by-governance-hardening-area) and [ADR-012](adr/ADR-012-governance-multisig-handoff.md): the contract-level multisig (ADR-008) is designed but not wired into `lib.rs`; there is no on-chain break-glass recovery key ([disaster-recovery-multisig-signers.md §2](disaster-recovery-multisig-signers.md)); the production admin's Stellar-account signer list is empty. **Accepted for testnet.** For mainnet these are prerequisites, not a checklist to run the day something breaks — a majority-loss lockout with no prior preparation is unrecoverable on the deployed contract.

---

## 5. Adversarial Test Coverage (Issue #813)

A dedicated test suite combines all governance attack vectors to verify the system rejects or economically deters combined attacks, not just isolated ones:

### Test Cases Implemented

| Test | Attack Vector | Outcome | Issue |
|---|---|---|---|
| `test_adversarial_flash_loan_attack()` | Flash-loan borrow, vote in same transaction, repay | ✅ Rejected: vote weight pinned at proposal creation (before borrow) | #813 |
| `test_sybil_proposal_spam()` | Create many Sybil addresses, spam proposals | ✅ Rejected: MinProposalBalance gate prevents spam | #813 |
| `test_delegation_cycle_prevention()` | Create delegation cycles (A→B→C→A) to confuse vote tallies | ✅ Rejected: cycle detection guard in `delegate_votes` | #813 |
| `test_delegation_depth_bound()` | Create deep delegation chain (>10 hops) to increase vote resolution cost | ✅ Rejected: MaxDelegationDepth = 10 enforced | #813 |
| `test_combined_adversarial_attack()` | Flash-loan + Sybil + delegation + voting in sequence | ✅ Rejected: honest voters outvote combined attack due to snapshot + quorum mechanics | #813 |
| `test_quorum_manipulation_accepted_risk()` | Undersupply `total_supply` argument to lower quorum threshold | ⚠️ Accepted risk: admin veto is interim backstop until on-chain supply read | #813 |

**Test location:** [`contracts/tests/tests_adversarial_governance.rs`](../contracts/tests/tests_adversarial_governance.rs)

**Coverage:** Each test demonstrates both the attack mechanism and the mitigation that prevents it, with comments explaining why the attack fails.

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
