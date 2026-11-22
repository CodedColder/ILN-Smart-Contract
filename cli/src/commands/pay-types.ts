/**
 * Types for the `iln pay` command.
 * Issue: #232
 */

/** Input options resolved from CLI flags. */
export interface PayOptions {
  /** Invoice ID to pay. */
  id: string;
  /** Amount to pay in USDC. Omit for full payment. */
  amount?: number;
  /** Skip confirmation prompt and pay immediately. */
  yes: boolean;
}

/** Invoice state fetched before payment. */
export interface InvoicePayState {
  id: string;
  totalAmount: number;
  remainingAmount: number;
  token: string;
  payer: string;
  lp?: string;
  /**
   * Face-value amount funded by `lp` (same units as `totalAmount`).
   * Required, together with `feeRateBps`, to compute LP settlement with the
   * on-chain formula. Source: `get_invoice_funders` on the invoice contract.
   */
  lpFundedAmount?: number;
  /**
   * Protocol fee rate in basis points that `mark_paid` will apply to the
   * full invoice amount on settlement (`effective_fee_rate`).
   */
  feeRateBps?: number;
}

/** Result returned after a successful payment. */
export interface PayResult {
  invoiceId: string;
  txHash: string;
  paidAmount: number;
  remainingAmount: number;
  token: string;
  lpEarnings?: number;
  isFullyPaid: boolean;
}
