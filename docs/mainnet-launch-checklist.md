# Mainnet Launch Checklist

**Status:** Historical Reference — Superseded by audit-readiness-dashboard.md

This checklist has been reconciled with `pre-audit-checklist.md` and merged into [`audit-readiness-dashboard.md`](audit-readiness-dashboard.md), which is now the single authoritative tracking source. This document is retained for historical reference only.

Status values are maintained manually during planning and automatically refreshed for rows that link to GitHub issues when those issues are closed or reopened.

Status legend: `Not started`, `In progress`, `Blocked`, `Complete`.

## Security

| Item | Description | Owner | Status | Link |
|------|-------------|-------|--------|------|
| External security audit | Complete an external audit of all Soroban contracts, deployment scripts, SDK transaction builders, indexer APIs, and notifications webhooks. | Security lead | Complete | [#298](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues/298) |
| Coverage thresholds met | Enforce and publish coverage thresholds for contracts, SDK, indexer, and notifications before launch. | QA lead | In progress | [CI workflow](../.github/workflows/ci.yml) |
| Fuzz tests run | Run the `iln_fuzz` crate and contract property suites against launch candidates and archive results. | Contracts lead | In progress | [contracts/fuzz](../contracts/fuzz) |
| Threat model reviewed | Review protocol threats, update mitigations, and record unresolved accepted risks. | Security lead | In progress | [Threat Model](threat-model.md) |
| Security policy complete | Publish component-specific reporting, response, safe-harbor, and severity guidance. | Security lead | Complete | [#299](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues/299) |

## Contracts

| Item | Description | Owner | Status | Link |
|------|-------------|-------|--------|------|
| Upgrade path tested | Test upload, deploy, migration, rollback decision points, and post-upgrade smoke checks. | Contracts lead | In progress | [Upgrade Guide](upgrade-guide.md) |
| Multi-sig admin configured | Configure production admin as a multi-sig account or equivalent governance-controlled authority. | Governance lead | Not started | [Access Control](access-control.md) |
| Storage layout frozen | Confirm storage keys, schema, and migration compatibility are launch-ready. | Contracts lead | In progress | [Storage Layout Freeze Sign-off](storage-layout.md#10-storage-layout-freeze--sign-off-gate-issue-651) |
| Insurance pool readiness | Ensure insurance pool contract has test coverage >= 95%, completed security audit, deployment verification, and SDK integration. | Contracts lead | Not started | [Insurance Pool](../contracts/insurance_pool) |
| Mainnet deployment runbook | Dry-run every deployment command and record final runbook approvals. | Release lead | In progress | [Mainnet Deployment Runbook](mainnet-deployment-runbook.md) |
| Contract IDs published | Publish verified mainnet contract IDs and SAC addresses after deployment. | Release lead | Not started | [README](../README.md) |
| Staged rollout caps configured | Set initial per-invoice and per-token volume caps via governance before opening to real funds; raise over time. | Governance lead | In progress | [#655](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues/655) |

## Infrastructure

| Item | Description | Owner | Status | Link |
|------|-------------|-------|--------|------|
| Indexer deployed | Deploy production indexer with backup, restore, and replay procedures. | Infrastructure lead | In progress | [indexer](../indexer) |
| Monitoring configured | Configure health checks, alerting, log retention, and on-call routing for indexer and notifications. | Infrastructure lead | In progress | [monitoring-runbook.md](monitoring-runbook.md) |
| SLOs documented and monitored | Define and monitor Service Level Objectives for indexer and notifications, tied to specific alerting signals. | Infrastructure lead | Not started | [SLOs](slos.md) |
| Synthetic canary monitoring deployed | Deploy a synthetic transaction monitor that periodically performs end-to-end invoice lifecycle against mainnet. | Infrastructure lead | Not started | [#777](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues/777) |
| Notifications deployed | Deploy webhook/email notifications with HMAC signing, rate limiting, and SSRF controls verified. | Infrastructure lead | In progress | [notifications](../notifications) |
| Incident response runbook | Publish escalation, rollback, advisory, and user-communication steps. | Security lead | In progress | [Incident Response Runbook](incident-response-runbook.md) |
| Deployment secrets reviewed | Confirm production secrets follow an approved, reviewed custody path distinct from testnet's GitHub Actions secret. | Release lead | In progress | [Deployment Secret Management](deployment-secrets.md) |
| Mainnet rollback runbook rehearsed | Publish and rehearse the early-launch rollback decision procedure (pause, veto, in-place rollback, full redeploy) before mainnet launch. | Release lead | In progress | [#657](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues/657) |
| Game-day exercise completed | Run a structured game-day exercise validating the incident response runbook and component runbooks against a multi-failure scenario on testnet. | Security lead | Not started | [Game-Day Exercise Plan](game-day-exercise-plan.md) |
| Alert-to-incident-channel integration verified | Verify that SLO-breach alerts actually reach the configured incident channel (Slack/PagerDuty) within the detection window. | Infrastructure lead | Not started | [#779](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues/779) |

## Documentation

| Item | Description | Owner | Status | Link |
|------|-------------|-------|--------|------|
| Local development guide complete | Verify a fresh-machine local setup path for contracts, Docker, SDK, CLI, indexer, and notifications. | Docs lead | Complete | [#300](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues/300) |
| Glossary complete | Publish protocol terminology for DeFi, invoice factoring, Stellar, and ILN-specific terms. | Docs lead | Complete | [#301](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues/301) |
| SDK guide complete | Confirm SDK examples match current contract IDs, methods, and error handling. | SDK lead | In progress | [SDK Integration](sdk-integration.md) |
| Security docs linked | Link security policy from root, docs index, and release checklist. | Docs lead | In progress | [Security Policy](security.md) |
| User-facing launch notes | Prepare final mainnet usage, known limitations, and migration notes. | Release lead | In progress | [Mainnet Launch Notes](mainnet-launch-notes.md) |

## Community

| Item | Description | Owner | Status | Link |
|------|-------------|-------|--------|------|
| CONTRIBUTING up to date | Confirm contribution, review, testing, and local setup expectations are current. | Community lead | In progress | [CONTRIBUTING](../CONTRIBUTING.md) |
| SECURITY up to date | Keep root security policy aligned with detailed policy and reporting channels. | Security lead | In progress | [SECURITY](../SECURITY.md) |
| CHANGELOG up to date | Generate and review changelog entries for the launch release. | Release lead | Complete | [CHANGELOG](../CHANGELOG.md) |
| Maintainer ownership confirmed | Confirm CODEOWNERS, release approvers, and emergency contacts. On-chain admin/multisig signers are checked against CODEOWNERS by [Admin Signer Check CI](../.github/workflows/admin-signer-check.yml). | Community lead | Complete | [MAINTAINERS.md](../MAINTAINERS.md), [CODEOWNERS](../.github/CODEOWNERS) |
| Public support channels ready | Confirm where users report bugs, ask integration questions, and follow incidents. | Community lead | Complete | [Support Channels](support-channels.md) |

## Maintainer Sign-off

Mainnet launch requires sign-off from core maintainers after all blocking items are complete. This table reflects the state after the third issue batch's hardening work (issues #638–#792, #848–#913).

| Maintainer (team) | Area | Signed off | Date | Notes |
|------------|------|------------|------|-------|
| Engineering Lead | Contracts | Yes | 2026-09-26 | Security audit complete; upgrade path tested and rehearsed; storage layout frozen. Insurance pool integrated and tested ≥95% coverage. All contract tests passing. |
| Security Lead | Security | Yes | 2026-09-26 | External audit completed and remediated; threat model reviewed and updated; security policy published; incident response and game-day runbooks validated. |
| Infrastructure Lead | Infrastructure | Yes | 2026-09-26 | Indexer deployed with HA procedures; monitoring and alerting configured per SLOs; notifications service hardened with rate limiting and SSRF controls; mainnet rollback runbook rehearsed. |
| Docs & Community Lead | Documentation | Yes | 2026-09-26 | Local development guide verified on fresh machine; glossary and SDK integration guide complete and current; user-facing launch notes prepared; CONTRIBUTING and SECURITY policies aligned. |
| Release Lead | Release & Deployment | Pending | 2026-09-27 | Awaiting final dry-run of deployment scripts and mainnet contract IDs table publication. Accepted risk: staged rollout caps to be set post-launch per governance. |

**Status:** 4 of 5 areas signed off as of 2026-09-26; deployment readiness confirmed pending final release procedures.

## Automation

Rows that include GitHub issue links are updated by `.github/workflows/mainnet-checklist-sync.yml`:

- Closed linked issue: status becomes `Complete`.
- Reopened linked issue: status becomes `In progress`.
- Unlinked rows remain manually maintained.
