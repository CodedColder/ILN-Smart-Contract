import type Database from 'better-sqlite3';

export interface InvoiceRecord {
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

export interface ReputationUpdateRecord {
  address: string;
  event_type: string;
  old_score: number;
  new_score: number;
  invoices_submitted: number;
  invoices_paid: number;
  invoices_defaulted: number;
  ledger: number;
  timestamp: number;
  contract_id: string | null;
  transaction_hash: string | null;
  transaction_paging_token: string | null;
  event_index: number | null;
  topics_json: string;
}

export interface InsurancePoolEnrollmentRecord {
  lp_address: string;
  contract_id: string;
  enrolled_at: number;
  ledger: number;
  transaction_hash: string;
  event_index: number;
}

export interface InsurancePoolPremiumRecord {
  lp_address: string;
  contract_id: string;
  amount: string;
  premium_at: number;
  ledger: number;
  transaction_hash: string;
  event_index: number;
}

export interface InsurancePoolClaimRecord {
  invoice_id: number;
  lp_address: string;
  contract_id: string;
  payout_amount: string;
  claimed_at: number;
  ledger: number;
  transaction_hash: string;
  event_index: number;
}

export interface IngestedEventRecord {
  invoice_id: number | null;
  event_type: string;
  ledger: number;
  timestamp: number;
  data: string;
  contract_id: string | null;
  contract_event_type: string | null;
  transaction_hash: string | null;
  transaction_paging_token: string | null;
  event_index: number | null;
  topics_json: string;
}

/** Position of an event in chain order: ledger, then paging token, then event index. */
export interface EventPosition {
  ledger: number;
  transaction_paging_token: string;
  event_index: number;
}

export interface StoredInvoiceEvent {
  ledger: number;
  /** Null only for legacy rows written before paging tokens were recorded. */
  transaction_paging_token: string | null;
  event_index: number | null;
  event_type: string;
  contract_event_type: string | null;
  contract_id: string | null;
  transaction_hash: string | null;
  timestamp: number;
  data: string;
  topics_json: string;
}

export interface EventRepository {
  /**
   * Run `fn` atomically: every write inside commits together or not at all.
   * Implementations without transactions may omit it; callers then fall back
   * to running `fn` directly.
   */
  transaction?<T>(fn: () => T): T;
  /** Whether an event for `invoiceId` sorts after `position` in chain order. */
  hasLaterInvoiceEvent?(invoiceId: number, position: EventPosition): boolean;
  /** Every stored event for `invoiceId`, in chain order. */
  listInvoiceEvents?(invoiceId: number): StoredInvoiceEvent[];
  getState(key: string): string | undefined;
  setState(key: string, value: string): void;
  getInvoice(id: number): InvoiceRecord | undefined;
  upsertInvoice(invoice: InvoiceRecord): void;
  insertEvent(event: IngestedEventRecord): void;
  insertReputationUpdate(update: ReputationUpdateRecord): void;
  insertInsuranceEnrollment(enrollment: InsurancePoolEnrollmentRecord): void;
  insertInsurancePremium(premium: InsurancePoolPremiumRecord): void;
  insertInsuranceClaim(claim: InsurancePoolClaimRecord): void;
  getInsurancePoolStats(contractId: string): { pool_balance: string; total_premiums_collected: string; total_claims_paid: string; enrolled_lp_count: number } | undefined;
  upsertInsurancePoolStats(contractId: string, balance: string, premiums: string, claims: string, enrolledCount: number): void;
}

export function createSqlEventRepository(db: Database.Database): EventRepository {
  return {
    transaction(fn) {
      // better-sqlite3 nests these as savepoints, so a batch transaction can
      // wrap per-record ones.
      return db.transaction(fn)();
    },
    hasLaterInvoiceEvent(invoiceId, position) {
      const row = db
        .prepare(
          `
          SELECT 1 FROM events
           WHERE invoice_id = @invoiceId
             AND (
               ledger > @ledger
               OR (ledger = @ledger AND CAST(transaction_paging_token AS INTEGER) > CAST(@token AS INTEGER))
               OR (ledger = @ledger AND transaction_paging_token = @token AND event_index > @eventIndex)
             )
           LIMIT 1
        `
        )
        .get({
          invoiceId,
          ledger: position.ledger,
          token: position.transaction_paging_token,
          eventIndex: position.event_index,
        });
      return row !== undefined;
    },
    listInvoiceEvents(invoiceId) {
      return db
        .prepare(
          `
          SELECT ledger, transaction_paging_token, event_index, event_type,
                 contract_event_type, contract_id, transaction_hash, timestamp,
                 data, topics_json
            FROM events
           WHERE invoice_id = ?
           ORDER BY ledger ASC, CAST(transaction_paging_token AS INTEGER) ASC, event_index ASC, id ASC
        `
        )
        .all(invoiceId) as StoredInvoiceEvent[];
    },
    getState(key) {
      const row = db
        .prepare('SELECT state_value FROM indexer_state WHERE state_key = ?')
        .get(key) as { state_value: string } | undefined;
      return row?.state_value;
    },
    setState(key, value) {
      db.prepare(
        `
        INSERT INTO indexer_state (state_key, state_value)
        VALUES (?, ?)
        ON CONFLICT(state_key) DO UPDATE SET state_value = excluded.state_value
      `
      ).run(key, value);
    },
    getInvoice(id) {
      return db
        .prepare('SELECT * FROM invoices WHERE id = ?')
        .get(id) as InvoiceRecord | undefined;
    },
    upsertInvoice(invoice) {
      db.prepare(
        `
        INSERT INTO invoices (
          id, freelancer, payer, token, amount, due_date, discount_rate, status,
          funder, funded_at, amount_funded, amount_paid, referral_code,
          submitter_reputation, created_at
        ) VALUES (
          @id, @freelancer, @payer, @token, @amount, @due_date, @discount_rate, @status,
          @funder, @funded_at, @amount_funded, @amount_paid, @referral_code,
          @submitter_reputation, @created_at
        )
        ON CONFLICT(id) DO UPDATE SET
          freelancer = excluded.freelancer,
          payer = excluded.payer,
          token = excluded.token,
          amount = excluded.amount,
          due_date = excluded.due_date,
          discount_rate = excluded.discount_rate,
          status = excluded.status,
          funder = excluded.funder,
          funded_at = excluded.funded_at,
          amount_funded = excluded.amount_funded,
          amount_paid = excluded.amount_paid,
          referral_code = excluded.referral_code,
          submitter_reputation = excluded.submitter_reputation,
          created_at = excluded.created_at
      `
      ).run(invoice);
    },
    insertEvent(event) {
      db.prepare(
        `
        INSERT INTO events (
          invoice_id, event_type, ledger, timestamp, data, contract_id,
          contract_event_type, transaction_hash, transaction_paging_token,
          event_index, topics_json
        ) VALUES (
          @invoice_id, @event_type, @ledger, @timestamp, @data, @contract_id,
          @contract_event_type, @transaction_hash, @transaction_paging_token,
          @event_index, @topics_json
        )
        ON CONFLICT(transaction_hash, event_index) DO NOTHING
      `
      ).run(event);
    },
    insertReputationUpdate(update) {
      db.prepare(
        `
        INSERT INTO reputation_updates (
          address, event_type, old_score, new_score, invoices_submitted,
          invoices_paid, invoices_defaulted, ledger, timestamp, contract_id,
          transaction_hash, transaction_paging_token, event_index, topics_json
        ) VALUES (
          @address, @event_type, @old_score, @new_score, @invoices_submitted,
          @invoices_paid, @invoices_defaulted, @ledger, @timestamp, @contract_id,
          @transaction_hash, @transaction_paging_token, @event_index, @topics_json
        )
        ON CONFLICT(transaction_hash, event_index) DO NOTHING
      `
      ).run(update);
    },
    insertInsuranceEnrollment(enrollment) {
      db.prepare(
        `
        INSERT INTO insurance_pool_enrollments (
          lp_address, contract_id, enrolled_at, ledger, transaction_hash, event_index
        ) VALUES (
          @lp_address, @contract_id, @enrolled_at, @ledger, @transaction_hash, @event_index
        )
        ON CONFLICT(lp_address, contract_id, transaction_hash, event_index) DO NOTHING
      `
      ).run(enrollment);
    },
    insertInsurancePremium(premium) {
      db.prepare(
        `
        INSERT INTO insurance_pool_premiums (
          lp_address, contract_id, amount, premium_at, ledger, transaction_hash, event_index
        ) VALUES (
          @lp_address, @contract_id, @amount, @premium_at, @ledger, @transaction_hash, @event_index
        )
        ON CONFLICT(lp_address, contract_id, transaction_hash, event_index) DO NOTHING
      `
      ).run(premium);
    },
    insertInsuranceClaim(claim) {
      db.prepare(
        `
        INSERT INTO insurance_pool_claims (
          invoice_id, lp_address, contract_id, payout_amount, claimed_at, ledger, transaction_hash, event_index
        ) VALUES (
          @invoice_id, @lp_address, @contract_id, @payout_amount, @claimed_at, @ledger, @transaction_hash, @event_index
        )
        ON CONFLICT(invoice_id, contract_id, transaction_hash, event_index) DO NOTHING
      `
      ).run(claim);
    },
    getInsurancePoolStats(contractId) {
      return db
        .prepare('SELECT pool_balance, total_premiums_collected, total_claims_paid, enrolled_lp_count FROM insurance_pool_stats WHERE contract_id = ?')
        .get(contractId) as { pool_balance: string; total_premiums_collected: string; total_claims_paid: string; enrolled_lp_count: number } | undefined;
    },
    upsertInsurancePoolStats(contractId, balance, premiums, claims, enrolledCount) {
      db.prepare(
        `
        INSERT INTO insurance_pool_stats (contract_id, pool_balance, total_premiums_collected, total_claims_paid, enrolled_lp_count, last_updated_at)
        VALUES (?, ?, ?, ?, ?, ?)
        ON CONFLICT(contract_id) DO UPDATE SET
          pool_balance = excluded.pool_balance,
          total_premiums_collected = excluded.total_premiums_collected,
          total_claims_paid = excluded.total_claims_paid,
          enrolled_lp_count = excluded.enrolled_lp_count,
          last_updated_at = excluded.last_updated_at
      `
      ).run(contractId, balance, premiums, claims, enrolledCount, Math.floor(Date.now() / 1000));
    },
  };
}
