# External Auditor Onboarding Package

**Last Updated:** 2026-08-30  
**Purpose:** Guide external audit firms through the ILN codebase efficiently.

---

## Recommended Reading Order

### 1. Architecture & System Overview

Start with the high-level architecture to understand the five-contract system and how components interact:

- [Architecture.md](Architecture.md) — System overview, component map, data flows
- [glossary.md](glossary.md) — Protocol and DeFi terminology

### 2. Core Contract Deep Dive

Read contracts in dependency order:

1. **`invoice_liquidity`** (core) — `contracts/invoice_liquidity/src/lib.rs`
   - Entry points: `submit_invoice`, `fund_invoice`, `mark_paid`, `claim_default`, `cancel_invoice`
   - [access-control.md](access-control.md) — Authorization matrix
   - [storage-layout.md](storage-layout.md) — On-chain storage keys
   - [error-codes.md](error-codes.md) — Error variants and remediation
   - [events.md](events.md) — Event schema

2. **`iln_governance`** — `contracts/iln_governance/src/lib.rs`
   - Proposals, voting, delegation, quorum, veto
   - [governance.md](governance.md) — Governance model

3. **`iln_distribution`** — `contracts/iln_distribution/src/lib.rs`
   - Yield and incentive distribution for LPs, freelancers, payers

4. **`insurance_pool`** — `contracts/insurance_pool/src/lib.rs`
   - Insurance pool for covering invoice defaults

5. **`reputation_bonus`** — `contracts/reputation_bonus/src/lib.rs`
   - Reputation-based discount bonuses

### 3. Security & Threat Analysis

- [threat-model.md](threat-model.md) — Threat analysis (v2.0, five-contract scope)
- [security.md](security.md) — Security policy and reporting
- [SECURITY.md](../SECURITY.md) — Root security policy

### 4. Operations & Deployment

- [mainnet-deployment-runbook.md](mainnet-deployment-runbook.md) — Deployment steps
- [mainnet-launch-checklist.md](mainnet-launch-checklist.md) — Pre-launch readiness
- [monitoring-runbook.md](monitoring-runbook.md) — Operational monitoring
- [disaster-recovery-multisig-signers.md](disaster-recovery-multisig-signers.md) — Recovery procedures

### 5. SDK & Integration

- [sdk-integration.md](sdk-integration.md) — SDK usage patterns
- [contract-abi.md](contract-abi.md) — Contract function signatures

---

## Known Accepted Risks

The following risks have been identified, assessed, and accepted for v1 launch:

| Risk | Severity | Rationale |
|------|----------|-----------|
| No timelock on governance parameter changes | Medium | ADR-005 documents decision; mitigated by multi-sig admin |
| Single-admin (no multi-sig in v1) | High | Multi-sig admin functions exist but are opt-in; production must configure |
| No reentrancy guard state flag | Medium | Soroban runtime provides some isolation; token transfers use checks-effects-interactions |
| `iln_distribution` emits no events | Medium | Acceptable for v1; indexer relies on core contract events |
| `decay_rate_bps` has no upper bound | Low | Admin-controlled; documented safe range is 0-500 |
| `high_rep_threshold` has no range check | Low | Admin-controlled; values >100 are unreachable but harmless |

---

## Areas Requiring Extra Scrutiny

Based on the hardening batch findings, focus audit attention on:

1. **Multi-sig admin flow** — `initialize_multisig_admin`, `propose_pause/unpause`, `sign_proposal`, `execute_proposal` (new in this batch)
2. **Token transfer paths** — `fund_invoice`, `mark_paid`, `claim_default`, `claim_yield` (checks-effects-interactions pattern)
3. **Oracle integration** — Stale data rejection, verified vs. unverified payer paths
4. **Fuzz test coverage** — `submit_invoice` is fuzzed; `fund_invoice` and `mark_paid` are not yet
5. **Distribution contract** — Mint authority, accrual calculations, event coverage gap
6. **Parameter bounds** — `decay_rate_bps`, `high_rep_threshold`, `min_discount_rate_bps` lack validation

---

## Audit Readiness

- [audit-readiness-dashboard.md](audit-readiness-dashboard.md) — Unified tracking of all pre-audit items
- [pre-audit-checklist.md](pre-audit-checklist.md) — Original pre-audit checklist (historical)

---

## Repository Structure

```
ILN-Smart-Contract/
├── contracts/              # Soroban smart contracts (WASM)
│   ├── invoice_liquidity/  # Core escrow contract
│   ├── iln_governance/     # Governance contract
│   ├── iln_distribution/   # Distribution contract
│   ├── insurance_pool/     # Insurance pool contract
│   ├── reputation_bonus/   # Reputation bonus contract
│   ├── fuzz/               # Fuzz testing suite
│   └── tests/              # Integration tests
├── sdk/                    # TypeScript SDK (@iln/sdk)
├── cli/                    # CLI tool (@iln/cli)
├── indexer/                # REST API event indexer
├── notifications/          # Webhook & email service
├── frontend/               # Web dApp (Next.js)
├── docs/                   # Documentation
└── scripts/                # Build, deploy, and test scripts
```

---

## CI/CD Overview

