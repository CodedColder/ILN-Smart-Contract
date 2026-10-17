#!/usr/bin/env node
/**
 * synthetic-canary.ts — Synthetic transaction monitoring for ILN mainnet.
 *
 * Periodically performs a real, small-value invoice lifecycle end-to-end
 * against mainnet to catch issues that passive checks miss:
 *   - RPC degradation
 *   - Contract misbehavior
 *   - Indexer lag / missing events
 *   - Notification delivery failures
 *
 * Uses a dedicated, clearly-labeled canary wallet to avoid being mistaken
 * for real protocol activity in analytics.
 *
 * Usage:
 *   npx tsx scripts/synthetic-canary.ts                  # one-shot run
 *   npx tsx scripts/synthetic-canary.ts --watch           # continuous schedule
 *   npx tsx scripts/synthetic-canary.ts --dry-run         # simulate without submitting
 *
 * Configuration (environment variables):
 *   SOROBAN_RPC_URL           Soroban RPC endpoint (default: mainnet)
 *   HORIZON_URL               Horizon endpoint (default: mainnet)
 *   NETWORK_PASSPHRASE        Stellar network passphrase (default: public)
 *   CANARY_SECRET_KEY         Secret key for the dedicated canary wallet
 *   CONTRACT_ID               Deployed invoice_liquidity contract address
 *   INDEXER_URL               Indexer base URL for state verification
 *   CANARY_AMOUNT             Invoice amount in smallest unit (default: 10000 = 0.001 XLM)
 *   CANARY_INTERVAL_MS        Interval between canary runs in watch mode (default: 300000 = 5 min)
 *   LATENCY_THRESHOLD_MS      Max acceptable latency per step (default: 10000 = 10s)
 *   INDEXER_REFLECT_WINDOW_MS Max time for indexer to reflect canary state (default: 60000 = 60s)
 *   ALERT_WEBHOOK_URL         Webhook URL for failure alerts (optional)
 */

import {
  rpc,
  Keypair,
  TransactionBuilder,
  Networks,
  Contract,
  Address,
  scValToNative,
  xdr,
} from "@stellar/stellar-sdk";

// ── Types ────────────────────────────────────────────────────────────────────

interface CanaryConfig {
  sorobanRpcUrl: string;
  horizonUrl: string;
  networkPassphrase: string;
  canarySecretKey: string;
  contractId: string;
  indexerUrl: string;
  canaryAmount: bigint;
  intervalMs: number;
  latencyThresholdMs: number;
  indexerReflectWindowMs: number;
  alertWebhookUrl: string;
  dryRun: boolean;
  /** Insurance pool contract address for solvency health checks. */
  insurancePoolContractId: string;
  /** Oracle contract address for TWAP feed reads. */
  oracleContractId: string;
}

interface CanaryStep {
  name: string;
  startedAt: string;
  completedAt: string | null;
  latencyMs: number | null;
  success: boolean;
  error: string | null;
  details: Record<string, unknown>;
}

interface CanaryReport {
  canaryId: string;
  runAt: string;
  steps: CanaryStep[];
  overallSuccess: boolean;
  totalLatencyMs: number;
}

// ── Config ───────────────────────────────────────────────────────────────────

function loadConfig(argv: string[]): CanaryConfig {
  const dryRun = argv.includes("--dry-run");
  return {
    sorobanRpcUrl:
      process.env.SOROBAN_RPC_URL || "https://soroban-mainnet.stellar.org",
    horizonUrl: (process.env.HORIZON_URL || "https://horizon.stellar.org").replace(
      /\/$/,
      ""
    ),
    networkPassphrase: process.env.NETWORK_PASSPHRASE || Networks.PUBLIC,
    canarySecretKey: process.env.CANARY_SECRET_KEY || "",
    contractId: process.env.CONTRACT_ID || "",
    indexerUrl: (process.env.INDEXER_URL || "http://localhost:3000").replace(
      /\/$/,
      ""
    ),
    canaryAmount: BigInt(process.env.CANARY_AMOUNT || "10000"), // 0.001 XLM
    intervalMs: Number(process.env.CANARY_INTERVAL_MS || 300_000), // 5 min
    latencyThresholdMs: Number(process.env.LATENCY_THRESHOLD_MS || 10_000),
    indexerReflectWindowMs: Number(process.env.INDEXER_REFLECT_WINDOW_MS || 60_000),
    alertWebhookUrl: process.env.ALERT_WEBHOOK_URL || "",
    dryRun,
    insurancePoolContractId: process.env.INSURANCE_POOL_CONTRACT_ID || '',
    oracleContractId: process.env.ORACLE_CONTRACT_ID || '',
  };
}

