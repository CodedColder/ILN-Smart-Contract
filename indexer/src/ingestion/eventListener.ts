import { humanizeEvents, xdr } from '@stellar/stellar-sdk';
import type {
  EventRepository,
  InvoiceRecord,
  IngestedEventRecord,
  ReputationUpdateRecord,
  InsurancePoolEnrollmentRecord,
  InsurancePoolPremiumRecord,
  InsurancePoolClaimRecord,
} from '../db/eventRepository.js';
import type { LedgerReorgDetector, ReorgDivergence } from './reorgDetector.js';

export interface HorizonTransactionRecord {
  hash: string;
  ledger: number;
  created_at: string;
  paging_token: string;
  result_meta_xdr: string;
}

export interface DecodedContractEvent {
  contractId: string | null;
  rawEventType: string;
  contractEventType: string;
  topics: unknown[];
  data: unknown;
}

export interface EventListenerOptions {
  repository: EventRepository;
  horizonUrl: string;
  contractAddress: string;
  fetchImpl?: typeof fetch;
  decodeTransactionEvents?: (record: HorizonTransactionRecord) => DecodedContractEvent[];
  logger?: Pick<Console, 'info' | 'warn' | 'error'>;
  initialBackoffMs?: number;
  maxBackoffMs?: number;
  /**
   * Ledger continuity verification (Issue #863). When present, every ledger
   * is checked against stored history *before* its transactions are written;
   * a mismatch latches the detector's halt and ingestion stops persisting.
   */
  reorgDetector?: LedgerReorgDetector;
  /**
   * Invoked once per detected divergence so the caller can run the shared
   * rollback-and-replay recovery (Issue #864). Omit to only flag the
   * divergence — the halt latch stays set until something clears it.
   */
  onReorgDetected?: (divergence: ReorgDivergence) => Promise<void>;
}

const DEFAULT_INITIAL_BACKOFF_MS = 1_000;
const DEFAULT_MAX_BACKOFF_MS = 30_000;
const LAST_CURSOR_STATE_KEY = 'last_processed_cursor';
const LAST_LEDGER_STATE_KEY = 'last_processed_ledger';

/**
 * Control-flow signal: reorg recovery rebuilt state and the SSE stream must
 * be re-opened from the recovered cursor. Thrown from `processTransaction`,
 * caught by `start()`; never escapes the ingestion loop.
 */
class StreamResyncSignal extends Error {
  constructor() {
    super('stream resync after reorg recovery');
    this.name = 'StreamResyncSignal';
  }
}

export class EventListener {
  private readonly repository: EventRepository;
  private readonly horizonUrl: string;
  private readonly contractAddress: string;
  private readonly fetchImpl: typeof fetch;
  private readonly decodeTransactionEvents: (record: HorizonTransactionRecord) => DecodedContractEvent[];
  private readonly logger: Pick<Console, 'info' | 'warn' | 'error'>;
  private readonly initialBackoffMs: number;
  private readonly maxBackoffMs: number;
  private readonly reorgDetector: LedgerReorgDetector | undefined;
  private readonly onReorgDetected: ((divergence: ReorgDivergence) => Promise<void>) | undefined;
  private stopped = false;
  /**
   * Set whenever a record was skipped because ingestion was halted for a
   * reorg: the records that arrived while halted are dropped (replay
   * rebuilds them), so the stream must reconnect from the recovered cursor
   * once ingestion resumes rather than continue on a connection that has
   * already skipped ahead.
   */
  private resyncPending = false;
  private activeAbortController: AbortController | null = null;

  constructor(options: EventListenerOptions) {
    this.repository = options.repository;
    this.horizonUrl = options.horizonUrl.replace(/\/$/, '');
    this.contractAddress = options.contractAddress;
    this.fetchImpl = options.fetchImpl ?? fetch;
    this.decodeTransactionEvents = options.decodeTransactionEvents ?? decodeTransactionEventsFromMeta;
    this.logger = options.logger ?? console;
    this.initialBackoffMs = options.initialBackoffMs ?? DEFAULT_INITIAL_BACKOFF_MS;
    this.maxBackoffMs = options.maxBackoffMs ?? DEFAULT_MAX_BACKOFF_MS;
    this.reorgDetector = options.reorgDetector;
    this.onReorgDetected = options.onReorgDetected;
  }

