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

// Configuration from environment variables
const SOROBAN_RPC_URL = process.env.SOROBAN_RPC_URL || "https://soroban-testnet.stellar.org";
const NETWORK_PASSPHRASE = process.env.NETWORK_PASSPHRASE || Networks.TESTNET;
const CONTRACT_ID = process.env.CONTRACT_ID;
const DISTRIBUTION_CONTRACT_ID = process.env.DISTRIBUTION_CONTRACT_ID;

if (!CONTRACT_ID) {
  console.error("❌ Error: CONTRACT_ID environment variable is required.");
  process.exit(1);
}

// Native XLM token SAC address on testnet
const NATIVE_XLM_SAC = "CDLZ472EC4UB7SA74XCHWYVEGERSIJU224RLUXEDCXTCW6U537BC7D37";

// Helper to convert bigint to ScVal i128
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

// Helper to convert bigint to ScVal u64
function bigintToU64ScVal(value: bigint): xdr.ScVal {
  return xdr.ScVal.scvU64(new xdr.Uint64(value));
}

// Helper to convert number to ScVal u32
function numberToU32ScVal(value: number): xdr.ScVal {
  return xdr.ScVal.scvU32(value);
}

// Helper to fund accounts using Stellar Friendbot
async function fundAccount(publicKey: string, retries = 3) {
  for (let i = 0; i < retries; i++) {
    try {
      console.log(`Funding account ${publicKey} via Friendbot (Attempt ${i + 1}/${retries})...`);
      const response = await fetch(`https://friendbot.stellar.org?addr=${publicKey}`);
      if (response.ok) {
        console.log(`✓ Successfully funded ${publicKey}`);
        return;
      }
      console.warn(`Friendbot warning: ${response.status} - ${await response.text()}`);
    } catch (err) {
      console.warn(`Friendbot error:`, err);
    }
    await new Promise((resolve) => setTimeout(resolve, 2000));
  }
  throw new Error(`Failed to fund account ${publicKey} via Friendbot after ${retries} attempts.`);
}

// Helper to invoke a contract function
async function invokeContract(
  server: rpc.Server,
  functionName: string,
  args: xdr.ScVal[],
  signer: Keypair
) {
  const contract = new Contract(CONTRACT_ID!);
  const sourceAddress = signer.publicKey();
  const account = await server.getAccount(sourceAddress);

  const tx = new TransactionBuilder(account, {
    fee: "100000",
    networkPassphrase: NETWORK_PASSPHRASE,
  })
    .addOperation(contract.call(functionName, ...args))
    .setTimeout(30)
    .build();

  console.log(`Simulating function '${functionName}'...`);
  const simulated = await server.simulateTransaction(tx);
  if (rpc.Api.isSimulationError(simulated)) {
    throw new Error(`Simulation failed for '${functionName}': ${JSON.stringify(simulated.error)}`);
  }

  const assembledTx = rpc.assembleTransaction(tx, simulated).build();
  assembledTx.sign(signer);

  const response = await server.sendTransaction(assembledTx);
  if (response.status === "ERROR") {
    throw new Error(`Send transaction failed: ${JSON.stringify(response.errorResultXdr)}`);
  }

  let status = response.status;
  const txHash = response.hash;
  console.log(`Transaction '${functionName}' submitted (hash: ${txHash}). Polling...`);

  while (status === "PENDING") {
    await new Promise((resolve) => setTimeout(resolve, 1500));
    const txResult = await server.getTransaction(txHash);
    status = txResult.status;
    if (status === "SUCCESS") {
      console.log(`✓ Transaction succeeded!`);
      return txResult;
    } else if (status === "FAILED") {
      throw new Error(`Transaction failed: ${JSON.stringify(txResult)}`);
    }
  }

  throw new Error(`Unexpected transaction status: ${status}`);
}

// Read-only contract call: simulate (never submit) and return the native value.
async function simulateReadonly(
  server: rpc.Server,
  contractId: string,
  functionName: string,
  args: xdr.ScVal[],
  signer: Keypair
) {
  const contract = new Contract(contractId);
  const account = await server.getAccount(signer.publicKey());

  const tx = new TransactionBuilder(account, {
    fee: "100000",
    networkPassphrase: NETWORK_PASSPHRASE,
  })
    .addOperation(contract.call(functionName, ...args))
    .setTimeout(30)
    .build();

  console.log(`Simulating read-only '${functionName}' on ${contractId}...`);
  const simulated = await server.simulateTransaction(tx);
  if (rpc.Api.isSimulationError(simulated)) {
    throw new Error(`Simulation failed for '${functionName}': ${JSON.stringify(simulated.error)}`);
  }
  if (!simulated.result?.retval) {
    throw new Error(`Simulation of '${functionName}' returned no value.`);
  }
  return scValToNative(simulated.result.retval);
}

