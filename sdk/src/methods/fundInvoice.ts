// @ts-nocheck
/**
 * fundInvoice — LP-facing SDK method for the Invoice Liquidity Network.
 *
 * Handles the full two-step allowance + funding flow automatically:
 *   1. Fetch the invoice; fund only its unfunded remainder.
 *   2. Check the LP's token balance covers the discounted cost
 *      (`computeFundingCost`) — fails before anything is signed.
 *   3. Query the LP's current token allowance for the contract.
 *   4. Build and simulate `fund_invoice` (typed ILNError on rejection).
 *   5. If the allowance is insufficient, sign and submit an `approve` tx.
 *   6. Sign and submit the `fund_invoice` call.
 *   7. Return `{ txHash, effectiveYieldBps }`.
 *
 * Failure/interruption semantics:
 *   - Failures in steps 1-4 have no on-chain effect.
 *   - `txHash` means the network ACCEPTED the tx (status PENDING/DUPLICATE);
 *     it is not a confirmation of ledger inclusion. Callers must poll
 *     `getTransaction(txHash)` before treating the funding as final.
 *   - `TRY_AGAIN_LATER` / `ERROR` are thrown; nothing was accepted.
 *   - If the approve is accepted but the fund submission fails or the process
 *     dies, the approve remains valid until it expires (~720 ledgers) and a
 *     re-run re-checks the allowance instead of approving twice.
 *   - Re-running after a successful fund throws "cannot be funded" (status
 *     Funded) — it never double-funds. A tranche that raced another LP is
 *     rejected on-chain (OverfundingRejected).
 *   - NOTE: the contract moves the LP's funds with `transfer` (funder
 *     authorisation), not `transfer_from`, so the allowance is not consumed
 *     by `fund_invoice`; the approve step is a defensive pre-condition kept
 *     for token/contract variants that pull funds.
 *
 * Progress is surfaced via optional callbacks on FundOptions so integrators
 * can update UI without polling.
 */

import {
  Contract,
  SorobanRpc,
  TransactionBuilder,
  Networks,
  BASE_FEE,
  scValToNative,
  nativeToScVal,
  Address,
  Account,
  Keypair,
  Transaction,
} from "@stellar/stellar-sdk";
import type { FundOptions, FundResult, InvoiceView } from "../types.js";
import {
  getAllowance,
  getTokenBalance,
  buildApproveTransaction,
  isAllowanceSufficient,
} from "../utils/allowance.js";
import { retry } from "../utils/retry.js";
import { ILNError } from "../errors.js";

/**
 * LP cost for funding `fundAmount` of an invoice, mirroring `fund_invoice`:
 *   cost = fundAmount - floor(fundAmount * discountRateBps / 10_000)
 * (the contract transfers `cost` from the LP; the freelancer later receives
 * the discounted payout and the payer repays the full face amount).
 */
export function computeFundingCost(
  fundAmount: bigint,
  discountRateBps: number
): bigint {
  const discount = (fundAmount * BigInt(discountRateBps)) / 10_000n;
  return fundAmount - discount;
}

// ---------------------------------------------------------------------------
// computeEffectiveYieldBps (exported for consumers and tests)
// ---------------------------------------------------------------------------

/**
 * Compute the annualised effective yield in basis points for an LP position.
 *
 *   effectiveYieldBps = discountRate × daysToMaturity / 365
 *
 * Returns `0` when the due date is already in the past.
 *
 * @param discountRateBps - Invoice discount rate in basis points (e.g. 300 = 3%)
 * @param dueDateUnix     - Invoice due date as Unix timestamp (seconds)
 * @param nowUnix         - Current time as Unix timestamp; defaults to Date.now()
 */
