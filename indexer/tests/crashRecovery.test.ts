/**
 * Crash-recovery coverage for checkpoint replay (Issue #1164).
 *
 * Failures are injected only at persistence boundaries — a SQLite trigger
 * that aborts a real commit, or a repository write that throws — never by
 * bypassing production decoding or accounting. File-backed databases are
 * closed and reopened to model a process restart.
 */

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import Database from 'better-sqlite3';
import { initializeSchema } from '../src/db/schema.js';
import { createSqlEventRepository, type EventRepository } from '../src/db/eventRepository.js';
import type { DecodedContractEvent } from '../src/ingestion/eventListener.js';
import { ReplayIncompleteError, runReplay, type ReplayOptions } from '../src/ingestion/replay.js';
import {
  LedgerReorgDetector,
  latchHalt,
  readPendingReorg,
  storeLedgerHeader,
} from '../src/ingestion/reorgDetector.js';
import { createStaticLedgerHeaderSource } from '../src/ingestion/ledgerHeaders.js';
import { runReconciliation } from '../src/reconciliation/consistencyJob.js';
import type { ChainReader } from '../src/reconciliation/chainReader.js';
import {
  CONTRACT,
  fundedEvent,
  makeHorizonFixture,
  paidEvent,
  submittedEvent,
  toid,
  type FakeEventSpec,
} from './horizonFixture.js';

type DB = Database.Database;

const silentLogger = { info: () => undefined, warn: () => undefined, error: () => undefined };
const FAST_RETRY = { maxAttempts: 2, initialBackoffMs: 0, maxBackoffMs: 0 };

function openDb(file: string): DB {
  const db = new Database(file);
  db.pragma('journal_mode = WAL');
  db.pragma('foreign_keys = ON');
  initializeSchema(db);
  return db;
}

function count(db: DB, table: string): number {
  return (db.prepare(`SELECT COUNT(*) AS n FROM ${table}`).get() as { n: number }).n;
}

function hashes(db: DB): string[] {
  return (
    db.prepare('SELECT transaction_hash FROM events ORDER BY ledger, event_index').all() as Array<{
      transaction_hash: string;
    }>
  ).map((row) => row.transaction_hash);
}

function reputationEvent(address: string, oldScore: number, newScore: number): DecodedContractEvent {
  return {
    contractId: CONTRACT,
    rawEventType: 'reputation_updated',
    contractEventType: 'ReputationUpdated',
    topics: ['reputation_updated'],
    data: { address, old_score: oldScore, new_score: newScore, invoices_submitted: 1 },
  };
}

/** Invoice 1 lifecycle with a reputation update riding on the payment. */
const HISTORY: FakeEventSpec[] = [
  { ledger: 100, hash: 'tx-100', events: submittedEvent(1, '1000000') },
  { ledger: 101, hash: 'tx-101', events: fundedEvent(1, 'GLP', '1000000') },
  { ledger: 102, hash: 'tx-102', events: submittedEvent(2, '500000') },
  {
    ledger: 103,
    hash: 'tx-103',
    events: [...paidEvent(1, '970000'), reputationEvent('GFREELANCER', 50, 55)],
  },
  { ledger: 104, hash: 'tx-104', events: fundedEvent(2, 'GLP2', '500000') },
  { ledger: 105, hash: 'tx-105', events: [reputationEvent('GFREELANCER', 55, 60)] },
];

function replayOptions(
  repository: EventRepository,
  overrides: Partial<ReplayOptions> = {}
): ReplayOptions {
  const fixture = makeHorizonFixture(HISTORY, CONTRACT, { alwaysNext: true });
  return {
    repository,
    horizonUrl: 'https://horizon.example',
    contractAddress: CONTRACT,
    fetchImpl: fixture.fetchImpl,
    decodeTransactionEvents: fixture.decodeTransactionEvents,
    logger: silentLogger,
    retry: FAST_RETRY,
    pageSize: 2,
    fromLedger: 100,
    toLedger: 106,
    ...overrides,
  };
}

/** Canonical end state of HISTORY, captured from an uninterrupted replay. */
async function referenceState() {
  const db = openDb(':memory:');
  await runReplay(replayOptions(createSqlEventRepository(db)));
  const snapshot = {
    invoices: db.prepare('SELECT * FROM invoices ORDER BY id').all(),
    events: hashes(db),
    reputation: count(db, 'reputation_updates'),
  };
  db.close();
  return snapshot;
}

