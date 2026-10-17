import { createServer } from 'node:http';
import { config } from './config.js';
import { getDb } from './database/db.js';
import { createApp } from './app.js';
import { EventWebSocketEndpoint } from './api/websocket.js';
import { createSqlEventRepository } from './db/eventRepository.js';
import { createEventListener } from './ingestion/eventListener.js';
import { createIngestionLock } from './ingestion/ingestionLock.js';
import { createHorizonLedgerHeaderSource } from './ingestion/ledgerHeaders.js';
import { LedgerReorgDetector } from './ingestion/reorgDetector.js';
import type { ReorgDivergence } from './ingestion/reorgDetector.js';
import { recoverFromReorg } from './ingestion/reorgRecovery.js';
import { startReconciliationSchedule, createWebhookAlertDispatcher } from './reconciliation/consistencyJob.js';
import { SorobanChainReader } from './reconciliation/chainReader.js';
import { logger } from './lib/logger.js';
import {
  classifyReorgDepth,
  classifyReorgSeverity,
  createAlert,
  getAlertRouter,
} from './services/alertRouter.js';

const db = getDb(config.dbPath);

// A single Soroban read-only reader, reused by the public `/protocol-status`
// endpoint (Issue #775) and by continuous reconciliation. Built whenever a
// contract id is configured; RPC URL falls back to the public testnet.
const sorobanRpcUrl = process.env.SOROBAN_RPC_URL || 'https://soroban-testnet.stellar.org';
const networkPassphrase = process.env.NETWORK_PASSPHRASE || 'Test SDF Network ; September 2015';
const chainReader = config.contractId
  ? new SorobanChainReader({
      rpcUrl: sorobanRpcUrl,
      contractId: config.contractId,
      networkPassphrase,
    })
  : undefined;

const app = createApp(db, { apiKeys: config.apiKeys, chainReader });
const eventRepository = createSqlEventRepository(db);

// Reorg handling (Issues #863/#864/#866): one header source (Horizon
// `GET /ledgers/{n}`) feeds both the live detector and the consistency-job
// backstop, one lock serializes recovery against ingestion, and both paths
// recover through the same rollback-and-replay implementation.
const ingestionLock = createIngestionLock({ db, logger });
const ledgerHeaderSource = createHorizonLedgerHeaderSource(config.horizonUrl);
const reorgDetector = new LedgerReorgDetector({ db, source: ledgerHeaderSource, logger });

const alertRouter = getAlertRouter();

const routeReorgAlert = async (divergence: ReorgDivergence): Promise<void> => {
  const severity = classifyReorgSeverity(divergence.forkDepth);
  const classification = classifyReorgDepth(divergence.forkDepth);
  const alert = createAlert(
    'reorg_detected',
    severity,
    `Ledger reorg detected at ledger ${divergence.divergenceLedger}`,
    divergence.message,
    {
      divergenceLedger: divergence.divergenceLedger,
      commonAncestorLedger: divergence.commonAncestorLedger,
      forkDepth: divergence.forkDepth,
      reason: divergence.reason,
      detectedBy: divergence.detectedBy,
      severity,
      reorgSeverity: classification,
    }
  );
  await alertRouter.route(alert);
};

const recoveryHook = async (divergence: ReorgDivergence): Promise<void> => {
  await recoverFromReorg(divergence, {
    db,
    repository: eventRepository,
    horizonUrl: config.horizonUrl,
    contractAddress: config.contractId,
    source: ledgerHeaderSource,
    lock: ingestionLock,
    logger,
  });
  await routeReorgAlert(divergence);
};

const eventListener = createEventListener({
  repository: eventRepository,
  horizonUrl: config.horizonUrl,
  contractAddress: config.contractId,
  reorgDetector,
  onReorgDetected: recoveryHook,
});

const httpServer = createServer(app);
const wsEndpoint = new EventWebSocketEndpoint({ server: httpServer, path: '/events' });
wsEndpoint.start();

if (config.contractId) {
  if (config.ingestionEnabled) {
    // Hold the ingestion lease for the lifetime of the loop: recovery
    // (same process, or a consistency job elsewhere) only rolls back while
    // it can prove no other writer is active. Losing the lease stops the
    // listener; regaining it restarts it from the persisted cursor.
    void ingestionLock
      .runAsLeader(async (signal) => {
        const stopListener = () => eventListener.stop();
        signal.addEventListener('abort', stopListener, { once: true });
        try {
          await eventListener.start();
        } finally {
          signal.removeEventListener('abort', stopListener);
        }
      })
      .catch((error) => {
        logger.error('indexer ingestion leadership loop exited unexpectedly', {
          error: error instanceof Error ? error.message : String(error),
        });
      });
  } else {
    logger.info('ingestion disabled in this process (INGESTION_ENABLED=false); read API only');
  }

  if (process.env.RECONCILIATION_ENABLED === 'true' && chainReader) {
    const alertUrl = process.env.RECONCILIATION_ALERT_URL;
    startReconciliationSchedule(db, chainReader, {
      ledgerHeaders: ledgerHeaderSource,
      onReorgDetected: recoverReorg,
      ...(alertUrl ? { alert: createWebhookAlertDispatcher(alertUrl) } : {}),
    });
    logger.info('continuous reconciliation schedule started');
  }
} else {
  logger.warn('ILN_CONTRACT_ID/CONTRACT_ID is not set; event ingestion is disabled');
}

httpServer.listen(config.port, () => {
  logger.info('ILN indexer API listening', { port: config.port, transport: 'http+ws' });
});

export { wsEndpoint, eventListener };