export function computeEffectiveYieldBps(
  discountRateBps: number,
  dueDateUnix: number,
  nowUnix: number = Math.floor(Date.now() / 1000)
): number {
  const secondsToMaturity = Math.max(0, dueDateUnix - nowUnix);
  const daysToMaturity = secondsToMaturity / 86400;
  return Math.round((discountRateBps * daysToMaturity) / 365);
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/** Simulate get_invoice and decode the result into InvoiceView. */
async function fetchInvoice(
  server: SorobanRpc.Server,
  contractAddress: string,
  invoiceId: bigint,
  sourceAccount: Account,
  networkPassphrase: string
): Promise<InvoiceView> {
  const contract = new Contract(contractAddress);
  const op = contract.call(
    "get_invoice",
    nativeToScVal(invoiceId, { type: "u64" })
  );

  const tx = new TransactionBuilder(sourceAccount, {
    fee: BASE_FEE,
    networkPassphrase,
  })
    .addOperation(op)
    .setTimeout(30)
    .build();

  const sim = await retry(() => server.simulateTransaction(tx));

  if (SorobanRpc.Api.isSimulationError(sim)) {
    throw new Error(`get_invoice simulation failed: ${sim.error}`);
  }
  if (!sim.result?.retval) {
    throw new Error(`Invoice ${invoiceId} not found`);
  }

  const raw = scValToNative(sim.result.retval) as Record<string, unknown>;
  return {
    id: BigInt(String(raw["id"])),
    token: String(raw["token"]),
    amount: BigInt(String(raw["amount"])),
    dueDate: Number(raw["due_date"]),
    discountRate: Number(raw["discount_rate"]),
    status: String(raw["status"]),
    amountFunded: BigInt(String(raw["amount_funded"] ?? 0)),
  };
}

/**
 * Sign a base64-XDR transaction envelope with `signer` and submit it.
 * Returns the transaction hash once the network has ACCEPTED it (PENDING or
 * DUPLICATE). This does not wait for ledger inclusion.
 *
 * @throws When the network returns `status: "ERROR"` or `"TRY_AGAIN_LATER"`
 *         (the transaction was not accepted, so no hash is returned).
 */
async function signAndSubmit(
  server: SorobanRpc.Server,
  envelopeXdr: string,
  signer: Keypair,
  networkPassphrase: string
): Promise<string> {
  const tx = new Transaction(envelopeXdr, networkPassphrase);
  tx.sign(signer);
  const result = await server.sendTransaction(tx);
  if (result.status === "ERROR") {
    throw new Error(
      `Transaction failed: ${JSON.stringify(result.errorResult)}`
    );
  }
  if (result.status === "TRY_AGAIN_LATER") {
    throw new Error(
      "Transaction was not accepted by the network (TRY_AGAIN_LATER); nothing was submitted, safe to retry"
    );
  }
  return result.hash;
}

/**
 * Verify that the contract has a price oracle configured and that the
 * simulation does not error. Acts as a guard when
 * `requireOracleVerification: true`.
 */
async function verifyOracle(
  server: SorobanRpc.Server,
  contractAddress: string,
  sourceAccount: Account,
  networkPassphrase: string
): Promise<void> {
  const contract = new Contract(contractAddress);
  const op = contract.call("get_price_oracle");

  const tx = new TransactionBuilder(sourceAccount, {
    fee: BASE_FEE,
    networkPassphrase,
  })
    .addOperation(op)
    .setTimeout(30)
    .build();

  const sim = await retry(() => server.simulateTransaction(tx));

  if (SorobanRpc.Api.isSimulationError(sim)) {
    throw new Error(`Oracle verification failed: ${sim.error}`);
  }
  if (!sim.result?.retval) {
    throw new Error(
      "Oracle verification required but no price oracle is configured on the contract"
    );
  }

  const oracleAddress = scValToNative(sim.result.retval);
  if (!oracleAddress) {
    throw new Error(
      "Oracle verification required but no price oracle is configured on the contract"
    );
  }
}

// ---------------------------------------------------------------------------
// fundInvoice
// ---------------------------------------------------------------------------

/**
 * Fund an invoice as a liquidity provider.
 *
 * Automatically manages the token allowance before calling `fund_invoice`
 * on-chain. When an `approve` transaction is needed, `onApprovalRequired`
 * fires first (so the UI can show a prompt), and `onApprovalSent` fires
 * after the approval is submitted. `onFunded` fires once the fund transaction
 * is sent.
 *
 * @param server              - Soroban RPC server connected to the target network
 * @param contractAddress     - Deployed invoice-liquidity contract address
 * @param lpKeypair           - Keypair of the liquidity provider (signs all txs)
 * @param invoiceId           - ID of the invoice to fund
 * @param options             - Optional configuration and progress callbacks:
 *   - `requireOracleVerification` — reject if the contract has no price oracle
 *   - `onApprovalRequired`        — fired before the approve tx is built
 *   - `onApprovalSent`            — fired after the approve tx is submitted
 *   - `onFunded`                  — fired after the fund tx is submitted
 * @param networkPassphrase   - Stellar network passphrase (default: TESTNET)
 *
 * @returns `{ txHash, effectiveYieldBps }` on success
 *
 * @throws When the invoice is not fundable (wrong status, not found, etc.)
 * @throws When the approve or fund transaction is rejected by the network
 * @throws When oracle verification is required but fails
 * @throws {ILNError.InsufficientAmount} When the LP's token balance is below the funding cost
 *
 * @example
 * ```ts
 * const result = await fundInvoice(server, CONTRACT_ID, lpKeypair, 42n, {
 *   onApprovalRequired: ({ requiredAmount, currentAllowance }) =>
 *     console.log(`Need to approve ${requiredAmount}, have ${currentAllowance}`),
 *   onApprovalSent: ({ approveTxHash }) =>
 *     console.log(`Approval submitted: ${approveTxHash}`),
 *   onFunded: ({ effectiveYieldBps, invoiceId }) =>
 *     console.log(`Invoice ${invoiceId} funded! Yield: ${effectiveYieldBps} bps`),
 * });
 * ```
 */
export async function fundInvoice(
  server: SorobanRpc.Server,
  contractAddress: string,
  lpKeypair: Keypair,
  invoiceId: bigint,
  options: FundOptions = {},
  networkPassphrase: string = Networks.TESTNET
): Promise<FundResult> {
  const {
    requireOracleVerification = false,
    onApprovalRequired,
    onApprovalSent,
    onFunded,
  } = options;

  // 1. Load the LP's on-chain account (for sequence numbers)
  const lpAddress = lpKeypair.publicKey();
  const accountData = await retry(() => server.getAccount(lpAddress));
  const sequence = accountData.sequence;

  const makeAccount = (seq: string) => new Account(lpAddress, seq);

  // 2. Fetch the invoice
  const invoice = await fetchInvoice(
    server,
    contractAddress,
    invoiceId,
    makeAccount(sequence),
    networkPassphrase
  );

  if (invoice.status !== "Pending" && invoice.status !== "PartiallyFunded") {
    throw new Error(
      `Invoice ${invoiceId} cannot be funded (status: ${invoice.status})`
    );
  }

  // 3. Oracle guard (optional)
  if (requireOracleVerification) {
    await verifyOracle(
      server,
      contractAddress,
      makeAccount(sequence),
      networkPassphrase
    );
  }

  // 4. Amount to fund and LP cost. `fund_invoice` rejects any tranche that
  //    would push amount_funded past amount (OverfundingRejected), so only
  //    the unfunded remainder is funded.
  const fundAmount = invoice.amount - (invoice.amountFunded ?? 0n);
  if (fundAmount <= 0n) {
    throw new Error(`Invoice ${invoiceId} has no remaining amount to fund`);
  }
  const cost = computeFundingCost(fundAmount, invoice.discountRate);

  // 5. Balance check (read-only). Fails before anything is signed so an LP
  //    who cannot pay never burns fees on an approve tx.
  const balance = await retry(() =>
    getTokenBalance(
      server,
      invoice.token,
      lpAddress,
      makeAccount(sequence),
      networkPassphrase
    )
  );
  if (balance < cost) {
    throw new ILNError.InsufficientAmount(
      `Insufficient token balance to fund invoice ${invoiceId}: need ${cost}, have ${balance}`
    );
  }

  // 6. Allowance check
  const ledgerInfo = await retry(() => server.getLatestLedger());
  const currentLedger = ledgerInfo.sequence;

  const allowance = await getAllowance(
    server,
    { tokenAddress: invoice.token, owner: lpAddress, spender: contractAddress },
    makeAccount(sequence),
    networkPassphrase
  );

  const needsApproval = !isAllowanceSufficient(
    allowance,
    fundAmount,
    currentLedger + 10 // require at least 10 ledgers validity remaining
  );

  // 7. Build and simulate fund_invoice BEFORE any approve is submitted, so a
  //    contract-level rejection (paused, overfunded, unauthorized, frozen
  //    funder, volume cap, ...) surfaces as a typed ILNError without costing
  //    the LP an approve fee. If an approve will be sent first it consumes
  //    one sequence number, so the fund tx is built at sequence + 1.
  //    Contract signature: fund_invoice(funder, invoice_id, fund_amount,
  //    require_oracle_verification).
  const fundSequence = needsApproval ? String(BigInt(sequence) + 1n) : sequence;
  const contract = new Contract(contractAddress);
  const fundOp = contract.call(
    "fund_invoice",
    new Address(lpAddress).toScVal(),
    nativeToScVal(invoiceId, { type: "u64" }),
    nativeToScVal(fundAmount, { type: "i128" }),
    nativeToScVal(requireOracleVerification, { type: "bool" })
  );

  const fundTx = new TransactionBuilder(makeAccount(fundSequence), {
    fee: BASE_FEE,
    networkPassphrase,
  })
    .addOperation(fundOp)
    .setTimeout(30)
    .build();

  let preparedFundTx;
  try {
    preparedFundTx = await retry(() => server.prepareTransaction(fundTx));
  } catch (err) {
    throw ILNError.fromError(err, { contractId: contractAddress });
  }

  if (needsApproval) {
    // 8a. Notify caller
    onApprovalRequired?.({
      requiredAmount: fundAmount,
      currentAllowance: allowance.amount,
    });

    // 8b. Build, sign and submit the approve tx
    const approveXdr = await buildApproveTransaction(
      server,
      invoice.token,
      makeAccount(sequence),
      contractAddress,
      fundAmount,
      networkPassphrase
    );

    const approveTxHash = await signAndSubmit(
      server,
      approveXdr,
      lpKeypair,
      networkPassphrase
    );

    onApprovalSent?.({ approveTxHash });
  }

  // 9. Sign and submit the (already simulated) fund_invoice tx
  (preparedFundTx as Transaction).sign(lpKeypair);

  const fundSendResult = await retry(() => server.sendTransaction(
    preparedFundTx as Transaction
  ));

  if (fundSendResult.status === "ERROR") {
    throw new Error(
      `fund_invoice failed: ${JSON.stringify(fundSendResult.errorResult)}`
    );
  }
  if (fundSendResult.status === "TRY_AGAIN_LATER") {
    throw new Error(
      "fund_invoice was not accepted by the network (TRY_AGAIN_LATER); nothing was submitted, safe to retry"
    );
  }

  // 10. Compute annualised yield and notify
  const effectiveYieldBps = computeEffectiveYieldBps(
    invoice.discountRate,
    invoice.dueDate
  );

  onFunded?.({ effectiveYieldBps, invoiceId });

  return { txHash: fundSendResult.hash, effectiveYieldBps };
}
