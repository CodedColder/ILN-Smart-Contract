# Integration Partner Onboarding Guide

**Audience:** External protocols and application developers integrating with ILN (not local contributors — those should start at [developer-quickstart.md](developer-quickstart.md)).  
**Status:** Living document.

## 1. What “integrating” means

An integration partner consumes ILN’s **stable public surfaces** — on-chain contracts and the `@iln/sdk` / indexer APIs — to submit, fund, settle, or observe invoices without forking the protocol.

## 2. Stable integration surfaces

| Surface | Stability expectation | Docs |
|---------|----------------------|------|
| `invoice_liquidity` entry points (submit / fund / settle / cancel / default) | Treated as the core product ABI; breaking changes require upgrade notice | [contract-abi.md](contract-abi.md), [Architecture.md](Architecture.md) |
| `@iln/sdk` typed methods | Semver; minor bumps additive | [sdk-integration.md](sdk-integration.md), [`sdk/README.md`](../sdk/README.md) |
| Indexer REST read APIs | Best-effort compatibility; additive fields preferred | [api-reference.md](api-reference.md) |
| Events / topics | Documented in [events.md](events.md); new topics additive | [events.md](events.md) |
| `iln_governance`, `iln_distribution`, `insurance_pool`, `reputation_bonus` | Integrate only via documented hooks; treat admin/governance as protocol-owned | ADRs + per-contract docs |

**Do not** depend on: private storage layouts, undocumented admin helpers, test-only mocks, or CLI flags marked experimental.

## 3. Versioning & upgrade notifications

- Contract upgrades follow [upgrade-guide.md](upgrade-guide.md) (wasm upload, migration, rollback decision points).
- Partners should:
  - Pin SDK versions in production and watch GitHub Releases / CHANGELOG
  - Subscribe to security advisories ([SECURITY.md](../SECURITY.md))
  - Re-run their go-live checklist after any mainnet upgrade announcement
- Breaking ABI changes must be called out in CHANGELOG and mainnet launch notes when applicable.

## 4. Support & escalation (distinct from bug-report drive-bys)

| Need | Channel |
|------|---------|
| Production incident affecting your integration | Follow [incident-response-runbook.md](incident-response-runbook.md) severity; open a private security report if funds are at risk |
| Integration design / partnership questions | [support-channels.md](support-channels.md) — GitHub Issues with a clear “partner integration” context in the body |
| Protocol bugs | Component bug templates under `.github/ISSUE_TEMPLATE/` |
| Vulnerability | **Never** public issues — [SECURITY.md](../SECURITY.md) |

This path is for partners with a live or imminent integration, not for general drive-by feature ideas (use the feature-request template for those).

## 5. Partner go-live checklist (mainnet)

Before routing real funds through ILN:

- [ ] Confirmed network (mainnet vs testnet) and published contract IDs from the official README / deployment notes
- [ ] SDK version pinned; `pnpm`/`npm` lockfile committed
- [ ] Exercised submit → fund → settle (and cancel/default as relevant) on **testnet** with your production build
- [ ] Verified event consumption / indexer reads match [events.md](events.md) / [api-reference.md](api-reference.md)
- [ ] Understood pause behavior ([access-control.md](access-control.md)) and what your UI shows if the protocol pauses
- [ ] Insurance / distribution optional hooks reviewed if you surface them to users
- [ ] Security contact on file; disclosure path understood
- [ ] Upgrade notification owner assigned on your side
- [ ] Support escalation contact listed in your runbook (points at [support-channels.md](support-channels.md))

## 6. Suggested reading order

1. [Architecture.md](Architecture.md)  
2. [sdk-integration.md](sdk-integration.md)  
3. [upgrade-guide.md](upgrade-guide.md)  
4. [support-channels.md](support-channels.md)  
5. [Protocol Economics & Risk](index.md#protocol-economics--risk) (if you intermediate LP capital)
