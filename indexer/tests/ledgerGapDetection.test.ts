import { describe, expect, it, vi } from 'vitest';
import {
  createHorizonLedgerHeaderSource,
  createStaticLedgerHeaderSource,
  type LedgerHeader,
} from '../src/ingestion/ledgerHeaders.js';
import {
  IngestionCatchup,
  CatchupError,
  catchupCheckpointKey,
} from '../src/ingestion/catchup.js';
import type { EventRepository } from '../src/db/eventRepository.js';

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

describe('Ledger gap detection and outage handling tests (Issue #1239)', () => {
  describe('createHorizonLedgerHeaderSource', () => {
    it('throws when ledger is missing (HTTP 404)', async () => {
      const fetchImpl = vi.fn(async () => ({
        ok: false,
        status: 404,
        json: async () => ({}),
      })) as unknown as typeof fetch;

      const source = createHorizonLedgerHeaderSource('http://horizon.local', fetchImpl);
      await expect(source.getLedgerHeader(500)).rejects.toThrow(
        'Horizon ledger 500 read failed: HTTP 404'
      );
    });

    it('throws on source outage (HTTP 500/503 or network failure)', async () => {
      const fetchOutage = vi.fn(async () => ({
        ok: false,
        status: 503,
        json: async () => ({}),
      })) as unknown as typeof fetch;

      const source = createHorizonLedgerHeaderSource('http://horizon.local', fetchOutage);
      await expect(source.getLedgerHeader(500)).rejects.toThrow(
        'Horizon ledger 500 read failed: HTTP 503'
      );

      const fetchNetworkError = vi.fn(async () => {
        throw new Error('Connection reset by peer');
      }) as unknown as typeof fetch;

      const sourceError = createHorizonLedgerHeaderSource('http://horizon.local', fetchNetworkError);
      await expect(sourceError.getLedgerHeader(500)).rejects.toThrow(
        'Connection reset by peer'
      );
    });

    it('throws when payload has missing hash or prev_hash', async () => {
      const fetchMalformed = vi.fn(async () => ({
        ok: true,
        status: 200,
        json: async () => ({ sequence: 500, hash: 'h500' }), // missing prev_hash
      })) as unknown as typeof fetch;

      const source = createHorizonLedgerHeaderSource('http://horizon.local', fetchMalformed);
      await expect(source.getLedgerHeader(500)).rejects.toThrow(
        'Horizon ledger 500 response is missing hash/prev_hash fields'
      );
    });
  });

  describe('createStaticLedgerHeaderSource', () => {
    it('throws when requested ledger sequence is absent', async () => {
      const headers: LedgerHeader[] = [
        { sequence: 100, hash: 'h100', parentHash: 'h99' },
        { sequence: 101, hash: 'h101', parentHash: 'h100' },
      ];
      const source = createStaticLedgerHeaderSource(headers);

      await expect(source.getLedgerHeader(102)).rejects.toThrow(
        'ledger 102 not found in static header source'
      );
    });
  });

  describe('Gap reporting vs silent catch-up in IngestionCatchup', () => {
    it('reports missing ledgers as a gap failure instead of silently completing', async () => {
      const repo = new MockRepository();
      const catchup = new IngestionCatchup({
        repository: repo,
        horizonUrl: 'http://horizon.local',
        contractAddress: 'C_CONTRACT',
        batchSize: 10,
        ledgerBounds: async () => ({
          coreLatestLedger: 200,
          historyLatestLedger: 150,
          historyElderLedger: 100,
        }),
      });

      // Request range [50, 120), where [50, 100) is outside Horizon history
      await expect(catchup.run({ fromLedger: 50, toLedger: 120 })).rejects.toThrow(
        CatchupError
      );

      try {
        await catchup.run({ fromLedger: 50, toLedger: 120 });
      } catch (err) {
        expect(err).toBeInstanceOf(CatchupError);
        const catchupError = err as CatchupError;
        expect(catchupError.kind).toBe('gap');
        expect(catchupError.missingRanges).toEqual([[50, 100]]);
      }

      // Checkpoint must NOT be marked complete or advanced over the gap
      const cpKey = catchupCheckpointKey(50, 120);
      expect(repo.getState(cpKey)).toBeUndefined();
    });

    it('rejects invalid or inverted ledger ordering in catchup range', async () => {
      const repo = new MockRepository();
      const catchup = new IngestionCatchup({
        repository: repo,
        horizonUrl: 'http://horizon.local',
        contractAddress: 'C_CONTRACT',
      });

      await expect(catchup.run({ fromLedger: 200, toLedger: 100 })).rejects.toThrow(
        /expected fromLedger < toLedger/i
      );
    });
  });
});
