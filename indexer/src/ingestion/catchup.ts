/**
 * Historical catch-up: backfill a ledger range the live stream never saw
 * (e.g. from contract deployment up to the first ledger the indexer
 * streamed).
 *
 * Range: `[fromLedger, toLedger)` — `fromLedger` included, `toLedger`
 * excluded — processed in batches of `batchSize` ledgers.
 *
 * Guarantees:
 *
 *  - **Exact coverage.** Each batch reads Horizon `/transactions` from the
 *    TOID cursor of its first ledger and follows `_links.next` until the
 *    batch's end ledger (or the chain tip) is reached. Before every batch the
 *    range is checked against Horizon's served history window; ledgers
 *    outside it are reported as a gap, never marked complete.
 *  - **Same decoding and writes as live ingestion.** Records go through
 *    `EventListener.processTransaction` (idempotent: invoice upserts,
 *    events/reputation deduplicated on (transaction_hash, event_index)), with
 *    ledger-header reorg verification when a detector is supplied.
 *  - **Checkpoint after commit.** Every transaction's rows commit atomically;
 *    the range checkpoint (`catchup_checkpoint:<from>-<to>` in
 *    `indexer_state`) advances to a batch's end only after all of that
 *    batch's transactions committed. A crash, abort, database failure or
 *    retry exhaustion leaves the checkpoint at the last fully committed
 *    batch, and the next run resumes there — re-processing the partial batch
 *    idempotently.
 *  - **Live cursor untouched.** Backfilled ledgers never move the live
 *    stream's cursor.
 */

import type { EventRepository } from '../db/eventRepository.js';
import { EventListener } from './eventListener.js';
import type { DecodedContractEvent, HorizonTransactionRecord } from './eventListener.js';
import {
  DEFAULT_RETRY_POLICY,
  HorizonFetchError,
  fetchHorizonLedgerBounds,
  iterateTransactionRange,
  type HorizonLedgerBounds,
  type RetryPolicy,
} from './horizonPages.js';
import type { LedgerReorgDetector } from './reorgDetector.js';

export interface CatchupOptions {
  repository: EventRepository;
  horizonUrl: string;
  contractAddress: string;
  fetchImpl?: typeof fetch;
  decodeTransactionEvents?: (record: HorizonTransactionRecord) => DecodedContractEvent[];
  logger?: Pick<Console, 'info' | 'warn' | 'error'>;
  /** Verifies each backfilled ledger against canonical headers. */
  reorgDetector?: LedgerReorgDetector;
  /** Ledgers per checkpointed batch. Defaults to 1000. */
  batchSize?: number;
  /** Horizon page size (`limit`). Defaults to 200. */
  pageSize?: number;
  retry?: RetryPolicy;
  /** Horizon's served history window. Defaults to reading Horizon's root. */
  ledgerBounds?: () => Promise<HorizonLedgerBounds>;
  /** Stops the run between transactions; the checkpoint stays at the last committed batch. */
  signal?: AbortSignal;
}

export interface CatchupRange {
  /** First ledger to ingest (inclusive). */
  fromLedger: number;
  /** Ledger to stop before (exclusive). */
  toLedger: number;
  /** Ignore an existing checkpoint for this range and start over. */
  restart?: boolean;
}

export interface CatchupCheckpoint {
  fromLedger: number;
  toLedger: number;
  /** First ledger not yet covered by a committed batch. */
  nextLedger: number;
  status: 'in_progress' | 'complete';
  updatedAt: string;
}

export interface CatchupResult {
  fromLedger: number;
  toLedger: number;
  /** Ledger this run started from (later than `fromLedger` when resumed). */
  startedAtLedger: number;
  batchesCommitted: number;
  transactionsProcessed: number;
  duplicatesSkipped: number;
  /** The range was already complete; nothing was fetched. */
  alreadyComplete: boolean;
}

export type CatchupFailureKind = 'gap' | 'fetch' | 'ingestion' | 'reorg' | 'aborted';

/**
 * Catch-up stopped before covering its range. Everything before
 * `resumeFromLedger` is committed and checkpointed.
 */
export class CatchupError extends Error {
  constructor(
    message: string,
    readonly kind: CatchupFailureKind,
    readonly resumeFromLedger: number,
    /** Half-open ledger ranges Horizon cannot serve (kind `gap`). */
    readonly missingRanges: Array<[number, number]> = [],
    readonly cause?: unknown
  ) {
    super(message);
    this.name = 'CatchupError';
  }
}

