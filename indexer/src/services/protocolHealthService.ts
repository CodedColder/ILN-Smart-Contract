/**
 * LP-queue, funding-velocity, and default-rate operational dashboard (issue #885).
 *
 * Tracks funding queue depth over time, time-to-fund distribution, and
 * rolling default rate for protocol health monitoring. Consumes invoice
 * and event data from the indexer database.
 */

import type Database from 'better-sqlite3';
import { logger } from '../lib/logger.js';

// ── Types ────────────────────────────────────────────────────────────────────

export interface QueueDepthSnapshot {
  timestamp: number;
  pendingCount: number;
  fundedCount: number;
  totalCount: number;
}

export interface FundingVelocityBucket {
  date: string;
  invoicesFunded: number;
  totalAmountFunded: string;
  avgTimeToFundHours: number | null;
}

export interface DefaultRateSnapshot {
  period: string;
  totalInvoices: number;
  defaultedInvoices: number;
  defaultRate: number;
}

export interface TimeToFundDistribution {
  bucket: string;
  count: number;
  percentage: number;
}

export interface ProtocolHealthDashboard {
  queueDepthOverTime: QueueDepthSnapshot[];
  fundingVelocity30d: FundingVelocityBucket[];
  fundingVelocity90d: FundingVelocityBucket[];
  defaultRate30d: DefaultRateSnapshot[];
  defaultRate90d: DefaultRateSnapshot[];
  timeToFundDistribution: TimeToFundDistribution[];
  currentQueueDepth: number;
  healthyRanges: {
    maxQueueDepth: number;
    maxDefaultRate: number;
    minFundingVelocityPerDay: number;
  };
  lastUpdatedAt: number;
}

export interface ProtocolHealthConfig {
  /** Queue depth sampling interval in seconds. Default: 1 hour. */
  queueDepthIntervalSeconds: number;
  /** Maximum queue depth considered healthy. */
  maxQueueDepth: number;
  /** Maximum default rate considered healthy (0-1). */
  maxDefaultRate: number;
  /** Minimum invoices funded per day considered healthy. */
  minFundingVelocityPerDay: number;
}

const DEFAULT_CONFIG: ProtocolHealthConfig = {
  queueDepthIntervalSeconds: 3600,
  maxQueueDepth: 50,
  maxDefaultRate: 0.1,
  minFundingVelocityPerDay: 5,
};

// ── Service ──────────────────────────────────────────────────────────────────

/**
 * Get queue depth snapshots over time by sampling invoice states at intervals.
 */
export function getQueueDepthOverTime(
  db: Database.Database,
  days: number = 30
): QueueDepthSnapshot[] {
  const sinceUnix = Math.floor(Date.now() / 1000) - days * 24 * 60 * 60;
  const interval = 3600; // 1 hour buckets

  const rows = db
    .prepare(
      `
      SELECT
        (created_at / ?) * ? AS bucket,
        SUM(CASE WHEN status = 'Pending' THEN 1 ELSE 0 END) AS pending_count,
        SUM(CASE WHEN status = 'Funded' THEN 1 ELSE 0 END) AS funded_count,
        COUNT(*) AS total_count
      FROM invoices
      WHERE created_at >= ?
      GROUP BY bucket
      ORDER BY bucket ASC
    `
    )
    .all(interval, interval, sinceUnix) as Array<{
    bucket: number;
    pending_count: number;
    funded_count: number;
    total_count: number;
  }>;

  return rows.map((row) => ({
    timestamp: row.bucket,
    pendingCount: row.pending_count,
    fundedCount: row.funded_count,
    totalCount: row.total_count,
  }));
}

/**
 * Get funding velocity: invoices funded per day with amounts.
 */
export function getFundingVelocity(
  db: Database.Database,
  days: 30 | 90
): FundingVelocityBucket[] {
  const sinceUnix = Math.floor(Date.now() / 1000) - days * 24 * 60 * 60;

  const rows = db
    .prepare(
      `
      SELECT
        date(datetime(funded_at, 'unixepoch')) AS date,
        COUNT(*) AS invoices_funded,
        COALESCE(SUM(CAST(amount_funded AS INTEGER)), 0) AS total_amount_funded,
        AVG(
          CASE WHEN funded_at > 0 AND created_at > 0
            THEN (funded_at - created_at) / 3600.0
            ELSE NULL
          END
        ) AS avg_time_to_fund_hours
      FROM invoices
      WHERE funded_at IS NOT NULL
        AND funded_at > 0
        AND funded_at >= ?
      GROUP BY date
      ORDER BY date ASC
    `
    )
    .all(sinceUnix) as Array<{
    date: string;
    invoices_funded: number;
    total_amount_funded: number | string;
    avg_time_to_fund_hours: number | null;
  }>;

  return rows.map((row) => ({
    date: row.date,
    invoicesFunded: row.invoices_funded,
    totalAmountFunded: BigInt(row.total_amount_funded).toString(),
    avgTimeToFundHours: row.avg_time_to_fund_hours !== null
      ? round6(row.avg_time_to_fund_hours)
      : null,
  }));
}

