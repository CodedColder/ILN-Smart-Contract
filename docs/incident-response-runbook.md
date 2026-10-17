# Protocol-Wide Incident Response Runbook

**Status:** Draft — pending Security-lead sign-off on the mainnet launch checklist.
**Owner:** Security lead.
**Scope:** The top-level document coordinating incident response **across contracts, indexer, and notifications** during a live incident. It defines severity, roles, decision authority, communication, and the post-incident process, and delegates the mechanics of each recovery path to the component-specific runbooks in [§12](#12-component-runbooks-invoked-as-sub-procedures).

This runbook does **not** replace the component runbooks — it is the entry point that decides *which* of them to invoke, *who* decides, and *how the incident is communicated*.

---

## Table of contents

1. [When to use this runbook](#1-when-to-use-this-runbook)
2. [Severity classification](#2-severity-classification)
3. [Roles and responsibilities](#3-roles-and-responsibilities)
4. [Decision authority for `pause()`](#4-decision-authority-for-pause)
5. [The first 15 minutes](#5-the-first-15-minutes)
6. [Response by incident class](#6-response-by-incident-class)
7. [On-chain emergency actions](#7-on-chain-emergency-actions)
8. [Off-chain service incidents (indexer / notifications)](#8-off-chain-service-incidents-indexer--notifications)
9. [User communication](#9-user-communication)
10. [Recovery and re-opening](#10-recovery-and-re-opening)
11. [Post-incident review](#11-post-incident-review)
12. [Component runbooks invoked as sub-procedures](#12-component-runbooks-invoked-as-sub-procedures)

---

## 1. When to use this runbook

Invoke this runbook the moment **any** of the following is suspected, not confirmed:

- Funds at risk on-chain (accounting invariant violation, auth bypass, oracle manipulation, governance takeover path — see [mainnet-rollback-runbook.md §2](mainnet-rollback-runbook.md)).
- A governance proposal is executing or about to execute an action its guards should have blocked.
- The indexer or notifications service is serving materially wrong data or has been compromised.
- A secret (admin key material, API key, HMAC signing key, deployment secret) is exposed or suspected exposed.

For a **cosmetic** bug with no funds/auth/data-integrity impact, do not use this runbook — file an issue and follow the normal review process.

---

## 2. Severity classification

Aligned 1:1 with the severity table in [`SECURITY.md`](../SECURITY.md#severity-levels) and [`docs/security.md`](security.md). Incident severity drives the response timeline, who is paged, and whether `pause()` is on the table. Concrete SLO targets that define "degraded enough to declare an incident" are in [`docs/slos.md`](slos.md) — the SLO breach thresholds in that document replace subjective judgment during a stressful moment.

| Severity | Definition (from `SECURITY.md`) | Incident response |
|----------|--------------------------------|-------------------|
| **Critical** | Direct loss or theft of user funds, permanent protocol insolvency, or unauthenticated upgrade/admin takeover | Page **all leads** immediately. `pause()` is the default first action ([§4](#4-decision-authority-for-pause)). Public advisory within hours. Rollback runbook on standby. |
| **High** | Material fund risk, broad data integrity failure, secret exposure, or reliable service compromise | Page **Security lead + on-call IC + the owning component lead**. `pause()` considered, not automatic. Rotate exposed secrets immediately. Advisory within 24h. |
| **Medium** | Limited financial or operational impact, DoS with a recovery path, or scoped data exposure | On-call IC triages; owning component lead engaged. Fix on the Medium timeline (target 30 days). Advisory only if users are affected. |
| **Low** | Defense-in-depth issue, documentation security gap, low-impact information exposure | Normal issue queue, security label. No incident channel. |
| **Informational** | No immediate exploit path but useful for hardening | Backlog. |

The **Security lead** owns the final severity call. When leads disagree, escalate one level until agreement — err toward the higher severity.

---

## 3. Roles and responsibilities

Roles mirror [mainnet-rollback-runbook.md §3](mainnet-rollback-runbook.md#3-roles). One person may hold multiple roles in a small incident; **Incident Commander and Security lead should not be the same person** for Critical/High.

| Role | Responsibility during an incident |
|------|-----------------------------------|
| **Incident Commander (IC)** | Owns the incident. Coordinates all roles, is the single source of truth for status, runs the timeline, decides when to hand off. Whoever is on-call when the alert fires assumes IC until handoff (see [monitoring-runbook.md §4](monitoring-runbook.md#4-on-call-escalation)). |
| **Security lead** | Confirms the vulnerability is real, assigns severity ([§2](#2-severity-classification)), signs off before any public communication, owns the eventual advisory and CVE-equivalent. |
| **Contracts lead** | Confirms on-chain root cause, prepares rollback WASM or new deployment, verifies contract state before and after any action. |
| **Release lead** | Executes on-chain commands, holds admin-key access per [access-control.md](access-control.md), drives the deployment/rollback runbooks. |
| **Infrastructure lead** | Owns indexer and notifications incidents ([§8](#8-off-chain-service-incidents-indexer--notifications)) — restore, replay, failover, scaling. |
| **Community lead** | Owns all user communication ([§9](#9-user-communication)) and the public incident channel. Nobody else posts externally. |

Decision authority for the single highest-stakes action — `pause()` — is called out separately in [§4](#4-decision-authority-for-pause).

---

## 4. Decision authority for `pause()`

`pause()` halts every mutating entry point on `invoice_liquidity` immediately — `submit_invoice`, `fund_invoice`, `mark_paid`, `claim_default`, `appeal_default`, batch submit, queue resolution ([access-control.md §7](access-control.md#7-pause-behavior--cross-contract-scope), confirmed by `tests_pause_checks.rs`). It is instant, admin-only, no timelock, no rate limit. `unpause()` reverses it. `get_contract_stats` and the read views (including [`get_protocol_status`](#7-on-chain-emergency-actions)) keep working while paused.

There are **two authorization paths** to a pause, and which one is available depends on how far the [ADR-012](adr/ADR-012-governance-multisig-handoff.md) handoff has progressed:

| Path | Who can trigger | When to use |
|------|-----------------|-------------|
| **Single admin key** (`pause()`) | The Release lead, on the IC's instruction, using the admin key per [access-control.md](access-control.md). If the admin is a Stellar account-level multisig, enough signer weight to meet the account threshold. | Default today. Any confirmed or strongly-suspected **Critical**; **High** at the IC's discretion. |
| **Contract-level multisig** (`propose_pause` → `sign_proposal` → `execute_proposal`) | Configured multisig signers, per [adr-008-multisig-admin.md](adr/adr-008-multisig-admin.md) and the disaster-recovery procedure in [disaster-recovery-multisig-signers.md](disaster-recovery-multisig-signers.md). | Once the multisig is wired (ADR-012 Phase 1) and is the operative emergency mechanism. Requires `threshold` signers to agree — slower, use only when the single-admin path is unavailable or compromised. |

**Decision rule.** For a **Critical** incident the IC authorizes `pause()` on the single-admin path *without waiting for consensus* — a false pause is recoverable (`unpause()`), a drained protocol is not. The IC records the decision (timestamp, trigger, who authorized) in the incident log before execution. For **High**, the IC and Security lead must both agree.

**Governance takeover path.** If the threat is a malicious governance proposal (not a contract bug), the first action is `veto_proposal(proposal_id, reason_hash)` — see [governance-security-summary.md §3.1](governance-security-summary.md#31-no-execution-timelock) and [§7](#7-on-chain-emergency-actions) — which blocks that one proposal without halting the whole protocol. `pause()` is the backstop if the veto is unavailable or the proposal has already executed.

---

## 5. The first 15 minutes

1. **Declare.** Whoever noticed opens the incident channel, states severity (best guess), and names themselves IC until handoff.
2. **Page.** Per [§2](#2-severity-classification) — Critical pages all leads; High pages Security + IC + component lead. Use the escalation path in [monitoring-runbook.md §4](monitoring-runbook.md#4-on-call-escalation).
3. **Snapshot state.** Capture, with timestamps:
   - `get_protocol_status()` on `invoice_liquidity` (paused flag, last pause timestamp, multisig config, oracle circuit state) — or the public [`/protocol-status`](indexer-operations.md) indexer endpoint if the RPC is slow.
   - `get_contract_stats()`, the affected invoice(s) via `get_invoice`, `get_recent_admin_actions`.
   - Indexer `/health`, notifications `/health`, current ledger from Horizon.
4. **Contain, if Critical.** IC authorizes `pause()` ([§4](#4-decision-authority-for-pause)). Do not wait for full root-cause.
5. **Assign workstreams.** Root cause (Contracts/Security lead), communication draft (Community lead, held until Security sign-off), evidence collection (IC).

Do **not** post publicly, do **not** deploy a fix, and do **not** `unpause()` in the first 15 minutes.

---

## 6. Response by incident class

| Incident class | First action | Runbook to invoke |
|----------------|--------------|-------------------|
| On-chain fund-at-risk / auth bypass, actively exploited | `pause()` immediately ([§4](#4-decision-authority-for-pause)) | [mainnet-rollback-runbook.md](mainnet-rollback-runbook.md) — Emergency path |
| On-chain bug, not yet exploited | Assess whether an expedited upgrade or a governance parameter change fixes it before pausing | [mainnet-rollback-runbook.md §4 (decision framework)](mainnet-rollback-runbook.md) → [upgrade-guide.md](upgrade-guide.md) |
| Malicious / erroneous governance proposal | `veto_proposal(proposal_id, reason_hash)` | [governance-security-summary.md](governance-security-summary.md), [governance.md §8](governance.md#8-admin-veto-power) |
| Oracle compromise / manipulation | `reset_oracle_circuit` is not the fix — `remove_oracle` / `remove_token_oracle`, or `pause()` as a blunt stopgap | [oracle-attack-economics.md §5.2, §8 rec 6](oracle-attack-economics.md) |
| Admin key / signer majority lost or compromised | Follow the recovery procedure; note there is no on-chain escape hatch today | [disaster-recovery-multisig-signers.md](disaster-recovery-multisig-signers.md) |
| Indexer data loss / corruption | Pause ingestion, choose restore / replay / resync | [indexer-incident-runbook.md](indexer-incident-runbook.md) |
| Notifications outage / abuse / injection | Check circuit-breaker state, delivery latency, HMAC failure rate | [notifications-operations.md](notifications-operations.md), [monitoring-runbook.md](monitoring-runbook.md) |
| Secret exposure | Rotate the specific secret immediately; assume everything it could reach is compromised | [deployment-secrets.md](deployment-secrets.md), [notifications-operations.md](notifications-operations.md) |

---

## 7. On-chain emergency actions

| Action | Call | Authority | Reverses / follows up with |
|--------|------|-----------|----------------------------|
| Halt the protocol | `pause()` | Admin ([§4](#4-decision-authority-for-pause)) | `unpause()` after [§10](#10-recovery-and-re-opening) |
| Halt via multisig | `propose_pause` → `sign_proposal` → `execute_proposal` | `threshold` multisig signers | `propose_unpause` … |
| Block one governance proposal | `veto_proposal(proposal_id, reason_hash)` | Veto signers (multisig-gated, Issue #642) | Proposal cannot be executed; no reversal needed |
| Remove a compromised oracle | `remove_oracle(feed_type)` / `remove_token_oracle(feed_type, token)` | Admin, or governance proposal | `register_oracle` a vetted replacement ([oracle-provider-vetting.md](oracle-provider-vetting.md)) |
| Reset a tripped oracle circuit (only after the oracle is confirmed healthy) | `reset_oracle_circuit(feed_type, token)` | Governance-gated | — |
| Observe status without mutating | `get_protocol_status()`, `get_contract_stats()`, `get_recent_admin_actions(limit)` | Anyone | — |
| Roll back / redeploy | See runbook | Release lead + Contracts lead | [mainnet-rollback-runbook.md](mainnet-rollback-runbook.md) |

`get_protocol_status()` returns `{ paused, last_pause_timestamp, admin, multisig_configured, multisig_threshold, multisig_signer_count, oracle_circuit_tripped, oracle_circuits_tripped }` — the operationally-relevant state in one call, also exposed publicly by the indexer at `/protocol-status` for the community "is it paused, and why" question.

---

## 8. Off-chain service incidents (indexer / notifications)

The indexer and notifications services never hold funds and never authorize on-chain actions — an incident in either is at most **High** (data integrity / service compromise), usually **Medium**. On-chain operations are unaffected; say so in every user communication.

- **Indexer:** detection via `/health` (`status: degraded`, ledger lag, DB failures) and reconciliation alerts. Recovery paths — restore from snapshot, replay from checkpoint, full resync — and their estimated recovery times are in [indexer-incident-runbook.md](indexer-incident-runbook.md). Monitoring thresholds are in [monitoring-runbook.md §2](monitoring-runbook.md#2-recommended-external-monitoring).
- **Notifications:** circuit-breaker open rate, webhook failure ratio, delivery latency, HMAC verification failures — see [notifications-operations.md](notifications-operations.md). Retain HMAC failures for abuse review.
- **Correlation across services:** every incident-relevant log line in both services carries a `correlationId` per [observability-standards.md](observability-standards.md) — use it to trace one event from ingestion through to delivery when reconstructing a timeline.

---

## 9. User communication

The **Community lead** owns all external communication. Nothing goes out until the **Security lead** has signed off on severity and wording. Post to every channel in [support-channels.md](support-channels.md) and pin the incident channel.

**Cadence:** first post within the [§2](#2-severity-classification) timeline for the severity; updates at least hourly for Critical/High until resolved, then a final "resolved" post.

### Template — on-chain incident, protocol paused

> **[ILN Incident — <severity>]** We have paused the ILN protocol at <UTC time> as a precaution while we investigate <one-line, non-exploitable description>. No new invoices can be submitted or funded while paused. Existing on-chain balances are unchanged. We will post an update by <UTC time>. Status: <link>.

### Template — on-chain incident, resolved

> **[ILN Incident — Resolved]** The issue identified at <time> has been <fixed / mitigated>. The protocol was <unpaused / redeployed> at <time>. <One line on impact: e.g. "No user funds were lost." / "N invoices totaling $X were affected; see the post-incident review.">. Full write-up: <link to post-incident review>.

### Template — indexer / dashboard degradation (reuse from [indexer-incident-runbook.md §4](indexer-incident-runbook.md#4-communication))

> We are currently experiencing degraded performance with the ILN dashboard data. **On-chain operations are unaffected and smart contracts are fully operational.** Our team is restoring the indexer service. Expected resolution in <time>.

Never publish exploit details, a working reproduction, or the specific vulnerable code path until a fix is deployed and (for Critical) users have had time to act. Follow the disclosure timeline in [`docs/security.md`](security.md).

---

## 10. Recovery and re-opening

1. **Root cause confirmed and fix verified** by the Contracts lead (on-chain) or Infrastructure lead (off-chain), on testnet where possible.
2. **State reconciled** — `get_contract_stats` / affected invoices match expectations; indexer reconciliation clean.
3. **Security lead sign-off** to re-open.
4. **`unpause()`** (or `propose_unpause` → threshold sign → `execute_proposal`). Record the timestamp and authorizer in the incident log.
5. **Watch** — keep the incident channel open and monitoring at elevated sensitivity for at least 24h; be ready to re-`pause()`.
6. **Public "resolved" post** ([§9](#9-user-communication)).

If the fix requires a contract change, do not `unpause()` the buggy contract — follow the upgrade or redeploy path in [mainnet-rollback-runbook.md](mainnet-rollback-runbook.md) and re-open only the fixed deployment.

---

## 11. Post-incident review

Within **5 business days** of resolution (Critical/High), the Security lead runs a blameless review using [`docs/postmortem-template.md`](postmortem-template.md). It must cover:

- Timeline (detection → containment → root cause → fix → re-open), with `correlationId`s where services were involved.
- Root cause and the contributing factors (why it wasn't caught earlier).
- Impact — funds, users, downtime — stated precisely.
- What worked / what didn't in this runbook and the component runbooks.
- Action items with owners and due dates, tracked as GitHub issues; feed any that change launch readiness back into [mainnet-launch-checklist.md](mainnet-launch-checklist.md).

The review is published (redacting only live-exploit detail) once all Critical action items are closed.

---

## 12. Component runbooks invoked as sub-procedures

This runbook is the coordinator. Each of the following is a **sub-procedure** invoked from the sections above — none of them decides severity, roles, or communication on their own.

| Runbook | Invoked from | Covers |
|---------|--------------|--------|
| [mainnet-rollback-runbook.md](mainnet-rollback-runbook.md) | [§6](#6-response-by-incident-class), [§7](#7-on-chain-emergency-actions), [§10](#10-recovery-and-re-opening) | Early-launch rollback decision framework: pause, veto, in-place WASM rollback, full redeploy. |
| [disaster-recovery-multisig-signers.md](disaster-recovery-multisig-signers.md) | [§4](#4-decision-authority-for-pause), [§6](#6-response-by-incident-class) | Lost / compromised admin signer majority — prevention (pre-mainnet) and the (limited) recovery options. |
| [adr/adr-008-multisig-admin.md](adr/adr-008-multisig-admin.md) · [adr/ADR-012-governance-multisig-handoff.md](adr/ADR-012-governance-multisig-handoff.md) | [§4](#4-decision-authority-for-pause) | The contract-level M-of-N multisig design and the phased plan for it to become the emergency mechanism. |
| [indexer-incident-runbook.md](indexer-incident-runbook.md) | [§6](#6-response-by-incident-class), [§8](#8-off-chain-service-incidents-indexer--notifications) | Indexer data-loss / corruption: restore from backup, replay from checkpoint, full resync, and recovery-time estimates. |
| [notifications-operations.md](notifications-operations.md) | [§6](#6-response-by-incident-class), [§8](#8-off-chain-service-incidents-indexer--notifications) | Notifications outage / abuse: circuit breaker, delivery latency, webhook failure ratio, HMAC failures. |
| [monitoring-runbook.md](monitoring-runbook.md) | [§3](#3-roles-and-responsibilities), [§5](#5-the-first-15-minutes), [§8](#8-off-chain-service-incidents-indexer--notifications) | What `/health` checks, alert thresholds, log retention, on-call escalation. |
| [observability-standards.md](observability-standards.md) | [§8](#8-off-chain-service-incidents-indexer--notifications), [§11](#11-post-incident-review) | Structured-logging format and the `correlationId` scheme used to correlate logs across services. |
| [governance-security-summary.md](governance-security-summary.md) · [governance.md §8](governance.md#8-admin-veto-power) | [§4](#4-decision-authority-for-pause), [§6](#6-response-by-incident-class), [§7](#7-on-chain-emergency-actions) | Governance takeover response: `veto_proposal`, quorum caveats, veto sunset state. |
| [oracle-attack-economics.md](oracle-attack-economics.md) | [§6](#6-response-by-incident-class), [§7](#7-on-chain-emergency-actions) | Oracle compromise: removal paths, exposure-window math, why `reset_oracle_circuit` is not the fix. |
| [upgrade-guide.md](upgrade-guide.md) | [§6](#6-response-by-incident-class), [§10](#10-recovery-and-re-opening) | Contract upgrade / in-place rollback mechanics for the non-emergency fix path. |
| [deployment-secrets.md](deployment-secrets.md) | [§6](#6-response-by-incident-class) | Secret custody and the rotation procedure on exposure. |
| [postmortem-template.md](postmortem-template.md) | [§11](#11-post-incident-review) | The blameless post-incident review structure. |


## Audit Findings
If responding to a newly disclosed audit finding, consult the [Contract Pause Authority Policy](audit-finding-pause-policy.md) to determine if a full pause is warranted.