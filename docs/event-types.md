# ILN Smart Contract — Event Types Reference

This document describes every event emitted by the ILN Smart Contract. Events are available via the ILN SDK, the WebSocket real-time feed, and the REST indexer API.

---

## Event availability

| Symbol | Meaning |
|--------|---------|
| SDK    | Available via `ILNClient.watchEvents()` and `ILNClient.getEvents()` |
| WS     | Pushed over the WebSocket connection at `wss://api.iln.network/ws` |
| REST   | Queryable from the indexer at `GET /api/v1/events` |

All events below are available in **SDK · WS · REST** unless noted otherwise.

---

## Common envelope fields

Every event payload includes these top-level fields before the event-specific body:

| Field | Type | Description |
|-------|------|-------------|
| `type` | `string` | Event type identifier (see sections below) |
| `contractId` | `string` | Soroban contract address that emitted the event |
| `ledger` | `number` | Ledger sequence number in which the event was finalised |
| `ledgerClosedAt` | `string` (ISO 8601) | Timestamp of the ledger close |
| `txHash` | `string` | Transaction hash that contains this event |

---

## Invoice lifecycle events

### `InvoiceSubmitted`

Emitted when a seller submits a new invoice to the contract.

**Trigger:** Seller calls `submit_invoice()`.

| Field | Type | Description |
|-------|------|-------------|
| `invoiceId` | `string` | Unique invoice identifier (contract-assigned) |
| `sellerAddress` | `string` | Stellar G-address of the seller |
| `buyerAddress` | `string` | Stellar G-address of the buyer |
| `amountStroops` | `string` | Invoice face value in stroops (string to avoid JS BigInt loss) |
| `tokenCode` | `string` | Asset code, e.g. `"XLM"` or `"USDC"` |
| `dueDate` | `string` | ISO 8601 payment due date |

**Example:**
```json
{
  "type": "InvoiceSubmitted",
  "contractId": "CBIELTK6YBZJU5UP2WWQEQ4YPE6BBMC5CXJAWLS5YF4SZJED7B7BZAO",
  "ledger": 54321,
  "ledgerClosedAt": "2026-06-28T12:00:00Z",
  "txHash": "a1b2c3d4e5f6...",
  "invoiceId": "inv_01j4zx...",
  "sellerAddress": "GABCDEF...",
  "buyerAddress": "GHIJKLM...",
  "amountStroops": "100000000",
  "tokenCode": "USDC",
  "dueDate": "2026-07-28T00:00:00Z"
}
```

---

### `InvoiceFunded`

Emitted when a liquidity provider (LP) funds an invoice before the buyer pays.

**Trigger:** LP calls `fund_invoice()`.

| Field | Type | Description |
|-------|------|-------------|
| `invoiceId` | `string` | Invoice that was funded |
| `lpAddress` | `string` | G-address of the liquidity provider |
| `fundedAmountStroops` | `string` | Amount the LP transferred into escrow |
| `feeStroops` | `string` | Protocol fee deducted from the LP amount |

**Example:**
```json
{
  "type": "InvoiceFunded",
  "contractId": "CBIELTK6YBZJU5UP2WWQEQ4YPE6BBMC5CXJAWLS5YF4SZJED7B7BZAO",
  "ledger": 54400,
  "ledgerClosedAt": "2026-06-28T12:05:00Z",
  "txHash": "b2c3d4e5f6a1...",
  "invoiceId": "inv_01j4zx...",
  "lpAddress": "GNOPQRS...",
  "fundedAmountStroops": "99500000",
  "feeStroops": "500000"
}
```

---

### `InvoicePaid`

Emitted when the buyer transfers payment, settling the invoice.

**Trigger:** Buyer calls `pay_invoice()`.

| Field | Type | Description |
|-------|------|-------------|
| `invoiceId` | `string` | Settled invoice |
| `buyerAddress` | `string` | G-address of the paying buyer |
| `paidAmountStroops` | `string` | Exact amount paid by the buyer |
| `settledToAddress` | `string` | Address that received the funds (LP or seller) |

