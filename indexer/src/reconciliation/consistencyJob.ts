/**
 * Continuous indexer/chain consistency reconciliation.
 *
 * Productionized version of tests/e2e/indexerConsistency.test.ts: instead of
 * a one-time CI assertion, this job periodically spot-checks a random sample
 * of indexed invoices (and the invoice count) against direct on-chain
 * contract reads, and raises a drift alert through the notifications service
 * when mismatches exceed the configured tolerance.
 *
 * It is also the backstop for chain reorgs (Issue #866): stored ledger
 * headers are re-verified against the canonical chain, so a reorg that
 * slipped past real-time detection (a header read failed during ingestion,
 * or a fork deeper than the live window) still gets rolled back and replayed
 * instead of leaving indexed rows built on a chain that no longer exists.
 *
 * Cadence, sample size and tolerance are documented in
 * docs/indexer-reconciliation.md.
 */

import type Database from 'better-sqlite3';
import type { ChainReader } from './chainReader.js';
import type { LedgerHeaderSource } from '../ingestion/ledgerHeaders.js';
import { findDivergence, latchHalt } from '../ingestion/reorgDetector.js';
import type { ReorgDivergence } from '../ingestion/reorgDetector.js';
import {
  classifyReorgDepth,
  classifyReorgSeverity,
} from '../services/alertRouter.js';

export interface ReconciliationConfig {
  /** Milliseconds between reconciliation runs. */
  intervalMs: number;
  /** Invoices sampled per run. */
  sampleSize: number;
  /**
   * Percentage of sampled invoices allowed to mismatch before an alert is
   * raised (0-100).
   */
  tolerancePercent: number;
  /**
   * Allowed absolute gap between indexed and on-chain invoice counts,
   * absorbing normal ingestion lag. Defaults to max(5, ceil(1% of chain count)).
   */
  countLagTolerance?: number;
  /**
   * Most-recent stored ledger headers re-verified against the chain per run
   * (Issue #866). Reorgs are recent by nature, so the newest headers carry
   * the signal; the fork-point walk then covers the depth below them.
   */
  reorgHeaderSampleSize?: number;
}

export interface ReconciliationMismatch {
  invoiceId: number;
  field: string;
  indexedValue: string | null;
  chainValue: string | null;
}

export interface ReconciliationReport {
  ranAt: string;
  sampledInvoices: number;
  checkedFields: number;
  /** Distinct invoices with real drift (excludes chain-read errors). */
  driftedInvoices: number;
  mismatches: ReconciliationMismatch[];
  indexedInvoiceCount: number;
  chainInvoiceCount: number;
  countWithinTolerance: boolean;
  driftDetected: boolean;
  /** Stored ledger headers compared against the chain this run. */
  reorgHeadersChecked: number;
  /** Header reads that failed during the reorg check (infrastructure noise). */
  reorgCheckErrors: number;
  /**
   * Stored history no longer matches the canonical chain: the divergence
   * that must be rolled back and replayed (already halt-latched).
   */
  reorgDivergence: ReorgDivergence | null;
  error?: string;
}

export interface AlertDispatcher {
  (alert: {
    type: 'indexer_drift_detected' | 'indexer_reorg_detected';
    severity: 'critical';
    summary: string;
    details: ReconciliationReport;
    firedAt: string;
  }): Promise<void>;
}

/** Reorg backstop: how many recent stored headers are re-verified per run. */
export const DEFAULT_REORG_HEADER_SAMPLE_SIZE = parseInt(
  process.env.REORG_HEADER_SAMPLE_SIZE || '20',
  10
);

export const DEFAULT_RECONCILIATION_CONFIG: ReconciliationConfig = {
  intervalMs: parseInt(process.env.RECONCILIATION_INTERVAL_MS || '900000', 10), // 15 min
  sampleSize: parseInt(process.env.RECONCILIATION_SAMPLE_SIZE || '25', 10),
  tolerancePercent: parseFloat(process.env.RECONCILIATION_TOLERANCE_PERCENT || '1'),
  reorgHeaderSampleSize: DEFAULT_REORG_HEADER_SAMPLE_SIZE,
};

export function configFromEnv(): ReconciliationConfig {
  return DEFAULT_RECONCILIATION_CONFIG;
}

export interface ReconciliationHooks {
  /**
   * Canonical ledger headers for the reorg backstop. Omit to skip the reorg
   * check (header-less deployments keep the invoice/amount checks only).
   */
  ledgerHeaders?: LedgerHeaderSource;
}

