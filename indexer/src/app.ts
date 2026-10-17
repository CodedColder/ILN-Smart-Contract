import express from 'express';
import type Database from 'better-sqlite3';
import { createLeaderboardRouter } from './api/routes/leaderboard.js';
import { createReputationRouter } from './api/routes/reputation.js';
import { createStatsRouter } from './api/routes/stats.js';
import { createInvoicesRouter } from './api/routes/invoices.js';
import { createInsuranceRouter } from './api/routes/insurance.js';
import { config } from './config.js';
import { createApiKeyMiddleware } from './middleware/apiKey.js';
import { createRateLimitMiddleware } from './middleware/rateLimit.js';
import { createEventsRouter } from './api/routes/events.js';
import { createMonitoringRouter } from './api/routes/monitoring.js';
import { createHealthRouter, type HealthCheckDeps } from './api/routes/health.js';
import { createProtocolStatusRouter } from './api/routes/protocolStatus.js';
import { createPublicHealthRouter } from './api/routes/publicHealth.js';
import {
  createProtocolStatusService,
  type ProtocolStatusService,
} from './services/protocolStatusService.js';
import type { ChainReader } from './reconciliation/chainReader.js';
import { createRequestIdMiddleware } from './middleware/requestId.js';
import { mountGraphQL } from './api/graphql/index.js';

export interface CreateAppOptions {
  apiKeys?: string[];
  /** @deprecated Prefer rateLimitAnonymousMax / rateLimitApiKeyMax. */
  rateLimitMax?: number;
  rateLimitAnonymousMax?: number;
  rateLimitApiKeyMax?: number;
  rateLimitWindowMs?: number;
  graphqlMaxDepth?: number;
  graphqlMaxComplexity?: number;
  health?: HealthCheckDeps;
  /**
   * Optional on-chain reader for the public `/protocol-status` endpoint
   * (Issue #775). When omitted, `/protocol-status` responds `503
   * { configured: false }`.
   */
  chainReader?: Pick<ChainReader, 'getProtocolStatus'>;
  /** Pre-built protocol-status service (overrides `chainReader`); for tests. */
  protocolStatusService?: ProtocolStatusService;
}

export function createApp(
  db: Database.Database,
  options: CreateAppOptions = {}
): express.Express {
  const app = express();
  const apiKeys = options.apiKeys ?? config.apiKeys;
  const rateLimitWindowMs = options.rateLimitWindowMs ?? config.rateLimitWindowMs;
  const rateLimitAnonymousMax =
    options.rateLimitAnonymousMax ??
    options.rateLimitMax ??
    config.rateLimitAnonymousMax;
  const rateLimitApiKeyMax =
    options.rateLimitApiKeyMax ??
    (options.rateLimitMax !== undefined
      ? options.rateLimitMax * 10
      : config.rateLimitApiKeyMax);

  // Correlation-ID first so every downstream log line for the request is
  // tagged (Issue #776).
  app.use(createRequestIdMiddleware());
  app.use(express.json());
  // API key auth must run before rate limiting so authenticated traffic gets
  // the higher tier and is keyed by API key rather than IP.
  app.use(createApiKeyMiddleware(apiKeys));
  app.use(
    createRateLimitMiddleware({
      anonymousLimit: rateLimitAnonymousMax,
      apiKeyLimit: rateLimitApiKeyMax,
      windowMs: rateLimitWindowMs,
      // `/protocol-status` is a public incident endpoint external monitors
      // poll frequently — exclude it from rate limiting like `/health`.
      skipPaths: ['/health', '/protocol-status'],
    })
  );

  // Health is registered early and is excluded from rate limiting so external
  // uptime checks are not throttled. Rate-limit middleware still runs first
  // but skips /health via skipPaths.
  const healthDeps: HealthCheckDeps = {
    horizonUrl: options.health?.horizonUrl ?? config.horizonUrl,
    maxLagLedgers: options.health?.maxLagLedgers ?? config.healthMaxLagLedgers,
    ingestionEnabled:
      options.health?.ingestionEnabled ?? config.ingestionEnabled,
  };
  if (options.health?.fetchImpl) {
    healthDeps.fetchImpl = options.health.fetchImpl;
  }
  if (options.health?.getChainTipLedger) {
    healthDeps.getChainTipLedger = options.health.getChainTipLedger;
  }
  app.use(createHealthRouter(db, healthDeps));

  const protocolStatusService =
    options.protocolStatusService ??
    createProtocolStatusService({ reader: options.chainReader });
  app.use(createProtocolStatusRouter(protocolStatusService));
  // Curated public summary feeding the static status page (Issue #892).
  app.use(createPublicHealthRouter(db, protocolStatusService));

  app.use(createLeaderboardRouter(db));
  app.use(createReputationRouter(db));
  app.use(createStatsRouter(db));
  app.use(createInvoicesRouter(db));
  app.use(createInsuranceRouter(db));
  app.use(createEventsRouter(db));
  app.use('/monitoring', createMonitoringRouter(db));
  mountGraphQL(app, db, {
    maxDepth: options.graphqlMaxDepth ?? config.graphqlMaxDepth,
    maxComplexity: options.graphqlMaxComplexity ?? config.graphqlMaxComplexity,
  });

  return app;
}
