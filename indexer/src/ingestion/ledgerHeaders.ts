/**
 * Canonical ledger-header reads (Issue #863).
 *
 * Reorg detection needs the *ledger* header — `hash` + `parent_hash` at a
 * sequence — not just the transactions that happened to land in it. Horizon
 * exposes that as `GET /ledgers/{sequence}`; everything else in the ingestion
 * path talks to Horizon already, so the header source reuses the same
 * endpoint and the injected `fetchImpl` the tests already stub.
 *
 * Both the real-time detector (`reorgDetector.ts`) and the periodic
 * consistency job (`reconciliation/consistencyJob.ts`) consume this module
 * so they compare indexed history against the *same* definition of chain
 * truth.
 */

export interface LedgerHeader {
  sequence: number;
  /** Hex hash of the ledger at `sequence`. */
  hash: string;
  /** Hex hash of `sequence - 1` on the chain this header belongs to. */
  parentHash: string;
}

export interface LedgerHeaderSource {
  /** Resolves the current canonical header for `sequence`. */
  getLedgerHeader(sequence: number): Promise<LedgerHeader>;
}

interface HorizonLedgerResource {
  sequence?: number;
  hash?: string;
  prev_hash?: string;
}

/**
 * Horizon-backed header source.
 *
 * Deliberately uncached: a header read is only interesting when it might have
 * changed (reorg detection), so caching across calls would hide exactly the
 * divergence we are looking for. Callers that read the same ledger several
 * times inside one pass (e.g. a ledger carrying many transactions) memoize
 * at their own level.
 */
export function createHorizonLedgerHeaderSource(
  horizonUrl: string,
  fetchImpl: typeof fetch = fetch
): LedgerHeaderSource {
  const base = horizonUrl.replace(/\/$/, '');

  return {
    async getLedgerHeader(sequence: number): Promise<LedgerHeader> {
      const response = await fetchImpl(`${base}/ledgers/${sequence}`, {
        headers: { Accept: 'application/json' },
      });

      if (!response.ok) {
        throw new Error(`Horizon ledger ${sequence} read failed: HTTP ${response.status}`);
      }

      const body = (await response.json()) as HorizonLedgerResource;
      const hash = typeof body.hash === 'string' ? body.hash : '';
      const parentHash = typeof body.prev_hash === 'string' ? body.prev_hash : '';

      if (!hash || !parentHash) {
        throw new Error(
          `Horizon ledger ${sequence} response is missing hash/prev_hash fields`
        );
      }

      return { sequence, hash, parentHash };
    },
  };
}

/** Test/fixture helper: an in-memory header source over a fixed chain. */
export function createStaticLedgerHeaderSource(
  headers: LedgerHeader[]
): LedgerHeaderSource & { set(headers: LedgerHeader[]): void; push(header: LedgerHeader): void } {
  let chain = [...headers];
  return {
    async getLedgerHeader(sequence: number): Promise<LedgerHeader> {
      const header = chain.find((entry) => entry.sequence === sequence);
      if (!header) {
        throw new Error(`ledger ${sequence} not found in static header source`);
      }
      return header;
    },
    set(next) {
      chain = [...next];
    },
    push(header) {
      chain = chain.filter((entry) => entry.sequence !== header.sequence).concat(header);
    },
  };
}
