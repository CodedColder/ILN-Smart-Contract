# SCF Technical Narrative

This document provides a coherent technical overview of the Invoice Liquidity
Network protocol for the Stellar Community Fund review. It synthesises the
protocol design, production-hardening work, **this Stellar Wave batch’s
community/verification deliverables**, and the current audit/testing posture
into a single narrative.

**Last reviewed:** 2026-09-24 (Issue #901). Evidence links point at in-repo
dashboards and labeled issue filters rather than aspirational claims.

**Non-author review:** Before any grant submission uses this narrative, a
maintainer who did **not** author the latest revision should check the
[Review confirmation](#review-confirmation) box below.

---

## Protocol Overview

Invoice Liquidity Network (ILN) is a two-sided protocol on Stellar/Soroban
that connects invoice holders (freelancers, SMEs) with liquidity providers. Invoice
holders submit invoices on-chain; liquidity providers fund those invoices at a
discount and collect the face value at maturity. The protocol earns its
economic security from escrowed collateral, on-chain reputation, and a
decentralised governance layer.

### Why Stellar

Stellar's low transaction costs, built-in asset issuance, and Soroban smart
contract platform make it well-suited for invoice factoring. The protocol
leverages Stellar's native multi-asset capabilities for settling invoices in
any approved token (EURC, USDC, XLM, and others).

### Core Value Proposition

- **Invoice holders** get early access to liquidity without traditional
  factoring fees
- **Liquidity providers** earn yield by advancing capital against verified
  invoices
- **The protocol** is governed on-chain with transparent rules for dispute
  resolution, defaults, and parameter updates

### Reputation and NFT lifecycle

Every participant starts at a neutral 50/50 reputation that accrues
asymmetrically (+1 per on-time settlement, −5 per default, floor 0, appeal
restores the pre-default score) and decays lazily toward 0 with inactivity —
no keeper required. Each funded invoice is simultaneously a transferable NFT
claim (minted on funding, held by the funder, burned on settlement), so *who
holds the claim* and *how the payer behaved* update atomically. Protocol
decisions read only `invoice_liquidity` scores; the standalone
`reputation_bonus` module keeps separate counters for discount bonuses. The
full cross-referenced walkthrough lives in the
[Reputation Model lifecycle guide](reputation-model.md#full-lifecycle-guide-issues-854--single-cross-referenced-narrative).

---

## Architecture Summary

The protocol is a monorepo containing five Soroban smart contracts, a
TypeScript SDK, CLI, event indexer, and notifications service.

### Smart Contracts

| Contract | Role |
|----------|------|
| `invoice_liquidity` | Core escrow: submit, fund, settle, cancel, default invoices; multi-token support; reputation scoring; optional payer oracle |
| `iln_governance` | On-chain governance: proposals, voting, delegation, quorum, timelocked admin actions |
| `iln_distribution` | Yield and incentive distribution for LPs, freelancers, and payers |
| `reputation_bonus` | Reputation-based discount bonuses and invoice hooks |
| `insurance_pool` | Default-protection insurance pool for liquidity providers |

### Off-Chain Services

| Service | Role |
|---------|------|
| `@iln/sdk` | Typed TypeScript client library wrapping Soroban RPC calls |
| `@iln/cli` | Terminal wallet and invoice management tool |
| `@iln/indexer` | REST API indexing Horizon events into Postgres |
| `@iln/notifications` | Webhook, Slack, and email delivery for invoice lifecycle events |

### On-Chain State Machine

```
Submitted → Funded → Settled (happy path)
                ↓
          Defaulted → Insurance Claim
                ↓
          Appealed → Resolved
```

### Data Flow

1. **Submit** — invoice holder submits an invoice with metadata
2. **Fund** — LP commits capital at a discount rate
3. **Settle** — payer pays the face value; LP receives return
4. **Default** — if payer does not pay, the insurance pool covers the LP
5. **Governance** — parameter changes, oracle registration, and emergency
   actions go through on-chain proposals

---

## Production-Hardening Summary

Summarises completed hardening (prior batches) plus **this batch’s** readiness
work. Prefer the linked artifacts over restating status here.

### Economic Security

Protocol economics, LP risk assumptions, insurance parameters, and oracle-attack
costing live in the unified
[Protocol Economics & Risk](index.md#protocol-economics--risk) section of the
docs index (LP risk guide → governance playbook → token economics → oracle
attack economics → insurance design/parameters, plus the review cadence). This
narrative keeps only a short capability summary:

- Multi-token support with token management functions and associated events
- Discount rate validation and bounds checking
- Payer verification oracle interface with mock oracle for testing
- Insurance pool for default protection with test coverage
- Reputation tracking with lazy decay for inactive addresses
- Incremental vote total caching for gas-efficient governance execution
- **This batch:** economics/risk documentation set and review cadence — see
  [`governance-policy-docs` issues](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues?q=label%3Agovernance-policy-docs)

### Governance Security

- Admin veto, ILN-gated quorum supply, checkpoint-aged snapshots, timelocks,
  delegation, pause/unpause, disaster-recovery multisig docs

### Infrastructure Hardening

- Fuzz/property tests, benchmark regression guard, 95% line coverage gate on
  `invoice_liquidity`, storage/upgrade docs, monitoring runbooks
- **This batch:** automated access-control matrix + public-doc CI gates
  ([`access-control-matrix.generated.md`](access-control-matrix.generated.md),
  Issues #856 / #857); cross-contract formal-verification work under
  [`formal-verification-automation`](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues?q=label%3Aformal-verification-automation)

### SCF & community readiness (this batch)

Tracked in [`scf-grant-milestone-tracker.md`](scf-grant-milestone-tracker.md)
and label [`scf-community`](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues?q=label%3Ascf-community):

- Public support channel audit, partner onboarding guide, audit-findings
  summary template, maintainer/emergency ownership (`MAINTAINERS.md`), this
  narrative refresh

---

## Current Test and Audit Status

### Audit Status

Per [`mainnet-launch-checklist.md`](mainnet-launch-checklist.md), the
**external security audit** checklist row is marked **Complete** (Issue #298)
for contracts, deployment scripts, SDK builders, indexer APIs, and
notifications webhooks. Day-to-day readiness (coverage gaps, parameter bounds,
docs) continues on the
[audit-readiness dashboard](audit-readiness-dashboard.md). Public findings
will be published via the
[audit findings summary template](audit-findings-summary.md) when the report
is disclosable.

### Test Coverage

| Area | Status (evidence) |
|------|-------------------|
| Unit tests | Present across all five contracts |
| Integration / cross-contract tests | e.g. full-protocol lifecycle + cross-contract invariant suite |
| Fuzz tests | `iln_fuzz` / property suites for high-risk paths |
| E2E | SDK + local stack under `tests/e2e` |
| Coverage threshold | 95% line coverage enforced on `invoice_liquidity` (CI) |
| Benchmark regression | `scripts/check_benchmark_regression.sh` |

### Known Gaps (honest)

- Insurance pool coverage and extended fuzz paths are still expanding
  (dashboard/test issues)
- Mainnet multisig signers list is **empty until appointed**
  (`mainnet-admin-signers.json`) — expected pre-launch
- Token-economics projections remain model-based until a durable velocity
  dashboard export exists
- Some `ContractError` variants still lack dedicated tests

### Honest Assessment

The protocol is **not mainnet-live**. Hardening and documentation for SCF
review are substantially advanced; residual risk is early-stage usage risk,
incomplete optional modules, and operational items still “In progress” on the
launch checklist. Pause/governance controls and staged caps are the intended
safety net for first mainnet capital.

---

## Review confirmation

- [ ] Non-author maintainer reviewed this narrative against the audit-readiness
      dashboard and grant milestone tracker on ________ (date) — reviewer: ________

---

## Links

- [Architecture](Architecture.md) — full system design
- [SCF Grant Milestone Tracker](scf-grant-milestone-tracker.md) — issue→deliverable map for this batch (label-linked)
- [Integration Partner Onboarding](integration-partner-onboarding.md) — external integrator go-live path
- [Audit Findings Summary](audit-findings-summary.md) — public findings template
- [Protocol Economics & Risk](index.md#protocol-economics--risk) — LP risk, token economics, insurance, oracle attack model, review cadence
- [Audit Readiness Dashboard](audit-readiness-dashboard.md) — audit tracking
- [SCF Grant Milestone Tracker](scf-grant-milestone-tracker.md) — issue→deliverable map
- [Protocol Economics & Risk](index.md#protocol-economics--risk) — LP/governance/token economics
- [Access Control (narrative)](access-control.md) · [Generated matrix](access-control-matrix.generated.md)
- [MAINTAINERS.md](../MAINTAINERS.md) — ownership & emergency contacts
- [Threat Model](threat-model.md) — security assumptions
- [Mainnet Launch Checklist](mainnet-launch-checklist.md) — launch readiness
- [CONTRIBUTING.md](../CONTRIBUTING.md) — contributor workflow
- [CHANGELOG.md](../CHANGELOG.md) — version history
