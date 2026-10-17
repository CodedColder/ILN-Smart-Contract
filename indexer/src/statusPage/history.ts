/**
 * Rolling sample history for the public status page (Issue #892).
 *
 * The page is a static file regenerated on a schedule, so "uptime" cannot be
 * computed from a live database. Each run appends one small sample to
 * `history.json`, which is published next to the page and fetched back by the
 * next run. Only booleans and an overall label are stored — nothing sensitive.
 */

import type { OverallStatus, PublicHealth } from '../services/publicHealthService.js';

export interface HistorySample {
  /** ISO-8601 time of the sample. */
  t: string;
  overall: OverallStatus;
  /** `null` when the on-chain status could not be read for this sample. */
  paused: boolean | null;
  oracleHealthy: boolean | null;
}

export interface Uptime {
  windowDays: number;
  /** Samples in the window with a known oracle state. */
  oracleSamples: number;
  /** % of known samples with no oracle circuit tripped; `null` when there are none. */
  oraclePct: number | null;
  /** Samples in the window with a known paused state. */
  protocolSamples: number;
  /** % of known samples where the protocol was not paused; `null` when there are none. */
  protocolPct: number | null;
}

export const HISTORY_RETENTION_DAYS = 35;
const DAY_MS = 24 * 60 * 60 * 1000;
const OVERALL: readonly OverallStatus[] = ['operational', 'degraded', 'paused', 'unknown'];

/** Tolerant parser: anything malformed is dropped rather than failing the publish. */
export function parseHistory(raw: unknown): HistorySample[] {
  if (!Array.isArray(raw)) return [];
  const out: HistorySample[] = [];
  for (const item of raw) {
    if (!item || typeof item !== 'object') continue;
    const s = item as Record<string, unknown>;
    if (typeof s.t !== 'string' || Number.isNaN(Date.parse(s.t))) continue;
    if (!OVERALL.includes(s.overall as OverallStatus)) continue;
    const bool = (v: unknown): boolean | null => (typeof v === 'boolean' ? v : null);
    out.push({
      t: s.t,
      overall: s.overall as OverallStatus,
      paused: bool(s.paused),
      oracleHealthy: bool(s.oracleHealthy),
    });
  }
  return out;
}

export function appendSample(
  history: HistorySample[],
  health: PublicHealth,
  nowMs: number,
): HistorySample[] {
  const cutoff = nowMs - HISTORY_RETENTION_DAYS * DAY_MS;
  const next = history
    .filter((s) => Date.parse(s.t) >= cutoff && s.t !== health.generatedAt)
    .concat({
      t: health.generatedAt,
      overall: health.overall,
      paused: health.protocol.paused,
      oracleHealthy: health.oracle.healthy,
    });
  return next.sort((a, b) => Date.parse(a.t) - Date.parse(b.t));
}

function ratio(good: number, known: number): number | null {
  return known === 0 ? null : Math.round((good / known) * 10000) / 100;
}

export function computeUptime(
  history: HistorySample[],
  windowDays: number,
  nowMs: number,
): Uptime {
  const since = nowMs - windowDays * DAY_MS;
  const inWindow = history.filter((s) => Date.parse(s.t) >= since);

  const oracleKnown = inWindow.filter((s) => s.oracleHealthy !== null);
  const protocolKnown = inWindow.filter((s) => s.paused !== null);

  return {
    windowDays,
    oracleSamples: oracleKnown.length,
    oraclePct: ratio(oracleKnown.filter((s) => s.oracleHealthy === true).length, oracleKnown.length),
    protocolSamples: protocolKnown.length,
    protocolPct: ratio(protocolKnown.filter((s) => s.paused === false).length, protocolKnown.length),
  };
}
