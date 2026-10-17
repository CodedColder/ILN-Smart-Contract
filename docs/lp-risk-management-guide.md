# LP Risk Management Guide

**Status:** Living document — reviewed on the cadence in [`review-cadence.md`](review-cadence.md).  
**Audience:** Liquidity providers, risk reviewers, and governance proposers.

## Purpose

This guide explains the material risks an LP faces when funding invoices on ILN, how protocol mechanisms mitigate them, and which thresholds should trigger caution or exit.

## Risk inventory

| Risk | Description | Primary mitigations | Residual |
|------|-------------|---------------------|----------|
| Payer default | Payer fails to settle face value | Insurance pool enrollment & claims; reputation; optional oracle verification | Pool insolvency; uncovered LPs |
| Discount / yield shortfall | Realized yield below expectation | Competitive LP queue; max discount bound (5 000 bps) | Utilization and tenor mismatch |
| Opportunity / lock-up | Capital locked until due date or default path | Transferable LP position (where enabled); cancel rules | Illiquidity before maturity |
| Oracle / attestation failure | Bad payer verification | Opt-in oracle; admin/governance removal; attack economics model | Trusted single oracle address today |
| Parameter / governance change | Premium rates, caps, pause | Timelocks; veto/pause runbooks | Emergency pause can halt funding |
| Smart-contract / ops failure | Bug, upgrade, indexer outage | Audits; pause; incident runbooks | Early-stage protocol residual |

## Working assumptions (re-validate each review cycle)

These numbers are **planning assumptions**, not guarantees. They are the baseline the [token economics paper](token-economics.md) and [review cadence](review-cadence.md) re-check against observed usage.

| Assumption | Baseline | Source / rationale |
|------------|----------|--------------------|
| Expected annualized default rate (grade-A) | 1–2% | Insurance launch parameters conservative estimate |
| Insurance base premium | 500 bps (5%) | [`insurance-pool-launch-parameters.md`](insurance-pool-launch-parameters.md) |
| Max discount rate | 5 000 bps (50%) | `MAX_DISCOUNT_RATE` |
| Mean invoice size (early mainnet) | ~100–500 units | Launch-parameter planning range |
| Oracle staleness window | ~24h (`max_oracle_age_ledgers` default) | Contract defaults |

## Practical LP checklist

1. **Before funding** — Confirm token, face value, due date, discount, and whether oracle verification is required for your risk appetite.
2. **Insurance** — Enroll and keep premiums current if you want default protection; understand tiered coverage and reserve circuit breakers ([`insurance-pool-design.md`](insurance-pool-design.md)).
3. **Concentration** — Cap exposure per payer, per token, and per tenor; treat uncapped invoice size as a portfolio risk ([`oracle-attack-economics.md`](oracle-attack-economics.md)).
4. **Monitoring** — Watch pause events, governance proposals affecting discount/oracle/insurance, and pool health views.
5. **Exit** — Know cancel/transfer paths and that a protocol pause freezes new economic activity.

## Escalation

- Protocol incidents → [`incident-response-runbook.md`](incident-response-runbook.md)
- Suspected vulnerability → [`SECURITY.md`](../SECURITY.md) (not public issues)
- Product / integration questions → [`support-channels.md`](support-channels.md)

## Related reading

1. [Insurance pool design](insurance-pool-design.md)
2. [Oracle attack economics](oracle-attack-economics.md)
3. [Token economics](token-economics.md)
4. [Threat model](threat-model.md)
5. [Governance operations playbook](governance-operations-playbook.md)
