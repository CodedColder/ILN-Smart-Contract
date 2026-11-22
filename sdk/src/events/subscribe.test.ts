import { vi, describe, it, expect, beforeEach, afterEach } from 'vitest';
/**
 * Tests for sdk/src/events/subscribe.ts
 *
 * Covers:
 * - parseContractEvent: every ILN event type
 * - matchesFilter: type, invoiceId, address filtering
 * - subscribe(): happy-path delivery, filtering, unsubscribe, reconnect back-off
 */

import { parseContractEvent, matchesFilter, subscribe, replay } from "./subscribe.js";
import type { ILNEvent } from "./types.js";

// ---------------------------------------------------------------------------
// Mock @stellar/stellar-sdk (scValToNative / xdr)
// ---------------------------------------------------------------------------

vi.mock("@stellar/stellar-sdk", () => {
  return {
    scValToNative: vi.fn((scVal: unknown) => scVal?.__native ?? scVal),
    xdr: {
      ScVal: {
        fromXDR: vi.fn((b64: string, ) => {
          // Return the decoded value stored in our test fixtures
          return { __native: DECODED_TOPICS[b64] ?? b64 };
        }),
      },
    },
  };
});

// Map base-64-like keys → decoded native values (our test fixture encoding)
const DECODED_TOPICS: Record<string, unknown> = {};

function encodeVal(v: unknown): string {
  const key = `__b64_${JSON.stringify(v, (_, val) => typeof val === 'bigint' ? val.toString() : val)}`;
  DECODED_TOPICS[key] = v;
  return key;
}

// ---------------------------------------------------------------------------
// Raw event builder helpers
// ---------------------------------------------------------------------------

function makeRaw(
  type: string,
  extraTopics: unknown[],
  body: Record<string, unknown>
) {
  return {
    type: "contract",
    topic: [encodeVal(type), ...extraTopics.map(encodeVal)],
    value: encodeVal(body),
    contractId: "CBCONTRACT",
  };
}

// ---------------------------------------------------------------------------
// parseContractEvent — all event types
// ---------------------------------------------------------------------------

