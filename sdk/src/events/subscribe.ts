// @ts-nocheck
/**
 * Real-time ILN contract event subscription backed by Horizon's SSE
 * `/effects` + `/contract-events` streaming endpoint.
 *
 * Features
 * --------
 * - Typed discriminated-union events (ILNEvent)
 * - Client-side filtering by event type, invoiceId, and address
 * - Transparent reconnection with exponential back-off on stream errors
 * - Clean `Unsubscribe` tear-down function returned to the caller
 */

import { Horizon } from "@stellar/stellar-sdk";
import type {
  EventFilter,
  ILNEvent,
  ILNEventType,
  Unsubscribe,
} from "./types.js";

// ---------------------------------------------------------------------------
// Back-off constants
// ---------------------------------------------------------------------------

const INITIAL_BACKOFF_MS = 500;
const MAX_BACKOFF_MS = 30_000;
const BACKOFF_FACTOR = 2;

// ---------------------------------------------------------------------------
// Raw Horizon contract-event record (simplified)
// ---------------------------------------------------------------------------

interface HorizonContractEvent {
  type: string;
  /** Array of base-64 XDR ScVal strings representing the topics. */
  topic: string[];
  /** Base-64 XDR ScVal string for the event body / value. */
  value: string;
  contractId?: string;
  ledger?: number;
  ledgerClosedAt?: string;
  txHash?: string;
  id?: string;
}

// ---------------------------------------------------------------------------
// XDR / ScVal decoding helpers
// ---------------------------------------------------------------------------

import { xdr, scValToNative } from "@stellar/stellar-sdk";

const invalidScVal = Symbol("invalid ScVal");

/**
 * Decode a base-64 XDR ScVal and convert it to a plain JS value via
 * `scValToNative` from the Stellar SDK.
 */
function decodeScVal(base64Xdr: string): unknown {
  try {
    const scVal = xdr.ScVal.fromXDR(base64Xdr, "base64");
    return scValToNative(scVal);
  } catch {
    return invalidScVal;
  }
}

/** Extract the event-type discriminant from the first topic (a Symbol). */
function extractEventType(topics: string[] | undefined): string | null {
  if (!topics) return null;
  if (!topics.length) return null;
  const decoded = decodeScVal(topics[0] || "");
  if (typeof decoded === "string") return decoded;
  return null;
}

// ---------------------------------------------------------------------------
// Event parsing
// ---------------------------------------------------------------------------

/**
 * Parse a raw Horizon contract-event record into a typed ILNEvent.
 *
 * Returns `null` when the event cannot be decoded or is not a known ILN type.
 */
export function parseContractEvent(
  raw: HorizonContractEvent
): ILNEvent | null {
  const event = parseContractEventInternal(raw);
  if (event) {
    event.ledger = raw.ledger;
    event.pagingToken = raw.id;
  }
  return event;
}

