/**
 * Curated, non-sensitive protocol-health summary for the public status page
 * (Issue #892).
 *
 * This is the **only** place that decides what becomes public. It returns a
 * fixed, allow-listed shape — it never spreads a database row or a chain
 * snapshot — so a new column or a new field on `OnChainProtocolStatus` cannot
 * leak by accident. Deliberately excluded: admin/signer addresses, multisig
 * configuration, admin-action logs, per-address data, raw amounts.
 *
 * All ratios are unit-free percentages so the public page never has to guess
 * token decimals or sum amounts across tokens.
 */

import type Database from 'better-sqlite3';
import type { ProtocolStatusSnapshot } from './protocolStatusService.js';

export type OverallStatus = 'operational' | 'degraded' | 'paused' | 'unknown';
export type SolvencyLevel = 'healthy' | 'watch' | 'stressed' | 'unknown';

export interface PublicHealthThresholds {
  /** Default rate (% of resolved funded invoices) at which level becomes `watch` / `stressed`. */
  defaultRateWatchPct: number;
  defaultRateStressedPct: number;
  /** Overdue share (% of outstanding funded invoices past due) for `watch` / `stressed`. */
  overdueShareWatchPct: number;
  overdueShareStressedPct: number;
  /** Insurance claims share (% of pool ever held that was paid out) for `watch` / `stressed`. */
  insuranceClaimsWatchPct: number;
  insuranceClaimsStressedPct: number;
  /** Minimum resolved invoices before a default rate is reported. */
  minResolvedForDefaultRate: number;
  /** Minimum outstanding funded invoices before an overdue share is reported. */
  minOutstandingForOverdueShare: number;
}

export const DEFAULT_THRESHOLDS: PublicHealthThresholds = {
  defaultRateWatchPct: 3,
  defaultRateStressedPct: 8,
  overdueShareWatchPct: 10,
  overdueShareStressedPct: 25,
  insuranceClaimsWatchPct: 25,
  insuranceClaimsStressedPct: 60,
  minResolvedForDefaultRate: 10,
  minOutstandingForOverdueShare: 5,
};

export interface PublicHealth {
  schemaVersion: 1;
  /** ISO-8601 time this summary was computed. */
  generatedAt: string;
  overall: OverallStatus;
  /** Short, human-readable explanations for any non-`operational` status. */
  reasons: string[];
  protocol: {
    /** `null` when the on-chain status could not be read. */
    paused: boolean | null;
    /** True when the chain read failed and the value comes from an older read. */
    stale: boolean;
  };
  solvency: {
    level: SolvencyLevel;
    fundedInvoices: number;
    settledInvoices: number;
    defaultedInvoices: number;
    /** `null` until enough invoices have resolved (see thresholds). */
    defaultRatePct: number | null;
    outstandingFundedInvoices: number;
    overdueFundedInvoices: number;
    /** `null` until enough invoices are outstanding. */
    overdueSharePct: number | null;
    insurancePool: {
      enrolledLps: number;
      /** % of everything the pool has held that has been paid out in claims. */
      claimsSharePct: number | null;
    } | null;
  };
  oracle: {
    /** Number of oracle circuit breakers currently tripped; `null` if unknown. */
    circuitsTripped: number | null;
    healthy: boolean | null;
  };
}

export interface PublicHealthOptions {
  protocolStatus: ProtocolStatusSnapshot;
  /** Restrict the insurance figures to one pool; defaults to the most recently updated pool. */
  insurancePoolContractId?: string | undefined;
  thresholds?: Partial<PublicHealthThresholds> | undefined;
  /** Injectable clock (ms) for tests. */
  now?: () => number;
}

const LEVEL_RANK: Record<SolvencyLevel, number> = { unknown: 0, healthy: 1, watch: 2, stressed: 3 };

function pct(numerator: number, denominator: number): number {
  return Math.round((numerator / denominator) * 1000) / 10;
}

function levelFor(value: number, watch: number, stressed: number): SolvencyLevel {
  if (value >= stressed) return 'stressed';
  if (value >= watch) return 'watch';
  return 'healthy';
}

function worst(levels: SolvencyLevel[]): SolvencyLevel {
  const measured = levels.filter((l) => l !== 'unknown');
  if (measured.length === 0) return 'unknown';
  return measured.reduce((a, b) => (LEVEL_RANK[b] > LEVEL_RANK[a] ? b : a));
}

/** `claims / (balance + claims)` in percent, using BigInt so large stroop values stay exact. */
function claimsSharePct(balance: string, claimsPaid: string): number | null {
  try {
    const b = BigInt(balance);
    const c = BigInt(claimsPaid);
    const total = b + c;
    if (total <= 0n) return null;
    return Number((c * 1000n) / total) / 10;
  } catch {
    return null;
  }
}

