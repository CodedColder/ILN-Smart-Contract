import { afterEach, vi, describe, it, expect } from 'vitest';
/**
 * Tests for `iln pay` — full and partial payment flows.
 * Issue: #232
 */

import { makePayCommand } from "../../src/commands/pay";
import { printPaymentPreview, printPartialProgress, printSettlementReceipt, computeLpSettlement } from "../../src/commands/pay";
import type { InvoicePayState, PayResult } from "../../src/commands/pay-types";

// ── Fixtures ──────────────────────────────────────────────────────────────────

const INVOICE_ID = "INV-100";

afterEach(() => vi.restoreAllMocks());

function mockInvoice(overrides: Partial<InvoicePayState> = {}): InvoicePayState {
  return {
    id: INVOICE_ID,
    totalAmount: 100,
    remainingAmount: 100,
    token: "USDC",
    payer: "GPAY0000000000000000000000000000000000000000000000000001",
    lp: "GLP00000000000000000000000000000000000000000000000000001",
    ...overrides,
  };
}

function mockResult(overrides: Partial<PayResult> = {}): PayResult {
  return {
    invoiceId: INVOICE_ID,
    txHash: "TX_FULL_PAYMENT",
    paidAmount: 100,
    remainingAmount: 0,
    token: "USDC",
    lpEarnings: 0.5,
    isFullyPaid: true,
    ...overrides,
  };
}

function makePromptYes() {
  return vi.fn().mockResolvedValue(true);
}

function makePromptNo() {
  return vi.fn().mockResolvedValue(false);
}

// ── Full payment ──────────────────────────────────────────────────────────────

describe("iln pay — full payment", () => {
  it("calls executor with full remaining amount when --amount is omitted", async () => {
    const fetcher = vi.fn().mockResolvedValue(mockInvoice());
    const executor = vi.fn().mockResolvedValue(mockResult());
    const cmd = makePayCommand(makePromptYes(), fetcher, executor);

    vi.spyOn(console, "log").mockImplementation(() => {});

    await cmd.parseAsync(["node", "iln", "--id", INVOICE_ID, "--yes"]);

    expect(executor).toHaveBeenCalledWith(INVOICE_ID, 100);
  });

  it("prints settlement receipt after full payment", async () => {
    const fetcher = vi.fn().mockResolvedValue(mockInvoice());
    const executor = vi.fn().mockResolvedValue(mockResult());
    const cmd = makePayCommand(makePromptYes(), fetcher, executor);

    const logs: string[] = [];
    vi.spyOn(console, "log").mockImplementation((...a) => logs.push(a.join(" ")));

    await cmd.parseAsync(["node", "iln", "--id", INVOICE_ID, "--yes"]);

    expect(logs.some((l) => l.includes("FULLY PAID"))).toBe(true);
    expect(logs.some((l) => l.includes("TX_FULL_PAYMENT"))).toBe(true);
  });

  it("shows LP earnings in settlement receipt", async () => {
    const fetcher = vi.fn().mockResolvedValue(mockInvoice());
    const executor = vi.fn().mockResolvedValue(mockResult({ lpEarnings: 0.5 }));
    const cmd = makePayCommand(makePromptYes(), fetcher, executor);

    const logs: string[] = [];
    vi.spyOn(console, "log").mockImplementation((...a) => logs.push(a.join(" ")));

    await cmd.parseAsync(["node", "iln", "--id", INVOICE_ID, "--yes"]);

    expect(logs.some((l) => l.includes("LP earned"))).toBe(true);
  });
});

// ── Partial payment ─────────────────────────────────────────────────