async function runSmokeTest() {
  console.log("=========================================");
  console.log("🚀 ILN Smoke Test: Stellar Testnet");
  console.log(`RPC URL: ${SOROBAN_RPC_URL}`);
  console.log(`Contract ID: ${CONTRACT_ID}`);
  console.log("=========================================\n");

  const server = new rpc.Server(SOROBAN_RPC_URL);

  // Generate roles
  const freelancer = Keypair.random();
  const payer = Keypair.random();
  const lp = Keypair.random();

  console.log(`Freelancer: ${freelancer.publicKey()}`);
  console.log(`Payer:      ${payer.publicKey()}`);
  console.log(`LP/Funder:  ${lp.publicKey()}`);
  console.log("");

  // Fund all roles
  await fundAccount(freelancer.publicKey());
  await fundAccount(payer.publicKey());
  await fundAccount(lp.publicKey());
  console.log("");

  // Step 1: Initialize contract if not initialized
  try {
    console.log("Step 1: Checking if contract is initialized...");
    await invokeContract(server, "get_invoice_count", [], freelancer);
    console.log("Contract is already initialized.");
  } catch (e) {
    console.log("Contract not initialized. Initializing now...");
    const initArgs = [
      Address.fromString(freelancer.publicKey()).toScVal(), // admin
      Address.fromString(NATIVE_XLM_SAC).toScVal(),         // usdc_token
      Address.fromString(NATIVE_XLM_SAC).toScVal(),         // eurc_token
      Address.fromString(NATIVE_XLM_SAC).toScVal(),         // xlm_token
    ];
    await invokeContract(server, "initialize", initArgs, freelancer);
    console.log("✓ Contract successfully initialized!");
  }
  console.log("");

  // Step 1b: Verify the governance-token SAC-admin invariant (Issue #861).
  // `iln_distribution.claim_tokens` mints through StellarAssetClient::mint,
  // which only the SAC admin may call, so a token administered by an
  // EOA/multisig would make every non-empty claim revert at runtime.
  // Optional: skipped unless DISTRIBUTION_CONTRACT_ID is set.
  if (DISTRIBUTION_CONTRACT_ID) {
    console.log("Step 1b: Verifying distribution mint authority...");
    const mintAuthority = await simulateReadonly(
      server,
      DISTRIBUTION_CONTRACT_ID,
      "verify_mint_authority",
      [],
      freelancer
    );
    if (mintAuthority !== true) {
      throw new Error(
        `verify_mint_authority() returned ${JSON.stringify(mintAuthority)} on ` +
          `${DISTRIBUTION_CONTRACT_ID}: the governance token is not administered by the ` +
          "distribution contract, so claims cannot mint."
      );
    }
    console.log("✓ verify_mint_authority() = true\n");
  } else {
    console.log(
      "Step 1b: DISTRIBUTION_CONTRACT_ID not set — skipping mint-authority check.\n"
    );
  }

  // Step 2: Submit Invoice
  console.log("Step 2: Submitting a test invoice...");
  const invoiceAmount = 1000000000n; // 100 XLM (7 decimals)
  const dueDate = BigInt(Math.floor(Date.now() / 1000) + 30 * 24 * 3600); // 30 days from now
  const discountRate = 500; // 5% (500 basis points)

  const submitArgs = [
    Address.fromString(freelancer.publicKey()).toScVal(),
    Address.fromString(payer.publicKey()).toScVal(),
    bigintToI128ScVal(invoiceAmount),
    bigintToU64ScVal(dueDate),
    numberToU32ScVal(discountRate),
    Address.fromString(NATIVE_XLM_SAC).toScVal(),
  ];

  const submitResult = await invokeContract(server, "submit_invoice", submitArgs, freelancer);
  if (!submitResult.returnValue) {
    throw new Error("Submit invoice did not return a value.");
  }
  const invoiceId = scValToNative(submitResult.returnValue) as bigint;
  console.log(`✓ Invoice submitted successfully with ID: ${invoiceId}\n`);

  // Step 3: Fund Invoice
  console.log("Step 3: Funding the invoice...");
  const fundArgs = [
    Address.fromString(lp.publicKey()).toScVal(),
    bigintToU64ScVal(invoiceId),
    bigintToI128ScVal(invoiceAmount),
    xdr.ScVal.scvBool(false), // require_oracle_verification = false
  ];

  await invokeContract(server, "fund_invoice", fundArgs, lp);
  console.log("✓ Invoice successfully funded!\n");

  // Step 4: Mark Paid
  console.log("Step 4: Payer settling the invoice...");
  const payArgs = [
    bigintToU64ScVal(invoiceId),
    bigintToI128ScVal(invoiceAmount),
  ];

  await invokeContract(server, "mark_paid", payArgs, payer);
  console.log("✓ Invoice successfully paid!\n");

  // Step 5: Query Final State and Assert
  console.log("Step 5: Verifying final state...");
  const getArgs = [bigintToU64ScVal(invoiceId)];
  const getResult = await invokeContract(server, "get_invoice", getArgs, freelancer);
  if (!getResult.returnValue) {
    throw new Error("Failed to retrieve final invoice state.");
  }
  
  const invoiceState = scValToNative(getResult.returnValue);
  console.log("Invoice State:", JSON.stringify(invoiceState, null, 2));

  // Extract and verify status
  const status = invoiceState.status;
  const statusStr = typeof status === "string" ? status : (status && status.name ? status.name : "");

  if (statusStr.toLowerCase() === "paid" || status === 3 || statusStr === "Paid") {
    console.log("\n🎉 Smoke test completed successfully!");
    console.log("=========================================");
    process.exit(0);
  } else {
    throw new Error(`Assertion failed: Expected invoice status to be 'Paid', but got: ${JSON.stringify(status)}`);
  }
}

runSmokeTest().catch((err) => {
  console.error("\n❌ Smoke test failed!");
  console.error(err);
  console.log("=========================================");
  process.exit(1);
});