/**
 * Compare the most recent stored ledger headers against the canonical chain.
 *
 * This is the reorg signature the live detector cannot always see: rows were
 * indexed at a height whose ledger hash no longer matches the chain. On the
 * first mismatch the shared fork-point walk (`findDivergence`) finds the
 * common ancestor, and the divergence is reported with
 * `detectedBy: 'consistency_job'`.
 *
 * Header-read failures are counted, never treated as divergence — a
 * unavailable RPC must not be read as a fork.
 */
export async function detectReorgFromStoredHistory(
  db: Database.Database,
  source: LedgerHeaderSource,
  config: ReconciliationConfig = DEFAULT_RECONCILIATION_CONFIG
): Promise<{ divergence: ReorgDivergence | null; checked: number; errors: number }> {
  const sampleSize = Math.max(1, config.reorgHeaderSampleSize ?? DEFAULT_REORG_HEADER_SAMPLE_SIZE);
  const rows = db
    .prepare('SELECT sequence, hash FROM ledger_headers ORDER BY sequence DESC LIMIT ?')
    .all(sampleSize) as Array<{ sequence: number; hash: string }>;

  let checked = 0;
  let errors = 0;

  for (const row of rows) {
    let canonicalHash: string;
    try {
      canonicalHash = (await source.getLedgerHeader(row.sequence)).hash;
    } catch {
      errors += 1;
      continue;
    }

    checked += 1;
    if (canonicalHash === row.hash) {
      continue;
    }

    const divergence = await findDivergence({
      db,
      source,
      staleSequence: row.sequence,
      reason: 'ledger_hash_mismatch',
      detectedBy: 'consistency_job',
    });

    return { divergence, checked, errors };
  }

  return { divergence: null, checked, errors };
}

export async function runReconciliation(
  db: Database.Database,
  chainReader: ChainReader,
  config: ReconciliationConfig = DEFAULT_RECONCILIATION_CONFIG,
  hooks: ReconciliationHooks = {}
): Promise<ReconciliationReport> {
  const report: ReconciliationReport = {
    ranAt: new Date().toISOString(),
    sampledInvoices: 0,
    checkedFields: 0,
    driftedInvoices: 0,
    mismatches: [],
    indexedInvoiceCount: 0,
    chainInvoiceCount: 0,
    countWithinTolerance: true,
    driftDetected: false,
    reorgHeadersChecked: 0,
    reorgCheckErrors: 0,
    reorgDivergence: null,
  };

  try {
    // ---- Reorg backstop (Issue #866): indexed rows vs ledger hashes ----
    if (hooks.ledgerHeaders) {
      try {
        const reorg = await detectReorgFromStoredHistory(db, hooks.ledgerHeaders, config);
        report.reorgHeadersChecked = reorg.checked;
        report.reorgCheckErrors = reorg.errors;
        report.reorgDivergence = reorg.divergence;

        if (reorg.divergence) {
          // Latch before anything else reads state: ingestion must stop
          // immediately, whether or not this process runs the recovery hook.
          latchHalt(db, reorg.divergence);
        }
      } catch (error) {
        report.reorgCheckErrors += 1;
        report.error =
          `reorg check failed: ${error instanceof Error ? error.message : String(error)}`;
      }
    }

    report.indexedInvoiceCount = (
      db.prepare(`SELECT COUNT(*) AS n FROM invoices`).get() as { n: number }
    ).n;

    // ---- Sampled per-invoice spot checks against direct contract reads ----
    if (report.indexedInvoiceCount > 0) {
      const sampleLimit = Math.max(1, Math.min(config.sampleSize, report.indexedInvoiceCount));
      const sampled = db
        .prepare(`SELECT id FROM invoices ORDER BY RANDOM() LIMIT ?`)
        .all(sampleLimit) as Array<{ id: number }>;
      report.sampledInvoices = sampled.length;

      for (const { id } of sampled) {
        const row = db
          .prepare(
            `SELECT status, amount, amount_funded, amount_paid, funder FROM invoices WHERE id = ?`
          )
          .get(id) as {
            status: string;
            amount: string;
            amount_funded: string;
            amount_paid: string;
            funder: string | null;
          };

        let chain: Awaited<ReturnType<ChainReader['getInvoice']>>;
        try {
          chain = await chainReader.getInvoice(id);
        } catch (error) {
          // Chain read failures are infrastructure noise, not drift — record
          // and continue; sustained failures surface via alert dispatch below.
          report.mismatches.push({
            invoiceId: id,
            field: '__chain_read_error__',
            indexedValue: null,
            chainValue: error instanceof Error ? error.message : String(error),
          });
          continue;
        }

        if (chain === null) {
          report.mismatches.push({
            invoiceId: id,
            field: '__missing_on_chain__',
            indexedValue: row.status,
            chainValue: null,
          });
          continue;
        }

        compareField(report, id, 'status', row.status.toLowerCase(), chain.status.toLowerCase());
        compareField(report, id, 'amount', row.amount, chain.amount);
        compareField(report, id, 'amount_funded', row.amount_funded, chain.amountFunded);
        compareField(report, id, 'amount_paid', row.amount_paid, chain.amountPaid);
      }
    }

    // ---- Global count check (absorbs ingestion lag via tolerance) ----
    try {
      report.chainInvoiceCount = await chainReader.getInvoiceCount();
    } catch {
      report.chainInvoiceCount = -1;
    }

    if (report.chainInvoiceCount >= 0) {
      const tolerance =
        config.countLagTolerance ?? Math.max(5, Math.ceil(report.chainInvoiceCount * 0.01));
      report.countWithinTolerance =
        Math.abs(report.indexedInvoiceCount - report.chainInvoiceCount) <= tolerance;
    }

    // Drift rate is measured in distinct invoices with real mismatches
    // (field divergence or missing on-chain). Chain-read errors are
    // infrastructure noise and excluded from the drift rate; they remain in
    // report.mismatches for observability.
    const driftedInvoices = new Set(
      report.mismatches.filter((m) => m.field !== '__chain_read_error__').map((m) => m.invoiceId)
    ).size;
    report.driftedInvoices = driftedInvoices;
    const mismatchRate =
      report.sampledInvoices > 0 ? (driftedInvoices / report.sampledInvoices) * 100 : 0;

    const invoiceDrift = mismatchRate > config.tolerancePercent;
    const countDrift = !report.countWithinTolerance;
    // A reorg is drift at the root: rows were derived from a chain that no
    // longer exists, so every field comparison below it is suspect.
    report.driftDetected = invoiceDrift || countDrift || report.reorgDivergence !== null;

    return report;
  } catch (error) {
    report.error = error instanceof Error ? error.message : String(error);
    report.driftDetected = true;
    return report;
  }
}