  stop(): void {
    this.stopped = true;
    this.activeAbortController?.abort();
  }

  async start(): Promise<void> {
    // Restartable: a lost ingestion lease stops the loop, and the next
    // leadership grant must resume it from the persisted cursor.
    this.stopped = false;
    let backoffMs = this.initialBackoffMs;
    let cursor = this.repository.getState(LAST_CURSOR_STATE_KEY) || 'now';

    while (!this.stopped) {
      const controller = new AbortController();
      this.activeAbortController = controller;

      try {
        this.logger.info(`indexer ingestion connected to Horizon transactions stream at cursor ${cursor}`);
        await this.consumeStream(cursor, controller.signal);
        backoffMs = this.initialBackoffMs;
        cursor = this.repository.getState(LAST_CURSOR_STATE_KEY) || cursor;
      } catch (error) {
        if (this.stopped) {
          break;
        }

        if (error instanceof StreamResyncSignal) {
          // Recovery is done and the latch is clear: reconnect immediately
          // from the recovered cursor so nothing between the last written
          // record and the chain tip is skipped.
          backoffMs = this.initialBackoffMs;
          cursor = this.repository.getState(LAST_CURSOR_STATE_KEY) || cursor;
          this.logger.info(
            `indexer ingestion resuming at cursor ${cursor} after reorg recovery`
          );
          continue;
        }

        const message = error instanceof Error ? error.message : String(error);
        this.logger.warn(`indexer ingestion stream error: ${message}`);
        await delay(backoffMs, controller.signal).catch(() => undefined);
        backoffMs = Math.min(backoffMs * 2, this.maxBackoffMs);
        cursor = this.repository.getState(LAST_CURSOR_STATE_KEY) || cursor;
      } finally {
        if (this.activeAbortController === controller) {
          this.activeAbortController = null;
        }
      }
    }
  }

  private async consumeStream(cursor: string, signal: AbortSignal): Promise<void> {
    const url = new URL('/transactions', this.horizonUrl);
    url.searchParams.set('cursor', cursor);
    url.searchParams.set('order', 'asc');
    url.searchParams.set('limit', '200');

    const response = await this.fetchImpl(url, {
      headers: {
        Accept: 'text/event-stream',
      },
      signal,
    });

    if (!response.ok) {
      throw new Error(`Horizon responded with HTTP ${response.status}`);
    }

    if (!response.body) {
      throw new Error('Horizon stream did not include a response body');
    }

    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';

    while (!this.stopped) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }

      buffer += decoder.decode(value, { stream: true });

