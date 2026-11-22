# @iln/sdk

The official TypeScript SDK for the Invoice Liquidity Network (ILN) smart contracts on Soroban.

This SDK allows freelancers, liquidity providers, and integrators to interact seamlessly with the ILN protocol. It provides a typed, easy-to-use interface over the raw Soroban RPC calls.

New to the protocol vocabulary? See the ILN [Glossary](../docs/glossary.md) for definitions of terms such as invoice factoring, LP, SAC, XDR, and effective yield.

## Installation

```bash
npm install @iln/sdk
```

Or with `pnpm`:
```bash
pnpm add @iln/sdk
```

Or with `yarn`:
```bash
yarn add @iln/sdk
```

## Quick Start

### Browser (Freighter Wallet)

```typescript
import { ILNClient } from "@iln/sdk";
import { isAllowed, setAllowed, getUserInfo, signTransaction } from "@stellar/freighter-api";

// Initialize the client connected to testnet
const client = ILNClient.testnet(async (tx) => {
  if (!(await isAllowed())) await setAllowed();
  const signedXdr = await signTransaction(tx.toXDR(), { network: "TESTNET" });
  // (convert string back to Transaction depending on freighter wrapper)
  return signedXdr;
});

// Fetch reputation for a user
const rep = await client.getReputation("G...");
console.log(rep.score);
```

### Node.js (Keypair)

```typescript
import { ILNClient } from "@iln/sdk";
import { Keypair } from "@stellar/stellar-sdk";

const signer = Keypair.fromSecret("S...");
const client = ILNClient.testnet(
  (tx) => {
    tx.sign(signer);
    return tx;
  }
);

// Fetch protocol stats
const stats = await client.getContractStats();
console.log(`Total Invoices: ${stats.totalInvoices}`);
```

## Method Reference

The SDK exports free functions that you can use with raw Soroban RPC servers, but you can also use `ILNClient` for read methods or call them directly.

### submitInvoice

```typescript
import { submitInvoice, Networks } from "@iln/sdk";

const { invoiceId, txHash } = await submitInvoice(
  server,
  contractAddress,
  {
    payer: "G...",
    amount: 1000n,
    dueDate: Math.floor(Date.now() / 1000) + 86400 * 30, // 30 days from now
    discountRate: 300, // 3%
    token: "C..."
  },
  sourceAccount,
  signTx,
  Networks.TESTNET
);
console.log(`Submitted invoice ID: ${invoiceId}`);
```

### cancelInvoice

```typescript
import { cancelInvoice, Networks } from "@iln/sdk";

const { txHash } = await cancelInvoice(
  server, 
  contractAddress, 
  42n, 
  sourceAccount, 
  signTx, 
  Networks.TESTNET
);
```

### fundInvoice

```typescript
import { fundInvoice, Networks } from "@iln/sdk";

const { txHash, effectiveYieldBps } = await fundInvoice(
  server,
  contractAddress,
  lpKeypair,
  42n,
  {
    onApprovalRequired: ({ requiredAmount }) => console.log(`Approving ${requiredAmount}`),
    onFunded: ({ effectiveYieldBps }) => console.log(`Funded yielding ${effectiveYieldBps} bps`)
  },
  Networks.TESTNET
);
```

### markPaid

```typescript
import { markPaid, Networks } from "@iln/sdk";

const { txHash, remainingBalance, fullySettled } = await markPaid(
  server,
  contractAddress,
  42n,
  100n, // partial payment
  sourceAccount,
  signTx,
  Networks.TESTNET
);
```

### Queries

```typescript
import { getInvoice, listInvoicesBySubmitter, listInvoicesByLP, Networks } from "@iln/sdk";

// Get single invoice
const invoice = await getInvoice(server, contractAddress, 42n, sourceAccount, Networks.TESTNET);

// List by submitter
const submitterInvoices = await listInvoicesBySubmitter(server, contractAddress, "G...", sourceAccount, Networks.TESTNET);

// List by LP
const lpInvoices = await listInvoicesByLP(server, contractAddress, "G...", sourceAccount, Networks.TESTNET);
```

## Error Handling

All contract-specific errors and validation errors throw subclasses of `ILNError`. This allows you to catch and handle them appropriately in your UI.

```typescript
import { ILNError, submitInvoice } from "@iln/sdk";

try {
  await submitInvoice(/* ... */);
} catch (error) {
  if (error instanceof ILNError.InvalidAmount) {
    console.error("The amount provided was zero or negative.");
  } else if (error instanceof ILNError.AlreadyFunded) {
    console.error("This invoice has already been funded.");
  } else if (error instanceof ILNError) {
    console.error("An ILN protocol error occurred:", error.message, error.code);
  } else {
    console.error("An unexpected error occurred:", error);
  }
}
```

## Integration Tests (testnet)

In addition to the mocked unit tests, the SDK ships an integration suite under
`tests/integration/` that exercises the full invoice lifecycle (submit → fund →
mark paid) against the **live Stellar testnet** deployment. This verifies that
XDR encoding, contract addresses and signing flows work end-to-end against real
Soroban.

The suite requires two Friendbot-funded testnet keypairs, supplied via
environment variables:

| Variable                | Description                                          |
| ----------------------- | ---------------------------------------------------- |
| `TEST_SUBMITTER_SECRET` | Secret key (`S…`) used to submit and cancel invoices |
| `TEST_LP_SECRET`        | Secret key (`S…`) used to fund invoices as the LP    |
| `TEST_RPC_URL`          | Optional override for the Soroban RPC endpoint       |

Both accounts are auto-funded via Friendbot at the start of the run. Generate
keypairs with `Keypair.random()` (or the Stellar Laboratory) and export their
secrets before running:

```bash
export TEST_SUBMITTER_SECRET="S..."
export TEST_LP_SECRET="S..."
npm run test:integration
```

When the secrets are absent the suite is skipped, so it never breaks the normal
`npm test` run. The tests clean up after themselves by cancelling any invoice
they leave in a `Pending` state. CI runs them nightly via
`.github/workflows/sdk-integration.yml`.

## TypeScript Integration

The SDK is written in TypeScript and exports all necessary types. You can import types like `Invoice`, `ReputationProfile`, `FundOptions`, and compose them into your own application's state or props.

```typescript
import type { Invoice, ReputationProfile, ContractStats } from "@iln/sdk/types";

function renderInvoiceStatus(invoice: Invoice) {
  if (invoice.status === "Pending") {
    return "Waiting for liquidity";
  }
  return invoice.status;
}
```
