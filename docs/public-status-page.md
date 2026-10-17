# Public Protocol-Health Status Page

**Issue:** #892 · **Status:** implemented; needs one-time hosting configuration (see [§6](#6-going-live)).

The dashboards elsewhere in the monitoring category are internal and ops-facing.
This page is the **public** counterpart: a small, curated, non-sensitive summary
for the community and for grant reviewers (the "Community & Ecosystem"
transparency goal in the [audit readiness dashboard](audit-readiness-dashboard.md)
and the [SCF technical narrative](scf-technical-narrative.md)).

---

## 1. What it shows

| Section | Content | Source |
|---|---|---|
| **Overall status** | `operational` · `degraded` · `paused` · `unknown`, plus short reasons | Derived (§3) |
| **Solvency & credit health** | Default rate, overdue share of funded invoices, insurance-pool payout share, an overall `healthy / watch / stressed` level | Indexer database |
| **Oracle & availability** | Oracle uptime and protocol availability over 30 days; oracle circuits tripped right now | On-chain `get_protocol_status()` + accumulated samples |
| **Freshness** | "Last updated" time and an automatic stale-data banner | Generation time |

All rates are **unit-free percentages**. The page never shows a token amount, so
it never has to guess decimals or add amounts across tokens.

### What is deliberately *not* shown

| Excluded | Why |
|---|---|
| Admin address, multisig signers/threshold, pause timestamps | Admin-action data; useful to an attacker, not to the community |
| The admin-action log (`get_recent_admin_actions`) | Explicitly out of scope by the issue |
| Per-address data (reputation, positions, LP identities) | Privacy |
| Raw balances and volumes | Unit ambiguity across tokens; competitive/privacy sensitivity |
| Anything from the internal dashboards | The page has one input: `GET /public/health` |

This is enforced structurally, not by convention:
`indexer/src/services/publicHealthService.ts` builds an **allow-listed** object
field by field — it never spreads a database row or a chain snapshot — and a test
asserts that admin/signer/timestamp data never appears in the output.

---

## 2. Architecture and hosting decision

```
 Soroban RPC ─┐
              ├─► Indexer ──► GET /public/health ──► generate-status-page.ts ──► static files ──► GitHub Pages
 Indexer DB ──┘   (curated)      (allow-listed JSON)     (scheduled, 30 min)     index.html
                                                                                  status.json
                                                                                  history.json
```

**Decision: a static page generated on a schedule, hosted on GitHub Pages.**

| Option | Verdict |
|---|---|
| Live page served by the indexer | Rejected: the page must stay up **when the indexer is down or under load**, which is exactly when people check it. It would also put a public traffic surface on the ops service |
| Third-party status provider | Rejected: adds a vendor and a data-sharing decision; our signals (on-chain pause, oracle circuits) are custom |
| **Static file generated from indexer data** | **Chosen**: no runtime, no database exposure, trivially cacheable, fails safe (§5), portable to any static host |
| Hosting target | GitHub Pages via `actions/deploy-pages`: free, versioned, no credentials to manage. The generator emits plain files, so S3/IPFS is a config change |

Refresh cadence: **every 30 minutes** (`.github/workflows/status-page.yml`,
plus manual `workflow_dispatch`). The page is explicitly **not real-time**, and
says so. For live incident state use the existing `/protocol-status` endpoint.

### Uptime without a database

A static page has no history of its own, so each run appends one small sample
(`{ t, overall, paused, oracleHealthy }`) to `history.json`, which is published
beside the page. The next run fetches it back from `STATUS_PAGE_URL`, appends,
prunes to 35 days and computes 30-day uptime. Samples where the state was
unknown are excluded from both numerator and denominator rather than counted as
up or down. If the previous history cannot be fetched, a fresh history starts
(uptime shows `n/a` until samples accumulate) — it never invents numbers.

---

## 3. How the overall status is decided

| Condition (first match wins) | `overall` |
|---|---|
| Protocol paused | `paused` |
| On-chain status unreadable and no cached value | `unknown` |
| Any oracle circuit tripped, **or** solvency level `stressed` | `degraded` |
| Otherwise | `operational` |

A `watch` solvency level does **not** downgrade the banner (it is shown in the
solvency section), to avoid alarming the public over normal variance.
A stale (cached) chain read is shown as a reason but does not change the state.

### Thresholds

Defaults live in `DEFAULT_THRESHOLDS` in
`indexer/src/services/publicHealthService.ts`. They are **judgement-based
starting points** and should be revisited with real data (§7).

