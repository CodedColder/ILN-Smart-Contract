/**
 * Checkpoint-based event replay.
 *
 * Re-processes Horizon transactions from an arbitrary starting ledger so
 * derived state corrupted by an indexer bug can be rebuilt without a full
 * resync from genesis. Processing reuses the live ingestion path
 * (EventListener.processTransaction), whose writes are idempotent:
 *   - invoices: upsert (ON CONFLICT(id) DO UPDATE) — corrected values win
 *   - events / reputation_updates: deduplicated on (transaction_hash, event_index)
 *
 * Each transaction's rows and the cursor covering it commit in one database
 * transaction, so a crash or commit failure leaves the database at a
 * transaction boundary. Replay is strict by default: the first transaction
 * that cannot be decoded or persisted stops the run with
 * `ReplayIncompleteError`, whose `resumeFromLedger` is safe to pass back as
 * `--from-ledger` (the ledger is re-processed idempotently). Horizon reads
 * retry transient failures with bounded backoff before giving up.
 *
 * Usage (see scripts/replay.ts):
 *   tsx indexer/scripts/replay.ts --from-ledger 12345678 [--to-ledger 12400000]
 */

import type { EventRepository } from '../db/eventRepository.js';
import { EventListener } from './eventListener.js';
import type { EventListenerOptions } from './eventListener.js';
import { iterateTransactionRange, type RetryPolicy } from './horizonPages.js';
import type { ReorgDivergence } from './reorgDetector.js';

export interface ReplayOptions
  extends Omit<
    EventListenerOptions,
    'initialBackoffMs' | 'maxBackoffMs' | 'onReorgDetected' | 'failOnTransactionError'
  > {
  /** Ledger sequence to replay from (inclusive). */
  fromLedger: number;
  /** Optional exclusive upper bound; omit to replay up to the chain tip. */
  toLedger?: number;
  pageSize?: number;
  /** Bounded retry policy for Horizon page reads. */
  retry?: RetryPolicy;
  /**
   * Log and skip transactions that fail to decode instead of stopping. The
   * skipped hashes are reported in `failedTransactionHashes`; the range is
   * then not `complete`. Persistence failures always stop the run.
   */
  continueOnError?: boolean;
}

export interface ReplayResult {
  fromLedger: number;
  lastProcessedLedger: number | null;
  transactionsProcessed: number;
  eventsIngested: number;
  reputationUpdatesIngested: number;
  failedTransactions: number;
  failedTransactionHashes: string[];
  /** Transactions Horizon returned twice across overlapping pages. */
  duplicatesSkipped: number;
  /**
   * A ledger divergence was (or already was) flagged while replaying. Rows
   * after it were not written: they would describe a non-canonical chain.
   */
  haltedByReorg: boolean;
  /** The flagged divergence, when one is recorded. */
  pendingReorg: ReorgDivergence | null;
  /** Every transaction in the range was persisted. */
  complete: boolean;
}

/**
 * Replay stopped before covering its range. Everything before
 * `resumeFromLedger` is committed; re-run from that ledger to finish.
 */
export class ReplayIncompleteError extends Error {
  constructor(
    message: string,
    readonly resumeFromLedger: number,
    readonly partial: ReplayResult,
    readonly cause?: unknown
  ) {
    super(message);
    this.name = 'ReplayIncompleteError';
  }
}

