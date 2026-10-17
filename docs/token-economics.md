# ILN Token Economics Paper

**Status:** Living model — assumptions reviewed per [`review-cadence.md`](review-cadence.md).  
**Last validation pass:** 2026-09-24 (testnet usage check — see [§5](#5-testnet-validation-against-observed-usage)).

## 1. Scope

This paper models the economic loop among freelancers (invoice submitters), payers, LPs, the insurance pool, and incentive distribution (`iln_distribution`). It is intentionally conservative for pre-mainnet parameter choice.

## 2. Value flow (simplified)

```text
Submit invoice → LP funds at discount → Payer settles face value
                      ↓ default
                 Insurance claim (if enrolled)
                      ↓
              Distribution / rewards (where configured)
```

Capital efficiency depends on **funding velocity** (invoices funded per unit time), **realized LP yield** (discount capture minus defaults/premiums), and **reward-pool depletion** (incentive emissions vs. sustainable inflows).

## 3. Baseline projections (pre-mainnet)

| Metric | Projection (early mainnet, first 90 days) | Notes |
|--------|-------------------------------------------|-------|
| Funding velocity | Low–moderate; ramp with partner onboarding | Not yet empirically confirmed on testnet at scale |
| Expected default rate | 1–2% annualized (grade-A) | Aligns with insurance launch assumptions |
| Insurance base premium | 500 bps | [`insurance-pool-launch-parameters.md`](insurance-pool-launch-parameters.md) |
| LP net yield target | Discount earned − defaults − premiums > 0 on portfolio | Portfolio construction in LP risk guide |
| Reward sustainability | Emissions should not outpace fee/premium inflows over a quarter | Re-check when distribution parameters finalize |

### 3.1 Risk thresholds that invalidate the model

Re-open this paper immediately (do not wait for the quarterly review) if any of the following hold on testnet or mainnet:

- Observed default rate **> 5%** annualized on a rolling 90-day window
- Insurance pool reserve ratio below the configured circuit-breaker for **> 7 days**
- Reward-pool runway **< 90 days** at the current emission rate
- Median time-to-fund rises enough that LP capital utilization collapses (qualitative + velocity dashboard)

## 4. Mainnet parameter implications

Until testnet produces statistically useful volume:

- Prefer the **conservative** insurance and discount bounds already documented
- Keep staged rollout caps ([mainnet checklist](mainnet-launch-checklist.md) / governance issues)
- Treat incentive programs as **opt-in experiments** with explicit budgets

## 5. Testnet validation against observed usage

### 5.1 Method

1. Identify the live testnet deployment (see CONTRIBUTING / developer quickstart; contract ID `CD3TE3IAHM737P236XZL2OYU275ZKD6MN7YH7PYYAXYIGEH55OPEWYJC` as of this writing).
2. Pull funding velocity, realized yields, and reward-pool depletion from the LP-queue / funding-velocity dashboard when available (frontend/infra dashboards; indexer APIs for funded/settled/defaulted counts).
3. Compare against §3 projections; record material divergence (>2× or opposite sign) and whether it changes mainnet parameters.

### 5.2 Findings (2026-09-24)

| Check | Result |
|-------|--------|
| Funding-velocity dashboard with production-like series | **Not available in-repo** as a reproducible data feed for this pass |
| Indexer-backed aggregate of testnet fund/settle/default rates | **Insufficient sample** for confirming or rejecting §3 projections |
| Reward-pool depletion rate | **Not observed** at meaningful scale on public testnet usage reflected in this repository |

**Conclusion:** There is **no material divergence to feed back into §3 yet**, because observed testnet usage is too sparse / not exported into a durable metrics artifact in this repo. The projections remain the planning baseline.

**Accepted residual risk:** Mainnet parameter choices still rely on modeled defaults and insurance stress tests rather than organic testnet volume. This is acceptable only while staged caps and pause/governance controls remain in place.

### 5.3 Follow-ups

- When the velocity dashboard ships durable exports, attach CSV/JSON snapshots under `docs/fixtures/` and update this section.
- Trigger an out-of-band review if any §3.1 threshold trips ([`review-cadence.md`](review-cadence.md)).

## 6. Related documents

- [LP risk management guide](lp-risk-management-guide.md)
- [Insurance pool design](insurance-pool-design.md) & [launch parameters](insurance-pool-launch-parameters.md)
- [Oracle attack economics](oracle-attack-economics.md)
- [Protocol economics & risk index](index.md#protocol-economics--risk)
