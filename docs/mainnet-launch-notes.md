# Mainnet Launch Notes & Known Limitations

**Audience:** freelancers, payers, liquidity providers (LPs), and integrators
moving from ILN testnet to ILN mainnet.

**Status:** Draft — tracks the "User-facing launch notes" item on the
[Mainnet Launch Checklist](mainnet-launch-checklist.md). This document is
finalized (dates and contract IDs filled in) as part of mainnet deployment
sign-off in the [Mainnet Deployment Runbook](mainnet-deployment-runbook.md).

---

## 1. What changes when you move from testnet to mainnet

- **Real funds, real risk.** Mainnet invoices are funded with real tokens
  (XLM, USDC, EURC, and other approved SACs). Testnet activity used faucet
  tokens with no economic value — mistakes on mainnet are not reversible the
  way a testnet reset is.
- **New contract IDs.** Mainnet contracts are freshly deployed and have
  different addresses than testnet. Do not reuse testnet contract IDs, SDK
  config, or bookmarked explorer links — see the
  [Mainnet Contract Addresses](../README.md#mainnet-contract-addresses) table
  in the README once it is published post-deployment.
- **No state carries over from testnet.** Testnet invoices, reputation
  scores, LP positions, and governance proposals are **not** migrated to
  mainnet. Mainnet starts from a clean, empty protocol state. Testnet remains
  available as an ongoing sandbox for integration testing, separate from
  mainnet.
- **Network configuration.** Point your wallet, SDK, and CLI at the Stellar
  mainnet network passphrase and a mainnet-capable Horizon/Soroban RPC
  endpoint. Testnet-configured tooling will not resolve mainnet contracts.

## 2. Known limitations at mainnet launch

These are accepted, documented risks for the initial mainnet release — not
oversights. Each links to the tracked checklist item so you can follow when
it changes.

| Limitation | Detail | Tracking |
|---|---|---|
| **No insurance pool coverage yet** | The insurance pool contract exists and is documented (see [Insurance Pool Design](insurance-pool-design.md) and [Launch Parameters](insurance-pool-launch-parameters.md)), but pool readiness (test coverage, audit, deployment, SDK integration) is not yet complete. At launch, **LPs funding invoices have no default-protection coverage** — a defaulted invoice is a full loss to the funding LP(s) until the pool is enabled by a follow-up governance action. | [Mainnet Launch Checklist — Insurance pool readiness](mainnet-launch-checklist.md#contracts) |
| **Admin authority is still centralized** | Production admin functions (pause, fee updates, oracle configuration, upgrades) are controlled by a single admin key at launch, not yet a multi-sig or DAO-governed account. This is more centralized than the intended end-state described in [Governance](governance.md) and [Access Control](access-control.md). Treat the admin key the way you would any centralized operator during this period. | [Mainnet Launch Checklist — Multi-sig admin configured](mainnet-launch-checklist.md#contracts) |
| **Upgrade path is tested but not yet exercised live on mainnet** | The v1→v2 state migration logic is verified against simulated, mainnet-shaped state (see [Upgrade Guide](upgrade-guide.md)), but no contract upgrade has yet been performed against live mainnet data. Early upgrades carry more operational risk than later ones. | [Mainnet Launch Checklist — Upgrade path tested](mainnet-launch-checklist.md#contracts) |
| **Governance veto power is active** | The Admin retains veto power over governance proposals (see [Governance](governance.md)) until explicitly disabled. Proposal outcomes are not yet fully trustless. | [Access Control — Governance Contract](access-control.md#4-governance-contract--permission-matrix) |
| **Limited operational history** | Monitoring, alerting, and incident-response tooling are in place (see [Monitoring Runbook](monitoring-runbook.md)) but have no mainnet track record yet. Response times during the first weeks may be slower than the steady-state target while on-call processes are proven out under real load. | [Mainnet Launch Checklist — Infrastructure](mainnet-launch-checklist.md#infrastructure) |

## 3. What is NOT changing

- **Contract logic and invoice lifecycle** are identical to what you tested
  on testnet — submit, fund, pay, dispute, appeal, and reputation mechanics
  behave the same way (see [Architecture](Architecture.md)).
- **Fee structure and discount-rate mechanics** launch with the same
  defaults validated on testnet, unless a governance proposal changes them
  before launch.
- **SDK and CLI interfaces** are unchanged — only the network configuration
  and contract IDs differ. See the [SDK Integration Guide](sdk-integration.md).

## 4. Action items for existing testnet users

1. **Do not assume any testnet balance, invoice, or reputation score
   transfers to mainnet.** Rebuild reputation on mainnet from real activity.
2. **Re-verify contract IDs** in any script, bot, or integration before
   pointing it at mainnet — copy them from the published
   [Mainnet Contract Addresses](../README.md#mainnet-contract-addresses)
   table, never from memory or an old `.env` file.
3. **Start with small amounts.** Given the limitations above (no insurance
   coverage, centralized admin), size initial mainnet activity accordingly
   while the protocol builds a live track record.
4. **Watch the known-limitations table above** — each row is expected to
   close out over time; the linked checklist rows reflect current status.

## 5. Where to get help

- **Bugs and integration questions:** open an issue using the
  [issue templates](../.github/ISSUE_TEMPLATE).
- **Security reports:** follow the responsible-disclosure process in
  [Security Policy](security.md) — do not open a public issue for a
  suspected vulnerability.
- **Incidents:** status and post-incident communication follows the process
  in the [Monitoring Runbook](monitoring-runbook.md).

## 6. Change history

User-facing changes between releases are tracked in the
[CHANGELOG](../CHANGELOG.md). See the curated
[Mainnet Launch Release](../CHANGELOG.md#mainnet-launch) section for a
summary of deliverables across all development and audit-hardening batches.
This document covers the one-time testnet-to-mainnet transition; it is not
updated per-release the way the changelog is.
