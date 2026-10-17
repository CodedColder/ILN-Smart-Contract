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
      invoice_id INTEGER,
      event_type TEXT NOT NULL,
      ledger INTEGER NOT NULL,
      timestamp INTEGER NOT NULL,
      data TEXT NOT NULL DEFAULT '{}',
      contract_id TEXT,
      contract_event_type TEXT,
      transaction_hash TEXT,
      transaction_paging_token TEXT,
      event_index INTEGER,
      topics_json TEXT NOT NULL DEFAULT '[]',
      FOREIGN KEY (invoice_id) REFERENCES invoices(id),
      UNIQUE(transaction_hash, event_index)
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
      timestamp INTEGER NOT NULL,
      contract_id TEXT,
      transaction_hash TEXT,
      transaction_paging_token TEXT,
      event_index INTEGER,
      topics_json TEXT NOT NULL DEFAULT '[]',
      UNIQUE(transaction_hash, event_index)
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

    CREATE TABLE IF NOT EXISTS indexer_state (
      state_key TEXT PRIMARY KEY,
      state_value TEXT NOT NULL
    );

    -- Canonical ledger headers captured as their ledgers are ingested.
    -- Reorg detection (Issue #863) compares each newly-ingested ledger's
    -- hash/parent_hash against this history; the consistency job (Issue
    -- #866) re-checks it against the chain as a backstop.
    CREATE TABLE IF NOT EXISTS ledger_headers (
      sequence INTEGER PRIMARY KEY,
      hash TEXT NOT NULL,
      parent_hash TEXT NOT NULL,
      ingested_at INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_events_invoice_id ON events(invoice_id);
    CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);
    CREATE INDEX IF NOT EXISTS idx_events_contract_id ON events(contract_id);
    CREATE INDEX IF NOT EXISTS idx_events_tx_hash ON events(transaction_hash);
    CREATE INDEX IF NOT EXISTS idx_reputation_address ON reputation_updates(address);
    CREATE INDEX IF NOT EXISTS idx_reputation_timestamp ON reputation_updates(timestamp);
    CREATE INDEX IF NOT EXISTS idx_reputation_tx_hash ON reputation_updates(transaction_hash);
    CREATE INDEX IF NOT EXISTS idx_invoices_status ON invoices(status);
    CREATE INDEX IF NOT EXISTS idx_invoices_created_at ON invoices(created_at);
    CREATE INDEX IF NOT EXISTS idx_invoices_token ON invoices(token);
    CREATE INDEX IF NOT EXISTS idx_invoices_funder ON invoices(funder);
    CREATE INDEX IF NOT EXISTS idx_events_event_type ON events(event_type);
    CREATE INDEX IF NOT EXISTS idx_events_contract_event_type ON events(contract_event_type);
    CREATE INDEX IF NOT EXISTS idx_reputation_updates_address_id ON reputation_updates(address, id);
    CREATE INDEX IF NOT EXISTS idx_stats_history_date ON stats_history(date);
    CREATE INDEX IF NOT EXISTS idx_indexer_state_key ON indexer_state(state_key);

    CREATE TABLE IF NOT EXISTS insurance_pool_enrollments (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      lp_address TEXT NOT NULL,
      contract_id TEXT NOT NULL,
      enrolled_at INTEGER NOT NULL,
      ledger INTEGER NOT NULL,
      transaction_hash TEXT NOT NULL,
      event_index INTEGER NOT NULL,
      UNIQUE(lp_address, contract_id, transaction_hash, event_index)
    );

    CREATE TABLE IF NOT EXISTS insurance_pool_premiums (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      lp_address TEXT NOT NULL,
      contract_id TEXT NOT NULL,
      amount TEXT NOT NULL,
      premium_at INTEGER NOT NULL,
      ledger INTEGER NOT NULL,
      transaction_hash TEXT NOT NULL,
      event_index INTEGER NOT NULL,
      UNIQUE(lp_address, contract_id, transaction_hash, event_index)
    );

    CREATE TABLE IF NOT EXISTS insurance_pool_claims (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      invoice_id INTEGER NOT NULL,
      lp_address TEXT NOT NULL,
      contract_id TEXT NOT NULL,
      payout_amount TEXT NOT NULL,
      claimed_at INTEGER NOT NULL,
      ledger INTEGER NOT NULL,
      transaction_hash TEXT NOT NULL,
      event_index INTEGER NOT NULL,
      UNIQUE(invoice_id, contract_id, transaction_hash, event_index)
    );

    CREATE TABLE IF NOT EXISTS insurance_pool_stats (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      contract_id TEXT NOT NULL UNIQUE,
      pool_balance TEXT NOT NULL DEFAULT '0',
      total_premiums_collected TEXT NOT NULL DEFAULT '0',
      total_claims_paid TEXT NOT NULL DEFAULT '0',
      enrolled_lp_count INTEGER NOT NULL DEFAULT 0,
      total_enrollment_events INTEGER NOT NULL DEFAULT 0,
      last_updated_at INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_insurance_enrollments_lp ON insurance_pool_enrollments(lp_address);
    CREATE INDEX IF NOT EXISTS idx_insurance_enrollments_contract ON insurance_pool_enrollments(contract_id);
    CREATE INDEX IF NOT EXISTS idx_insurance_enrollments_timestamp ON insurance_pool_enrollments(enrolled_at);
    CREATE INDEX IF NOT EXISTS idx_insurance_premiums_lp ON insurance_pool_premiums(lp_address);
    CREATE INDEX IF NOT EXISTS idx_insurance_premiums_contract ON insurance_pool_premiums(contract_id);
    CREATE INDEX IF NOT EXISTS idx_insurance_premiums_timestamp ON insurance_pool_premiums(premium_at);
    CREATE INDEX IF NOT EXISTS idx_insurance_claims_invoice ON insurance_pool_claims(invoice_id);
    CREATE INDEX IF NOT EXISTS idx_insurance_claims_lp ON insurance_pool_claims(lp_address);
    CREATE INDEX IF NOT EXISTS idx_insurance_claims_contract ON insurance_pool_claims(contract_id);
    CREATE INDEX IF NOT EXISTS idx_insurance_claims_timestamp ON insurance_pool_claims(claimed_at);
  `);
}
