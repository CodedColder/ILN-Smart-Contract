# Multi-Token Integration Guide

Complete reference for submitting, funding, and settling invoices in USDC, EURC, and XLM on the Invoice Liquidity Network.

---

## Token Reference

| Token | Symbol | Decimals | Min Amount | Testnet Address | Mainnet Address |
|-------|--------|----------|------------|-----------------|-----------------|
| USD Coin | USDC | 6 | 0.01 USDC (10,000 stroops) | `CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIUZQFKUUL` | `CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75` |
| Euro Coin | EURC | 6 | 0.01 EURC (10,000 stroops) | `CAQCFVLOBK5GIULPNZVXBPDRDZSS5DN3LRGFGMFQRVXWWF2JNBTJWWGB` | `CDTKPWPLOURQA3WMLYKR3IMAXC5T3EYBWS7GWWLBZ5Z6GS4XRTXTZVYX` |
| Stellar Lumens | XLM | 7 | 0.0000001 XLM (1 stroop) | Native (no contract) | Native (no contract) |

> **Key difference**: USDC and EURC use **6 decimal places**. XLM uses **7 decimal places**. Always convert human-readable amounts to stroops before calling the contract.

---

## Decimal Precision Guide

### Converting amounts

```
stroops = human_amount × 10^decimals
```

### Worked examples

**USDC (6 decimals)**:
- 100.00 USDC = 100 × 10^6 = 100,000,000 stroops
- 0.01 USDC = 0.01 × 10^6 = 10,000 stroops (minimum)
- 1,500.50 USDC = 1,500.50 × 10^6 = 1,500,500,000 stroops

**XLM (7 decimals)**:
- 100.00 XLM = 100 × 10^7 = 1,000,000,000 stroops
- 0.0000001 XLM = 1 stroop (minimum)
- 1,500.50 XLM = 1,500.50 × 10^7 = 15,005,000,000 stroops

**Common mistake**: Using 6 decimals for XLM (off by 10×).

### SDK helper

```typescript
import { formatTokenAmount, parseTokenAmount } from "@iln/sdk";

// Parse human-readable to stroops
const usdcStroops = parseTokenAmount("100.00", "USDC"); // 100000000n
const xlmStroops = parseTokenAmount("100.00", "XLM");   // 1000000000n

// Format stroops to human-readable
formatTokenAmount(100000000n, "USDC"); // "100.00"
formatTokenAmount(1000000000n, "XLM"); // "100.00"
```

---

## Submitting an Invoice

### SDK (TypeScript)

```typescript
import { ILNClient, Token } from "@iln/sdk";

const client = new ILNClient({ network: "testnet" });

// Submit invoice in USDC
const invoice = await client.submitInvoice({
  freelancer: freelancerKeypair,
  payer: payerAddress,
  amount: parseTokenAmount("500.00", "USDC"),
  token: Token.USDC,
  dueDate: Math.floor(Date.now() / 1000) + 86400 * 30, // 30 days
  discountRate: 500, // 5% annualized
});

console.log(`Invoice ${invoice.id} created for 500.00 USDC`);
```

### CLI (Stellar CLI)

```bash
# USDC invoice
soroban contract invoke \
  --id $INVOICE_LIQUIDITY_ID \
  --network testnet \
  --source $FREELANCER_SECRET \
  -- submit_invoice \
  --freelancer $FREELANCER_ADDRESS \
  --payer $PAYER_ADDRESS \
  --amount 500000000 \
  --token $USDC_TESTNET_ADDRESS \
  --due_date 1735689600 \
  --discount_rate 500

# XLM invoice (note: 7 decimals, native token uses SAC address)
soroban contract invoke \
  --id $INVOICE_LIQUIDITY_ID \
  --network testnet \
  --source $FREELANCER_SECRET \
  -- submit_invoice \
  --freelancer $FREELANCER_ADDRESS \
  --payer $PAYER_ADDRESS \
  --amount 1000000000 \
  --token $NATIVE_XLM_SAC \
  --due_date 1735689600 \
  --discount_rate 500
```

---

## Funding an Invoice

LPs fund invoices in the same token the invoice was created with.

```typescript
// Fund an invoice (token must match invoice's token)
const funded = await client.fundInvoice({
  lp: lpKeypair,
  invoiceId: invoice.id,
  amount: parseTokenAmount("475.00", "USDC"), // discounted amount
});
```

---

## Event Payloads

The contract emits events for each multi-token operation. Indexers should parse these for token-specific tracking.

### InvoiceCreated

```
Topics: ["invoice_created", invoice_id]
Data: { freelancer, payer, amount, token, due_date, discount_rate }
```

### InvoiceFunded

```
Topics: ["invoice_funded", invoice_id]
Data: { lp_address, amount, token, funded_at }
```

### InvoiceSettled

```
Topics: ["invoice_settled", invoice_id]
Data: { payer, lp, amount, token, settled_at }
```

---

## Error Codes

