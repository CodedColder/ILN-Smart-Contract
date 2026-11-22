import { describe, expect, it, vi } from 'vitest';
import type { EventRepository } from '../src/db/eventRepository.js';
import type {
  DecodedContractEvent,
  HorizonTransactionRecord,
} from '../src/ingestion/eventListener.js';
import { EventListener } from '../src/ingestion/eventListener.js';
import type { LedgerReorgDetector, ReorgDivergence } from '../src/ingestion/reorgDetector.js';

class MockRepository implements EventRepository {
  state = new Map<string, string>();
  invoices = new Map<number, any>();
  events: any[] = [];
  reputationUpdates: any[] = [];

  getState(key: string): string | undefined {
    return this.state.get(key);
  }

  setState(key: string, value: string): void {
    this.state.set(key, value);
  }

  getInvoice(id: number): any {
    return this.invoices.get(id);
  }

  upsertInvoice(invoice: any): void {
    this.invoices.set(invoice.id, invoice);
  }

  insertEvent(event: any): void {
    this.events.push(event);
  }

  insertReputationUpdate(update: any): void {
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

function makeStream(messages: unknown[], signal?: AbortSignal): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  return new ReadableStream({
    start(controller) {
      if (signal) {
        signal.addEventListener('abort', () => {
          try {
            controller.close();
          } catch {}
        }, { once: true });
      }
      for (const msg of messages) {
        controller.enqueue(encoder.encode(`data: ${JSON.stringify(msg)}\n\n`));
      }
    },
  });
}

describe('Live stream resync after reorg (Issue #1241)', () => {
  const contractId = 'C_TEST_RESYNC';

  it('skips transactions without persisting when reorgDetector is halted', async () => {
    const repo = new MockRepository();
    repo.setState('last_processed_cursor', 'c100');

    const fakeDetector = {
      isHalted: vi.fn(() => true),
      observe: vi.fn(),
    } as unknown as LedgerReorgDetector;

    const listener = new EventListener({
      repository: repo,
      horizonUrl: 'http://horizon.local',
      contractAddress: contractId,
      reorgDetector: fakeDetector,
    });

    const record: HorizonTransactionRecord = {
      hash: 'tx-fork-1',
      ledger: 105,
      created_at: new Date().toISOString(),
      paging_token: 'c105',
      result_meta_xdr: '',
    };

    await listener.processTransaction(record);

    // No events should be inserted
    expect(repo.events).toHaveLength(0);
    // Cursor should remain unchanged
    expect(repo.getState('last_processed_cursor')).toBe('c100');
  });

  it('reopens stream from recovered cursor after reorg recovery', async () => {
    const repo = new MockRepository();
    repo.setState('last_processed_cursor', 'c100');

    let halted = false;
    const fakeDetector = {
      isHalted: vi.fn(() => halted),
      observe: vi.fn(async (ledger: number) => {
        if (ledger === 105) {
          halted = true;
          return {
            divergenceLedger: 105,
            commonAncestorLedger: 104,
            reason: 'ledger_hash_mismatch',
            detectedBy: 'ingestion',
            detectedAt: new Date().toISOString(),
            indexedHash: 'old-105',
            observedHash: 'new-105',
            observedParentHash: 'h104',
            forkDepth: 1,
            message: 'reorg at 105',
          } as ReorgDivergence;
        }
        return null;
      }),
    } as unknown as LedgerReorgDetector;

    let recoveryExecuted = false;
    const onReorgDetected = vi.fn(async (_div: ReorgDivergence) => {
      recoveryExecuted = true;
      // Recovery rolls back cursor to ancestor
      repo.setState('last_processed_cursor', 'c104');
      halted = false; // Recovery clears the halt latch
    });

    const connectionCursors: string[] = [];
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input));
      const requestedCursor = url.searchParams.get('cursor') || '';
      connectionCursors.push(requestedCursor);

      if (connectionCursors.length === 1) {
        // First connection yields transaction at ledger 105 which triggers reorg
        const records = [
          {
            hash: 'tx-105-fork',
            ledger: 105,
            created_at: new Date().toISOString(),
            paging_token: 'c105-fork',
            result_meta_xdr: '',
          },
          // Second record arriving on same stale stream after recovery
          {
            hash: 'tx-106-fork',
            ledger: 106,
            created_at: new Date().toISOString(),
            paging_token: 'c106-fork',
            result_meta_xdr: '',
          },
        ];
        return new Response(makeStream(records, init?.signal), {
          status: 200,
          headers: { 'content-type': 'text/event-stream' },
        });
      }

      // Second connection resumes from recovered cursor c104
      return new Response(makeStream([], init?.signal), {
        status: 200,
        headers: { 'content-type': 'text/event-stream' },
      });
    });

    const listener = new EventListener({
      repository: repo,
      horizonUrl: 'http://horizon.local',
      contractAddress: contractId,
      fetchImpl: fetchMock as unknown as typeof fetch,
      reorgDetector: fakeDetector,
      onReorgDetected,
      initialBackoffMs: 1,
      maxBackoffMs: 2,
      decodeTransactionEvents: () => [
        {
          contractId,
          rawEventType: 'submitted',
          contractEventType: 'InvoiceSubmitted',
          topics: ['submitted', 1],
          data: { invoice_id: 1 },
        },
      ],
    });

    const startPromise = listener.start();

    // Give it a brief tick to process stream
    await new Promise((resolve) => setTimeout(resolve, 50));
    listener.stop();
    await startPromise;

    expect(recoveryExecuted).toBe(true);
    // Should have reconnected at recovered cursor 'c104'
    expect(connectionCursors).toContain('c100');
    expect(connectionCursors).toContain('c104');
  });
});