**Example:**
```json
{
  "type": "InvoicePaid",
  "contractId": "CBIELTK6YBZJU5UP2WWQEQ4YPE6BBMC5CXJAWLS5YF4SZJED7B7BZAO",
  "ledger": 54500,
  "ledgerClosedAt": "2026-06-28T12:10:00Z",
  "txHash": "c3d4e5f6a1b2...",
  "invoiceId": "inv_01j4zx...",
  "buyerAddress": "GHIJKLM...",
  "paidAmountStroops": "100000000",
  "settledToAddress": "GNOPQRS..."
}
```

---

### `InvoiceCancelled`

Emitted when a seller cancels an invoice that has not yet been funded.

**Trigger:** Seller calls `cancel_invoice()` while invoice is in `Pending` state.

| Field | Type | Description |
|-------|------|-------------|
| `invoiceId` | `string` | Cancelled invoice |
| `cancelledBy` | `string` | G-address of the cancelling party |
| `reason` | `string \| null` | Optional cancellation reason provided by the seller |

**Example:**
```json
{
  "type": "InvoiceCancelled",
  "contractId": "CBIELTK6YBZJU5UP2WWQEQ4YPE6BBMC5CXJAWLS5YF4SZJED7B7BZAO",
  "ledger": 54350,
  "ledgerClosedAt": "2026-06-28T12:03:00Z",
  "txHash": "d4e5f6a1b2c3...",
  "invoiceId": "inv_01j4zx...",
  "cancelledBy": "GABCDEF...",
  "reason": "Order revised by buyer"
}
```

---

### `InvoiceExpired`

Emitted by the contract automation when the due date passes and the invoice has not been paid or cancelled.

**Trigger:** Automated expiry check on the first ledger after `dueDate`.

| Field | Type | Description |
|-------|------|-------------|
| `invoiceId` | `string` | Expired invoice |
| `dueDate` | `string` | ISO 8601 timestamp of the due date |
| `refundedLpAddress` | `string \| null` | LP address if an LP had funded and was refunded |
| `refundedAmountStroops` | `string \| null` | Amount returned to LP on expiry, if applicable |

**Example:**
```json
{
  "type": "InvoiceExpired",
  "contractId": "CBIELTK6YBZJU5UP2WWQEQ4YPE6BBMC5CXJAWLS5YF4SZJED7B7BZAO",
  "ledger": 55000,
  "ledgerClosedAt": "2026-07-28T00:00:30Z",
  "txHash": "e5f6a1b2c3d4...",
  "invoiceId": "inv_01j4zx...",
  "dueDate": "2026-07-28T00:00:00Z",
  "refundedLpAddress": "GNOPQRS...",
  "refundedAmountStroops": "99500000"
}
```

---

### `InvoiceDisputed`

Emitted when either party raises a formal dispute on a funded invoice.

**Trigger:** Seller or buyer calls `dispute_invoice()`.

| Field | Type | Description |
|-------|------|-------------|
| `invoiceId` | `string` | Disputed invoice |
| `raisedBy` | `string` | G-address of the party raising the dispute |
| `disputeId` | `string` | Contract-assigned dispute identifier |
| `reason` | `string` | Required plain-text reason for the dispute |

**Example:**
```json
{
  "type": "InvoiceDisputed",
  "contractId": "CBIELTK6YBZJU5UP2WWQEQ4YPE6BBMC5CXJAWLS5YF4SZJED7B7BZAO",
  "ledger": 54600,
  "ledgerClosedAt": "2026-06-28T12:12:00Z",
  "txHash": "f6a1b2c3d4e5...",
  "invoiceId": "inv_01j4zx...",
  "raisedBy": "GABCDEF...",
  "disputeId": "disp_09zqw...",
  "reason": "Goods not delivered as described"
}
```

---

## Reputation events

### `ReputationUpdated`

Emitted whenever an address's on-chain reputation score changes.

**Trigger:** Automatic after `InvoicePaid`, `InvoiceExpired`, or dispute resolution.

