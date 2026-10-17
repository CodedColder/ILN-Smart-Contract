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
 * Usage (see scripts/replay.ts):
 *   tsx indexer/scripts/replay.ts --from-ledger 12345678 [--to-ledger 12400000]
 */

import type { EventRepository } from '../db/eventRepository.js';
import { EventListener } from './eventListener.js';
import type {
  EventListenerOptions,
  HorizonTransactionRecord,
} from './eventListener.js';

export interface ReplayOptions
  extends Omit<
    EventListenerOptions,
    'initialBackoffMs' | 'maxBackoffMs' | 'onReorgDetected'
  > {
  /** Ledger sequence to replay from (inclusive). */
  fromLedger: number;
  /** Optional exclusive upper bound; omit to replay up to the chain tip. */
  toLedger?: number;
  pageSize?: number;
}

export interface ReplayResult {
  fromLedger: number;
  lastProcessedLedger: number | null;
  transactionsProcessed: number;
  eventsIngested: number;
  reputationUpdatesIngested: number;
  failedTransactions: number;
}

interface HorizonPage {
  _embedded: {
    records: Array<HorizonTransactionRecord & { _links: { next?: unknown } }>;
  };
  _links: {
    next?: { href: string };
  };
}

export async function runReplay(options: ReplayOptions): Promise<ReplayResult> {
  // Wrap the repository to observe how many events replay actually ingested
  // (deduplicated re-inserts still count as processed work).
  let eventsIngested = 0;
  let reputationIngested = 0;
  const countingRepository: EventRepository = {
    getState: (key) => options.repository.getState(key),
    setState: (key, value) => options.repository.setState(key, value),
    getInvoice: (id) => options.repository.getInvoice(id),
    upsertInvoice: (invoice) => options.repository.upsertInvoice(invoice),
    insertEvent: (event) => {
      eventsIngested += 1;
      options.repository.insertEvent(event);
    },
    insertReputationUpdate: (update) => {
      reputationIngested += 1;
      options.repository.insertReputationUpdate(update);
    },
    // Every write must forward: a method missing here would throw at replay
    // time and silently drop the row the live path would have persisted
    // (reorg recovery runs through this same runner).
    insertInsuranceEnrollment: (enrollment) =>
      options.repository.insertInsuranceEnrollment(enrollment),
    insertInsurancePremium: (premium) => options.repository.insertInsurancePremium(premium),
    insertInsuranceClaim: (claim) => options.repository.insertInsuranceClaim(claim),
    getInsurancePoolStats: (contractId) => options.repository.getInsurancePoolStats(contractId),
    upsertInsurancePoolStats: (contractId, balance, premiums, claims, enrolledCount) =>
      options.repository.upsertInsurancePoolStats(
        contractId,
        balance,
        premiums,
        claims,
        enrolledCount
      ),
  };

  const fetchImpl = options.fetchImpl ?? fetch;
  const userLogger = options.logger ?? console;
  const pageSize = options.pageSize ?? 200;

  // EventListener.processTransaction logs-and-continues on per-transaction
  // failures; count those log lines so replay can report them.
  let failedTransactions = 0;
  const countingLogger: Pick<Console, 'info' | 'warn' | 'error'> = {
    info: (...args: Parameters<Console['info']>) => userLogger.info(...args),
    warn: (...args: Parameters<Console['warn']>) => userLogger.warn(...args),
    error: (...args: Parameters<Console['error']>) => {
      const text = args.join(' ');
      if (text.includes('failed to process transaction')) {
        failedTransactions += 1;
      }
      userLogger.error(...args);
    },
  };

  const listener = new EventListener({
    ...options,
    repository: countingRepository,
    logger: countingLogger,
  });

  const url = new URL('/transactions', options.horizonUrl.replace(/\/$/, ''));
  // Horizon's cursor is strictly-after: position one ledger back so the
  // checkpoint ledger itself is re-processed (inclusive --from-ledger).
  const startCursor = Math.max(options.fromLedger - 1, 0);
  url.searchParams.set('cursor', String(startCursor));
  url.searchParams.set('order', 'asc');
  url.searchParams.set('limit', String(pageSize));

  let nextHref: string | null = url.toString();
  let lastProcessedLedger: number | null = null;
  let transactionsProcessed = 0;

  while (nextHref !== null) {
    const response = await fetchImpl(nextHref, { headers: { Accept: 'application/json' } });
    if (!response.ok) {
      throw new Error(`Horizon responded with HTTP ${response.status} during replay`);
    }

    const page = (await response.json()) as HorizonPage;

    for (const record of page._embedded.records) {
      if (options.toLedger !== undefined && record.ledger >= options.toLedger) {
        nextHref = null;
        break;
      }

      try {
        await listener.processTransaction({
          hash: record.hash,
          ledger: record.ledger,
          created_at: record.created_at,
          paging_token: record.paging_token,
          result_meta_xdr: record.result_meta_xdr,
        });
        transactionsProcessed += 1;
        lastProcessedLedger = Math.max(lastProcessedLedger ?? record.ledger, record.ledger);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        // Not routed through logger.error to avoid double-counting with the
        // listener's own 'failed to process transaction' log interception.
        userLogger.error(`replay: unexpected failure on ${record.hash}: ${message}`);
        throw error instanceof Error ? error : new Error(message);
      }
    }

    // The loop breaks above when hitting toLedger; otherwise follow pagination.
    if (nextHref !== null) {
      nextHref = page._links.next?.href ?? null;
    }
  }

  return {
    fromLedger: options.fromLedger,
    lastProcessedLedger,
    transactionsProcessed,
    eventsIngested,
    reputationUpdatesIngested: reputationIngested,
    failedTransactions,
  };
}
