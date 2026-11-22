import type Database from 'better-sqlite3';
import { getProtocolStats } from '../../services/statsService.js';
import { getLeaderboard } from '../../services/leaderboardService.js';
import { parsePagination } from '../pagination.js';

// ---------------------------------------------------------------------------
// Internal row types
// ---------------------------------------------------------------------------

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

interface ReputationRow {
  new_score: number;
  invoices_submitted: number;
  invoices_paid: number;
  invoices_defaulted: number;
  ledger: number;
  event_type: string;
  timestamp: number;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function computeEffectiveYieldBps(discountRate: number, dueDate: number): number {
  const now = Math.floor(Date.now() / 1000);
  const secondsToExpiry = dueDate - now;
  if (secondsToExpiry <= 0) return 0;
  return Math.round((discountRate * secondsToExpiry) / (86400 * 365));
}

function mapInvoiceRow(invoice: InvoiceRow) {
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
    Math.ceil((invoice.due_date - now) / 86400)
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
}

// ---------------------------------------------------------------------------
// Resolver args types
// ---------------------------------------------------------------------------

interface InvoicesArgs {
  filter?: { status?: string; token?: string; submitter?: string };
  pagination?: { page?: number; pageSize?: number };
}

interface ReputationArgs {
  address: string;
}

interface LeaderboardArgs {
  limit?: number;
}

interface InvoiceArgs {
  id: number;
}

// ---------------------------------------------------------------------------
// Resolvers factory
// ---------------------------------------------------------------------------

export function createResolvers(db: Database.Database) {
  return {
    Query: {
      invoice(_: unknown, { id }: InvoiceArgs) {
        const row = db
          .prepare('SELECT * FROM invoices WHERE id = ?')
          .get(id) as InvoiceRow | undefined;

        return row ? mapInvoiceRow(row) : null;
      },

      invoices(_: unknown, { filter = {}, pagination = {} }: InvoicesArgs) {
        const { page, pageSize, offset } = parsePagination(
          pagination.page,
          pagination.pageSize
        );

        const conditions: string[] = [];
        const params: unknown[] = [];

        if (filter.status) {
          conditions.push('status = ?');
          params.push(filter.status);
        }
        if (filter.token) {
          conditions.push('token = ?');
          params.push(filter.token);
        }
        if (filter.submitter) {
          conditions.push('freelancer = ?');
          params.push(filter.submitter);
        }

        const where = conditions.length > 0 ? `WHERE ${conditions.join(' AND ')}` : '';

        const { total } = db
          .prepare(`SELECT COUNT(*) as total FROM invoices ${where}`)
          .get(...params) as { total: number };

        const rows = db
          .prepare(
            `SELECT * FROM invoices ${where} ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?`
          )
          .all(...params, pageSize, offset) as InvoiceRow[];

        return {
          invoices: rows.map(mapInvoiceRow),
          total,
          page,
          pageSize,
        };
      },

      reputation(_: unknown, { address }: ReputationArgs) {
        const latest = db
          .prepare(
            `SELECT new_score, invoices_submitted, invoices_paid, invoices_defaulted, ledger
             FROM reputation_updates
             WHERE address = ?
             ORDER BY id DESC
             LIMIT 1`
          )
          .get(address) as ReputationRow | undefined;

        const history = db
          .prepare(
            `SELECT new_score, ledger, event_type, timestamp
             FROM reputation_updates
             WHERE address = ?
             ORDER BY timestamp ASC`
          )
          .all(address) as Array<{
          new_score: number;
          ledger: number;
          event_type: string;
          timestamp: number;
        }>;

        return {
          address,
          score: latest?.new_score ?? 0,
          invoicesPaid: latest?.invoices_paid ?? 0,
          invoicesDefaulted: latest?.invoices_defaulted ?? 0,
          invoicesSubmitted: latest?.invoices_submitted ?? 0,
          lastActivityLedger: latest?.ledger ?? 0,
          history: history.map((r) => ({
            ledger: r.ledger,
            score: r.new_score,
            eventType: r.event_type,
            timestamp: r.timestamp,
          })),
        };
      },

      leaderboard(_: unknown, { limit = 50 }: LeaderboardArgs) {
        const safeLimit = Math.min(Math.max(1, limit), 100);
        return getLeaderboard(db, safeLimit);
      },

      stats() {
        return getProtocolStats(db);
      },

      governanceProposals() {
        return [];
      },
    },
  };
}
