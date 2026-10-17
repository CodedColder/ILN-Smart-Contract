/**
 * Tests for monitoring & analytics services (issues #884–#887).
 */

import { describe, it, expect, beforeEach } from 'vitest';
import Database from 'better-sqlite3';
import { initializeSchema } from '../src/database/schema.js';
import {
  getReputationHistory,
  getReputationAuditSummary,
  detectReputationAnomalies,
} from '../src/services/reputationAuditService.js';
import {
  getQueueDepthOverTime,
  getFundingVelocity,
  getDefaultRate,
  getTimeToFundDistribution,
  getCurrentQueueDepth,
  getProtocolHealthDashboard,
} from '../src/services/protocolHealthService.js';
import { AdminActionMonitor } from '../src/services/adminActionMonitor.js';
import {
  getOracleHealthDashboard,
  getRecentOracleTrips,
} from '../src/services/oracleHealthService.js';
import { AlertRouter } from '../src/services/alertRouter.js';

let db: Database.Database;

beforeEach(() => {
  db = new Database(':memory:');
  initializeSchema(db);
});

// ── #884: Reputation Audit Trail ─────────────────────────────────────────────

describe('Reputation Audit Trail (#884)', () => {
  function insertReputationUpdate(
    address: string,
    oldScore: number,
    newScore: number,
    ledger: number,
    timestamp: number
  ) {
    db.prepare(
      `
      INSERT INTO reputation_updates
        (address, old_score, new_score, invoices_submitted, invoices_paid, invoices_defaulted, ledger, timestamp)
      VALUES (?, ?, ?, 1, 1, 0, ?, ?)
    `
    ).run(address, oldScore, newScore, ledger, timestamp);
  }

  it('fetches reputation history for an address', () => {
    insertReputationUpdate('addr1', 0, 10, 100, 1000);
    insertReputationUpdate('addr1', 10, 25, 101, 1100);
    insertReputationUpdate('addr2', 0, 5, 102, 1200);

    const history = getReputationHistory(db, 'addr1');
    expect(history).toHaveLength(2);
    expect(history[0]!.newScore).toBe(25);
    expect(history[1]!.newScore).toBe(10);
  });

  it('returns empty history for unknown address', () => {
    const history = getReputationHistory(db, 'unknown');
    expect(history).toHaveLength(0);
  });

  it('computes audit summary with anomaly detection', () => {
    // Normal updates
    for (let i = 0; i < 10; i++) {
      insertReputationUpdate('addr1', i * 10, i * 10 + 5, 100 + i, 1000 + i * 100);
    }
    // Anomalous spike
    insertReputationUpdate('addr1', 50, 200, 110, 2000);

    const summary = getReputationAuditSummary(db);
    expect(summary.totalUpdates).toBe(11);
    expect(summary.uniqueAddresses).toBe(1);
    expect(summary.recentUpdates.length).toBeGreaterThan(0);
  });

  it('detects anomalous score spikes', () => {
    const history = [];
    const now = Math.floor(Date.now() / 1000);
    for (let i = 0; i < 10; i++) {
      history.push({
        address: 'addr1',
        oldScore: i * 10,
        newScore: i * 10 + 2,
        scoreDelta: 2,
        invoicesSubmitted: 1,
        invoicesPaid: 1,
        invoicesDefaulted: 0,
        ledger: 100 + i,
        timestamp: now - (10 - i) * 3600,
        eventType: 'reputation_updated',
      });
    }
    // Anomalous spike
    history.push({
      address: 'addr1',
      oldScore: 20,
      newScore: 200,
      scoreDelta: 180,
      invoicesSubmitted: 1,
      invoicesPaid: 1,
      invoicesDefaulted: 0,
      ledger: 111,
      timestamp: now,
      eventType: 'reputation_updated',
    });

    const anomalies = detectReputationAnomalies(history);
    expect(anomalies.length).toBeGreaterThanOrEqual(1);
    expect(anomalies.some((a) => a.direction === 'spike')).toBe(true);
  });
});

// ── #885: Protocol Health Dashboard ──────────────────────────────────────────

describe('Protocol Health Dashboard (#885)', () => {
  function insertInvoice(
    status: string,
    createdAt: number,
    fundedAt: number | null,
    amountFunded: string = '0',
    amountPaid: string = '0'
  ) {
    db.prepare(
      `
      INSERT INTO invoices
        (freelancer, payer, token, amount, due_date, discount_rate, status, amount_funded, amount_paid, created_at, funded_at)
      VALUES ('freelancer', 'payer', 'USDC', '1000', 9999999999, 500, ?, ?, ?, ?, ?)
    `
    ).run(status, amountFunded, amountPaid, createdAt, fundedAt);
  }

  it('tracks queue depth over time', () => {
    const now = Math.floor(Date.now() / 1000);
    insertInvoice('Pending', now - 7200, null);
    insertInvoice('Funded', now - 3600, now - 1800, '1000', '1050');

    const depth = getQueueDepthOverTime(db);
    expect(depth.length).toBeGreaterThanOrEqual(1);
  });

  it('computes funding velocity', () => {
    const now = Math.floor(Date.now() / 1000);
    insertInvoice('Paid', now - 86400, now - 82800, '1000', '1050');

    const velocity = getFundingVelocity(db, 30);
    expect(velocity.length).toBeGreaterThanOrEqual(1);
    expect(velocity[0]!.invoicesFunded).toBeGreaterThanOrEqual(1);
  });

  it('computes default rate', () => {
    const now = Math.floor(Date.now() / 1000);
    insertInvoice('Expired', now - 86400, null);
    insertInvoice('Paid', now - 43200, now - 36000, '1000', '1050');

    const rates = getDefaultRate(db, 30);
    expect(rates.length).toBeGreaterThanOrEqual(1);
  });

  it('computes time-to-fund distribution', () => {
    const now = Math.floor(Date.now() / 1000);
    insertInvoice('Paid', now - 7200, now - 3600, '1000', '1050');

    const dist = getTimeToFundDistribution(db);
    expect(dist.length).toBeGreaterThanOrEqual(1);
  });

  it('gets current queue depth', () => {
    const now = Math.floor(Date.now() / 1000);
    insertInvoice('Pending', now - 1000, null);
    insertInvoice('Pending', now - 500, null);

    expect(getCurrentQueueDepth(db)).toBe(2);
  });

  it('builds full dashboard', () => {
    const dashboard = getProtocolHealthDashboard(db);
    expect(dashboard).toHaveProperty('queueDepthOverTime');
    expect(dashboard).toHaveProperty('fundingVelocity30d');
    expect(dashboard).toHaveProperty('defaultRate30d');
    expect(dashboard).toHaveProperty('healthyRanges');
  });
});

