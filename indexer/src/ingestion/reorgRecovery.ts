/**
 * Rollback-and-replay recovery for a detected chain reorg (Issue #864).
 *
 * On `ReorgDetected` the indexed state must be rolled back to the divergence
 * point's parent ledger (the last ledger whose stored header still matches
 * the canonical chain) and rebuilt forward from there. Re-derivation reuses
 * `replay.ts`'s checkpoint replay — the same idempotent-upsert path used for
 * ordinary corruption repair — so the two recovery mechanisms cannot drift
 * apart in behaviour.
 *
 * Guarantees:
 *
 *  - **Serialized against live ingestion.** The whole rollback+replay runs
 *    under `ingestionLock.ts` leadership (or reuses the leadership the live
 *    path already holds in this process), so a concurrent ingestor cannot
 *    interleave writes with the rollback. In-process calls are additionally
 *    chained through a single-flight promise.
 *  - **Fail closed.** The halt latch stays set until replay completes; if
 *    replay throws, ingestion remains blocked instead of resuming against a
 *    half-rolled-back database.
 *  - **Loop safe.** The stored headers of the rolled-back range are dropped
 *    with the rows they describe, so replaying the range cannot re-detect the
 *    divergence it just recovered from; a genuinely *second* divergence is
 *    re-latched by replay's own detector and reported as `reDetected`.
 */

import type Database from 'better-sqlite3';
import type { DecodedContractEvent, EventListenerOptions, HorizonTransactionRecord } from './eventListener.js';
import type { EventRepository } from '../db/eventRepository.js';
import type { IngestionLockHandle } from './ingestionLock.js';
import type { LedgerHeaderSource } from './ledgerHeaders.js';
import { createHorizonLedgerHeaderSource } from './ledgerHeaders.js';
import { runReplay, type ReplayOptions, type ReplayResult } from './replay.js';
import {
  LedgerReorgDetector,
  latchHalt,
  clearHaltLatch,
  clearPendingReorg,
  invalidateLedgerHeadersFrom,
  readPendingReorg,
} from './reorgDetector.js';
import type { ReorgDivergence } from './reorgDetector.js';

const LAST_LEDGER_STATE_KEY = 'last_processed_ledger';

export interface ReorgRecoveryOptions
  extends Pick<EventListenerOptions, 'repository' | 'horizonUrl' | 'contractAddress'> {
  db: Database.Database;
  /** Canonical chain truth for the replay's own reorg checks. */
  source?: LedgerHeaderSource;
  /** Serializes this rollback-and-replay against concurrent ingestion. */
  lock?: IngestionLockHandle;
  logger?: Pick<Console, 'info' | 'warn' | 'error'>;
  fetchImpl?: typeof fetch;
  decodeTransactionEvents?: (record: HorizonTransactionRecord) => DecodedContractEvent[];
  pageSize?: number;
  /** Upper bound on waiting for ingestion leadership. Defaults to 30s. */
  acquireTimeoutMs?: number;
}

export interface ReorgRecoveryResult {
  divergence: ReorgDivergence;
  /** Ledger whose state survives the rollback (divergence point's parent). */
  parentLedger: number;
  deletedEvents: number;
  deletedReputationUpdates: number;
  deletedInvoices: number;
  deletedLedgerHeaders: number;
  replay: ReplayResult | null;
  /** A newer divergence appeared while replaying — recovery must re-run. */
  reDetected: boolean;
  pendingReorg: ReorgDivergence | null;
}

/** In-process serialization: recoveries never interleave their awaits. */
let inFlight: Promise<unknown> = Promise.resolve();

/**
 * Run rollback-and-replay for `divergence`.
 *
 * Callers: the ingestion path (`EventListener.onReorgDetected`) and the
 * periodic consistency job (`consistencyJob.ts`), the backstop for a reorg
 * that slips past real-time detection (Issue #866). Both go through this
 * function so there is exactly one rollback-and-replay implementation.
 */
export async function recoverFromReorg(
  divergence: ReorgDivergence,
  options: ReorgRecoveryOptions
): Promise<ReorgRecoveryResult> {
  const run = inFlight.catch(() => undefined).then(() => runRecovery(divergence, options));
  inFlight = run.catch(() => undefined);
  return run;
}

async function runRecovery(
  divergence: ReorgDivergence,
  options: ReorgRecoveryOptions
): Promise<ReorgRecoveryResult> {
  const logger = options.logger ?? console;

  return withIngestionLock(options, async () => {
    // Block live ingestion for the duration of the rollback before touching
    // a single row: the consistency job detects divergences the live path has
    // not seen, so the latch may not be set yet when we get here.
    latchHalt(options.db, divergence);

    const rollback = rollbackIndexedState(options, divergence.divergenceLedger);
    logger.warn(
      `reorg recovery: rolled back to ledger ${divergence.divergenceLedger - 1} ` +
        `(${rollback.deletedEvents} events, ${rollback.deletedInvoices} invoices, ` +
        `${rollback.deletedLedgerHeaders} ledger headers dropped)`
    );

    // The latch stays set through replay: only recovery's own detector
    // (ignoreHaltLatch) keeps verifying headers while live writes are frozen.
    let replay: ReplayResult | null = null;
    try {
      replay = await replayFromDivergence(divergence, options);
    } catch (error) {
      // Fail closed: the rollback already happened, so keep the halt *and*
      // restore the divergence record so operators (and the next
      // consistency run) still see what blocked ingestion.
      latchHalt(options.db, divergence);
      throw error;
    }

    const pendingReorg = readPendingReorg(options.db);
    const reDetected = pendingReorg !== null;

    if (reDetected) {
      logger.error(
        `reorg recovery: second divergence at ledger ${pendingReorg?.divergenceLedger} ` +
          'while replaying — ingestion stays halted'
      );
    } else {
      clearHaltLatch(options.db);
    }

    return {
      divergence,
      parentLedger: divergence.divergenceLedger - 1,
      ...rollback,
      replay,
      reDetected,
      pendingReorg,
    };
  });
}

