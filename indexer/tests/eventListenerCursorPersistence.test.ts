import { describe, expect, it, vi } from 'vitest';
import type {
  EventRepository,
  IngestedEventRecord,
  InvoiceRecord,
  ReputationUpdateRecord,
} from '../src/db/eventRepository.js';
import type {
  DecodedContractEvent,
  HorizonTransactionRecord,
} from '../src/ingestion/eventListener.js';
import { EventListener } from '../src/ingestion/eventListener.js';

class MockTransactionalRepository implements EventRepository {
  state = new Map<string, string>();
  invoices = new Map<number, InvoiceRecord>();
  events: IngestedEventRecord[] = [];
  reputationUpdates: ReputationUpdateRecord[] = [];

  // Snapshot for transactional rollback simulation
  private inTransaction = false;
  private stateSnapshot = new Map<string, string>();
  private eventsSnapshot: IngestedEventRecord[] = [];

  transaction<T>(fn: () => T): T {
    this.inTransaction = true;
    this.stateSnapshot = new Map(this.state);
    this.eventsSnapshot = [...this.events];
    try {
      const result = fn();
      this.inTransaction = false;
      return result;
    } catch (err) {
      this.state = new Map(this.stateSnapshot);
      this.events = [...this.eventsSnapshot];
      this.inTransaction = false;
      throw err;
    }
  }

  getState(key: string): string | undefined {
    return this.state.get(key);
  }

  setState(key: string, value: string): void {
    this.state.set(key, value);
  }

  getInvoice(id: number): InvoiceRecord | undefined {
    return this.invoices.get(id);
  }

  upsertInvoice(invoice: InvoiceRecord): void {
    this.invoices.set(invoice.id, invoice);
  }

  insertEvent(event: IngestedEventRecord): void {
    this.events.push(event);
  }

  insertReputationUpdate(update: ReputationUpdateRecord): void {
    this.reputationUpdates.push(update);
  }

  insertInsuranceEnrollment(): void {}
  insertInsurancePremium(): void {}
  insertInsuranceClaim(): void {}
  getInsurancePoolStats(): undefined {
    return undefined;
  }
  upsertInsurancePoolStats(): void {}
}

describe('EventListener cursor persistence audit (Issue #1238)', () => {
  const contractId = 'C_TEST_CONTRACT';

  function createRecord(seq: number, token: string): HorizonTransactionRecord {
    return {
      hash: `tx-${seq}`,
      ledger: seq,
      created_at: new Date().toISOString(),
      paging_token: token,
      result_meta_xdr: '',
    };
  }

  function createMockEvents(contract: string, count: number): DecodedContractEvent[] {
    const events: DecodedContractEvent[] = [];
    for (let i = 0; i < count; i++) {
      events.push({
        contractId: contract,
        rawEventType: 'submitted',
        contractEventType: 'InvoiceSubmitted',
        topics: ['submitted', i + 1],
        data: {
          invoice_id: i + 1,
          freelancer: 'G_FREELANCER',
          payer: 'G_PAYER',
          token: 'G_TOKEN',
          amount: '1000',
          due_date: 500,
          discount_rate: 100,
          status: 'Pending',
        },
      });
    }
    return events;
  }

  it('does not advance cursor or ledger when an event fails to persist mid-transaction', async () => {
    const repo = new MockTransactionalRepository();
    repo.setState('last_processed_cursor', 'cursor-100');
    repo.setState('last_processed_ledger', '100');

    const multiEvents = createMockEvents(contractId, 3);
    const record = createRecord(101, 'cursor-101');

    // Make the second event fail during persistence
    let insertCount = 0;
    const originalInsertEvent = repo.insertEvent.bind(repo);
    repo.insertEvent = (event) => {
      insertCount++;
      if (insertCount === 2) {
        throw new Error('Database disk full or constraint violation');
      }
      originalInsertEvent(event);
    };

    const listener = new EventListener({
      repository: repo,
      horizonUrl: 'http://horizon.local',
      contractAddress: contractId,
      decodeTransactionEvents: () => multiEvents,
    });

    await expect(listener.processTransaction(record)).rejects.toThrow(
      'Database disk full or constraint violation'
    );

    // Assert that the cursor was NOT advanced to cursor-101
    expect(repo.getState('last_processed_cursor')).toBe('cursor-100');
    expect(repo.getState('last_processed_ledger')).toBe('100');
    // Assert that transactional rollback cleared any partial events
    expect(repo.events).toHaveLength(0);
  });

  it('advances cursor and ledger state strictly after all events are persisted', async () => {
    const repo = new MockTransactionalRepository();
    repo.setState('last_processed_cursor', 'cursor-100');
    repo.setState('last_processed_ledger', '100');

    const multiEvents = createMockEvents(contractId, 2);
    const record = createRecord(101, 'cursor-101');

    const executionOrder: string[] = [];
    const origInsert = repo.insertEvent.bind(repo);
    repo.insertEvent = (event) => {
      executionOrder.push('insert_event');
      origInsert(event);
    };
    const origSetState = repo.setState.bind(repo);
    repo.setState = (key, val) => {
      executionOrder.push(`set_state_${key}`);
      origSetState(key, val);
    };

    const listener = new EventListener({
      repository: repo,
      horizonUrl: 'http://horizon.local',
      contractAddress: contractId,
      decodeTransactionEvents: () => multiEvents,
    });

    await listener.processTransaction(record);

    expect(executionOrder).toEqual([
      'insert_event',
      'insert_event',
      'set_state_last_processed_cursor',
      'set_state_last_processed_ledger',
    ]);
    expect(repo.getState('last_processed_cursor')).toBe('cursor-101');
    expect(repo.getState('last_processed_ledger')).toBe('101');
    expect(repo.events).toHaveLength(2);
  });

  it('does not touch cursor or ledger state when persistLiveCursor is false', async () => {
    const repo = new MockTransactionalRepository();
    repo.setState('last_processed_cursor', 'cursor-100');
    repo.setState('last_processed_ledger', '100');

    const events = createMockEvents(contractId, 1);
    const record = createRecord(101, 'cursor-101');

    const listener = new EventListener({
      repository: repo,
      horizonUrl: 'http://horizon.local',
      contractAddress: contractId,
      decodeTransactionEvents: () => events,
      persistLiveCursor: false,
    });

    await listener.processTransaction(record);

    expect(repo.events).toHaveLength(1);
    expect(repo.getState('last_processed_cursor')).toBe('cursor-100');
    expect(repo.getState('last_processed_ledger')).toBe('100');
  });
});