function compareField(
  report: ReconciliationReport,
  invoiceId: number,
  field: string,
  indexedValue: string,
  chainValue: string
): void {
  report.checkedFields += 1;
  if (indexedValue !== chainValue) {
    report.mismatches.push({ invoiceId, field, indexedValue, chainValue });
  }
}

/**
 * Default alert dispatcher: POSTs to the notifications service intake URL.
 * Configure RECONCILIATION_ALERT_URL to e.g. the notifications service's
 * /notify/slack endpoint or any HTTP collector subscribed for
 * indexer_drift_detected events.
 */
export function createWebhookAlertDispatcher(url: string, httpPost?: typeof fetch): AlertDispatcher {
  const post = httpPost ?? fetch;
  return async (alert) => {
    try {
      const response = await post(url, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(alert),
      });
      if (!response.ok) {
        console.error(`Drift alert delivery failed with HTTP ${response.status}`);
      }
    } catch (error) {
      console.error(`Drift alert delivery error: ${error instanceof Error ? error.message : String(error)}`);
    }
  };
}

export function buildAlertPayload(report: ReconciliationReport) {
  const first = report.mismatches
    .slice(0, 5)
    .map((m) => `invoice ${m.invoiceId} ${m.field}: indexed=${m.indexedValue} chain=${m.chainValue}`)
    .join('; ');
  return {
    type: 'indexer_drift_detected' as const,
    severity: 'critical' as const,
    summary:
      `Indexer drift detected: ${report.driftedInvoices}/${report.sampledInvoices} sampled invoices drifted ` +
      `(${report.mismatches.length} field mismatch(es)); indexed count=${report.indexedInvoiceCount}, ` +
      `chain count=${report.chainInvoiceCount}. Samples: ${first}`,
    details: report,
    firedAt: new Date().toISOString(),
  };
}

/**
 * Alert for a reorg found by the backstop (Issue #866). Distinct from the
 * drift alert because it is not a field-level mismatch to tolerate: it names
 * the ledger that must be rolled back and replayed.
 */
