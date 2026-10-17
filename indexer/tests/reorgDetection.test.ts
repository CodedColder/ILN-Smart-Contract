/**
 * Ledger reorg detection in the ingestion path (Issue #863).
 */

import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import Database from 'better-sqlite3';
import { initializeSchema } from '../src/db/schema.js';
import {
  LedgerReorgDetector,
  findDivergence,
  readPendingReorg,
  storeLedgerHeader,
  REORG_HALTED_KEY,
} from '../src/ingestion/reorgDetector.js';
import {
  createStaticLedgerHeaderSource,
  type LedgerHeader,
  type LedgerHeaderSource,
} from '../src/ingestion/ledgerHeaders.js';

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

/** Indexed (pre-reorg) history: fork point at ledger 100. */
function indexedChain(): LedgerHeader[] {
  return [
    { sequence: 99, hash: 'h99', parentHash: 'h98' },
    { sequence: 100, hash: 'h100', parentHash: 'h99' },
    { sequence: 101, hash: 'a101', parentHash: 'h100' },
    { sequence: 102, hash: 'a102', parentHash: 'a101' },
  ];
}

/** Canonical chain after the reorg: same prefix, different 101+. */
function canonicalChain(): LedgerHeader[] {
  return [
    { sequence: 99, hash: 'h99', parentHash: 'h98' },
    { sequence: 100, hash: 'h100', parentHash: 'h99' },
    { sequence: 101, hash: 'b101', parentHash: 'h100' },
    { sequence: 102, hash: 'b102', parentHash: 'b101' },
  ];
}

function seedStoredHistory(db: DB, headers: LedgerHeader[]): void {
  for (const header of headers) {
    storeLedgerHeader(db, header);
  }
}

function trackingSource(inner: LedgerHeaderSource) {
  const calls: number[] = [];
  return {
    calls,
    source: {
      async getLedgerHeader(sequence: number) {
        calls.push(sequence);
        return inner.getLedgerHeader(sequence);
      },
    } satisfies LedgerHeaderSource,
  };
}

