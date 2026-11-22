import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import Database from 'better-sqlite3';
import { initializeSchema } from '../../db/schema.js';
import { createResolvers } from './resolvers.js';
import { clearStatsCache } from '../../services/statsService.js';

// ---------------------------------------------------------------------------
// In-memory database setup
// ---------------------------------------------------------------------------

let db: Database.Database;

beforeEach(() => {
  db = new Database(':memory:');
  initializeSchema(db);
});

afterEach(() => {
  db.close();
  clearStatsCache();
});

// Convenience getter — resolvers are pure functions over the DB
function resolvers() {
  return createResolvers(db).Query;
}

// ---------------------------------------------------------------------------
// Seed helpers
// ---------------------------------------------------------------------------

function seedInvoice(overrides: Partial<{
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
}> = {}) {
  const now = Math.floor(Date.now() / 1000);
  const defaults = {
    id: 1,
    freelancer: 'GFREELANCER0000000000000000000000000000000000000000000000',
    payer: 'GPAYER000000000000000000000000000000000000000000000000000',
    token: 'CDTOKEN000000000000000000000000000000000000000000000000000',
    amount: '1000000',
    due_date: now + 86400 * 30,
    discount_rate: 300,
    status: 'Pending',
    funder: null,
    funded_at: null,
    amount_funded: '0',
    amount_paid: '0',
    referral_code: null,
    submitter_reputation: 50,
    created_at: now,
  };
  const row = { ...defaults, ...overrides };

  db.prepare(`
    INSERT INTO invoices
      (id, freelancer, payer, token, amount, due_date, discount_rate, status,
       funder, funded_at, amount_funded, amount_paid, referral_code,
       submitter_reputation, created_at)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
  `).run(
    row.id, row.freelancer, row.payer, row.token, row.amount,
    row.due_date, row.discount_rate, row.status, row.funder,
    row.funded_at, row.amount_funded, row.amount_paid,
    row.referral_code, row.submitter_reputation, row.created_at
  );

  return row;
}

function seedReputation(address: string, score = 80, invoicesPaid = 5, invoicesDefaulted = 0) {
  const now = Math.floor(Date.now() / 1000);
  db.prepare(`
    INSERT INTO reputation_updates
      (address, event_type, old_score, new_score, invoices_submitted,
       invoices_paid, invoices_defaulted, ledger, timestamp)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
  `).run(address, 'reputation_updated', 0, score, invoicesPaid + invoicesDefaulted,
     invoicesPaid, invoicesDefaulted, 1000, now);
}

// ---------------------------------------------------------------------------
// invoice query
// ---------------------------------------------------------------------------

