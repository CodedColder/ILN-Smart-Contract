# SCF Grant Milestone Tracker

**Purpose:** Map engineering work in this repository to Stellar Community Fund–style milestone deliverables for transparent progress reporting.  
**How to keep it current:** Prefer **links to GitHub issues, labels, and milestones** over copying status text. Update this page when a milestone’s *definition* changes; let GitHub reflect open/closed state.

## Milestone map (this batch + priors)

| SCF-style deliverable | What “done” looks like | Primary GitHub filter | Example issues |
|-----------------------|------------------------|----------------------|----------------|
| **M1 — Protocol economics & risk policy** | Unified docs index, review cadence, LP/governance/token economics artifacts | [`governance-policy-docs`](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues?q=label%3Agovernance-policy-docs) | #893–#898 |
| **M2 — SCF & community readiness** | Support channels, partner onboarding, audit-findings template, narrative/tracker | [`scf-community`](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues?q=label%3Ascf-community) | #899–#905 |
| **M3 — Formal verification & cross-contract automation** | Per-contract + cross-contract specs and CI-backed checks | [`formal-verification-automation`](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/issues?q=label%3Aformal-verification-automation) | #855–#862 |
| **M4 — Audit readiness / launch hygiene** | Dashboard, checklists, CODEOWNERS, CHANGELOG | [audit-readiness-dashboard.md](audit-readiness-dashboard.md), [mainnet-launch-checklist.md](mainnet-launch-checklist.md) | #899 (findings template), #904, #905 |
| **M5 — Prior hardening batches** | Economic security, governance hardening, infra (already summarized in narrative) | [scf-technical-narrative.md](scf-technical-narrative.md) Production-Hardening Summary | Prior closed batches linked from narrative |

## This batch — issue → deliverable

Lightweight checklist. **Source of truth for open/closed is the issue itself.**

### M1 — Economics & risk

| Issue | Title | Deliverable |
|------|-------|-------------|
| #893 | LP risk management guide | M1 |
| #894 | Governance operations playbook | M1 |
| #895 | Token economics paper | M1 |
| #896 | Economics & risk index section | M1 |
| #897 | Review cadence | M1 |
| #898 | Testnet validation of economics model | M1 |

### M2 — Community readiness

| Issue | Title | Deliverable |
|------|-------|-------------|
| #899 | Audit findings summary template | M2 / M4 |
| #900 | Integration partner onboarding guide | M2 |
| #901 | SCF narrative refresh | M2 |
| #902 | Public support channels | M2 |
| #903 | This tracker | M2 |
| #904 | CODEOWNERS / emergency contacts | M2 / M4 |
| #905 | CHANGELOG consolidation | M2 / M4 |

### M3 — Formal verification automation

| Issue | Title | Deliverable |
|------|-------|-------------|
| #855 | Cross-contract formal verification spec | M3 |
| #856–#862 | Access-control matrix, event coverage, dashboards, etc. | M3 |

## Native GitHub features (preferred)

- **Labels** above are the category keys — filter rather than re-typing status.
- Optional: create GitHub **Milestones** named `SCF-M1` … `SCF-M5` and attach issues; if present, link them here.
- Project boards are optional; do not duplicate column state into this markdown.

## Reporting snippet (for SCF updates)

> Progress for \[date\]: M1 \[n/m\] issues closed; M2 … (link this page + label searches). Residual risks: \[link audit dashboard / economics review cadence\].

## Related

- [SCF technical narrative](scf-technical-narrative.md)
- [docs index](index.md)