      let boundary = buffer.indexOf('\n\n');
      while (boundary >= 0) {
        const chunk = buffer.slice(0, boundary);
        buffer = buffer.slice(boundary + 2);
        await this.handleSseChunk(chunk);
        boundary = buffer.indexOf('\n\n');
      }
    }
  }

  private async handleSseChunk(chunk: string): Promise<void> {
    const lines = chunk.split(/\r?\n/);
    const dataLines = lines
      .filter((line) => line.startsWith('data:'))
      .map((line) => line.slice(5).trimStart());

    if (dataLines.length === 0) {
      return;
    }

    const rawPayload = dataLines.join('\n');
    if (!rawPayload || rawPayload === '[DONE]') {
      return;
    }

    const record = JSON.parse(rawPayload) as HorizonTransactionRecord;
    await this.processTransaction(record);
  }

  /**
   * Decode, persist, and checkpoint a single Horizon transaction record.
   * Public so the replay runner can re-process historical transactions with
   * identical semantics to live ingestion (idempotent upserts).
   *
   * When a reorg detector is attached, the record's ledger is verified
   * against stored history first (Issue #863): a divergence is logged as
   * `ReorgDetected`, flagged in `indexer_state`, and handed to
   * `onReorgDetected` for rollback-and-replay (Issue #864) instead of being
   * written on top of the now-invalid chain.
   */
  async processTransaction(record: HorizonTransactionRecord): Promise<void> {
    // Fast path: another process/rotation already flagged this divergence.
    if (this.reorgDetector?.isHalted()) {
      this.resyncPending = true;
      return;
    }

    if (this.reorgDetector) {
      const divergence = await this.reorgDetector.observe(record.ledger);
      if (divergence) {
        // Marked before recovery so the first successfully-written record
        // after recovery reopens the stream from the rebuilt cursor.
        this.resyncPending = true;
        await this.handleReorg(divergence);
        return;
      }
      // The header read above is an await point: recovery may have latched
      // the halt in the meantime (concurrent consistency-job run).
      if (this.reorgDetector.isHalted()) {
        this.resyncPending = true;
        return;
      }
    }

    let processedSuccessfully = false;

    try {
      const decodedEvents = this.decodeTransactionEvents(record).filter(
        (event) => event.contractId === this.contractAddress
      );

      for (let index = 0; index < decodedEvents.length; index += 1) {
        const event = decodedEvents[index];
        const payload = toRecordPayload(normalizeJsonValue(event.data));
        const invoiceId = coerceNullableNumber(payload.invoice_id);
        const existingInvoice = invoiceId === null ? undefined : this.repository.getInvoice(invoiceId);
        const processed = normalizeProcessedEvent(event, record, index, existingInvoice, payload);

        // Upsert the parent invoice row BEFORE the child event row so the
        // events.invoice_id -> invoices(id) foreign key holds when foreign
        // keys are enforced (production enables them via getDb()).
        if (processed.invoiceRow) {
          this.repository.upsertInvoice(processed.invoiceRow);
        }

        if (processed.reputationUpdateRow) {
          this.repository.insertReputationUpdate(processed.reputationUpdateRow);
        }

        if (processed.insuranceEnrollmentRow) {
          this.repository.insertInsuranceEnrollment(processed.insuranceEnrollmentRow);
        }

        if (processed.insurancePremiumRow) {
          this.repository.insertInsurancePremium(processed.insurancePremiumRow);
        }

        if (processed.insuranceClaimRow) {
          this.repository.insertInsuranceClaim(processed.insuranceClaimRow);
        }

        this.repository.insertEvent(processed.rawEvent);

        this.logger.info(
          `processed ${processed.rawEvent.contract_event_type ?? processed.rawEvent.event_type} at ledger ${processed.rawEvent.ledger} (tx ${record.hash})`
        );
      }

      processedSuccessfully = true;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.logger.error(`failed to process transaction ${record.hash}: ${message}`);
    }

    if (processedSuccessfully) {
      this.repository.setState(LAST_CURSOR_STATE_KEY, record.paging_token);
      this.repository.setState(LAST_LEDGER_STATE_KEY, String(record.ledger));

      // Records skipped while halted were dropped from this connection;
      // recovery re-derived them, so reopen the stream from the checkpoint
      // rather than trusting a connection that skipped ahead. Gated on
      // owning recovery: replay runs the same code without a recovery hook
      // and must not be interrupted mid-replay.
      if (this.resyncPending && this.onReorgDetected) {
        this.resyncPending = false;
        throw new StreamResyncSignal();
      }
    }
  }

  /**
   * Emit the `ReorgDetected` entry for a flagged divergence and hand it to
   * the recovery callback. The detector has already latched the halt, so the
   * stream can keep running — nothing further is persisted until recovery
   * clears the latch (or the callback is deliberately omitted and the
   * divergence stays flagged for an operator/consistency job to resolve).
   */
  private async handleReorg(divergence: ReorgDivergence): Promise<void> {
    this.logger.error(
      `ReorgDetected ledger=${divergence.divergenceLedger} reason=${divergence.reason} ` +
        `fork=${divergence.commonAncestorLedger} detectedBy=${divergence.detectedBy}`
    );
    this.logger.error(divergence.message);

    if (!this.onReorgDetected) {
      return;
    }

    try {
      await this.onReorgDetected(divergence);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.logger.error(
        `reorg recovery failed at ledger ${divergence.divergenceLedger}: ${message}`
      );
      // The halt latch stays set on failure: ingestion remains blocked until
      // a recovery succeeds rather than resuming on a broken chain.
    }
  }
}