function parseContractEventInternal(
  raw: HorizonContractEvent
): ILNEvent | null {
  const eventType = extractEventType(raw.topic);
  if (!eventType) return null;

  // Decode the topics (beyond the first which is the type) and the value
  const topics = (raw.topic || []).slice(1).map(decodeScVal);
  const value = decodeScVal(raw.value || "");
  if (topics.includes(invalidScVal) || value === invalidScVal) return null;
  if (value != null && (typeof value !== "object" || Array.isArray(value))) return null;
  const body = (value ?? {}) as Record<string, unknown>;

  const big = (v: unknown): bigint => {
    try { return BigInt(String(v)); } catch { return 0n; }
  };
  const num = (v: unknown): number => Number(v ?? 0);
  const str = (v: unknown): string => String(v ?? "");
  const bool = (v: unknown): boolean => Boolean(v);

  switch (eventType as ILNEventType) {
    case "submitted":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "submitted",
        invoiceId: big(topics[0] || ""),
        freelancer: str(topics[1]),
        payer: str(topics[2]),
        token: str(body["token"]),
        amount: big(body["amount"]),
        dueDate: big(body["due_date"]),
        discountRate: num(body["discount_rate"]),
        status: str(body["status"]),
        
      };

    case "funded":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "funded",
        invoiceId: big(topics[0] || ""),
        funder: str(topics[1]),
        freelancer: str(body["freelancer"]),
        payer: str(body["payer"]),
        token: str(body["token"]),
        fundAmount: big(body["fund_amount"]),
        amountFunded: big(body["amount_funded"]),
        invoiceAmount: big(body["invoice_amount"]),
        dueDate: big(body["due_date"]),
        discountRate: num(body["discount_rate"]),
        fundedAt: body["funded_at"] != null ? big(body["funded_at"]) : null,
        status: str(body["status"]),
        lp: str(body["lp"]),
        effectiveYieldBps: num(body["effective_yield_bps"]),
        
      };

    case "paid":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "paid",
        invoiceId: big(topics[0] || ""),
        payer: str(topics[1]),
        lp: str(topics[2]),
        freelancer: str(body["freelancer"]),
        token: str(body["token"]),
        amountPaid: big(body["amount_paid"]),
        lpEarned: big(body["lp_earned"]),
        lpPayout: big(body["lp_payout"]),
        settlementTimestamp: big(body["settlement_timestamp"]),
        paidOnTime: bool(body["paid_on_time"]),
        status: str(body["status"]),
      };

    case "partially_paid":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "partially_paid",
        invoiceId: big(topics[0] || ""),
        payer: str(topics[1]),
        amountPaidNow: big(body["amount_paid_now"]),
        totalAmountPaid: big(body["total_amount_paid"]),
        remainingAmount: big(body["remaining_amount"]),
      };

    case "defaulted":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "defaulted",
        invoiceId: big(topics[0] || ""),
        funder: str(topics[1]),
        freelancer: str(body["freelancer"]),
        payer: str(body["payer"]),
        token: str(body["token"]),
        amount: big(body["amount"]),
        dueDate: big(body["due_date"]),
        defaultedAt: big(body["defaulted_at"]),
        discountAmount: big(body["discount_amount"]),
        status: str(body["status"]),
      };

    case "default_appealed":
    case "appealed":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "appealed",
        invoiceId: big(topics[0] || ""),
        payer: str(topics[1]),
        evidenceHash: str(body["evidence_hash"]),
        appealedAt: big(body["appealed_at"]),
      };

    case "appeal_resolved":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "appeal_resolved",
        invoiceId: big(topics[0] || ""),
        payer: str(topics[1]),
        upheld: bool(body["upheld"]),
        resolvedAt: big(body["resolved_at"]),
      };

    case "disputed":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "disputed",
        invoiceId: big(topics[0] || ""),
        payer: str(topics[1]),
        reasonHash: str(body["reason_hash"]),
        disputedAt: big(body["disputed_at"]),
      };

    case "dispute_resolved":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "dispute_resolved",
        invoiceId: big(topics[0] || ""),
        resolutionHash: str(topics[1]),
        resolution: num(body["resolution"]),
        resolvedAt: big(body["resolved_at"]),
      };

    case "token_added":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "token_added",
        token: str(topics[0] || ""),
        decimals: num(body["decimals"]),
      };

    case "token_removed":
      return {
        timestamp: num(body["timestamp"] ?? (raw.ledgerClosedAt as string)),
        txHash: ((raw.txHash || "") as string) || "",
        type: "token_removed",
        token: str(topics[0]),
      } as unknown as ILNEvent;

    case "transferred":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "transferred",
        invoiceId: big(topics[0] || ""),
        oldFreelancer: str(body["old_freelancer"]),
        newFreelancer: str(body["new_freelancer"]),
        status: str(body["status"]),
      };

    case "cancelled":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "cancelled",
        invoiceId: big(topics[0] || ""),
        freelancer: str(body["freelancer"]),
        status: str(body["status"]),
      };

    case "paused":
      return { txHash: raw.txHash || "", type: "paused", timestamp: num(body["timestamp"]) };

    case "unpaused":
      return { txHash: raw.txHash || "", type: "unpaused", timestamp: num(body["timestamp"]) };

    case "upgraded":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "upgraded",
        admin: str(topics[0] || ""),
        newWasmHash: str(body["new_wasm_hash"]),
        
      };

    case "admin_changed":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "admin_changed",
        oldAdmin: str(body["old_admin"]),
        newAdmin: str(body["new_admin"]),
        
      };

    case "parameter_updated":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "parameter_updated",
        paramName: str(topics[0]),
        oldValue: big(body["old_value"]),
        newValue: big(body["new_value"]),
        updatedBy: str(body["updated_by"]),
      };

    case "fund_requested":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "fund_requested",
        invoiceId: big(topics[0] || ""),
        lp: str(topics[1]),
        score: num(body["score"]),
      };

    case "fund_queue_resolved":
      return {
        timestamp: num(body["timestamp"] ?? ((raw.ledgerClosedAt as string) as string)),
        txHash: (((raw.txHash || "") as string) as string) || "",
        type: "fund_queue_resolved",
        invoiceId: big(topics[0] || ""),
        approvedLp: str(topics[1]),
        score: num(body["score"]),
      };

    default:
      return null;
  }
}

