/**
 * Bounded, paginated reads of Horizon's `/transactions` collection for
 * historical ingestion (catch-up backfill and checkpoint replay).
 *
 * Range semantics: every helper here works on a half-open ledger range
 * `[fromLedger, toLedger)` — `fromLedger` included, `toLedger` excluded.
 *
 * Cursor semantics: Horizon paging tokens are TOIDs
 * (`ledger << 32 | txOrder << 12 | opIndex`) and a `cursor` is strictly
 * exclusive. Transaction application order starts at 1, so the TOID
 * `ledger << 32` sorts after every transaction of `ledger - 1` and before the
 * first transaction of `ledger`: it is the exact cursor for "start at ledger".
 * A bare ledger number is *not* a valid cursor — `cursor=12345677` addresses a
 * position inside ledger 0.
 */

import type { HorizonTransactionRecord } from './eventListener.js';

export interface RetryPolicy {
  /** Total attempts per request, including the first. */
  maxAttempts: number;
  initialBackoffMs: number;
  maxBackoffMs: number;
  /** Injectable for deterministic tests. */
  sleep?: (ms: number) => Promise<void>;
}

export const DEFAULT_RETRY_POLICY: RetryPolicy = {
  maxAttempts: 5,
  initialBackoffMs: 500,
  maxBackoffMs: 15_000,
};

/** A Horizon request that failed terminally (non-retryable or retries exhausted). */
export class HorizonFetchError extends Error {
  constructor(
    message: string,
    readonly url: string,
    readonly attempts: number,
    readonly status: number | null
  ) {
    super(message);
    this.name = 'HorizonFetchError';
  }
}

interface HorizonTransactionPage {
  _embedded?: { records?: HorizonTransactionRecord[] };
  _links?: { next?: { href?: string } };
}

export interface HorizonLedgerBounds {
  /** Oldest ledger Horizon still serves history for. */
  elderLedger: number;
  /** Newest ledger Horizon has ingested. */
  latestLedger: number;
}

/** Exclusive cursor positioned immediately before the first transaction of `ledger`. */
export function ledgerStartCursor(ledger: number): string {
  if (!Number.isSafeInteger(ledger) || ledger < 0) {
    throw new RangeError(`invalid ledger sequence ${ledger}`);
  }
  return (BigInt(ledger) << 32n).toString();
}

/** 429 and 5xx are transient; any other non-2xx is a request Horizon will never serve. */
function isRetryableStatus(status: number): boolean {
  return status === 429 || status >= 500;
}

function defaultSleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * GET a Horizon JSON resource with bounded exponential backoff.
 *
 * Network errors, 429, 5xx and unparseable bodies are retried; other HTTP
 * errors fail immediately. Exhaustion throws `HorizonFetchError` — callers
 * must treat that as terminal and never advance a checkpoint past it.
 */
export async function fetchHorizonJson<T>(
  url: string,
  fetchImpl: typeof fetch,
  retry: RetryPolicy = DEFAULT_RETRY_POLICY,
  logger: Pick<Console, 'warn'> = console
): Promise<T> {
  const sleep = retry.sleep ?? defaultSleep;
  const maxAttempts = Math.max(1, retry.maxAttempts);
  let backoffMs = retry.initialBackoffMs;
  let lastStatus: number | null = null;
  let lastMessage = 'no attempt made';

  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    try {
      const response = await fetchImpl(url, { headers: { Accept: 'application/json' } });
      lastStatus = response.status;
      if (!response.ok) {
        lastMessage = `Horizon responded with HTTP ${response.status}`;
        if (!isRetryableStatus(response.status)) {
          throw new HorizonFetchError(lastMessage, url, attempt, response.status);
        }
      } else {
        return (await response.json()) as T;
      }
    } catch (error) {
      if (error instanceof HorizonFetchError) {
        throw error;
      }
      lastMessage = error instanceof Error ? error.message : String(error);
    }

    if (attempt < maxAttempts) {
      logger.warn(
        `Horizon request failed (attempt ${attempt}/${maxAttempts}): ${lastMessage}; retrying in ${backoffMs}ms`
      );
      await sleep(backoffMs);
      backoffMs = Math.min(backoffMs * 2, retry.maxBackoffMs);
    }
  }

  throw new HorizonFetchError(
    `Horizon request failed after ${maxAttempts} attempt(s): ${lastMessage}`,
    url,
    maxAttempts,
    lastStatus
  );
}