export function createEventListener(options: EventListenerOptions): EventListener {
  return new EventListener(options);
}

export function decodeTransactionEventsFromMeta(
  record: HorizonTransactionRecord
): DecodedContractEvent[] {
  const meta = xdr.TransactionMeta.fromXDR(record.result_meta_xdr, 'base64');
  const contractEvents = extractContractEvents(meta);
  return humanizeEvents(contractEvents as any)
    .filter((event) => event.type === 'contract')
    .map((event) => {
      const topics = event.topics ?? [];
      const rawEventType = String(topics[0] ?? '');
      return {
        contractId: event.contractId ?? null,
        rawEventType,
        contractEventType: canonicalEventType(rawEventType),
        topics,
        data: event.data,
      };
    });
}

function extractContractEvents(
  meta: ReturnType<typeof xdr.TransactionMeta.fromXDR>
): Array<xdr.ContractEvent | xdr.TransactionEvent> {
  switch (meta.switch()) {
    case 3: {
      const transactionMeta = meta.value() as xdr.TransactionMetaV3;
      const sorobanMeta = transactionMeta.sorobanMeta();
      return (sorobanMeta?.events() ?? []) as Array<xdr.ContractEvent | xdr.TransactionEvent>;
    }
    case 4: {
      const transactionMeta = meta.value() as xdr.TransactionMetaV4;
      return (transactionMeta.events() ?? []) as Array<xdr.ContractEvent | xdr.TransactionEvent>;
    }
    default:
      return [];
  }
}