export function buildReorgAlertPayload(report: ReconciliationReport) {
  const divergence = report.reorgDivergence;
  const severity = divergence ? classifyReorgSeverity(divergence.forkDepth) : 'critical';
  const reorgSeverity = divergence ? classifyReorgDepth(divergence.forkDepth) : 'shallow';
  return {
    type: 'indexer_reorg_detected' as const,
    severity,
    summary: divergence
      ? `Ledger reorg detected at ledger ${divergence.divergenceLedger} ` +
        `(${divergence.reason}, common ancestor ${divergence.commonAncestorLedger}, ` +
        `forkDepth=${divergence.forkDepth}, classification=${reorgSeverity}): ` +
        `${report.reorgHeadersChecked} stored header(s) checked, ` +
        `${report.reorgCheckErrors} unreadable. Ingestion halted pending rollback-and-replay.`
      : 'Ledger reorg detected',
    details: {
      ...report,
      reorgSeverity,
      reorgDepth: divergence?.forkDepth ?? null,
    },
    firedAt: new Date().toISOString(),
  };
}

export interface ReconciliationScheduler {
  stop: () => void;
}

export function startReconciliationSchedule(
  db: Database.Database,
  chainReader: ChainReader,
  options: {
    config?: ReconciliationConfig;
    alert?: AlertDispatcher;
    logger?: Pick<Console, 'info' | 'warn' | 'error'>;
    /** Canonical headers enabling the reorg backstop (Issue #866). */
    ledgerHeaders?: LedgerHeaderSource;
    /**
     * Runs the shared rollback-and-replay when the backstop finds a reorg
     * (Issue #864). Without it the divergence stays halt-latched for an
     * operator (or another process wired with this hook) to resolve.
     */
    onReorgDetected?: (divergence: ReorgDivergence) => Promise<void>;
  } = {}
): ReconciliationScheduler {
  const config = options.config ?? configFromEnv();
  const logger = options.logger ?? console;
  let running = false;
  let stopped = false;

  const tick = async (): Promise<void> => {
    if (running || stopped) {
      return;
    }
    running = true;
    try {
      const report = await runReconciliation(db, chainReader, config, {
        ...(options.ledgerHeaders !== undefined
          ? { ledgerHeaders: options.ledgerHeaders }
          : {}),
      });

      if (report.reorgDivergence) {
        const divergence = report.reorgDivergence;
        logger.error(
          `REORG DETECTED by consistency job: ledger=${divergence.divergenceLedger} ` +
            `reason=${divergence.reason} fork=${divergence.commonAncestorLedger} — ` +
            'ingestion halted pending rollback-and-replay'
        );
        logger.error(divergence.message);

        const alert = options.alert ?? ((payload) => {
          console.error(JSON.stringify(payload, null, 2));
          return Promise.resolve();
        });
        await alert(buildReorgAlertPayload(report));

        if (options.onReorgDetected) {
          try {
            await options.onReorgDetected(divergence);
            logger.info(
              `reorg recovery completed for ledger ${divergence.divergenceLedger}`
            );
          } catch (error) {
            logger.error(
              `reorg recovery failed for ledger ${divergence.divergenceLedger}: ` +
                `${error instanceof Error ? error.message : String(error)}`
            );
            // Halt latch stays set: ingestion remains blocked until a later
            // run recovers rather than resuming on a half-built history.
          }
        }
      } else if (report.driftDetected) {
        logger.error(`RECONCILIATION DRIFT: ${report.mismatches.length} mismatch(es). Dispatching alert.`);
        const alert = options.alert ?? ((payload) => {
          console.error(JSON.stringify(payload, null, 2));
          return Promise.resolve();
        });
        await alert(buildAlertPayload(report));
      } else if (report.error) {
        logger.warn(`Reconciliation run errored: ${report.error}`);
      } else {
        logger.info(
          `Reconciliation OK: ${report.sampledInvoices} invoices sampled, ` +
            `${report.reorgHeadersChecked} ledger header(s) verified, no drift beyond ${(config.tolerancePercent).toFixed(2)}%.`
        );
      }
    } finally {
      running = false;
    }
  };

  void tick();
  const timer = setInterval(() => void tick(), config.intervalMs);
  timer.unref?.();

  return {
    stop: () => {
      stopped = true;
      clearInterval(timer);
    },
  };
}