describe("iln pay — partial payment", () => {
  it("calls executor with --amount value for partial payment", async () => {
    const fetcher = vi.fn().mockResolvedValue(mockInvoice());
    const executor = vi.fn().mockResolvedValue(
      mockResult({ paidAmount: 50, remainingAmount: 50, isFullyPaid: false }),
    );
    const cmd = makePayCommand(makePromptYes(), fetcher, executor);

    vi.spyOn(console, "log").mockImplementation(() => {});

    await cmd.parseAsync(["node", "iln", "--id", INVOICE_ID, "--amount", "50", "--yes"]);

    expect(executor).toHaveBeenCalledWith(INVOICE_ID, 50);
  });

  it("prints partial progress line on partial payment", async () => {
    const fetcher = vi.fn().mockResolvedValue(mockInvoice());
    const executor = vi.fn().mockResolvedValue(
      mockResult({ paidAmount: 50, remainingAmount: 50, isFullyPaid: false }),
    );
    const cmd = makePayCommand(makePromptYes(), fetcher, executor);
    const logs: string[] = [];
    vi.spyOn(console, "log").mockImplementation((...a) => logs.push(a.join(" ")));

    await cmd.parseAsync(["node", "iln", "--id", INVOICE_ID, "--amount", "50", "--yes"]);

    expect(logs.some((l) => l.includes("50 USDC remaining"))).toBe(true);
  });

  it("rejects amount exceeding remaining balance", async () => {
    const fetcher = vi.fn().mockResolvedValue(mockInvoice({ remainingAmount: 30 }));
    const executor = vi.fn();
    const cmd = makePayCommand(makePromptYes(), fetcher, executor);

    const errors: string[] = [];
    vi.spyOn(console, "error").mockImplementation((...a) => errors.push(a.join(" ")));
    const exitSpy = vi.spyOn(process, "exit").mockImplementation(() => { throw new Error("exit"); });

    await expect(
      cmd.parseAsync(["node", "iln", "--id", INVOICE_ID, "--amount", "50", "--yes"]),
    ).rejects.toThrow("exit");

    expect(errors.some((e) => e.includes("exceeds remaining balance"))).toBe(true);
    expect(executor).not.toHaveBeenCalled();
    exitSpy.mockRestore();
  });

  it.each(["NaN", "Infinity", "50USDC"])("rejects invalid amount %s before confirmation", async (amount) => {
    const fetcher = vi.fn().mockResolvedValue(mockInvoice());
    const executor = vi.fn();
    const prompter = makePromptYes();
    const cmd = makePayCommand(prompter, fetcher, executor);
    const exitSpy = vi.spyOn(process, "exit").mockImplementation(() => { throw new Error("exit"); });

    try {
      await expect(cmd.parseAsync(["node", "iln", "--id", INVOICE_ID, "--amount", amount])).rejects.toThrow("exit");
      expect(prompter).not.toHaveBeenCalled();
      expect(executor).not.toHaveBeenCalled();
    } finally {
      exitSpy.mockRestore();
    }
  });

  it("does not retry or print a receipt after an unconfirmed submission", async () => {
    const fetcher = vi.fn().mockResolvedValue(mockInvoice());
    const executor = vi.fn().mockRejectedValue(new Error("RPC timed out"));
    const cmd = makePayCommand(makePromptYes(), fetcher, executor);
    const exitSpy = vi.spyOn(process, "exit").mockImplementation(() => { throw new Error("exit"); });
    const errors = vi.spyOn(console, "error").mockImplementation(() => {});
    const logs = vi.spyOn(console, "log").mockImplementation(() => {});

    try {
      await expect(cmd.parseAsync(["node", "iln", "--id", INVOICE_ID, "--yes"])).rejects.toThrow("exit");
      expect(executor).toHaveBeenCalledTimes(1);
      expect(errors).toHaveBeenCalledWith(expect.stringContaining("before retrying"));
      expect(logs).not.toHaveBeenCalledWith(expect.stringContaining("Settlement Receipt"));
    } finally {
      exitSpy.mockRestore();
      errors.mockRestore();
      logs.mockRestore();
    }
  });
});

// ── Payment preview ───────────────────────────────────────────────────────────

describe("iln pay — payment preview", () => {
  it("prints preview before signing", async () => {
    const fetcher = vi.fn().mockResolvedValue(mockInvoice());
    const executor = vi.fn().mockResolvedValue(mockResult());
    const cmd = makePayCommand(makePromptYes(), fetcher, executor);

    const logs: string[] = [];
    vi.spyOn(console, "log").mockImplementation((...a) => logs.push(a.join(" ")));

    await cmd.parseAsync(["node", "iln", "--id", INVOICE_ID, "--yes"]);

    expect(
      logs.some((l) => l.includes("Paying") && l.includes("remaining on Invoice")),
    ).toBe(true);
  });

  it("printPaymentPreview says LP is not paid on a partial payment", () => {
    const logs: string[] = [];
    vi.spyOn(console, "log").mockImplementation((...a) => logs.push(a.join(" ")));

    printPaymentPreview(mockInvoice(), 50);

    expect(logs[0]).toMatch(/LP is paid only when the invoice is fully settled/);
    expect(logs[0]).not.toMatch(/0\.25/);
  });

  it("printPaymentPreview omits LP note when lp is undefined", () => {
    const logs: string[] = [];
    vi.spyOn(console, "log").mockImplementation((...a) => logs.push(a.join(" ")));

    printPaymentPreview(mockInvoice({ lp: undefined }), 50);

    expect(logs[0]).not.toMatch(/LP/);
  });

  it("printPaymentPreview uses the on-chain formula on final payment", () => {
    const logs: string[] = [];
    vi.spyOn(console, "log").mockImplementation((...a) => logs.push(a.join(" ")));

    // fee = 100 * 50bps = 0.5; distribute = 99.5; LP funded 100 of 100 → payout 99.5,
    // earned floored at 0 (payout < funded), still rendered from the formula.
    printPaymentPreview(mockInvoice({ lpFundedAmount: 100, feeRateBps: 50 }), 100);

    expect(logs[0]).toMatch(/on-chain formula/);
  });

  it("printPaymentPreview does not invent a figure without fee/funding data", () => {
    const logs: string[] = [];
    vi.spyOn(console, "log").mockImplementation((...a) => logs.push(a.join(" ")));

    printPaymentPreview(mockInvoice(), 100);

    expect(logs[0]).toMatch(/preview unavailable/);
  });
});