function normalizeProcessedEvent(
  event: DecodedContractEvent,
  record: HorizonTransactionRecord,
  eventIndex: number,
  existingInvoice: InvoiceRecord | undefined,
  payload: Record<string, unknown>
): {
  rawEvent: IngestedEventRecord;
  invoiceRow?: InvoiceRecord;
  reputationUpdateRow?: ReputationUpdateRecord;
  insuranceEnrollmentRow?: InsurancePoolEnrollmentRecord;
  insurancePremiumRow?: InsurancePoolPremiumRecord;
  insuranceClaimRow?: InsurancePoolClaimRecord;
} {
  const normalizedData = normalizeJsonValue(event.data);
  const topicsJson = JSON.stringify(normalizeJsonValue(event.topics));
  const txTimestamp = parseRecordTimestamp(record);
  const rawEventType = event.rawEventType || inferRawEventType(event.contractEventType);
  const contractEventType = event.contractEventType || canonicalEventType(rawEventType);
  const ledger = record.ledger;
  const common = {
    contract_id: event.contractId,
    contract_event_type: contractEventType,
    transaction_hash: record.hash,
    transaction_paging_token: record.paging_token,
    event_index: eventIndex,
    topics_json: topicsJson,
  };

  switch (contractEventType) {
    case 'InvoiceSubmitted': {
      const invoiceRow: InvoiceRecord = {
        id: coerceNumber(payload.invoice_id),
        freelancer: coerceString(payload.freelancer, existingInvoice?.freelancer ?? ''),
        payer: coerceString(payload.payer, existingInvoice?.payer ?? ''),
        token: coerceString(payload.token, existingInvoice?.token ?? ''),
        amount: coerceString(payload.amount, existingInvoice?.amount ?? '0'),
        due_date: coerceNumber(payload.due_date, existingInvoice?.due_date ?? txTimestamp),
        discount_rate: coerceNumber(payload.discount_rate, existingInvoice?.discount_rate ?? 0),
        status: coerceString(payload.status, existingInvoice?.status ?? 'Pending'),
        funder: existingInvoice?.funder ?? null,
        funded_at: existingInvoice?.funded_at ?? null,
        amount_funded: coerceString(payload.amount_funded, existingInvoice?.amount_funded ?? '0'),
        amount_paid: coerceString(payload.amount_paid, existingInvoice?.amount_paid ?? '0'),
        referral_code: serializeNullableText(payload.referral_code) ?? existingInvoice?.referral_code ?? null,
        submitter_reputation: coerceNumber(payload.submitter_reputation, existingInvoice?.submitter_reputation ?? 0),
        created_at: coerceNumber(payload.timestamp, existingInvoice?.created_at ?? txTimestamp),
      };

      return {
        rawEvent: {
          invoice_id: invoiceRow.id,
          event_type: rawEventType,
          ledger,
          timestamp: invoiceRow.created_at,
          data: JSON.stringify(normalizedData),
          ...common,
        },
        invoiceRow,
      };
    }
    case 'InvoiceFunded': {
      const invoiceRow: InvoiceRecord = {
        id: coerceNumber(payload.invoice_id),
        freelancer: coerceString(payload.freelancer, existingInvoice?.freelancer ?? ''),
        payer: coerceString(payload.payer, existingInvoice?.payer ?? ''),
        token: coerceString(payload.token, existingInvoice?.token ?? ''),
        amount: coerceString(payload.invoice_amount ?? payload.amount, existingInvoice?.amount ?? '0'),
        due_date: coerceNumber(payload.due_date, existingInvoice?.due_date ?? txTimestamp),
        discount_rate: coerceNumber(payload.discount_rate, existingInvoice?.discount_rate ?? 0),
        status: coerceString(payload.status, existingInvoice?.status ?? 'Funded'),
        funder: coerceNullableString(payload.funder ?? payload.lp) ?? existingInvoice?.funder ?? null,
        funded_at: coerceNullableNumber(payload.funded_at, existingInvoice?.funded_at ?? txTimestamp),
        amount_funded: coerceString(payload.amount_funded ?? payload.fund_amount, existingInvoice?.amount_funded ?? '0'),
        amount_paid: existingInvoice?.amount_paid ?? '0',
        referral_code: existingInvoice?.referral_code ?? null,
        submitter_reputation: existingInvoice?.submitter_reputation ?? 0,
        created_at: existingInvoice?.created_at ?? txTimestamp,
      };

      return {
        rawEvent: {
          invoice_id: invoiceRow.id,
          event_type: rawEventType,
          ledger,
          timestamp: coerceNumber(payload.timestamp, txTimestamp),
          data: JSON.stringify(normalizedData),
          ...common,
        },
        invoiceRow,
      };
    }
    case 'InvoicePaid': {
      const invoiceRow: InvoiceRecord = {
        id: coerceNumber(payload.invoice_id),
        freelancer: coerceString(payload.freelancer, existingInvoice?.freelancer ?? ''),
        payer: coerceString(payload.payer, existingInvoice?.payer ?? ''),
        token: coerceString(payload.token, existingInvoice?.token ?? ''),
        amount: coerceString(payload.amount_paid ?? payload.invoice_amount, existingInvoice?.amount ?? '0'),
        due_date: existingInvoice?.due_date ?? txTimestamp,
        discount_rate: existingInvoice?.discount_rate ?? 0,
        status: coerceString(payload.status, existingInvoice?.status ?? 'Paid'),
        funder: coerceNullableString(payload.lp) ?? existingInvoice?.funder ?? null,
        funded_at: existingInvoice?.funded_at ?? null,
        amount_funded: coerceString(payload.lp_payout ?? payload.amount_paid, existingInvoice?.amount_funded ?? '0'),
        amount_paid: coerceString(payload.amount_paid, existingInvoice?.amount_paid ?? '0'),
        referral_code: existingInvoice?.referral_code ?? null,
        submitter_reputation: existingInvoice?.submitter_reputation ?? 0,
        created_at: existingInvoice?.created_at ?? txTimestamp,
      };

      return {
        rawEvent: {
          invoice_id: invoiceRow.id,
          event_type: rawEventType,
          ledger,
          timestamp: coerceNumber(payload.settlement_timestamp, txTimestamp),
          data: JSON.stringify(normalizedData),
          ...common,
        },
        invoiceRow,
      };
    }
    case 'InvoiceCancelled':
    case 'InvoiceExpired':
    case 'InvoiceDisputed': {
      const invoiceRow: InvoiceRecord = {
        id: coerceNumber(payload.invoice_id),
        freelancer: coerceString(payload.freelancer, existingInvoice?.freelancer ?? ''),
        payer: coerceString(payload.payer, existingInvoice?.payer ?? ''),
        token: coerceString(payload.token, existingInvoice?.token ?? ''),
        amount: coerceString(payload.amount, existingInvoice?.amount ?? '0'),
        due_date: coerceNumber(payload.due_date, existingInvoice?.due_date ?? txTimestamp),
        discount_rate: coerceNumber(payload.discount_rate, existingInvoice?.discount_rate ?? 0),
        status: coerceString(payload.status, existingInvoice?.status ?? contractEventType.replace(/^Invoice/, '')),
        funder: coerceNullableString(payload.funder) ?? existingInvoice?.funder ?? null,
        funded_at: coerceNullableNumber(payload.funded_at, existingInvoice?.funded_at ?? null),
        amount_funded: coerceString(payload.amount_funded, existingInvoice?.amount_funded ?? '0'),
        amount_paid: coerceString(payload.amount_paid, existingInvoice?.amount_paid ?? '0'),
        referral_code: existingInvoice?.referral_code ?? null,
        submitter_reputation: existingInvoice?.submitter_reputation ?? 0,
        created_at: existingInvoice?.created_at ?? txTimestamp,
      };

      return {
        rawEvent: {
          invoice_id: invoiceRow.id,
          event_type: rawEventType,
          ledger,
          timestamp: coerceNumber(payload.timestamp ?? payload.disputed_at ?? payload.defaulted_at, txTimestamp),
          data: JSON.stringify(normalizedData),
          ...common,
        },
        invoiceRow,
      };
    }
    case 'ReputationUpdated': {
      const reputationUpdateRow: ReputationUpdateRecord = {
        address: coerceString(payload.address),
        event_type: rawEventType,
        old_score: coerceNumber(payload.old_score, 0),
        new_score: coerceNumber(payload.new_score, 0),
        invoices_submitted: coerceNumber(payload.invoices_submitted, 0),
        invoices_paid: coerceNumber(payload.invoices_paid, 0),
        invoices_defaulted: coerceNumber(payload.invoices_defaulted, 0),
        ledger,
        timestamp: txTimestamp,
        contract_id: event.contractId,
        transaction_hash: record.hash,
        transaction_paging_token: record.paging_token,
        event_index: eventIndex,
        topics_json: topicsJson,
      };

      return {
        rawEvent: {
          invoice_id: null,
          event_type: rawEventType,
          ledger,
          timestamp: txTimestamp,
          data: JSON.stringify(normalizedData),
          ...common,
        },
        reputationUpdateRow,
      };
    }
    // Insurance pool events (Issue #695)
    case 'InsurancePoolEnrolled': {
      const lp = coerceString(payload.lp, '');
      const enrollmentRow: InsurancePoolEnrollmentRecord = {
        lp_address: lp,
        contract_id: common.contract_id ?? '',
        enrolled_at: txTimestamp,
        ledger,
        transaction_hash: record.hash,
        event_index: eventIndex,
      };

      return {
        rawEvent: {
          invoice_id: null,
          event_type: rawEventType,
          ledger,
          timestamp: txTimestamp,
          data: JSON.stringify(normalizedData),
          ...common,
        },
        insuranceEnrollmentRow: enrollmentRow,
      };
    }
    case 'InsurancePoolPremiumDeposited': {
      const lp = coerceString(payload.lp, '');
      const amount = coerceString(payload.amount, '0');
      const premiumRow: InsurancePoolPremiumRecord = {
        lp_address: lp,
        contract_id: common.contract_id ?? '',
        amount,
        premium_at: txTimestamp,
        ledger,
        transaction_hash: record.hash,
        event_index: eventIndex,
      };

      return {
        rawEvent: {
          invoice_id: null,
          event_type: rawEventType,
          ledger,
          timestamp: txTimestamp,
          data: JSON.stringify(normalizedData),
          ...common,
        },
        insurancePremiumRow: premiumRow,
      };
    }
    case 'InsurancePoolClaimed': {
      const invoiceId = coerceNumber(payload.invoice_id, 0);
      const lp = coerceString(payload.lp, '');
      const payout = coerceString(payload.payout, '0');
      const claimRow: InsurancePoolClaimRecord = {
        invoice_id: invoiceId,
        lp_address: lp,
        contract_id: common.contract_id ?? '',
        payout_amount: payout,
        claimed_at: txTimestamp,
        ledger,
        transaction_hash: record.hash,
        event_index: eventIndex,
      };

      return {
        rawEvent: {
          invoice_id: invoiceId > 0 ? invoiceId : null,
          event_type: rawEventType,
          ledger,
          timestamp: txTimestamp,
          data: JSON.stringify(normalizedData),
          ...common,
        },
        insuranceClaimRow: claimRow,
      };
    }
    default: {
      return {
        rawEvent: {
          invoice_id: coerceNullableNumber(payload.invoice_id),
          event_type: rawEventType,
          ledger,
          timestamp: coerceNumber(payload.timestamp, txTimestamp),
          data: JSON.stringify(normalizedData),
          ...common,
        },
      };
    }
  }
}

