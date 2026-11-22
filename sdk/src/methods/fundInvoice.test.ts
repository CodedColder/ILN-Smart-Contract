import { vi, describe, it, expect, beforeEach } from "vitest";
/**
 * Tests for fundInvoice() — covers:
 *   - pre-approved path (no approval needed)
 *   - approval-needed path (two-step flow)
 *   - insufficient balance / error propagation
 *   - oracle verification guard
 *   - callback invocation (onApprovalRequired, onApprovalSent, onFunded)
 *   - computeEffectiveYieldBps helper
 */

import { fundInvoice, computeEffectiveYieldBps, computeFundingCost } from "./fundInvoice.js";
import { SorobanRpc, Networks, Account, Keypair, scValToNative } from "@stellar/stellar-sdk";

// Mock scValToNative to control invoice decoding in tests, and Transaction
// so signAndSubmit doesn't attempt to parse fake XDR strings.
vi.mock("@stellar/stellar-sdk", async () => {
  const actual = await vi.importActual<typeof import("@stellar/stellar-sdk")>("@stellar/stellar-sdk");
  class MockTransaction {
    sign = vi.fn();
    constructor() {}
  }
  return {
    ...actual,
    scValToNative: vi.fn(),
    Transaction: MockTransaction,
  };
});

// ---------------------------------------------------------------------------
// Mocks
// ---------------------------------------------------------------------------

// Mock the allowance utilities so fundInvoice tests are isolated
vi.mock("../utils/allowance.js", () => ({
  getAllowance: vi.fn(),
  getTokenBalance: vi.fn(),
  buildApproveTransaction: vi.fn(),
  isAllowanceSufficient: vi.fn(),
}));

import {
  getAllowance,
  getTokenBalance,
  buildApproveTransaction,
  isAllowanceSufficient,
} from "../utils/allowance.js";
import { ILNError } from "../errors.js";

const mockGetAllowance = getAllowance as vi.MockedFunction<typeof getAllowance>;
const mockGetTokenBalance = getTokenBalance as vi.MockedFunction<typeof getTokenBalance>;
const mockBuildApprove = buildApproveTransaction as vi.MockedFunction<
  typeof buildApproveTransaction
>;
const mockIsAllowanceSufficient = isAllowanceSufficient as vi.MockedFunction<
  typeof isAllowanceSufficient
>;

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

const LP_SECRET = "SAQFWOYMQZD3ZQ2GMY7IXITDC7IDYMZYVEKB5LOXTMT2MCPDEUUXEN2E";
const LP_KEYPAIR = Keypair.fromSecret(LP_SECRET);
const CONTRACT_ID = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";
const TOKEN_ID = "CAAQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABDQF";

// Minimal invoice fixture (status Pending, 30-day maturity, 3% discount)
const MOCK_INVOICE = {
  id: 1n,
  token: TOKEN_ID,
  amount: 1_000_000n, // 1 USDC
  dueDate: Math.floor(Date.now() / 1000) + 86400 * 30,
  discountRate: 300, // 3.00%
  status: "Pending",
};

// Server mock — we stub all calls individually per test
const mockServer = {
  getAccount: vi.fn(),
  simulateTransaction: vi.fn(),
  getLatestLedger: vi.fn(),
  prepareTransaction: vi.fn(),
  sendTransaction: vi.fn(),
} as unknown as SorobanRpc.Server;

// Helper: make getAccount resolve with the LP's fake account data
function mockAccountLoad(seq = "100") {
  (mockServer.getAccount as vi.Mock).mockResolvedValue({ sequence: seq });
}

// scValToNative mock reference
const mockScValToNative = scValToNative as ReturnType<typeof vi.fn>;

