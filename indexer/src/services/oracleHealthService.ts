/**
 * Oracle health monitoring dashboard (issue #887).
 *
 * Consumes oracle health-check state and circuit-breaker trip events
 * to track latency, trip frequency, and denial rate per feed. Routes
 * repeated trips through the shared alerting infrastructure.
 */

import type Database from 'better-sqlite3';
import { AlertRouter, createAlert, getAlertRouter } from './alertRouter.js';
import { logger } from '../lib/logger.js';

// ── Types ────────────────────────────────────────────────────────────────────

export interface OracleHealthRecord {
  feedType: string;
  token: string;
  latencyLedgers: number;
  isHealthy: boolean;
  circuitTripped: boolean;
  lastCheckedAt: number;
  tripCount: number;
}

export interface OracleTripEvent {
  feedType: string;
  token: string;
  timestamp: number;
  ledger: number;
  type: 'tripped' | 'reset';
}

export interface OracleFeedHealth {
  feedType: string;
  token: string;
  currentLatency: number | null;
  isHealthy: boolean;
  circuitTripped: boolean;
  tripCount30d: number;
  tripCount90d: number;
  denialRate30d: number;
  lastCheckedAt: number;
}

export interface OracleHealthDashboard {
  feeds: OracleFeedHealth[];
  aggregateMetrics: {
    totalFeeds: number;
    healthyFeeds: number;
    trippedFeeds: number;
    avgLatency: number;
    totalTrips30d: number;
    totalTrips90d: number;
    overallDenialRate: number;
  };
  recentTrips: OracleTripEvent[];
  healthyRanges: {
    maxLatencyLedgers: number;
    maxDenialRate: number;
    maxTripsPerDay: number;
  };
  lastUpdatedAt: number;
}

export interface OracleHealthConfig {
  /** Maximum latency in ledgers considered healthy. */
  maxLatencyLedgers: number;
  /** Maximum denial rate considered healthy (0-1). */
  maxDenialRate: number;
  /** Maximum trips per day before alerting. */
  maxTripsPerDay: number;
  /** Number of recent trips to return. */
  recentTripsLimit: number;
}

const DEFAULT_CONFIG: OracleHealthConfig = {
  maxLatencyLedgers: 5,
  maxDenialRate: 0.05,
  maxTripsPerDay: 3,
  recentTripsLimit: 20,
};

// ── Service ──────────────────────────────────────────────────────────────────

/**
 * Get health status for all oracle feeds from the database.
 */
