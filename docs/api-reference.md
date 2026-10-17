# ILN Indexer — REST API Reference

A standalone reference for every endpoint exposed by the **ILN Indexer**, the
read-only REST service that indexes Invoice Liquidity Network events from a
Stellar network and serves them over HTTP. It is written for third-party
developers building bots, dashboards, and integrations who need to consume the
API without reading the indexer source.

- **Source of truth:** `indexer/src/api/routes/` (this document is kept in sync
  with those route definitions).
- **Content type:** all responses are `application/json`.
- **Read-only:** every endpoint is `GET`; the indexer never mutates chain state.

> **Tip:** prefer the typed `@iln/sdk` query helpers (`docs/sdk-integration.md`)
> if you are in a TypeScript project. This reference is for direct HTTP access.

---

## Table of contents

- [Base URL](#base-url)
- [Authentication](#authentication)
- [Conventions](#conventions)
- [Common error responses](#common-error-responses)
- [Endpoints](#endpoints)
  - [GET /health](#get-health)
  - [GET /invoices](#get-invoices)
  - [GET /invoices/:id](#get-invoicesid)
  - [GET /stats](#get-stats)
  - [GET /stats/history](#get-statshistory)
  - [GET /reputation/:address](#get-reputationaddress)
  - [GET /leaderboard](#get-leaderboard)
  - [GET /events](#get-events)
  - [GET /public/health](#get-publichealth)
- [Enumerations](#enumerations)
- [Generating an OpenAPI spec](#generating-an-openapi-spec)

---

## Base URL

| Environment | Base URL |
|-------------|----------|
| Local (default) | `http://localhost:3001` |
| Docker Compose | `http://localhost:3000` (mapped in `docker-compose.yml`) |
| Testnet deployment | `https://indexer.testnet.iln.example` (replace with your deployment) |

The listen port is controlled by the `PORT` environment variable (default
`3001`; see `indexer/src/config.ts`). All paths below are relative to the base
URL. Examples use `http://localhost:3001`.

---

## Authentication

The indexer serves **public, read-only** data and **does not require
authentication** by default — every endpoint can be called anonymously.

For production deployments that place the indexer behind an API gateway or
reverse proxy, the recommended convention is a static API key passed in the
`X-API-Key` request header:

```bash
curl -H "X-API-Key: $ILN_API_KEY" http://localhost:3001/stats
```

> The indexer core does **not** validate this header itself; enforcement (and
> rate limiting) is expected to be handled by the gateway in front of it. If
> your deployment requires the header, requests missing or presenting an invalid
> key will receive `401 Unauthorized` from the gateway. Against a bare indexer
> the header is simply ignored.

---

## Conventions

- **Amounts** (`amount`, `amountFunded`, `amountPaid`, `remainingBalance`,
  `totalVolume`, `volumeByToken` values) are returned as **strings** of integer
  stroops to avoid JavaScript number precision loss. Divide by the token's
  decimal scale to display.
- **Timestamps** (`createdAt`, `fundedAt`, `dueDate`, `timestamp`,
  `lastUpdatedAt`) are **Unix seconds** unless noted.
- **`ledger`** values are Stellar ledger sequence numbers.
- **Basis points (bps):** `discountRate`, `effectiveYieldBps`,
  `avgDiscountRateBps` are integers where `100 bps = 1%`.
- **Pagination:** list endpoints accept `page` (1-based) and `pageSize` and
  return `{ ..., total, page, pageSize }`. `page`/`pageSize` below `1` are
  clamped to `1`.

---

## Common error responses

| Status | Body | Meaning |
|--------|------|---------|
| `400 Bad Request` | `{ "error": "<reason>" }` | Invalid path or query parameter (e.g. non-numeric invoice id, unknown `period`, missing required `address`). |
| `404 Not Found` | `{ "error": "Invoice not found" }` | Resource does not exist (only `GET /invoices/:id`). |
| `401 Unauthorized` | `{ "error": "unauthorized" }` | Only when deployed behind an API-key gateway (see [Authentication](#authentication)). |
| `500 Internal Server Error` | `{ "error": "..." }` | Unexpected server/database error. |

Unknown reputation addresses are **not** an error — `GET /reputation/:address`
returns a zeroed record with `200 OK`.

---

## Endpoints

### GET /health

Liveness probe.

| | |
|---|---|
| **Method / Path** | `GET /health` |
| **Query parameters** | none |
| **Request body** | none |

**Response `200 OK`**

```json
{ "status": "ok" }
```

**curl**

```bash
curl http://localhost:3001/health
```

---

### GET /invoices

List invoices with filtering, sorting, and pagination.

| | |
|---|---|
| **Method / Path** | `GET /invoices` |
| **Caching** | `Cache-Control: max-age=10` |

**Query parameters**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `state` | string | — | Filter by status (see [Invoice status](#invoice-status)). |
| `token` | string | — | Filter by token contract id / symbol. |
| `submitter` | string | — | Filter by freelancer (submitter) `G...` address. |
| `page` | integer | `1` | 1-based page number. |
| `pageSize` | integer | `20` | Items per page. |
| `sortBy` | string | `createdAt` | One of `createdAt`, `dueDate`, `amount`, `discountRate`. Unknown values fall back to `createdAt`. |
| `sortOrder` | string | `desc` | `asc` or `desc`. |

**Response `200 OK`**

```json
{
  "invoices": [
    {
      "id": 42,
      "freelancer": "GAAA...FREELANCER",
      "payer": "GBBB...PAYER",
      "token": "CDLZ...USDC",
      "amount": "1000000",
      "dueDate": 1735689600,
      "discountRate": 500,
      "status": "Funded",
      "funder": "GCCC...FUNDER",
      "fundedAt": 1733097600,
      "amountFunded": "950000",
      "amountPaid": "0",
      "referralCode": null,
      "submitterReputation": 72,
      "createdAt": 1733000000,
      "effectiveYieldBps": 41,
      "remainingBalance": "950000",
      "daysUntilExpiry": 12
    }
  ],
  "total": 137,
  "page": 1,
  "pageSize": 20
}
```

Each list item is the invoice **summary** (the per-invoice `events` array is
omitted; fetch a single invoice for its event log). Field meanings:

| Field | Type | Description |
|-------|------|-------------|
| `id` | integer | Invoice id. |
| `freelancer` | string | Submitter `G...` address. |
| `payer` | string | Payer `G...` address. |
| `token` | string | Token contract id. |
| `amount` | string | Face value, stroops. |
| `dueDate` | integer | Unix seconds. |
| `discountRate` | integer | Discount in bps. |
| `status` | string | See [Invoice status](#invoice-status). |
| `funder` | string \| null | Funder address, or `null` if unfunded. |
| `fundedAt` | integer \| null | Funding time, Unix seconds. |
| `amountFunded` | string | Amount advanced to the freelancer, stroops. |
| `amountPaid` | string | Amount repaid by the payer, stroops. |
| `referralCode` | string \| null | Hex referral code, if any. |
| `submitterReputation` | integer | Submitter reputation score at index time. |
| `createdAt` | integer | Submission time, Unix seconds. |
| `effectiveYieldBps` | integer | Annualised yield to expiry, bps (derived). |
| `remainingBalance` | string | `amountFunded - amountPaid` (floored at 0), stroops. |
| `daysUntilExpiry` | integer | Whole days until `dueDate` (floored at 0). |

**curl**

```bash
# Funded USDC invoices, newest first
curl "http://localhost:3001/invoices?state=Funded&token=CDLZ...USDC&sortBy=createdAt&sortOrder=desc"

# Page 2, 50 per page, sorted by amount ascending
curl "http://localhost:3001/invoices?page=2&pageSize=50&sortBy=amount&sortOrder=asc"
```

---

### GET /invoices/:id

Fetch one invoice including its full event log.

| | |
|---|---|
| **Method / Path** | `GET /invoices/:id` |

**Path parameters**

| Name | Type | Description |
|------|------|-------------|
| `id` | integer | Invoice id. Must be numeric. |

**Response `200 OK`** — all [summary fields](#get-invoices) plus:

| Field | Type | Description |
|-------|------|-------------|
| `events` | array | Chronological (oldest-first) event log for this invoice. |
| `events[].type` | string | Event type (see [Event types](#event-types)). |
| `events[].ledger` | integer | Ledger sequence the event was emitted in. |
| `events[].timestamp` | integer | Unix seconds. |
| `events[].data` | object | Raw decoded event payload. |

```json
{
  "id": 42,
  "freelancer": "GAAA...FREELANCER",
  "payer": "GBBB...PAYER",
  "token": "CDLZ...USDC",
  "amount": "1000000",
  "dueDate": 1735689600,
  "discountRate": 500,
  "status": "Funded",
  "funder": "GCCC...FUNDER",
  "fundedAt": 1733097600,
  "amountFunded": "950000",
  "amountPaid": "0",
  "referralCode": null,
  "submitterReputation": 72,
  "createdAt": 1733000000,
  "effectiveYieldBps": 41,
  "remainingBalance": "950000",
  "daysUntilExpiry": 12,
  "events": [
    { "type": "submitted", "ledger": 100234, "timestamp": 1733000000, "data": { "amount": "1000000" } },
    { "type": "funded",    "ledger": 100890, "timestamp": 1733097600, "data": { "amountFunded": "950000" } }
  ]
}
```

**Errors**

| Status | Body | When |
|--------|------|------|
| `400` | `{ "error": "Invalid invoice ID" }` | `:id` is not numeric. |
| `404` | `{ "error": "Invoice not found" }` | No invoice with that id. |

**curl**

```bash
curl http://localhost:3001/invoices/42
```

---

### GET /stats

Aggregate protocol statistics (cached for 60 s server-side).

| | |
|---|---|
| **Method / Path** | `GET /stats` |
| **Query parameters** | none |

**Response `200 OK`**

```json
{
  "totalInvoices": 1372,
  "totalFunded": 1041,
  "totalPaid": 905,
  "totalCancelled": 60,
  "totalExpired": 42,
  "totalDisputed": 8,
  "volumeByToken": { "CDLZ...USDC": "918340000", "CXLM...XLM": "12000000000" },
  "avgDiscountRateBps": 487,
  "disputeRate": 0.0058,
  "lastUpdatedAt": 1733184000
}
```

| Field | Type | Description |
|-------|------|-------------|
| `totalInvoices` | integer | All invoices ever submitted. |
| `totalFunded` | integer | Invoices that reached `Funded`, `Paid`, or `Defaulted`. |
| `totalPaid` | integer | Invoices in `Paid`. |
| `totalCancelled` | integer | Invoices in `Cancelled`. |
| `totalExpired` | integer | Invoices in `Expired`. |
| `totalDisputed` | integer | Invoices in `Disputed`. |
| `volumeByToken` | object | Map of token id → total repaid volume (stroops, string). |
| `avgDiscountRateBps` | integer | Mean discount rate across invoices, bps. |
| `disputeRate` | number | `totalDisputed / totalInvoices`, rounded to 4 dp. |
| `lastUpdatedAt` | integer | When the stats snapshot was computed (Unix ms). |

**curl**

```bash
curl http://localhost:3001/stats
```

---

### GET /stats/history

Time series of daily protocol stats.

| | |
|---|---|
| **Method / Path** | `GET /stats/history` |

**Query parameters**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `period` | string | `30d` | One of `30d`, `90d`, `all`. |

**Response `200 OK`** — array of daily entries (oldest-first):

```json
[
  {
    "date": "2025-12-01",
    "totalInvoices": 28,
    "totalFunded": 21,
    "totalPaid": 18,
    "totalVolume": "21450000",
    "avgDiscountRateBps": 462
  }
]
```

| Field | Type | Description |
|-------|------|-------------|
| `date` | string | `YYYY-MM-DD`. |
| `totalInvoices` | integer | Invoices submitted that day. |
| `totalFunded` | integer | Invoices funded that day. |
| `totalPaid` | integer | Invoices paid that day. |
| `totalVolume` | string | Volume that day, stroops. |
| `avgDiscountRateBps` | integer | Mean discount rate that day, bps. |

**Errors**

| Status | Body | When |
|--------|------|------|
| `400` | `{ "error": "Invalid period. Use 30d, 90d, or all" }` | `period` not in the allowed set. |

**curl**

```bash
curl "http://localhost:3001/stats/history?period=90d"
```

---

### GET /reputation/:address

Reputation score and history for an account.

| | |
|---|---|
| **Method / Path** | `GET /reputation/:address` |

**Path parameters**

| Name | Type | Description |
|------|------|-------------|
| `address` | string | `G...` account address. |

**Query parameters**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `historyPeriod` | string | `all` | `all`, `30d`, or `90d`. Filters the `history` array. |

**Response `200 OK`**

```json
{
  "score": 100,
  "invoicesPaid": 12,
  "invoicesDefaulted": 0,
  "invoicesSubmitted": 12,
  "lastActivityLedger": 100890,
  "history": [
    { "ledger": 100234, "score": 60, "eventType": "invoice_paid", "timestamp": 1733000000 },
    { "ledger": 100890, "score": 100, "eventType": "invoice_paid", "timestamp": 1733097600 }
  ]
}
```

| Field | Type | Description |
|-------|------|-------------|
| `score` | integer | Current reputation score. |
| `invoicesPaid` | integer | Lifetime invoices paid. |
| `invoicesDefaulted` | integer | Lifetime invoices defaulted. |
| `invoicesSubmitted` | integer | Lifetime invoices submitted. |
| `lastActivityLedger` | integer | Ledger of the most recent reputation update. |
| `history` | array | Score changes over time (oldest-first), within `historyPeriod`. |

> **Unknown address:** returns `200 OK` with all counters `0` and an empty
> `history` (not `404`).

**curl**

```bash
curl "http://localhost:3001/reputation/GBBB...PAYER?historyPeriod=30d"
```

---

### GET /leaderboard

Top accounts ranked by reputation score.

| | |
|---|---|
| **Method / Path** | `GET /leaderboard` |

**Query parameters**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `limit` | integer | `50` | Number of entries. Clamped to a maximum of `100`. |
| `token` | string | — | Restrict to accounts that have transacted in this token. |

**Response `200 OK`** — array ordered by descending score:

```json
[
  {
    "rank": 1,
    "address": "GBBB...PAYER",
    "score": 100,
    "invoicesPaid": 12,
    "invoicesDefaulted": 0,
    "totalVolume": "918340000"
  }
]
```

| Field | Type | Description |
|-------|------|-------------|
| `rank` | integer | 1-based rank in this result set. |
| `address` | string | Account address. |
| `score` | integer | Reputation score. |
| `invoicesPaid` | integer | Lifetime invoices paid. |
| `invoicesDefaulted` | integer | Lifetime invoices defaulted. |
| `totalVolume` | string | Total paid volume attributed to the account, stroops. |

**Errors**

| Status | Body | When |
|--------|------|------|
| `400` | `{ "error": "Invalid limit parameter" }` | `limit` is non-numeric or `< 1`. |

**curl**

```bash
curl "http://localhost:3001/leaderboard?limit=10"
curl "http://localhost:3001/leaderboard?limit=25&token=CDLZ...USDC"
```

---

### GET /events

Activity feed for an account across every invoice it participates in (as
freelancer, payer, or funder).

| | |
|---|---|
| **Method / Path** | `GET /events` |

> The path `/events` is also served as a **WebSocket** endpoint for live event
> streaming. This section documents the HTTP `GET` (historical/paginated) form.

**Query parameters**

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `address` | string | — | **Required.** `G...` account to fetch events for. |
| `types` | string | — | Comma-separated list of [event types](#event-types) to include (e.g. `submitted,funded`). |
| `page` | integer | `1` | 1-based page number. |
| `pageSize` | integer | `20` | Items per page. |

**Response `200 OK`** — newest-first:

```json
{
  "events": [
    {
      "id": 9001,
      "invoiceId": 42,
      "type": "funded",
      "ledger": 100890,
      "timestamp": 1733097600,
      "data": { "funder": "GCCC...FUNDER", "amountFunded": "950000" }
    }
  ],
  "total": 57,
  "page": 1,
  "pageSize": 20
}
```

| Field | Type | Description |
|-------|------|-------------|
| `events[].id` | integer | Event id. |
| `events[].invoiceId` | integer | Invoice the event belongs to. |
| `events[].type` | string | Event type. |
| `events[].ledger` | integer | Ledger sequence. |
| `events[].timestamp` | integer | Unix seconds. |
| `events[].data` | object | Decoded event payload. |

**Errors**

| Status | Body | When |
|--------|------|------|
| `400` | `{ "error": "Address parameter is required" }` | `address` omitted. |

**curl**

```bash
# All events for an account
curl "http://localhost:3001/events?address=GBBB...PAYER"

# Only funded + paid events, second page
curl "http://localhost:3001/events?address=GBBB...PAYER&types=funded,paid&page=2&pageSize=20"
```

---

### GET /public/health

Curated, non-sensitive protocol-health summary. It is the only data source for the
[public status page](public-status-page.md) and is safe to expose publicly. It never
returns admin or signer addresses, multisig configuration, admin-action history,
per-address data or raw amounts.

| | |
|---|---|
| **Method / Path** | `GET /public/health` |
| **Query parameters** | none |
| **Request body** | none |

Always `200`. If the on-chain status cannot be read, `overall` is `"unknown"`.

**Response `200 OK`**

```json
{
  "schemaVersion": 1,
  "generatedAt": "2027-01-15T12:00:00.000Z",
  "overall": "operational",
  "reasons": [],
  "protocol": { "paused": false, "stale": false },
  "solvency": {
    "level": "healthy",
    "fundedInvoices": 120,
    "settledInvoices": 110,
    "defaultedInvoices": 2,
    "defaultRatePct": 1.8,
    "outstandingFundedInvoices": 8,
    "overdueFundedInvoices": 0,
    "overdueSharePct": 0,
    "insurancePool": { "enrolledLps": 12, "claimsSharePct": 4.2 }
  },
  "oracle": { "circuitsTripped": 0, "healthy": true }
}
```

`overall` is one of `operational`, `degraded`, `paused`, `unknown`. `solvency.level` is
`healthy`, `watch`, `stressed` or `unknown`; rate fields are `null` until enough invoices
exist (see [public-status-page.md](public-status-page.md#thresholds)).

**curl**

```bash
curl http://localhost:3001/public/health
```

---

## Enumerations

### Invoice status

`Pending` · `Funded` · `Paid` · `Cancelled` · `Expired` · `Disputed` ·
`Defaulted`

### Event types

`submitted` · `funded` · `paid` (and other lifecycle events such as
`cancelled`, `expired`, `disputed`, `defaulted` as emitted by the contracts).
See [`docs/events.md`](events.md) for the full event catalogue and payload
schemas.

---

## Generating an OpenAPI spec

This reference is hand-maintained against `indexer/src/api/routes/`. To produce
a machine-readable OpenAPI 3 document you can annotate the route handlers with
[`swagger-jsdoc`](https://github.com/Surnet/swagger-jsdoc) JSDoc blocks and
serve them with [`swagger-ui-express`](https://github.com/scottie1984/swagger-ui-express):

```bash
cd indexer
npm install --save-dev swagger-jsdoc swagger-ui-express
```

```ts
// indexer/src/api/openapi.ts
import swaggerJSDoc from 'swagger-jsdoc';

export const openapiSpec = swaggerJSDoc({
  definition: {
    openapi: '3.0.0',
    info: { title: 'ILN Indexer API', version: '0.1.0' },
    servers: [{ url: 'http://localhost:3001' }],
  },
  apis: ['./src/api/routes/*.ts'], // read @openapi JSDoc from the route files
});
```

```ts
// add to indexer/src/app.ts
import swaggerUi from 'swagger-ui-express';
import { openapiSpec } from './api/openapi.js';
app.use('/docs', swaggerUi.serve, swaggerUi.setup(openapiSpec));
app.get('/openapi.json', (_req, res) => res.json(openapiSpec));
```

Then annotate each handler, e.g. in `indexer/src/api/routes/stats.ts`:

```ts
/**
 * @openapi
 * /stats:
 *   get:
 *     summary: Aggregate protocol statistics
 *     responses:
 *       200:
 *         description: Protocol stats snapshot
 */
router.get('/stats', (_req, res) => { /* ... */ });
```

Interactive docs are then served at `/docs` and the raw spec at `/openapi.json`.

---

_Validate examples against your live deployment, e.g. the testnet indexer, by
substituting the [base URL](#base-url) and a real address/invoice id._