/**
 * Get default rate by period (week).
 */
export function getDefaultRate(
  db: Database.Database,
  days: 30 | 90
): DefaultRateSnapshot[] {
  const sinceUnix = Math.floor(Date.now() / 1000) - days * 24 * 60 * 60;

  const rows = db
    .prepare(
      `
      SELECT
        strftime('%Y-%W', datetime(created_at, 'unixepoch')) AS period,
        COUNT(*) AS total_invoices,
        SUM(CASE WHEN status = 'Expired' OR status = 'Cancelled' THEN 1 ELSE 0 END) AS defaulted_invoices
      FROM invoices
      WHERE created_at >= ?
      GROUP BY period
      ORDER BY period ASC
    `
    )
    .all(sinceUnix) as Array<{
    period: string;
    total_invoices: number;
    defaulted_invoices: number;
  }>;

  return rows.map((row) => ({
    period: row.period,
    totalInvoices: row.total_invoices,
    defaultedInvoices: row.defaulted_invoices,
    defaultRate: row.total_invoices > 0
      ? round6(row.defaulted_invoices / row.total_invoices)
      : 0,
  }));
}

/**
 * Get time-to-fund distribution: how quickly invoices get funded.
 */
export function getTimeToFundDistribution(
  db: Database.Database
): TimeToFundDistribution[] {
  const rows = db
    .prepare(
      `
      SELECT
        CASE
          WHEN (funded_at - created_at) < 3600 THEN '< 1 hour'
          WHEN (funded_at - created_at) < 14400 THEN '1-4 hours'
          WHEN (funded_at - created_at) < 86400 THEN '4-24 hours'
          WHEN (funded_at - created_at) < 259200 THEN '1-3 days'
          WHEN (funded_at - created_at) < 604800 THEN '3-7 days'
          ELSE '> 7 days'
        END AS bucket,
        COUNT(*) AS count
      FROM invoices
      WHERE funded_at IS NOT NULL
        AND funded_at > 0
        AND created_at > 0
        AND funded_at > created_at
      GROUP BY bucket
      ORDER BY
        CASE bucket
          WHEN '< 1 hour' THEN 1
          WHEN '1-4 hours' THEN 2
          WHEN '4-24 hours' THEN 3
          WHEN '1-3 days' THEN 4
          WHEN '3-7 days' THEN 5
          ELSE 6
        END
    `
    )
    .all() as Array<{ bucket: string; count: number }>;

  const total = rows.reduce((sum, row) => sum + row.count, 0);

  return rows.map((row) => ({
    bucket: row.bucket,
    count: row.count,
    percentage: total > 0 ? round6(row.count / total) : 0,
  }));
}

/**
 * Get the current queue depth (pending invoices).
 */
export function getCurrentQueueDepth(db: Database.Database): number {
  const row = db
    .prepare(
      `
      SELECT COUNT(*) AS count
      FROM invoices
      WHERE status = 'Pending'
    `
    )
    .get() as { count: number };

  return row.count;
}

/**
 * Build the full protocol health dashboard.
 */
export function getProtocolHealthDashboard(
  db: Database.Database,
  config: ProtocolHealthConfig = DEFAULT_CONFIG
): ProtocolHealthDashboard {
  const dashboard: ProtocolHealthDashboard = {
    queueDepthOverTime: getQueueDepthOverTime(db),
    fundingVelocity30d: getFundingVelocity(db, 30),
    fundingVelocity90d: getFundingVelocity(db, 90),
    defaultRate30d: getDefaultRate(db, 30),
    defaultRate90d: getDefaultRate(db, 90),
    timeToFundDistribution: getTimeToFundDistribution(db),
    currentQueueDepth: getCurrentQueueDepth(db),
    healthyRanges: {
      maxQueueDepth: config.maxQueueDepth,
      maxDefaultRate: config.maxDefaultRate,
      minFundingVelocityPerDay: config.minFundingVelocityPerDay,
    },
    lastUpdatedAt: Date.now(),
  };

  logger.info('Protocol health dashboard computed', {
    currentQueueDepth: dashboard.currentQueueDepth,
    velocityBuckets30d: dashboard.fundingVelocity30d.length,
    defaultRatePeriods30d: dashboard.defaultRate30d.length,
  });

  return dashboard;
}

function round6(value: number): number {
  return Math.round(value * 1_000_000) / 1_000_000;
}
