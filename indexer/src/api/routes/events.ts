import { Router } from 'express';
import type Database from 'better-sqlite3';
import { parsePagination } from '../pagination.js';

interface EventRow {
  id: number;
  invoice_id: number;
  event_type: string;
  ledger: number;
  timestamp: number;
  data: string;
  freelancer: string;
  payer: string;
  funder: string | null;
}

export function createEventsRouter(db: Database.Database): Router {
  const router = Router();

  router.get('/events', (req, res) => {
    const address = req.query.address as string;
    const typesStr = req.query.types as string | undefined;
    const { page, pageSize, offset } = parsePagination(
      req.query.page,
      req.query.pageSize
    );

    if (!address) {
      res.status(400).json({ error: 'Address parameter is required' });
      return;
    }

    const conditions: string[] = ['(i.freelancer = ? OR i.payer = ? OR i.funder = ?)'];
    const params: any[] = [address, address, address];

    if (typesStr) {
      const types = typesStr.split(',');
      const placeHolders = types.map(() => '?').join(',');
      conditions.push(`e.event_type IN (${placeHolders})`);
      params.push(...types);
    }

    const whereClause = conditions.length > 0 ? `WHERE ${conditions.join(' AND ')}` : '';

    // Count query
    const countQuery = `
      SELECT COUNT(*) as total
      FROM events e
      JOIN invoices i ON e.invoice_id = i.id
      ${whereClause}
    `;
    const totalRow = db.prepare(countQuery).get(...params) as { total: number };
    const total = totalRow ? totalRow.total : 0;

    // Select query
    const selectQuery = `
      SELECT e.*, i.freelancer, i.payer, i.funder
      FROM events e
      JOIN invoices i ON e.invoice_id = i.id
      ${whereClause}
      ORDER BY e.timestamp DESC, e.id DESC
      LIMIT ? OFFSET ?
    `;
    const rows = db.prepare(selectQuery).all(...params, pageSize, offset) as EventRow[];

    const events = rows.map(r => ({
      id: r.id,
      invoiceId: r.invoice_id,
      type: r.event_type,
      ledger: r.ledger,
      timestamp: r.timestamp,
      data: JSON.parse(r.data || '{}'),
    }));

    res.json({
      events,
      total,
      page,
      pageSize
    });
  });

  return router;
}
