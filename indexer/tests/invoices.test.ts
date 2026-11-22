import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import request from 'supertest';
import {
  createTestDb,
  seedInvoice,
  seedEvent,
  createTestApp,
} from './helpers.js';

describe('GET /invoices/:id', () => {
  let db: ReturnType<typeof createTestDb>;
  let app: ReturnType<typeof createTestApp>;

  beforeEach(() => {
    db = createTestDb();
    app = createTestApp(db);
  });

  afterEach(() => {
    db.close();
  });

  it('should return 404 for unknown invoice ID', async () => {
    const res = await request(app).get('/invoices/999');
    expect(res.status).toBe(404);
    expect(res.body.error).toBe('Invoice not found');
  });

  it('should return 400 for invalid invoice ID', async () => {
    const res = await request(app).get('/invoices/abc');
    expect(res.status).toBe(400);
    expect(res.body.error).toBe('Invalid invoice ID');
  });

  it('should return full invoice with all fields', async () => {
    const dueDate = Math.floor(Date.now() / 1000) + 30 * 24 * 60 * 60;
    seedInvoice(db, {
      id: 42,
      freelancer: 'GFREELANCER',
      payer: 'GPAYER',
      token: 'USDC_CONTRACT',
      amount: '5000000',
      due_date: dueDate,
      discount_rate: 500,
      status: 'Funded',
      funder: 'GLP_FUNDER',
      funded_at: 1700000000,
      amount_funded: '5000000',
      amount_paid: '0',
      referral_code: 'abc123',
      submitter_reputation: 75,
    });

    const res = await request(app).get('/invoices/42');
    expect(res.status).toBe(200);
    expect(res.body.id).toBe(42);
    expect(res.body.freelancer).toBe('GFREELANCER');
    expect(res.body.payer).toBe('GPAYER');
    expect(res.body.token).toBe('USDC_CONTRACT');
    expect(res.body.amount).toBe('5000000');
    expect(res.body.dueDate).toBe(dueDate);
    expect(res.body.discountRate).toBe(500);
    expect(res.body.status).toBe('Funded');
    expect(res.body.funder).toBe('GLP_FUNDER');
    expect(res.body.fundedAt).toBe(1700000000);
    expect(res.body.amountFunded).toBe('5000000');
    expect(res.body.amountPaid).toBe('0');
    expect(res.body.referralCode).toBe('abc123');
    expect(res.body.submitterReputation).toBe(75);
  });

  it('should include computed fields', async () => {
    const dueDate = Math.floor(Date.now() / 1000) + 30 * 24 * 60 * 60;
    seedInvoice(db, {
      id: 1,
      due_date: dueDate,
      discount_rate: 600,
      amount_funded: '1000000',
      amount_paid: '0',
    });

    const res = await request(app).get('/invoices/1');
    expect(res.status).toBe(200);
    expect(res.body.effectiveYieldBps).toBeGreaterThan(0);
    expect(res.body.remainingBalance).toBe('1000000');
    expect(res.body.daysUntilExpiry).toBeGreaterThan(0);
  });

  it('should compute remainingBalance correctly', async () => {
    seedInvoice(db, {
      id: 1,
      amount_funded: '5000000',
      amount_paid: '2000000',
      due_date: Math.floor(Date.now() / 1000) + 30 * 24 * 60 * 60,
    });

    const res = await request(app).get('/invoices/1');
    expect(res.body.remainingBalance).toBe('3000000');
  });

  it('should compute zero remainingBalance when fully paid', async () => {
    seedInvoice(db, {
      id: 1,
      amount_funded: '5000000',
      amount_paid: '5000000',
      due_date: Math.floor(Date.now() / 1000) + 30 * 24 * 60 * 60,
    });

    const res = await request(app).get('/invoices/1');
    expect(res.body.remainingBalance).toBe('0');
  });

  it('should include event history sorted by timestamp', async () => {
    seedInvoice(db, { id: 1 });
    seedEvent(db, {
      invoice_id: 1,
      event_type: 'submitted',
      ledger: 100,
      timestamp: 1700000000,
      data: JSON.stringify({ token: 'USDC' }),
    });
    seedEvent(db, {
      invoice_id: 1,
      event_type: 'funded',
      ledger: 200,
      timestamp: 1700100000,
      data: JSON.stringify({ funder: 'GLP' }),
    });
    seedEvent(db, {
      invoice_id: 1,
      event_type: 'paid',
      ledger: 300,
      timestamp: 1700200000,
      data: JSON.stringify({ amount: '500000' }),
    });

    const res = await request(app).get('/invoices/1');
    expect(res.status).toBe(200);
    expect(res.body.events).toHaveLength(3);
    expect(res.body.events[0].type).toBe('submitted');
    expect(res.body.events[0].ledger).toBe(100);
    expect(res.body.events[0].data).toEqual({ token: 'USDC' });
    expect(res.body.events[1].type).toBe('funded');
    expect(res.body.events[2].type).toBe('paid');
  });

  it('should only return events for the specific invoice', async () => {
    seedInvoice(db, { id: 1 });
    seedInvoice(db, { id: 2 });
    seedEvent(db, { invoice_id: 1, event_type: 'submitted', ledger: 100, timestamp: 1700000000 });
    seedEvent(db, { invoice_id: 2, event_type: 'submitted', ledger: 200, timestamp: 1700100000 });

    const res = await request(app).get('/invoices/1');
    expect(res.body.events).toHaveLength(1);
    expect(res.body.events[0].ledger).toBe(100);
  });

  it('should return empty events array for invoice with no events', async () => {
    seedInvoice(db, { id: 1 });

    const res = await request(app).get('/invoices/1');
    expect(res.status).toBe(200);
    expect(res.body.events).toEqual([]);
  });

  it('should handle createdAt field', async () => {
    seedInvoice(db, { id: 1, created_at: 1700000000 });

    const res = await request(app).get('/invoices/1');
    expect(res.body.createdAt).toBe(1700000000);
  });
});

