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

/** Horizon paging token (TOID) for the `order`-th transaction (1-based) in `ledger`. */
export function toid(ledger: number, order = 1): string {
  return ((BigInt(ledger) << 32n) | (BigInt(order) << 12n)).toString();
}

export function makeTx(ledger: number, hash: string, order = 1): HorizonTransactionRecord {
  return {
    hash,
    ledger,
    created_at: new Date(Date.UTC(2026, 7, 26, ledger % 24)).toISOString(),
    paging_token: toid(ledger, order),
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
  /** Every URL requested, in order. */
  requests: string[];
}

export interface HorizonFixtureOptions {
  /**
   * Emit `_links.next` on every page, including the last one, exactly like
   * Horizon does at the chain tip. Defaults to false (next only when more).
   */
  alwaysNext?: boolean;
  /**
   * Repeat this many records from the previous page at the start of each
   * following page (overlapping pages / duplicate transactions).
   */
  overlap?: number;
  /**
   * Called before serving request number `n` (0-based); return a Response to
   * override it (e.g. a 503) or throw to simulate a network failure.
   */
  intercept?: (n: number, url: URL) => Response | undefined;
  /** Served by `GET /` (Horizon root). */
  ledgerBounds?: { elder: number; latest: number };
}

export function makeHorizonFixture(
  specs: FakeEventSpec[],
  contractId: string = CONTRACT,
  options: HorizonFixtureOptions = {}
): HorizonFixture {
  void contractId;
  const orderInLedger = new Map<number, number>();
  const records = [...specs]
    .sort((a, b) => a.ledger - b.ledger)
    .map((spec) => {
      const order = (orderInLedger.get(spec.ledger) ?? 0) + 1;
      orderInLedger.set(spec.ledger, order);
      return { ...makeTx(spec.ledger, spec.hash, order), _links: {} };
    });

  const decodedByHash = new Map(specs.map((s) => [s.hash, s.events]));
  const requests: string[] = [];

  const fetchImpl = (async (url: unknown) => {
    const parsed = new URL(String(url));
    const n = requests.length;
    requests.push(parsed.toString());
    const override = options.intercept?.(n, parsed);
    if (override) {
      return override;
    }

    if (parsed.pathname === '/' || parsed.pathname === '') {
      const bounds = options.ledgerBounds ?? { elder: 1, latest: 1_000_000 };
      return jsonResponse({
        history_elder_ledger: bounds.elder,
        history_latest_ledger: bounds.latest,
      });
    }

    const cursor = BigInt(parsed.searchParams.get('cursor') || '0');
    const limit = Number(parsed.searchParams.get('limit') || 200);
    const firstAfter = records.findIndex((r) => BigInt(r.paging_token) > cursor);
    const startIndex =
      firstAfter === -1 ? records.length : Math.max(0, firstAfter - (n > 0 ? options.overlap ?? 0 : 0));
    const pageRecords = records.slice(startIndex, startIndex + limit);
    const hasMore = startIndex + pageRecords.length < records.length;
    const last = pageRecords[pageRecords.length - 1];
    const nextCursor = last ? last.paging_token : String(cursor);

    return jsonResponse({
      _embedded: { records: pageRecords },
      _links:
        hasMore || options.alwaysNext
          ? {
              next: {
                href: `${parsed.origin}/transactions?cursor=${nextCursor}&order=asc&limit=${limit}`,
              },
            }
          : {},
    });
  }) as typeof fetch;

  const decodeTransactionEvents = (record: HorizonTransactionRecord): DecodedContractEvent[] =>
    decodedByHash.get(record.hash) ?? [];

  return { fetchImpl, decodeTransactionEvents, requests };
}

export function jsonResponse(body: unknown, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
  } as Response;
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
