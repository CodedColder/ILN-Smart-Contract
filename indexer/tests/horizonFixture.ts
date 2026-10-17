/**
 * Injectable Horizon fixtures for ingestion/replay/recovery tests.
 *
 * Builds a fake `fetch` (transaction pages) plus a matching
 * `decodeTransactionEvents` from a compact spec, so tests never have to
 * craft XDR transaction meta by hand. Mirrors the shape the production
 * `EventListener` and `runReplay` consume.
 */

import type {
  DecodedContractEvent,
  HorizonTransactionRecord,
} from '../src/ingestion/eventListener.js';

export const CONTRACT = 'CDEMOCONTRACT';

export function makeTx(ledger: number, hash: string): HorizonTransactionRecord {
  return {
    hash,
    ledger,
    created_at: new Date(Date.UTC(2026, 7, 26, ledger % 24)).toISOString(),
    paging_token: String(1200000000 + ledger),
    result_meta_xdr: 'unused-by-injected-decoder',
  };
}

export interface FakeEventSpec {
  ledger: number;
  hash: string;
  events: DecodedContractEvent[];
}

export interface HorizonFixture {
  fetchImpl: typeof fetch;
  decodeTransactionEvents: (record: HorizonTransactionRecord) => DecodedContractEvent[];
}

export function makeHorizonFixture(
  specs: FakeEventSpec[],
  contractId: string = CONTRACT
): HorizonFixture {
  const records = specs.map((spec) => ({
    ...makeTx(spec.ledger, spec.hash),
    _links: {},
  }));

  const decodedByHash = new Map(specs.map((s) => [s.hash, s.events]));

  const fetchImpl = (async (url: unknown) => {
    const parsed = new URL(String(url));
    const cursor = Number(parsed.searchParams.get('cursor') || 0);
    const limit = Number(parsed.searchParams.get('limit') || 200);
    const eligible = records.filter((r) => r.ledger > cursor);

    const pageRecords = eligible.slice(0, limit);
    const hasMore = eligible.length > pageRecords.length;

    return {
      ok: true,
      status: 200,
      json: async () => ({
        _embedded: { records: hasMore ? pageRecords : eligible },
        _links: hasMore
          ? {
              next: {
                href: `${parsed.origin}/transactions?cursor=${
                  pageRecords[pageRecords.length - 1].paging_token
                }&order=asc&limit=${limit}`,
              },
            }
          : {},
      }),
    } as Response;
  }) as typeof fetch;

  const decodeTransactionEvents = (record: HorizonTransactionRecord): DecodedContractEvent[] =>
    decodedByHash.get(record.hash) ?? [];

  return { fetchImpl, decodeTransactionEvents };
}

export function submittedEvent(
  invoiceId: number,
  amount: string,
  contractId: string = CONTRACT
): DecodedContractEvent[] {
  return [
    {
      contractId,
      rawEventType: 'submitted',
      contractEventType: 'InvoiceSubmitted',
      topics: ['submitted'],
      data: { invoice_id: invoiceId, amount, token: 'USDC', status: 'Pending' },
    },
  ];
}

export function fundedEvent(
  invoiceId: number,
  funder: string,
  amountFunded: string,
  contractId: string = CONTRACT
): DecodedContractEvent[] {
  return [
    {
      contractId,
      rawEventType: 'funded',
      contractEventType: 'InvoiceFunded',
      topics: ['funded'],
      data: {
        invoice_id: invoiceId,
        funder,
        amount_funded: amountFunded,
        status: 'Funded',
      },
    },
  ];
}

export function paidEvent(
  invoiceId: number,
  amountPaid: string,
  contractId: string = CONTRACT
): DecodedContractEvent[] {
  return [
    {
      contractId,
      rawEventType: 'paid',
      contractEventType: 'InvoicePaid',
      topics: ['paid'],
      data: { invoice_id: invoiceId, amount_paid: amountPaid, status: 'Paid', lp: 'G-LP' },
    },
  ];
}
