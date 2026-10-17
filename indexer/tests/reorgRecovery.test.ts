/**
 * Automatic rollback-and-replay recovery on a detected reorg (Issue #864).
 */

import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import Database from 'better-sqlite3';
import { initializeSchema } from '../src/db/schema.js';
import { createSqlEventRepository } from '../src/db/eventRepository.js';
import {
  findDivergence,
  getStoredHeader,
  readPendingReorg,
  storeLedgerHeader,
  REORG_HALTED_KEY,
} from '../src/ingestion/reorgDetector.js';
import { recoverFromReorg } from '../src/ingestion/reorgRecovery.js';
import { runReplay } from '../src/ingestion/replay.js';
import { createIngestionLock } from '../src/ingestion/ingestionLock.js';
import { createStaticLedgerHeaderSource } from '../src/ingestion/ledgerHeaders.js';
import type { LedgerHeader } from '../src/ingestion/ledgerHeaders.js';
import type { ReorgDivergence } from '../src/ingestion/reorgDetector.js';
import {
  CONTRACT,
  fundedEvent,
  makeHorizonFixture,
  paidEvent,
  submittedEvent,
} from './horizonFixture.js';

type DB = Database.Database;

const silentLogger = {
  info: () => undefined,
  warn: () => undefined,
  error: () => undefined,
};

function createTestDb(): DB {
  const db = new Database(':memory:');
  db.pragma('journal_mode = WAL');
  db.pragma('foreign_keys = ON');
  initializeSchema(db);
  return db;
}

function state(db: DB, key: string): string | undefined {
  const row = db
    .prepare('SELECT state_value FROM indexer_state WHERE state_key = ?')
    .get(key) as { state_value: string } | undefined;
  return row?.state_value;
}

function countEvents(db: DB): number {
  return (db.prepare('SELECT COUNT(*) AS n FROM events').get() as { n: number }).n;
}

/** Indexed (now invalid) history: common ancestor at ledger 100. */
function indexedHeaders(): LedgerHeader[] {
  return [
    { sequence: 99, hash: 'h99', parentHash: 'h98' },
    { sequence: 100, hash: 'h100', parentHash: 'h99' },
    { sequence: 101, hash: 'a101', parentHash: 'h100' },
    { sequence: 102, hash: 'a102', parentHash: 'a101' },
  ];
}

/** Canonical chain after the reorg (same prefix, new 101+). */
function canonicalHeaders(overrides: Partial<Record<number, Partial<LedgerHeader>>> = {}): LedgerHeader[] {
  return [
    { sequence: 99, hash: 'h99', parentHash: 'h98' },
    { sequence: 100, hash: 'h100', parentHash: 'h99' },
    { sequence: 101, hash: 'b101', parentHash: 'h100' },
    { sequence: 102, hash: 'b102', parentHash: 'b101' },
  ].map((header) => ({ ...header, ...overrides[header.sequence] }));
}

function chainAEventsFixture() {
  return makeHorizonFixture([
    { ledger: 100, hash: 'tx-a100', events: submittedEvent(1, '1000000') },
    { ledger: 101, hash: 'tx-a101', events: fundedEvent(1, 'GLP-A', '1000000') },
    { ledger: 102, hash: 'tx-a102', events: paidEvent(1, '970000') },
  ]);
}

function chainBEventsFixture() {
  return makeHorizonFixture([
    { ledger: 100, hash: 'tx-a100', events: submittedEvent(1, '1000000') },
    { ledger: 101, hash: 'tx-b101', events: submittedEvent(1, '1000000') },
    { ledger: 102, hash: 'tx-b102', events: fundedEvent(1, 'GLP-B', '1000000') },
  ]);
}

async function seedIndexedChainA(db: DB): Promise<void> {
  const fixture = chainAEventsFixture();
  await runReplay({
    repository: createSqlEventRepository(db),
    horizonUrl: 'https://horizon.example',
    contractAddress: CONTRACT,
    decodeTransactionEvents: fixture.decodeTransactionEvents,
    fetchImpl: fixture.fetchImpl,
    fromLedger: 99,
    toLedger: 103,
  });
  for (const header of indexedHeaders()) {
    storeLedgerHeader(db, header);
  }
}

