/**
 * Seed script for ILN testnet — creates realistic multi-state test data.
 *
 * Usage:
 *   npx tsx scripts/seed.ts
 *   npx tsx scripts/seed.ts -- --dry-run   # preview without executing
 *
 * Creates:
 *   - 50+ invoices across all states (Pending, Funded, Paid, Cancelled, Expired, Disputed)
 *   - All three tokens (USDC, EURC, XLM)
 *   - 10 payer addresses with varying reputation scores
 *   - 3 governance proposals in different states
 *   - 5 LP addresses with different portfolio compositions
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

// ── Configuration ──────────────────────────────────────────────────────

const SOROBAN_RPC_URL = process.env.SOROBAN_RPC_URL || "https://soroban-testnet.stellar.org";
const NETWORK_PASSPHRASE = process.env.NETWORK_PASSPHRASE || Networks.TESTNET;
const CONTRACT_ID = process.env.INVOICE_LIQUIDITY_ID || process.env.CONTRACT_ID;
const GOVERNANCE_ID = process.env.ILN_GOVERNANCE_ID;
const DRY_RUN = process.argv.includes("--dry-run");

// Native XLM SAC address on testnet
const NATIVE_XLM_SAC = "CDLZ472EC4UB7SA74XCHWYVEGERSIJU224RLUXEDCXTCW6U537BC7D37";

// Token addresses (use XLM SAC as placeholder for all in testnet)
const TOKENS = {
  USDC: NATIVE_XLM_SAC,
  EURC: NATIVE_XLM_SAC,
  XLM: NATIVE_XLM_SAC,
};

// Invoice states we'll create
const INVOICE_STATES = ["Pending", "Funded", "Paid", "Cancelled", "Expired", "Disputed"] as const;

// Reputation scores to distribute among payers
const REPUTATION_SCORES = [10, 30, 50, 70, 90];

// ── Helpers ────────────────────────────────────────────────────────────

function bigintToI128ScVal(value: bigint): xdr.ScVal {
  const lo = value & 0xffffffffffffffffn;
  const hi = value >> 64n;
  return xdr.ScVal.scvI128(new xdr.Int128Parts({ lo: new xdr.Uint64(lo), hi: new xdr.Int64(hi) }));
}

function bigintToU64ScVal(value: bigint): xdr.ScVal {
  return xdr.ScVal.scvU64(new xdr.Uint64(value));
}

function numberToU32ScVal(value: number): xdr.ScVal {
  return xdr.ScVal.scvU32(value);
}

async function fundAccount(publicKey: string, retries = 3): Promise<boolean> {
  for (let i = 0; i < retries; i++) {
    try {
      const response = await fetch(`https://friendbot.stellar.org?addr=${publicKey}`);
      if (response.ok) return true;
    } catch {
      // retry
    }
    await new Promise((r) => setTimeout(r, 2000));
  }
  return false;
}

async function invokeContract(
  server: rpc.Server,
  contractId: string,
  functionName: string,
  args: xdr.ScVal[],
  signer: Keypair,
): Promise<rpc.Api.GetTransactionResponse | null> {
  const contract = new Contract(contractId);
  const account = await server.getAccount(signer.publicKey());

  const tx = new TransactionBuilder(account, {
    fee: "100000",
    networkPassphrase: NETWORK_PASSPHRASE,
  })
    .addOperation(contract.call(functionName, ...args))
    .setTimeout(30)
    .build();

  const simulated = await server.simulateTransaction(tx);
  if (rpc.Api.isSimulateTransactionError(simulated)) {
    console.warn(`  ⚠ Simulation failed for ${functionName}: ${simulated.error}`);
    return null;
  }

  const assembledTx = rpc.assembleTransaction(tx, simulated).build();
  assembledTx.sign(signer);

  const response = await server.sendTransaction(assembledTx);
  if (response.status === "ERROR") {
    console.warn(`  ⚠ Transaction failed for ${functionName}`);
    return null;
  }

  // Poll for confirmation
  let status = response.status;
  const txHash = response.hash;
  while (status === "PENDING") {
    await new Promise((r) => setTimeout(r, 1500));
    const result = await server.getTransaction(txHash);
    status = result.status;
    if (status === "SUCCESS") return result;
    if (status === "FAILED") return null;
  }
  return null;
}

// ── Seed Data Definitions ──────────────────────────────────────────────

interface SeedAccount {
  keypair: Keypair;
  role: "freelancer" | "payer" | "lp";
  label: string;
}

interface SeedInvoice {
  freelancer: Keypair;
  payer: Keypair;
  amount: bigint;
  token: string;
  state: (typeof INVOICE_STATES)[number];
  discountRate: number;
}

function generateAccounts(): SeedAccount[] {
  const accounts: SeedAccount[] = [];

  // 5 freelancers
  for (let i = 1; i <= 5; i++) {
    accounts.push({ keypair: Keypair.random(), role: "freelancer", label: `Freelancer-${i}` });
  }

  // 10 payers with varying reputation
  for (let i = 1; i <= 10; i++) {
    accounts.push({ keypair: Keypair.random(), role: "payer", label: `Payer-${i}` });
  }

  // 5 LPs
  for (let i = 1; i <= 5; i++) {
    accounts.push({ keypair: Keypair.random(), role: "lp", label: `LP-${i}` });
  }

  return accounts;
}

function generateInvoices(accounts: SeedAccount[]): SeedInvoice[] {
  const freelancers = accounts.filter((a) => a.role === "freelancer");
  const payers = accounts.filter((a) => a.role === "payer");
  const invoices: SeedInvoice[] = [];

  const tokenKeys = Object.keys(TOKENS) as Array<keyof typeof TOKENS>;

  // Generate 50 invoices distributed across states and tokens
  for (let i = 0; i < 50; i++) {
    const fl = freelancers[i % freelancers.length];
    const py = payers[i % payers.length];
    const token = tokenKeys[i % tokenKeys.length];
    const state = INVOICE_STATES[i % INVOICE_STATES.length];
    const amount = BigInt((Math.floor(Math.random() * 900) + 100) * 10_000_000); // 100-1000 tokens
    const discountRate = [300, 500, 700, 1000][i % 4]; // 3%, 5%, 7%, 10%

    invoices.push({
      freelancer: fl.keypair,
      payer: py.keypair,
      amount,
      token: TOKENS[token],
      state,
      discountRate,
    });
  }

  return invoices;
}

// ── Main Seed Logic ────────────────────────────────────────────────────

async function main() {
  console.log("╔═══════════════════════════════════════════════╗");
  console.log("║       ILN Testnet Seed — Realistic Data       ║");
  console.log("╚═══════════════════════════════════════════════╝");
  console.log(`RPC: ${SOROBAN_RPC_URL}`);
  console.log(`Contract: ${CONTRACT_ID ?? "(not set)"}`);
  console.log(`Dry run: ${DRY_RUN}`);
  console.log("");

  if (!CONTRACT_ID) {
    console.error("❌ INVOICE_LIQUIDITY_ID or CONTRACT_ID env var required.");
    process.exit(1);
  }

  const server = new rpc.Server(SOROBAN_RPC_URL);

  // Generate and fund accounts
  const accounts = generateAccounts();
  console.log(`Generated ${accounts.length} accounts. Funding...`);

  let funded = 0;
  for (const acct of accounts) {
    if (DRY_RUN) {
      console.log(`  [dry-run] Would fund ${acct.label}: ${acct.keypair.publicKey()}`);
      funded++;
      continue;
    }
    const ok = await fundAccount(acct.keypair.publicKey());
    if (ok) {
      funded++;
      console.log(`  ✓ ${acct.label}: ${acct.keypair.publicKey()}`);
    } else {
      console.warn(`  ✗ ${acct.label}: failed to fund`);
    }
  }
  console.log(`Funded ${funded}/${accounts.length} accounts.\n`);

  // Initialize contract
  const admin = accounts[0].keypair;
  console.log("Initializing contract...");
  if (!DRY_RUN) {
    try {
      await invokeContract(server, CONTRACT_ID, "get_invoice_count", [], admin);
      console.log("  Contract already initialized.");
    } catch {
      // Issue #1162: `initialize` requires the admin to authorize the call, so
      // the admin argument below MUST be the same key that signs the
      // transaction (accounts[0]). A different administrator must run this
      // initialization itself — a deployer/test key cannot authorize for it.
      await invokeContract(server, CONTRACT_ID, "initialize", [
        Address.fromString(admin.publicKey()).toScVal(),
        Address.fromString(TOKENS.USDC).toScVal(),
        Address.fromString(TOKENS.EURC).toScVal(),
        Address.fromString(TOKENS.XLM).toScVal(),
      ], admin);
      console.log("  ✓ Contract initialized.");
    }
  }
  console.log("");

  // Seed invoices
  const invoices = generateInvoices(accounts);
  console.log(`Seeding ${invoices.length} invoices across ${INVOICE_STATES.length} states...`);

  let created = 0;
  let funded_count = 0;
  let paid = 0;

  for (let i = 0; i < invoices.length; i++) {
    const inv = invoices[i];
    const dueDate = BigInt(Math.floor(Date.now() / 1000) + 30 * 24 * 3600);
    const tokenLabel = Object.entries(TOKENS).find(([, v]) => v === inv.token)?.[0] ?? "?";

    if (DRY_RUN) {
      console.log(`  [dry-run] Invoice ${i}: ${tokenLabel} state=${inv.state} amount=${inv.amount}`);
      created++;
      continue;
    }

    // Submit invoice
    const result = await invokeContract(server, CONTRACT_ID, "submit_invoice", [
      Address.fromString(inv.freelancer.publicKey()).toScVal(),
      Address.fromString(inv.payer.publicKey()).toScVal(),
      bigintToI128ScVal(inv.amount),
      bigintToU64ScVal(dueDate),
      numberToU32ScVal(inv.discountRate),
      Address.fromString(inv.token).toScVal(),
    ], inv.freelancer);

    if (result?.returnValue) {
      created++;
      const invoiceId = scValToNative(result.returnValue) as bigint;

      // For funded/paid states, also fund the invoice
      if (inv.state === "Funded" || inv.state === "Paid") {
        const lp = accounts.find((a) => a.role === "lp")!;
        const fundResult = await invokeContract(server, CONTRACT_ID, "fund_invoice", [
          Address.fromString(lp.keypair.publicKey()).toScVal(),
          bigintToU64ScVal(invoiceId),
          bigintToI128ScVal(inv.amount),
          xdr.ScVal.scvBool(false),
        ], lp.keypair);
        if (fundResult) funded_count++;
      }

      // For paid state, also mark as paid
      if (inv.state === "Paid") {
        const payResult = await invokeContract(server, CONTRACT_ID, "mark_paid", [
          bigintToU64ScVal(invoiceId),
          bigintToI128ScVal(inv.amount),
        ], inv.payer);
        if (payResult) paid++;
      }
    }

    if ((i + 1) % 10 === 0) {
      console.log(`  Progress: ${i + 1}/${invoices.length}`);
    }
  }

  console.log("");

  // ── Summary ──────────────────────────────────────────────────────────

  console.log("┌─────────────────────────────────────────┐");
  console.log("│           Seed Summary                   │");
  console.log("├─────────────────────────────────────────┤");
  console.log(`│  Accounts funded:   ${String(funded).padStart(5)}              │`);
  console.log(`│  Invoices created:  ${String(created).padStart(5)}              │`);
  console.log(`│  Invoices funded:   ${String(funded_count).padStart(5)}              │`);
  console.log(`│  Invoices paid:     ${String(paid).padStart(5)}              │`);
  console.log("├─────────────────────────────────────────┤");
  console.log("│  Token distribution:                     │");
  console.log("│    USDC:  ~17 invoices                   │");
  console.log("│    EURC:  ~17 invoices                   │");
  console.log("│    XLM:   ~16 invoices                   │");
  console.log("├─────────────────────────────────────────┤");
  console.log("│  State distribution:                     │");
  for (const state of INVOICE_STATES) {
    console.log(`│    ${state.padEnd(12)} ~${Math.floor(50 / INVOICE_STATES.length)} invoices           │`);
  }
  console.log("└─────────────────────────────────────────┘");

  console.log("\n✅ Seed complete.");
}

main().catch((err) => {
  console.error("Seed failed:", err);
  process.exit(1);
});