beforeEach(() => {
  vi.clearAllMocks();
  // Default: LP can afford the invoice
  mockGetTokenBalance.mockResolvedValue(10_000_000n);
  // Default: scValToNative returns the mock invoice struct
  mockScValToNative.mockReturnValue({
    id: "1",
    token: TOKEN_ID,
    amount: "1000000",
    due_date: MOCK_INVOICE.dueDate,
    discount_rate: 300,
    status: "Pending",
  });
});

// ---------------------------------------------------------------------------
// computeEffectiveYieldBps
// ---------------------------------------------------------------------------

describe("computeEffectiveYieldBps", () => {
  const nowUnix = 1_700_000_000;

  it("calculates correct yield for 30-day 3% discount", () => {
    const dueDate = nowUnix + 86400 * 30;
    const bps = computeEffectiveYieldBps(300, dueDate, nowUnix);
    // 300 * 30 / 365 ≈ 24
    expect(bps).toBe(Math.round((300 * 30) / 365));
  });

  it("returns 0 when due date is in the past", () => {
    const dueDate = nowUnix - 1;
    expect(computeEffectiveYieldBps(300, dueDate, nowUnix)).toBe(0);
  });

  it("returns 0 when due date equals now", () => {
    expect(computeEffectiveYieldBps(300, nowUnix, nowUnix)).toBe(0);
  });

  it("scales linearly with discount rate", () => {
    const dueDate = nowUnix + 86400 * 365;
    const bps = computeEffectiveYieldBps(500, dueDate, nowUnix);
    expect(bps).toBe(500); // 500 * 365 / 365 = 500
  });

  it("uses Date.now() when nowUnix is omitted", () => {
    const dueDate = Math.floor(Date.now() / 1000) + 86400 * 30;
    const bps = computeEffectiveYieldBps(300, dueDate);
    expect(bps).toBeGreaterThan(0);
    expect(bps).toBeLessThanOrEqual(300);
  });
});

// ---------------------------------------------------------------------------
// fundInvoice — pre-approved path (no approval needed)
// ---------------------------------------------------------------------------

describe("fundInvoice — pre-approved path", () => {
  beforeEach(() => {
    mockAccountLoad("100");
    mockIsAllowanceSufficient.mockReturnValue(true);
    mockGetAllowance.mockResolvedValue({
      amount: 10_000_000n,
      expirationLedger: 9999,
    });
    (mockServer.getLatestLedger as vi.Mock).mockResolvedValue({
      sequence: 200,
    });
    (mockServer.simulateTransaction as vi.Mock).mockResolvedValue({
      result: { retval: { _stub: true } },
    });
    (mockServer.prepareTransaction as vi.Mock).mockImplementation(
      async (tx) => ({
        ...tx,
        sign: vi.fn(),
        toEnvelope: () => ({ toXDR: () => "xdr" }),
      })
    );
    (mockServer.sendTransaction as vi.Mock).mockResolvedValue({
      status: "PENDING",
      hash: "abc123fundtxhash",
    });
  });

  it("returns txHash and effectiveYieldBps without firing approval callbacks", async () => {
    const onApprovalRequired = vi.fn();
    const onApprovalSent = vi.fn();
    const onFunded = vi.fn();

    const result = await fundInvoice(
      mockServer,
      CONTRACT_ID,
      LP_KEYPAIR,
      1n,
      { onApprovalRequired, onApprovalSent, onFunded },
      Networks.TESTNET
    );

    expect(result.txHash).toBe("abc123fundtxhash");
    expect(result.effectiveYieldBps).toBeGreaterThanOrEqual(0);
    expect(onApprovalRequired).not.toHaveBeenCalled();
    expect(onApprovalSent).not.toHaveBeenCalled();
    expect(onFunded).toHaveBeenCalledWith(
      expect.objectContaining({ invoiceId: 1n })
    );
  });

  it("does NOT call buildApproveTransaction when allowance is sufficient", async () => {
    await fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 1n, {}, Networks.TESTNET);
    expect(mockBuildApprove).not.toHaveBeenCalled();
  });

  it("calls onFunded with correct effectiveYieldBps", async () => {
    const onFunded = vi.fn();
    await fundInvoice(
      mockServer,
      CONTRACT_ID,
      LP_KEYPAIR,
      1n,
      { onFunded },
      Networks.TESTNET
    );
    expect(onFunded).toHaveBeenCalledWith(
      expect.objectContaining({ effectiveYieldBps: expect.any(Number) })
    );
  });
});