async function divergenceAt101(db: DB): Promise<ReorgDivergence> {
  return findDivergence({
    db,
    source: createStaticLedgerHeaderSource(canonicalHeaders()),
    staleSequence: 102,
    reason: 'ledger_hash_mismatch',
    detectedBy: 'ingestion',
  });
}

describe('indexer reorg rollback-and-replay recovery', () => {
  let db: DB;

  beforeEach(() => {
    db = createTestDb();
  });

  afterEach(() => {
    db.close();
  });

  it('rolls back to the divergence parent and rebuilds state on the canonical chain', async () => {
    await seedIndexedChainA(db);
    expect(countEvents(db)).toBe(3);

    const divergence = await divergenceAt101(db);
    const fixture = chainBEventsFixture();

    const result = await recoverFromReorg(divergence, {
      db,
      repository: createSqlEventRepository(db),
      horizonUrl: 'https://horizon.example',
      contractAddress: CONTRACT,
      source: createStaticLedgerHeaderSource(canonicalHeaders()),
      fetchImpl: fixture.fetchImpl,
      decodeTransactionEvents: fixture.decodeTransactionEvents,
      logger: silentLogger,
    });

    // Rolled back everything at/after the divergence ledger.
    expect(result.parentLedger).toBe(100);
    expect(result.deletedEvents).toBe(2); // tx-a101, tx-a102
    expect(result.deletedLedgerHeaders).toBe(2);
    expect(result.deletedInvoices).toBe(0); // invoice 1 still has its ledger-100 event

    // Rebuilt forward from the divergence ledger with chain B's transactions.
    expect(result.replay?.fromLedger).toBe(101);
    expect(result.replay?.transactionsProcessed).toBe(2);
    expect(result.replay?.failedTransactions).toBe(0);
    expect(countEvents(db)).toBe(3);
    const invoice = db.prepare('SELECT * FROM invoices WHERE id = 1').get() as {
      status: string;
      funder: string;
      amount_funded: string;
    };
    expect(invoice.status).toBe('Funded');
    expect(invoice.funder).toBe('GLP-B');
    expect(invoice.amount_funded).toBe('1000000');

    // Stored history now matches the canonical chain.
    expect(getStoredHeader(db, 100)?.hash).toBe('h100');
    expect(getStoredHeader(db, 101)?.hash).toBe('b101');
    expect(getStoredHeader(db, 102)?.hash).toBe('b102');
    expect((db.prepare('SELECT COUNT(*) AS n FROM ledger_headers').get() as { n: number }).n).toBe(4);

    // Checkpoints moved with the rebuild; ingestion resumes.
    expect(state(db, 'last_processed_ledger')).toBe('102');
    expect(state(db, REORG_HALTED_KEY)).toBeUndefined();
    expect(readPendingReorg(db)).toBeNull();
    expect(result.reDetected).toBe(false);
  });

  it('stays halted when replay cannot run, so ingestion never resumes on a rolled-back database', async () => {
    await seedIndexedChainA(db);
    const divergence = await divergenceAt101(db);

    const failingFetch = (async () => {
      throw new Error('Horizon unreachable');
    }) as unknown as typeof fetch;

    await expect(
      recoverFromReorg(divergence, {
        db,
        repository: createSqlEventRepository(db),
        horizonUrl: 'https://horizon.example',
        contractAddress: CONTRACT,
        source: createStaticLedgerHeaderSource(canonicalHeaders()),
        fetchImpl: failingFetch,
        logger: silentLogger,
      })
    ).rejects.toThrow('Horizon unreachable');

    // Fail closed: rollback happened, replay did not, and the halt latch plus
    // the divergence record both stay for the next attempt.
    expect(countEvents(db)).toBe(1);
    expect(state(db, REORG_HALTED_KEY)).toBe('1');
    expect(readPendingReorg(db)?.divergenceLedger).toBe(101);
    expect(state(db, 'last_processed_ledger')).toBe('100');
  });

  it('keeps ingestion halted when a second divergence appears during replay', async () => {
    await seedIndexedChainA(db);
    const divergence = await divergenceAt101(db);

    // Chain truth changes again mid-recovery: canonical 101 no longer links
    // to the (unchanged) ledger 100, so replay's own detector re-latches.
    const source = createStaticLedgerHeaderSource(
      canonicalHeaders({ 101: { parentHash: 'foreign-parent' } })
    );
    const fixture = chainBEventsFixture();

    const result = await recoverFromReorg(divergence, {
      db,
      repository: createSqlEventRepository(db),
      horizonUrl: 'https://horizon.example',
      contractAddress: CONTRACT,
      source,
      fetchImpl: fixture.fetchImpl,
      decodeTransactionEvents: fixture.decodeTransactionEvents,
      logger: silentLogger,
    });

    expect(result.reDetected).toBe(true);
    expect(result.pendingReorg?.reason).toBe('parent_hash_mismatch');
    expect(state(db, REORG_HALTED_KEY)).toBe('1');
  });

  it('runs directly when this process already holds the ingestion lease', async () => {
    await seedIndexedChainA(db);
    const divergence = await divergenceAt101(db);
    const lock = createIngestionLock({ db, instanceId: 'leader' });
    expect(lock.tryAcquire()).toBe(true);

    const fixture = chainBEventsFixture();
    const result = await recoverFromReorg(divergence, {
      db,
      repository: createSqlEventRepository(db),
      horizonUrl: 'https://horizon.example',
      contractAddress: CONTRACT,
      source: createStaticLedgerHeaderSource(canonicalHeaders()),
      fetchImpl: fixture.fetchImpl,
      decodeTransactionEvents: fixture.decodeTransactionEvents,
      lock,
      logger: silentLogger,
    });

    expect(result.reDetected).toBe(false);
    // Nested acquisition must not release the lease the ingestion loop holds.
    expect(lock.isLeader()).toBe(true);
  });

  it('refuses to roll back while another process holds the ingestion lease', async () => {
    await seedIndexedChainA(db);
    const divergence = await divergenceAt101(db);

    const holder = createIngestionLock({ db, instanceId: 'holder' });
    const contender = createIngestionLock({ db, instanceId: 'contender' });
    expect(holder.tryAcquire()).toBe(true);

    await expect(
      recoverFromReorg(divergence, {
        db,
        repository: createSqlEventRepository(db),
        horizonUrl: 'https://horizon.example',
        contractAddress: CONTRACT,
        source: createStaticLedgerHeaderSource(canonicalHeaders()),
        lock: contender,
        acquireTimeoutMs: 100,
        logger: silentLogger,
      })
    ).rejects.toThrow(/timed out after 100ms/);

    // Nothing was touched: no halt, no rollback, no divergence record.
    expect(state(db, REORG_HALTED_KEY)).toBeUndefined();
    expect(readPendingReorg(db)).toBeNull();
    expect(countEvents(db)).toBe(3);
    expect(state(db, 'last_processed_ledger')).toBe('102');
  });

  it('serializes concurrent recoveries instead of interleaving their rollbacks', async () => {
    await seedIndexedChainA(db);
    const first = await divergenceAt101(db);
    const second: ReorgDivergence = { ...first, divergenceLedger: 102, commonAncestorLedger: 101 };

    const rollbackOrder: string[] = [];
    const logger = {
      info: () => undefined,
      warn: (message: string) => rollbackOrder.push(message),
      error: () => undefined,
    };
    const fixture = chainBEventsFixture();
    const options = {
      db,
      repository: createSqlEventRepository(db),
      horizonUrl: 'https://horizon.example',
      contractAddress: CONTRACT,
      source: createStaticLedgerHeaderSource(canonicalHeaders()),
      fetchImpl: fixture.fetchImpl,
      decodeTransactionEvents: fixture.decodeTransactionEvents,
      logger,
    };

    await Promise.all([
      recoverFromReorg(first, options),
      recoverFromReorg(second, options),
    ]);

    expect(rollbackOrder).toHaveLength(2);
    expect(rollbackOrder[0]).toContain('rolled back to ledger 100');
    expect(rollbackOrder[1]).toContain('rolled back to ledger 101');
    expect(state(db, REORG_HALTED_KEY)).toBeUndefined();
  });
});
