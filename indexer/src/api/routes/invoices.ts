import { Router } from 'express';
import type Database from 'better-sqlite3';
import { parsePagination } from '../pagination.js';

interface InvoiceRow {
  id: number;
  freelancer: string;
  payer: string;
  token: string;
  amount: string;
  due_date: number;
  discount_rate: number;
  status: string;
  funder: string | null;
  funded_at: number | null;
  amount_funded: string;
  amount_paid: string;
  referral_code: string | null;
  submitter_reputation: number;
  created_at: number;
}

interface EventRow {
  event_type: string;
  ledger: number;
  timestamp: number;
  data: string;
}

interface InvoiceResponse {
  id: number;
  freelancer: string;
  payer: string;
  token: string;
  amount: string;
  dueDate: number;
  discountRate: number;
  status: string;
  funder: string | null;
  fundedAt: number | null;
  amountFunded: string;
  amountPaid: string;
  referralCode: string | null;
  submitterReputation: number;
  createdAt: number;
  effectiveYieldBps: number;
  remainingBalance: string;
  daysUntilExpiry: number;
  events: Array<{
    type: string;
    ledger: number;
    timestamp: number;
    data: Record<string, unknown>;
  }>;
}

function computeEffectiveYieldBps(discountRate: number, dueDate: number): number {
  const now = Math.floor(Date.now() / 1000);
  const secondsToExpiry = dueDate - now;
  if (secondsToExpiry <= 0) return 0;
  const daysToExpiry = secondsToExpiry / (24 * 60 * 60);
  return Math.round((discountRate * daysToExpiry) / 365);
}

export function createInvoicesRouter(db: Database.Database): Router {
  const router = Router();

  router.get('/invoices/:id', (req, res) => {
    const invoiceId = parseInt(req.params.id, 10);
    if (isNaN(invoiceId)) {
      res.status(400).json({ error: 'Invalid invoice ID' });
      return;
    }

    const invoice = db
      .prepare('SELECT * FROM invoices WHERE id = ?')
      .get(invoiceId) as InvoiceRow | undefined;

    if (!invoice) {
      res.status(404).json({ error: 'Invoice not found' });
      return;
    }

    const eventRows = db
      .prepare('SELECT * FROM events WHERE invoice_id = ? ORDER BY timestamp ASC')
      .all(invoiceId) as EventRow[];

    const effectiveYieldBps = computeEffectiveYieldBps(
      invoice.discount_rate,
      invoice.due_date
    );

    const amountFunded = BigInt(invoice.amount_funded || '0');
    const amountPaid = BigInt(invoice.amount_paid || '0');
    const remainingBalance =
      amountPaid > amountFunded ? '0' : String(amountFunded - amountPaid);

    const now = Math.floor(Date.now() / 1000);
    const daysUntilExpiry = Math.max(
      0,
      Math.ceil((invoice.due_date - now) / (24 * 60 * 60))
    );

    const response: InvoiceResponse = {
      id: invoice.id,
      freelancer: invoice.freelancer,
      payer: invoice.payer,
      token: invoice.token,
      amount: invoice.amount,
      dueDate: invoice.due_date,
      discountRate: invoice.discount_rate,
      status: invoice.status,
      funder: invoice.funder,
      fundedAt: invoice.funded_at,
      amountFunded: invoice.amount_funded,
      amountPaid: invoice.amount_paid,
      referralCode: invoice.referral_code,
      submitterReputation: invoice.submitter_reputation,
      createdAt: invoice.created_at,
      effectiveYieldBps,
      remainingBalance,
      daysUntilExpiry,
      events: eventRows.map((e) => ({
        type: e.event_type,
        ledger: e.ledger,
        timestamp: e.timestamp,
        data: JSON.parse(e.data || '{}'),
      })),
    };

    res.json(response);
  });

  router.get('/invoices', (req, res) => {
    res.setHeader('Cache-Control', 'max-age=10');

    const state = req.query.state as string | undefined;
    const token = req.query.token as string | undefined;
    const submitter = req.query.submitter as string | undefined;

    const { page, pageSize, offset } = parsePagination(
      req.query.page,
      req.query.pageSize
    );

    const sortByParam = req.query.sortBy as string || 'createdAt';
    const sortOrderParam = (req.query.sortOrder as string || 'desc').toLowerCase();

    const sortFieldMap: Record<string, string> = {
      createdAt: 'created_at',
      dueDate: 'due_date',
      amount: 'CAST(amount AS REAL)',
      discountRate: 'discount_rate'
    };

    const orderBy = sortFieldMap[sortByParam] || 'created_at';
    const order = sortOrderParam === 'asc' ? 'ASC' : 'DESC';

    const conditions: string[] = [];
    const params: any[] = [];

    if (state) {
      conditions.push('status = ?');
      params.push(state);
    }
    if (token) {
      conditions.push('token = ?');
      params.push(token);
    }
    if (submitter) {
      conditions.push('freelancer = ?');
      params.push(submitter);
    }

    const whereClause = conditions.length > 0 ? `WHERE ${conditions.join(' AND ')}` : '';

    const countQuery = `SELECT COUNT(*) as total FROM invoices ${whereClause}`;
    const totalRow = db.prepare(countQuery).get(...params) as { total: number };
    const total = totalRow ? totalRow.total : 0;

    // `id` is the deterministic tie-breaker: without it, rows sharing the sort
    // key can be reordered between pages and appear twice or be skipped.
    const selectQuery = `
      SELECT * FROM invoices
      ${whereClause}
      ORDER BY ${orderBy} ${order}, id ${order}
      LIMIT ? OFFSET ?
    `;
    const rows = db.prepare(selectQuery).all(...params, pageSize, offset) as InvoiceRow[];

    const invoices = rows.map((invoice) => {
      const effectiveYieldBps = computeEffectiveYieldBps(
        invoice.discount_rate,
        invoice.due_date
      );
      const amountFunded = BigInt(invoice.amount_funded || '0');
      const amountPaid = BigInt(invoice.amount_paid || '0');
      const remainingBalance =
        amountPaid > amountFunded ? '0' : String(amountFunded - amountPaid);
      const now = Math.floor(Date.now() / 1000);
      const daysUntilExpiry = Math.max(
        0,
        Math.ceil((invoice.due_date - now) / (24 * 60 * 60))
      );

      return {
        id: invoice.id,
        freelancer: invoice.freelancer,
        payer: invoice.payer,
        token: invoice.token,
        amount: invoice.amount,
        dueDate: invoice.due_date,
        discountRate: invoice.discount_rate,
        status: invoice.status,
        funder: invoice.funder,
        fundedAt: invoice.funded_at,
        amountFunded: invoice.amount_funded,
        amountPaid: invoice.amount_paid,
        referralCode: invoice.referral_code,
        submitterReputation: invoice.submitter_reputation,
        createdAt: invoice.created_at,
        effectiveYieldBps,
        remainingBalance,
        daysUntilExpiry,
      };
    });

    res.json({
      invoices,
      total,
      page,
      pageSize
    });
  });

  return router;
}