// ---------------------------------------------------------------------------
// fundInvoice — approval-needed path (two-step flow)
// ---------------------------------------------------------------------------

describe("fundInvoice — approval-needed path", () => {
  beforeEach(() => {
    mockAccountLoad("100");
    mockIsAllowanceSufficient.mockReturnValue(false);
    mockGetAllowance.mockResolvedValue({
      amount: 0n,
      expirationLedger: 0,
    });
    mockBuildApprove.mockResolvedValue("APPROVALXDR==");
    (mockServer.getLatestLedger as vi.Mock).mockResolvedValue({
      sequence: 200,
    });
    (mockServer.simulateTransaction as vi.Mock).mockResolvedValue({
      result: { retval: { _stub: true } },
    });
    (mockServer.prepareTransaction as vi.Mock).mockImplementation(
      async (tx) => ({
        ...tx,
        sign: vi.fn(),
        toEnvelope: () => ({ toXDR: () => "xdr" }),
      })
    );

    // sendTransaction: first call = approve tx, second call = fund tx
    (mockServer.sendTransaction as vi.Mock)
      .mockResolvedValueOnce({ status: "PENDING", hash: "approvetxhash" })
      .mockResolvedValueOnce({ status: "PENDING", hash: "fundtxhash" });
  });

  it("fires onApprovalRequired with requiredAmount and currentAllowance", async () => {
    const onApprovalRequired = vi.fn();

    await fundInvoice(
      mockServer,
      CONTRACT_ID,
      LP_KEYPAIR,
      1n,
      { onApprovalRequired },
      Networks.TESTNET
    );

    expect(onApprovalRequired).toHaveBeenCalledWith({
      requiredAmount: 1_000_000n,
      currentAllowance: 0n,
    });
  });

  it("fires onApprovalSent with the approve tx hash", async () => {
    const onApprovalSent = vi.fn();

    await fundInvoice(
      mockServer,
      CONTRACT_ID,
      LP_KEYPAIR,
      1n,
      { onApprovalSent },
      Networks.TESTNET
    );

    expect(onApprovalSent).toHaveBeenCalledWith({
      approveTxHash: "approvetxhash",
    });
  });

  it("calls buildApproveTransaction with correct parameters", async () => {
    await fundInvoice(
      mockServer,
      CONTRACT_ID,
      LP_KEYPAIR,
      1n,
      {},
      Networks.TESTNET
    );

    expect(mockBuildApprove).toHaveBeenCalledWith(
      mockServer,
      TOKEN_ID,
      expect.any(Account),
      CONTRACT_ID,
      1_000_000n,
      Networks.TESTNET
    );
  });

  it("returns the fund tx hash (not the approve tx hash)", async () => {
    const result = await fundInvoice(
      mockServer,
      CONTRACT_ID,
      LP_KEYPAIR,
      1n,
      {},
      Networks.TESTNET
    );

    expect(result.txHash).toBe("fundtxhash");
  });

  it("sends two transactions total (approve + fund)", async () => {
    await fundInvoice(
      mockServer,
      CONTRACT_ID,
      LP_KEYPAIR,
      1n,
      {},
      Networks.TESTNET
    );

    expect(mockServer.sendTransaction).toHaveBeenCalledTimes(2);
  });
});

// ---------------------------------------------------------------------------
// fundInvoice — error paths
// ---------------------------------------------------------------------------

