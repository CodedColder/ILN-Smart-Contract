# ILN Indexer

REST API indexer for the Invoice Liquidity Network smart contracts. Indexes Horizon events from a Stellar network and exposes them via HTTP.

## Local Development

### Prerequisites
- Node.js 20+
- npm 10+

### Setup

```bash
cd indexer
npm install
npm run dev
```

The indexer will start on `http://localhost:3000` and create a local SQLite database at `./data/indexer.db`.

### Running Tests

```bash
# Run tests
npm test

# Run tests with coverage report
npm run test:coverage

# Run tests in watch mode
npm run test:watch
```

## Docker Deployment

### Building the Docker Image

The Dockerfile uses a multi-stage build to optimize image size:

```bash
docker build -t iln/indexer:latest .
```

Image size is optimized to be under 200MB through:
- Alpine Linux base image (lightweight)
- Multi-stage build (production deps only in runtime)
- npm prune to remove dev dependencies

### Using Docker Compose

To run the full stack including Stellar, indexer, and notifications:

```bash
docker compose up -d
```

This will:
- Start Stellar quickstart on port 8000
- Start the indexer on port 3000 with a local SQLite database
- Start the notifications service on port 3001 with PostgreSQL

Check health status:
```bash
docker compose logs -f indexer | grep "health\|error"
```

Stop all services:
```bash
docker compose down
```

### Environment Variables

- `PORT` - Server port (default: 3000)
- `HORIZON_URL` - Stellar Horizon URL (default: http://localhost:8000)
- `CONTRACT_ID` or `ILN_CONTRACT_ID` - ILN contract address used for Horizon event ingestion
- `DB_PATH` - SQLite database file path (default: ./data/indexer.db)
- `LOG_LEVEL` - Logging level (default: info)
- `API_KEYS` - Optional comma-separated list of `X-API-Key` values (higher rate-limit tier)
- `RATE_LIMIT_ANON_MAX` - Anonymous requests per window (default `60`)
- `RATE_LIMIT_API_KEY_MAX` - API-key requests per window (default `600`)
- `RATE_LIMIT_WINDOW_MS` - Rate-limit window in ms (default `60000`)
- `TRUST_PROXY` - Express `trust proxy` value (e.g. `1`, `true`, or a subnet list). Leave unset when directly exposed so `X-Forwarded-For` cannot spoof rate-limit identity.
- `GRAPHQL_MAX_DEPTH` / `GRAPHQL_MAX_COMPLEXITY` - GraphQL abuse limits (defaults `8` / `200`)
- `INGESTION_ENABLED` - Set `false` on read-only API replicas (default `true`)
- `HEALTH_MAX_LAG_LEDGERS` - Lag threshold for degraded health (default `50`)
- `CATCHUP_FROM_LEDGER` - Enables historical catch-up from this ledger (inclusive) before live streaming
- `CATCHUP_TO_LEDGER` - Catch-up end ledger (exclusive; default: earliest ledger already indexed)
- `CATCHUP_BATCH_SIZE` - Ledgers per checkpointed catch-up batch (default `1000`)

### Historical catch-up, replay and recovery

- **Catch-up** backfills `[CATCHUP_FROM_LEDGER, CATCHUP_TO_LEDGER)` in checkpointed batches. A batch's checkpoint advances only after all of its rows commit; failed, interrupted, or gap-blocked runs resume from the checkpoint on the next start, and ledgers Horizon cannot serve are reported as a gap instead of being marked complete.
- **Replay** (`npm run replay -- --from-ledger N [--to-ledger M]`, range `[N, M)`) re-processes history idempotently, commits each transaction atomically with its cursor, retries transient Horizon errors, and stops with a resume ledger on any unrecoverable failure.

Operator runbook: [docs/indexer-operations.md §5–§5a](../docs/indexer-operations.md#5-checkpoint-replay-from-an-arbitrary-ledger).

### Health Check

The health endpoint is available at:
```
GET /health
```

Returns DB, Horizon, ingestion lag/leader signals. See [monitoring-runbook.md](../docs/monitoring-runbook.md) and [indexer-ha.md](../docs/indexer-ha.md).

## API Endpoints

- `GET /invoices` - List all invoices (filter, sort, paginate)
- `GET /invoices/:id` - Single invoice with its event log
- `GET /stats` - Global statistics
- `GET /stats/history` - Daily stats time series
- `GET /reputation/:address` - Reputation score and history
- `GET /leaderboard` - Top contributors
- `GET /events` - Account activity feed
- `GET /health` - Health check

See the **[REST API Reference](../docs/api-reference.md)** for the complete
specification: query parameters, request/response schemas, error codes,
authentication, and a curl example for every endpoint.

## Production Considerations

- Use a managed Postgres or SQLite backup for the database
- Configure appropriate log retention
- Set up monitoring on the `/health` endpoint
- Use environment variables for all configuration
- Run with a proper process manager (systemd, PM2, Docker, etc.)
