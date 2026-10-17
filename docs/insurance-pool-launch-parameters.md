# Insurance Pool Launch Parameter Recommendations

**Status:** Ready for governance proposal (Issue #697)

## Executive Summary

This document provides data-backed recommendations for the initial configuration parameters of the insurance pool before mainnet launch. These recommendations are derived from stress testing, solvency modeling, and tier boundary analysis conducted in parallel issue work.

## Recommended Parameters

### 1. Base Premium Rate (`base_premium_rate_bps`)

**Recommended Value:** `500` basis points (5.0%)

**Rationale:**
- Base rate of 5% provides a balance between affordability for LPs and pool solvency.
- Historical industry benchmarks for short-term lending insurance range from 3% to 8%.
- At 5%, an LP funding a 100 stablecoin invoice would pay 5 stablecoins as annual premium.
- This allows the pool to accumulate premiums faster than expected default rates (conservatively estimated at 1-2% annual for grade-A borrowers).

**Conservative Starting Value:** YES — This can be increased after initial mainnet data shows pool health metrics.

### 2. Risk Multiplier (`risk_multiplier`)

**Recommended Value:** Numerator `50`, Denominator `100` (0.5x multiplier per default)

**Rationale:**
- LPs with prior defaults should pay higher premiums to reflect increased risk.
- A 0.5x multiplier means each default on an LP's record increases their rate by 50 bps (e.g., from 5% to 5.5% after one default).
- This incentivizes LPs to improve creditworthiness without pricing them out entirely.
- Provides sufficient buffer to identify and exclude repeat defaulters early.

**Conservative Starting Value:** YES — Can be adjusted upward (e.g., 1.0x) if defaults are higher than expected.

### 3. Coverage Tiers

**Recommended Tier Structure:**

| LP Premium History   | Tier | Coverage Payout |
|---------------------|------|-----------------|
| < 10% of cap        | 1    | 50% of coverage |
| 10–25% of cap       | 2    | 75% of coverage |
| 25–50% of cap       | 3    | 100% of coverage |
| > 50% of cap        | 4    | 150% of coverage |

**Rationale:**
- Tiered coverage rewards long-term participation and premium accumulation.
- LPs paying less than 10% of the coverage cap receive partial protection (50%), incentivizing higher participation.
- LPs paying 25% or more get full or enhanced coverage, reflecting their commitment and risk contribution.
- The 150% tier for high contributors acts as a retention mechanism and accounts for their outsized risk management efforts.

**Conservative Starting Value:** YES — This structure is proven in testing and is not aggressive.

### 4. Flat Coverage Cap per Claim

**Recommended Value:** `10_000_000_000` stroops (1000 XLM equivalent @ 1e7 stroops per unit)

**Rationale:**
- Sets a per-claim maximum payout to prevent catastrophic pool depletion on a single default.
- At 1000 XLM, an LP funding a 100 XLM invoice receives up to 1000 XLM if it defaults (10x coverage for Tier 4 LPs).
- This is conservative; can be increased to 2000 XLM after 6 months of data showing pool solvency.
- Aligns with expected invoice sizes on ILN (estimated mean ~100–500 XLM in beta).

**Conservative Starting Value:** YES — Lower bound to prove pool sustainability before increasing.

### 5. Balance Cap (Optional)

**Recommended Value:** Initially uncapped; review after 30 days of mainnet operation.

**Rationale:**
- Leaving the balance cap unset allows the pool to accumulate premiums freely.
- After mainnet launch and real data on claim frequency and LP participation, we can set a cap if needed.
- A cap prevents the pool from becoming too large and reduces incentives for new LP participation.
- Suggestion: If pool balance exceeds 10,000,000 stroops (1000 XLM) after 30 days, cap new deposits and propose governance vote on fee redistribution.

**Conservative Starting Value:** YES — No cap is safer than a cap that's too low.

### 6. Solvency Circuit Breaker Threshold (Recommended) — `min_reserve_ratio_bps`

**Recommended Value:** `1000` bps (10% of the per-claim coverage cap) as a
trip threshold; `0` (disabled) is the safe pre-governance default.

**Rationale:**
- On mainnet the pool should be better protected than the liquid-premium-only model of
  the stub: the breaker pauses **new claim payouts** automatically when the
  pool's reserve ratio (`(balance + backstop) / coverage`) falls below the
  threshold (Issue #826).
- Enrollments and premium deposits are never blocked by the breaker, so it is
  a *stop-loss* for remaining LPs, not a lock-in: governance restores reserves
  and calls `reset_solvency_circuit()` to resume.
- A trip emits `SolvencyCircuitTripped`, which the Week-3/4 review should treat
  as a **trigger event** for an emergency governance review (see schedule
  below).

**Conservative Starting Value:** NO default with the breaker armed is
appropriate to ship today (a fresh pool has near-zero ratio and would be
permanently stuck); the **decision** is that mainnet governance arms it via
`set_min_reserve_ratio_bps` before public launch.

### 7. Capital Backstop Funding Share (Recommended) — `backstop_funding_bps`

**Recommended Value:** `0` initially (disabled); protocol funding via
`top_up_backstop` from a DAO/treasury source. Re-evaluate a fee-share
(`set_backstop_funding_bps`) after 30 days of data.

**Rationale:**
- The pool previously had *no* defence in depth beyond premiums
  ([threat-model G3](threat-model.md#g3-pool-drainage--insolvency-risk)); the
  backstop (Issue #827, [ADR-013](adr/ADR-013-capital-backstop.md)) adds a
  dedicated reserve drawn only after liquid balance is exhausted.
- A `0` default preserves existing deposit behavior; a fee-share does not need
  to be set until value is proven.
- `get_backstop_balance()` and `get_total_reserve()` let monitors verify the
  buffer continuously.

### 8. Claim Review Window (Optional) — `review_window_seconds`

**Recommended Value:** `0` (disabled) initially for the automatic flow; enable
per risk tier only if fraud patterns warrant a dispute buffer.

**Rationale:**
- `0` keeps `invoice_liquidity`'s automatic compensation path unchanged
  (Issue #828).
- For high-value tiers, governance can set a window (e.g. 86_400s = 1 day) so a
  claim pays out only after evidence + window elapse. A gated claim reports
  `compensated: false` until a follow-up payout, so it degrades gracefully
  without blocking `claim_default`.

## Governance Proposal Template

Below is a template ready for submission to the governance system:

```
Title: "Insurance Pool Mainnet Launch: Set Initial Premium Rates and Coverage Tiers"

Description:
"This proposal sets the initial configuration for the insurance pool on mainnet,
enabling liquidity providers (LPs) to opt into default protection via premium
payments.

Configuration:
- Base premium rate: 500 bps (5%)
- Risk multiplier: 0.5x per LP default
- Coverage tiers: 50% / 75% / 100% / 150% based on premium history
- Per-claim coverage cap: 10,000,000 stroops (1000 XLM)
- Balance cap: None (uncapped)

Rationale:
These parameters balance pool solvency with affordability for LPs. After 30 days
of mainnet operation, we will review claim frequency, pool health, and LP
participation to adjust parameters if needed.

Recommendation:
Approve this proposal to enable insurance pool operations on mainnet."
```

## Post-Launch Review Schedule

### Week 1–2: Monitor Pool Health
- Total premiums collected
- Number of enrolled LPs
- Average LP deposit size
- Pool balance

### Week 3–4: Review Claim Activity
- Number of default claims
- Total claims payout
- Pool solvency ratio (balance / outstanding coverage)

### Month 2: Governance Vote on Adjustments
- If claim rate > 5% annual: increase base rate to 750 bps (7.5%)
- If pool balance > 1000 XLM: cap new deposits and vote on fee reduction
- If pool balance < 100 XLM: reduce coverage tier multipliers to preserve solvency

## Open Questions & Next Steps

1. **Token Custody:** Current implementation records premiums as accounting balance. Before mainnet, confirm that token settlement (actual SAC transfer) is in place.

2. **Payout Priority:** If multiple LPs default simultaneously and pool balance is insufficient, clarify the claim priority (FIFO, pro-rata, or risk-weighted).

3. ~~**LP Reputation Integration:** Clarify whether an LP's payer reputation score influences their premium rate or coverage tier.~~ **Resolved by ADR-015:** Higher reputation scores reduce the effective default count used for premium pricing. See `docs/adr/ADR-015-reputation-premium-integration.md`.

4. **Governance Parameter Updates:** Confirm the process for governance to adjust `base_premium_rate_bps`, `risk_multiplier`, or `coverage_cap` without requiring a re-deployment.

## Conclusion

These parameters are conservative and data-backed. They prioritize pool sustainability over aggressive growth, allowing for safe scaling after we observe real mainnet behavior. The governance system enables rapid adjustment if needed.

**Recommendation:** Submit this proposal to governance for mainnet launch approval.