describe("fundInvoice — error handling", () => {
  beforeEach(() => {
    mockAccountLoad("100");
    (mockServer.getLatestLedger as vi.Mock).mockResolvedValue({
      sequence: 200,
    });
  });

  it("throws when get_invoice simulation fails", async () => {
    (mockServer.simulateTransaction as vi.Mock).mockResolvedValue({
      error: "contract trap",
      _parsed: true,
    });

    await expect(
      fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 99n, {}, Networks.TESTNET)
    ).rejects.toThrow();
  });

  it("throws when invoice status is not Pending or PartiallyFunded", async () => {
    mockScValToNative.mockReturnValue({
      id: "1",
      token: TOKEN_ID,
      amount: "1000000",
      due_date: MOCK_INVOICE.dueDate,
      discount_rate: 300,
      status: "Paid",
    });
    (mockServer.simulateTransaction as vi.Mock).mockResolvedValue({
      result: { retval: { _stub: true } },
    });
    mockGetAllowance.mockResolvedValue({ amount: 999999999n, expirationLedger: 9999 });
    mockIsAllowanceSufficient.mockReturnValue(true);

    await expect(
      fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 1n, {}, Networks.TESTNET)
    ).rejects.toThrow("cannot be funded");
  });

  it("throws when fund_invoice transaction errors", async () => {
    mockIsAllowanceSufficient.mockReturnValue(true);
    mockGetAllowance.mockResolvedValue({ amount: 999999999n, expirationLedger: 9999 });
    (mockServer.simulateTransaction as vi.Mock).mockResolvedValue({
      result: { retval: { _stub: true } },
    });
    (mockServer.prepareTransaction as vi.Mock).mockImplementation(
      async (tx) => ({ ...tx, sign: vi.fn(), toEnvelope: () => ({ toXDR: () => "xdr" }) })
    );
    (mockServer.sendTransaction as vi.Mock).mockResolvedValue({
      status: "ERROR",
      errorResult: { code: "INSUFFICIENT_BALANCE" },
    });

    await expect(
      fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 1n, {}, Networks.TESTNET)
    ).rejects.toThrow("fund_invoice failed");
  });

  it("throws when approve transaction errors", async () => {
    mockIsAllowanceSufficient.mockReturnValue(false);
    mockGetAllowance.mockResolvedValue({ amount: 0n, expirationLedger: 0 });
    mockBuildApprove.mockResolvedValue("APPROVALXDR==");
    (mockServer.simulateTransaction as vi.Mock).mockResolvedValue({
      result: { retval: { _stub: true } },
    });
    (mockServer.sendTransaction as vi.Mock).mockResolvedValue({
      status: "ERROR",
      errorResult: { code: "INSUFFICIENT_BALANCE" },
    });

    await expect(
      fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 1n, {}, Networks.TESTNET)
    ).rejects.toThrow("Transaction failed");
  });

  it("throws when getAccount fails (network error)", async () => {
    (mockServer.getAccount as vi.Mock).mockRejectedValue(
      new Error("Network unreachable")
    );

    await expect(
      fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 1n, {}, Networks.TESTNET)
    ).rejects.toThrow("Network unreachable");
  });
});

// ---------------------------------------------------------------------------
// fundInvoice — PartiallyFunded status is allowed
// ---------------------------------------------------------------------------

describe("fundInvoice — PartiallyFunded invoice", () => {
  beforeEach(() => {
    mockAccountLoad("100");
    mockIsAllowanceSufficient.mockReturnValue(true);
    mockGetAllowance.mockResolvedValue({ amount: 999999999n, expirationLedger: 9999 });
    (mockServer.getLatestLedger as vi.Mock).mockResolvedValue({ sequence: 200 });
    (mockServer.simulateTransaction as vi.Mock).mockResolvedValue({
      result: { retval: { _stub: true } },
    });
    (mockServer.prepareTransaction as vi.Mock).mockImplementation(
      async (tx) => ({ ...tx, sign: vi.fn(), toEnvelope: () => ({ toXDR: () => "xdr" }) })
    );
    (mockServer.sendTransaction as vi.Mock).mockResolvedValue({
      status: "PENDING",
      hash: "partialtxhash",
    });
  });

  it("succeeds for PartiallyFunded invoices", async () => {
    mockScValToNative.mockReturnValue({
      id: "1",
      token: TOKEN_ID,
      amount: "1000000",
      due_date: MOCK_INVOICE.dueDate,
      discount_rate: 300,
      status: "PartiallyFunded",
    });

    const result = await fundInvoice(
      mockServer,
      CONTRACT_ID,
      LP_KEYPAIR,
      1n,
      {},
      Networks.TESTNET
    );

    expect(result.txHash).toBe("partialtxhash");
  });
});