// ── Helpers ──────────────────────────────────────────────────────────────────

function bigintToI128ScVal(value: bigint): xdr.ScVal {
  const lo = value & 0xffffffffffffffffn;
  const hi = value >> 64n;
  return xdr.ScVal.scvI128(
    new xdr.Int128Parts({
      lo: new xdr.Uint64(lo),
      hi: new xdr.Int64(hi),
    })
  );
}

function bigintToU64ScVal(value: bigint): xdr.ScVal {
  return xdr.ScVal.scvU64(new xdr.Uint64(value));
}

function numberToU32ScVal(value: number): xdr.ScVal {
  return xdr.ScVal.scvU32(value);
}

function errMsg(e: unknown): string {
  if (e instanceof Error) return e.message;
  return String(e);
}

function stepStart(name: string): CanaryStep {
  return {
    name,
    startedAt: new Date().toISOString(),
    completedAt: null,
    latencyMs: null,
    success: false,
    error: null,
    details: {},
  };
}

function stepComplete(step: CanaryStep, success: boolean, details: Record<string, unknown> = {}, error?: string): CanaryStep {
  return {
    ...step,
    completedAt: new Date().toISOString(),
    latencyMs: Date.now() - new Date(step.startedAt).getTime(),
    success,
    error: error || null,
    details,
  };
}

async function timedFetch(
  url: string,
  init: RequestInit,
  timeoutMs: number
): Promise<{ res: Response; latencyMs: number }> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  const start = Date.now();
  try {
    const res = await fetch(url, { ...init, signal: controller.signal });
    return { res, latencyMs: Date.now() - start };
  } finally {
    clearTimeout(timer);
  }
}

async function alertFailure(
  report: CanaryReport,
  webhookUrl: string
): Promise<void> {
  if (!webhookUrl) return;
  const failedSteps = report.steps.filter((s) => !s.success);
  const text = [
    `:rotating_light: *ILN Canary Alert* — ${report.runAt}`,
    `Canary ID: \`${report.canaryId}\``,
    `Failed steps: ${failedSteps.map((s) => s.name).join(", ")}`,
    ...failedSteps.map((s) => `• *${s.name}*: ${s.error ?? "failed"}`),
  ].join("\n");

  try {
    await fetch(webhookUrl, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ text }),
    });
  } catch {
    // Alert delivery failure is logged but does not fail the canary run.
    console.error("Failed to send canary alert to webhook");
  }
}

// ── Canary steps ─────────────────────────────────────────────────────────────

async function stepFundCanary(
  cfg: CanaryConfig,
  server: rpc.Server,
  canaryKeypair: Keypair
): Promise<CanaryStep> {
  const step = stepStart("fund_canary");
  const start = Date.now();
  try {
    const publicKey = canaryKeypair.publicKey();
    const response = await fetch(
      `https://friendbot.stellar.org?addr=${publicKey}`
    );
    const latencyMs = Date.now() - start;
    if (!response.ok) {
      return stepComplete(step, false, { latencyMs, status: response.status },
        `Friendbot returned HTTP ${response.status}`);
    }
    return stepComplete(step, true, { latencyMs, publicKey });
  } catch (e) {
    return stepComplete(step, false, { latencyMs: Date.now() - start }, errMsg(e));
  }
}