export async function runReplay(options: ReplayOptions): Promise<ReplayResult> {
  // Wrap the repository to observe how many events replay actually ingested
  // (deduplicated re-inserts still count as processed work).
  let eventsIngested = 0;
  let reputationIngested = 0;
  const base = options.repository;
  const countingRepository: EventRepository = {
    ...(base.transaction ? { transaction: <T>(fn: () => T): T => base.transaction!(fn) } : {}),
    ...(base.hasLaterInvoiceEvent
      ? { hasLaterInvoiceEvent: base.hasLaterInvoiceEvent.bind(base) }
      : {}),
    ...(base.listInvoiceEvents ? { listInvoiceEvents: base.listInvoiceEvents.bind(base) } : {}),
    getState: (key) => base.getState(key),
    setState: (key, value) => base.setState(key, value),
    getInvoice: (id) => base.getInvoice(id),
    upsertInvoice: (invoice) => base.upsertInvoice(invoice),
    insertEvent: (event) => {
      eventsIngested += 1;
      base.insertEvent(event);
    },
    insertReputationUpdate: (update) => {
      reputationIngested += 1;
      base.insertReputationUpdate(update);
    },
    // Every write must forward: a method missing here would throw at replay
    // time and silently drop the row the live path would have persisted
    // (reorg recovery runs through this same runner).
    insertInsuranceEnrollment: (enrollment) => base.insertInsuranceEnrollment(enrollment),
    insertInsurancePremium: (premium) => base.insertInsurancePremium(premium),
    insertInsuranceClaim: (claim) => base.insertInsuranceClaim(claim),
    getInsurancePoolStats: (contractId) => base.getInsurancePoolStats(contractId),
    upsertInsurancePoolStats: (contractId, balance, premiums, claims, enrolledCount) =>
      base.upsertInsurancePoolStats(contractId, balance, premiums, claims, enrolledCount),
  };

  const fetchImpl = options.fetchImpl ?? fetch;
  const userLogger = options.logger ?? console;
  const pageSize = options.pageSize ?? 200;
  const continueOnError = options.continueOnError ?? false;

  // EventListener.processTransaction logs-and-continues on per-transaction
  // decode failures in continue-on-error mode; count those log lines so
  // replay can report them.
  const failedTransactionHashes: string[] = [];
  const countingLogger: Pick<Console, 'info' | 'warn' | 'error'> = {
    info: (...args: Parameters<Console['info']>) => userLogger.info(...args),
    warn: (...args: Parameters<Console['warn']>) => userLogger.warn(...args),
    error: (...args: Parameters<Console['error']>) => {
      const text = args.join(' ');
      const match = /failed to process transaction (\S+):/.exec(text);
      if (match) {
        failedTransactionHashes.push(match[1]!);
      }
      userLogger.error(...args);
    },
  };

  const listener = new EventListener({
    ...options,
    repository: countingRepository,
    logger: countingLogger,
    failOnTransactionError: !continueOnError,
  });

  const detector = options.reorgDetector;
  const reorgBlocking = (): boolean =>
    detector !== undefined && (detector.isHalted() || detector.pendingReorg() !== null);

  let lastProcessedLedger: number | null = null;
  let transactionsProcessed = 0;
  let duplicatesSkipped = 0;
  let haltedByReorg = reorgBlocking();

  const snapshot = (stoppedEarly = false): ReplayResult => ({
    fromLedger: options.fromLedger,
    lastProcessedLedger,
    transactionsProcessed,
    eventsIngested,
    reputationUpdatesIngested: reputationIngested,
    failedTransactions: failedTransactionHashes.length,
    failedTransactionHashes: [...failedTransactionHashes],
    duplicatesSkipped,
    haltedByReorg,
    pendingReorg: detector?.pendingReorg() ?? null,
    complete: !stoppedEarly && !haltedByReorg && failedTransactionHashes.length === 0,
  });

  if (haltedByReorg) {
    userLogger.error(
      'replay: refusing to write while a ledger divergence is pending; run reorg recovery first'
    );
    return snapshot();
  }

  // Ledger of the transaction currently in flight: everything before it is
  // committed, so it is the safe resume point if anything below throws.
  let resumeFromLedger = options.fromLedger;

  try {
    for await (const record of iterateTransactionRange({
      horizonUrl: options.horizonUrl,
      fromLedger: options.fromLedger,
      ...(options.toLedger !== undefined ? { toLedger: options.toLedger } : {}),
      pageSize,
      fetchImpl,
      ...(options.retry !== undefined ? { retry: options.retry } : {}),
      logger: userLogger,
      onDuplicate: () => {
        duplicatesSkipped += 1;
      },
    })) {
      resumeFromLedger = record.ledger;
      await listener.processTransaction({
        hash: record.hash,
        ledger: record.ledger,
        created_at: record.created_at,
        paging_token: record.paging_token,
        result_meta_xdr: record.result_meta_xdr,
      });

      // The listener drops records (without throwing) once a divergence is
      // flagged; stop here so nothing built on the stale fork is presented
      // as a completed replay.
      haltedByReorg = reorgBlocking();
      if (haltedByReorg) {
        userLogger.error(
          `replay: stopped at ledger ${record.ledger}; ledger divergence flagged, ` +
            'rows from this point on were not written'
        );
        break;
      }

      transactionsProcessed += 1;
      lastProcessedLedger = Math.max(lastProcessedLedger ?? record.ledger, record.ledger);
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    userLogger.error(`replay: stopped at ledger ${resumeFromLedger}: ${message}`);
    throw new ReplayIncompleteError(
      `replay stopped at ledger ${resumeFromLedger}: ${message}`,
      resumeFromLedger,
      snapshot(true),
      error
    );
  }

  return snapshot();
}