export function getOracleFeedHealth(
  db: Database.Database,
  config: OracleHealthConfig = DEFAULT_CONFIG
): OracleFeedHealth[] {
  const sinceUnix30d = Math.floor(Date.now() / 1000) - 30 * 24 * 60 * 60;
  const sinceUnix90d = Math.floor(Date.now() / 1000) - 90 * 24 * 60 * 60;

  // Get distinct feed/token pairs from events
  const feedPairs = db
    .prepare(
      `
      SELECT DISTINCT
        JSON_extract(data, '$.feed_type') AS feed_type,
        JSON_extract(data, '$.token') AS token
      FROM events
      WHERE event_type LIKE '%Oracle%'
         OR contract_event_type LIKE '%Oracle%'
         OR event_type LIKE '%oracle%'
      UNION
      SELECT DISTINCT
        JSON_extract(data, '$.feedType') AS feed_type,
        JSON_extract(data, '$.token') AS token
      FROM events
      WHERE event_type LIKE '%Oracle%'
         OR contract_event_type LIKE '%Oracle%'
         OR event_type LIKE '%oracle%'
      `
    )
    .all() as Array<{ feed_type: string | null; token: string | null }>;

  const feeds: OracleFeedHealth[] = [];

  for (const pair of feedPairs) {
    if (!pair.feed_type || !pair.token) continue;

    const feedType = pair.feed_type;
    const token = pair.token;

    // Get trip counts
    const trips30d = db
      .prepare(
        `
        SELECT COUNT(*) AS count
        FROM events
        WHERE (event_type LIKE '%OracleCircuitTripped%'
           OR contract_event_type LIKE '%OracleCircuitTripped%')
          AND JSON_extract(data, '$.feedType') = ?
          AND JSON_extract(data, '$.token') = ?
          AND timestamp >= ?
      `
      )
      .get(feedType, token, sinceUnix30d) as { count: number };

    const trips90d = db
      .prepare(
        `
        SELECT COUNT(*) AS count
        FROM events
        WHERE (event_type LIKE '%OracleCircuitTripped%'
           OR contract_event_type LIKE '%OracleCircuitTripped%')
          AND JSON_extract(data, '$.feedType') = ?
          AND JSON_extract(data, '$.token') = ?
          AND timestamp >= ?
      `
      )
      .get(feedType, token, sinceUnix90d) as { count: number };

    // Get denial rate (circuit tripped events / total health checks)
    const totalChecks = db
      .prepare(
        `
        SELECT COUNT(*) AS count
        FROM events
        WHERE (event_type LIKE '%OracleHealth%'
           OR contract_event_type LIKE '%OracleHealth%')
          AND JSON_extract(data, '$.feedType') = ?
          AND JSON_extract(data, '$.token') = ?
          AND timestamp >= ?
      `
      )
      .get(feedType, token, sinceUnix30d) as { count: number };

    const denials = db
      .prepare(
        `
        SELECT COUNT(*) AS count
        FROM events
        WHERE (event_type LIKE '%OracleCircuitTripped%'
           OR contract_event_type LIKE '%OracleCircuitTripped%')
          AND JSON_extract(data, '$.feedType') = ?
          AND JSON_extract(data, '$.token') = ?
          AND timestamp >= ?
      `
      )
      .get(feedType, token, sinceUnix30d) as { count: number };

    // Get latest latency
    const latestEvent = db
      .prepare(
        `
        SELECT
          JSON_extract(data, '$.latencyLedgers') AS latency,
          JSON_extract(data, '$.isHealthy') AS is_healthy,
          JSON_extract(data, '$.circuitTripped') AS circuit_tripped,
          timestamp
        FROM events
        WHERE (event_type LIKE '%OracleHealth%'
           OR contract_event_type LIKE '%OracleHealth%'
           OR event_type LIKE '%oracle_health%')
          AND JSON_extract(data, '$.feedType') = ?
          AND JSON_extract(data, '$.token') = ?
        ORDER BY timestamp DESC
        LIMIT 1
      `
      )
      .get(feedType, token) as {
      latency: number | null;
      is_healthy: number | null;
      circuit_tripped: number | null;
      timestamp: number;
    } | undefined;

    feeds.push({
      feedType,
      token,
      currentLatency: latestEvent?.latency ?? null,
      isHealthy: latestEvent?.is_healthy === 1,
      circuitTripped: latestEvent?.circuit_tripped === 1,
      tripCount30d: trips30d.count,
      tripCount90d: trips90d.count,
      denialRate30d:
        totalChecks.count > 0
          ? Math.round((denials.count / totalChecks.count) * 1_000_000) / 1_000_000
          : 0,
      lastCheckedAt: latestEvent?.timestamp ?? 0,
    });
  }

  return feeds;
}

/**
 * Get recent oracle circuit-breaker trip events.
 */
export function getRecentOracleTrips(
  db: Database.Database,
  limit: number = 20
): OracleTripEvent[] {
  const rows = db
    .prepare(
      `
      SELECT
        JSON_extract(data, '$.feedType') AS feed_type,
        JSON_extract(data, '$.token') AS token,
        timestamp,
        ledger,
        CASE
          WHEN event_type LIKE '%Tripped%' OR contract_event_type LIKE '%Tripped%'
            THEN 'tripped'
          ELSE 'reset'
        END AS type
      FROM events
      WHERE event_type LIKE '%OracleCircuit%'
         OR contract_event_type LIKE '%OracleCircuit%'
      ORDER BY timestamp DESC
      LIMIT ?
    `
    )
    .all(limit) as Array<{
    feed_type: string;
    token: string;
    timestamp: number;
    ledger: number;
    type: string;
  }>;

  return rows.map((row) => ({
    feedType: row.feed_type,
    token: row.token,
    timestamp: row.timestamp,
    ledger: row.ledger,
    type: row.type as 'tripped' | 'reset',
  }));
}