function normalizeJsonValue(value: unknown): unknown {
  if (typeof value === 'bigint') {
    return value.toString();
  }

  if (Array.isArray(value)) {
    return value.map((entry) => normalizeJsonValue(entry));
  }

  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>).map(([key, entry]) => [
        key,
        normalizeJsonValue(entry),
      ])
    );
  }

  return value;
}

function toRecordPayload(value: unknown): Record<string, unknown> {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    return value as Record<string, unknown>;
  }

  return {};
}

function coerceString(value: unknown, fallback = ''): string {
  if (typeof value === 'string') {
    return value;
  }

  if (typeof value === 'bigint' || typeof value === 'number' || typeof value === 'boolean') {
    return String(value);
  }

  return fallback;
}

function coerceNullableString(value: unknown): string | null {
  if (value === null || value === undefined) {
    return null;
  }

  const text = coerceString(value, '');
  return text.length > 0 ? text : null;
}

function coerceNumber(value: unknown, fallback = 0): number {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value;
  }

  if (typeof value === 'bigint') {
    return Number(value);
  }

  if (typeof value === 'string' && value.trim().length > 0) {
    const parsed = Number(value);
    if (Number.isFinite(parsed)) {
      return parsed;
    }
  }

  return fallback;
}

function coerceNullableNumber(value: unknown, fallback: number | null = null): number | null {
  if (value === null || value === undefined) {
    return fallback;
  }

  return coerceNumber(value, fallback ?? 0);
}