| Field | Type | Description |
|-------|------|-------------|
| `address` | `string` | G-address whose reputation changed |
| `previousScore` | `number` | Score before this update |
| `newScore` | `number` | Score after this update |
| `delta` | `number` | Signed difference (`newScore - previousScore`) |
| `reason` | `string` | Machine-readable cause: `"invoice_paid"` \| `"invoice_expired"` \| `"dispute_resolved_against"` \| `"dispute_resolved_for"` |

**Example:**
```json
{
  "type": "ReputationUpdated",
  "contractId": "CBIELTK6YBZJU5UP2WWQEQ4YPE6BBMC5CXJAWLS5YF4SZJED7B7BZAO",
  "ledger": 54501,
  "ledgerClosedAt": "2026-06-28T12:10:05Z",
  "txHash": "c3d4e5f6a1b2...",
  "address": "GHIJKLM...",
  "previousScore": 82,
  "newScore": 84,
  "delta": 2,
  "reason": "invoice_paid"
}
```

---

## Contract administration events

### `ContractPaused`

Emitted when an admin pauses the contract, halting all state-changing operations.

**Trigger:** Admin calls `pause()`.

| Field | Type | Description |
|-------|------|-------------|
| `pausedBy` | `string` | G-address of the admin who triggered the pause |
| `reason` | `string \| null` | Optional human-readable reason |

**Example:**
```json
{
  "type": "ContractPaused",
  "contractId": "CBIELTK6YBZJU5UP2WWQEQ4YPE6BBMC5CXJAWLS5YF4SZJED7B7BZAO",
  "ledger": 60000,
  "ledgerClosedAt": "2026-06-30T08:00:00Z",
  "txHash": "a1c2e3f4b5d6...",
  "pausedBy": "GADMIN1...",
  "reason": "Emergency maintenance"
}
```

---

### `ContractUnpaused`

Emitted when an admin lifts a pause, re-enabling the contract.

**Trigger:** Admin calls `unpause()` while contract is paused.

| Field | Type | Description |
|-------|------|-------------|
| `unpausedBy` | `string` | G-address of the admin who lifted the pause |

**Example:**
```json
{
  "type": "ContractUnpaused",
  "contractId": "CBIELTK6YBZJU5UP2WWQEQ4YPE6BBMC5CXJAWLS5YF4SZJED7B7BZAO",
  "ledger": 60100,
  "ledgerClosedAt": "2026-06-30T08:30:00Z",
  "txHash": "b2d3f4a5c6e7...",
  "unpausedBy": "GADMIN1..."
}
```

---

## Token registry events

### `TokenAdded`

Emitted when an admin adds a new supported payment token.

**Trigger:** Admin calls `add_token()`.

| Field | Type | Description |
|-------|------|-------------|
| `tokenCode` | `string` | Asset code (e.g. `"USDC"`) |
| `issuerAddress` | `string` | G-address of the token issuer |
| `addedBy` | `string` | G-address of the admin who added the token |

**Example:**
```json
{
  "type": "TokenAdded",
  "contractId": "CBIELTK6YBZJU5UP2WWQEQ4YPE6BBMC5CXJAWLS5YF4SZJED7B7BZAO",
  "ledger": 50000,
  "ledgerClosedAt": "2026-06-01T09:00:00Z",
  "txHash": "c3e4f5a6b7c8...",
  "tokenCode": "USDC",
  "issuerAddress": "GA5ZSJ...",
  "addedBy": "GADMIN1..."
}
```

---

### `TokenRemoved`

Emitted when an admin removes a token from the supported list.

**Trigger:** Admin calls `remove_token()`.

| Field | Type | Description |
|-------|------|-------------|
| `tokenCode` | `string` | Asset code that was removed |
| `issuerAddress` | `string` | G-address of the removed token's issuer |
| `removedBy` | `string` | G-address of the admin who removed the token |

