# SDK Integration Guide

> External protocol partners: see also [`integration-partner-onboarding.md`](integration-partner-onboarding.md) for stable surfaces, upgrade expectations, and the mainnet go-live checklist.

A single, end-to-end integration guide for third-party builders working with the
Invoice Liquidity Network (ILN) on Stellar/Soroban. Every flow below uses the
official [`@iln/sdk`](../sdk/README.md) package and is exercised against the
**testnet** deployment.

- [Quick Start](#quick-start)
  - [Node.js (Keypair)](#nodejs-keypair)
  - [Browser (Freighter)](#browser-freighter)
- [Freelancer Flow](#freelancer-flow) — submit, cancel
- [LP Flow](#lp-flow) — browse marketplace, fund, transfer
- [Payer Flow](#payer-flow) — pay, dispute
- [Governance](#governance) — propose, vote, execute
- [Analytics](#analytics) — reputation, stats, event stream
- [Error Handling](#error-handling)
- [Testing Against Testnet](#testing-against-testnet)

> **Conventions.** All amounts are `bigint` values in the token's smallest unit
> (USDC and XLM use 7 decimals on Stellar, so `10_000000n` = 10.0). Discount and
> yield rates are expressed in basis points (`300` = 3.00 %). Invoice IDs are
> `bigint`.

---

## Quick Start

### Install

```bash
npm install @iln/sdk @stellar/stellar-sdk
# browser wallet integration also needs:
npm install @stellar/freighter-api
```

### Testnet constants

```ts
import { Networks } from "@stellar/stellar-sdk";

export const RPC_URL = "https://soroban-testnet.stellar.org";
export const NETWORK_PASSPHRASE = Networks.TESTNET;
// Primary invoice_liquidity contract (see project README → Testnet deployment).
export const CONTRACT_ID = "CD3TE3IAHM737P236XZL2OYU275ZKD6MN7YH7PYYAXYIGEH55OPEWYJC";
// Testnet USDC issued as a Stellar Asset Contract (SAC).
export const USDC_TOKEN = "CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIHMXQDAMA";
```

The SDK ships two environments out of the box. Read-only calls work without a
signer; state-changing calls need one.

### Node.js (Keypair)

Use a `KeypairSigner` for scripts, bots, and backends. **Never hard-code secret
keys** — read them from the environment.

```ts
import { SorobanRpc, Keypair, type Transaction } from "@stellar/stellar-sdk";
import { ILNClient, KeypairSigner } from "@iln/sdk";
import { RPC_URL, CONTRACT_ID, NETWORK_PASSPHRASE } from "./config";

const server = new SorobanRpc.Server(RPC_URL);
const keypair = Keypair.fromSecret(process.env.ILN_SECRET_KEY!);
const signer = new KeypairSigner(keypair);

// ILNClient is the easiest entry point for read methods.
const client = ILNClient.testnet(signer, { contractId: CONTRACT_ID });

const stats = await client.getContractStats();
console.log(`Total invoices: ${stats.totalInvoices}`);
// → Total invoices: 128
```

Many write helpers are exported as free functions and take an explicit
`signTransaction` callback. For a keypair that callback is simply:

```ts
const signTx = (tx: Transaction) => {
  tx.sign(keypair);
  return tx;
};
```

### Browser (Freighter)

In the browser, delegate signing to the user's Freighter wallet so private keys
never leave it.

```ts
import {
  SorobanRpc,
  TransactionBuilder,
  type Transaction,
} from "@stellar/stellar-sdk";
import {
  isConnected,
  requestAccess,
  getAddress,
  signTransaction as freighterSign,
} from "@stellar/freighter-api";
import { ILNClient, FreighterSigner } from "@iln/sdk";
import { RPC_URL, CONTRACT_ID, NETWORK_PASSPHRASE } from "./config";

const server = new SorobanRpc.Server(RPC_URL);

if (!(await isConnected())) throw new Error("Freighter not installed");
await requestAccess();
const { address: publicKey } = await getAddress();

// signTransaction callback for the free-function helpers.
const signTx = async (tx: Transaction) => {
  const { signedTxXdr } = await freighterSign(tx.toXDR(), {
    networkPassphrase: NETWORK_PASSPHRASE,
    address: publicKey,
  });
  return TransactionBuilder.fromXDR(signedTxXdr, NETWORK_PASSPHRASE) as Transaction;
};

// Read-only client (no signer required for queries):
const client = ILNClient.testnet(new FreighterSigner(), { contractId: CONTRACT_ID });
const rep = await client.getReputation(publicKey);
console.log(`Reputation score: ${rep.score}`);
```

> In every flow below, `server`, `account`, and `signTx` refer to the values
> created in this section. `account` is fetched per transaction with
> `await server.getAccount(publicKey)` so the sequence number is fresh.

---

## Freelancer Flow

Freelancers tokenize an unpaid invoice so a liquidity provider can fund it early.

### Submit an invoice

`submitInvoice` validates inputs locally before building the transaction, then
returns the new on-chain invoice ID.

```ts
import { submitInvoice } from "@iln/sdk";
import { CONTRACT_ID, NETWORK_PASSPHRASE, USDC_TOKEN } from "./config";

const account = await server.getAccount(freelancerPublicKey);

const { invoiceId, txHash } = await submitInvoice(
  server,
  CONTRACT_ID,
  {
    payer: payerPublicKey,           // G… address expected to pay
    amount: 1_000_0000000n,          // 1,000 USDC (7 decimals)
    dueDate: Math.floor(Date.now() / 1000) + 60 * 60 * 24 * 30, // +30 days
    discountRate: 300,               // 3.00 % — the LP's reward
    token: USDC_TOKEN,
  },
  account,
  signTx,
  NETWORK_PASSPHRASE
);

console.log(`Invoice #${invoiceId} submitted in tx ${txHash}`);
// → Invoice #129 submitted in tx 9f3c…
```

**Returns:** `{ invoiceId: bigint, txHash: string }`.

**Errors:** throws `ILNError` with code `InvalidAmount` (amount ≤ 0),
`InvalidDiscountRate` (outside 1–5000 bps), `DueDateTooSoon` (< 24h),
or `DueDateTooFar` (> 365 days). See [Error Handling](#error-handling).

### Cancel an invoice

Only the submitter can cancel, and only while the invoice is still `Pending`
(unfunded).

```ts
import { cancelInvoice } from "@iln/sdk";

const account = await server.getAccount(freelancerPublicKey);

const { txHash } = await cancelInvoice(
  server,
  CONTRACT_ID,
  129n,            // invoiceId
  account,
  signTx,
  NETWORK_PASSPHRASE
);

console.log(`Invoice cancelled: ${txHash}`);
```

**Returns:** `{ txHash: string }`.

**Errors:** `ILNError` `NotAuthorized` (caller is not the submitter) or
`InvalidStatus` (already funded/paid).

---

## LP Flow

Liquidity providers (LPs) browse open invoices, fund the ones they like, and can
transfer a funded position to another LP on a secondary market.

### Browse the marketplace

For discovery, query the **indexer API** (fast, paginated, off-chain). For
authoritative on-chain reads, use the SDK query helpers.

```ts
// Off-chain marketplace listing via the indexer REST API.
const res = await fetch(
  "https://indexer.testnet.iln.network/invoices?status=pending&page=1&pageSize=20"
);
const { invoices, total } = await res.json();
console.log(`${total} open invoices; first id = ${invoices[0]?.id}`);
```

```ts
// On-chain reads via the SDK.
import { getInvoice, listInvoicesBySubmitter, listInvoicesByLP } from "@iln/sdk";

const account = await server.getAccount(lpPublicKey);

const invoice = await getInvoice(server, CONTRACT_ID, 129n, account, NETWORK_PASSPHRASE);
console.log(invoice.status, invoice.amount, invoice.discountRate);

const submitted = await listInvoicesBySubmitter(
  server,
  CONTRACT_ID,
  freelancerPublicKey,
  account,
  NETWORK_PASSPHRASE,
  0,    // page
  50    // pageSize
);

const myPositions = await listInvoicesByLP(
  server,
  CONTRACT_ID,
  lpPublicKey,
  account,
  NETWORK_PASSPHRASE
);
```

**Returns:** `getInvoice` → a full `Invoice`; `listInvoicesBySubmitter` /
`listInvoicesByLP` → `Invoice[]`.

### Fund an invoice

`fundInvoice` handles the SEP-41 token allowance automatically — it checks the
LP's current allowance, submits an approval if needed, then funds. Lifecycle
callbacks let you surface progress in a UI.

```ts
import { fundInvoice } from "@iln/sdk";

const result = await fundInvoice(
  server,
  CONTRACT_ID,
  lpKeypair,        // Keypair (Node.js); funding requires a real signer
  129n,             // invoiceId
  {
    onApprovalRequired: ({ requiredAmount, currentAllowance }) =>
      console.log(`Approving ${requiredAmount} (have ${currentAllowance})`),
    onApprovalSent: ({ approveTxHash }) =>
      console.log(`Approval tx: ${approveTxHash}`),
    onFunded: ({ effectiveYieldBps, invoiceId }) =>
      console.log(`Funded #${invoiceId} @ ${effectiveYieldBps} bps`),
  },
  NETWORK_PASSPHRASE
);

console.log(`Funded in ${result.txHash}, yield ${result.effectiveYieldBps} bps`);
```

**Returns:** `{ txHash: string, effectiveYieldBps: number }` — the annualised
yield derived from the discount rate and remaining days to maturity.

**Errors:** `ILNError` `InsufficientAllowance`, `InvalidStatus` (already funded),
or `OracleStale` when `requireOracleVerification: true`.

### Transfer an LP position

A funded position can be reassigned to another LP (secondary market / OTC sale).
Only the current LP may transfer.

```ts
import { transferLPPosition } from "@iln/sdk";

const account = await server.getAccount(lpPublicKey);

const { txHash } = await transferLPPosition(
  server,
  CONTRACT_ID,
  129n,                 // invoiceId
  newLpPublicKey,       // G… address of the buyer
  account,
  signTx,
  NETWORK_PASSPHRASE
);
```

**Returns:** `{ txHash: string }`.

**Errors:** `ILNError` `NotAuthorized` (caller is not the current LP) or
`InvalidGAddress` (malformed destination address).

---

## Payer Flow

The payer settles the invoice (paying the LP, or the freelancer if unfunded) or
disputes it.

### Pay an invoice

`markPaid` settles the outstanding balance. Pass `undefined` for the amount to
settle in full, or a `bigint` for a partial payment.

```ts
import { markPaid } from "@iln/sdk";

const account = await server.getAccount(payerPublicKey);

const result = await markPaid(
  server,
  CONTRACT_ID,
  129n,             // invoiceId
  undefined,        // pay the full outstanding balance
  account,
  signTx,
  NETWORK_PASSPHRASE
);

console.log(`Paid ${result.amountPaid}; status now ${result.status}`);
```

**Returns:** `MarkPaidResult` — `{ txHash, amountPaid, status }`.

**Errors:** `ILNError` `InvalidStatus` (already paid/cancelled) or
`InsufficientAllowance` (token approval missing).

### Dispute an invoice

The SDK hashes your human-readable evidence with SHA-256 and submits only the
digest on-chain — the raw text never leaves the client.

```ts
import { disputeInvoice, KeypairSigner } from "@iln/sdk";

const result = await disputeInvoice({
  rpc: server,
  contractAddress: CONTRACT_ID,
  signer: new KeypairSigner(payerKeypair),
  invoiceId: 129n,
  evidence: "Goods never delivered — see support ticket #8842",
});

console.log(`Dispute tx: ${result.txHash}`);
console.log(`Evidence hash: ${result.evidenceHash}`);
```

**Returns:** `{ txHash: string, evidenceHash: string }`.

**Errors:** `ILNError` `NotAuthorized` (caller is not the payer) or
`InvalidStatus` (invoice not in a disputable state).

### Appeal a dispute ruling

If a dispute ruling is unfavourable, the losing party can file an appeal within
the appeal window.

```ts
import { appealInvoice, KeypairSigner } from "@iln/sdk";

const appeal = await appealInvoice({
  rpc: server,
  contractAddress: CONTRACT_ID,
  signer: new KeypairSigner(payerKeypair),
  invoiceId: 129n,
  reason: "Ruling ignored submitted evidence — see ticket #8842",
});

console.log(`Appeal tx: ${appeal.txHash}`);
```

**Returns:** `AppealInvoiceResult` — `{ txHash: string }`.

**Errors:** `ILNError` `NotAuthorized`, `InvalidStatus` (no ruling to appeal),
or `AppealWindowClosed` (appeal period has expired).

### Listen for dispute and appeal events

Subscribe to real-time dispute and appeal events to update UIs or trigger
notifications without polling.

```ts
import { subscribe } from "@iln/sdk";

const unsubscribe = subscribe(
  server,
  CONTRACT_ID,
  { types: ["invoice_disputed", "dispute_resolved", "invoice_appealed", "appeal_resolved"] },
  (event) => {
    console.log(event.type, event.invoiceId, event.ledger);
  }
);

// Stop listening when done.
unsubscribe();
```

For historical dispute/appeal events, query the indexer's `/events` endpoint
with a `type` filter; see [docs/events.md](events.md) for the full catalogue.

---

## Governance

Protocol parameters (e.g. max discount rate, insurance fee) are adjusted through
on-chain proposals. The lifecycle is **create → vote → execute**.

```ts
import {
  createProposal,
  castVote,
  executeProposal,
  getProposal,
  listProposals,
  ProposalAction,
  ProposalStatus,
} from "@iln/sdk";

const account = await server.getAccount(memberPublicKey);

// 1. Create a proposal (descriptionHash is a 32-byte hex digest of the rationale).
const { proposalId, txHash } = await createProposal(
  server,
  CONTRACT_ID,
  ProposalAction.UpdateMaxDiscountRate,
  500n,                 // proposed value (e.g. new max = 5.00 %)
  descriptionHash,      // hex string, 32 bytes
  account,
  signTx,
  NETWORK_PASSPHRASE
);

// 2. Cast a vote.
await castVote(server, CONTRACT_ID, proposalId, true /* support */, account, signTx, NETWORK_PASSPHRASE);

// 3. Inspect and, once passed, execute.
const proposal = await getProposal(server, CONTRACT_ID, proposalId, account, NETWORK_PASSPHRASE);
if (proposal.status === ProposalStatus.Passed) {
  await executeProposal(server, CONTRACT_ID, proposalId, account, signTx, NETWORK_PASSPHRASE);
}

// List with an optional status filter.
const active = await listProposals(server, CONTRACT_ID, account, NETWORK_PASSPHRASE, {
  status: ProposalStatus.Active,
});
```

**Returns:** `createProposal` → `{ proposalId, txHash }`; `castVote` /
`executeProposal` → `{ txHash }`; `getProposal` → `Proposal`; `listProposals`
→ `Proposal[]`.

**Errors:** `ILNError` `NotAuthorized`, `AlreadyVoted`, `ProposalNotActive`, or
`QuorumNotReached` (on execute). See [docs/governance.md](governance.md) for the
full state machine.

### Delegate votes

Token holders can delegate their voting power to another address, or undelegate
to reclaim it.

```ts
import { delegateVotes, undelegateVotes } from "@iln/sdk";

const account = await server.getAccount(memberPublicKey);

// Delegate voting power to a trusted representative.
const { txHash: delegateTx } = await delegateVotes(
  server,
  CONTRACT_ID,
  delegatePublicKey,
  account,
  signTx,
  NETWORK_PASSPHRASE
);
console.log(`Delegated votes in tx ${delegateTx}`);

// Reclaim voting power at any time.
const { txHash: undelegateTx } = await undelegateVotes(
  server,
  CONTRACT_ID,
  account,
  signTx,
  NETWORK_PASSPHRASE
);
console.log(`Undelegated votes in tx ${undelegateTx}`);
```

**Returns:** `{ txHash: string }` for both.

**Errors:** `ILNError` `NotAuthorized` (no active delegation to undo) or
`InvalidGAddress` (malformed delegate address).

### Listen for governance events

```ts
import { subscribe } from "@iln/sdk";

const unsubscribe = subscribe(
  server,
  CONTRACT_ID,
  { types: ["proposal_created", "vote_cast", "proposal_executed"] },
  (event) => {
    console.log(event.type, event.ledger);
  }
);

// Stop listening when done.
unsubscribe();
```

---

## Analytics

Read-only data for dashboards and reputation displays. None of these require a
signer.

### Reputation

```ts
import { getReputation } from "@iln/sdk";

const rep = await getReputation(server, CONTRACT_ID, freelancerPublicKey, NETWORK_PASSPHRASE);
console.log(rep.score, rep.invoicesSubmitted, rep.invoicesPaid, rep.invoicesDefaulted);
// Unknown addresses return a zeroed profile rather than throwing.
```

**Returns:** `ReputationProfile`.

### Protocol stats

```ts
import { getContractStats } from "@iln/sdk";

const stats = await getContractStats(server, CONTRACT_ID, NETWORK_PASSPHRASE);
console.log(stats.totalInvoices, stats.totalFunded, stats.totalVolume);
```

**Returns:** `ContractStats`.

### Live event stream

Subscribe to contract events to update UIs in real time.

```ts
import { subscribe } from "@iln/sdk";

const unsubscribe = subscribe(
  server,
  CONTRACT_ID,
  { types: ["invoice_funded", "invoice_paid"] },
  (event) => console.log(event.type, event.invoiceId, event.ledger)
);

// later…
unsubscribe();
```

For historical, paginated event data prefer the indexer's `/events` endpoint;
see [docs/events.md](events.md) for the full event catalogue.

---

## Error Handling

Every state-changing helper throws a typed `ILNError`. Switch on `ILNErrorCode`
to give users actionable messages.

```ts
import { submitInvoice, ILNError, ILNErrorCode } from "@iln/sdk";

try {
  await submitInvoice(server, CONTRACT_ID, params, account, signTx, NETWORK_PASSPHRASE);
} catch (e) {
  if (e instanceof ILNError) {
    switch (e.code) {
      case ILNErrorCode.InvalidDiscountRate:
        console.error("Discount rate must be between 1 and 5000 bps");
        break;
      case ILNErrorCode.DueDateTooSoon:
        console.error("Due date must be at least 24 hours out");
        break;
      default:
        console.error(`ILN error (${e.code}): ${e.message}`);
    }
  } else {
    throw e; // network / RPC failure — retry with backoff
  }
}
```

A complete list of on-chain error codes is in
[docs/error-codes.md](error-codes.md).

---

## Testing Against Testnet

Fund a fresh account from Friendbot, then run any flow above:

```bash
# Create + fund a testnet account
curl "https://friendbot.stellar.org/?addr=$(stellar keys address my-key)"
```

```ts
import { Keypair } from "@stellar/stellar-sdk";

const kp = Keypair.random();
await fetch(`https://friendbot.stellar.org/?addr=${kp.publicKey()}`);
// kp is now funded on testnet and ready to sign ILN transactions.
```

The repository also ships runnable references you can copy from:

- [`scripts/smoke-test.ts`](../scripts/smoke-test.ts) — full submit → fund → pay
  cycle against testnet.
- [`scripts/seed.ts`](../scripts/seed.ts) — seed the deployment with sample data.
- [`sdk/tests`](../sdk/tests) — unit and integration tests for every method.

See the [SDK package README](../sdk/README.md) for the full method reference.

## Examples
See `sdk/examples/` for runnable integration scripts covering:
- [Invoice Lifecycle](../sdk/examples/invoice_lifecycle.ts)
- [Insurance Claims](../sdk/examples/insurance_claim.ts)
- [Governance Lifecycle](../sdk/examples/governance_lifecycle.ts)