async function stepSubmitInvoice(
  cfg: CanaryConfig,
  server: rpc.Server,
  canaryKeypair: Keypair,
  invoiceId: { value: string }
): Promise<CanaryStep> {
  const step = stepStart("submit_invoice");
  const start = Date.now();
  try {
    const contract = new Contract(cfg.contractId);
    const account = await server.getAccount(canaryKeypair.publicKey());
    const dueDate = BigInt(Math.floor(Date.now() / 1000) + 3600); // 1 hour from now

    const args = [
      Address.fromString(canaryKeypair.publicKey()).toScVal(), // submitter
      Address.fromString(canaryKeypair.publicKey()).toScVal(), // payer (canary pays itself)
      bigintToI128ScVal(cfg.canaryAmount),
      bigintToU64ScVal(dueDate),
      numberToU32ScVal(0), // 0% discount for canary
      Address.fromString(canaryKeypair.publicKey()).toScVal(), // token (XLM via account SAC)
    ];

    const tx = new TransactionBuilder(account, {
      fee: "100000",
      networkPassphrase: cfg.networkPassphrase,
    })
      .addOperation(contract.call("submit_invoice", ...args))
      .setTimeout(30)
      .build();

    if (cfg.dryRun) {
      const simulated = await server.simulateTransaction(tx);
      if (rpc.Api.isSimulateTransactionError(simulated)) {
        return stepComplete(step, false, {}, `Simulation failed: ${JSON.stringify(simulated.error)}`);
      }
      return stepComplete(step, true, { dryRun: true, simulated: true });
    }

    const simulated = await server.simulateTransaction(tx);
    if (rpc.Api.isSimulateTransactionError(simulated)) {
      return stepComplete(step, false, {}, `Simulation failed: ${JSON.stringify(simulated.error)}`);
    }

    const assembledTx = rpc.assembleTransaction(tx, simulated).build();
    assembledTx.sign(canaryKeypair);

    const response = await server.sendTransaction(assembledTx);
    if (response.status === "ERROR") {
      return stepComplete(step, false, {}, `Send failed: ${JSON.stringify(response.errorResultXdr)}`);
    }

    // Poll for confirmation
    let status = response.status;
    const txHash = response.hash;
    while (status === "PENDING") {
      await new Promise((resolve) => setTimeout(resolve, 1500));
      const txResult = await server.getTransaction(txHash);
      status = txResult.status;
      if (status === "SUCCESS") {
        const returnValue = txResult.returnValue;
        const id = returnValue ? scValToNative(returnValue) : null;
        invoiceId.value = String(id);
        return stepComplete(step, true, {
          latencyMs: Date.now() - start,
          txHash,
          invoiceId: invoiceId.value,
        });
      } else if (status === "FAILED") {
        return stepComplete(step, false, { txHash }, `Transaction failed: ${JSON.stringify(txResult)}`);
      }
    }

    return stepComplete(step, false, {}, `Unexpected status: ${status}`);
  } catch (e) {
    return stepComplete(step, false, { latencyMs: Date.now() - start }, errMsg(e));
  }
}

async function stepFundInvoice(
  cfg: CanaryConfig,
  server: rpc.Server,
  canaryKeypair: Keypair,
  invoiceId: string
): Promise<CanaryStep> {
  const step = stepStart("fund_invoice");
  const start = Date.now();
  try {
    const contract = new Contract(cfg.contractId);
    const account = await server.getAccount(canaryKeypair.publicKey());

    const args = [
      Address.fromString(canaryKeypair.publicKey()).toScVal(),
      bigintToU64ScVal(BigInt(invoiceId)),
      bigintToI128ScVal(cfg.canaryAmount),
      xdr.ScVal.scvBool(false), // require_oracle_verification = false
    ];

    const tx = new TransactionBuilder(account, {
      fee: "100000",
      networkPassphrase: cfg.networkPassphrase,
    })
      .addOperation(contract.call("fund_invoice", ...args))
      .setTimeout(30)
      .build();

    if (cfg.dryRun) {
      const simulated = await server.simulateTransaction(tx);
      if (rpc.Api.isSimulateTransactionError(simulated)) {
        return stepComplete(step, false, {}, `Simulation failed: ${JSON.stringify(simulated.error)}`);
      }
      return stepComplete(step, true, { dryRun: true });
    }

    const simulated = await server.simulateTransaction(tx);
    if (rpc.Api.isSimulateTransactionError(simulated)) {
      return stepComplete(step, false, {}, `Simulation failed: ${JSON.stringify(simulated.error)}`);
    }

    const assembledTx = rpc.assembleTransaction(tx, simulated).build();
    assembledTx.sign(canaryKeypair);

    const response = await server.sendTransaction(assembledTx);
    if (response.status === "ERROR") {
      return stepComplete(step, false, {}, `Send failed: ${JSON.stringify(response.errorResultXdr)}`);
    }

    let status = response.status;
    const txHash = response.hash;
    while (status === "PENDING") {
      await new Promise((resolve) => setTimeout(resolve, 1500));
      const txResult = await server.getTransaction(txHash);
      status = txResult.status;
      if (status === "SUCCESS") {
        return stepComplete(step, true, { latencyMs: Date.now() - start, txHash });
      } else if (status === "FAILED") {
        return stepComplete(step, false, { txHash }, `Transaction failed`);
      }
    }

    return stepComplete(step, false, {}, `Unexpected status: ${status}`);
  } catch (e) {
    return stepComplete(step, false, { latencyMs: Date.now() - start }, errMsg(e));
  }
}