function serializeNullableText(value: unknown): string | null {
  if (value === null || value === undefined) {
    return null;
  }

  if (typeof value === 'string') {
    return value;
  }

  if (typeof value === 'object') {
    return JSON.stringify(normalizeJsonValue(value));
  }

  return String(value);
}

function parseRecordTimestamp(record: HorizonTransactionRecord): number {
  const parsed = Date.parse(record.created_at);
  if (Number.isFinite(parsed)) {
    return Math.floor(parsed / 1000);
  }

  return record.ledger;
}

function canonicalEventType(rawEventType: string): string {
  const normalized = rawEventType.trim();

  switch (normalized) {
    case 'submitted':
      return 'InvoiceSubmitted';
    case 'updated':
      return 'InvoiceUpdated';
    case 'funded':
      return 'InvoiceFunded';
    case 'paid':
      return 'InvoicePaid';
    case 'partially_paid':
      return 'InvoicePartiallyPaid';
    case 'transferred':
      return 'InvoiceTransferred';
    case 'lp_position_transferred':
      return 'LPPositionTransferred';
    case 'cancelled':
      return 'InvoiceCancelled';
    case 'defaulted':
      return 'InvoiceDefaulted';
    case 'disputed':
      return 'InvoiceDisputed';
    case 'expired':
      return 'InvoiceExpired';
    case 'reputation_updated':
      return 'ReputationUpdated';
    case 'paused':
      return 'ContractPaused';
    case 'unpaused':
      return 'ContractUnpaused';
    case 'admin_changed':
      return 'AdminChanged';
    case 'parameter_updated':
      return 'ParameterUpdated';
    case 'contract_upgraded':
      return 'ContractUpgraded';
    // Insurance pool events (Issue #695)
    case 'init':
      return 'InsurancePoolInitialized';
    case 'enrolled':
      return 'InsurancePoolEnrolled';
    case 'premium':
      return 'InsurancePoolPremiumDeposited';
    case 'claimed':
      return 'InsurancePoolClaimed';
    case 'cov_prop':
      return 'InsuranceCoverageProposed';
    case 'cov_exec':
      return 'InsuranceCoverageExecuted';
    case 'cov_cncl':
      return 'InsuranceCoverageCancelled';
    case 'adm_prop':
      return 'InsuranceAdminProposed';
    case 'adm_exec':
      return 'InsuranceAdminExecuted';
    case 'adm_cncl':
      return 'InsuranceAdminCancelled';
    case 'prem_gov':
      return 'InsurancePremiumRateGovernance';
    case 'cap_set':
      return 'InsuranceBalanceCapSet';
    // TWAP Oracle events
    case 'twap_enabled':
      return 'TwapEnabledForFeed';
    case 'twap_window_updated':
      return 'TwapWindowUpdated';
    case 'twap_insufficient_data':
      return 'TwapInsufficientData';
    default:
      return normalized
        .split('_')
        .map((part) => {
          if (part.toLowerCase() === 'lp') {
            return 'LP';
          }
          return part ? part[0].toUpperCase() + part.slice(1) : '';
        })
        .join('');
  }
}