describe('replay crash recovery', () => {
  let dir: string;
  let file: string;
  let db: DB;

  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'iln-crash-'));
    file = path.join(dir, 'indexer.db');
    db = openDb(file);
  });

  afterEach(() => {
    if (db.open) db.close();
    fs.rmSync(dir, { recursive: true, force: true });
  });

  /** Close and reopen the database file, as a restarted process would. */
  function restart(): void {
    db.close();
    db = openDb(file);
  }

  it('a crash after processing a transaction but before its cursor is persisted leaves no orphan rows', async () => {
    const base = createSqlEventRepository(db);
    const crashing: EventRepository = {
      ...base,
      setState(key, value) {
        // Process dies while writing the cursor for tx-103.
        if (key === 'last_processed_cursor' && value === toid(103)) {
          throw new Error('process killed (SIGKILL) during cursor write');
        }
        base.setState(key, value);
      },
    };

    const error = await runReplay(replayOptions(crashing)).catch((e) => e);
    expect(error).toBeInstanceOf(ReplayIncompleteError);
    expect(error.resumeFromLedger).toBe(103);
    expect(error.partial.complete).toBe(false);

    restart();
    // tx-103's events and reputation row rolled back together with the cursor.
    expect(hashes(db)).toEqual(['tx-100', 'tx-101', 'tx-102']);
    expect(count(db, 'reputation_updates')).toBe(0);
    expect(
      (db.prepare('SELECT status FROM invoices WHERE id = 1').get() as { status: string }).status
    ).toBe('Funded');
    const repo = createSqlEventRepository(db);
    expect(repo.getState('last_processed_cursor')).toBe(toid(102));
    expect(repo.getState('last_processed_ledger')).toBe('102');

    // Restart from the persisted cursor ledger (re-covering it) to the end.
    const resumedFrom = Number(repo.getState('last_processed_ledger'));
    const result = await runReplay(replayOptions(repo, { fromLedger: resumedFrom }));
    expect(result.complete).toBe(true);

    const reference = await referenceState();
    expect(hashes(db)).toEqual(reference.events);
    expect(count(db, 'reputation_updates')).toBe(reference.reputation);
    expect(db.prepare('SELECT * FROM invoices ORDER BY id').all()).toEqual(reference.invoices);
  });

  it('a database commit failure stops the replay and nothing past it is marked processed', async () => {
    // A real SQLite failure inside the write transaction for tx-104.
    db.exec(`
      CREATE TRIGGER fail_commit BEFORE INSERT ON events
      WHEN NEW.transaction_hash = 'tx-104'
      BEGIN SELECT RAISE(ABORT, 'disk I/O error (simulated)'); END;
    `);

    const error = await runReplay(replayOptions(createSqlEventRepository(db))).catch((e) => e);

    expect(error).toBeInstanceOf(ReplayIncompleteError);
    expect(error.message).toContain('disk I/O error');
    expect(error.resumeFromLedger).toBe(104);
    expect(error.partial.complete).toBe(false);
    expect(error.partial.lastProcessedLedger).toBe(103);

    restart();
    expect(hashes(db)).toEqual(['tx-100', 'tx-101', 'tx-102', 'tx-103', 'tx-103']);
    // Invoice 2's funding (tx-104) never landed, and the cursor did not move past it.
    expect(
      (db.prepare('SELECT status FROM invoices WHERE id = 2').get() as { status: string }).status
    ).toBe('Pending');
    expect(createSqlEventRepository(db).getState('last_processed_ledger')).toBe('103');
  });

  it('a partial batch failure preserves committed data and resumes without omission', async () => {
    db.exec(`
      CREATE TRIGGER fail_once BEFORE INSERT ON reputation_updates
      WHEN NEW.transaction_hash = 'tx-105'
      BEGIN SELECT RAISE(ABORT, 'SQLITE_BUSY (simulated)'); END;
    `);

    const error = await runReplay(replayOptions(createSqlEventRepository(db))).catch((e) => e);
    expect(error.resumeFromLedger).toBe(105);
    expect(count(db, 'events')).toBe(6); // tx-100..tx-104 (tx-103 carries two)

    db.exec('DROP TRIGGER fail_once');
    restart();
    const result = await runReplay(
      replayOptions(createSqlEventRepository(db), { fromLedger: error.resumeFromLedger })
    );
    expect(result.complete).toBe(true);

    const reference = await referenceState();
    expect(hashes(db)).toEqual(reference.events);
    expect(count(db, 'reputation_updates')).toBe(reference.reputation);
    expect(db.prepare('SELECT * FROM invoices ORDER BY id').all()).toEqual(reference.invoices);
  });

  it('overlapping replay ranges never duplicate events or reputation updates', async () => {
    const repo = createSqlEventRepository(db);
    await runReplay(replayOptions(repo, { fromLedger: 100, toLedger: 104 }));
    await runReplay(replayOptions(repo, { fromLedger: 102, toLedger: 106 }));
    await runReplay(replayOptions(repo, { fromLedger: 100, toLedger: 106 }));

    const reference = await referenceState();
    expect(hashes(db)).toEqual(reference.events);
    expect(count(db, 'reputation_updates')).toBe(2);
    expect(db.prepare('SELECT * FROM invoices ORDER BY id').all()).toEqual(reference.invoices);
  });

  it('replaying an older window after newer state exists does not roll invoices back', async () => {
    const repo = createSqlEventRepository(db);
    await runReplay(replayOptions(repo));
    const reference = db.prepare('SELECT * FROM invoices ORDER BY id').all();

    // Only re-cover the submission ledgers; later funding/payment already stored.
    await runReplay(replayOptions(repo, { fromLedger: 100, toLedger: 101 }));
    await runReplay(replayOptions(repo, { fromLedger: 102, toLedger: 103 }));

    expect(db.prepare('SELECT * FROM invoices ORDER BY id').all()).toEqual(reference);
  });

  it('reconciliation flags an on-chain mismatch in replayed data instead of passing', async () => {
    await runReplay(replayOptions(createSqlEventRepository(db)));

    const chain = (statusFor: (id: number) => string): ChainReader => ({
      async getInvoice(id) {
        const row = db.prepare('SELECT * FROM invoices WHERE id = ?').get(id) as any;
        return {
          id,
          status: statusFor(id),
          amount: row.amount,
          amountFunded: row.amount_funded,
          amountPaid: row.amount_paid,
          funder: row.funder,
        };
      },
      async getInvoiceCount() {
        return count(db, 'invoices');
      },
    });
    const config = { intervalMs: 1_000, sampleSize: 10, tolerancePercent: 0 };

    const matching = await runReconciliation(
      db,
      chain((id) => (db.prepare('SELECT status FROM invoices WHERE id = ?').get(id) as any).status),
      config
    );
    expect(matching.healthy).toBe(true);

    // On-chain, invoice 2 was actually paid — the replayed index missed it.
    const drifted = await runReconciliation(
      db,
      chain((id) => (id === 2 ? 'Paid' : (db.prepare('SELECT status FROM invoices WHERE id = ?').get(id) as any).status)),
      config
    );
    expect(drifted.driftDetected).toBe(true);
    expect(drifted.healthy).toBe(false);
    expect(drifted.mismatches).toContainEqual({
      invoiceId: 2,
      field: 'status',
      indexedValue: 'funded',
      chainValue: 'paid',
    });

    // An unreachable chain is inconclusive, never a pass.
    const unreachable = await runReconciliation(
      db,
      {
        getInvoice: async () => {
          throw new Error('rpc down');
        },
        getInvoiceCount: async () => {
          throw new Error('rpc down');
        },
      },
      config
    );
    expect(unreachable.driftDetected).toBe(false);
    expect(unreachable.healthy).toBe(false);
    expect(unreachable.chainReadFailures).toBe(2);
    expect(unreachable.chainCountReadFailed).toBe(true);
  });

  it('a pending reorg stops replay from presenting stale data as canonical', async () => {
    const detector = new LedgerReorgDetector({
      db,
      source: createStaticLedgerHeaderSource([]),
      logger: silentLogger,
      strictSequence: false,
    });
    latchHalt(db, {
      divergenceLedger: 101,
      commonAncestorLedger: 100,
      forkDepth: 1,
      reason: 'ledger_hash_mismatch',
      detectedBy: 'consistency_job',
      message: 'test divergence',
    } as any);

    const result = await runReplay(
      replayOptions(createSqlEventRepository(db), { reorgDetector: detector })
    );

    expect(result.haltedByReorg).toBe(true);
    expect(result.complete).toBe(false);
    expect(result.pendingReorg?.divergenceLedger).toBe(101);
    expect(count(db, 'events')).toBe(0);
  });

  it('a divergence found mid-replay stops writing at the fork', async () => {
    const canonical = [
      { sequence: 100, hash: 'h100', parentHash: 'h99' },
      { sequence: 101, hash: 'h101', parentHash: 'h100' },
      { sequence: 102, hash: 'h102', parentHash: 'h101' },
      { sequence: 103, hash: 'h103', parentHash: 'h102' },
      { sequence: 104, hash: 'h104', parentHash: 'h103' },
      { sequence: 105, hash: 'h105', parentHash: 'h104' },
    ];
    storeLedgerHeader(db, { sequence: 99, hash: 'h99', parentHash: 'h98' });
    // Previously indexed ledger 102 is on a fork the chain abandoned.
    storeLedgerHeader(db, { sequence: 102, hash: 'stale102', parentHash: 'h101' });
    const detector = new LedgerReorgDetector({
      db,
      source: createStaticLedgerHeaderSource([
        { sequence: 99, hash: 'h99', parentHash: 'h98' },
        ...canonical,
      ]),
      logger: silentLogger,
      strictSequence: false,
    });

    const result = await runReplay(
      replayOptions(createSqlEventRepository(db), { reorgDetector: detector })
    );

    expect(result.haltedByReorg).toBe(true);
    expect(result.complete).toBe(false);
    expect(result.lastProcessedLedger).toBe(101);
    expect(readPendingReorg(db)?.divergenceLedger).toBe(102);
    expect(hashes(db)).toEqual(['tx-100', 'tx-101']);
  });
});