async function stepSettleInvoice(
  cfg: CanaryConfig,
  server: rpc.Server,
  canaryKeypair: Keypair,
  invoiceId: string
): Promise<CanaryStep> {
  const step = stepStart("settle_invoice");
  const start = Date.now();
  try {
    const contract = new Contract(cfg.contractId);
    const account = await server.getAccount(canaryKeypair.publicKey());

    const args = [
      bigintToU64ScVal(BigInt(invoiceId)),
      bigintToI128ScVal(cfg.canaryAmount),
    ];

    const tx = new TransactionBuilder(account, {
      fee: "100000",
      networkPassphrase: cfg.networkPassphrase,
    })
      .addOperation(contract.call("mark_paid", ...args))
      .setTimeout(30)
      .build();

    if (cfg.dryRun) {
      const simulated = await server.simulateTransaction(tx);
      if (rpc.Api.isSimulateTransactionError(simulated)) {
        return stepComplete(step, false, {}, `Simulation failed: ${JSON.stringify(simulated.error)}`);
      }
      return stepComplete(step, true, { dryRun: true });
    }

    const simulated = await server.simulateTransaction(tx);
    if (rpc.Api.isSimulateTransactionError(simulated)) {
      return stepComplete(step, false, {}, `Simulation failed: ${JSON.stringify(simulated.error)}`);
    }

    const assembledTx = rpc.assembleTransaction(tx, simulated).build();
    assembledTx.sign(canaryKeypair);

    const response = await server.sendTransaction(assembledTx);
    if (response.status === "ERROR") {
      return stepComplete(step, false, {}, `Send failed: ${JSON.stringify(response.errorResultXdr)}`);
    }

    let status = response.status;
    const txHash = response.hash;
    while (status === "PENDING") {
      await new Promise((resolve) => setTimeout(resolve, 1500));
      const txResult = await server.getTransaction(txHash);
      status = txResult.status;
      if (status === "SUCCESS") {
        return stepComplete(step, true, { latencyMs: Date.now() - start, txHash });
      } else if (status === "FAILED") {
        return stepComplete(step, false, { txHash }, `Transaction failed`);
      }
    }

    return stepComplete(step, false, {}, `Unexpected status: ${status}`);
  } catch (e) {
    return stepComplete(step, false, { latencyMs: Date.now() - start }, errMsg(e));
  }
}

async function stepVerifyIndexerReflection(
  cfg: CanaryConfig,
  invoiceId: string
): Promise<CanaryStep> {
  const step = stepStart("verify_indexer_reflection");
  const start = Date.now();
  try {
    const deadline = start + cfg.indexerReflectWindowMs;
    let lastError = "";

    while (Date.now() < deadline) {
      try {
        const { res, latencyMs } = await timedFetch(
          `${cfg.indexerUrl}/invoices/${invoiceId}`,
          { method: "GET", headers: { accept: "application/json" } },
          cfg.latencyThresholdMs
        );

        if (res.ok) {
          const data = (await res.json()) as Record<string, unknown>;
          return stepComplete(step, true, {
            latencyMs,
            invoiceId,
            indexerState: data.status,
          });
        }

        lastError = `HTTP ${res.status}`;
      } catch (e) {
        lastError = errMsg(e);
      }

      await new Promise((resolve) => setTimeout(resolve, 3000));
    }

    return stepComplete(step, false, { latencyMs: Date.now() - start },
      `Indexer did not reflect invoice ${invoiceId} within ${cfg.indexerReflectWindowMs}ms: ${lastError}`);
  } catch (e) {
    return stepComplete(step, false, { latencyMs: Date.now() - start }, errMsg(e));
  }
}

