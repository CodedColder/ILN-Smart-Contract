import type Database from 'better-sqlite3';

export function initializeSchema(db: Database.Database): void {
  db.exec(`
    CREATE TABLE IF NOT EXISTS invoices (
      id INTEGER PRIMARY KEY,
      freelancer TEXT NOT NULL,
      payer TEXT NOT NULL,
      token TEXT NOT NULL,
      amount TEXT NOT NULL,
      due_date INTEGER NOT NULL,
      discount_rate INTEGER NOT NULL,
      status TEXT NOT NULL DEFAULT 'Pending',
      funder TEXT,
      funded_at INTEGER,
      amount_funded TEXT NOT NULL DEFAULT '0',
      amount_paid TEXT NOT NULL DEFAULT '0',
      referral_code TEXT,
      submitter_reputation INTEGER NOT NULL DEFAULT 0,
      created_at INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS events (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      invoice_id INTEGER NOT NULL,
      event_type TEXT NOT NULL,
      ledger INTEGER NOT NULL,
      timestamp INTEGER NOT NULL,
      data TEXT NOT NULL DEFAULT '{}',
      FOREIGN KEY (invoice_id) REFERENCES invoices(id)
    );

    CREATE TABLE IF NOT EXISTS reputation_updates (
      id INTEGER PRIMARY KEY,
      address TEXT NOT NULL,
      event_type TEXT NOT NULL DEFAULT 'reputation_updated',
      old_score INTEGER NOT NULL DEFAULT 0,
      new_score INTEGER NOT NULL DEFAULT 0,
      invoices_submitted INTEGER NOT NULL DEFAULT 0,
      invoices_paid INTEGER NOT NULL DEFAULT 0,
      invoices_defaulted INTEGER NOT NULL DEFAULT 0,
      ledger INTEGER NOT NULL,
      timestamp INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS stats_snapshots (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      total_invoices INTEGER NOT NULL DEFAULT 0,
      total_funded INTEGER NOT NULL DEFAULT 0,
      total_paid INTEGER NOT NULL DEFAULT 0,
      total_cancelled INTEGER NOT NULL DEFAULT 0,
      total_expired INTEGER NOT NULL DEFAULT 0,
      total_disputed INTEGER NOT NULL DEFAULT 0,
      volume_by_token TEXT NOT NULL DEFAULT '{}',
      avg_discount_rate_bps REAL NOT NULL DEFAULT 0,
      dispute_rate REAL NOT NULL DEFAULT 0,
      last_updated_at INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS stats_history (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      date TEXT NOT NULL,
      total_invoices INTEGER NOT NULL DEFAULT 0,
      total_funded INTEGER NOT NULL DEFAULT 0,
      total_paid INTEGER NOT NULL DEFAULT 0,
      total_volume TEXT NOT NULL DEFAULT '0',
      avg_discount_rate_bps REAL NOT NULL DEFAULT 0
    );

    CREATE INDEX IF NOT EXISTS idx_events_invoice_id ON events(invoice_id);
    CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);
    CREATE INDEX IF NOT EXISTS idx_reputation_address ON reputation_updates(address);
    CREATE INDEX IF NOT EXISTS idx_reputation_timestamp ON reputation_updates(timestamp);
    CREATE INDEX IF NOT EXISTS idx_invoices_status ON invoices(status);
    CREATE INDEX IF NOT EXISTS idx_invoices_created_at ON invoices(created_at);
    CREATE INDEX IF NOT EXISTS idx_invoices_token ON invoices(token);
    CREATE INDEX IF NOT EXISTS idx_invoices_funder ON invoices(funder);
    CREATE INDEX IF NOT EXISTS idx_events_event_type ON events(event_type);
    CREATE INDEX IF NOT EXISTS idx_events_contract_event_type ON events(contract_event_type);
    CREATE INDEX IF NOT EXISTS idx_reputation_updates_address_id ON reputation_updates(address, id);
    CREATE INDEX IF NOT EXISTS idx_stats_history_date ON stats_history(date);
    CREATE INDEX IF NOT EXISTS idx_invoices_freelancer ON invoices(freelancer);
    CREATE INDEX IF NOT EXISTS idx_invoices_payer ON invoices(payer);
    CREATE INDEX IF NOT EXISTS idx_invoices_funder ON invoices(funder);
  `);
}