**Example:**
```json
{
  "type": "TokenRemoved",
  "contractId": "CBIELTK6YBZJU5UP2WWQEQ4YPE6BBMC5CXJAWLS5YF4SZJED7B7BZAO",
  "ledger": 51000,
  "ledgerClosedAt": "2026-06-05T10:00:00Z",
  "txHash": "d4f5a6b7c8d9...",
  "tokenCode": "USDC",
  "issuerAddress": "GA5ZSJ...",
  "removedBy": "GADMIN1..."
}
```

---

## Liquidity provider events

### `LPPositionTransferred`

Emitted when a liquidity provider transfers their funded position in an invoice to another address.

**Trigger:** LP calls `transfer_position()`.

| Field | Type | Description |
|-------|------|-------------|
| `invoiceId` | `string` | Invoice whose LP position was transferred |
| `fromAddress` | `string` | Original LP G-address |
| `toAddress` | `string` | New LP G-address |
| `positionAmountStroops` | `string` | Value of the transferred position in stroops |

**Example:**
```json
{
  "type": "LPPositionTransferred",
  "contractId": "CBIELTK6YBZJU5UP2WWQEQ4YPE6BBMC5CXJAWLS5YF4SZJED7B7BZAO",
  "ledger": 54450,
  "ledgerClosedAt": "2026-06-28T12:07:30Z",
  "txHash": "e5f6a7b8c9d0...",
  "invoiceId": "inv_01j4zx...",
  "fromAddress": "GNOPQRS...",
  "toAddress": "GTUVWXY...",
  "positionAmountStroops": "99500000"
}
```

---

## Insurance pool events

### `Enrolled`

Emitted when a liquidity provider enrolls in the default-protection insurance pool.

**Trigger:** LP calls `enroll()` on the insurance pool contract.

| Field | Type | Description |
|-------|------|-------------|
| `lpAddress` | `string` | G-address of the LP who enrolled |

**Example:**
```json
{
  "type": "Enrolled",
  "contractId": "CAINSURANCE...",
  "ledger": 54300,
  "ledgerClosedAt": "2026-06-28T12:00:00Z",
  "txHash": "a1b2c3d4e5f6...",
  "lpAddress": "GNOPQRS..."
}
```

---

### `Premium`

Emitted when a liquidity provider deposits a premium payment into the insurance pool. Auto-enrolls the LP on first payment.

**Trigger:** LP calls `deposit_premium()` on the insurance pool contract.

| Field | Type | Description |
|-------|------|-------------|
| `lpAddress` | `string` | G-address of the LP paying the premium |
| `amountStroops` | `string` | Premium amount deposited in stroops |

**Example:**
```json
{
  "type": "Premium",
  "contractId": "CAINSURANCE...",
  "ledger": 54310,
  "ledgerClosedAt": "2026-06-28T12:02:00Z",
  "txHash": "b2c3d4e5f6a1...",
  "lpAddress": "GNOPQRS...",
  "amountStroops": "1000000"
}
```

---

### `Claimed`

Emitted when the pool processes an insurance claim for a defaulted invoice, compensating the LP from the accumulated premium balance (up to the coverage cap).

**Trigger:** Admin (in production, the invoice_liquidity contract) calls `claim()` on the insurance pool contract.

| Field | Type | Description |
|-------|------|-------------|
| `invoiceId` | `string` | ID of the defaulted invoice |
| `payoutStroops` | `string` | Compensation payout amount in stroops (≤ coverage cap, ≤ pool balance) |

**Example:**
```json
{
  "type": "Claimed",
  "contractId": "CAINSURANCE...",
  "ledger": 54500,
  "ledgerClosedAt": "2026-06-28T12:15:00Z",
  "txHash": "c3d4e5f6a1b2...",
  "invoiceId": "inv_01j4zx...",
  "payoutStroops": "500000"
}
```

---

### `SolvencyCircuitTripped`

Emitted when a claim payout leaves the pool's reserve ratio at or below the governance-configured minimum (`MinReserveRatioBps`), automatically tripping the solvency circuit breaker and pausing new claim payouts (Issue #826).

