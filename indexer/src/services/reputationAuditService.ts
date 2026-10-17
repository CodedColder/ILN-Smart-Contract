/**
 * Reputation audit-trail dashboard (issue #884).
 *
 * Consumes the ReputationUpdated event stream to build a per-address
 * reputation-change history with anomaly highlighting for unusually large
 * single-event score jumps.
 */

import type Database from 'better-sqlite3';
import { logger } from '../lib/logger.js';

// ── Types ────────────────────────────────────────────────────────────────────

export interface ReputationHistoryEntry {
  address: string;
  oldScore: number;
  newScore: number;
  scoreDelta: number;
  invoicesSubmitted: number;
  invoicesPaid: number;
  invoicesDefaulted: number;
  ledger: number;
  timestamp: number;
  eventType: string;
}

export interface ReputationAnomaly {
  address: string;
  entry: ReputationHistoryEntry;
  meanDelta: number;
  stdDevDelta: number;
  upperBound: number;
  isAnomaly: boolean;
  direction: 'spike' | 'drop' | 'normal';
}

export interface ReputationAuditSummary {
  totalUpdates: number;
  uniqueAddresses: number;
  anomalies: ReputationAnomaly[];
  recentUpdates: ReputationHistoryEntry[];
  lastUpdatedAt: number;
}

export interface ReputationAuditConfig {
  /** Number of standard deviations above mean to flag as anomaly. */
  anomalyThresholdStdDevs: number;
  /** Minimum history entries before anomaly detection activates. */
  minimumSampleSize: number;
  /** Maximum recent updates to return. */
  recentLimit: number;
}

const DEFAULT_CONFIG: ReputationAuditConfig = {
  anomalyThresholdStdDevs: 2.0,
  minimumSampleSize: 5,
  recentLimit: 50,
};

// ── Service ──────────────────────────────────────────────────────────────────

/**
 * Fetch reputation change history for a specific address.
 */
export function getReputationHistory(
  db: Database.Database,
  address: string,
  limit: number = 100
): ReputationHistoryEntry[] {
  const rows = db
    .prepare(
      `
      SELECT
        address, old_score, new_score,
        (new_score - old_score) AS score_delta,
        invoices_submitted, invoices_paid, invoices_defaulted,
        ledger, timestamp, event_type
      FROM reputation_updates
      WHERE address = ?
      ORDER BY timestamp DESC
      LIMIT ?
    `
    )
    .all(address, limit) as Array<{
    address: string;
    old_score: number;
    new_score: number;
    score_delta: number;
    invoices_submitted: number;
    invoices_paid: number;
    invoices_defaulted: number;
    ledger: number;
    timestamp: number;
    event_type: string;
  }>;

  return rows.map((row) => ({
    address: row.address,
    oldScore: row.old_score,
    newScore: row.new_score,
    scoreDelta: row.score_delta,
    invoicesSubmitted: row.invoices_submitted,
    invoicesPaid: row.invoices_paid,
    invoicesDefaulted: row.invoices_defaulted,
    ledger: row.ledger,
    timestamp: row.timestamp,
    eventType: row.event_type,
  }));
}

/**
 * Detect anomalous reputation score jumps for a given address.
 *
 * Uses a simple z-score approach: if the latest score delta is more than
 * `anomalyThresholdStdDevs` standard deviations from the mean of historical
 * deltas, it is flagged.
 */