| Indicator | `watch` at | `stressed` at | Reported only when |
|---|---|---|---|
| Default rate (defaulted ÷ resolved) | ≥ 3% | ≥ 8% | ≥ 10 resolved invoices |
| Overdue share (past-due ÷ outstanding funded) | ≥ 10% | ≥ 25% | ≥ 5 outstanding funded invoices |
| Insurance payout share (claims ÷ (balance + claims)) | ≥ 25% | ≥ 60% | Pool data present |

The overall solvency level is the **worst measured** indicator. With no measurable
indicator it is `unknown` ("Not enough data yet") — never a default `healthy`.

---

## 4. Files and commands

| File | Role |
|---|---|
| `indexer/src/services/publicHealthService.ts` | Curated summary + thresholds |
| `indexer/src/api/routes/publicHealth.ts` | `GET /public/health` ([API reference](api-reference.md#get-publichealth)) |
| `indexer/src/statusPage/history.ts` | Sample history and uptime |
| `indexer/src/statusPage/render.ts` | Escaped, self-contained HTML (no external resources; light/dark) |
| `indexer/scripts/generate-status-page.ts` | CLI: fetch → history → render → write |
| `.github/workflows/status-page.yml` | Schedule + Pages deployment |

Generate locally:

```bash
STATUS_INDEXER_URL=http://localhost:3001 \
STATUS_OUT_DIR=status-site \
pnpm --filter @iln/indexer run status-page
# open status-site/index.html
```

| Variable | Purpose |
|---|---|
| `STATUS_INDEXER_URL` | Indexer base URL (required) |
| `STATUS_HISTORY_URL` | Previously published `history.json` (optional) |
| `STATUS_OUT_DIR` | Output directory (default `status-site`) |
| `STATUS_DOCS_URL` | Footer link (optional) |

Tests: `pnpm --filter @iln/indexer exec vitest run tests/publicHealth.test.ts tests/statusPage.test.ts`.

---

## 5. Failure behaviour

| Failure | Behaviour |
|---|---|
| Indexer unreachable / non-200 / wrong shape | Generator exits non-zero, **writes nothing**; the previously published page stays up, and its "last updated" time plus an automatic banner (after 2 h) tell readers it is old |
| Chain RPC down (indexer is up) | `/public/health` still answers `200` with `overall: "unknown"` (or a cached value flagged as stale); the page renders that honestly |
| Previous history missing | Fresh history; uptime `n/a` until samples accumulate |
| Workflow disabled | Page goes stale; banner appears |

The generator deliberately refuses to publish a "green" page it cannot back with data.

---

## 6. Going live

One-time setup (requires repository admin; not done by this change):

1. Enable **GitHub Pages → Source: GitHub Actions** for the repository.
2. Set repository variable `STATUS_INDEXER_URL` to the public indexer base URL.
3. After the first deploy, set `STATUS_PAGE_URL` to the published URL so uptime
   history carries forward.
4. Run the workflow manually once (`workflow_dispatch`) and check the page.
5. Add the published URL to the README and SCF narrative (currently linked to this document; replace with the live URL).

**Prerequisite:** the indexer must be able to read `get_protocol_status()` (a chain
reader configured in `createApp`). Without it `overall` is `unknown` and oracle
uptime stays `n/a`.

---

## 7. Known limitations and review triggers

| Limitation | Note |
|---|---|
| Not real-time (≤ 30 min old) | By design; incident state is `/protocol-status` |
| Oracle "uptime" is **circuit-breaker state sampled every 30 min**, not per-query success | The contract exposes tripped circuits, not query history. A brief trip between samples is missed |
| Default/overdue rates are **counts**, not value-weighted | Amounts span tokens; value-weighting needs price normalisation |
| Thresholds are unvalidated starting points | Revisit after real mainnet data |
| Indexer correctness is assumed | The page is only as accurate as the index; [indexer reconciliation](indexer-reconciliation.md) covers drift |
| One insurance pool | Uses the most recently updated pool unless configured |

Review this page's thresholds and exclusions when: the protocol goes live on
mainnet; a new contract or oracle feed is added; the insurance pool design
changes; a public incident occurs; or `get_protocol_status()` changes shape.

**Privacy review checklist for any change to `/public/health`:** is each new
field aggregate and unit-free? does it reveal an address, key, signer, or
admin action? would an attacker learn something actionable? If unsure, leave it
out.

---

## Related documents

[Monitoring runbook](monitoring-runbook.md) · [Incident response runbook](incident-response-runbook.md) ·
[Indexer operations](indexer-operations.md) · [Insurance pool design](insurance-pool-design.md) ·
[SCF technical narrative](scf-technical-narrative.md) · [API reference](api-reference.md)