describe('GET /invoices', () => {
  let db: ReturnType<typeof createTestDb>;
  let app: ReturnType<typeof createTestApp>;

  beforeEach(() => {
    db = createTestDb();
    app = createTestApp(db);
  });

  afterEach(() => {
    db.close();
  });

  it('should return all invoices when no query parameters are provided', async () => {
    seedInvoice(db, { id: 1, token: 'USDC', status: 'Pending', freelancer: 'G1' });
    seedInvoice(db, { id: 2, token: 'USDC', status: 'Funded', freelancer: 'G2' });

    const res = await request(app).get('/invoices');
    expect(res.status).toBe(200);
    expect(res.headers['cache-control']).toBe('max-age=10');
    expect(res.body.invoices).toHaveLength(2);
    expect(res.body.total).toBe(2);
  });

  it('should filter by state, token, and submitter', async () => {
    seedInvoice(db, { id: 1, token: 'USDC', status: 'Pending', freelancer: 'G1' });
    seedInvoice(db, { id: 2, token: 'XLM', status: 'Funded', freelancer: 'G1' });
    seedInvoice(db, { id: 3, token: 'USDC', status: 'Pending', freelancer: 'G2' });

    const res = await request(app).get('/invoices?state=Pending&token=USDC&submitter=G1');
    expect(res.status).toBe(200);
    expect(res.body.invoices).toHaveLength(1);
    expect(res.body.invoices[0].id).toBe(1);
    expect(res.body.total).toBe(1);
  });

  it('should support pagination', async () => {
    seedInvoice(db, { id: 1 });
    seedInvoice(db, { id: 2 });
    seedInvoice(db, { id: 3 });

    const res = await request(app).get('/invoices?page=2&pageSize=2&sortBy=createdAt&sortOrder=asc');
    expect(res.status).toBe(200);
    expect(res.body.invoices).toHaveLength(1);
    expect(res.body.total).toBe(3);
    expect(res.body.page).toBe(2);
    expect(res.body.pageSize).toBe(2);
  });

  it('should cap pageSize at 100 to match GraphQL', async () => {
    seedInvoice(db, { id: 1 });

    const res = await request(app).get('/invoices?pageSize=9999');
    expect(res.status).toBe(200);
    expect(res.body.pageSize).toBe(100);
  });

  it('should fall back to defaults for invalid page parameters', async () => {
    seedInvoice(db, { id: 1 });

    const res = await request(app).get('/invoices?page=abc&pageSize=nope');
    expect(res.status).toBe(200);
    expect(res.body.page).toBe(1);
    expect(res.body.pageSize).toBe(20);
    expect(res.body.invoices).toHaveLength(1);
  });

  it('should not duplicate or skip rows across pages when sort keys tie', async () => {
    seedInvoice(db, { id: 1, created_at: 1000 });
    seedInvoice(db, { id: 2, created_at: 1000 });
    seedInvoice(db, { id: 3, created_at: 1000 });
    seedInvoice(db, { id: 4, created_at: 1000 });

    const page1 = await request(app).get(
      '/invoices?sortBy=createdAt&sortOrder=desc&page=1&pageSize=2'
    );
    const page2 = await request(app).get(
      '/invoices?sortBy=createdAt&sortOrder=desc&page=2&pageSize=2'
    );

    const ids = [
      ...page1.body.invoices.map((invoice: { id: number }) => invoice.id),
      ...page2.body.invoices.map((invoice: { id: number }) => invoice.id),
    ];
    expect(new Set(ids).size).toBe(4);
    expect(ids).toEqual([4, 3, 2, 1]);
  });
});