// ---------------------------------------------------------------------------
// fundInvoice — oracle verification
// ---------------------------------------------------------------------------

describe("fundInvoice — oracle verification", () => {
  beforeEach(() => {
    mockAccountLoad("100");
    mockIsAllowanceSufficient.mockReturnValue(true);
    mockGetAllowance.mockResolvedValue({ amount: 999999999n, expirationLedger: 9999 });
    (mockServer.getLatestLedger as vi.Mock).mockResolvedValue({ sequence: 200 });
  });

  it("throws when requireOracleVerification=true and oracle simulation errors", async () => {
    // First sim call = get_invoice (succeeds), second = get_price_oracle (errors)
    (mockServer.simulateTransaction as vi.Mock)
      .mockResolvedValueOnce({ result: { retval: { _stub: true } } }) // get_invoice
      .mockResolvedValueOnce({ error: "no oracle", _parsed: true }); // get_price_oracle

    await expect(
      fundInvoice(
        mockServer,
        CONTRACT_ID,
        LP_KEYPAIR,
        1n,
        { requireOracleVerification: true },
        Networks.TESTNET
      )
    ).rejects.toThrow("Oracle verification failed");
  });

  it("throws when requireOracleVerification=true and oracle retval is null", async () => {
    (mockServer.simulateTransaction as vi.Mock)
      .mockResolvedValueOnce({ result: { retval: { _stub: true } } }) // get_invoice
      .mockResolvedValueOnce({ result: { retval: null } }); // get_price_oracle

    // scValToNative on null retval path won't be reached; the null check fires first
    await expect(
      fundInvoice(
        mockServer,
        CONTRACT_ID,
        LP_KEYPAIR,
        1n,
        { requireOracleVerification: true },
        Networks.TESTNET
      )
    ).rejects.toThrow("Oracle verification required");
  });

  it("proceeds when requireOracleVerification=false (default)", async () => {
    (mockServer.simulateTransaction as vi.Mock).mockResolvedValue({
      result: { retval: { _stub: true } },
    });
    (mockServer.prepareTransaction as vi.Mock).mockImplementation(
      async (tx) => ({ ...tx, sign: vi.fn(), toEnvelope: () => ({ toXDR: () => "xdr" }) })
    );
    (mockServer.sendTransaction as vi.Mock).mockResolvedValue({
      status: "PENDING",
      hash: "txhash",
    });

    const result = await fundInvoice(
      mockServer,
      CONTRACT_ID,
      LP_KEYPAIR,
      1n,
      {}, // requireOracleVerification defaults to false
      Networks.TESTNET
    );
    expect(result.txHash).toBe("txhash");
  });
});

// ---------------------------------------------------------------------------
// fundInvoice — callbacks not required (all optional)
// ---------------------------------------------------------------------------

