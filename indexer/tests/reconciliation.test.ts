import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import Database from 'better-sqlite3';
import { initializeSchema } from '../src/db/schema.js';
import { createSqlEventRepository } from '../src/db/eventRepository.js';
import type { ChainReader, OnChainInvoice } from '../src/reconciliation/chainReader.js';
import {
  buildAlertPayload,
  buildReorgAlertPayload,
  createWebhookAlertDispatcher,
  detectReorgFromStoredHistory,
  runReconciliation,
  startReconciliationSchedule,
  DEFAULT_RECONCILIATION_CONFIG,
} from '../src/reconciliation/consistencyJob.js';
import {
  createStaticLedgerHeaderSource,
  type LedgerHeader,
  type LedgerHeaderSource,
} from '../src/ingestion/ledgerHeaders.js';
import {
  readPendingReorg,
  storeLedgerHeader,
  REORG_HALTED_KEY,
} from '../src/ingestion/reorgDetector.js';
import { recoverFromReorg } from '../src/ingestion/reorgRecovery.js';
import { runReplay } from '../src/ingestion/replay.js';
import {
  CONTRACT,
  fundedEvent,
  makeHorizonFixture,
  submittedEvent,
} from './horizonFixture.js';

type DB = Database.Database;

function createTestDb(): DB {
  const db = new Database(':memory:');
  db.pragma('journal_mode = WAL');
  db.pragma('foreign_keys = ON');
  initializeSchema(db);
  return db;
}

function chainInvoice(overrides: Partial<OnChainInvoice> & { id: number }): OnChainInvoice {
  return {
    status: 'Pending',
    amount: '1000000',
    amountFunded: '0',
    amountPaid: '0',
    funder: null,
    ...overrides,
  };
}

function seedInvoice(
  db: DB,
  id: number,
  overrides: Partial<{ status: string; amount: string; amount_funded: string; amount_paid: string; funder: string | null }> = {}
): void {
  const row = {
    status: 'Pending',
    amount: '1000000',
    amount_funded: '0',
    amount_paid: '0',
    funder: null as string | null,
    ...overrides,
  };
  db.prepare(
    `INSERT INTO invoices (id, freelancer, payer, token, amount, due_date, discount_rate, status,
       funder, funded_at, amount_funded, amount_paid, referral_code, submitter_reputation, created_at)
     VALUES (?, 'G-FREELANCER', 'G-PAYER', 'USDC', ?, ?, ?, ?, ?, NULL, ?, ?, NULL, 50, ?)`
  ).run(id, row.amount, Math.floor(Date.now() / 1000) + 86400, 500, row.status, row.funder, row.amount_funded, row.amount_paid, Math.floor(Date.now() / 1000));
}

/** Chain reader that answers from a mirror map — simulates perfect sync. */
function mirrorReader(db: DB): ChainReader {
  return {
    async getInvoice(id: number) {
      const row = db.prepare(`SELECT * FROM invoices WHERE id = ?`).get(id) as any;
      if (!row) return null;
      return chainInvoice({
        id,
        status: row.status,
        amount: String(row.amount),
        amountFunded: String(row.amount_funded),
        amountPaid: String(row.amount_paid),
        funder: row.funder,
      });
    },
    async getInvoiceCount() {
      return (db.prepare(`SELECT COUNT(*) AS n FROM invoices`).get() as { n: number }).n;
    },
  };
}