async function stepCheckLatencyThresholds(
  steps: CanaryStep[],
  thresholdMs: number
): Promise<CanaryStep> {
  const step = stepStart("check_latency_thresholds");
  const start = Date.now();
  const violations: { step: string; latencyMs: number }[] = [];

  for (const s of steps) {
    if (s.latencyMs != null && s.latencyMs > thresholdMs) {
      violations.push({ step: s.name, latencyMs: s.latencyMs });
    }
  }

  if (violations.length > 0) {
    return stepComplete(step, false, { violations },
      `${violations.length} steps exceeded ${thresholdMs}ms threshold`);
  }

  return stepComplete(step, true, { thresholdMs, checked: steps.length });
}

// ── TWAP and Insurance Pool Canary Steps ─────────────────────────────────────

/**
 * Step: Read a TWAP-enabled price feed from the oracle contract.
 * Verifies that the oracle is responsive and returning valid data.
 */
async function stepReadTwapFeed(
  cfg: CanaryConfig,
  server: rpc.Server
): Promise<CanaryStep> {
  const step = stepStart('read_twap_feed');
  const start = Date.now();
  try {
    if (!cfg.oracleContractId) {
      return stepComplete(step, true, { skipped: true, reason: 'ORACLE_CONTRACT_ID not set' });
    }

    const contract = new Contract(cfg.oracleContractId);
    // Read the TWAP price for XLM/USD — a simple read-only call
    const args = [
      nativeToScVal('XLM', { type: 'symbol' }),
      nativeToScVal('USD', { type: 'symbol' }),
    ];

    const tx = new TransactionBuilder(
      await server.getAccount('GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'),
      { fee: '100', networkPassphrase: cfg.networkPassphrase }
    )
      .addOperation(contract.call('get_twap', ...args))
      .setTimeout(30)
      .build();

    const simulated = await server.simulateTransaction(tx);
    const latencyMs = Date.now() - start;

    if (rpc.Api.isSimulateTransactionError(simulated)) {
      return stepComplete(step, false, { latencyMs }, `TWAP simulation failed: ${JSON.stringify(simulated.error)}`);
    }

    if (!rpc.Api.isSimulationSuccess(simulated)) {
      return stepComplete(step, false, { latencyMs }, 'TWAP simulation returned non-success status');
    }

    const retval = simulated.result?.retval;
    const value = retval ? scValToNative(retval) : null;

    return stepComplete(step, true, {
      latencyMs,
      oracleContract: cfg.oracleContractId,
      twapValue: value,
    });
  } catch (e) {
    return stepComplete(step, false, { latencyMs: Date.now() - start }, errMsg(e));
  }
}

/**
 * Step: Check insurance pool solvency health.
 * Reads the pool's solvency ratio and verifies the circuit breaker is not tripped.
 */