describe("fundInvoice — optional callbacks", () => {
  it("works without any callbacks supplied", async () => {
    mockAccountLoad("100");
    mockIsAllowanceSufficient.mockReturnValue(true);
    mockGetAllowance.mockResolvedValue({ amount: 999999999n, expirationLedger: 9999 });
    (mockServer.getLatestLedger as vi.Mock).mockResolvedValue({ sequence: 200 });
    (mockServer.simulateTransaction as vi.Mock).mockResolvedValue({
      result: { retval: { _stub: true } },
    });
    (mockServer.prepareTransaction as vi.Mock).mockImplementation(
      async (tx) => ({ ...tx, sign: vi.fn(), toEnvelope: () => ({ toXDR: () => "xdr" }) })
    );
    (mockServer.sendTransaction as vi.Mock).mockResolvedValue({
      status: "PENDING",
      hash: "nocalltxhash",
    });

    await expect(
      fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 1n)
    ).resolves.toMatchObject({ txHash: "nocalltxhash" });
  });
});

// ---------------------------------------------------------------------------
// fundInvoice — allowance / balance / simulation audit (issue: SDK allowance handling)
// ---------------------------------------------------------------------------

describe("computeFundingCost", () => {
  it("mirrors the contract's floor-division discount", () => {
    // 1_000_000 * 300 / 10_000 = 30_000 → cost 970_000
    expect(computeFundingCost(1_000_000n, 300)).toBe(970_000n);
    // 999 * 300 / 10_000 = 29.97 → floor 29 → cost 970
    expect(computeFundingCost(999n, 300)).toBe(970n);
  });
});