export function detectReputationAnomalies(
  history: ReputationHistoryEntry[],
  config: ReputationAuditConfig = DEFAULT_CONFIG
): ReputationAnomaly[] {
  if (history.length < 2) return [];

  // Sort by timestamp ascending for analysis
  const sorted = [...history].sort((a, b) => a.timestamp - b.timestamp);
  const deltas = sorted.map((e) => e.scoreDelta);

  const anomalies: ReputationAnomaly[] = [];

  for (let i = config.minimumSampleSize; i < sorted.length; i++) {
    const historicalDeltas = deltas.slice(0, i);
    const mean = historicalDeltas.reduce((s, v) => s + v, 0) / historicalDeltas.length;
    const variance =
      historicalDeltas.reduce((s, v) => s + (v - mean) ** 2, 0) / historicalDeltas.length;
    const stdDev = Math.sqrt(variance);

    const currentDelta = deltas[i]!;
    const upperBound = mean + config.anomalyThresholdStdDevs * stdDev;
    const lowerBound = mean - config.anomalyThresholdStdDevs * stdDev;

    let direction: 'spike' | 'drop' | 'normal' = 'normal';
    let isAnomaly = false;

    if (currentDelta > upperBound && stdDev > 0) {
      isAnomaly = true;
      direction = 'spike';
    } else if (currentDelta < lowerBound && stdDev > 0) {
      isAnomaly = true;
      direction = 'drop';
    }

    if (isAnomaly) {
      anomalies.push({
        address: sorted[i]!.address,
        entry: sorted[i]!,
        meanDelta: round6(mean),
        stdDevDelta: round6(stdDev),
        upperBound: round6(upperBound),
        isAnomaly,
        direction,
      });
    }
  }

  return anomalies;
}

/**
 * Get a full audit-trail summary: total updates, unique addresses,
 * detected anomalies, and recent updates across all addresses.
 */
export function getReputationAuditSummary(
  db: Database.Database,
  config: ReputationAuditConfig = DEFAULT_CONFIG
): ReputationAuditSummary {
  const totals = db
    .prepare(
      `
      SELECT
        COUNT(*) AS total_updates,
        COUNT(DISTINCT address) AS unique_addresses
      FROM reputation_updates
    `
    )
    .get() as { total_updates: number; unique_addresses: number };

  const recentRows = db
    .prepare(
      `
      SELECT
        address, old_score, new_score,
        (new_score - old_score) AS score_delta,
        invoices_submitted, invoices_paid, invoices_defaulted,
        ledger, timestamp, event_type
      FROM reputation_updates
      ORDER BY timestamp DESC
      LIMIT ?
    `
    )
    .all(config.recentLimit) as Array<{
    address: string;
    old_score: number;
    new_score: number;
    score_delta: number;
    invoices_submitted: number;
    invoices_paid: number;
    invoices_defaulted: number;
    ledger: number;
    timestamp: number;
    event_type: string;
  }>;

  const recentUpdates: ReputationHistoryEntry[] = recentRows.map((row) => ({
    address: row.address,
    oldScore: row.old_score,
    newScore: row.new_score,
    scoreDelta: row.score_delta,
    invoicesSubmitted: row.invoices_submitted,
    invoicesPaid: row.invoices_paid,
    invoicesDefaulted: row.invoices_defaulted,
    ledger: row.ledger,
    timestamp: row.timestamp,
    eventType: row.event_type,
  }));

  // Detect anomalies across all addresses
  const allAnomalies: ReputationAnomaly[] = [];
  const addressRows = db
    .prepare(
      `
      SELECT DISTINCT address FROM reputation_updates
      `
    )
    .all() as Array<{ address: string }>;

  for (const { address } of addressRows) {
    const history = getReputationHistory(db, address, 200);
    const anomalies = detectReputationAnomalies(history, config);
    allAnomalies.push(...anomalies);
  }

  // Sort anomalies by timestamp descending, most recent first
  allAnomalies.sort((a, b) => b.entry.timestamp - a.entry.timestamp);

  logger.info('Reputation audit summary computed', {
    totalUpdates: totals.total_updates,
    uniqueAddresses: totals.unique_addresses,
    anomalyCount: allAnomalies.length,
  });

  return {
    totalUpdates: totals.total_updates,
    uniqueAddresses: totals.unique_addresses,
    anomalies: allAnomalies.slice(0, config.recentLimit),
    recentUpdates,
    lastUpdatedAt: Date.now(),
  };
}

function round6(value: number): number {
  return Math.round(value * 1_000_000) / 1_000_000;
}