describe("iln pay — computeLpSettlement", () => {
  it("matches mark_paid truncating integer math", () => {
    // 1000 total, 30 bps fee => fee 3, distribute 997, LP funded 1000 => payout 997
    expect(
      computeLpSettlement({ totalAmount: 1000, lpFundedAmount: 1000, feeRateBps: 30 }),
    ).toEqual({ protocolFee: 3, lpPayout: 997, lpEarned: 0 });
  });

  it("floors lpEarned at zero instead of going negative", () => {
    const s = computeLpSettlement({ totalAmount: 100, lpFundedAmount: 50, feeRateBps: 100 });
    expect(s?.lpEarned).toBe(0);
  });

  it.each([
    { totalAmount: 100, lpFundedAmount: undefined, feeRateBps: 30 },
    { totalAmount: 100, lpFundedAmount: 50, feeRateBps: undefined },
    { totalAmount: 0, lpFundedAmount: 0, feeRateBps: 30 },
    { totalAmount: 100, lpFundedAmount: 150, feeRateBps: 30 },
    { totalAmount: 100, lpFundedAmount: 50, feeRateBps: 10_001 },
    { totalAmount: NaN, lpFundedAmount: 50, feeRateBps: 30 },
  ])("returns undefined for missing/invalid input %#", (input) => {
    expect(computeLpSettlement(input)).toBeUndefined();
  });
});

// ── Confirmation prompt ───────────────────────────────────────────────────────

describe("iln pay — confirmation prompt", () => {
  it("prompts user when --yes is not passed", async () => {
    const fetcher = vi.fn().mockResolvedValue(mockInvoice());
    const executor = vi.fn().mockResolvedValue(mockResult());
    const prompter = makePromptYes();
    const cmd = makePayCommand(prompter, fetcher, executor);

    vi.spyOn(console, "log").mockImplementation(() => {});

    await cmd.parseAsync(["node", "iln", "--id", INVOICE_ID]);

    expect(prompter).toHaveBeenCalled();
    expect(executor).toHaveBeenCalled();
  });

  it("skips prompt with --yes flag", async () => {
    const fetcher = vi.fn().mockResolvedValue(mockInvoice());
    const executor = vi.fn().mockResolvedValue(mockResult());
    const prompter = vi.fn();
    const cmd = makePayCommand(prompter, fetcher, executor);

    vi.spyOn(console, "log").mockImplementation(() => {});

    await cmd.parseAsync(["node", "iln", "--id", INVOICE_ID, "--yes"]);

    expect(prompter).not.toHaveBeenCalled();
  });

  it("cancels payment when user declines confirmation", async () => {
    const fetcher = vi.fn().mockResolvedValue(mockInvoice());
    const executor = vi.fn();
    const cmd = makePayCommand(makePromptNo(), fetcher, executor);

    const logs: string[] = [];
    vi.spyOn(console, "log").mockImplementation((...a) => logs.push(a.join(" ")));

    await cmd.parseAsync(["node", "iln", "--id", INVOICE_ID]);

    expect(executor).not.toHaveBeenCalled();
    expect(logs.some((l) => l.includes("Payment cancelled"))).toBe(true);
  });
});

// ── printSettlementReceipt / printPartialProgress unit tests ─────────────

describe("iln pay — receipt helpers", () => {
  it("printSettlementReceipt shows FULLY PAID for complete payment", () => {
    const logs: string[] = [];
    vi.spyOn(console, "log").mockImplementation((...a) => logs.push(a.join(" ")));

    printSettlementReceipt(mockResult());

    expect(logs.some((l) => l.includes("FULLY PAID"))).toBe(true);
  });

  it("printSettlementReceipt shows partial status when not fully paid", () => {
    const logs: string[] = [];
    vi.spyOn(console, "log").mockImplementation((...a) => logs.push(a.join(" ")));

    printSettlementReceipt(mockResult({ isFullyPaid: false, remainingAmount: 50 }));

    expect(logs.some((l) => l.includes("still open"))).toBe(true);
  });

  it("printPartialProgress shows paid and remaining amounts", () => {
    const logs: string[] = [];
    vi.spyOn(console, "log").mockImplementation((...a) => logs.push(a.join(" ")));

    printPartialProgress(mockResult({ paidAmount: 50, remainingAmount: 50, isFullyPaid: false }));

    expect(logs[0]).toMatch(/Paid 50 USDC/);
    expect(logs[0]).toMatch(/50 USDC remaining/);
  });
});