async function stepCheckInsurancePoolHealth(
  cfg: CanaryConfig,
  server: rpc.Server
): Promise<CanaryStep> {
  const step = stepStart('check_insurance_pool_health');
  const start = Date.now();
  try {
    if (!cfg.insurancePoolContractId) {
      return stepComplete(step, true, { skipped: true, reason: 'INSURANCE_POOL_CONTRACT_ID not set' });
    }

    const contract = new Contract(cfg.insurancePoolContractId);
    const tx = new TransactionBuilder(
      await server.getAccount('GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'),
      { fee: '100', networkPassphrase: cfg.networkPassphrase }
    )
      .addOperation(contract.call('get_solvency_info'))
      .setTimeout(30)
      .build();

    const simulated = await server.simulateTransaction(tx);
    const latencyMs = Date.now() - start;

    if (rpc.Api.isSimulateTransactionError(simulated)) {
      return stepComplete(step, false, { latencyMs }, `Insurance pool simulation failed: ${JSON.stringify(simulated.error)}`);
    }

    if (!rpc.Api.isSimulationSuccess(simulated)) {
      return stepComplete(step, false, { latencyMs }, 'Insurance pool simulation returned non-success status');
    }

    const retval = simulated.result?.retval;
    const solvencyInfo = retval ? scValToNative(retval) : null;

    // Check if circuit breaker is tripped (solvency ratio < 1.0 or explicit flag)
    let isHealthy = true;
    if (solvencyInfo && typeof solvencyInfo === 'object') {
      const info = solvencyInfo as Record<string, unknown>;
      if ('solvency_ratio' in info) {
        const ratio = Number(info.solvency_ratio);
        isHealthy = ratio >= 1.0;
      }
      if ('circuit_tripped' in info) {
        isHealthy = isHealthy && !Boolean(info.circuit_tripped);
      }
    }

    return stepComplete(step, isHealthy, {
      latencyMs,
      poolContract: cfg.insurancePoolContractId,
      solvencyInfo,
      healthy: isHealthy,
    }, isHealthy ? undefined : 'Insurance pool solvency circuit is tripped or ratio below 1.0');
  } catch (e) {
    return stepComplete(step, false, { latencyMs: Date.now() - start }, errMsg(e));
  }
}

// ── Main ─────────────────────────────────────────────────────────────────────

async function runCanaryOnce(cfg: CanaryConfig): Promise<CanaryReport> {
  const canaryId = `canary-${Date.now()}`;
  const runAt = new Date().toISOString();
  const steps: CanaryStep[] = [];

  if (!cfg.canarySecretKey) {
    console.error("ERROR: CANARY_SECRET_KEY is required");
    process.exit(1);
  }
  if (!cfg.contractId) {
    console.error("ERROR: CONTRACT_ID is required");
    process.exit(1);
  }

  const canaryKeypair = Keypair.fromSecret(cfg.canarySecretKey);
  const server = new rpc.Server(cfg.sorobanRpcUrl);

  console.log(`[${runAt}] Starting canary run ${canaryId}`);
  console.log(`  Canary wallet: ${canaryKeypair.publicKey()}`);
  console.log(`  Contract: ${cfg.contractId}`);
  console.log(`  Amount: ${cfg.canaryAmount}`);
  console.log(`  Dry run: ${cfg.dryRun}`);

  // Step 1: Fund canary wallet
  const fundStep = await stepFundCanary(cfg, server, canaryKeypair);
  steps.push(fundStep);
  console.log(`  [${fundStep.success ? "PASS" : "FAIL"}] fund_canary: ${fundStep.latencyMs ?? "?"}ms`);
  if (!fundStep.success) {
    const report = { canaryId, runAt, steps, overallSuccess: false, totalLatencyMs: 0 };
    await alertFailure(report, cfg.alertWebhookUrl);
    return report;
  }

  // Step 2: Submit invoice
  const invoiceId = { value: "" };
  const submitStep = await stepSubmitInvoice(cfg, server, canaryKeypair, invoiceId);
  steps.push(submitStep);
  console.log(`  [${submitStep.success ? "PASS" : "FAIL"}] submit_invoice: ${submitStep.latencyMs ?? "?"}ms`);
  if (!submitStep.success) {
    const report = { canaryId, runAt, steps, overallSuccess: false, totalLatencyMs: 0 };
    await alertFailure(report, cfg.alertWebhookUrl);
    return report;
  }

  // Step 3: Fund invoice
  const fundInvoiceStep = await stepFundInvoice(cfg, server, canaryKeypair, invoiceId.value);
  steps.push(fundInvoiceStep);
  console.log(`  [${fundInvoiceStep.success ? "PASS" : "FAIL"}] fund_invoice: ${fundInvoiceStep.latencyMs ?? "?"}ms`);
  if (!fundInvoiceStep.success) {
    const report = { canaryId, runAt, steps, overallSuccess: false, totalLatencyMs: 0 };
    await alertFailure(report, cfg.alertWebhookUrl);
    return report;
  }

  // Step 4: Settle invoice
  const settleStep = await stepSettleInvoice(cfg, server, canaryKeypair, invoiceId.value);
  steps.push(settleStep);
  console.log(`  [${settleStep.success ? "PASS" : "FAIL"}] settle_invoice: ${settleStep.latencyMs ?? "?"}ms`);
  if (!settleStep.success) {
    const report = { canaryId, runAt, steps, overallSuccess: false, totalLatencyMs: 0 };
    await alertFailure(report, cfg.alertWebhookUrl);
    return report;
  }

  // Step 5: Verify indexer reflection
  const indexerStep = await stepVerifyIndexerReflection(cfg, invoiceId.value);
  steps.push(indexerStep);
  console.log(`  [${indexerStep.success ? "PASS" : "FAIL"}] verify_indexer: ${indexerStep.latencyMs ?? "?"}ms`);

  // Step 6: Read TWAP feed from oracle
  const twapStep = await stepReadTwapFeed(cfg, server);
  steps.push(twapStep);
  console.log(`  [${twapStep.success ? "PASS" : "FAIL"}] read_twap_feed: ${twapStep.latencyMs ?? "?"}ms`);

  // Step 7: Check insurance pool solvency health
  const insuranceStep = await stepCheckInsurancePoolHealth(cfg, server);
  steps.push(insuranceStep);
  console.log(`  [${insuranceStep.success ? "PASS" : "FAIL"}] check_insurance_pool: ${insuranceStep.latencyMs ?? "?"}ms`);

  // Step 8: Check latency thresholds
  const latencyStep = await stepCheckLatencyThresholds(steps, cfg.latencyThresholdMs);
  steps.push(latencyStep);
  console.log(`  [${latencyStep.success ? "PASS" : "FAIL"}] check_latency: ${latencyStep.latencyMs ?? "?"}ms`);

  const overallSuccess = steps.every((s) => s.success);
  const totalLatencyMs =
    (steps[steps.length - 1]?.latencyMs ?? 0) -
    (steps[0]?.latencyMs ?? 0);

  const report: CanaryReport = {
    canaryId,
    runAt,
    steps,
    overallSuccess,
    totalLatencyMs,
  };

  console.log(
    `\n[${runAt}] Canary run ${canaryId}: ${overallSuccess ? "PASS" : "FAIL"}`
  );

  if (!overallSuccess) {
    await alertFailure(report, cfg.alertWebhookUrl);
  }

  return report;
}