describe('indexer/chain consistency reconciliation', () => {
  let db: DB;

  beforeEach(() => {
    db = createTestDb();
  });

  afterEach(() => {
    db.close();
  });

  it('reports no drift when indexed data matches direct contract reads', async () => {
    seedInvoice(db, 1);
    seedInvoice(db, 2);

    const report = await runReconciliation(
      db,
      mirrorReader(db),
      { ...DEFAULT_RECONCILIATION_CONFIG, sampleSize: 2 }
    );

    expect(report.sampledInvoices).toBe(2);
    expect(report.mismatches).toHaveLength(0);
    expect(report.driftedInvoices).toBe(0);
    expect(report.countWithinTolerance).toBe(true);
    expect(report.driftDetected).toBe(false);
  });

  it('flags drift and dispatches an alert when indexed rows diverge beyond tolerance', async () => {
    for (let id = 1; id <= 4; id += 1) {
      seedInvoice(db, id);
    }

    // Chain truth: invoice 1 is Paid with different amounts; invoice 3 missing on-chain.
    const reader: ChainReader = {
      async getInvoice(id: number) {
        if (id === 1) {
          return chainInvoice({ id, status: 'Paid', amount: '111', amountPaid: '999' });
        }
        if (id === 3) {
          return null;
        }
        return mirrorReader(db).getInvoice(id);
      },
      getInvoiceCount: () => Promise.resolve(4),
    };

    const alertPayloads: any[] = [];
    const report = await runReconciliation(db, reader, {
      ...DEFAULT_RECONCILIATION_CONFIG,
      sampleSize: 4,
      tolerancePercent: 1,
    });
    expect(report.driftedInvoices).toBeGreaterThanOrEqual(1);
    expect(report.driftDetected).toBe(true);
    expect(report.mismatches.some((m) => m.field === 'status' && m.invoiceId === 1)).toBe(true);

    const alert = vi.fn(async () => undefined);
    await buildAlertPayload(report); // payload shape sanity
    const scheduler = startReconciliationSchedule(db, reader, { alert, config: { ...DEFAULT_RECONCILIATION_CONFIG, intervalMs: 50 } });
    await new Promise((r) => setTimeout(r, 150));
    scheduler.stop();

    // Scheduler ticks immediately on start; drift must have produced alerts.
    expect(alert).toHaveBeenCalled();
    const payload = alert.mock.calls[0][0];
    expect(payload.type).toBe('indexer_drift_detected');
    expect(payload.severity).toBe('critical');
    alertPayloads.push(payload);
  });

  it('tolerates small mismatches within the configured threshold without alerting', async () => {
    for (let id = 1; id <= 10; id += 1) {
      seedInvoice(db, id);
    }

    // Exactly one drifted invoice out of ten sampled (10% > default 1% would
    // alert; here we raise tolerance to prove the boundary).
    const reader: ChainReader = {
      async getInvoice(id: number) {
        if (id === 7) {
          return chainInvoice({ id, status: 'Funded' });
        }
        return mirrorReader(db).getInvoice(id);
      },
      getInvoiceCount: () => Promise.resolve(10),
    };

    const report = await runReconciliation(db, reader, {
      ...DEFAULT_RECONCILIATION_CONFIG,
      sampleSize: 10,
      tolerancePercent: 15,
    });

    expect(report.driftedInvoices).toBeLessThanOrEqual(1);
    expect(report.driftDetected).toBe(false);
  });

  it('alerts when the indexed invoice count lags the chain beyond tolerance', async () => {
    seedInvoice(db, 1);
    seedInvoice(db, 2);

    const reader: ChainReader = {
      getInvoice: (id) => mirrorReader(db).getInvoice(id),
      getInvoiceCount: () => Promise.resolve(500), // massive lag
    };

    const report = await runReconciliation(db, reader, {
      ...DEFAULT_RECONCILIATION_CONFIG,
      sampleSize: 5,
    });

    expect(report.chainInvoiceCount).toBe(500);
    expect(report.indexedInvoiceCount).toBe(2);
    expect(report.countWithinTolerance).toBe(false);
    expect(report.driftDetected).toBe(true);
  });

  it('excludes chain read errors from the drift rate but records them', async () => {
    seedInvoice(db, 1);
    seedInvoice(db, 2);

    const failingReader: ChainReader = {
      getInvoice: () => Promise.reject(new Error('RPC unreachable')),
      getInvoiceCount: () => Promise.reject(new Error('RPC unreachable')),
    };

    const report = await runReconciliation(db, failingReader, {
      ...DEFAULT_RECONCILIATION_CONFIG,
      sampleSize: 2,
    });

    expect(report.mismatches.every((m) => m.field === '__chain_read_error__')).toBe(true);
    expect(report.driftDetected).toBe(false);
  });

  it('webhook dispatcher POSTs the alert to the notifications service intake', async () => {
    const posted: Array<{ url: string; init: RequestInit }> = [];
    const fakeFetch = (async (url: any, init: any) => {
      posted.push({ url: String(url), init });
      return { ok: true, status: 200 } as Response;
    }) as unknown as typeof fetch;

    const dispatcher = createWebhookAlertDispatcher('http://notifications:3002/notify/indexer', fakeFetch);
    await dispatcher(buildAlertPayload({
      ranAt: new Date().toISOString(),
      sampledInvoices: 25,
      checkedFields: 100,
      driftedInvoices: 3,
      mismatches: [],
      indexedInvoiceCount: 120,
      chainInvoiceCount: 125,
      countWithinTolerance: false,
      driftDetected: true,
    }));

    expect(posted).toHaveLength(1);
    expect(posted[0].url).toBe('http://notifications:3002/notify/indexer');
    const body = JSON.parse(String(posted[0].init.body));
    expect(body.type).toBe('indexer_drift_detected');
    expect(body.severity).toBe('critical');
    expect(body.details.sampledInvoices).toBe(25);
  });
});