interface RollbackCounts {
  deletedEvents: number;
  deletedReputationUpdates: number;
  deletedInvoices: number;
  deletedLedgerHeaders: number;
}

/**
 * Delete every row produced at or after the divergence ledger and reset the
 * checkpoint to its parent, inside one transaction.
 *
 * Invoices carry no ledger column, so an invoice survives only if it still
 * has at least one event below the divergence point; rows introduced wholly
 * inside the rolled-back window are removed and recreated by the replay.
 *
 * The paging-token cursor is deliberately left alone: Horizon tokens are
 * monotonic across ledger sequence numbers, so a pre-reorg cursor resumes the
 * stream at the right place, and the replay already re-covers everything from
 * the divergence ledger to the chain tip.
 */
function rollbackIndexedState(
  options: ReorgRecoveryOptions,
  divergenceLedger: number
): RollbackCounts {
  const db = options.db;

  const run = db.transaction((): RollbackCounts => {
    const deletedEvents = db
      .prepare('DELETE FROM events WHERE ledger >= ?')
      .run(divergenceLedger).changes;
    const deletedReputationUpdates = db
      .prepare('DELETE FROM reputation_updates WHERE ledger >= ?')
      .run(divergenceLedger).changes;
    db.prepare('DELETE FROM insurance_pool_enrollments WHERE ledger >= ?').run(divergenceLedger);
    db.prepare('DELETE FROM insurance_pool_premiums WHERE ledger >= ?').run(divergenceLedger);
    db.prepare('DELETE FROM insurance_pool_claims WHERE ledger >= ?').run(divergenceLedger);

    const deletedInvoices = db
      .prepare(
        `DELETE FROM invoices
          WHERE id NOT IN (
            SELECT DISTINCT invoice_id FROM events WHERE invoice_id IS NOT NULL
          )`
      )
      .run().changes;

    const deletedLedgerHeaders = invalidateLedgerHeadersFrom(db, divergenceLedger);

    options.repository.setState(LAST_LEDGER_STATE_KEY, String(divergenceLedger - 1));
    // Drop the divergence record we are recovering from; the halt latch is
    // kept so live ingestion cannot resume mid-recovery.
    clearPendingReorg(db);

    return {
      deletedEvents,
      deletedReputationUpdates,
      deletedInvoices,
      deletedLedgerHeaders,
    };
  });

  return run();
}

async function replayFromDivergence(
  divergence: ReorgDivergence,
  options: ReorgRecoveryOptions
): Promise<ReplayResult> {
  const replayDetector = new LedgerReorgDetector({
    db: options.db,
    source: headerSourceFor(options),
    logger: options.logger ?? console,
    // Replay walks backwards over indexed ledgers by design; hash/parent
    // verification still runs and re-latches the halt on a second fork.
    strictSequence: false,
    ignoreHaltLatch: true,
    detectedBy: 'ingestion',
  });

  const replayOptions: ReplayOptions = {
    repository: options.repository,
    horizonUrl: options.horizonUrl,
    contractAddress: options.contractAddress,
    fromLedger: divergence.divergenceLedger,
    reorgDetector: replayDetector,
    ...(options.fetchImpl !== undefined ? { fetchImpl: options.fetchImpl } : {}),
    ...(options.decodeTransactionEvents !== undefined
      ? { decodeTransactionEvents: options.decodeTransactionEvents }
      : {}),
    ...(options.logger !== undefined ? { logger: options.logger } : {}),
    ...(options.pageSize !== undefined ? { pageSize: options.pageSize } : {}),
  };

  return runReplay(replayOptions);
}

function headerSourceFor(options: ReorgRecoveryOptions): LedgerHeaderSource {
  return (
    options.source ??
    createHorizonLedgerHeaderSource(options.horizonUrl, options.fetchImpl ?? fetch)
  );
}

/**
 * Run `fn` while this process holds ingestion leadership.
 *
 * When the caller already is the leader (recovery triggered from the live
 * ingestion path) the work runs directly — contending for the lease we hold
 * would deadlock against our own heartbeat. Otherwise leadership is contested
 * with the same compare-and-swap lease `ingestionLock.ts` uses, bounded by
 * `acquireTimeoutMs`, so a rollback can never interleave with a concurrent
 * ingestor's writes.
 */
async function withIngestionLock<T>(
  options: ReorgRecoveryOptions,
  fn: () => Promise<T>
): Promise<T> {
  const lock = options.lock;
  if (!lock) {
    return fn();
  }
  if (lock.isLeader()) {
    return fn();
  }

  const timeoutMs = options.acquireTimeoutMs ?? 30_000;
  const deadline = Date.now() + timeoutMs;
  while (!lock.tryAcquire()) {
    if (Date.now() >= deadline) {
      throw new Error(
        `timed out after ${timeoutMs}ms waiting for ingestion leadership to run reorg recovery`
      );
    }
    await sleep(250);
  }

  try {
    return await fn();
  } finally {
    lock.release();
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