describe("fundInvoice — allowance, balance and simulation guards", () => {
  beforeEach(() => {
    mockAccountLoad("100");
    mockIsAllowanceSufficient.mockReturnValue(true);
    mockGetAllowance.mockResolvedValue({ amount: 10_000_000n, expirationLedger: 9999 });
    (mockServer.getLatestLedger as vi.Mock).mockResolvedValue({ sequence: 200 });
    (mockServer.simulateTransaction as vi.Mock).mockResolvedValue({
      result: { retval: { _stub: true } },
    });
    (mockServer.prepareTransaction as vi.Mock).mockImplementation(async (tx) => ({
      ...tx,
      sign: vi.fn(),
      toEnvelope: () => ({ toXDR: () => "xdr" }),
    }));
    (mockServer.sendTransaction as vi.Mock).mockResolvedValue({
      status: "PENDING",
      hash: "fundhash",
    });
    mockBuildApprove.mockResolvedValue("APPROVALXDR==");
  });

  it("rejects with InsufficientAmount before signing or approving when balance < cost", async () => {
    mockGetTokenBalance.mockResolvedValue(969_999n); // cost is 970_000
    mockIsAllowanceSufficient.mockReturnValue(false);

    await expect(
      fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 1n, {}, Networks.TESTNET)
    ).rejects.toBeInstanceOf(ILNError.InsufficientAmount);

    expect(mockBuildApprove).not.toHaveBeenCalled();
    expect(mockServer.prepareTransaction).not.toHaveBeenCalled();
    expect(mockServer.sendTransaction).not.toHaveBeenCalled();
  });

  it("allows funding when balance exactly equals the discounted cost", async () => {
    mockGetTokenBalance.mockResolvedValue(970_000n);
    const result = await fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 1n, {}, Networks.TESTNET);
    expect(result.txHash).toBe("fundhash");
  });

  it("funds only the unfunded remainder of a PartiallyFunded invoice", async () => {
    mockScValToNative.mockReturnValue({
      id: "1",
      token: TOKEN_ID,
      amount: "1000000",
      amount_funded: "400000",
      due_date: MOCK_INVOICE.dueDate,
      discount_rate: 300,
      status: "PartiallyFunded",
    });
    mockIsAllowanceSufficient.mockReturnValue(false);
    mockGetAllowance.mockResolvedValue({ amount: 0n, expirationLedger: 0 });
    const onApprovalRequired = vi.fn();

    await fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 1n, { onApprovalRequired }, Networks.TESTNET);

    expect(onApprovalRequired).toHaveBeenCalledWith({ requiredAmount: 600_000n, currentAllowance: 0n });
    expect(mockIsAllowanceSufficient).toHaveBeenCalledWith(expect.anything(), 600_000n, expect.any(Number));
    // cost of the remainder: 600_000 - 18_000
    expect(mockGetTokenBalance).toHaveBeenCalledTimes(1);
  });

  it("calls fund_invoice with the contract's 4 arguments incl. require_oracle_verification", async () => {
    await fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 1n, {}, Networks.TESTNET);
    const tx = (mockServer.prepareTransaction as vi.Mock).mock.calls[0][0];
    const args = tx.operations[0].func.invokeContract().args();
    expect(args).toHaveLength(4);
  });

  it("passes the networkPassphrase to the allowance and balance queries", async () => {
    await fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 1n, {}, Networks.PUBLIC);
    expect(mockGetAllowance).toHaveBeenCalledWith(
      mockServer, expect.anything(), expect.anything(), Networks.PUBLIC
    );
    expect(mockGetTokenBalance).toHaveBeenCalledWith(
      mockServer, TOKEN_ID, LP_KEYPAIR.publicKey(), expect.anything(), Networks.PUBLIC
    );
  });

  it("simulates fund_invoice before submitting the approve and surfaces a typed error", async () => {
    mockIsAllowanceSufficient.mockReturnValue(false);
    mockGetAllowance.mockResolvedValue({ amount: 0n, expirationLedger: 0 });
    (mockServer.prepareTransaction as vi.Mock).mockRejectedValue(
      new Error("HostError: Error(Contract, #12)")
    );

    await expect(
      fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 1n, {}, Networks.TESTNET)
    ).rejects.toBeInstanceOf(ILNError.OverfundingRejected);

    expect(mockBuildApprove).not.toHaveBeenCalled();
    expect(mockServer.sendTransaction).not.toHaveBeenCalled();
  });

  it("builds the fund tx at sequence + 1 when an approve is submitted first", async () => {
    mockIsAllowanceSufficient.mockReturnValue(false);
    mockGetAllowance.mockResolvedValue({ amount: 0n, expirationLedger: 0 });

    await fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 1n, {}, Networks.TESTNET);

    const fundTx = (mockServer.prepareTransaction as vi.Mock).mock.calls[0][0];
    expect(fundTx.sequence).toBe("102"); // account seq 100 → tx seq 101 (approve) → 102 (fund)
  });

  it("throws and does not report success when sendTransaction returns TRY_AGAIN_LATER", async () => {
    (mockServer.sendTransaction as vi.Mock).mockResolvedValue({ status: "TRY_AGAIN_LATER", hash: "h" });
    const onFunded = vi.fn();

    await expect(
      fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 1n, { onFunded }, Networks.TESTNET)
    ).rejects.toThrow("TRY_AGAIN_LATER");
    expect(onFunded).not.toHaveBeenCalled();
  });

  it("does not submit the fund tx when the approve is not accepted", async () => {
    mockIsAllowanceSufficient.mockReturnValue(false);
    mockGetAllowance.mockResolvedValue({ amount: 0n, expirationLedger: 0 });
    (mockServer.sendTransaction as vi.Mock).mockResolvedValue({ status: "TRY_AGAIN_LATER", hash: "h" });

    await expect(
      fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 1n, {}, Networks.TESTNET)
    ).rejects.toThrow("TRY_AGAIN_LATER");
    expect(mockServer.sendTransaction).toHaveBeenCalledTimes(1);
  });

  it("rejects an invoice with nothing left to fund", async () => {
    mockScValToNative.mockReturnValue({
      id: "1", token: TOKEN_ID, amount: "1000000", amount_funded: "1000000",
      due_date: MOCK_INVOICE.dueDate, discount_rate: 300, status: "PartiallyFunded",
    });
    await expect(
      fundInvoice(mockServer, CONTRACT_ID, LP_KEYPAIR, 1n, {}, Networks.TESTNET)
    ).rejects.toThrow("no remaining amount");
  });
});
