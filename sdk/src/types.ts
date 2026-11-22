/**
 * Options for the fundInvoice() SDK method.
 */
export interface FundOptions {
  /**
   * When true, the SDK will reject funding if the oracle price feed is stale
   * or unavailable. Defaults to false.
   */
  requireOracleVerification?: boolean;

  /**
   * Called when the LP's current token allowance is less than the invoice
   * amount and an approval transaction is about to be built.
   *
   * @param params.requiredAmount  - Amount that needs to be approved (in token base units)
   * @param params.currentAllowance - Current allowance the contract holds
   */
  onApprovalRequired?: (params: {
    requiredAmount: bigint;
    currentAllowance: bigint;
  }) => void;

  /**
   * Called immediately after the approval transaction has been submitted to
   * the network (before confirmation).
   *
   * @param params.approveTxHash - Hash of the submitted approval transaction
   */
  onApprovalSent?: (params: { approveTxHash: string }) => void;

  /**
   * Called after the fund_invoice contract call has been simulated and the
   * transaction is ready to sign and submit.
   *
   * @param params.effectiveYieldBps - Annualised yield in basis points derived
   *   from the invoice's discount rate and time-to-due-date
   * @param params.invoiceId         - The invoice being funded
   */
  onFunded?: (params: {
    effectiveYieldBps: number;
    invoiceId: bigint;
  }) => void;
}

/**
 * Return value of a successful fundInvoice() call.
 */
export interface FundResult {
  /** Hash of the fund_invoice transaction. */
  txHash: string;
  /**
   * Annualised effective yield in basis points for this funding position.
   * Derived from discountRate × daysToMaturity / 365.
   */
  effectiveYieldBps: number;
}

/**
 * Minimal view of an invoice returned by get_invoice().
 */
export interface InvoiceView {
  id: bigint;
  /** Token contract address used for this invoice. */
  token: string;
  /** Full invoice amount in token base units. */
  amount: bigint;
  /** Unix timestamp (seconds) when payment is due. */
  dueDate: number;
  /** Discount rate in basis points (e.g. 300 = 3.00 %). */
  discountRate: number;
  /** Current lifecycle status string. */
  status: string;
  /**
   * Face amount already funded (token base units). Used by `fundInvoice` to
   * fund only the remaining amount of a PartiallyFunded invoice. Absent
   * means 0.
   */
  amountFunded?: bigint;
}

/** Allowance query parameters. */
export interface AllowanceParams {
  /** Token contract address (SEP-41 / SAC). */
  tokenAddress: string;
  /** Address of the token owner (the LP). */
  owner: string;
  /** Address that will spend the tokens (the invoice-liquidity contract). */
  spender: string;
}

/** Result of an allowance check. */
export interface AllowanceResult {
  /** Current approved amount in token base units. */
  amount: bigint;
  /** Ledger sequence at which the allowance expires (0 = no expiry stored). */
  expirationLedger: number;
}


/**
 * On-chain metadata for a minted invoice NFT.
 */
export interface InvoiceNftMetadata {
  /** The invoice ID this NFT represents. */
  invoiceId: bigint;
  /** Full invoice amount in stroops. */
  amount: bigint;
  /** Unix timestamp of when the invoice is due. */
  dueDate: number;
  /** Discount rate in basis points (e.g. 300 = 3.00%). */
  discountRate: number;
  /** Token contract address used for the invoice. */
  token: string;
  /** Current owner of the NFT. */
  owner: string;
  /** Timestamp when the NFT was minted. */
  mintedAt: number;
}

/**
 * A single entry in the LP priority queue for invoice funding requests.
 */
export interface LpFundRequest {
  /** Address of the liquidity provider. */
  lp: string;
  /** LP reputation score snapshotted at request time (used for ordering). */
  score: number;
}

/**
 * Appeal record stored per invoice when a default is contested.
 */
export interface AppealRecord {
  /** SHA-256 hash of off-chain evidence submitted by the payer. */
  evidenceHash: string;
  /** Ledger timestamp when the appeal was filed. */
  appealedAt: number;
  /** Payer reputation score just before the default was applied. */
  preDefaultScore: number;
}

/**
 * Dispute record stored per invoice.
 */
export interface DisputeRecord {
  /** SHA-256 hash of off-chain dispute evidence. */
  reasonHash: string;
  /** Ledger sequence when the dispute was filed. */
  disputedAt: number;
}

/**
 * A single entry in the top-payers leaderboard.
 */
export interface TopPayerEntry {
  /** Address of the payer. */
  address: string;
  /** Reputation score. */
  score: number;
}

/**
 * Reputation profile tracked per address.
 */
export interface ReputationProfile {
  /** The address this profile belongs to. */
  address: string;
  /** Number of invoices submitted. */
  invoicesSubmitted: number;
  /** Number of invoices paid. */
  invoicesPaid: number;
  /** Number of invoices defaulted. */
  invoicesDefaulted: number;
  /** Reputation score. */
  score: number;
}

/**
 * Aggregate contract-wide statistics.
 */
export interface ContractStats {
  /** Total number of invoices created. */
  totalInvoices: bigint;
  /** Total number of invoices funded. */
  totalFunded: bigint;
  /** Total number of invoices paid. */
  totalPaid: bigint;
  /** Total volume in USDC (token base units). */
  totalVolumeUsdc: bigint;
  /** Total volume in EURC (token base units). */
  totalVolumeEurc: bigint;
  /** Total volume in XLM (stroops). */
  totalVolumeXlm: bigint;
  /** Per-token volume breakdown: [tokenAddress, volume][]. */
  tokenVolumes: [string, bigint][];
  /** Total volume normalized to USD (token base units). */
  totalVolumeUsdNormalized: bigint;
}