/**
 * Build the full oracle health monitoring dashboard.
 */
export function getOracleHealthDashboard(
  db: Database.Database,
  config: OracleHealthConfig = DEFAULT_CONFIG
): OracleHealthDashboard {
  const feeds = getOracleFeedHealth(db, config);
  const recentTrips = getRecentOracleTrips(db, config.recentTripsLimit);

  const totalFeeds = feeds.length;
  const healthyFeeds = feeds.filter((f) => f.isHealthy).length;
  const trippedFeeds = feeds.filter((f) => f.circuitTripped).length;
  const avgLatency =
    feeds.filter((f) => f.currentLatency !== null).length > 0
      ? feeds.reduce((sum, f) => sum + (f.currentLatency ?? 0), 0) /
        feeds.filter((f) => f.currentLatency !== null).length
      : 0;
  const totalTrips30d = feeds.reduce((sum, f) => sum + f.tripCount30d, 0);
  const totalTrips90d = feeds.reduce((sum, f) => sum + f.tripCount90d, 0);
  const overallDenialRate =
    feeds.length > 0
      ? feeds.reduce((sum, f) => sum + f.denialRate30d, 0) / feeds.length
      : 0;

  const dashboard: OracleHealthDashboard = {
    feeds,
    aggregateMetrics: {
      totalFeeds,
      healthyFeeds,
      trippedFeeds,
      avgLatency: Math.round(avgLatency * 1_000_000) / 1_000_000,
      totalTrips30d,
      totalTrips90d,
      overallDenialRate: Math.round(overallDenialRate * 1_000_000) / 1_000_000,
    },
    recentTrips,
    healthyRanges: {
      maxLatencyLedgers: config.maxLatencyLedgers,
      maxDenialRate: config.maxDenialRate,
      maxTripsPerDay: config.maxTripsPerDay,
    },
    lastUpdatedAt: Date.now(),
  };

  logger.info('Oracle health dashboard computed', {
    totalFeeds,
    healthyFeeds,
    trippedFeeds,
    totalTrips30d,
  });

  return dashboard;
}

/**
 * Process an oracle circuit-breaker event and route through alerting.
 * Call this when an OracleCircuitTripped or OracleCircuitReset event
 * is emitted on-chain.
 */
export async function handleOracleCircuitEvent(
  event: OracleTripEvent,
  router?: AlertRouter
): Promise<void> {
  const alertRouter = router ?? getAlertRouter();

  if (event.type === 'tripped') {
    const alert = createAlert(
      'oracle_health_degraded',
      'warning',
      `Oracle circuit tripped: ${event.feedType}/${event.token}`,
      `The oracle circuit breaker tripped for feed "${event.feedType}" ` +
        `token "${event.token}" at ledger ${event.ledger}. ` +
        `Data freshness is degraded until the circuit resets.`,
      {
        feedType: event.feedType,
        token: event.token,
        ledger: event.ledger,
        timestamp: event.timestamp,
      }
    );

    logger.warn('Oracle circuit tripped', {
      feedType: event.feedType,
      token: event.token,
      ledger: event.ledger,
    });

    await alertRouter.route(alert);
  } else {
    const alert = createAlert(
      'oracle_health_degraded',
      'info',
      `Oracle circuit reset: ${event.feedType}/${event.token}`,
      `The oracle circuit breaker reset for feed "${event.feedType}" ` +
        `token "${event.token}" at ledger ${event.ledger}. ` +
        `Data freshness is restored.`,
      {
        feedType: event.feedType,
        token: event.token,
        ledger: event.ledger,
        timestamp: event.timestamp,
      }
    );

    logger.info('Oracle circuit reset', {
      feedType: event.feedType,
      token: event.token,
      ledger: event.ledger,
    });

    await alertRouter.route(alert);
  }
}