export function buildPublicHealth(
  db: Database.Database,
  options: PublicHealthOptions,
): PublicHealth {
  const thresholds = { ...DEFAULT_THRESHOLDS, ...options.thresholds };
  const nowMs = (options.now ?? Date.now)();
  const nowSec = Math.floor(nowMs / 1000);

  const counts = db
    .prepare(
      `SELECT
         SUM(CASE WHEN status IN ('Funded', 'Paid', 'Defaulted') THEN 1 ELSE 0 END) AS funded,
         SUM(CASE WHEN status = 'Paid' THEN 1 ELSE 0 END) AS settled,
         SUM(CASE WHEN status = 'Defaulted' THEN 1 ELSE 0 END) AS defaulted,
         SUM(CASE WHEN status = 'Funded' THEN 1 ELSE 0 END) AS outstanding,
         SUM(CASE WHEN status = 'Funded' AND due_date < ? THEN 1 ELSE 0 END) AS overdue
       FROM invoices`,
    )
    .get(nowSec) as {
    funded: number | null;
    settled: number | null;
    defaulted: number | null;
    outstanding: number | null;
    overdue: number | null;
  };

  const funded = counts.funded ?? 0;
  const settled = counts.settled ?? 0;
  const defaulted = counts.defaulted ?? 0;
  const outstanding = counts.outstanding ?? 0;
  const overdue = counts.overdue ?? 0;

  const resolved = settled + defaulted;
  const defaultRatePct =
    resolved >= thresholds.minResolvedForDefaultRate ? pct(defaulted, resolved) : null;
  const overdueSharePct =
    outstanding >= thresholds.minOutstandingForOverdueShare ? pct(overdue, outstanding) : null;

  const poolRow = (
    options.insurancePoolContractId
      ? db
          .prepare(
            `SELECT pool_balance, total_claims_paid, enrolled_lp_count
             FROM insurance_pool_stats WHERE contract_id = ?`,
          )
          .get(options.insurancePoolContractId)
      : db
          .prepare(
            `SELECT pool_balance, total_claims_paid, enrolled_lp_count
             FROM insurance_pool_stats ORDER BY last_updated_at DESC LIMIT 1`,
          )
          .get()
  ) as { pool_balance: string; total_claims_paid: string; enrolled_lp_count: number } | undefined;

  const insurancePool = poolRow
    ? {
        enrolledLps: poolRow.enrolled_lp_count,
        claimsSharePct: claimsSharePct(poolRow.pool_balance, poolRow.total_claims_paid),
      }
    : null;

  const solvencyLevel = worst([
    defaultRatePct === null
      ? 'unknown'
      : levelFor(defaultRatePct, thresholds.defaultRateWatchPct, thresholds.defaultRateStressedPct),
    overdueSharePct === null
      ? 'unknown'
      : levelFor(overdueSharePct, thresholds.overdueShareWatchPct, thresholds.overdueShareStressedPct),
    insurancePool?.claimsSharePct == null
      ? 'unknown'
      : levelFor(
          insurancePool.claimsSharePct,
          thresholds.insuranceClaimsWatchPct,
          thresholds.insuranceClaimsStressedPct,
        ),
  ]);

  const chain = options.protocolStatus.status;
  const paused = chain ? chain.paused : null;
  const circuitsTripped = chain ? chain.oracleCircuitsTripped : null;
  const oracleHealthy = chain ? !chain.oracleCircuitTripped && chain.oracleCircuitsTripped === 0 : null;

  const reasons: string[] = [];
  let overall: OverallStatus;
  if (paused === true) {
    overall = 'paused';
    reasons.push('The protocol is paused; new invoices cannot be submitted or funded.');
  } else if (paused === null) {
    overall = 'unknown';
    reasons.push('On-chain status could not be read.');
  } else if (oracleHealthy === false || solvencyLevel === 'stressed') {
    overall = 'degraded';
    if (oracleHealthy === false) reasons.push('An oracle circuit breaker is tripped.');
    if (solvencyLevel === 'stressed') reasons.push('Credit-health indicators are in the stressed range.');
  } else {
    overall = 'operational';
  }
  if (options.protocolStatus.stale && paused !== null) {
    reasons.push('On-chain status is from an earlier read; the latest read failed.');
  }

  return {
    schemaVersion: 1,
    generatedAt: new Date(nowMs).toISOString(),
    overall,
    reasons,
    protocol: { paused, stale: options.protocolStatus.stale },
    solvency: {
      level: solvencyLevel,
      fundedInvoices: funded,
      settledInvoices: settled,
      defaultedInvoices: defaulted,
      defaultRatePct,
      outstandingFundedInvoices: outstanding,
      overdueFundedInvoices: overdue,
      overdueSharePct,
      insurancePool,
    },
    oracle: { circuitsTripped, healthy: oracleHealthy },
  };
}