// ── #886: Admin Action Monitor ───────────────────────────────────────────────

describe('Admin Action Monitor (#886)', () => {
  it('detects rapid successive actions', async () => {
    const router = new AlertRouter({ minSeverity: 'info', cooldownMs: 0 });
    const delivered: unknown[] = [];
    router.addChannel({
      name: 'test',
      severityFilter: ['critical', 'warning', 'info'],
      send: async (alert) => { delivered.push(alert); },
    });

    const monitor = new AdminActionMonitor(
      { rapidSuccessiveThreshold: 3, rapidSuccessiveWindowMinutes: 30 },
      router
    );

    const now = Math.floor(Date.now() / 1000);
    await monitor.handleAdminAction({ action: 'update_fee_rate', timestamp: now - 1200, ledger: 100 });
    await monitor.handleAdminAction({ action: 'update_max_discount', timestamp: now - 600, ledger: 101 });
    const anomalies = await monitor.handleAdminAction({
      action: 'set_admin',
      timestamp: now,
      ledger: 102,
    });

    expect(anomalies.length).toBeGreaterThanOrEqual(1);
    expect(anomalies.some((a) => a.type === 'rapid_successive')).toBe(true);
  });

  it('detects off-hours activity', async () => {
    const router = new AlertRouter({ minSeverity: 'info', cooldownMs: 0 });
    const delivered: unknown[] = [];
    router.addChannel({
      name: 'test',
      severityFilter: ['critical', 'warning', 'info'],
      send: async (alert) => { delivered.push(alert); },
    });

    const monitor = new AdminActionMonitor(
      { offHoursStart: 22, offHoursEnd: 6 },
      router
    );

    // 23:00 UTC is off-hours
    const offHoursTime = new Date();
    offHoursTime.setUTCHours(23, 0, 0, 0);

    const anomalies = await monitor.handleAdminAction({
      action: 'set_admin',
      timestamp: Math.floor(offHoursTime.getTime() / 1000),
      ledger: 100,
    });

    expect(anomalies.some((a) => a.type === 'off_hours')).toBe(true);
  });

  it('routes anomalies through alert router', async () => {
    const router = new AlertRouter({ minSeverity: 'info', cooldownMs: 0 });
    const delivered: unknown[] = [];
    router.addChannel({
      name: 'test',
      severityFilter: ['critical', 'warning', 'info'],
      send: async (alert) => { delivered.push(alert); },
    });

    const monitor = new AdminActionMonitor(
      { rapidSuccessiveThreshold: 2, rapidSuccessiveWindowMinutes: 30 },
      router
    );

    const now = Math.floor(Date.now() / 1000);
    await monitor.handleAdminAction({ action: 'a', timestamp: now - 600, ledger: 100 });
    await monitor.handleAdminAction({ action: 'b', timestamp: now, ledger: 101 });

    expect(delivered.length).toBeGreaterThanOrEqual(1);
  });
});

// ── #887: Oracle Health Dashboard ────────────────────────────────────────────

describe('Oracle Health Dashboard (#887)', () => {
  function insertOracleEvent(eventType: string, data: Record<string, unknown>, timestamp: number) {
    db.prepare(
      `
      INSERT INTO events (invoice_id, event_type, ledger, timestamp, data)
      VALUES (1, ?, 100, ?, ?)
    `
    ).run(eventType, timestamp, JSON.stringify(data));
  }

  it('computes oracle health dashboard', () => {
    const now = Math.floor(Date.now() / 1000);
    insertOracleEvent('OracleHealthRecorded', {
      feedType: 'price',
      token: 'USDC',
      latencyLedgers: 2,
      isHealthy: true,
      circuitTripped: false,
    }, now);

    const dashboard = getOracleHealthDashboard(db);
    expect(dashboard).toHaveProperty('feeds');
    expect(dashboard).toHaveProperty('aggregateMetrics');
    expect(dashboard).toHaveProperty('healthyRanges');
  });

  it('fetches recent oracle trips', () => {
    const now = Math.floor(Date.now() / 1000);
    insertOracleEvent('OracleCircuitTripped', {
      feedType: 'price',
      token: 'USDC',
    }, now);
    insertOracleEvent('OracleCircuitReset', {
      feedType: 'price',
      token: 'USDC',
    }, now + 100);

    const trips = getRecentOracleTrips(db);
    expect(trips.length).toBeGreaterThanOrEqual(1);
  });
});
