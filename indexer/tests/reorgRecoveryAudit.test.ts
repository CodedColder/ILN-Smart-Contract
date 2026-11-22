import { describe, expect, it } from 'vitest';
import Database from 'better-sqlite3';
import { initializeSchema } from '../src/db/schema.js';
import { createSqlEventRepository } from '../src/db/eventRepository.js';
import { recoverFromReorg } from '../src/ingestion/reorgRecovery.js';
import type { ReorgDivergence } from '../src/ingestion/reorgDetector.js';
import { storeLedgerHeader } from '../src/ingestion/reorgDetector.js';
import { createStaticLedgerHeaderSource } from '../src/ingestion/ledgerHeaders.js';

function createTestDb(): Database.Database {
  const db = new Database(':memory:');
  db.pragma('journal_mode = WAL');
  db.pragma('foreign_keys = ON');
  initializeSchema(db);
  return db;
}

describe('Reorg rollback coverage audit across all persisted tables (Issue #1240)', () => {
  it('rolls back events, reputation, insurance tables, invoices, and ledger headers', async () => {
    const db = createTestDb();
    const repo = createSqlEventRepository(db);

    // 1. Seed data before divergence point (ledger 100)
    db.prepare(`
      INSERT INTO invoices (id, freelancer, payer, token, amount, due_date, discount_rate, status, amount_funded, amount_paid, created_at)
      VALUES (1, 'FL1', 'PAYER1', 'USDC', '1000', 500, 100, 'Pending', '0', '0', 1000);
    `).run();

    db.prepare(`
      INSERT INTO events (id, invoice_id, event_type, ledger, timestamp, data, transaction_hash, event_index)
      VALUES (1, 1, 'InvoiceSubmitted', 100, 1000, '{}', 'tx-100', 0);
    `).run();

    db.prepare(`
      INSERT INTO reputation_updates (id, address, event_type, old_score, new_score, ledger, timestamp, transaction_hash, event_index)
      VALUES (1, 'FL1', 'reputation_updated', 0, 10, 100, 1000, 'tx-100', 1);
    `).run();

    db.prepare(`
      INSERT INTO insurance_pool_enrollments (lp_address, contract_id, enrolled_at, ledger, transaction_hash, event_index)
      VALUES ('LP1', 'C_CONTRACT', 1000, 100, 'tx-100', 2);
    `).run();

    db.prepare(`
      INSERT INTO insurance_pool_premiums (lp_address, contract_id, amount, premium_at, ledger, transaction_hash, event_index)
      VALUES ('LP1', 'C_CONTRACT', '50', 1000, 100, 'tx-100', 3);
    `).run();

    db.prepare(`
      INSERT INTO insurance_pool_claims (invoice_id, lp_address, contract_id, payout_amount, claimed_at, ledger, transaction_hash, event_index)
      VALUES (1, 'LP1', 'C_CONTRACT', '100', 1000, 100, 'tx-100', 4);
    `).run();

    storeLedgerHeader(db, { sequence: 100, hash: 'hash-100', parentHash: 'hash-99' }, 1000);

    // 2. Seed data on the diverged branch (ledger 101 and 102)
    db.prepare(`
      INSERT INTO invoices (id, freelancer, payer, token, amount, due_date, discount_rate, status, amount_funded, amount_paid, created_at)
      VALUES (2, 'FL2', 'PAYER2', 'USDC', '2000', 600, 100, 'Pending', '0', '0', 1010);
    `).run();

    db.prepare(`
      INSERT INTO events (id, invoice_id, event_type, ledger, timestamp, data, transaction_hash, event_index)
      VALUES 
        (2, 2, 'InvoiceSubmitted', 101, 1010, '{}', 'tx-101', 0),
        (3, 1, 'InvoiceFunded', 102, 1020, '{}', 'tx-102', 0);
    `).run();

    db.prepare(`
      INSERT INTO reputation_updates (id, address, event_type, old_score, new_score, ledger, timestamp, transaction_hash, event_index)
      VALUES (2, 'FL2', 'reputation_updated', 0, 20, 101, 1010, 'tx-101', 1);
    `).run();

    db.prepare(`
      INSERT INTO insurance_pool_enrollments (lp_address, contract_id, enrolled_at, ledger, transaction_hash, event_index)
      VALUES ('LP2', 'C_CONTRACT', 1010, 101, 'tx-101', 2);
    `).run();

    db.prepare(`
      INSERT INTO insurance_pool_premiums (lp_address, contract_id, amount, premium_at, ledger, transaction_hash, event_index)
      VALUES ('LP2', 'C_CONTRACT', '75', 1010, 101, 'tx-101', 3);
    `).run();

    db.prepare(`
      INSERT INTO insurance_pool_claims (invoice_id, lp_address, contract_id, payout_amount, claimed_at, ledger, transaction_hash, event_index)
      VALUES (2, 'LP2', 'C_CONTRACT', '200', 1010, 101, 'tx-101', 4);
    `).run();

    storeLedgerHeader(db, { sequence: 101, hash: 'stale-101', parentHash: 'hash-100' }, 1010);
    storeLedgerHeader(db, { sequence: 102, hash: 'stale-102', parentHash: 'stale-101' }, 1020);

    // Verify initial counts before reorg rollback
    expect((db.prepare('SELECT COUNT(*) as n FROM events').get() as any).n).toBe(3);
    expect((db.prepare('SELECT COUNT(*) as n FROM invoices').get() as any).n).toBe(2);
    expect((db.prepare('SELECT COUNT(*) as n FROM reputation_updates').get() as any).n).toBe(2);
    expect((db.prepare('SELECT COUNT(*) as n FROM insurance_pool_enrollments').get() as any).n).toBe(2);
    expect((db.prepare('SELECT COUNT(*) as n FROM insurance_pool_premiums').get() as any).n).toBe(2);
    expect((db.prepare('SELECT COUNT(*) as n FROM insurance_pool_claims').get() as any).n).toBe(2);
    expect((db.prepare('SELECT COUNT(*) as n FROM ledger_headers').get() as any).n).toBe(3);

    // Divergence detected at ledger 101 (common ancestor ledger 100)
    const divergence: ReorgDivergence = {
      divergenceLedger: 101,
      commonAncestorLedger: 100,
      reason: 'ledger_hash_mismatch',
      detectedBy: 'ingestion',
      detectedAt: new Date().toISOString(),
      indexedHash: 'stale-101',
      observedHash: 'canonical-101',
      observedParentHash: 'hash-100',
      forkDepth: 2,
      message: 'Reorg detected at ledger 101',
    };

    const source = createStaticLedgerHeaderSource([
      { sequence: 100, hash: 'hash-100', parentHash: 'hash-99' },
      { sequence: 101, hash: 'canonical-101', parentHash: 'hash-100' },
    ]);

    const result = await recoverFromReorg(divergence, {
      db,
      repository: repo,
      horizonUrl: 'http://horizon.local',
      contractAddress: 'C_CONTRACT',
      source,
      fetchImpl: async () =>
        new Response(
          JSON.stringify({
            _embedded: { records: [] },
            _links: { next: { href: '' } },
          }),
          { status: 200, headers: { 'Content-Type': 'application/json' } }
        ),
    });

    // Check rollback counts
    expect(result.deletedEvents).toBe(2);
    expect(result.deletedReputationUpdates).toBe(1);
    expect(result.deletedInvoices).toBe(1); // invoice 2 was introduced in ledger 101 and had no prior events
    expect(result.deletedLedgerHeaders).toBe(2);
    expect(result.deletedInsuranceEnrollments).toBe(1);
    expect(result.deletedInsurancePremiums).toBe(1);
    expect(result.deletedInsuranceClaims).toBe(1);

    // Verify state after rollback: exactly ledger 100 data survives
    expect((db.prepare('SELECT COUNT(*) as n FROM events').get() as any).n).toBe(1);
    expect((db.prepare('SELECT COUNT(*) as n FROM reputation_updates').get() as any).n).toBe(1);
    expect((db.prepare('SELECT COUNT(*) as n FROM insurance_pool_enrollments').get() as any).n).toBe(1);
    expect((db.prepare('SELECT COUNT(*) as n FROM insurance_pool_premiums').get() as any).n).toBe(1);
    expect((db.prepare('SELECT COUNT(*) as n FROM insurance_pool_claims').get() as any).n).toBe(1);
    expect((db.prepare('SELECT COUNT(*) as n FROM ledger_headers').get() as any).n).toBe(1);

    // Invoice 1 survived because it has an event in ledger 100; Invoice 2 was completely deleted
    const survivingInvoices = db.prepare('SELECT id FROM invoices').all() as any[];
    expect(survivingInvoices.map((inv) => inv.id)).toEqual([1]);

    // Check last processed ledger was rolled back to parent ledger (100)
    expect(repo.getState('last_processed_ledger')).toBe('100');
  });
});
