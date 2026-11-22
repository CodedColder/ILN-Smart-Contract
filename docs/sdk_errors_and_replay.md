# ILN SDK Error Handling & Event Replay

This document details the refined error handling layer and the historical event replay/catch-up system added to the Invoice Liquidity Network (ILN) TypeScript SDK.

---

## 1. SDK Error Handling

The SDK maps low-level Soroban contract simulation and transaction execution errors into a structured hierarchy of typed classes, allowing applications to catch specific classes of errors and respond dynamically.

### Error Hierarchy

All SDK errors inherit from the base `ILNError` class:

*   **`ILNError`** (extends `Error`)
    *   **`ValidationError`** — Raised when input arguments or values violate validation bounds.
        *   `InvoiceNotFound`
        *   `InvalidAmount`
        *   `InvalidDiscountRate`
        *   `InvalidDueDate`
        *   `DueDateTooSoon`
        *   `DueDateTooFar`
        *   `SelfInvoice`
        *   `AmountTooSmall`
        *   `MaxInvoiceAmountExceeded`
        *   `InvalidTwapWindow`
        *   `InvalidAddress` *(client-side only, no on-chain code)*
        *   `BatchTooLarge`
    *   **`AuthorizationError`** — Raised when the caller does not have sufficient permissions.
        *   `Unauthorized`
        *   `NotApprovedFunder`
        *   `PayerUnverified`
    *   **`InvoiceStateError`** — Raised when an invalid state transition or action is performed.
        *   `AlreadyFunded`
        *   `AlreadyPaid`
        *   `NotFunded`
        *   `InvoiceDefaulted`
        *   `NothingToClaim`
        *   `NotYetDefaulted`
        *   `OverfundingRejected`
        *   `InvoiceExpired`
        *   `AlreadyCancelled`
        *   `AlreadyInitialized`
        *   `AlreadyAppealed`
        *   `AppealWindowClosed`
        *   `NotDefaulted`
        *   `AlreadyInQueue`
        *   `InvoiceAppealed`
        *   `AlreadyDisputed`
        *   `NotDisputed`
        *   `InvoiceDisputed`
        *   `OverpaymentRejected`
        *   `PayerReputationTooLow`
        *   `InvoiceNotCancellable` *(client-side only, no on-chain code)*
        *   `QueueFull`
    *   **`ContractExecutionError`** — Raised during contract execution failures.
        *   `ContractPaused`
        *   `ArithmeticOverflow`
        *   `FeeOnTransferToken`
        *   `OracleDataStale`
        *   `Reentrancy`
        *   `RateLimited` *(retry recommended)*
        *   `QueueNotMature` *(retry recommended)*
        *   `QueueNotStalled` *(retry recommended)*
        *   `IncompatibleInterfaceVersion`
        *   `OracleCircuitOpen`
        *   `NoPriceSource`
        *   `AllPriceSourcesRejected`
        *   `GlobalVolumeCapExceeded`
        *   `InvalidTransfer` *(client-side only, no on-chain code)*
        *   `InsufficientAmount` *(client-side only, sentinel code 999)*
    *   **`NetworkError`** — Raised during transient network, RPC connectivity, or connection abort failures.

### Contract error code mapping

`ILNError.fromError` extracts the code from `Error(Contract, #N)` (the format Soroban emits)
or the legacy `Error(Contract, N)` and maps it by the table in `contracts/invoice_liquidity/src/errors.rs`
(codes 1-48; `errors.test.ts` asserts every variant). Behaviour for the remaining cases:

*   **Unknown code** (contract newer than the SDK): a `ContractExecutionError` whose `originalCode` is the
    code and whose message tells the caller to upgrade `@iln/sdk`. It is never silently downgraded to a
    code-less error.
*   **Not a contract error** (and not transient): a base `ILNError` carrying the original message.
*   **Other contracts** (`reputation_bonus`, `iln_distribution`, ... ) have their own `ContractError`
    numbering; their codes are **not** interpreted by this mapping and a clash such as `Error(Contract, #2)`
    from another contract would be mislabelled. Use `context.contractId` to tell them apart.
*   Client-side-only errors (`InvalidAddress`, `InvalidTransfer`, `InvoiceNotCancellable`) are thrown by
    SDK pre-checks and have `code === undefined`; before this audit they reused on-chain codes 37-39 by mistake.

### Context Metadata

Every error instance exposes the following diagnostic properties where available:

*   `txHash?: string` — The transaction hash in which the error occurred.
*   `ledger?: number` — The ledger sequence number.
*   `contractId?: string` — The target contract ID.
*   `originalCode?: number` — The original integer error code returned from the contract.

### Retry Guidance

Transient errors expose a `recommendRetry` boolean flag:

*   **`recommendRetry: true`** is returned for transient errors, such as:
    *   Network timeouts / DNS failures
    *   Rate limiting / HTTP 429
    *   Temporary server unavailability / HTTP 503
    *   Ledger synchronization delays
*   **`recommendRetry: false`** is returned for permanent failures (e.g. invalid arguments, unauthorized calls).

---

## 2. Event Replay & Gap Recovery

To maintain high data consistency and reliability for client applications, indexing services, and dashboards, the SDK supports historical event replay and automatic ledger gap recovery.

### Replaying Historical Events

The `replay` function allows querying contract events backwards or forwards from a specific ledger sequence:

```typescript
import { replay } from "@iln/sdk";

const finalCursor = await replay(
  horizon,
  CONTRACT_ID,
  { fromLedger: 1234500 }, // event filters
  1234500, // starting ledger
  (event) => {
    console.log("Replayed historical event:", event);
  }
);
```

### Event Subscription with Replay

Pass the `fromLedger` property inside the filter parameter to `subscribe` to seamlessly replay history before starting the live SSE event stream:

```typescript
import { subscribe } from "@iln/sdk";

const unsubscribe = subscribe(
  horizon,
  CONTRACT_ID,
  { fromLedger: 1234500, types: ["funded"] },
  (event) => {
    console.log("Processed event:", event);
  },
  (err) => {
    console.error("Subscription stream error:", err);
  }
);

// Stop the subscription later
unsubscribe();
```

### Ledger Gap Recovery & Deduplication

During real-time streaming, network interruptions, indexer restarts, or connection lag can lead to missing events. The `subscribe` logic actively guards against this:

1.  **Gap Detection**: Whenever a new live event is received, the subscriber checks if its ledger sequence is greater than the next expected ledger sequence (`event.ledger > lastProcessedLedger + 1`).
2.  **Recovery**: If a gap is detected, the subscriber triggers an asynchronous `replay` starting from the missing ledger sequence up to the current event.
3.  **Deduplication**: A slide-capped cache of processed paging tokens is maintained. Replayed events that have already been processed (or duplicates received from the SSE stream) are automatically ignored.

### New Batch Errors (Distribution, TWAP, Insurance)
- `DistributionError`: **Terminal**. Indicates a fatal math or rounding error during yield/reward distribution. Do not retry; manual intervention required to balance pools.
- `TwapStaleOracleError`: **Retryable**. The oracle hasn't been updated recently enough. Retry after the next oracle heartbeat (typically 5-10 minutes).
- `TwapPriceDeviationError`: **Terminal**. The queried price deviates beyond the maximum allowed spread for the TWAP window.
- `InsuranceInsufficientPremium`: **Terminal**. The provided premium does not meet the dynamically calculated rate.
- `InsuranceClaimDenied`: **Terminal**. The automated condition for the claim was not met.

*(Note: These replace several older paths that previously panicked. The SDK now safely decodes these into strongly-typed Error objects).*
