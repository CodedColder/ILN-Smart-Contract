# Economics & Risk Documentation Review Cadence

**Status:** Policy  
**Owns:** Docs lead (`@Keengfk/docs-lead`), with Contracts lead and Security lead as required reviewers for assumption changes that affect on-chain parameters.

## Why this exists

Assumptions in the LP risk guide, governance playbook, and token economics paper (default rates, reward sustainability, risk thresholds) drift as real usage grows. Without a defined cadence they go stale the same way tracking checklists did.

## Cadence

| Trigger | When | Action |
|---------|------|--------|
| **Quarterly review** | Every calendar quarter (target: first two weeks of Jan / Apr / Jul / Oct) | Full re-validation of listed docs |
| **Usage-growth trigger** | Trailing-30-day funded notional **≥ 2×** the prior quarter’s average, **or** first mainnet month end | Out-of-band review (same checklist) |
| **Incident / threshold trigger** | Any trip of the kill-criteria in [`token-economics.md` §3.1](token-economics.md#31-risk-thresholds-that-invalidate-the-model) | Immediate review; patch docs in the same PR as the incident follow-up when possible |

## Scope (what gets re-validated)

Each cycle, the owner confirms or revises:

1. **Default-rate assumption** (1–2% grade-A baseline) vs observed defaults  
2. **Insurance premium / coverage / reserve** assumptions vs pool health  
3. **Reward / emission sustainability** vs reward-pool runway  
4. **LP risk thresholds** and concentration guidance  
5. **Oracle attack economics** qualitative conclusions if oracle parameters changed  
6. **Support / ops pointers** still match live channels ([`support-channels.md`](support-channels.md))

## Outputs

- PR updating the affected docs (even if the only change is “Last reviewed: YYYY-MM-DD — no change”)
- Short note in the PR body: metrics consulted, divergences, accepted residual risk
- If on-chain parameters should change, link a governance proposal draft

## Document set under this policy

| Document | Path |
|----------|------|
| LP risk management guide | [`lp-risk-management-guide.md`](lp-risk-management-guide.md) |
| Governance operations playbook | [`governance-operations-playbook.md`](governance-operations-playbook.md) |
| Token economics paper | [`token-economics.md`](token-economics.md) |
| Oracle attack economics | [`oracle-attack-economics.md`](oracle-attack-economics.md) |
| Insurance pool design / launch parameters | [`insurance-pool-design.md`](insurance-pool-design.md), [`insurance-pool-launch-parameters.md`](insurance-pool-launch-parameters.md) |

## Related

- Index entry: [Protocol Economics & Risk](index.md#protocol-economics--risk)
- Contributor workflow: [CONTRIBUTING.md](../CONTRIBUTING.md)
