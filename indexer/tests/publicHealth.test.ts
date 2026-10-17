import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import request from 'supertest';
import { createTestApp, createTestDb, seedInvoice } from './helpers.js';
import { buildPublicHealth } from '../src/services/publicHealthService.js';
import type { ProtocolStatusSnapshot, ProtocolStatusService } from '../src/services/protocolStatusService.js';
import type { OnChainProtocolStatus } from '../src/reconciliation/chainReader.js';

const NOW_MS = 1_800_000_000_000;
const NOW_SEC = NOW_MS / 1000;
const DAY = 86_400;

const chainStatus = (over: Partial<OnChainProtocolStatus> = {}): OnChainProtocolStatus => ({
  paused: false,
  lastPauseTimestamp: 1234,
  admin: 'GADMINSECRETADDRESS',
  multisigConfigured: true,
  multisigThreshold: 2,
  multisigSignerCount: 3,
  oracleCircuitTripped: false,
  oracleCircuitsTripped: 0,
  ...over,
});

const snapshot = (
  status: OnChainProtocolStatus | null,
  over: Partial<ProtocolStatusSnapshot> = {},
): ProtocolStatusSnapshot => ({
  status,
  fetchedAt: status ? new Date(NOW_MS).toISOString() : null,
  stale: false,
  source: status ? 'chain' : 'unavailable',
  ...over,
});

function seedMany(db: ReturnType<typeof createTestDb>, status: string, count: number, startId: number, due?: number) {
  for (let i = 0; i < count; i++) {
    seedInvoice(db, { id: startId + i, status, due_date: due ?? NOW_SEC + 30 * DAY });
  }
}

