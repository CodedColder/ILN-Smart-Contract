# Governance Operations Playbook

**Status:** Living document — reviewed on the cadence in [`review-cadence.md`](review-cadence.md).  
**Audience:** Maintainers, governance proposers, and emergency operators.

## Purpose

Day-to-day and emergency procedures for ILN governance: proposals, voting, timelocks, pause, oracle registry changes, and insurance parameter updates.

## Roles

| Role | Responsibility |
|------|----------------|
| Docs lead (`@Keengfk/docs-lead`) | Keeps this playbook and economics/risk docs current |
| Contracts / security leads | Parameter and pause decisions; incident severity |
| Governance proposers | Draft proposals with clear motivation, bounds, and rollback |

See [CODEOWNERS](../.github/CODEOWNERS) for review routing.

## Steady-state operations

1. **Propose** — Prefer governance actions over hot admin keys once the multisig/governance handoff is complete ([`governance.md`](governance.md), ADR-012).
2. **Vote** — Respect voting window, quorum, and checkpoint-aged snapshots; no flash-loan first-vote shortcuts.
3. **Timelock / execute** — Wait execution delay; smoke-test after execution.
4. **Communicate** — Record material parameter changes in CHANGELOG and, for mainnet, user-facing launch notes.

## Emergency paths

| Situation | First action | Follow-up |
|-----------|--------------|-----------|
| Active fund loss / critical exploit | Consider `pause()`; open private security path | Incident runbook; advisory |
| Malicious or stale oracle | Remove/replace via admin or governance | Re-read oracle attack economics |
| Insurance pool stress | Check reserve circuit breaker / claims pause | Parameter proposal if needed |
| Multisig / signer loss | Disaster-recovery multisig runbook | Rotate signers; verify CI signer check |

Primary references: [`incident-response-runbook.md`](incident-response-runbook.md), [`multisig-admin-runbook.md`](multisig-admin-runbook.md), [`disaster-recovery-multisig-signers.md`](disaster-recovery-multisig-signers.md).

## Adjustable Parameter Safe Ranges

All adjustable parameters across all five contracts must remain within the bounds below. Parameters are enforced by code validation, rate limiting, or procedural governance review.

### Invoice Liquidity Contract

| Parameter | Safe Range | Rationale | Update method |
|-----------|-----------|-----------|---|
| **Fee rate** | 50–5,000 bps | Below 50 bps unsustainable; above 50% drives users elsewhere | `ProposalAction::UpdateFeeRate` |
| **Max discount rate** | 1,000–9,000 bps | Below 10% doesn't attract LP funding; above 90% leaves minimal LP yield | `ProposalAction::UpdateMaxDiscountRate` |
| **Min discount rate** | 1–5,000 bps | Baseline LP yield floor; must be ≤ max_discount_rate | `update_config` (batch) |
| **High reputation threshold** | 50–95 (unitless 0–100) | Below 50: too many LPs qualify; above 95: almost none qualify | `update_config` (batch) |
| **Decay rate** | 1–500 bps per period | Below 0.01%: no decay; above 5%: history too stale | `update_config` (batch) |
| **Decay period** | ~6–12 months | Default: ~6 months ≈ 15,768,000 ledgers | `update_config` (batch) |
| **Dispute timeout** | 1–30 days | Below 1 day: insufficient appeal time; above 30 days: capital risk | `update_config` (batch) |

### Reputation Bonus Contract

| Parameter | Safe Range | Rationale | Update method |
|-----------|-----------|-----------|---|
| **Bonus bps** | 0–500 bps (hard-coded max) | Economic cap per audit findings (Issue #692) | `update_config` (batch) |
| **High rep threshold** | **Must defer to invoice_liquidity** | Single source of truth (ADR-011, Issue #917) | `update_config` (batch) |
| **Min discount rate** | 1–5,000 bps | Synchronized with invoice_liquidity | `update_config` (batch) |

### Insurance Pool Contract

| Parameter | Safe Range | Rationale | Enforcement |
|-----------|-----------|-----------|---|
| **Min pair defaults to flag** | 2–10 | Collusion heuristic sensitivity; 3 is default | Code constant |
| **Collusion share threshold** | 3,000–8,000 bps | Flags pairs where ≥30–80% of defaults from single payer | Code constant |
| **Timelock delay** | 1–7 days | Admin action delay; default 3 days | Code constant |
| **Coverage cap per claim** | > 0, ≤ avg invoice size | Prevents over-insurance; set at init | Admin function |

### Governance Contract

| Parameter | Safe Range | Rationale | Enforcement |
|-----------|-----------|-----------|---|
| **Vote hold period** | ≥ 10 ledgers | Flash-loan defense; fixed in code | Code constant |
| **Voting period (per proposal)** | 3–14 days | Below 3 days: insufficient notice; above 14 days: evolution too slow | Per-proposal setting |
| **Quorum threshold** | 30–50% | Below 30%: minority rule; above 50%: supermajority (standard governance) | Per-proposal setting |

### TWAP Oracle

| Parameter | Safe Range | Rationale | Update method |
|-----------|-----------|-----------|---|
| **TWAP window** | 360–17,280 ledgers (30 min–24 hr) | Prevents single-block manipulation; enforced code bounds | `set_twap_window_ledgers` (rate-limited ~10 min) |
| **Max oracle age** | 30 min–7 days | Below 30 min: network latency risk; above 7 days: price too stale | `set_max_oracle_age` (admin, no rate limit) |
| **Consecutive stale queries before circuit break** | 2–5 | Default 3; trips breaker after this many stale reads | Code constant |

## Parameter change checklist

Before proposing discount, oracle-age, insurance premium/coverage, or pause-policy changes:

- [ ] Cite the assumption being changed ([`lp-risk-management-guide.md`](lp-risk-management-guide.md), [`token-economics.md`](token-economics.md))
- [ ] Confirm new value is within safe ranges above (or document exceptional justification)
- [ ] Note impact on LPs already enrolled or in-flight invoices
- [ ] Link simulation / testnet evidence when available
- [ ] Schedule post-change review under [`review-cadence.md`](review-cadence.md)

## Related reading

- [Governance](governance.md)
- [Governance security summary](governance-security-summary.md)
- [Access control](access-control.md)
- [Security policy](security.md#remediation-sla-by-severity) (parameter bounds audit checklist)
- [Audit finding remediation runbook](audit-finding-remediation-runbook.md)
- [Protocol economics & risk index](index.md#protocol-economics--risk)
