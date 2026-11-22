import request from 'supertest';
import {
  createTestDb,
  seedInvoice,
  seedEvent,
  createTestApp,
} from './helpers.js';

describe('GET /events', () => {
  let db: ReturnType<typeof createTestDb>;
  let app: ReturnType<typeof createTestApp>;

  beforeEach(() => {
    db = createTestDb();
    app = createTestApp(db);
  });

  afterEach(() => {
    db.close();
  });

  it('should return 400 if address is missing', async () => {
    const res = await request(app).get('/events');
    expect(res.status).toBe(400);
    expect(res.body.error).toBe('Address parameter is required');
  });

  it('should return events where address is freelancer, payer, or funder', async () => {
    seedInvoice(db, { id: 1, freelancer: 'GFREELANCER', payer: 'GPAYER', funder: 'GLP' });
    seedEvent(db, { invoice_id: 1, event_type: 'InvoiceFunded', ledger: 100, timestamp: 1000, data: '{}' });
    seedEvent(db, { invoice_id: 1, event_type: 'InvoicePaid', ledger: 101, timestamp: 1001, data: '{}' });

    // Query for freelancer
    let res = await request(app).get('/events?address=GFREELANCER');
    expect(res.status).toBe(200);
    expect(res.body.events).toHaveLength(2);
    expect(res.body.total).toBe(2);

    // Query for funder/LP
    res = await request(app).get('/events?address=GLP');
    expect(res.status).toBe(200);
    expect(res.body.events).toHaveLength(2);
  });

  it('should filter events by type', async () => {
    seedInvoice(db, { id: 1, freelancer: 'GFREELANCER', payer: 'GPAYER' });
    seedEvent(db, { invoice_id: 1, event_type: 'InvoiceFunded', ledger: 100, timestamp: 1000 });
    seedEvent(db, { invoice_id: 1, event_type: 'InvoicePaid', ledger: 101, timestamp: 1001 });

    const res = await request(app).get('/events?address=GFREELANCER&types=InvoicePaid');
    expect(res.status).toBe(200);
    expect(res.body.events).toHaveLength(1);
    expect(res.body.events[0].type).toBe('InvoicePaid');
  });

  it('should support pagination and sort newest first', async () => {
    seedInvoice(db, { id: 1, freelancer: 'GFREELANCER' });
    seedEvent(db, { invoice_id: 1, event_type: 'InvoiceFunded', ledger: 100, timestamp: 1000 });
    seedEvent(db, { invoice_id: 1, event_type: 'InvoicePaid', ledger: 101, timestamp: 1001 });

    const res = await request(app).get('/events?address=GFREELANCER&page=1&pageSize=1');
    expect(res.status).toBe(200);
    expect(res.body.events).toHaveLength(1);
    expect(res.body.events[0].type).toBe('InvoicePaid'); // timestamp 1001 is newer
  });

  it('should cap pageSize at 100 and fall back on invalid paging params', async () => {
    seedInvoice(db, { id: 1, freelancer: 'GFREELANCER' });
    seedEvent(db, { invoice_id: 1 });

    const capped = await request(app).get('/events?address=GFREELANCER&pageSize=9999');
    expect(capped.status).toBe(200);
    expect(capped.body.pageSize).toBe(100);

    const invalid = await request(app).get('/events?address=GFREELANCER&page=x&pageSize=y');
    expect(invalid.status).toBe(200);
    expect(invalid.body.page).toBe(1);
    expect(invalid.body.pageSize).toBe(20);
  });
});
