export const config = {
  port: parseInt(process.env.PORT || '3001', 10),
  dbPath: process.env.DB_PATH || './indexer.db',
  cacheTtlMs: parseInt(process.env.CACHE_TTL_MS || '60000', 10),
  maxLeaderboardLimit: 100,
  defaultLeaderboardLimit: 50,
  apiKeys: parseCsv(process.env.API_KEYS),
  horizonUrl: process.env.HORIZON_URL || 'http://localhost:8000',
  contractId: process.env.ILN_CONTRACT_ID || process.env.CONTRACT_ID || '',
  /**
   * When false, this process serves the read API only and does not run Horizon
   * ingestion. Use for horizontally scaled API replicas.
   */
  ingestionEnabled: parseBool(process.env.INGESTION_ENABLED, true),
  /** Anonymous requests per window (production default: 60/min). */
  rateLimitAnonymousMax: parseInt(process.env.RATE_LIMIT_ANON_MAX || '60', 10),
  /** API-key authenticated requests per window (production default: 600/min). */
  rateLimitApiKeyMax: parseInt(process.env.RATE_LIMIT_API_KEY_MAX || '600', 10),
  rateLimitWindowMs: parseInt(process.env.RATE_LIMIT_WINDOW_MS || '60000', 10),
  /** Max GraphQL selection-set depth. */
  graphqlMaxDepth: parseInt(process.env.GRAPHQL_MAX_DEPTH || '8', 10),
  /** Max GraphQL query complexity score. */
  graphqlMaxComplexity: parseInt(process.env.GRAPHQL_MAX_COMPLEXITY || '200', 10),
  /** Ledger lag above this marks health as degraded. */
  healthMaxLagLedgers: parseInt(process.env.HEALTH_MAX_LAG_LEDGERS || '50', 10),
  /**
   * Express `trust proxy` setting. Leave unset when the indexer is directly
   * exposed; set it only when a trusted reverse proxy strips client-supplied
   * forwarding headers. Accepts `true`/`false`, a hop count, or a subnet list.
   */
  trustProxy: parseTrustProxy(process.env.TRUST_PROXY),
  /**
   * Historical catch-up range [CATCHUP_FROM_LEDGER, CATCHUP_TO_LEDGER). Unset
   * FROM disables catch-up; unset TO means "up to the earliest indexed ledger".
   */
  catchupFromLedger: parseLedger(process.env.CATCHUP_FROM_LEDGER),
  catchupToLedger: parseLedger(process.env.CATCHUP_TO_LEDGER),
  catchupBatchSize: parseInt(process.env.CATCHUP_BATCH_SIZE || '1000', 10),
};

function parseLedger(value: string | undefined): number | undefined {
  if (value === undefined || value.trim() === '') {
    return undefined;
  }
  const ledger = Number(value);
  if (!Number.isSafeInteger(ledger) || ledger < 0) {
    throw new Error(`invalid ledger sequence: ${value}`);
  }
  return ledger;
}

function parseCsv(value: string | undefined): string[] {
  return (value || '')
    .split(',')
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0);
}

function parseTrustProxy(
  value: string | undefined
): boolean | number | string | undefined {
  if (value === undefined || value.trim() === '') {
    return undefined;
  }
  const normalized = value.trim();
  if (['1', 'true', 'yes', 'on'].includes(normalized.toLowerCase())) {
    return true;
  }
  if (['0', 'false', 'no', 'off'].includes(normalized.toLowerCase())) {
    return false;
  }
  const hopCount = Number(normalized);
  if (Number.isSafeInteger(hopCount) && hopCount > 0) {
    return hopCount;
  }
  return normalized;
}

function parseBool(value: string | undefined, fallback: boolean): boolean {
  if (value === undefined || value.trim() === '') {
    return fallback;
  }
  const normalized = value.trim().toLowerCase();
  if (['1', 'true', 'yes', 'on'].includes(normalized)) {
    return true;
  }
  if (['0', 'false', 'no', 'off'].includes(normalized)) {
    return false;
  }
  return fallback;
}
