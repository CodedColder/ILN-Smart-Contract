// @ts-nocheck

export interface ILNErrorContext {
  txHash?: string;
  ledger?: number;
  contractId?: string;
  originalCode?: number;
}

export class ILNError extends Error {
  public readonly txHash?: string;
  public readonly ledger?: number;
  public readonly contractId?: string;
  public readonly originalCode?: number;
  public readonly recommendRetry: boolean;

  constructor(
    message: string,
    public readonly code?: number,
    context?: ILNErrorContext,
    recommendRetry: boolean = false
  ) {
    super(message);
    this.name = this.constructor.name;
    this.txHash = context?.txHash;
    this.ledger = context?.ledger;
    this.contractId = context?.contractId;
    this.originalCode = context?.originalCode ?? code;
    this.recommendRetry = recommendRetry;
    Object.setPrototypeOf(this, new.target.prototype);
  }

  static ValidationError: any;
  static AuthorizationError: any;
  static InvoiceStateError: any;
  static ContractExecutionError: any;
  static NetworkError: any;

  static InvoiceNotFound: any;
  static AlreadyFunded: any;
  static AlreadyPaid: any;
  static NotFunded: any;
  static Unauthorized: any;
  static InvalidAmount: any;
  static InvalidDiscountRate: any;
  static InvalidDueDate: any;
  static InvoiceDefaulted: any;
  static NothingToClaim: any;
  static NotYetDefaulted: any;
  static OverfundingRejected: any;
  static InvoiceExpired: any;
  static BatchTooLarge: any;
  static AlreadyCancelled: any;
  static AlreadyInitialized: any;
  static AlreadyAppealed: any;
  static AppealWindowClosed: any;
  static NotDefaulted: any;
  static AlreadyInQueue: any;
  static NotApprovedFunder: any;
  static InvoiceAppealed: any;
  static AlreadyDisputed: any;
  static NotDisputed: any;
  static InvoiceDisputed: any;
  static ContractPaused: any;
  static DueDateTooSoon: any;
  static DueDateTooFar: any;
  static SelfInvoice: any;
  static OverpaymentRejected: any;
  static PayerReputationTooLow: any;
  static ArithmeticOverflow: any;
  static FeeOnTransferToken: any;
  static PayerUnverified: any;
  static OracleDataStale: any;
  static AmountTooSmall: any;
  static InvoiceNotCancellable: any;
  static InvalidAddress: any;
  static InvalidTransfer: any;
  static InsufficientAmount: any;
  static Reentrancy: any;
  static RateLimited: any;
  static QueueNotMature: any;
  static IncompatibleInterfaceVersion: any;
  static OracleCircuitOpen: any;
  static NoPriceSource: any;
  static AllPriceSourcesRejected: any;
  static MaxInvoiceAmountExceeded: any;
  static GlobalVolumeCapExceeded: any;
  static InvalidTwapWindow: any;
  static QueueFull: any;
  static QueueNotStalled: any;