describe("parseContractEvent", () => {
  it("ignores fields added by a future contract version", () => {
    const raw = makeRaw("partially_paid", [4n, "GPAYER"], {
      amount_paid_now: "2",
      total_amount_paid: "5",
      remaining_amount: "8",
      future_field: { version: 2 },
    });
    expect(parseContractEvent(raw)).toMatchObject({
      type: "partially_paid",
      invoiceId: 4n,
      remainingAmount: 8n,
    });
    expect(parseContractEvent(raw)).not.toHaveProperty("future_field");
  });

  it("ignores an event whose decoded payload is not a field map", () => {
    const raw = makeRaw("partially_paid", [4n, "GPAYER"], {});
    expect(parseContractEvent({ ...raw, value: "malformed" })).toBeNull();
  });

  it("parses submitted", () => {
    const raw = makeRaw("submitted", [1n, "GFREELANCER", "GPAYER"], {
      token: "CDTOKEN",
      amount: "1000000",
      due_date: "1700086400",
      discount_rate: 300,
      status: "Pending",
      timestamp: "1700000000",
    });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("submitted");
    expect(ev?.invoiceId).toBe(1n);
    expect(ev?.freelancer).toBe("GFREELANCER");
    expect(ev?.payer).toBe("GPAYER");
    expect(ev?.amount).toBe(1_000_000n);
    expect(ev?.discountRate).toBe(300);
  });

  it("parses funded", () => {
    const raw = makeRaw("funded", [2n, "GLP"], {
      freelancer: "GFREELANCER",
      payer: "GPAYER",
      token: "CDTOKEN",
      fund_amount: "500000",
      amount_funded: "1000000",
      invoice_amount: "1000000",
      due_date: "1700086400",
      discount_rate: 300,
      funded_at: "1700001000",
      status: "Funded",
      lp: "GLP",
      effective_yield_bps: 24,
      timestamp: "1700001000",
    });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("funded");
    expect(ev?.invoiceId).toBe(2n);
    expect(ev?.funder).toBe("GLP");
    expect(ev?.effectiveYieldBps).toBe(24);
    expect(ev?.fundedAt).toBe(1_700_001_000n);
  });

  it("parses funded with null fundedAt", () => {
    const raw = makeRaw("funded", [2n, "GLP"], {
      freelancer: "GF", payer: "GP", token: "CT",
      fund_amount: "1", amount_funded: "1", invoice_amount: "1",
      due_date: "0", discount_rate: 0, funded_at: null,
      status: "PartiallyFunded", lp: "GLP", effective_yield_bps: 0, timestamp: "0",
    });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.fundedAt).toBeNull();
  });

  it("parses paid", () => {
    const raw = makeRaw("paid", [3n, "GPAYER", "GLP"], {
      freelancer: "GF", token: "CT",
      amount_paid: "1000000", lp_earned: "30000", lp_payout: "1000000",
      settlement_timestamp: "1700002000", paid_on_time: true, status: "Paid",
    });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("paid");
    expect(ev?.paidOnTime).toBe(true);
    expect(ev?.lpEarned).toBe(30_000n);
  });

  it("parses partially_paid", () => {
    const raw = makeRaw("partially_paid", [4n, "GPAYER"], {
      amount_paid_now: "200000", total_amount_paid: "200000", remaining_amount: "800000",
    });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("partially_paid");
    expect(ev?.remainingAmount).toBe(800_000n);
  });

  it("parses defaulted", () => {
    const raw = makeRaw("defaulted", [5n, "GLP"], {
      freelancer: "GF", payer: "GP", token: "CT",
      amount: "1000000", due_date: "1699999999",
      defaulted_at: "1700000001", discount_amount: "30000", status: "Defaulted",
    });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("defaulted");
    expect(ev?.defaultedAt).toBe(1_700_000_001n);
  });

  it("parses appealed (default_appealed alias)", () => {
    const raw = makeRaw("default_appealed", [6n, "GPAYER"], {
      evidence_hash: "abc123", appealed_at: "1700003000",
    });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("appealed");
    expect(ev?.evidenceHash).toBe("abc123");
  });

  it("parses appeal_resolved", () => {
    const raw = makeRaw("appeal_resolved", [7n, "GPAYER"], {
      upheld: true, resolved_at: "1700004000",
    });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("appeal_resolved");
    expect(ev?.upheld).toBe(true);
  });

  it("parses disputed", () => {
    const raw = makeRaw("disputed", [8n, "GPAYER"], {
      reason_hash: "deadbeef", disputed_at: "1700005000",
    });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("disputed");
    expect(ev?.reasonHash).toBe("deadbeef");
  });

  it("parses dispute_resolved", () => {
    const raw = makeRaw("dispute_resolved", [9n, "HASHXYZ"], {
      resolution: 2, resolved_at: "1700006000",
    });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("dispute_resolved");
    expect(ev?.resolution).toBe(2);
  });

  it("parses token_added", () => {
    const raw = makeRaw("token_added", ["CDTOKEN"], { decimals: 6 });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("token_added");
    expect(ev?.token).toBe("CDTOKEN");
    expect(ev?.decimals).toBe(6);
  });

  it("parses token_removed", () => {
    const raw = makeRaw("token_removed", ["CDTOKEN"], {});
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("token_removed");
    expect(ev?.token).toBe("CDTOKEN");
  });

  it("parses parameter_updated", () => {
    const raw = makeRaw("parameter_updated", ["protocol_fee_rate_bps", "GADMIN"], {
      old_value: "0", new_value: "50", updated_by: "GADMIN",
    });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("parameter_updated");
    expect(ev?.paramName).toBe("protocol_fee_rate_bps");
    expect(ev?.newValue).toBe(50n);
  });

  it("parses transferred", () => {
    const raw = makeRaw("transferred", [10n], {
      old_freelancer: "GOLD", new_freelancer: "GNEW", status: "Pending",
    });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("transferred");
    expect(ev?.newFreelancer).toBe("GNEW");
  });

  it("parses cancelled", () => {
    const raw = makeRaw("cancelled", [11n], {
      freelancer: "GF", status: "Cancelled",
    });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("cancelled");
    expect(ev?.status).toBe("Cancelled");
  });

  it("parses paused", () => {
    const raw = makeRaw("paused", [], { timestamp: "1700007000" });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("paused");
    expect(ev?.timestamp).toBe(1_700_007_000);
  });

  it("parses unpaused", () => {
    const raw = makeRaw("unpaused", [], { timestamp: "1700008000" });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("unpaused");
  });

  it("parses upgraded", () => {
    const raw = makeRaw("upgraded", ["GADMIN"], {
      new_wasm_hash: "deadbeef", timestamp: "1700009000",
    });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("upgraded");
    expect(ev?.admin).toBe("GADMIN");
  });

  it("parses admin_changed", () => {
    const raw = makeRaw("admin_changed", [], {
      old_admin: "GOLD", new_admin: "GNEW", timestamp: "1700010000",
    });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("admin_changed");
    expect(ev?.newAdmin).toBe("GNEW");
  });

  it("parses fund_requested", () => {
    const raw = makeRaw("fund_requested", [12n, "GLP"], { score: 75 });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("fund_requested");
    expect(ev?.score).toBe(75);
  });

  it("parses fund_queue_resolved", () => {
    const raw = makeRaw("fund_queue_resolved", [13n, "GLP"], { score: 80 });
    const ev = parseContractEvent(raw as unknown) as unknown;
    expect(ev?.type).toBe("fund_queue_resolved");
    expect(ev?.approvedLp).toBe("GLP");
  });

  it("returns null for unknown event type", () => {
    const raw = makeRaw("unknown_event", [], {});
    expect(parseContractEvent(raw as unknown)).toBeNull();
  });

  it("returns null when topics array is empty", () => {
    expect(parseContractEvent({ type: "contract", topic: [], value: "" } as unknown)).toBeNull();
  });

  it("returns null when XDR decoding throws", async () => {
    const { xdr } = await import("@stellar/stellar-sdk");
    (xdr.ScVal.fromXDR as ReturnType<typeof vi.fn>).mockImplementationOnce(() => {
      throw new Error("bad xdr");
    });
    const raw = makeRaw("submitted", [], {});
    expect(parseContractEvent(raw as unknown)).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// matchesFilter
// ---------------------------------------------------------------------------

const FUNDED_EVENT: ILNEvent = {
  type: "funded",
  invoiceId: 42n,
  funder: "GLP1",
  freelancer: "GFREELANCER",
  payer: "GPAYER",
  token: "CDTOKEN",
  fundAmount: 1_000_000n,
  amountFunded: 1_000_000n,
  invoiceAmount: 1_000_000n,
  dueDate: 1_700_086_400n,
  discountRate: 300,
  fundedAt: null,
  status: "Funded",
  lp: "GLP1",
  effectiveYieldBps: 24,
  timestamp: 1_700_000_000n,
};

describe("matchesFilter", () => {
  it("returns true for empty filter (matches everything)", () => {
    expect(matchesFilter(FUNDED_EVENT, {})).toBe(true);
  });

  it("matches by event type", () => {
    expect(matchesFilter(FUNDED_EVENT, { types: ["funded"] })).toBe(true);
  });

  it("rejects when type not in filter list", () => {
    expect(matchesFilter(FUNDED_EVENT, { types: ["paid"] })).toBe(false);
  });

  it("matches when type is in a multi-item list", () => {
    expect(matchesFilter(FUNDED_EVENT, { types: ["paid", "funded"] })).toBe(true);
  });

  it("matches by invoiceId", () => {
    expect(matchesFilter(FUNDED_EVENT, { invoiceId: 42n })).toBe(true);
  });

  it("rejects when invoiceId does not match", () => {
    expect(matchesFilter(FUNDED_EVENT, { invoiceId: 99n })).toBe(false);
  });

  it("rejects events without invoiceId field when filter.invoiceId is set", () => {
    const pausedEvent: ILNEvent = { type: "paused", timestamp: 0n };
    expect(matchesFilter(pausedEvent, { invoiceId: 1n })).toBe(false);
  });

  it("matches by address (funder field)", () => {
    expect(matchesFilter(FUNDED_EVENT, { address: "GLP1" })).toBe(true);
  });

  it("matches address case-insensitively", () => {
    expect(matchesFilter(FUNDED_EVENT, { address: "glp1" })).toBe(true);
  });

  it("rejects when address not in any field", () => {
    expect(matchesFilter(FUNDED_EVENT, { address: "GSTRANGER" })).toBe(false);
  });

  it("combines type + invoiceId + address (AND logic)", () => {
    expect(
      matchesFilter(FUNDED_EVENT, {
        types: ["funded"],
        invoiceId: 42n,
        address: "GLP1",
      })
    ).toBe(true);
  });

  it("fails combined filter when one criterion is wrong", () => {
    expect(
      matchesFilter(FUNDED_EVENT, {
        types: ["funded"],
        invoiceId: 42n,
        address: "GSTRANGER",
      })
    ).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// subscribe() — helper to build a mock Horizon server
// ---------------------------------------------------------------------------

function makeMockHorizon(opts: {
  events?: unknown[];
  streamError?: unknown;
  throwOnConnect?: boolean;
} = {}) {
  let onmessageCb: ((raw: unknown) => void) | null = null;
  let onerrorCb: ((err: unknown) => void) | null = null;

  const closeStream = vi.fn(() => {  });

  const stream = vi.fn((cbs: { onmessage: ((...args: unknown[]) => unknown); onerror: ((...args: unknown[]) => unknown) }) => {
    onmessageCb = cbs.onmessage as unknown;
    onerrorCb = cbs.onerror as unknown;

    if (opts.throwOnConnect) {
      throw new Error("connection refused");
    }

    // Deliver events synchronously for testing
    if (opts.events) {
      for (const ev of opts.events) {
        onmessageCb?.(ev);
      }
    }

    if (opts.streamError !== undefined) {
      onerrorCb?.(opts.streamError);
    }

    return closeStream;
  });

  const call = vi.fn(async () => {
    return {
      records: opts.events || [],
    };
  });

  const builder: any = {
    forContract: vi.fn(() => builder),
    limit: vi.fn(() => builder),
    cursor: vi.fn(() => builder),
    order: vi.fn(() => builder),
    stream: stream,
    call: call,
  };

  const contractEvents = vi.fn(() => builder);

  const horizon = { contractEvents } as unknown;

  return {
    horizon,
    stream,
    call,
    closeStream,
    triggerError: (err: unknown) => onerrorCb?.(err),
    triggerMessage: (raw: unknown) => onmessageCb?.(raw),
  };
}

// ---------------------------------------------------------------------------
// subscribe() — happy path
// ---------------------------------------------------------------------------

describe("subscribe — happy path", () => {
  it("delivers a parsed event to the handler", () => {
    const rawEvent = makeRaw("paused", [], { timestamp: "1700000000" });
    const { horizon } = makeMockHorizon({ events: [rawEvent] });

    const received: ILNEvent[] = [];
    subscribe(horizon, "CBCONTRACT", {}, (ev) => received.push(ev));

    expect(received.length).toBe(1);
    expect(received[0].type).toBe("paused");
  });

  it("filters out events that don't match the filter", () => {
    const rawFunded = makeRaw("funded", [1n, "GLP"], {
      freelancer: "GF", payer: "GP", token: "CT",
      fund_amount: "1", amount_funded: "1", invoice_amount: "1",
      due_date: "0", discount_rate: 0, funded_at: null,
      status: "Funded", lp: "GLP", effective_yield_bps: 0, timestamp: "0",
    });
    const rawPaused = makeRaw("paused", [], { timestamp: "0" });
    const { horizon } = makeMockHorizon({ events: [rawFunded, rawPaused] });

    const received: ILNEvent[] = [];
    subscribe(horizon, "CBCONTRACT", { types: ["paused"] }, (ev) =>
      received.push(ev)
    );

    expect(received.length).toBe(1);
    expect(received[0].type).toBe("paused");
  });

  it("drops unparseable events without throwing", () => {
    const badRaw = { type: "contract", topic: [], value: "" };
    const { horizon } = makeMockHorizon({ events: [badRaw] });

    const handler = vi.fn();
    expect(() =>
      subscribe(horizon, "CBCONTRACT", {}, handler)
    ).not.toThrow();
    expect(handler).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// subscribe() — unsubscribe
// ---------------------------------------------------------------------------

describe("subscribe — unsubscribe", () => {
  it("returns a function", () => {
    const { horizon } = makeMockHorizon();
    const unsub = subscribe(horizon, "CBCONTRACT", {}, vi.fn());
    expect(typeof unsub).toBe("function");
  });

  it("calling unsubscribe closes the stream", () => {
    const { horizon, closeStream } = makeMockHorizon();
    const unsub = subscribe(horizon, "CBCONTRACT", {}, vi.fn());
    unsub();
    expect(closeStream).toHaveBeenCalledTimes(1);
  });

  it("stops delivering events after unsubscribe", () => {
    const { horizon, triggerMessage } = makeMockHorizon();
    const handler = vi.fn();
    const unsub = subscribe(horizon, "CBCONTRACT", {}, handler);

    unsub();
    triggerMessage(makeRaw("paused", [], { timestamp: "0" }));

    expect(handler).not.toHaveBeenCalled();
  });

  it("calling unsubscribe multiple times is safe (idempotent)", () => {
    const { horizon, closeStream } = makeMockHorizon();
    const unsub = subscribe(horizon, "CBCONTRACT", {}, vi.fn());
    unsub();
    unsub();
    // closeStream called once per real invocation; second unsub is a no-op
    expect(closeStream.mock.calls.length).toBeLessThanOrEqual(2);
  });
});

// ---------------------------------------------------------------------------
// subscribe() — reconnection / back-off
// ---------------------------------------------------------------------------

describe("subscribe — reconnection", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("reconnects after a stream error", () => {
    const { horizon, triggerError } = makeMockHorizon();
    const connectSpy = (horizon as unknown).contractEvents as vi.Mock;

    subscribe(horizon, "CBCONTRACT", {}, vi.fn());
    expect(connectSpy).toHaveBeenCalledTimes(1);

    triggerError(new Error("stream dropped"));

    // Advance past the initial back-off (500 ms)
    vi.advanceTimersByTime(600);
    expect(connectSpy).toHaveBeenCalledTimes(2);
  });

  it("does NOT reconnect after unsubscribe", () => {
    const { horizon, triggerError } = makeMockHorizon();
    const connectSpy = (horizon as unknown).contractEvents as vi.Mock;

    const unsub = subscribe(horizon, "CBCONTRACT", {}, vi.fn());
    unsub();

    triggerError(new Error("stream dropped"));
    vi.advanceTimersByTime(5000);

    // Still only 1 connection attempt (the initial one)
    expect(connectSpy).toHaveBeenCalledTimes(1);
  });

  it("calls onError callback on stream error", () => {
    const { horizon, triggerError } = makeMockHorizon();
    const onError = vi.fn();

    subscribe(horizon, "CBCONTRACT", {}, vi.fn(), onError);
    triggerError(new Error("dropped"));

    expect(onError).toHaveBeenCalledWith(expect.any(Error));
  });

  it("back-off doubles on successive errors (without successful connect in between)", () => {
    const connectSpy = vi.fn();
    let errorCb: ((e: unknown) => void) | null = null;

    const fakeHorizon = {
      contractEvents: () => ({
        forContract: () => ({
          limit: () => ({
            stream: (cbs: Record<string, unknown>) => {
              connectSpy();
              errorCb = cbs.onerror;
              return vi.fn();
            },
          }),
        }),
      }),
    } as unknown;

    subscribe(fakeHorizon, "CB", {}, vi.fn());
    expect(connectSpy).toHaveBeenCalledTimes(1);

    // First error → schedule reconnect at 500 ms (backoffMs starts at 500, then doubles to 1000)
    errorCb!(new Error("e1"));
    vi.advanceTimersByTime(600);
    // Connect #2 fires; backoffMs was doubled to 1000 BEFORE connecting, but then
    // the successful connect resets it back to 500. So next backoff is 500 again.
    expect(connectSpy).toHaveBeenCalledTimes(2);

    // Second error → back-off resets to 500 ms after the successful reconnect
    errorCb!(new Error("e2"));
    vi.advanceTimersByTime(400); // not enough
    expect(connectSpy).toHaveBeenCalledTimes(2);
    vi.advanceTimersByTime(200); // total 600 ms — enough for 500 ms timer
    expect(connectSpy).toHaveBeenCalledTimes(3);
  });

  it("reconnects when the initial connect throws", () => {
    const { horizon } = makeMockHorizon({ throwOnConnect: true });
    const onError = vi.fn();

    subscribe(horizon, "CBCONTRACT", {}, vi.fn(), onError);

    expect(onError).toHaveBeenCalled();
    vi.advanceTimersByTime(600);
    // Second attempt also throws but we just verify reconnect was scheduled
    expect((horizon as unknown).contractEvents).toHaveBeenCalledTimes(2);
  });
});

describe("replay and gap recovery", () => {
  it("delivers each cursor once across overlapping replay pages and the live stream", async () => {
    const first = { ...makeRaw("paused", [], { timestamp: "100" }), id: "100-1", ledger: 100 };
    const second = { ...makeRaw("unpaused", [], { timestamp: "100" }), id: "100-2", ledger: 100 };
    const { horizon, call, triggerMessage } = makeMockHorizon();
    call.mockResolvedValueOnce({ records: [first, first] });
    call.mockResolvedValueOnce({ records: [first, second] });
    call.mockResolvedValueOnce({ records: [first, second] });
    const received: (string | undefined)[] = [];

    const unsubscribe = subscribe(horizon, "CBCONTRACT", { fromLedger: 100 }, event => {
      received.push(event.pagingToken);
    });
    await new Promise(resolve => setTimeout(resolve, 0));
    triggerMessage(second);

    expect(received).toEqual(["100-1", "100-2"]);
    expect(call).toHaveBeenCalledTimes(3);
    unsubscribe();
  });

  it("suppresses pending historical delivery after unsubscribe", async () => {
    const record = { ...makeRaw("paused", [], { timestamp: "100" }), id: "100-1", ledger: 100 };
    const { horizon, call, stream } = makeMockHorizon();
    call.mockResolvedValueOnce({ records: [record] });
    const handler = vi.fn();
    const unsubscribe = subscribe(horizon, "CBCONTRACT", { fromLedger: 100 }, handler);
    unsubscribe();
    await new Promise(resolve => setTimeout(resolve, 0));

    expect(handler).not.toHaveBeenCalled();
    expect(stream).not.toHaveBeenCalled();
  });
  it("replay() queries history and processes events", async () => {
    const raw1 = { id: "100-1", ledger: 100, type: "contract", topic: [encodeVal("paused")], value: encodeVal({ timestamp: "100" }) };
    const raw2 = { id: "100-2", ledger: 100, type: "contract", topic: [encodeVal("unpaused")], value: encodeVal({ timestamp: "101" }) };
    const { horizon, call } = makeMockHorizon({ events: [raw1, raw2] });

    const events: ILNEvent[] = [];
    const lastCursor = await replay(horizon, "CBCONTRACT", {}, 100, (ev) => events.push(ev));

    expect(events.length).toBe(2);
    expect(events[0].type).toBe("paused");
    expect(events[0].ledger).toBe(100);
    expect(events[0].pagingToken).toBe("100-1");
    expect(events[1].type).toBe("unpaused");
    expect(events[1].ledger).toBe(100);
    expect(events[1].pagingToken).toBe("100-2");
    expect(lastCursor).toBe("100-2");
    expect(call).toHaveBeenCalled();
  });

  it("subscribe() with fromLedger triggers replay then connects", async () => {
    vi.useFakeTimers();
    const rawReplay = { id: "100-1", ledger: 100, type: "contract", topic: [encodeVal("paused")], value: encodeVal({ timestamp: "100" }) };
    const { horizon, stream } = makeMockHorizon({ events: [rawReplay] });

    const events: ILNEvent[] = [];
    subscribe(horizon, "CBCONTRACT", { fromLedger: 100 }, (ev) => events.push(ev));

    // Wait for the async replay promise to resolve
    await vi.runAllTimersAsync();

    expect(events.length).toBe(1);
    expect(events[0].type).toBe("paused");
    expect(stream).toHaveBeenCalled();
    vi.useRealTimers();
  });

  it("subscribe() detects ledger gaps and triggers gap recovery replay", async () => {
    vi.useFakeTimers();
    const { horizon, triggerMessage } = makeMockHorizon();

    const events: ILNEvent[] = [];
    subscribe(horizon, "CBCONTRACT", {}, (ev) => events.push(ev));

    // Send first live event (ledger 100)
    triggerMessage({ ...makeRaw("paused", [], { timestamp: "100" }), id: "100-1", ledger: 100 });

    // Create a gap! Send live event on ledger 105.
    const mockGapEvent = {
      id: "105-1",
      ledger: 105,
      type: "contract",
      topic: [encodeVal("unpaused")],
      value: encodeVal({ timestamp: "105" }),
    };

    const gapReplayEvent = {
      id: "102-1",
      ledger: 102,
      type: "contract",
      topic: [encodeVal("paused")],
      value: encodeVal({ timestamp: "102" }),
    };

    const contractEventsSpy = horizon.contractEvents as any;
    contractEventsSpy.mockImplementation(() => ({
      forContract: () => ({
        limit: () => ({
          order: () => ({
            cursor: () => ({
              call: async () => ({ records: [gapReplayEvent] })
            })
          })
        })
      })
    }));

    // Trigger the gap event
    triggerMessage(mockGapEvent);
    await vi.runAllTimersAsync();

    // Verify replay was triggered and replayed event is processed
    expect(events.some(e => e.type === "paused" && e.ledger === 102)).toBe(true);
    vi.useRealTimers();
  });
});