| Workflow | Purpose |
|----------|---------|
| `ci.yml` | Rustfmt, Clippy |
| `cargo-deny.yml` | Dependency audit (advisories, licenses, bans) |
| `admin-signer-check.yml` | Verifies on-chain admin matches CODEOWNERS |
| `codeql.yml` | Code security analysis |
| `e2e-allure.yml` | End-to-end test reporting |
| `storybook.yml` | Frontend component tests |

## Consolidated Automated-Audit Dashboard (Issue #862)

Single handoff view of every automated check in the formal-verification /
cross-contract automation category (Issues #54–#58) plus the repo's standing
security gates. **Status badges are the source of truth** — each one queries
the live GitHub Actions run on `dev`, not a point-in-time snapshot. Badges
point at `Invoice-Liquidity-Network/ILN-Smart-Contract` (the upstream repo,
where PR CI runs; fork-only badge URLs will not resolve until merge).

| Check | What it verifies | Where it lives | Status | Re-run locally |
|-------|------------------|----------------|--------|----------------|
| Event coverage (#854/#858) | Every state-mutating entrypoint across all five contracts emits an event or carries a documented exemption; docs/events.md cannot drift | `scripts/check-event-coverage.ts`, `.github/workflows/event-coverage.yml` | ![event coverage](https://img.shields.io/github/actions/workflow/status/Invoice-Liquidity-Network/ILN-Smart-Contract/event-coverage.yml?branch=dev&label=dev) | `make event-coverage` |
| Rust unit/integration suite | Contract behavior + regression tests (incl. rate-limit, oracle health-gate, multisig, NFT, reputation, TWAP suites) | `contracts/**`, Makefile | local gate | `make test` (per-crate: `make test-invoice`, `test-governance`, `test-insurance`, `test-distribution`) |
| Lint & panic-path gate | rustfmt + clippy `-D warnings` + no `unwrap()`/`expect()` in non-test contract source (#845) + event coverage | `make lint` | local gate | `make lint` |
| Dependency audit | Advisories, license compliance, duplicate/forbidden crates | `.github/workflows/cargo-deny.yml` | ![cargo deny](https://img.shields.io/github/actions/workflow/status/Invoice-Liquidity-Network/ILN-Smart-Contract/cargo-deny.yml?branch=dev&label=dev) | `cargo deny check` |
| CodeQL | Semantic code security analysis | `.github/workflows/codeql.yml` | ![codeql](https://img.shields.io/github/actions/workflow/status/Invoice-Liquidity-Network/ILN-Smart-Contract/codeql.yml?branch=dev&label=dev) | GitHub → Security → Code scanning |
| Admin signer / CODEOWNERS drift | On-chain admin signer set matches `CODEOWNERS` + mainnet env (daily cron) | `.github/workflows/admin-signer-check.yml` | ![admin signer](https://img.shields.io/github/actions/workflow/status/Invoice-Liquidity-Network/ILN-Smart-Contract/admin-signer-check.yml?branch=dev&label=dev) | `npx tsx scripts/verify-admin-signers.ts` |
| Env config drift | mainnet/testnet env files have not diverged unintentionally | `.github/workflows/env-config-drift-check.yml` | ![env drift](https://img.shields.io/github/actions/workflow/status/Invoice-Liquidity-Network/ILN-Smart-Contract/env-config-drift-check.yml?branch=dev&label=dev) | `npx tsx scripts/check-env-config-drift.ts` |
| E2E (Allure) | End-to-end protocol flows against a local network | `.github/workflows/e2e-allure.yml` | ![e2e](https://img.shields.io/github/actions/workflow/status/Invoice-Liquidity-Network/ILN-Smart-Contract/e2e-allure.yml?branch=dev&label=dev) | `make test-e2e` |
| Storybook | Frontend component tests | `.github/workflows/storybook.yml` | ![storybook](https://img.shields.io/github/actions/workflow/status/Invoice-Liquidity-Network/ILN-Smart-Contract/storybook.yml?branch=dev&label=dev) | `pnpm --filter @iln/storybook test` |
| Fuzz / property tests | Invariant testing across queue, reputation, TWAP paths | `contracts/fuzz`, Makefile | local gate | `make fuzz` |
| Code coverage | tarpaulin HTML report for the workspace | Makefile | local gate | `make coverage` |
| Bump/benchmark regression | Benchmark suite output vs recorded baseline | `scripts/check_benchmark_regression.sh` | local gate | `bash scripts/check_benchmark_regression.sh` |

Issue-to-check traceability for this batch:

- **#54 / #858** — event coverage row (`make event-coverage`).
- **#55 / #859** — rate-limit matrix covered by the Rust suite rows
  (`docs/rate-limiting.md`; tests `test_oracle_admin_functions_rate_limited`,
  `test_update_config_rate_limited`).
- **#56 / #860** — oracle health-gate regression tests in the Rust suite rows
  (`test_get_verified_price_rejects_tripped_circuit`,
  `test_get_verified_price_rejects_stale_health`,
  `test_get_twap_price_none_when_health_degraded`,
  `test_contract_stats_skips_stale_price_normalization`).
- **#58 / #862** — this section.

A point-in-time status snapshot (for offline audit packets) lives in
[audit-readiness-dashboard.md](./audit-readiness-dashboard.md); this section
stays authoritative because the badges update themselves.