async function main(argv: string[]): Promise<number> {
  const cfg = loadConfig(argv);
  const watchMode = argv.includes("--watch");

  if (!cfg.canarySecretKey) {
    console.error("CANARY_SECRET_KEY is required. Generate a dedicated canary wallet and set its secret key.");
    console.error("The canary wallet's purpose should be documented publicly so it isn't mistaken for real protocol activity.");
    return 1;
  }

  if (!cfg.contractId) {
    console.error("CONTRACT_ID is required. Set it to the deployed invoice_liquidity contract address.");
    return 1;
  }

  // Log the canary wallet identity for public documentation
  const canaryKeypair = Keypair.fromSecret(cfg.canarySecretKey);
  console.log("=== ILN Synthetic Canary ===");
  console.log(`Canary wallet: ${canaryKeypair.publicKey()}`);
  console.log("This wallet is a dedicated synthetic monitoring canary.");
  console.log("Any activity from this wallet is test data, not real protocol usage.");
  console.log("============================\n");

  if (!watchMode) {
    const report = await runCanaryOnce(cfg);
    process.stdout.write(JSON.stringify(report, null, 2) + "\n");
    return report.overallSuccess ? 0 : 1;
  }

  // Watch mode: run on a schedule
  console.log(`Watch mode: running every ${cfg.intervalMs}ms`);
  while (true) {
    try {
      await runCanaryOnce(cfg);
    } catch (e) {
      console.error(`Canary run crashed: ${errMsg(e)}`);
    }
    await new Promise((resolve) => setTimeout(resolve, cfg.intervalMs));
  }
}

// Run only when invoked directly (not when imported by tests).
const invokedDirectly =
  typeof process !== "undefined" &&
  process.argv[1] &&
  /synthetic-canary\.(ts|js|mjs)$/.test(process.argv[1]);

if (invokedDirectly) {
  main(process.argv.slice(2)).then(
    (code) => process.exit(code),
    (err) => {
      console.error(`synthetic-canary crashed: ${errMsg(err)}`);
      process.exit(1);
    }
  );
}