describe("invoice", () => {
  it("returns the invoice when found", () => {
    seedInvoice({ id: 1 });
    const result = resolvers().invoice(null, { id: 1 });
    expect(result).not.toBeNull();
    expect(result!.id).toBe(1);
    expect(result!.status).toBe('Pending');
  });

  it("returns null when invoice does not exist", () => {
    const result = resolvers().invoice(null, { id: 999 });
    expect(result).toBeNull();
  });

  it("maps snake_case columns to camelCase fields", () => {
    const now = Math.floor(Date.now() / 1000);
    seedInvoice({ id: 2, due_date: now + 86400, created_at: now });
    const result = resolvers().invoice(null, { id: 2 });
    expect(result).toHaveProperty('dueDate');
    expect(result).toHaveProperty('createdAt');
    expect(result).toHaveProperty('amountFunded');
    expect(result).toHaveProperty('effectiveYieldBps');
    expect(result).toHaveProperty('remainingBalance');
    expect(result).toHaveProperty('daysUntilExpiry');
  });

  it("computes remainingBalance as amountFunded - amountPaid", () => {
    seedInvoice({ id: 3, amount_funded: '1000000', amount_paid: '400000' });
    const result = resolvers().invoice(null, { id: 3 });
    expect(result!.remainingBalance).toBe('600000');
  });

  it("clamps remainingBalance to '0' when fully paid", () => {
    seedInvoice({ id: 4, amount_funded: '500000', amount_paid: '600000' });
    const result = resolvers().invoice(null, { id: 4 });
    expect(result!.remainingBalance).toBe('0');
  });

  it("returns 0 effectiveYieldBps when due date has passed", () => {
    const past = Math.floor(Date.now() / 1000) - 1;
    seedInvoice({ id: 5, due_date: past });
    const result = resolvers().invoice(null, { id: 5 });
    expect(result!.effectiveYieldBps).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// invoices query
// ---------------------------------------------------------------------------

describe("invoices", () => {
  beforeEach(() => {
    seedInvoice({ id: 1, status: 'Pending', token: 'USDC' });
    seedInvoice({ id: 2, status: 'Funded', token: 'USDC',
      freelancer: 'GFREELANCER1111111111111111111111111111111111111111111111' });
    seedInvoice({ id: 3, status: 'Paid', token: 'EURC',
      freelancer: 'GFREELANCER2222222222222222222222222222222222222222222222' });
  });

  it("returns all invoices with default pagination", () => {
    const result = resolvers().invoices(null, {});
    expect(result.total).toBe(3);
    expect(result.invoices).toHaveLength(3);
    expect(result.page).toBe(1);
    expect(result.pageSize).toBe(20);
  });

  it("filters by status", () => {
    const result = resolvers().invoices(null, { filter: { status: 'Pending' } });
    expect(result.total).toBe(1);
    expect(result.invoices[0].status).toBe('Pending');
  });

  it("filters by token", () => {
    const result = resolvers().invoices(null, { filter: { token: 'EURC' } });
    expect(result.total).toBe(1);
    expect(result.invoices[0].token).toBe('EURC');
  });

  it("filters by submitter (freelancer)", () => {
    const result = resolvers().invoices(null, {
      filter: { submitter: 'GFREELANCER2222222222222222222222222222222222222222222222' },
    });
    expect(result.total).toBe(1);
    expect(result.invoices[0].id).toBe(3);
  });

  it("paginates correctly", () => {
    const result = resolvers().invoices(null, {
      pagination: { page: 1, pageSize: 2 },
    });
    expect(result.invoices).toHaveLength(2);
    expect(result.page).toBe(1);
    expect(result.pageSize).toBe(2);
    expect(result.total).toBe(3);
  });

  it("returns second page", () => {
    const result = resolvers().invoices(null, {
      pagination: { page: 2, pageSize: 2 },
    });
    expect(result.invoices).toHaveLength(1);
    expect(result.page).toBe(2);
  });

  it("caps pageSize at 100", () => {
    const result = resolvers().invoices(null, { pagination: { pageSize: 9999 } });
    expect(result.pageSize).toBe(100);
  });

  it("paginates deterministically when sort keys tie", () => {
    db.prepare(`UPDATE invoices SET created_at = 1000`).run();

    const page1 = resolvers().invoices(null, { pagination: { page: 1, pageSize: 2 } });
    const page2 = resolvers().invoices(null, { pagination: { page: 2, pageSize: 2 } });
    const ids = [...page1.invoices, ...page2.invoices].map((invoice) => invoice.id);

    expect(new Set(ids).size).toBe(3);
    expect(ids).toEqual([3, 2, 1]);
  });

  it("combines multiple filters", () => {
    const result = resolvers().invoices(null, {
      filter: { status: 'Funded', token: 'USDC' },
    });
    expect(result.total).toBe(1);
    expect(result.invoices[0].id).toBe(2);
  });

  it("returns empty list when no invoices match", () => {
    const result = resolvers().invoices(null, { filter: { status: 'Disputed' } });
    expect(result.total).toBe(0);
    expect(result.invoices).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// reputation query
// ---------------------------------------------------------------------------

describe("reputation", () => {
  const ADDR = 'GAAZI4TCR3TY5OJHCTJC2A4QSY6CJWJH5IAJTGKIN2ER7LBNVKOCCWN';

  it("returns zero profile for unknown address", () => {
    const result = resolvers().reputation(null, { address: ADDR });
    expect(result.score).toBe(0);
    expect(result.invoicesPaid).toBe(0);
    expect(result.invoicesDefaulted).toBe(0);
    expect(result.history).toHaveLength(0);
  });

  it("returns the latest reputation score", () => {
    seedReputation(ADDR, 80, 5, 1);
    const result = resolvers().reputation(null, { address: ADDR });
    expect(result.score).toBe(80);
    expect(result.invoicesPaid).toBe(5);
    expect(result.invoicesDefaulted).toBe(1);
  });

  it("includes history entries", () => {
    seedReputation(ADDR, 60, 3, 0);
    const result = resolvers().reputation(null, { address: ADDR });
    expect(result.history).toHaveLength(1);
    expect(result.history[0].score).toBe(60);
    expect(result.history[0].eventType).toBe('reputation_updated');
  });

  it("echoes the queried address", () => {
    const result = resolvers().reputation(null, { address: ADDR });
    expect(result.address).toBe(ADDR);
  });
});

// ---------------------------------------------------------------------------
// leaderboard query
// ---------------------------------------------------------------------------

describe("leaderboard", () => {
  it("returns an empty array when no reputations exist", () => {
    const result = resolvers().leaderboard(null, {});
    expect(result).toEqual([]);
  });

  it("returns entries ranked by score", () => {
    const A = 'GAAZI4TCR3TY5OJHCTJC2A4QSY6CJWJH5IAJTGKIN2ER7LBNVKOCCWN';
    const B = 'GBUVRIIBMHKC4REQCA754YCMQZYS3CJZQ5CKEKV2OHZ6C3XXZR3KFMK';
    seedReputation(A, 90, 10, 0);
    seedReputation(B, 50, 3, 1);

    const result = resolvers().leaderboard(null, { limit: 10 });
    expect(result).toHaveLength(2);
    expect(result[0].score).toBeGreaterThanOrEqual(result[1].score);
  });

  it("respects the limit parameter", () => {
    const addresses = [
      'GAAZI4TCR3TY5OJHCTJC2A4QSY6CJWJH5IAJTGKIN2ER7LBNVKOCCWN',
      'GBUVRIIBMHKC4REQCA754YCMQZYS3CJZQ5CKEKV2OHZ6C3XXZR3KFMK',
      'GCEZWKCA5VLDNRLN3RPRJMRZOX3Z6G5CHCGZM81ENQHQT36IOESXUQA',
    ];
    addresses.forEach((a, i) => seedReputation(a, (i + 1) * 10, i, 0));

    const result = resolvers().leaderboard(null, { limit: 2 });
    expect(result.length).toBeLessThanOrEqual(2);
  });
});

// ---------------------------------------------------------------------------
// stats query
// ---------------------------------------------------------------------------

describe("stats", () => {
  it("returns zero stats on an empty database", () => {
    const result = resolvers().stats(null, undefined);
    expect(result.totalInvoices).toBe(0);
    expect(result.totalFunded).toBe(0);
    expect(result.totalPaid).toBe(0);
  });

  it("counts invoices by status correctly", () => {
    seedInvoice({ id: 1, status: 'Pending' });
    seedInvoice({ id: 2, status: 'Paid',
      freelancer: 'GFREELANCER1111111111111111111111111111111111111111111111' });
    seedInvoice({ id: 3, status: 'Funded',
      freelancer: 'GFREELANCER2222222222222222222222222222222222222222222222' });

    const result = resolvers().stats(null, undefined);
    expect(result.totalInvoices).toBe(3);
    expect(result.totalPaid).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// governanceProposals query
// ---------------------------------------------------------------------------

describe("governanceProposals", () => {
  it("returns an empty array (proposals are on-chain only)", () => {
    const result = resolvers().governanceProposals(null, undefined);
    expect(result).toEqual([]);
  });
});
