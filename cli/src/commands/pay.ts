/**
 * `iln pay` command — pay an invoice in full or partially.
 *
 * Usage:
 *   iln pay --id X              → full payment
 *   iln pay --id X --amount 50  → partial payment of 50 USDC
 *   iln pay --id X --yes        → skip confirmation prompt
 *
 * Issue: #232
 */

import { Command } from "commander";
import type { PayOptions, InvoicePayState, PayResult } from "./pay-types";

// ── Injected dependency types ─────────────────────────────────────────────────

/** Fetches current invoice state (remaining amount, token, payer, etc.) */
export type InvoiceFetcher = (id: string) => Promise<InvoicePayState>;

/** Executes the payment and returns a PayResult. */
export type PayExecutor = (
  id: string,
  amount: number,
) => Promise<PayResult>;

/** Prompts the user for a y/n confirmation. Returns true if confirmed. */
export type Prompter = (message: string) => Promise<boolean>;

// ── Preview printer ───────────────────────────────────────────────────────────

/**
 * Prints the payment preview line shown before signing.
 *
 * LP note rules (mirrors `mark_paid` in contracts/invoice_liquidity):
 *  - Partial payment: funders are NOT paid; the contract only records
 *    `amount_paid`. The note says so instead of showing an LP figure.
 *  - Final payment with fee/funding data: LP payout is computed with the
 *    on-chain formula (see {@link computeLpSettlement}).
 *  - Final payment without that data: no number is invented; the note says
 *    the LP amount cannot be previewed.
 */
export function printPaymentPreview(
  invoice: InvoicePayState,
  payAmount: number,
): void {
  let lpNote = "";
  if (invoice.lp) {
    if (payAmount < invoice.remainingAmount) {
      lpNote = " (LP is paid only when the invoice is fully settled)";
    } else {
      const settlement = computeLpSettlement(invoice);
      lpNote = settlement
        ? ` (LP net gain on settlement: ${settlement.lpEarned} ${invoice.token}, ` +
          `LP payout ${settlement.lpPayout} ${invoice.token}; on-chain formula)`
        : " (LP earnings preview unavailable: fee/funding data not provided)";
    }
  }
  console.log(
    `\nPaying ${payAmount} ${invoice.token} of ${invoice.remainingAmount} ` +
      `${invoice.token} remaining on Invoice #${invoice.id}${lpNote}`,
  );
}

/** Receipt printer for full payment. */
export function printSettlementReceipt(result: PayResult): void {
  console.log("\n── Settlement Receipt ──────────────────────────────");
  console.log(`  Invoice:   ${result.invoiceId}`);
  console.log(`  Paid:      ${result.paidAmount} ${result.token}`);
  console.log(`  Remaining: ${result.remainingAmount} ${result.token}`);
  if (result.lpEarnings !== undefined) {
    console.log(`  LP earned: ${result.lpEarnings} ${result.token}`);
  }
  console.log(`  Tx hash:   ${result.txHash}`);
  console.log(`  Status:    ${result.isFullyPaid ? "FULLY PAID ✓" : "PARTIAL — invoice still open"}`);
  console.log("────────────────────────────────────────────────────\n");
}

/** Partial payment progress line. */
export function printPartialProgress(result: PayResult): void {
  console.log(
    `Paid ${result.paidAmount} ${result.token}. ` +
      `${result.remainingAmount} ${result.token} remaining.`,
  );
}

// ── LP settlement (on-chain formula) ──────────────────────────────────────────

/** Token amounts use 7 decimals on the invoice contract (stroops). */
const SCALE = 10_000_000n;
const BPS = 10_000n;

function toUnits(amount: number): bigint {
  return BigInt(Math.round(amount * Number(SCALE)));
}

function fromUnits(units: bigint): number {
  return Number(units) / Number(SCALE);
}

export interface LpSettlement {
  /** Protocol fee deducted from the invoice amount on settlement. */
  protocolFee: number;
  /** Amount transferred to the LP when the invoice is fully paid. */
  lpPayout: number;
  /** `lpPayout - lpFundedAmount`, floored at 0 (matches `lp_earned`). */
  lpEarned: number;
}