/** Read Horizon's served history window from its root resource. */
export async function fetchHorizonLedgerBounds(
  horizonUrl: string,
  fetchImpl: typeof fetch,
  retry: RetryPolicy = DEFAULT_RETRY_POLICY,
  logger: Pick<Console, 'warn'> = console
): Promise<HorizonLedgerBounds> {
  const root = await fetchHorizonJson<{
    history_elder_ledger?: number;
    history_latest_ledger?: number;
  }>(`${horizonUrl.replace(/\/$/, '')}/`, fetchImpl, retry, logger);

  const elderLedger = Number(root.history_elder_ledger);
  const latestLedger = Number(root.history_latest_ledger);
  if (!Number.isSafeInteger(elderLedger) || !Number.isSafeInteger(latestLedger)) {
    throw new Error('Horizon root did not report history_elder_ledger/history_latest_ledger');
  }
  return { elderLedger, latestLedger };
}

export interface TransactionRangeOptions {
  horizonUrl: string;
  fromLedger: number;
  /** Exclusive upper bound; omit to read up to Horizon's current tip. */
  toLedger?: number;
  pageSize: number;
  fetchImpl: typeof fetch;
  retry?: RetryPolicy;
  logger?: Pick<Console, 'warn'>;
  /** Called once per transaction skipped as an already-seen duplicate. */
  onDuplicate?: (record: HorizonTransactionRecord) => void;
}

/**
 * Yield every transaction in `[fromLedger, toLedger)` in ascending paging
 * order, following `_links.next` until the range is exhausted.
 *
 * Termination: a page whose records reach `toLedger`, or an empty page (the
 * chain tip — Horizon keeps returning a `next` link there, so following it
 * blindly would never end). Records at or before the last yielded paging
 * token are duplicates from an overlapping page and are skipped.
 */
export async function* iterateTransactionRange(
  options: TransactionRangeOptions
): AsyncGenerator<HorizonTransactionRecord> {
  const base = options.horizonUrl.replace(/\/$/, '');
  const url = new URL(`${base}/transactions`);
  url.searchParams.set('cursor', ledgerStartCursor(options.fromLedger));
  url.searchParams.set('order', 'asc');
  url.searchParams.set('limit', String(options.pageSize));

  let nextHref: string | null = url.toString();
  let lastToken: bigint | null = null;
  const logger = options.logger ?? console;

  while (nextHref !== null) {
    const page: HorizonTransactionPage = await fetchHorizonJson<HorizonTransactionPage>(
      nextHref,
      options.fetchImpl,
      options.retry,
      logger
    );
    const records = page._embedded?.records ?? [];
    if (records.length === 0) {
      return;
    }

    for (const record of records) {
      if (options.toLedger !== undefined && record.ledger >= options.toLedger) {
        return;
      }

      const token = BigInt(record.paging_token);
      if (lastToken !== null && token <= lastToken) {
        options.onDuplicate?.(record);
        continue;
      }
      lastToken = token;

      // Horizon never returns a record before the cursor; guard anyway so a
      // misbehaving proxy cannot smuggle out-of-range rows into a batch.
      if (record.ledger < options.fromLedger) {
        continue;
      }

      yield record;
    }

    const href = page._links?.next?.href;
    nextHref = href ? new URL(href, `${base}/`).toString() : null;
  }
}