**Trigger:** A claim on the insurance pool pays out its final bound payout and the resulting reserve ratio (`(balance + backstop) / coverage * 10_000`) is at or below the threshold. The tripping claim completes first because the sticky flag write must land on an invocation that returns `Ok` (in Soroban, a write followed by a panic in the same invocation is rolled back). Once tripped, payouts stay paused until governance calls `reset_solvency_circuit()`. Enrollments and premium deposits continue unaffected.

| Field | Type | Description |
|-------|------|-------------|
| `ratioBps` | `string` | Reserve ratio (bps) at the time of the trip (`(balance + backstop) / coverage * 10_000`) |
| `reserveStroops` | `string` | Total claimable reserve (liquid balance + backstop) observed at the trip |

**Example:**
```json
{
  "type": "SolvencyCircuitTripped",
  "contractId": "CAINSURANCE...",
  "ledger": 54600,
  "ledgerClosedAt": "2026-06-28T13:00:00Z",
  "txHash": "d4e5f6a1b2c3...",
  "ratioBps": "300",
  "reserveStroops": "300000"
}
```

---

### `SolvencyCircuitReset`

Emitted when governance resumes claim payouts after a solvency circuit trip (Issue #826).

**Trigger:** Admin calls `reset_solvency_circuit()` on the insurance pool contract.

| Field | Type | Description |
|-------|------|-------------|
| `ratioBps` | `string` | Reserve ratio (bps) at the moment of the reset |
| `reserveStroops` | `string` | Total claimable reserve (liquid balance + backstop) at the reset |

---

### `BackstopTopUp`

Emitted when capital is added to the protocol's backstop fund — either a governance-led top-up or the automatic share of a premium deposit (when `BackstopFundingBps > 0`) (Issue #827).

**Trigger:** Admin calls `top_up_backstop()`, or an LP calls `deposit_premium()` with backstop funding enabled.

| Field | Type | Description |
|-------|------|-------------|
| `from` | `string` | G-address of the funding source |
| `amountStroops` | `string` | Amount credited to the backstop in stroops |
| `backstopBalanceStroops` | `string` | New backstop balance after the credit |

---

### `ClaimEvidence`

Emitted when an evidence hash is attached to a claim's invoice (Issue #828).

**Trigger:** Admin calls `submit_claim_evidence()` on the insurance pool contract.

| Field | Type | Description |
|-------|------|-------------|
| `invoiceId` | `string` | ID of the defaulted invoice the evidence backs |
| `evidenceHash` | `string` | 32-byte digest of the off-chain evidence (e.g. IPFS CID / document hash) |
| `submittedAt` | `string` | Ledger timestamp of submission |

---

### `PairDefaultRecorded`

Emitted when a confirmed default is recorded for a specific (LP, payer) pair, surfacing data for the off-chain collusion detector (Issue #829).

**Trigger:** Admin calls `record_pair_default()` on the insurance pool contract.

| Field | Type | Description |
|-------|------|-------------|
| `lp` | `string` | G-address of the liquidity provider |
| `payer` | `string` | G-address of the defaulting payer |
| `pairCount` | `string` | Running default count for this (LP, payer) pair |

---

## Governance events

### `AdminChanged`

Emitted when the contract admin role is transferred.

**Trigger:** Current admin calls `transfer_admin()`.

| Field | Type | Description |
|-------|------|-------------|
| `previousAdmin` | `string` | G-address of the outgoing admin |
| `newAdmin` | `string` | G-address of the incoming admin |

**Example:**
```json
{
  "type": "AdminChanged",
  "contractId": "CBIELTK6YBZJU5UP2WWQEQ4YPE6BBMC5CXJAWLS5YF4SZJED7B7BZAO",
  "ledger": 48000,
  "ledgerClosedAt": "2026-05-15T14:00:00Z",
  "txHash": "f6a7b8c9d0e1...",
  "previousAdmin": "GADMIN1...",
  "newAdmin": "GADMIN2..."
}
```

---

### `ParameterUpdated`

Emitted when an admin changes a contract configuration parameter.

**Trigger:** Admin calls `update_parameter()`.

| Field | Type | Description |
|-------|------|-------------|
| `parameter` | `string` | Machine-readable parameter name (e.g. `"fee_basis_points"`, `"max_invoice_ttl_days"`) |
| `previousValue` | `string` | Previous value serialised as a string |
| `newValue` | `string` | New value serialised as a string |
| `updatedBy` | `string` | G-address of the admin making the change |

**Example:**
```json
{
  "type": "ParameterUpdated",
  "contractId": "CBIELTK6YBZJU5UP2WWQEQ4YPE6BBMC5CXJAWLS5YF4SZJED7B7BZAO",
  "ledger": 49000,
  "ledgerClosedAt": "2026-05-20T10:00:00Z",
  "txHash": "a7b8c9d0e1f2...",
  "parameter": "fee_basis_points",
  "previousValue": "50",
  "newValue": "40",
  "updatedBy": "GADMIN2..."
}
```

---

## TWAP Oracle events

### `TwapEnabledForFeed`

Emitted when TWAP is enabled or disabled for a specific feed type.

| Field | Type | Description |
|-------|------|-------------|
| `feedType` | `string` | Oracle feed type |
| `enabled` | `boolean` | True if TWAP is enabled |

### `TwapWindowUpdated`

Emitted when the TWAP window size is updated.

| Field | Type | Description |
|-------|------|-------------|
| `oldWindow` | `string` | Previous TWAP window in ledgers |
| `newWindow` | `string` | New TWAP window in ledgers |

### `TwapInsufficientData`

Emitted when a TWAP read fails due to insufficient observations.

| Field | Type | Description |
|-------|------|-------------|
| `feedType` | `string` | Oracle feed type queried |
| `token` | `string` | Token queried |
| `observations` | `number` | Number of observations found |
| `minRequired` | `number` | Minimum observations required |

---

## Quick-reference table

| Event | Category | SDK | WS | REST |
|-------|----------|-----|----|------|
| `InvoiceSubmitted` | Invoice | ✓ | ✓ | ✓ |
| `InvoiceFunded` | Invoice | ✓ | ✓ | ✓ |
| `InvoicePaid` | Invoice | ✓ | ✓ | ✓ |
| `InvoiceCancelled` | Invoice | ✓ | ✓ | ✓ |
| `InvoiceExpired` | Invoice | ✓ | ✓ | ✓ |
| `InvoiceDisputed` | Invoice | ✓ | ✓ | ✓ |
| `ReputationUpdated` | Reputation | ✓ | ✓ | ✓ |
| `ContractPaused` | Admin | ✓ | ✓ | ✓ |
| `ContractUnpaused` | Admin | ✓ | ✓ | ✓ |
| `TokenAdded` | Token | ✓ | ✓ | ✓ |
| `TokenRemoved` | Token | ✓ | ✓ | ✓ |
| `LPPositionTransferred` | LP | ✓ | ✓ | ✓ |
| `Enrolled` | Insurance | ✓ | ✓ | ✓ |
| `Premium` | Insurance | ✓ | ✓ | ✓ |
| `Claimed` | Insurance | ✓ | ✓ | ✓ |
| `SolvencyCircuitTripped` | Insurance | ✓ | ✓ | ✓ |
| `SolvencyCircuitReset` | Insurance | ✓ | ✓ | ✓ |
| `BackstopTopUp` | Insurance | ✓ | ✓ | ✓ |
| `ClaimEvidence` | Insurance | ✓ | ✓ | ✓ |
| `PairDefaultRecorded` | Insurance | ✓ | ✓ | ✓ |
| `AdminChanged` | Governance | ✓ | ✓ | ✓ |
| `ParameterUpdated` | Governance | ✓ | ✓ | ✓ |
| `TwapEnabledForFeed` | Oracle | ✓ | ✓ | ✓ |
| `TwapWindowUpdated` | Oracle | ✓ | ✓ | ✓ |
| `TwapInsufficientData` | Oracle | ✓ | ✓ | ✓ |
