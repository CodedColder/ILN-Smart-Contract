/**
 * Reconciliation CLI.
 *
 * One-shot run (default) or continuous schedule (--watch):
 *   tsx indexer/scripts/reconcile.ts --once
 *   tsx indexer/scripts/reconcile.ts --watch
 *
 * Environment:
 *   DB_PATH                        SQLite file (default ./indexer.db)
 *   SOROBAN_RPC_URL                Soroban RPC endpoint
 *   ILN_CONTRACT_ID / CONTRACT_ID  invoice-liquidity contract address
 *   NETWORK_PASSPHRASE             network passphrase (default Testnet)
 *   RECONCILIATION_ALERT_URL       notifications service intake URL for alerts
 *   RECONCILIATION_INTERVAL_MS / _SAMPLE_SIZE / _TOLERANCE_PERCENT  (see consistencyJob.ts)
 */

import Database from 'better-sqlite3';
import { config } from '../src/config.js';
import { createSqlEventRepository } from '../src/db/eventRepository.js';
import { createHorizonLedgerHeaderSource } from '../src/ingestion/ledgerHeaders.js';
import { createIngestionLock } from '../src/ingestion/ingestionLock.js';
import { recoverFromReorg } from '../src/ingestion/reorgRecovery.js';
import type { ReorgDivergence } from '../src/ingestion/reorgDetector.js';
import {
  buildReorgAlertPayload,
  createWebhookAlertDispatcher,
  startReconciliationSchedule,
} from '../src/reconciliation/consistencyJob.js';
import { DEFAULT_RECONCILIATION_CONFIG } from '../src/reconciliation/consistencyJob.js';
import { SorobanChainReader } from '../src/reconciliation/chainReader.js';

const watch = process.argv.includes('--watch');

if (!config.contractId) {
  console.error('ILN_CONTRACT_ID/CONTRACT_ID is required for chain reconciliation.');
  process.exit(1);
}

const rpcUrl = process.env.SOROBAN_RPC_URL || 'https://soroban-testnet.stellar.org';
const networkPassphrase =
  process.env.NETWORK_PASSPHRASE || 'Test SDF Network ; September 2015';

const db = new Database(config.dbPath);
const chainReader = new SorobanChainReader({
  rpcUrl,
  contractId: config.contractId,
  networkPassphrase,
});

const alertUrl = process.env.RECONCILIATION_ALERT_URL;
const alert = alertUrl ? createWebhookAlertDispatcher(alertUrl) : undefined;
if (!alertUrl) {
  console.warn('RECONCILIATION_ALERT_URL not set — drift alerts will be logged only.');
}

// Reorg backstop (Issue #866): same header source, lock and
// rollback-and-replay path the running indexer uses.
const ledgerHeaders = createHorizonLedgerHeaderSource(config.horizonUrl);
const ingestionLock = createIngestionLock({ db });
const onReorgDetected = (divergence: ReorgDivergence) =>
  recoverFromReorg(divergence, {
    db,
    repository: createSqlEventRepository(db),
    horizonUrl: config.horizonUrl,
    contractAddress: config.contractId,
    source: ledgerHeaders,
    lock: ingestionLock,
  });

if (watch) {
  console.log(
    `Starting reconciliation watch every ${DEFAULT_RECONCILIATION_CONFIG.intervalMs}ms ` +
      `(sample=${DEFAULT_RECONCILIATION_CONFIG.sampleSize}, tolerance=${DEFAULT_RECONCILIATION_CONFIG.tolerancePercent}%).`
  );
  const scheduler = startReconciliationSchedule(db, chainReader, {
    alert,
    ledgerHeaders,
    onReorgDetected,
  });
  for (const signal of ['SIGINT', 'SIGTERM'] as const) {
    process.on(signal, () => {
      scheduler.stop();
      db.close();
      process.exit(0);
    });
  }
} else {
  import('../src/reconciliation/consistencyJob.js')
    .then(({ runReconciliation }) => runReconciliation(db, chainReader, undefined, { ledgerHeaders }))
    .then(async (report) => {
      if (report.reorgDivergence) {
        // Halt latch is already set by the run; recover or leave it for the
        // running indexer — either way the operator sees the divergence.
        if (alert) {
          await alert(buildReorgAlertPayload(report));
        }
        await onReorgDetected(report.reorgDivergence);
        return report;
      }
      if (alert && report.driftDetected) {
        return alert({
          type: 'indexer_drift_detected',
          severity: 'critical',
          summary: `Indexer drift detected: ${report.driftedInvoices}/${report.sampledInvoices} sampled invoices drifted.`,
          details: report,
          firedAt: new Date().toISOString(),
        }).then(() => report);
      }
      return report;
    })
    .then((report) => {
      console.log(JSON.stringify(report, null, 2));
      db.close();
      process.exitCode = report.driftDetected ? 2 : 0;
    })
    .catch((error) => {
      console.error(`Reconciliation crashed: ${error instanceof Error ? error.message : String(error)}`);
      db.close();
      process.exitCode = 1;
    });
}
