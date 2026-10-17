# Governance Timelock Threat Model

**Scope:** `contracts/iln_governance` (`set_execution_delay`, `execute_proposal`, `veto_proposal`, `checkpoint_balance`, `cast_vote`) and [ADR-005](adr/ADR-005-governance-timelock.md).
**Issues:** #992, #994, #998, #1001. Complements [`threat-model.md`](threat-model.md) and [`governance-security-summary.md`](governance-security-summary.md).

## 1. What the timelock protects

A passed proposal is queued (`Active -> Passed`, `eta_ledger = sequence + delay`) and can execute only once `sequence >= eta_ledger` (RFC 1982 comparison, rollover-safe). The window lets token holders exit and lets veto signers react. The timelock is only as strong as: (a) the delay value actually applied, (b) the eta stamped on each proposal, (c) the identity/integrity of the proposal being moved, and (d) the voting weight that produced the pass.

## 2. Assets, actors, trust

| Actor | Trust |
|-------|-------|
| Admin (`set_execution_delay`, `migrate_legacy_execution_delay`) | Trusted until veto retirement; may only move the delay within `0..=518_400` (and `>= 17_280` after retirement). |
| ILN contract (`disable_veto_power*`, parameter setters) | Acts only via passed proposals. |
| Veto signers (multisig) | Can block `Active`/`Passed` proposals while veto power is enabled. |
| Token holder / flash-loan attacker | Untrusted; may borrow funds inside one transaction. |
| Anyone | May call `execute_proposal`, `rebase_legacy_proposal_eta`. |

## 3. Threats and controls

| ID | Threat | Control | Residual risk |
|----|--------|---------|---------------|
| T1 | Delay set to 0 (or never set) so a passed proposal executes at once. | Delay clamped to 17,280 ledgers after veto retirement; `migrate_legacy_execution_delay` persists it (#985, #992). | While veto is enabled the admin may still choose a short delay; the veto is the compensating control. |
| T2 | Legacy `Passed` proposal (`eta == 0`) skips a newly enabled timelock. | `LegacyDelayMigrationRequired` unless the zero-delay regime still applies; `rebase_legacy_proposal_eta` re-stamps (#992). | Rebase restarts the wait for that proposal (intended). |
| T3 | Eta computed across `u32::MAX` rollover stalls or bypasses. | `wrapping_add` + serial-number compare (#987); a wrap onto 0 becomes 1 (#992). | Test host cannot run at near-max sequences; covered by pure-function tests. |
| T4 | Admin retires veto with no timelock in place. | Effective-delay floor applies from the moment veto is retired. | Operators should still migrate first (playbook). |
| T5 | Stored proposer altered so the deposit refund/forfeit or veto acts on a different identity. | Proposer bound immutably at creation; mismatch refuses execute/queue/veto (#998). | Does not defend against a compromised contract upgrade key (out of scope). |
| T6 | Flash-loaned or late-refreshed balance mints or loses voting power. | Checkpoints must predate the proposal snapshot ledger by `MIN_VOTE_HOLD_LEDGERS`; weight is `min(checkpoint, live)` (#805, #1001). | Weight is capped by the checkpoint and the live balance, not a true historical balance: tokens moved between aged addresses after the snapshot can be counted twice. |
| T7 | Voter refreshes a checkpoint after a snapshot and is silently disenfranchised, or a missing snapshot silently uses "now". | Two-slot history (latest + prior), eligibility is bound to the proposal's snapshot ledger; missing snapshot fails closed `SnapshotLedgerMissing` (#1001). | Three refreshes inside one proposal's lifetime evict the proof; refresh sparingly. |
| T8 | Unbounded state growth. | One extra entry per proposal (`ProposalProposer`) and one per voter (`PriorBalanceCheckpoint`); no loops over history. | Linear in proposals/voters, as before. |

## 4. Out of scope

Compromise of a majority of governance tokens (governed by quorum/veto), contract upgrade authority, and token-contract bugs.

## 5. Rollout, recovery and upgrade impact

* Upgrade is storage-additive: new keys are optional, `GovernanceProposal` XDR is unchanged, new errors are 39-41.
* Order: deploy build → `migrate_legacy_execution_delay` → rebase legacy `Passed` proposals → (later) retire veto. See the [operations playbook](governance-operations-playbook.md#legacy-zero-delay-migration-runbook-issue-992).
* Rollback: previous build ignores the new keys; the migrated delay remains valid and can be changed with `set_execution_delay`.
* Behavior changes to expect: legacy `Passed` proposals may need a rebase; votes on proposals lacking a snapshot ledger now fail (`SnapshotLedgerMissing`).
