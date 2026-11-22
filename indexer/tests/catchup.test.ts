/**
 * Historical catch-up: exact range coverage, pagination, bounded retries,
 * checkpointing only after commit, resumability and gap reporting
 * (Issue #1163).
 */

import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import Database from 'better-sqlite3';
import { initializeSchema } from '../src/db/schema.js';
import { createSqlEventRepository, type EventRepository } from '../src/db/eventRepository.js';
import {
  CatchupError,
  IngestionCatchup,
  catchupCheckpointKey,
  type CatchupOptions,
} from '../src/ingestion/catchup.js';
import { LedgerReorgDetector, storeLedgerHeader } from '../src/ingestion/reorgDetector.js';
import { createStaticLedgerHeaderSource } from '../src/ingestion/ledgerHeaders.js';
import { runReplay } from '../src/ingestion/replay.js';
import {
  CONTRACT,
  fundedEvent,
  jsonResponse,
  makeHorizonFixture,
  submittedEvent,
  type FakeEventSpec,
  type HorizonFixtureOptions,
} from './horizonFixture.js';

type DB = Database.Database;

const silentLogger = { info: () => undefined, warn: () => undefined, error: () => undefined };
const FAST_RETRY = { maxAttempts: 3, initialBackoffMs: 0, maxBackoffMs: 0 };
const HORIZON = 'https://horizon.example';

function createTestDb(): DB {
  const db = new Database(':memory:');
  db.pragma('journal_mode = WAL');
  db.pragma('foreign_keys = ON');
  initializeSchema(db);
  return db;
}

function count(db: DB, table: string): number {
  return (db.prepare(`SELECT COUNT(*) AS n FROM ${table}`).get() as { n: number }).n;
}

function eventHashes(db: DB): string[] {
  return (
    db.prepare('SELECT transaction_hash FROM events ORDER BY ledger, id').all() as Array<{
      transaction_hash: string;
    }>
  ).map((row) => row.transaction_hash);
}

/** One InvoiceSubmitted per spec, invoice id = ledger. */
function submittedAt(ledgers: number[], prefix = 'tx'): FakeEventSpec[] {
  return ledgers.map((ledger, i) => ({
    ledger,
    hash: `${prefix}-${ledger}-${i}`,
    events: submittedEvent(ledger * 10 + i, String(1000 + ledger)),
  }));
}

function makeCatchup(
  db: DB,
  specs: FakeEventSpec[],
  overrides: Partial<CatchupOptions> = {},
  fixtureOptions: HorizonFixtureOptions = {}
) {
  const fixture = makeHorizonFixture(specs, CONTRACT, fixtureOptions);
  const catchup = new IngestionCatchup({
    repository: createSqlEventRepository(db),
    horizonUrl: HORIZON,
    contractAddress: CONTRACT,
    fetchImpl: fixture.fetchImpl,
    decodeTransactionEvents: fixture.decodeTransactionEvents,
    logger: silentLogger,
    retry: FAST_RETRY,
    ledgerBounds: async () => ({ elderLedger: 1, latestLedger: 1_000_000 }),
    ...overrides,
  });
  return { catchup, fixture };
}

function checkpoint(db: DB, from: number, to: number) {
  const raw = createSqlEventRepository(db).getState(catchupCheckpointKey(from, to));
  return raw ? (JSON.parse(raw) as { nextLedger: number; status: string }) : null;
}

/** Wraps the SQL repository, failing chosen writes at the persistence boundary. */
function failingRepository(
  db: DB,
  shouldFail: (method: string, arg: unknown) => boolean
): EventRepository {
  const base = createSqlEventRepository(db);
  return {
    ...base,
    insertEvent(event) {
      if (shouldFail('insertEvent', event)) throw new Error('simulated SQLITE_IOERR on insertEvent');
      base.insertEvent(event);
    },
    setState(key, value) {
      if (shouldFail('setState', key)) throw new Error(`simulated SQLITE_FULL writing ${key}`);
      base.setState(key, value);
    },
  };
}