/**
 * Reorg backstop (Issue #866): indexed rows whose ledger hashes no longer
 * match the canonical chain, including a reorg that real-time detection
 * never saw (the detector's header read failed during ingestion).
 */
describe('consistency reconciliation reorg backstop', () => {
  let db: DB;

  beforeEach(() => {
    db = createTestDb();
  });

  afterEach(() => {
    db.close();
  });

  function chainAHeaders(): LedgerHeader[] {
    return [
      { sequence: 99, hash: 'h99', parentHash: 'h98' },
      { sequence: 100, hash: 'h100', parentHash: 'h99' },
      { sequence: 101, hash: 'a101', parentHash: 'h100' },
    ];
  }

  function chainBHeaders(): LedgerHeader[] {
    return [
      { sequence: 99, hash: 'h99', parentHash: 'h98' },
      { sequence: 100, hash: 'h100', parentHash: 'h99' },
      { sequence: 101, hash: 'b101', parentHash: 'h100' },
    ];
  }

  function chainAEvents() {
    return makeHorizonFixture([
      { ledger: 100, hash: 'tx-a100', events: submittedEvent(1, '1000000') },
      { ledger: 101, hash: 'tx-a101', events: fundedEvent(1, 'GLP-A', '1000000') },
    ]);
  }

  function chainBEvents() {
    return makeHorizonFixture([
      { ledger: 100, hash: 'tx-a100', events: submittedEvent(1, '1000000') },
      { ledger: 101, hash: 'tx-b101', events: fundedEvent(1, 'GLP-B', '1000000') },
    ]);
  }

  /** Index chain A and record its headers, as the live detector would. */
  async function seedIndexedChainA(): Promise<void> {
    const fixture = chainAEvents();
    await runReplay({
      repository: createSqlEventRepository(db),
      horizonUrl: 'https://horizon.example',
      contractAddress: CONTRACT,
      decodeTransactionEvents: fixture.decodeTransactionEvents,
      fetchImpl: fixture.fetchImpl,
      fromLedger: 99,
      toLedger: 102,
    });
    for (const header of chainAHeaders()) {
      storeLedgerHeader(db, header);
    }
  }

  function state(key: string): string | undefined {
    const row = db
      .prepare('SELECT state_value FROM indexer_state WHERE state_key = ?')
      .get(key) as { state_value: string } | undefined;
    return row?.state_value;
  }

  const config = { ...DEFAULT_RECONCILIATION_CONFIG, sampleSize: 5 };

  it('flags stored history that no longer matches the chain and halts ingestion', async () => {
    await seedIndexedChainA();

    const report = await runReconciliation(db, mirrorReader(db), config, {
      ledgerHeaders: createStaticLedgerHeaderSource(chainBHeaders()),
    });

    expect(report.reorgDivergence).not.toBeNull();
    expect(report.reorgDivergence?.divergenceLedger).toBe(101);
    expect(report.reorgDivergence?.commonAncestorLedger).toBe(100);
    expect(report.reorgDivergence?.reason).toBe('ledger_hash_mismatch');
    expect(report.reorgDivergence?.detectedBy).toBe('consistency_job');
    expect(report.reorgHeadersChecked).toBe(1);
    expect(report.reorgCheckErrors).toBe(0);
    // A reorg is drift at the root, even when every sampled invoice still
    // matches its own (now-stale) row.
    expect(report.driftDetected).toBe(true);

    // Halt latched before any recovery hook runs.
    expect(state(REORG_HALTED_KEY)).toBe('1');
    expect(readPendingReorg(db)?.divergenceLedger).toBe(101);

    const payload = buildReorgAlertPayload(report);
    expect(payload.type).toBe('indexer_reorg_detected');
    expect(payload.severity).toBe('critical');
    expect(payload.summary).toContain('ledger 101');
  });

  it('recovers a reorg that real-time detection missed through the shared rollback-and-replay path', async () => {
    await seedIndexedChainA();
    expect(
      (db.prepare('SELECT funder FROM invoices WHERE id = 1').get() as { funder: string }).funder
    ).toBe('GLP-A');

    const ledgerHeaders = createStaticLedgerHeaderSource(chainBHeaders());
    const fixture = chainBEvents();
    const alert = vi.fn(async () => undefined);

    const scheduler = startReconciliationSchedule(db, mirrorReader(db), {
      config: { ...config, intervalMs: 50 },
      alert,
      ledgerHeaders,
      onReorgDetected: (divergence) =>
        recoverFromReorg(divergence, {
          db,
          repository: createSqlEventRepository(db),
          horizonUrl: 'https://horizon.example',
          contractAddress: CONTRACT,
          source: ledgerHeaders,
          fetchImpl: fixture.fetchImpl,
          decodeTransactionEvents: fixture.decodeTransactionEvents,
        }),
    });

    await new Promise((resolve) => setTimeout(resolve, 300));
    scheduler.stop();

    // Alert fired for the reorg, and recovery cleared the halt.
    expect(alert).toHaveBeenCalled();
    expect(alert.mock.calls[0][0].type).toBe('indexer_reorg_detected');
    expect(state(REORG_HALTED_KEY)).toBeUndefined();
    expect(readPendingReorg(db)).toBeNull();

    // Indexed state now reflects the canonical chain.
    const invoice = db.prepare('SELECT status, funder FROM invoices WHERE id = 1').get() as {
      status: string;
      funder: string;
    };
    expect(invoice.status).toBe('Funded');
    expect(invoice.funder).toBe('GLP-B');
    expect(
      (db.prepare('SELECT hash FROM ledger_headers WHERE sequence = 101').get() as {
        hash: string;
      }).hash
    ).toBe('b101');

    // A follow-up run finds no reorg and no drift.
    const followUp = await runReconciliation(db, mirrorReader(db), config, { ledgerHeaders });
    expect(followUp.reorgDivergence).toBeNull();
    expect(followUp.reorgCheckErrors).toBe(0);
    expect(followUp.driftDetected).toBe(false);
  });

  it('leaves the divergence halt-latched when no recovery hook is wired', async () => {
    await seedIndexedChainA();

    const report = await runReconciliation(db, mirrorReader(db), config, {
      ledgerHeaders: createStaticLedgerHeaderSource(chainBHeaders()),
    });

    expect(report.reorgDivergence).not.toBeNull();
    expect(state(REORG_HALTED_KEY)).toBe('1');
    // Stays flagged for an operator (or another process wired with the hook).
    expect(readPendingReorg(db)?.divergenceLedger).toBe(101);
  });

  it('counts unreadable headers as infrastructure noise, never as a fork', async () => {
    await seedIndexedChainA();

    const broken: LedgerHeaderSource = {
      async getLedgerHeader(sequence: number): Promise<LedgerHeader> {
        throw new Error(`Horizon ledger ${sequence} read failed: HTTP 503`);
      },
    };

    const direct = await detectReorgFromStoredHistory(db, broken, config);
    expect(direct.divergence).toBeNull();
    expect(direct.checked).toBe(0);
    expect(direct.errors).toBe(3); // 99, 100, 101

    const report = await runReconciliation(db, mirrorReader(db), config, {
      ledgerHeaders: broken,
    });
    expect(report.reorgDivergence).toBeNull();
    expect(report.reorgCheckErrors).toBe(3);
    expect(report.driftDetected).toBe(false);
    expect(state(REORG_HALTED_KEY)).toBeUndefined();
  });

  it('skips the reorg check entirely when no header source is configured', async () => {
    await seedIndexedChainA();

    const report = await runReconciliation(db, mirrorReader(db), config);

    expect(report.reorgHeadersChecked).toBe(0);
    expect(report.reorgCheckErrors).toBe(0);
    expect(report.reorgDivergence).toBeNull();
    expect(report.driftDetected).toBe(false);
  });

  it('finds nothing when stored headers still match the canonical chain', async () => {
    await seedIndexedChainA();

    const report = await runReconciliation(db, mirrorReader(db), config, {
      ledgerHeaders: createStaticLedgerHeaderSource(chainAHeaders()),
    });

    expect(report.reorgDivergence).toBeNull();
    expect(report.reorgHeadersChecked).toBe(3);
    expect(state(REORG_HALTED_KEY)).toBeUndefined();
  });
});