export function catchupCheckpointKey(fromLedger: number, toLedger: number): string {
  return `catchup_checkpoint:${fromLedger}-${toLedger}`;
}

const DEFAULT_BATCH_SIZE = 1_000;
const DEFAULT_PAGE_SIZE = 200;

export class IngestionCatchup {
  private readonly options: CatchupOptions;
  private readonly logger: Pick<Console, 'info' | 'warn' | 'error'>;
  private readonly fetchImpl: typeof fetch;
  private readonly batchSize: number;
  private readonly pageSize: number;
  private readonly retry: RetryPolicy;
  private readonly listener: EventListener;

  constructor(options: CatchupOptions) {
    this.options = options;
    this.logger = options.logger ?? console;
    this.fetchImpl = options.fetchImpl ?? fetch;
    this.batchSize = positiveInteger(options.batchSize ?? DEFAULT_BATCH_SIZE, 'batchSize');
    this.pageSize = positiveInteger(options.pageSize ?? DEFAULT_PAGE_SIZE, 'pageSize');
    this.retry = options.retry ?? DEFAULT_RETRY_POLICY;
    this.listener = new EventListener({
      repository: options.repository,
      horizonUrl: options.horizonUrl,
      contractAddress: options.contractAddress,
      fetchImpl: this.fetchImpl,
      logger: this.logger,
      failOnTransactionError: true,
      persistLiveCursor: false,
      ...(options.decodeTransactionEvents !== undefined
        ? { decodeTransactionEvents: options.decodeTransactionEvents }
        : {}),
      ...(options.reorgDetector !== undefined ? { reorgDetector: options.reorgDetector } : {}),
    });
  }

  readCheckpoint(fromLedger: number, toLedger: number): CatchupCheckpoint | null {
    const raw = this.options.repository.getState(catchupCheckpointKey(fromLedger, toLedger));
    if (!raw) {
      return null;
    }
    try {
      const parsed = JSON.parse(raw) as CatchupCheckpoint;
      if (
        Number.isSafeInteger(parsed.nextLedger) &&
        parsed.nextLedger >= fromLedger &&
        parsed.nextLedger <= toLedger
      ) {
        return parsed;
      }
    } catch {
      // fall through: an unreadable checkpoint restarts the range
    }
    this.logger.warn(
      `catch-up: ignoring unreadable checkpoint for ${fromLedger}-${toLedger}; restarting range`
    );
    return null;
  }

  async executeCatchup(range: CatchupRange): Promise<CatchupResult> {
    const { fromLedger, toLedger } = range;
    if (!Number.isSafeInteger(fromLedger) || !Number.isSafeInteger(toLedger) || fromLedger < 0) {
      throw new RangeError(`invalid catch-up range ${fromLedger}-${toLedger}`);
    }
    if (toLedger < fromLedger) {
      throw new RangeError(`catch-up range end ${toLedger} is before its start ${fromLedger}`);
    }

    const checkpoint = range.restart ? null : this.readCheckpoint(fromLedger, toLedger);
    const startedAtLedger = checkpoint?.nextLedger ?? fromLedger;
    const result: CatchupResult = {
      fromLedger,
      toLedger,
      startedAtLedger,
      batchesCommitted: 0,
      transactionsProcessed: 0,
      duplicatesSkipped: 0,
      alreadyComplete: checkpoint?.status === 'complete',
    };

    if (result.alreadyComplete || startedAtLedger >= toLedger) {
      this.writeCheckpoint(fromLedger, toLedger, toLedger);
      this.logger.info(`catch-up: ledgers ${fromLedger}-${toLedger} already complete`);
      return result;
    }

    this.logger.info(
      `catch-up: ingesting ledgers [${startedAtLedger}, ${toLedger})` +
        (checkpoint ? ` (resuming checkpoint for [${fromLedger}, ${toLedger}))` : '')
    );

    let batchFrom = startedAtLedger;
    while (batchFrom < toLedger) {
      const batchTo = Math.min(batchFrom + this.batchSize, toLedger);
      this.throwIfAborted(batchFrom);
      await this.assertServed(batchFrom, batchTo);

      const batch = await this.ingestBatch(batchFrom, batchTo);
      result.transactionsProcessed += batch.transactions;
      result.duplicatesSkipped += batch.duplicates;

      // Only reached once every transaction in [batchFrom, batchTo) committed.
      try {
        this.writeCheckpoint(fromLedger, toLedger, batchTo);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        throw new CatchupError(
          `catch-up committed [${batchFrom}, ${batchTo}) but could not record the checkpoint: ${message}`,
          'ingestion',
          batchFrom,
          [],
          error
        );
      }
      result.batchesCommitted += 1;
      this.logger.info(
        `catch-up: committed ledgers [${batchFrom}, ${batchTo}) — ${batch.transactions} transaction(s), ` +
          `${toLedger - batchTo} ledger(s) remaining`
      );
      batchFrom = batchTo;
    }

    this.logger.info(`catch-up: ledgers [${fromLedger}, ${toLedger}) complete`);
    return result;
  }