/**
 * Mirrors the full-payment branch of `mark_paid`:
 *
 *   protocol_fee = invoice.amount * fee_rate_bps / 10_000        (floor)
 *   distribute   = invoice.amount - protocol_fee
 *   lp_payout    = distribute * lp_funded / invoice.amount       (floor)
 *   lp_earned    = max(lp_payout - lp_funded, 0)
 *
 * Integer division truncates exactly like the contract. Returns `undefined`
 * when the inputs needed to apply the formula (`lpFundedAmount`,
 * `feeRateBps`) are missing or invalid, so callers never print a made-up
 * figure. Assumption: the CLI-supplied values equal the on-chain values at
 * submission time; the fee tier or funders may change before the tx lands, in
 * which case the receipt (`PayResult.lpEarnings`) is authoritative.
 */
export function computeLpSettlement(
  invoice: Pick<InvoicePayState, "totalAmount" | "lpFundedAmount" | "feeRateBps">,
): LpSettlement | undefined {
  const { totalAmount, lpFundedAmount, feeRateBps } = invoice;
  if (
    lpFundedAmount === undefined ||
    feeRateBps === undefined ||
    !Number.isFinite(totalAmount) ||
    !Number.isFinite(lpFundedAmount) ||
    !Number.isFinite(feeRateBps) ||
    totalAmount <= 0 ||
    lpFundedAmount < 0 ||
    lpFundedAmount > totalAmount ||
    feeRateBps < 0 ||
    feeRateBps > 10_000
  ) {
    return undefined;
  }

  const amount = toUnits(totalAmount);
  const funded = toUnits(lpFundedAmount);
  const fee = (amount * BigInt(Math.round(feeRateBps))) / BPS;
  const distribute = amount - fee;
  const payout = (distribute * funded) / amount;
  const earned = payout > funded ? payout - funded : 0n;

  return {
    protocolFee: fromUnits(fee),
    lpPayout: fromUnits(payout),
    lpEarned: fromUnits(earned),
  };
}

// ── Command factory ───────────────────────────────────────────────────────────

/**
 * Factory function following the project's injected-deps pattern.
 * Keeps the command pure and fully testable without real network calls.
 */
export function makePayCommand(
  prompter: Prompter,
  fetcher: InvoiceFetcher,
  executor: PayExecutor,
): Command {
  const cmd = new Command("pay");

  cmd
    .description("Pay an invoice in full or partially")
    .requiredOption("--id <invoiceId>", "Invoice ID to pay")
    .option("--amount <usdc>", "Amount to pay (omit for full payment)", Number)
    .option("--yes", "Skip confirmation prompt", false)
    .action(async (opts: { id: string; amount?: number; yes: boolean }) => {
      const options: PayOptions = {
        id: opts.id,
        amount: opts.amount,
        yes: opts.yes,
      };

      // ── 1. Fetch invoice state ──────────────────────────────────────────
      let invoice: InvoicePayState;
      try {
        invoice = await fetcher(options.id);
      } catch (err) {
        console.error(
          `Error fetching invoice ${options.id}: ${err instanceof Error ? err.message : String(err)}`,
        );
        process.exit(1);
      }

      // ── 2. Resolve pay amount ───────────────────────────────────────────
      const payAmount =
        options.amount !== undefined
          ? options.amount
          : invoice.remainingAmount; // full payment

      if (!Number.isFinite(payAmount) || payAmount <= 0) {
        console.error("Payment amount must be a finite number greater than 0.");
        process.exit(1);
      }

      if (!Number.isFinite(invoice.remainingAmount) || payAmount > invoice.remainingAmount) {
        console.error(
          `Amount ${payAmount} exceeds remaining balance of ${invoice.remainingAmount} ${invoice.token}.`,
        );
        process.exit(1);
      }

      // ── 3. Payment preview ──────────────────────────────────────────────
      printPaymentPreview(invoice, payAmount);

      // ── 4. Confirmation ─────────────────────────────────────────────────
      if (!options.yes) {
        const confirmed = await prompter(
          `Confirm payment of ${payAmount} ${invoice.token}? [y/N] `,
        );
        if (!confirmed) {
          console.log("Payment cancelled.");
          return;
        }
      }

      // ── 5. Execute payment ──────────────────────────────────────────────
      let result: PayResult;
      try {
        result = await executor(options.id, payAmount);
      } catch (err) {
        console.error(
          `Payment could not be confirmed: ${err instanceof Error ? err.message : String(err)}`,
        );
        console.error("Check the transaction or current invoice state before retrying.");
        process.exit(1);
      }

      // ── 6. Output ───────────────────────────────────────────────────────
      if (result.isFullyPaid) {
        printSettlementReceipt(result);
      } else {
        printPartialProgress(result);
      }
    });

  return cmd;
}