| Code | Error | Cause | Fix |
|------|-------|-------|-----|
| 100 | `InvalidToken` | Token address not in supported list | Use a supported token address |
| 101 | `TokenMismatch` | Payment token differs from invoice token | Pay using the same token as the invoice |
| 102 | `AmountBelowMinimum` | Amount less than token's minimum | Increase amount above minimum threshold |
| 103 | `DecimalPrecisionError` | Amount has wrong decimal precision | Check token decimals (6 for USDC/EURC, 7 for XLM) |
| 104 | `TokenNotWhitelisted` | Token not approved via governance | Submit governance proposal to add token |

---

## Token Registry (SDK)

The SDK's `TokenRegistry` provides token metadata and formatting helpers.

```typescript
import { TokenRegistry } from "@iln/sdk";

const registry = new TokenRegistry("testnet");

// Get token info
const usdc = registry.getToken("USDC");
console.log(usdc.decimals);      // 6
console.log(usdc.minAmount);     // 10000n (0.01 USDC)
console.log(usdc.contractAddress); // "CBIELTK..."

// List all supported tokens
const tokens = registry.listTokens();
// [{ symbol: "USDC", decimals: 6, ... }, { symbol: "EURC", ... }, { symbol: "XLM", ... }]

// Validate an amount
registry.validateAmount("USDC", parseTokenAmount("0.001", "USDC"));
// throws AmountBelowMinimumError

// Format for display
registry.format("USDC", 500000000n); // "500.00"
registry.format("XLM", 5000000000n); // "500.00"
```

---

## On-Chain Token Admission Checks

`add_token(token, decimals)` enforces the following (Issues #1070, #1073):

- The token must implement `decimals()`, and `decimals` must equal what it
  reports (at most 18). Classic-asset SACs report **7**.
- The token must expose the SAC `authorized()` interface, and both the ILN
  contract and the admin must be authorized holders.
- The fee-on-transfer probe (1,000,000 units from the admin) must round-trip
  exactly.

Frozen accounts and issuer clawback are handled at runtime by escrowing
payouts and by typed errors. See [token-safety.md](token-safety.md) for the
operator runbook.

## Adding a New Token via Governance

New tokens require a governance proposal and community vote.

### Steps

1. **Submit proposal** — call `iln_governance.submit_proposal` with:
   - Proposal type: `AddToken`
   - Parameters: `{ code: "NEW_TOKEN", issuer: "G...", min_amount: 10000, decimals: 7 }`

2. **Vote period** — LPs vote during the voting window (default: 7 days)

3. **Execution** — if passed, the contract automatically adds the token to the registry

4. **Verify** — call `get_supported_tokens` to confirm the new token appears

See [governance.md](./governance.md) for full governance documentation.

---

## Storage Layout

Token registry data is stored in the contract's persistent storage:

| Key | Type | Description |
|-----|------|-------------|
| `TokenRegistry` | `Map<Address, TokenInfo>` | All supported tokens and metadata |
| `SupportedTokens` | `Vec<Address>` | Ordered list of supported token addresses |
| `TokenDecimals` | `Map<Address, u32>` | Decimal precision per token |

---

## Testing Multi-Token Scenarios

```typescript
// tests/multiToken.test.ts
import { ILNClient, Token, parseTokenAmount } from "@iln/sdk";

describe("multi-token invoices", () => {
  it("creates invoice in each supported token", async () => {
    for (const token of [Token.USDC, Token.EURC, Token.XLM]) {
      const invoice = await client.submitInvoice({
        freelancer: freelancerKeypair,
        payer: payerAddress,
        amount: parseTokenAmount("100.00", token),
        token,
        dueDate: futureTimestamp,
        discountRate: 500,
      });
      expect(invoice.token).toBe(token);
    }
  });

  it("rejects cross-token payment", async () => {
    const invoice = await createInvoice(Token.USDC);
    await expect(
      client.fundInvoice({ lp: lpKeypair, invoiceId: invoice.id, amount: 100n, token: Token.EURC })
    ).rejects.toThrow("TokenMismatch");
  });

  it("handles decimal precision correctly", async () => {
    const usdc = parseTokenAmount("100.00", "USDC"); // 100000000n
    const xlm = parseTokenAmount("100.00", "XLM");   // 1000000000n
    expect(usdc.toString()).toBe("100000000");
    expect(xlm.toString()).toBe("1000000000");
  });
});
```

---

## FAQ

**Can I pay a USDC invoice in EURC?**
No. Each invoice is locked to a single token at creation.

**Can I change the token after invoice creation?**
No. Create a new invoice with the desired token.

**Why not auto-convert tokens?**
Auto-conversion introduces oracle dependency risk, inconsistent pricing, and added contract complexity. The system prioritizes deterministic settlement.

**What happens if I send the wrong token?**
The transaction will fail with `TokenMismatch` (error 101). Your funds are not lost — the transaction simply does not execute.

**How do I get testnet USDC/EURC?**
Use the Stellar Laboratory or the official Circle testnet faucet to mint test tokens.