  static fromError(error: unknown, context?: ILNErrorContext): Error {
    if (error instanceof ILNError) {
      return error;
    }
    const errorString = String(error);
    const lowerError = errorString.toLowerCase();

    // Check for transient network/RPC issues
    if (
      lowerError.includes("timeout") ||
      lowerError.includes("network error") ||
      lowerError.includes("connection refused") ||
      lowerError.includes("fetch failed") ||
      lowerError.includes("abort") ||
      lowerError.includes("socket") ||
      lowerError.includes("rpc failure") ||
      lowerError.includes("rate limit") ||
      lowerError.includes("status: 429") ||
      lowerError.includes("status: 503")
    ) {
      return new ILNError.NetworkError(errorString, context);
    }

    if (
      lowerError.includes("ledger synchronization delay") ||
      lowerError.includes("sync delay") ||
      lowerError.includes("ledger sync")
    ) {
      return new ILNError.ContractExecutionError(errorString, undefined, context, true);
    }

    // Soroban renders contract errors as `Error(Contract, #N)`; accept the
    // legacy `Error(Contract, N)` spelling too.
    const match = errorString.match(/Error\(Contract,\s*#?(\d+)\)/);
    if (match) {
      const code = parseInt(match[1] || "", 10);
      const ctx = { ...context, originalCode: code };
      switch (code) {
        case 1: return new ILNError.InvoiceNotFound(undefined, ctx);
        case 2: return new ILNError.AlreadyFunded(undefined, ctx);
        case 3: return new ILNError.AlreadyPaid(undefined, ctx);
        case 4: return new ILNError.NotFunded(undefined, ctx);
        case 5: return new ILNError.Unauthorized(undefined, ctx);
        case 6: return new ILNError.InvalidAmount(undefined, ctx);
        case 7: return new ILNError.InvalidDiscountRate(undefined, ctx);
        case 8: return new ILNError.InvalidDueDate(undefined, ctx);
        case 9: return new ILNError.InvoiceDefaulted(undefined, ctx);
        case 10: return new ILNError.NothingToClaim(undefined, ctx);
        case 11: return new ILNError.NotYetDefaulted(undefined, ctx);
        case 12: return new ILNError.OverfundingRejected(undefined, ctx);
        case 13: return new ILNError.InvoiceExpired(undefined, ctx);
        case 14: return new ILNError.BatchTooLarge(undefined, ctx);
        case 15: return new ILNError.AlreadyCancelled(undefined, ctx);
        case 16: return new ILNError.AlreadyInitialized(undefined, ctx);
        case 17: return new ILNError.AlreadyAppealed(undefined, ctx);
        case 18: return new ILNError.AppealWindowClosed(undefined, ctx);
        case 19: return new ILNError.NotDefaulted(undefined, ctx);
        case 20: return new ILNError.AlreadyInQueue(undefined, ctx);
        case 21: return new ILNError.NotApprovedFunder(undefined, ctx);
        case 22: return new ILNError.InvoiceAppealed(undefined, ctx);
        case 23: return new ILNError.AlreadyDisputed(undefined, ctx);
        case 24: return new ILNError.NotDisputed(undefined, ctx);
        case 25: return new ILNError.InvoiceDisputed(undefined, ctx);
        case 26: return new ILNError.ContractPaused(undefined, ctx);
        case 27: return new ILNError.DueDateTooSoon(undefined, ctx);
        case 28: return new ILNError.DueDateTooFar(undefined, ctx);
        case 29: return new ILNError.SelfInvoice(undefined, ctx);
        case 30: return new ILNError.OverpaymentRejected(undefined, ctx);
        case 31: return new ILNError.PayerReputationTooLow(undefined, ctx);
        case 32: return new ILNError.ArithmeticOverflow(undefined, ctx);
        case 33: return new ILNError.FeeOnTransferToken(undefined, ctx);
        case 34: return new ILNError.PayerUnverified(undefined, ctx);
        case 35: return new ILNError.OracleDataStale(undefined, ctx);
        case 36: return new ILNError.AmountTooSmall(undefined, ctx);
        // Codes below mirror `ContractError` in contracts/invoice_liquidity/src/errors.rs.
        case 37: return new ILNError.Reentrancy(undefined, ctx);
        case 38: return new ILNError.RateLimited(undefined, ctx);
        case 39: return new ILNError.QueueNotMature(undefined, ctx);
        case 40: return new ILNError.IncompatibleInterfaceVersion(undefined, ctx);
        case 41: return new ILNError.OracleCircuitOpen(undefined, ctx);
        case 42: return new ILNError.NoPriceSource(undefined, ctx);
        case 43: return new ILNError.AllPriceSourcesRejected(undefined, ctx);
        case 44: return new ILNError.MaxInvoiceAmountExceeded(undefined, ctx);
        case 45: return new ILNError.GlobalVolumeCapExceeded(undefined, ctx);
        case 46: return new ILNError.InvalidTwapWindow(undefined, ctx);
        case 47: return new ILNError.QueueFull(undefined, ctx);
        case 48: return new ILNError.QueueNotStalled(undefined, ctx);
      }
      // A well-formed contract error whose code this SDK build does not know
      // (e.g. contract upgraded ahead of the SDK). Keep the code and say what
      // to do instead of returning an opaque generic error.
      return new ContractExecutionError(
        `Unknown ILN contract error code ${code} (${errorString}). ` +
          "The contract may be newer than this SDK; upgrade @iln/sdk or check the contract's ContractError enum.",
        code,
        ctx,
        false
      );
    }
    return new ILNError(errorString, undefined, context, false);
  }
}

export class ValidationError extends ILNError {
  constructor(message: string, code?: number, context?: ILNErrorContext) {
    super(message, code, context, false);
  }
}

export class AuthorizationError extends ILNError {
  constructor(message: string, code?: number, context?: ILNErrorContext) {
    super(message, code, context, false);
  }
}

export class InvoiceStateError extends ILNError {
  constructor(message: string, code?: number, context?: ILNErrorContext) {
    super(message, code, context, false);
  }
}

export class ContractExecutionError extends ILNError {
  constructor(message: string, code?: number, context?: ILNErrorContext, recommendRetry: boolean = false) {
    super(message, code, context, recommendRetry);
  }
}

export class NetworkError extends ILNError {
  constructor(message: string, context?: ILNErrorContext) {
    super(message, undefined, context, true);
  }
}

export class InvoiceNotFound extends ValidationError { constructor(msg = "Invoice not found", context?: ILNErrorContext) { super(msg, 1, context); } }
export class AlreadyFunded extends InvoiceStateError { constructor(msg = "Invoice already funded", context?: ILNErrorContext) { super(msg, 2, context); } }
export class AlreadyPaid extends InvoiceStateError { constructor(msg = "Invoice already paid", context?: ILNErrorContext) { super(msg, 3, context); } }
export class NotFunded extends InvoiceStateError { constructor(msg = "Invoice not funded", context?: ILNErrorContext) { super(msg, 4, context); } }
export class Unauthorized extends AuthorizationError { constructor(msg = "Unauthorized", context?: ILNErrorContext) { super(msg, 5, context); } }
export class InvalidAmount extends ValidationError { constructor(msg = "Invalid amount", context?: ILNErrorContext) { super(msg, 6, context); } }
export class InvalidDiscountRate extends ValidationError { constructor(msg = "Invalid discount rate", context?: ILNErrorContext) { super(msg, 7, context); } }
export class InvalidDueDate extends ValidationError { constructor(msg = "Invalid due date", context?: ILNErrorContext) { super(msg, 8, context); } }
export class InvoiceDefaulted extends InvoiceStateError { constructor(msg = "Invoice defaulted", context?: ILNErrorContext) { super(msg, 9, context); } }
export class NothingToClaim extends InvoiceStateError { constructor(msg = "Nothing to claim", context?: ILNErrorContext) { super(msg, 10, context); } }
export class NotYetDefaulted extends InvoiceStateError { constructor(msg = "Not yet defaulted", context?: ILNErrorContext) { super(msg, 11, context); } }
export class OverfundingRejected extends InvoiceStateError { constructor(msg = "Overfunding rejected", context?: ILNErrorContext) { super(msg, 12, context); } }
export class InvoiceExpired extends InvoiceStateError { constructor(msg = "Invoice expired", context?: ILNErrorContext) { super(msg, 13, context); } }
export class BatchTooLarge extends ValidationError { constructor(msg = "Batch too large", context?: ILNErrorContext) { super(msg, 14, context); } }
export class AlreadyCancelled extends InvoiceStateError { constructor(msg = "Already cancelled", context?: ILNErrorContext) { super(msg, 15, context); } }
export class AlreadyInitialized extends InvoiceStateError { constructor(msg = "Already initialized", context?: ILNErrorContext) { super(msg, 16, context); } }
export class AlreadyAppealed extends InvoiceStateError { constructor(msg = "Already appealed", context?: ILNErrorContext) { super(msg, 17, context); } }
export class AppealWindowClosed extends InvoiceStateError { constructor(msg = "Appeal window closed", context?: ILNErrorContext) { super(msg, 18, context); } }
export class NotDefaulted extends InvoiceStateError { constructor(msg = "Not defaulted", context?: ILNErrorContext) { super(msg, 19, context); } }
export class AlreadyInQueue extends InvoiceStateError { constructor(msg = "Already in queue", context?: ILNErrorContext) { super(msg, 20, context); } }
export class NotApprovedFunder extends AuthorizationError { constructor(msg = "Not approved funder", context?: ILNErrorContext) { super(msg, 21, context); } }
export class InvoiceAppealed extends InvoiceStateError { constructor(msg = "Invoice appealed", context?: ILNErrorContext) { super(msg, 22, context); } }
export class AlreadyDisputed extends InvoiceStateError { constructor(msg = "Already disputed", context?: ILNErrorContext) { super(msg, 23, context); } }
export class NotDisputed extends InvoiceStateError { constructor(msg = "Not disputed", context?: ILNErrorContext) { super(msg, 24, context); } }
export class InvoiceDisputed extends InvoiceStateError { constructor(msg = "Invoice disputed", context?: ILNErrorContext) { super(msg, 25, context); } }
export class ContractPaused extends ContractExecutionError { constructor(msg = "Contract paused", context?: ILNErrorContext) { super(msg, 26, context); } }
export class DueDateTooSoon extends ValidationError { constructor(msg = "Due date too soon", context?: ILNErrorContext) { super(msg, 27, context); } }
export class DueDateTooFar extends ValidationError { constructor(msg = "Due date too far", context?: ILNErrorContext) { super(msg, 28, context); } }
export class SelfInvoice extends ValidationError { constructor(msg = "Self invoice", context?: ILNErrorContext) { super(msg, 29, context); } }
export class OverpaymentRejected extends InvoiceStateError { constructor(msg = "Overpayment rejected", context?: ILNErrorContext) { super(msg, 30, context); } }
export class PayerReputationTooLow extends InvoiceStateError { constructor(msg = "Payer reputation too low", context?: ILNErrorContext) { super(msg, 31, context); } }
export class ArithmeticOverflow extends ContractExecutionError { constructor(msg = "Arithmetic overflow", context?: ILNErrorContext) { super(msg, 32, context); } }
export class FeeOnTransferToken extends ContractExecutionError { constructor(msg = "Fee on transfer token", context?: ILNErrorContext) { super(msg, 33, context); } }
export class PayerUnverified extends AuthorizationError { constructor(msg = "Payer unverified", context?: ILNErrorContext) { super(msg, 34, context); } }
export class OracleDataStale extends ContractExecutionError { constructor(msg = "Oracle data stale", context?: ILNErrorContext) { super(msg, 35, context); } }
export class AmountTooSmall extends ValidationError { constructor(msg = "Amount too small", context?: ILNErrorContext) { super(msg, 36, context); } }
export class InvoiceNotCancellable extends InvoiceStateError { constructor(msg = "Invoice not cancellable", context?: ILNErrorContext) { super(msg, undefined, context); } }
export class InvalidAddress extends ValidationError { constructor(msg = "Invalid address", context?: ILNErrorContext) { super(msg, undefined, context); } }
export class InvalidTransfer extends ContractExecutionError { constructor(msg = "Invalid transfer", context?: ILNErrorContext) { super(msg, undefined, context); } }
export class InsufficientAmount extends ContractExecutionError { constructor(msg = "Insufficient amount", context?: ILNErrorContext) { super(msg, 999, context); } }

// Client-side only (no on-chain code): InvoiceNotCancellable, InvalidAddress,
// InvalidTransfer and InsufficientAmount are raised by SDK pre-checks and never
// produced by `fromError`. Their `code` is therefore undefined (InsufficientAmount
// keeps the 999 sentinel) so they cannot be confused with on-chain codes 37-48.

// On-chain codes 37-48 (see contracts/invoice_liquidity/src/errors.rs).
export class Reentrancy extends ContractExecutionError { constructor(msg = "Reentrant call detected", context?: ILNErrorContext) { super(msg, 37, context); } }
export class RateLimited extends ContractExecutionError { constructor(msg = "Rate limited: cooldown has not elapsed", context?: ILNErrorContext) { super(msg, 38, context, true); } }
export class QueueNotMature extends ContractExecutionError { constructor(msg = "Fund queue not mature yet", context?: ILNErrorContext) { super(msg, 39, context, true); } }
export class IncompatibleInterfaceVersion extends ContractExecutionError { constructor(msg = "Incompatible contract interface version", context?: ILNErrorContext) { super(msg, 40, context); } }
export class OracleCircuitOpen extends ContractExecutionError { constructor(msg = "Oracle circuit breaker is open", context?: ILNErrorContext) { super(msg, 41, context); } }
export class NoPriceSource extends ContractExecutionError { constructor(msg = "No price source available", context?: ILNErrorContext) { super(msg, 42, context); } }
export class AllPriceSourcesRejected extends ContractExecutionError { constructor(msg = "All price sources rejected", context?: ILNErrorContext) { super(msg, 43, context); } }
export class MaxInvoiceAmountExceeded extends ValidationError { constructor(msg = "Invoice amount exceeds the per-invoice cap", context?: ILNErrorContext) { super(msg, 44, context); } }
export class GlobalVolumeCapExceeded extends ContractExecutionError { constructor(msg = "Token volume cap exceeded", context?: ILNErrorContext) { super(msg, 45, context); } }
export class InvalidTwapWindow extends ValidationError { constructor(msg = "Invalid TWAP window", context?: ILNErrorContext) { super(msg, 46, context); } }
export class QueueFull extends InvoiceStateError { constructor(msg = "Fund queue is full", context?: ILNErrorContext) { super(msg, 47, context); } }
export class QueueNotStalled extends ContractExecutionError { constructor(msg = "Fund queue is not stalled", context?: ILNErrorContext, ) { super(msg, 48, context, true); } }

ILNError.ValidationError = ValidationError;
ILNError.AuthorizationError = AuthorizationError;
ILNError.InvoiceStateError = InvoiceStateError;
ILNError.ContractExecutionError = ContractExecutionError;
ILNError.NetworkError = NetworkError;

ILNError.InvoiceNotFound = InvoiceNotFound;
ILNError.AlreadyFunded = AlreadyFunded;
ILNError.AlreadyPaid = AlreadyPaid;
ILNError.NotFunded = NotFunded;
ILNError.Unauthorized = Unauthorized;
ILNError.InvalidAmount = InvalidAmount;
ILNError.InvalidDiscountRate = InvalidDiscountRate;
ILNError.InvalidDueDate = InvalidDueDate;
ILNError.InvoiceDefaulted = InvoiceDefaulted;
ILNError.NothingToClaim = NothingToClaim;
ILNError.NotYetDefaulted = NotYetDefaulted;
ILNError.OverfundingRejected = OverfundingRejected;
ILNError.InvoiceExpired = InvoiceExpired;
ILNError.BatchTooLarge = BatchTooLarge;
ILNError.AlreadyCancelled = AlreadyCancelled;
ILNError.AlreadyInitialized = AlreadyInitialized;
ILNError.AlreadyAppealed = AlreadyAppealed;
ILNError.AppealWindowClosed = AppealWindowClosed;
ILNError.NotDefaulted = NotDefaulted;
ILNError.AlreadyInQueue = AlreadyInQueue;
ILNError.NotApprovedFunder = NotApprovedFunder;
ILNError.InvoiceAppealed = InvoiceAppealed;
ILNError.AlreadyDisputed = AlreadyDisputed;
ILNError.NotDisputed = NotDisputed;
ILNError.InvoiceDisputed = InvoiceDisputed;
ILNError.ContractPaused = ContractPaused;
ILNError.DueDateTooSoon = DueDateTooSoon;
ILNError.DueDateTooFar = DueDateTooFar;
ILNError.SelfInvoice = SelfInvoice;
ILNError.OverpaymentRejected = OverpaymentRejected;
ILNError.PayerReputationTooLow = PayerReputationTooLow;
ILNError.ArithmeticOverflow = ArithmeticOverflow;
ILNError.FeeOnTransferToken = FeeOnTransferToken;
ILNError.PayerUnverified = PayerUnverified;
ILNError.OracleDataStale = OracleDataStale;
ILNError.AmountTooSmall = AmountTooSmall;
ILNError.InvoiceNotCancellable = InvoiceNotCancellable;
ILNError.InvalidAddress = InvalidAddress;
ILNError.InvalidTransfer = InvalidTransfer;
ILNError.InsufficientAmount = InsufficientAmount;
ILNError.Reentrancy = Reentrancy;
ILNError.RateLimited = RateLimited;
ILNError.QueueNotMature = QueueNotMature;
ILNError.IncompatibleInterfaceVersion = IncompatibleInterfaceVersion;
ILNError.OracleCircuitOpen = OracleCircuitOpen;
ILNError.NoPriceSource = NoPriceSource;
ILNError.AllPriceSourcesRejected = AllPriceSourcesRejected;
ILNError.MaxInvoiceAmountExceeded = MaxInvoiceAmountExceeded;
ILNError.GlobalVolumeCapExceeded = GlobalVolumeCapExceeded;
ILNError.InvalidTwapWindow = InvalidTwapWindow;
ILNError.QueueFull = QueueFull;
ILNError.QueueNotStalled = QueueNotStalled;
