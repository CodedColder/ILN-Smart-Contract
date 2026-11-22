import { describe, it, expect } from "vitest";
import {
  ILNError,
  ValidationError,
  AuthorizationError,
  InvoiceStateError,
  ContractExecutionError,
  NetworkError,
} from "./errors.js";

describe("SDK Error Handling", () => {
  describe("fromError mapping", () => {
    it("maps contract error code 1 to ValidationError.InvoiceNotFound", () => {
      const err = ILNError.fromError("Error(Contract, 1)");
      expect(err).toBeInstanceOf(ILNError.InvoiceNotFound);
      expect(err).toBeInstanceOf(ValidationError);
      expect((err as ILNError).originalCode).toBe(1);
      expect((err as ILNError).recommendRetry).toBe(false);
    });

    it("maps contract error code 2 to InvoiceStateError.AlreadyFunded", () => {
      const err = ILNError.fromError("Error(Contract, 2)");
      expect(err).toBeInstanceOf(ILNError.AlreadyFunded);
      expect(err).toBeInstanceOf(InvoiceStateError);
      expect((err as ILNError).originalCode).toBe(2);
      expect((err as ILNError).recommendRetry).toBe(false);
    });

    it("maps contract error code 5 to AuthorizationError.Unauthorized", () => {
      const err = ILNError.fromError("Error(Contract, 5)");
      expect(err).toBeInstanceOf(ILNError.Unauthorized);
      expect(err).toBeInstanceOf(AuthorizationError);
      expect((err as ILNError).originalCode).toBe(5);
      expect((err as ILNError).recommendRetry).toBe(false);
    });

    it("maps contract error code 26 to ContractExecutionError.ContractPaused", () => {
      const err = ILNError.fromError("Error(Contract, 26)");
      expect(err).toBeInstanceOf(ILNError.ContractPaused);
      expect(err).toBeInstanceOf(ContractExecutionError);
      expect((err as ILNError).originalCode).toBe(26);
      expect((err as ILNError).recommendRetry).toBe(false);
    });

    it("maps network timeout to NetworkError with retry recommendation", () => {
      const err = ILNError.fromError("Request timeout on endpoint");
      expect(err).toBeInstanceOf(NetworkError);
      expect((err as ILNError).recommendRetry).toBe(true);
    });

    it("maps ledger synchronization delay to ContractExecutionError with retry recommendation", () => {
      const err = ILNError.fromError("Ledger synchronization delay detected");
      expect(err).toBeInstanceOf(ContractExecutionError);
      expect((err as ILNError).recommendRetry).toBe(true);
    });

    it("preserves context metadata in the mapped error", () => {
      const context = {
        txHash: "0x123",
        ledger: 456,
        contractId: "C123",
      };
      const err = ILNError.fromError("Error(Contract, 1)", context) as ILNError;
      expect(err.txHash).toBe("0x123");
      expect(err.ledger).toBe(456);
      expect(err.contractId).toBe("C123");
      expect(err.originalCode).toBe(1);
    });
  });
  describe("ContractError completeness (contracts/invoice_liquidity/src/errors.rs)", () => {
    // Keep in sync with the on-chain enum. Every variant must map to a distinct,
    // correctly named SDK class carrying the original code.
    const ON_CHAIN: Array<[number, string]> = [
      [1, "InvoiceNotFound"], [2, "AlreadyFunded"], [3, "AlreadyPaid"], [4, "NotFunded"],
      [5, "Unauthorized"], [6, "InvalidAmount"], [7, "InvalidDiscountRate"], [8, "InvalidDueDate"],
      [9, "InvoiceDefaulted"], [10, "NothingToClaim"], [11, "NotYetDefaulted"], [12, "OverfundingRejected"],
      [13, "InvoiceExpired"], [14, "BatchTooLarge"], [15, "AlreadyCancelled"], [16, "AlreadyInitialized"],
      [17, "AlreadyAppealed"], [18, "AppealWindowClosed"], [19, "NotDefaulted"], [20, "AlreadyInQueue"],
      [21, "NotApprovedFunder"], [22, "InvoiceAppealed"], [23, "AlreadyDisputed"], [24, "NotDisputed"],
      [25, "InvoiceDisputed"], [26, "ContractPaused"], [27, "DueDateTooSoon"], [28, "DueDateTooFar"],
      [29, "SelfInvoice"], [30, "OverpaymentRejected"], [31, "PayerReputationTooLow"], [32, "ArithmeticOverflow"],
      [33, "FeeOnTransferToken"], [34, "PayerUnverified"], [35, "OracleDataStale"], [36, "AmountTooSmall"],
      [37, "Reentrancy"], [38, "RateLimited"], [39, "QueueNotMature"], [40, "IncompatibleInterfaceVersion"],
      [41, "OracleCircuitOpen"], [42, "NoPriceSource"], [43, "AllPriceSourcesRejected"],
      [44, "MaxInvoiceAmountExceeded"], [45, "GlobalVolumeCapExceeded"], [46, "InvalidTwapWindow"],
      [47, "QueueFull"], [48, "QueueNotStalled"],
    ];

    it.each(ON_CHAIN)("maps code %i to %s", (code, name) => {
      const err = ILNError.fromError(`Error(Contract, ${code})`) as ILNError;
      expect(err).toBeInstanceOf((ILNError as any)[name]);
      expect(err.code).toBe(code);
      expect(err.originalCode).toBe(code);
    });

    it("accepts the real Soroban `Error(Contract, #N)` spelling", () => {
      const err = ILNError.fromError("HostError: Error(Contract, #3)\nEvent log...");
      expect(err).toBeInstanceOf(ILNError.AlreadyPaid);
    });

    it("does not mislabel on-chain 37/38/39 as client-side errors", () => {
      expect(ILNError.fromError("Error(Contract, #37)")).not.toBeInstanceOf(ILNError.InvoiceNotCancellable);
      expect(ILNError.fromError("Error(Contract, #38)")).not.toBeInstanceOf(ILNError.InvalidAddress);
      expect(ILNError.fromError("Error(Contract, #39)")).not.toBeInstanceOf(ILNError.InvalidTransfer);
    });

    it("client-side-only errors carry no on-chain code", () => {
      expect(new ILNError.InvalidAddress().code).toBeUndefined();
      expect(new ILNError.InvalidTransfer().code).toBeUndefined();
      expect(new ILNError.InvoiceNotCancellable().code).toBeUndefined();
    });

    it("keeps unknown contract codes actionable", () => {
      const err = ILNError.fromError("Error(Contract, #4242)") as ILNError;
      expect(err).toBeInstanceOf(ContractExecutionError);
      expect(err.originalCode).toBe(4242);
      expect(err.message).toMatch(/Unknown ILN contract error code 4242/);
      expect(err.message).toMatch(/upgrade @iln\/sdk/);
      expect(err.recommendRetry).toBe(false);
    });

    it("recommends retry only for time-gated codes", () => {
      for (const code of [38, 39, 48]) {
        expect((ILNError.fromError(`Error(Contract, #${code})`) as ILNError).recommendRetry).toBe(true);
      }
      expect((ILNError.fromError("Error(Contract, #37)") as ILNError).recommendRetry).toBe(false);
    });

    it("wraps non-contract failures in a base ILNError preserving the message", () => {
      const err = ILNError.fromError("something odd") as ILNError;
      expect(err).toBeInstanceOf(ILNError);
      expect(err.message).toContain("something odd");
    });
  });
});