// ---------------------------------------------------------------------------
// Client-side filter
// ---------------------------------------------------------------------------

/**
 * Returns true when the parsed event satisfies the caller's EventFilter.
 */
export function matchesFilter(event: ILNEvent, filter: EventFilter): boolean {
  if (filter.types?.length && !filter.types.includes(event.type as ILNEventType)) {
    return false;
  }

  if (filter.invoiceId !== undefined) {
    const id = (event as Record<string, unknown>)["invoiceId"];
    if (id === undefined || BigInt(String(id)) !== filter.invoiceId) return false;
  }

  if (filter.address !== undefined) {
    const addr = filter.address.toLowerCase();
    const values = Object.values(event as Record<string, unknown>);
    const found = values.some(
      (v) => typeof v === "string" && v.toLowerCase() === addr
    );
    if (!found) return false;
  }

  return true;
}

// ---------------------------------------------------------------------------
// subscribe()
// ---------------------------------------------------------------------------

/**
 * Subscribe to real-time ILN contract events via Horizon's SSE stream.
 *
 * Handles stream disconnection with exponential back-off (500 ms → 30 s).
 * The returned `Unsubscribe` function closes the stream and cancels any
 * pending reconnection timer.
 *
 * @param horizon         - Horizon server instance (use `new Server(horizonUrl)`)
 * @param contractId      - Deployed invoice-liquidity contract address
 * @param filter          - Optional criteria; empty object matches all events
 * @param handler         - Called once per matching decoded event
 * @param onError         - Optional error callback for logging / monitoring
 * @returns Unsubscribe   - Call to stop the stream
 *
 * @example
 * ```ts
 * const unsub = subscribe(horizon, CONTRACT_ID, { types: ["funded", "paid"] },
 *   (event) => console.log(event));
 * // later…
 * unsub();
 * ```
 */