describe('buildPublicHealth', () => {
  let db: ReturnType<typeof createTestDb>;
  beforeEach(() => {
    db = createTestDb();
  });
  afterEach(() => db.close());

  const build = (status: ProtocolStatusSnapshot, extra = {}) =>
    buildPublicHealth(db, { protocolStatus: status, now: () => NOW_MS, ...extra });

  it('reports operational with unknown solvency on an empty index', () => {
    const h = build(snapshot(chainStatus()));
    expect(h.overall).toBe('operational');
    expect(h.solvency.level).toBe('unknown');
    expect(h.solvency.defaultRatePct).toBeNull();
    expect(h.solvency.overdueSharePct).toBeNull();
    expect(h.solvency.insurancePool).toBeNull();
    expect(h.oracle).toEqual({ circuitsTripped: 0, healthy: true });
    expect(h.generatedAt).toBe(new Date(NOW_MS).toISOString());
  });

  it('reports paused when the protocol is paused', () => {
    const h = build(snapshot(chainStatus({ paused: true })));
    expect(h.overall).toBe('paused');
    expect(h.reasons.join(' ')).toMatch(/paused/i);
  });

  it('reports unknown when the chain status is unavailable', () => {
    const h = build(snapshot(null));
    expect(h.overall).toBe('unknown');
    expect(h.protocol.paused).toBeNull();
    expect(h.oracle).toEqual({ circuitsTripped: null, healthy: null });
  });

  it('reports degraded when an oracle circuit is tripped', () => {
    const h = build(snapshot(chainStatus({ oracleCircuitTripped: true, oracleCircuitsTripped: 2 })));
    expect(h.overall).toBe('degraded');
    expect(h.oracle).toEqual({ circuitsTripped: 2, healthy: false });
  });

  it('notes stale chain data without hiding the value', () => {
    const h = build(snapshot(chainStatus(), { stale: true, source: 'cache' }));
    expect(h.overall).toBe('operational');
    expect(h.protocol.stale).toBe(true);
    expect(h.reasons.join(' ')).toMatch(/earlier read/i);
  });

  it('does not report a default rate below the minimum sample', () => {
    seedMany(db, 'Paid', 5, 1);
    seedMany(db, 'Defaulted', 4, 100);
    const h = build(snapshot(chainStatus()));
    expect(h.solvency.defaultRatePct).toBeNull();
    expect(h.solvency.level).toBe('unknown');
  });

  it('computes default rate and marks healthy / watch / stressed', () => {
    seedMany(db, 'Paid', 97, 1);
    seedMany(db, 'Defaulted', 3, 200); // 3% -> watch
    let h = build(snapshot(chainStatus()));
    expect(h.solvency.defaultRatePct).toBe(3);
    expect(h.solvency.level).toBe('watch');
    expect(h.overall).toBe('operational');

    seedMany(db, 'Defaulted', 6, 300); // 9/106 = 8.5% -> stressed
    h = build(snapshot(chainStatus()));
    expect(h.solvency.defaultRatePct).toBe(8.5);
    expect(h.solvency.level).toBe('stressed');
    expect(h.overall).toBe('degraded');
  });

  it('computes overdue share from funded invoices past their due date only', () => {
    seedMany(db, 'Funded', 3, 1, NOW_SEC - DAY); // overdue
    seedMany(db, 'Funded', 7, 50, NOW_SEC + DAY); // not yet due
    seedMany(db, 'Pending', 10, 100, NOW_SEC - DAY); // not funded: ignored
    const h = build(snapshot(chainStatus()));
    expect(h.solvency.outstandingFundedInvoices).toBe(10);
    expect(h.solvency.overdueFundedInvoices).toBe(3);
    expect(h.solvency.overdueSharePct).toBe(30);
    expect(h.solvency.level).toBe('stressed');
  });

  it('summarises the insurance pool as a unit-free share and no raw amounts', () => {
    db.prepare(
      `INSERT INTO insurance_pool_stats (contract_id, pool_balance, total_premiums_collected, total_claims_paid, enrolled_lp_count, last_updated_at)
       VALUES ('CPOOL', '7500000000', '9000000000', '2500000000', 12, 1)`,
    ).run();
    const h = build(snapshot(chainStatus()));
    expect(h.solvency.insurancePool).toEqual({ enrolledLps: 12, claimsSharePct: 25 });
    expect(h.solvency.level).toBe('watch');
    expect(JSON.stringify(h)).not.toContain('7500000000');
  });

  it('honours thresholds overrides and a specific pool id', () => {
    db.prepare(
      `INSERT INTO insurance_pool_stats (contract_id, pool_balance, total_premiums_collected, total_claims_paid, enrolled_lp_count, last_updated_at)
       VALUES ('CA', '100', '0', '0', 1, 1), ('CB', '0', '0', '100', 2, 2)`,
    ).run();
    const a = build(snapshot(chainStatus()), { insurancePoolContractId: 'CA' });
    expect(a.solvency.insurancePool?.claimsSharePct).toBe(0);
    const latest = build(snapshot(chainStatus()));
    expect(latest.solvency.insurancePool?.enrolledLps).toBe(2);
    const strict = build(snapshot(chainStatus()), {
      insurancePoolContractId: 'CB',
      thresholds: { insuranceClaimsStressedPct: 90 },
    });
    expect(strict.solvency.level).toBe('stressed');
  });

  it('never exposes admin, signer or timestamp data from the chain snapshot', () => {
    const json = JSON.stringify(build(snapshot(chainStatus())));
    for (const forbidden of ['GADMINSECRETADDRESS', 'admin', 'multisig', 'Signer', 'lastPause', '1234']) {
      expect(json).not.toContain(forbidden);
    }
  });
});

describe('GET /public/health', () => {
  let db: ReturnType<typeof createTestDb>;
  beforeEach(() => {
    db = createTestDb();
  });
  afterEach(() => db.close());

  const serviceFor = (snap: ProtocolStatusSnapshot): ProtocolStatusService => ({ get: async () => snap });

  it('returns the curated summary with public cache headers', async () => {
    const app = createTestApp(db, { protocolStatusService: serviceFor(snapshot(chainStatus())) });
    const res = await request(app).get('/public/health');
    expect(res.status).toBe(200);
    expect(res.headers['cache-control']).toContain('public');
    expect(res.body.schemaVersion).toBe(1);
    expect(res.body.overall).toBe('operational');
    expect(Object.keys(res.body).sort()).toEqual(
      ['generatedAt', 'oracle', 'overall', 'protocol', 'reasons', 'schemaVersion', 'solvency'].sort(),
    );
    expect(JSON.stringify(res.body)).not.toContain('GADMINSECRETADDRESS');
  });

  it('still answers 200 with overall unknown when the chain read throws', async () => {
    const app = createTestApp(db, {
      protocolStatusService: {
        get: async () => {
          throw new Error('rpc down');
        },
      },
    });
    const res = await request(app).get('/public/health');
    expect(res.status).toBe(200);
    expect(res.body.overall).toBe('unknown');
  });

  it('answers 200 unknown when no chain reader is configured', async () => {
    const res = await request(createTestApp(db)).get('/public/health');
    expect(res.status).toBe(200);
    expect(res.body.overall).toBe('unknown');
  });
});