describe('indexer ledger reorg detection', () => {
  let db: DB;

  beforeEach(() => {
    db = createTestDb();
  });

  afterEach(() => {
    db.close();
  });

  it('accepts a ledger that chains onto stored history and records its header', async () => {
    seedStoredHistory(db, indexedChain());
    const source = createStaticLedgerHeaderSource(indexedChain());
    const detector = new LedgerReorgDetector({ db, source, logger: silentLogger });

    // Chain A's own next ledger: hash matches stored, parent links to 101.
    const divergence = await detector.observe(101);

    expect(divergence).toBeNull();
    expect(state(db, REORG_HALTED_KEY)).toBeUndefined();
    expect(detector.getStoredHeader(101)?.hash).toBe('a101');
  });

  it('flags a ledger whose hash no longer matches the chain and halts ingestion', async () => {
    seedStoredHistory(db, indexedChain());
    const source = createStaticLedgerHeaderSource(canonicalChain());
    const detector = new LedgerReorgDetector({ db, source, logger: silentLogger });

    // The chain reorged 101+, and a transaction from the *new* chain arrives.
    const divergence = await detector.observe(102);

    expect(divergence).not.toBeNull();
    expect(divergence?.reason).toBe('ledger_hash_mismatch');
    expect(divergence?.detectedBy).toBe('ingestion');
    // Walk-back: 102 stale, 101 stale, 100 still common.
    expect(divergence?.divergenceLedger).toBe(101);
    expect(divergence?.commonAncestorLedger).toBe(100);
    expect(divergence?.indexedHash).toBe('a101');
    expect(divergence?.observedHash).toBe('b101');
    expect(divergence?.forkDepth).toBe(2);
    expect(divergence?.message).toContain('ReorgDetected');

    // Halt latched in the shared DB + divergence persisted for recovery.
    expect(state(db, REORG_HALTED_KEY)).toBe('1');
    expect(readPendingReorg(db)?.divergenceLedger).toBe(101);
    expect(await detector.observe(102)).toBeNull();
  });

  it('flags a parent-hash break as the classic fork signature', async () => {
    seedStoredHistory(db, indexedChain());
    const source = createStaticLedgerHeaderSource(canonicalChain());
    const detector = new LedgerReorgDetector({ db, source, logger: silentLogger });

    // Stored history stops at 101 (chain A); ledger 102 arrives from chain B,
    // whose parent (b101) is not the stored 101 we think we have.
    db.prepare('DELETE FROM ledger_headers WHERE sequence = 102').run();
    const divergence = await detector.observe(102);

    expect(divergence?.reason).toBe('parent_hash_mismatch');
    expect(divergence?.observedParentHash).toBe('b101');
    expect(divergence?.divergenceLedger).toBe(101);
    expect(state(db, REORG_HALTED_KEY)).toBe('1');
  });

  it('does not flag a checkpoint replay of an older ledger that still links forward', async () => {
    seedStoredHistory(db, indexedChain().filter((h) => h.sequence !== 100));
    const source = createStaticLedgerHeaderSource(indexedChain());
    const detector = new LedgerReorgDetector({ db, source, logger: silentLogger });

    // Ledger 100 was never stored, but stored 101 links back to it: a normal
    // replay walk backwards, not a chain break.
    const divergence = await detector.observe(100);

    expect(divergence).toBeNull();
    expect(state(db, REORG_HALTED_KEY)).toBeUndefined();
    expect(detector.getStoredHeader(100)?.hash).toBe('h100');
  });

  it('flags sequence regression when the stored successor does not chain to it', async () => {
    seedStoredHistory(db, indexedChain().filter((h) => h.sequence !== 100));
    const source = createStaticLedgerHeaderSource(canonicalChain());
    const detector = new LedgerReorgDetector({ db, source, logger: silentLogger });

    // 100 is missing locally, but stored 101 claims parent a101's chain —
    // canonical 100 (h100) does not link stored 101 to it.
    db.prepare(`UPDATE ledger_headers SET parent_hash = 'stale-parent' WHERE sequence = 101`).run();
    const divergence = await detector.observe(100);

    expect(divergence?.reason).toBe('sequence_regression');
    expect(state(db, REORG_HALTED_KEY)).toBe('1');
  });

  it('treats an unreadable header as unverifiable instead of a fork', async () => {
    seedStoredHistory(db, indexedChain());
    const source = {
      async getLedgerHeader(): Promise<LedgerHeader> {
        throw new Error('Horizon ledger read failed: HTTP 503');
      },
    };

    const detector = new LedgerReorgDetector({ db, source, logger: silentLogger });
    const divergence = await detector.observe(102);

    expect(divergence).toBeNull();
    expect(state(db, REORG_HALTED_KEY)).toBeUndefined();
    expect(readPendingReorg(db)).toBeNull();
  });

  it('shares the halt across detector instances through the database', async () => {
    seedStoredHistory(db, indexedChain());
    const source = createStaticLedgerHeaderSource(canonicalChain());

    const first = new LedgerReorgDetector({ db, source, logger: silentLogger });
    await first.observe(102);
    expect(first.isHalted()).toBe(true);

    // A second process (new instance, same DB) must also stop ingesting.
    const second = new LedgerReorgDetector({ db, source, logger: silentLogger });
    expect(second.isHalted()).toBe(true);
    expect(second.pendingReorg()?.divergenceLedger).toBe(101);

    second.clear();
    expect(second.isHalted()).toBe(false);
    expect(readPendingReorg(db)).toBeNull();
  });

  it('bounds the fork-point walk so a fully-diverged history cannot replay genesis', async () => {
    // 30 stored headers, every one of them stale.
    const stored: LedgerHeader[] = Array.from({ length: 30 }, (_, i) => ({
      sequence: 100 + i,
      hash: `old-${100 + i}`,
      parentHash: `old-${99 + i}`,
    }));
    const canonical: LedgerHeader[] = Array.from({ length: 30 }, (_, i) => ({
      sequence: 100 + i,
      hash: `new-${100 + i}`,
      parentHash: `new-${99 + i}`,
    }));
    seedStoredHistory(db, stored);

    const tracked = trackingSource(createStaticLedgerHeaderSource(canonical));
    const divergence = await findDivergence({
      db,
      source: tracked.source,
      staleSequence: 129,
      reason: 'ledger_hash_mismatch',
      detectedBy: 'ingestion',
      forkMaxDepth: 3,
    });

    // Only the bounded window was consulted (129, 128, 127), and the
    // unverified ledger below it becomes the (conservative) divergence.
    expect(tracked.calls).toEqual([129, 128, 127]);
    expect(divergence.forkDepth).toBe(3);
    expect(divergence.divergenceLedger).toBe(127);
    expect(divergence.commonAncestorLedger).toBe(126);
  });

  it('skips stored gaps instead of giving up when history is sparse', async () => {
    // Only ledgers with events carry headers: 100 and 102 (101 empty).
    seedStoredHistory(db, [
      { sequence: 100, hash: 'h100', parentHash: 'h99' },
      { sequence: 102, hash: 'a102', parentHash: 'a101' },
    ]);
    const source = createStaticLedgerHeaderSource([
      { sequence: 100, hash: 'h100', parentHash: 'h99' },
      { sequence: 102, hash: 'b102', parentHash: 'b101' },
    ]);

    const divergence = await findDivergence({
      db,
      source,
      staleSequence: 102,
      reason: 'ledger_hash_mismatch',
      detectedBy: 'ingestion',
    });

    // 102 is stale, 101 has no stored header (skipped), 100 still matches —
    // so 101 is the first ledger that can no longer be proven valid, and it
    // is rolled back too (idempotent replay makes over-rolling safe).
    expect(divergence.divergenceLedger).toBe(101);
    expect(divergence.commonAncestorLedger).toBe(100);
  });
});
