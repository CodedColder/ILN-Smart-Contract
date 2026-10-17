# ADR-015: Integrate LP Reputation Score into Insurance Premium Rate

**Date:** 2026-09-24
**Status:** Accepted

## Context

Open Question 3 in `docs/insurance-pool-launch-parameters.md` asks whether an
LP's reputation score should influence their premium rate or coverage tier.
The `reputation_bonus` contract already tracks scores independently via
`ReputationScore` (0–100 based on `invoices_paid / invoices_submitted`).
Leaving this unresolved means the two systems don't reinforce each other.

The insurance pool's `calculate_premium_rate_bps` currently uses only
`default_count` to price risk. An LP with zero defaults pays the base rate
regardless of whether they have 1 paid invoice or 1000.

## Decision

**Higher reputation reduces the effective default count used for premium
pricing.** Specifically:

```
reputation_discount = reputation_score / 100   (0.0 – 1.0)
effective_default_count = default_count × (1 - reputation_discount)
```

Examples:
- LP with 2 defaults and score 50: effective = 2 × 0.5 = 1
- LP with 3 defaults and score 80: effective = 3 × 0.2 = 0.6 → floor to 0
- LP with 0 defaults and score 100: effective = 0 (no change)

The floor ensures that reputation never *increases* the premium — it can
only reduce or maintain it.

### Cross-Contract Read

The insurance pool reads the reputation score via `env.invoke_contract` to
the `reputation_bonus` contract's `get_reputation(address)` method. The
reputation contract address is stored in the insurance pool's instance
storage (set by admin at init or via a governance proposal).

### Fallback

If the reputation contract address is not configured, or the cross-contract
call fails, the premium calculation falls back to the current behavior
(ignoring reputation). This ensures backward compatibility and graceful
degradation.

## Alternatives Considered

| Alternative | Why rejected |
|-------------|--------------|
| Reputation influences coverage tier instead of premium rate | Coverage tier is already driven by premiums_paid, adding reputation creates a circular dependency |
| Reputation is read from invoice_liquidity's ReputationProfile | The two reputation systems are intentionally separate (ADR-011); mixing sources would create confusion |
| Hardcode reputation contract address | Not upgradeable; admin-configurable is more flexible |

## Consequences

**Positive:**
- Rewards reliable LPs with lower premiums
- Aligns insurance and reputation incentive systems
- Provides additional solvency headroom by reducing effective risk

**Negative / Trade-offs:**
- Adds cross-contract call overhead (~1 wasm invocation per premium calculation)
- New admin responsibility: must configure reputation contract address
- Fallback behavior means LPs without reputation data get no discount (by design)