  private async ingestBatch(
    batchFrom: number,
    batchTo: number
  ): Promise<{ transactions: number; duplicates: number }> {
    let transactions = 0;
    let duplicates = 0;
    let current = batchFrom;

    try {
      for await (const record of iterateTransactionRange({
        horizonUrl: this.options.horizonUrl,
        fromLedger: batchFrom,
        toLedger: batchTo,
        pageSize: this.pageSize,
        fetchImpl: this.fetchImpl,
        retry: this.retry,
        logger: this.logger,
        onDuplicate: () => {
          duplicates += 1;
        },
      })) {
        current = record.ledger;
        this.throwIfAborted(batchFrom);
        await this.listener.processTransaction(record);

        // processTransaction drops records once a divergence is flagged;
        // a batch containing dropped records must not be checkpointed.
        const detector = this.options.reorgDetector;
        if (detector && (detector.isHalted() || detector.pendingReorg() !== null)) {
          throw new CatchupError(
            `catch-up halted at ledger ${record.ledger}: ledger divergence flagged; run reorg recovery`,
            'reorg',
            batchFrom
          );
        }
        transactions += 1;
      }
    } catch (error) {
      if (error instanceof CatchupError) {
        throw error;
      }
      const message = error instanceof Error ? error.message : String(error);
      const kind: CatchupFailureKind = error instanceof HorizonFetchError ? 'fetch' : 'ingestion';
      this.logger.error(
        `catch-up: batch [${batchFrom}, ${batchTo}) failed at ledger ${current} (${kind}): ${message}`
      );
      throw new CatchupError(
        `catch-up batch [${batchFrom}, ${batchTo}) failed at ledger ${current}: ${message}`,
        kind,
        batchFrom,
        [],
        error
      );
    }

    return { transactions, duplicates };
  }

  /** Refuse to cover ledgers Horizon no longer (or does not yet) serve. */
  private async assertServed(batchFrom: number, batchTo: number): Promise<void> {
    let bounds: HorizonLedgerBounds;
    try {
      bounds = this.options.ledgerBounds
        ? await this.options.ledgerBounds()
        : await fetchHorizonLedgerBounds(
            this.options.horizonUrl,
            this.fetchImpl,
            this.retry,
            this.logger
          );
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      throw new CatchupError(
        `catch-up cannot verify Horizon history covers [${batchFrom}, ${batchTo}): ${message}`,
        'fetch',
        batchFrom,
        [],
        error
      );
    }

    const missing: Array<[number, number]> = [];
    if (batchFrom < bounds.elderLedger) {
      missing.push([batchFrom, Math.min(bounds.elderLedger, batchTo)]);
    }
    if (batchTo - 1 > bounds.latestLedger) {
      missing.push([Math.max(bounds.latestLedger + 1, batchFrom), batchTo]);
    }
    if (missing.length > 0) {
      const described = missing.map(([a, b]) => `[${a}, ${b})`).join(', ');
      this.logger.error(
        `catch-up: Horizon serves ledgers ${bounds.elderLedger}-${bounds.latestLedger}; ` +
          `missing ${described} — range left incomplete`
      );
      throw new CatchupError(
        `ledger gap: Horizon cannot serve ${described}`,
        'gap',
        batchFrom,
        missing
      );
    }
  }

  private writeCheckpoint(fromLedger: number, toLedger: number, nextLedger: number): void {
    const checkpoint: CatchupCheckpoint = {
      fromLedger,
      toLedger,
      nextLedger,
      status: nextLedger >= toLedger ? 'complete' : 'in_progress',
      updatedAt: new Date().toISOString(),
    };
    this.options.repository.setState(
      catchupCheckpointKey(fromLedger, toLedger),
      JSON.stringify(checkpoint)
    );
  }

  private throwIfAborted(resumeFromLedger: number): void {
    if (this.options.signal?.aborted) {
      throw new CatchupError('catch-up aborted', 'aborted', resumeFromLedger);
    }
  }
}

function positiveInteger(value: number, name: string): number {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new RangeError(`${name} must be a positive integer, got ${value}`);
  }
  return value;
}