describe('historical catch-up (IngestionCatchup)', () => {
  let db: DB;

  beforeEach(() => {
    db = createTestDb();
  });

  afterEach(() => {
    db.close();
  });

  it('ingests exactly [fromLedger, toLedger): start included, end excluded', async () => {
    const { catchup } = makeCatchup(db, submittedAt([99, 100, 104, 105, 106]), { batchSize: 2 });

    const result = await catchup.executeCatchup({ fromLedger: 100, toLedger: 105 });

    expect(eventHashes(db)).toEqual(['tx-100-1', 'tx-104-2']);
    expect(result.transactionsProcessed).toBe(2);
    // [100,102) [102,104) [104,105)
    expect(result.batchesCommitted).toBe(3);
    expect(checkpoint(db, 100, 105)).toMatchObject({ nextLedger: 105, status: 'complete' });
  });

  it('positions the Horizon cursor at the TOID of the first ledger, not the bare ledger number', async () => {
    const { catchup, fixture } = makeCatchup(db, submittedAt([100]));

    await catchup.executeCatchup({ fromLedger: 100, toLedger: 101 });

    const firstPage = new URL(fixture.requests.find((u) => u.includes('/transactions'))!);
    expect(firstPage.searchParams.get('cursor')).toBe((100n << 32n).toString());
    expect(firstPage.searchParams.get('order')).toBe('asc');
  });

  it('follows continuation links across partial pages and stops at the tip even though Horizon always returns next', async () => {
    // Seven transactions, three sharing ledger 12, two-record pages.
    const specs = submittedAt([10, 11, 12, 12, 12, 13, 15]);
    const { catchup, fixture } = makeCatchup(
      db,
      specs,
      { pageSize: 2, batchSize: 100 },
      { alwaysNext: true }
    );

    const result = await catchup.executeCatchup({ fromLedger: 10, toLedger: 20 });

    expect(result.transactionsProcessed).toBe(7);
    expect(count(db, 'events')).toBe(7);
    // 4 pages of records + 1 empty tip page; then termination.
    expect(fixture.requests.filter((u) => u.includes('/transactions'))).toHaveLength(5);
  });

  it('skips duplicate transactions served by overlapping pages', async () => {
    const { catchup } = makeCatchup(
      db,
      submittedAt([10, 11, 12, 13, 14]),
      { pageSize: 2 },
      { overlap: 1 }
    );

    const result = await catchup.executeCatchup({ fromLedger: 10, toLedger: 15 });

    expect(count(db, 'events')).toBe(5);
    expect(result.transactionsProcessed).toBe(5);
    expect(result.duplicatesSkipped).toBeGreaterThan(0);
  });

  it('retries transient Horizon failures (503, network error) within the bound', async () => {
    const { catchup } = makeCatchup(db, submittedAt([10, 11]), {}, {
      intercept: (n, url) => {
        if (!url.pathname.startsWith('/transactions')) return undefined;
        if (n === 0) return jsonResponse({}, 503);
        if (n === 1) throw new TypeError('fetch failed: ECONNRESET');
        return undefined;
      },
    });

    const result = await catchup.executeCatchup({ fromLedger: 10, toLedger: 12 });

    expect(result.transactionsProcessed).toBe(2);
    expect(checkpoint(db, 10, 12)?.status).toBe('complete');
  });

  it('fails terminally on retry exhaustion without advancing the checkpoint past the failed batch', async () => {
    let transactionCalls = 0;
    const { catchup } = makeCatchup(db, submittedAt([10, 11, 12, 13]), { batchSize: 2 }, {
      intercept: (_n, url) => {
        if (!url.pathname.startsWith('/transactions')) return undefined;
        transactionCalls += 1;
        // First batch [10,12) is served; every request for the second fails.
        return transactionCalls > 1 ? jsonResponse({}, 503) : undefined;
      },
    });

    const error = await catchup.executeCatchup({ fromLedger: 10, toLedger: 14 }).catch((e) => e);

    expect(error).toBeInstanceOf(CatchupError);
    expect(error.kind).toBe('fetch');
    expect(error.resumeFromLedger).toBe(12);
    expect(transactionCalls).toBe(1 + FAST_RETRY.maxAttempts);
    expect(checkpoint(db, 10, 14)).toMatchObject({ nextLedger: 12, status: 'in_progress' });
    expect(eventHashes(db)).toEqual(['tx-10-0', 'tx-11-1']);

    // Horizon recovers: the next run resumes at the failed batch only.
    const resumed = makeCatchup(db, submittedAt([10, 11, 12, 13]), { batchSize: 2 });
    const result = await resumed.catchup.executeCatchup({ fromLedger: 10, toLedger: 14 });
    expect(result.startedAtLedger).toBe(12);
    expect(eventHashes(db)).toEqual(['tx-10-0', 'tx-11-1', 'tx-12-2', 'tx-13-3']);
    expect(checkpoint(db, 10, 14)?.status).toBe('complete');
  });

  it('does not retry a non-transient 4xx', async () => {
    let calls = 0;
    const { catchup } = makeCatchup(db, submittedAt([10]), {}, {
      intercept: (_n, url) => {
        if (!url.pathname.startsWith('/transactions')) return undefined;
        calls += 1;
        return jsonResponse({}, 400);
      },
    });

    await expect(catchup.executeCatchup({ fromLedger: 10, toLedger: 11 })).rejects.toMatchObject({
      kind: 'fetch',
    });
    expect(calls).toBe(1);
  });

  it('rolls back a transaction whose write fails and leaves the checkpoint for resume', async () => {
    let failures = 1;
    const repository = failingRepository(db, (method, arg) => {
      if (method === 'insertEvent' && (arg as { transaction_hash: string }).transaction_hash === 'tx-11-1' && failures > 0) {
        failures -= 1;
        return true;
      }
      return false;
    });
    const { catchup } = makeCatchup(db, submittedAt([10, 11, 12]), { repository, batchSize: 10 });

    const error = await catchup.executeCatchup({ fromLedger: 10, toLedger: 13 }).catch((e) => e);

    expect(error).toBeInstanceOf(CatchupError);
    expect(error.kind).toBe('ingestion');
    // tx-10 committed; tx-11's invoice upsert was rolled back with its failed
    // event insert — no half-written transaction.
    expect(eventHashes(db)).toEqual(['tx-10-0']);
    expect(db.prepare('SELECT id FROM invoices WHERE id = 111').get()).toBeUndefined();
    expect(checkpoint(db, 10, 13)).toBeNull();

    const result = await catchup.executeCatchup({ fromLedger: 10, toLedger: 13 });
    expect(result.startedAtLedger).toBe(10);
    expect(eventHashes(db)).toEqual(['tx-10-0', 'tx-11-1', 'tx-12-2']);
    expect(count(db, 'invoices')).toBe(3);
  });

  it('reports a checkpoint write failure and re-covers the batch idempotently on resume', async () => {
    let failCheckpoint = true;
    const repository = failingRepository(
      db,
      (method, key) => method === 'setState' && String(key).startsWith('catchup_checkpoint') && failCheckpoint
    );
    const { catchup } = makeCatchup(db, submittedAt([10, 11]), { repository });

    await expect(catchup.executeCatchup({ fromLedger: 10, toLedger: 12 })).rejects.toMatchObject({
      kind: 'ingestion',
      resumeFromLedger: 10,
    });
    expect(checkpoint(db, 10, 12)).toBeNull();

    failCheckpoint = false;
    await catchup.executeCatchup({ fromLedger: 10, toLedger: 12 });
    expect(count(db, 'events')).toBe(2);
    expect(checkpoint(db, 10, 12)?.status).toBe('complete');
  });

  it('survives process interruption mid-batch and a fresh process resumes from the checkpoint', async () => {
    const specs = submittedAt([10, 11, 12, 13, 14, 15]);
    const controller = new AbortController();
    let processed = 0;
    const base = createSqlEventRepository(db);
    // "Kill" the process after the 4th transaction commits (mid second batch).
    const repository: EventRepository = {
      ...base,
      insertEvent(event) {
        base.insertEvent(event);
        processed += 1;
        if (processed === 4) controller.abort();
      },
    };
    const first = makeCatchup(db, specs, { repository, batchSize: 3, signal: controller.signal });

    await expect(first.catchup.executeCatchup({ fromLedger: 10, toLedger: 16 })).rejects.toMatchObject({
      kind: 'aborted',
      resumeFromLedger: 13,
    });
    expect(checkpoint(db, 10, 16)).toMatchObject({ nextLedger: 13, status: 'in_progress' });
    expect(count(db, 'events')).toBe(4); // tx 13 committed, batch not checkpointed

    // New process, new repository object, same database file.
    const second = makeCatchup(db, specs, { batchSize: 3 });
    const result = await second.catchup.executeCatchup({ fromLedger: 10, toLedger: 16 });

    expect(result.startedAtLedger).toBe(13);
    expect(count(db, 'events')).toBe(6);
    expect(new Set(eventHashes(db)).size).toBe(6);
    expect(checkpoint(db, 10, 16)?.status).toBe('complete');
  });

  it('reports ledgers Horizon no longer retains as a gap and marks nothing complete', async () => {
    const { catchup, fixture } = makeCatchup(db, submittedAt([10, 50]), {
      ledgerBounds: async () => ({ elderLedger: 40, latestLedger: 1_000 }),
    });

    const error = await catchup.executeCatchup({ fromLedger: 10, toLedger: 60 }).catch((e) => e);

    expect(error).toBeInstanceOf(CatchupError);
    expect(error.kind).toBe('gap');
    expect(error.missingRanges).toEqual([[10, 40]]);
    expect(count(db, 'events')).toBe(0);
    expect(checkpoint(db, 10, 60)).toBeNull();
    expect(fixture.requests.some((u) => u.includes('/transactions'))).toBe(false);
  });

  it('commits batches Horizon has and reports the tail beyond its latest ledger as a gap', async () => {
    const { catchup } = makeCatchup(db, submittedAt([10, 15]), {
      batchSize: 5,
      ledgerBounds: async () => ({ elderLedger: 1, latestLedger: 17 }),
    });

    const error = await catchup.executeCatchup({ fromLedger: 10, toLedger: 25 }).catch((e) => e);

    expect(error.kind).toBe('gap');
    expect(error.missingRanges).toEqual([[18, 20]]);
    expect(checkpoint(db, 10, 25)).toMatchObject({ nextLedger: 15, status: 'in_progress' });
  });

  it('is idempotent when a completed range is run again', async () => {
    const specs = submittedAt([10, 11, 12]);
    const { catchup, fixture } = makeCatchup(db, specs);
    await catchup.executeCatchup({ fromLedger: 10, toLedger: 13 });
    const before = db.prepare('SELECT * FROM invoices ORDER BY id').all();

    const requestsBefore = fixture.requests.length;
    const again = await catchup.executeCatchup({ fromLedger: 10, toLedger: 13 });
    expect(again.alreadyComplete).toBe(true);
    expect(fixture.requests.length).toBe(requestsBefore);

    const forced = await catchup.executeCatchup({ fromLedger: 10, toLedger: 13, restart: true });
    expect(forced.transactionsProcessed).toBe(3);
    expect(count(db, 'events')).toBe(3);
    expect(db.prepare('SELECT * FROM invoices ORDER BY id').all()).toEqual(before);
  });

  it('never moves the live stream cursor', async () => {
    const repo = createSqlEventRepository(db);
    repo.setState('last_processed_cursor', '999999999999');
    repo.setState('last_processed_ledger', '500');
    const { catchup } = makeCatchup(db, submittedAt([10, 11]));

    await catchup.executeCatchup({ fromLedger: 10, toLedger: 12 });

    expect(repo.getState('last_processed_cursor')).toBe('999999999999');
    expect(repo.getState('last_processed_ledger')).toBe('500');
  });

  it('backfills an older event without rolling back newer invoice state already indexed live', async () => {
    // Live ingestion only saw the funding at ledger 200.
    const live = makeHorizonFixture([
      { ledger: 200, hash: 'tx-fund', events: fundedEvent(7, 'GLP', '1000000') },
    ]);
    await runReplay({
      repository: createSqlEventRepository(db),
      horizonUrl: HORIZON,
      contractAddress: CONTRACT,
      fetchImpl: live.fetchImpl,
      decodeTransactionEvents: live.decodeTransactionEvents,
      fromLedger: 200,
      toLedger: 201,
      logger: silentLogger,
    });
    expect(db.prepare('SELECT amount, token FROM invoices WHERE id = 7').get()).toEqual({
      amount: '0',
      token: '',
    });

    // Catch-up backfills the submission at ledger 50.
    const { catchup } = makeCatchup(db, [
      { ledger: 50, hash: 'tx-submit', events: submittedEvent(7, '1000000') },
    ]);
    await catchup.executeCatchup({ fromLedger: 1, toLedger: 200 });

    const invoice = db
      .prepare('SELECT status, funder, amount, token, amount_funded FROM invoices WHERE id = 7')
      .get();
    expect(invoice).toEqual({
      status: 'Funded',
      funder: 'GLP',
      amount: '1000000',
      token: 'USDC',
      amount_funded: '1000000',
    });
  });

  it('refuses to checkpoint a batch once a ledger divergence is flagged', async () => {
    const detector = new LedgerReorgDetector({
      db,
      source: createStaticLedgerHeaderSource([
        { sequence: 10, hash: 'h10', parentHash: 'h9' },
        { sequence: 11, hash: 'h11', parentHash: 'h10' },
      ]),
      logger: silentLogger,
      strictSequence: false,
    });
    // Stored history disagrees with the canonical header for ledger 11.
    storeLedgerHeader(db, { sequence: 11, hash: 'stale11', parentHash: 'h10' });
    const { catchup } = makeCatchup(db, submittedAt([10, 11]), { reorgDetector: detector });

    const error = await catchup.executeCatchup({ fromLedger: 10, toLedger: 12 }).catch((e) => e);

    expect(error).toBeInstanceOf(CatchupError);
    expect(error.kind).toBe('reorg');
    expect(checkpoint(db, 10, 12)).toBeNull();
    expect(eventHashes(db)).toEqual(['tx-10-0']);
  });

  it('rejects an inverted range', async () => {
    const { catchup } = makeCatchup(db, []);
    await expect(catchup.executeCatchup({ fromLedger: 20, toLedger: 10 })).rejects.toThrow(RangeError);
  });
});
