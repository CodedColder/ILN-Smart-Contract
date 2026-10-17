# Documentation Index

Start here when you are looking for ILN protocol, contract, service, or contributor documentation.

## Getting Started

| Document | Description |
|----------|-------------|
| [Developer Quickstart](developer-quickstart.md) | Rust, Stellar CLI, contract build, tests, and testnet deployment. |
| [Local Development Guide](local-development.md) | Full local stack setup for contracts, Docker services, SDK, CLI, indexer, and notifications. |
| [First Invoice Tutorial](tutorials/first-invoice.md) | End-to-end invoice lifecycle walkthrough. |
| [Glossary](glossary.md) | Protocol terminology for DeFi, invoice factoring, Stellar, and ILN-specific concepts. |

## Architecture And Protocol

| Document | Description |
|----------|-------------|
| [Architecture](Architecture.md) | Actors, money flow, state machine, and component boundaries. |
| [Contract ABI](contract-abi.md) | Public contract functions and error codes. |
| [Events](events.md) | Contract event topics and payloads. |
| [Governance](governance.md) | Proposal lifecycle, voting, quorum, and timelocks. |
| [Reputation Model](reputation-model.md) | Reputation and NFT lifecycle guide: initialization, accrual, lazy decay, NFT interaction, source of truth. |
| [Reputation System](reputation.md) | Reputation mechanics, parameters, events, and FAQ. |
| [Multi-Token Support](multi-token.md) | SAC, USDC, XLM, and supported token configuration. |
| [Token Safety](token-safety.md) | Token admission checks, decimals, SAC authorization, frozen accounts, clawback, and escrowed payouts. |
| [Storage Layout](storage-layout.md) | Contract storage keys and data structures. |

## Security And Operations

| Document | Description |
|----------|-------------|
| [Security Policy](security.md) | Vulnerability classes, reporting, response timelines, severity, and safe harbor. |
| [Threat Model](threat-model.md) | Trust assumptions, risks, and mitigations. |
| [Governance Security Summary](governance-security-summary.md) | Reviewer-facing synthesis of governance-hardening findings: quadratic voting, delegation bounds, snapshot timing, spam resistance, quorum, and the veto sunset roadmap. |
| [Public Status Page](public-status-page.md) | Public, non-sensitive protocol-health page: what is shown/excluded, hosting decision, thresholds, failure behaviour. |
| [Incident Response Runbook](incident-response-runbook.md) | Protocol-wide incident coordination: severity, roles, `pause()` decision authority, communication, and links to every component runbook. |
| [Observability Standards](observability-standards.md) | Structured JSON logging format and the correlation-ID scheme across the indexer and notifications services. |
| [Access Control](access-control.md) | Roles and authorization rules. |
| [Upgrade Guide](upgrade-guide.md) | Contract upgrade procedure and safeguards. |
| [Mainnet Launch Checklist](mainnet-launch-checklist.md) | Launch-readiness checklist with owners, statuses, and sign-off. |
| [Mainnet Launch Notes](mainnet-launch-notes.md) | User-facing testnet-to-mainnet changes, known limitations, and migration notes. |
| [Mainnet Deployment Runbook](mainnet-deployment-runbook.md) | Dry-run-verified procedure for deploying, verifying, and publishing mainnet contracts. |
| [Pre-Audit Checklist](pre-audit-checklist.md) | Audit preparation tasks. |
| [Formal Verification (lifecycle)](formal-verification.md) | Invoice/governance invariants. |
| [Formal Verification (cross-contract)](formal-verification-cross-contract.md) | ILN × distribution × insurance system invariants. |
| [Cross-Contract Proof Obligations](proof-obligations.md) | Machine-checked registry mapping each cross-contract invariant to its code path and discharging test. |


## Protocol Economics & Risk

Read in this order when onboarding to protocol economics, LP risk, and governance operations. Deeper attack/insurance models follow the guides.

| Order | Document | Description |
|-------|----------|-------------|
| 1 | [LP Risk Management Guide](lp-risk-management-guide.md) | Default, yield, lock-up, oracle, and governance risks for LPs; working assumptions and checklist. |
| 2 | [Governance Operations Playbook](governance-operations-playbook.md) | Steady-state proposal flow, emergency pause/oracle/insurance paths, parameter-change checklist. |
| 3 | [Token Economics Paper](token-economics.md) | Value loop, baseline projections, kill-criteria, and testnet validation findings. |
| 4 | [Oracle Attack Economics](oracle-attack-economics.md) | Cost/benefit model for payer-oracle manipulation at current parameters. |
| 5 | [Insurance Pool Design](insurance-pool-design.md) | Default-protection pool interface, claims, and integration. |
| 6 | [Insurance Pool Launch Parameters](insurance-pool-launch-parameters.md) | Recommended premium, coverage tiers, caps, and circuit-breaker values. |
| — | [Economics & Risk Review Cadence](review-cadence.md) | Who reviews these assumptions, how often, and what each cycle re-validates. |

The [SCF technical narrative](scf-technical-narrative.md) points here instead of duplicating economics content.

## Integrations

| Document | Description |
|----------|-------------|
| [SDK Integration Guide](sdk-integration.md) | TypeScript examples for contract interactions. |
| [SDK README](../sdk/README.md) | Package usage for `@iln/sdk`. |
| [Oracle Design](oracle-design.md) | Optional payer-verification oracle model. |
| [Oracle Integration](oracle-integration.md) | Deploying and registering compatible oracles. |
| [Oracle Provider Vetting](oracle-provider-vetting.md) | Governance vetting criteria and proposal template for approving oracle providers. |
| [Oracle Attack Economics](oracle-attack-economics.md) | Cost/benefit model of oracle manipulation at current parameters, with recommendations. |


## Community, SCF, And Partners

| Document | Description |
|----------|-------------|
| [Integration Partner Onboarding](integration-partner-onboarding.md) | Stable surfaces, upgrade expectations, support path, and mainnet go-live checklist for external integrators. |
| [Public Audit Findings Summary](audit-findings-summary.md) | Template for publishing audit finding ID, severity, public excerpt, status, and remediation links. |
| [SCF Grant Milestone Tracker](scf-grant-milestone-tracker.md) | Maps this batch’s issues to SCF-style deliverables via labels (no duplicated status). |
| [Support Channels](support-channels.md) | Where to report bugs, ask questions, and disclose vulnerabilities. |
| [SCF Technical Narrative](scf-technical-narrative.md) | Protocol overview and hardening summary for Community Fund reviewers. |

## Project Process

| Document | Description |
|----------|-------------|
| [Architecture Decision Records](adr/README.md) | ADR list and decision history. |
| [CI/CD](ci-cd.md) | Continuous integration and deployment workflows. |
| [Code Freeze Procedure](code-freeze-procedure.md) | Release freeze process. |
| [Benchmarks](benchmarks.md) | Gas and resource usage tracking. |
| [Support Channels](support-channels.md) | Where to report bugs, ask questions, and request features. |