export function subscribe(
  horizon: Horizon.Server,
  contractId: string,
  filter: EventFilter,
  handler: (event: ILNEvent) => void,
  onError?: (err: unknown) => void
): Unsubscribe {
  let stopped = false;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let closeStream: (() => void) | null = null;
  let backoffMs = INITIAL_BACKOFF_MS;

  let lastProcessedLedger: number | undefined = filter.fromLedger ? filter.fromLedger - 1 : undefined;
  let lastProcessedCursor: string | undefined = undefined;
  const processedTokens = new Set<string>();

  function trackEvent(event: ILNEvent) {
    if (event.pagingToken) {
      processedTokens.add(event.pagingToken);
      if (processedTokens.size > 1000) {
        const firstVal = processedTokens.values().next().value;
        if (firstVal !== undefined) {
          processedTokens.delete(firstVal);
        }
      }
      lastProcessedCursor = event.pagingToken;
    }
    if (event.ledger !== undefined) {
      lastProcessedLedger = event.ledger;
    }
  }

  function connect(): void {
    if (stopped) return;

    try {
      // Horizon's contractEvents() returns an EventSource-like stream.
      // The SDK builder pattern: horizon.contractEvents(contractId).stream(...)
      let builder = (horizon as unknown)
        .contractEvents()
        .forContract(contractId)
        .limit(200);

      if (lastProcessedCursor) {
        builder = (builder as any).cursor(lastProcessedCursor);
      }

      closeStream = builder.stream({
        async onmessage(raw: any) {
          if (stopped) return;
          try {
            const event = parseContractEvent(raw);
            if (!event) return;

            // Avoid duplicate processing
            if (event.pagingToken && processedTokens.has(event.pagingToken)) {
              return;
            }

            // Ledger Gap Recovery
            if (
              lastProcessedLedger !== undefined &&
              event.ledger !== undefined &&
              event.ledger > lastProcessedLedger + 1
            ) {
              try {
                const replayStartLedger = lastProcessedLedger + 1;
                await replay(horizon, contractId, filter, replayStartLedger, (replayEv) => {
                  if (stopped || (replayEv.pagingToken && processedTokens.has(replayEv.pagingToken))) {
                    return;
                  }
                  if (matchesFilter(replayEv, filter)) {
                    handler(replayEv);
                  }
                  trackEvent(replayEv);
                });
              } catch (replayErr) {
                if (!stopped) onError?.(replayErr);
              }
            }

            // Final deduplication check after potential replay catch-up
            if (stopped || (event.pagingToken && processedTokens.has(event.pagingToken))) {
              return;
            }

            if (matchesFilter(event, filter)) {
              handler(event);
            }
            trackEvent(event);
          } catch (parseErr) {
            onError?.(parseErr);
          }
        },
        onerror(err: unknown) {
          if (stopped) return;
          onError?.(err);
          // Close current stream and schedule reconnect
          closeStream?.();
          closeStream = null;
          scheduleReconnect();
        },
      }) as () => void;

      // Successful connection → reset back-off
      backoffMs = INITIAL_BACKOFF_MS;
    } catch (err) {
      onError?.(err);
      scheduleReconnect();
    }
  }

  function scheduleReconnect(): void {
    if (stopped) return;
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      connect();
    }, backoffMs);
    backoffMs = Math.min(backoffMs * BACKOFF_FACTOR, MAX_BACKOFF_MS);
  }

  // Handle optional initial event replay
  if (filter.fromLedger !== undefined) {
    replay(horizon, contractId, filter, filter.fromLedger, (replayEv) => {
      if (stopped || (replayEv.pagingToken && processedTokens.has(replayEv.pagingToken))) {
        return;
      }
      if (matchesFilter(replayEv, filter)) {
        handler(replayEv);
      }
      trackEvent(replayEv);
    })
      .then((finalCursor) => {
        if (stopped) return;
        if (finalCursor) {
          lastProcessedCursor = finalCursor;
        }
        connect();
      })
      .catch((err) => {
        if (!stopped) {
          onError?.(err);
          connect();
        }
      });
  } else {
    connect();
  }

  return function unsubscribe(): void {
    stopped = true;
    if (reconnectTimer !== null) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
    closeStream?.();
    closeStream = null;
  };
}

/**
 * Replay historical events starting from `fromLedger` sequence number.
 * Paginates forward and calls the handler for matching events.
 * Returns the final paging token cursor.
 */
export async function replay(
  horizon: Horizon.Server,
  contractId: string,
  filter: EventFilter,
  fromLedger: number,
  handler: (event: ILNEvent) => void
): Promise<string | undefined> {
  let cursor: string | undefined = `${fromLedger}`;
  let lastCursor: string | undefined = undefined;
  // Tracks every record id already handled so a page that repeats previously-seen
  // records (e.g. a non-advancing or misbehaving pagination source) can't cause
  // records to be reprocessed forever — `lastCursor` alone only catches one id
  // per page and lets other repeated records slip through indefinitely.
  const seenIds = new Set<string>();

  while (true) {
    let builder = (horizon as any)
      .contractEvents()
      .forContract(contractId)
      .limit(200)
      .order("asc");

    if (cursor) {
      builder = builder.cursor(cursor);
    }

    const page = await builder.call();
    if (!page || !page.records || page.records.length === 0) {
      break;
    }

    let processedAny = false;
    for (const record of page.records) {
      if (record.id !== undefined && seenIds.has(record.id)) continue;

      // Filter out events below the target ledger
      if (record.ledger !== undefined && record.ledger < fromLedger) {
        continue;
      }

      const event = parseContractEvent(record);
      if (event && matchesFilter(event, filter)) {
        handler(event);
      }
      if (record.id !== undefined) seenIds.add(record.id);
      cursor = record.id;
      processedAny = true;
    }

    if (!processedAny || cursor === lastCursor) {
      break;
    }
    lastCursor = cursor;
  }

  return lastCursor;
}