function inferRawEventType(contractEventType: string): string {
  const mapping: Record<string, string> = {
    InvoiceSubmitted: 'submitted',
    InvoiceUpdated: 'updated',
    InvoiceFunded: 'funded',
    InvoicePaid: 'paid',
    InvoicePartiallyPaid: 'partially_paid',
    InvoiceTransferred: 'transferred',
    LPPositionTransferred: 'lp_position_transferred',
    InvoiceCancelled: 'cancelled',
    InvoiceDefaulted: 'defaulted',
    InvoiceDisputed: 'disputed',
    InvoiceExpired: 'expired',
    ReputationUpdated: 'reputation_updated',
    ContractPaused: 'paused',
    ContractUnpaused: 'unpaused',
    AdminChanged: 'admin_changed',
    ParameterUpdated: 'parameter_updated',
    ContractUpgraded: 'contract_upgraded',
    // Insurance pool events (Issue #695)
    InsurancePoolInitialized: 'init',
    InsurancePoolEnrolled: 'enrolled',
    InsurancePoolPremiumDeposited: 'premium',
    InsurancePoolClaimed: 'claimed',
    InsuranceCoverageProposed: 'cov_prop',
    InsuranceCoverageExecuted: 'cov_exec',
    InsuranceCoverageCancelled: 'cov_cncl',
    InsuranceAdminProposed: 'adm_prop',
    InsuranceAdminExecuted: 'adm_exec',
    InsuranceAdminCancelled: 'adm_cncl',
    InsurancePremiumRateGovernance: 'prem_gov',
    InsuranceBalanceCapSet: 'cap_set',
    TwapEnabledForFeed: 'twap_enabled',
    TwapWindowUpdated: 'twap_window_updated',
    TwapInsufficientData: 'twap_insufficient_data',
  };

  return mapping[contractEventType] ?? contractEventType.toLowerCase();
}

async function delay(ms: number, signal: AbortSignal): Promise<void> {
  if (ms <= 0) {
    return;
  }

  await new Promise<void>((resolve, reject) => {
    const timeout = setTimeout(() => {
      signal.removeEventListener('abort', onAbort);
      resolve();
    }, ms);

    const onAbort = () => {
      clearTimeout(timeout);
      reject(new Error('aborted'));
    };

    signal.addEventListener('abort', onAbort, { once: true });
  });
}
