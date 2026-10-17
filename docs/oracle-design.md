# Oracle Design — Payer Verification

## Overview

The Invoice Liquidity Network (ILN) supports an optional off-chain payer verification oracle. The oracle surfaces KYC or creditworthiness data gathered off-chain into the smart contract, allowing invoice funding to be gated on verified payers.

The oracle is entirely optional. When no oracle address is registered, all payer-verification checks pass (fail-open). This preserves backwards compatibility and lets the system operate without an oracle in development or permissive deployments.

---

## Oracle Data Format

Each payer verification record carries two fields:

| Field | Type | Description |
|---|---|---|
| `verified` | `bool` | Whether the payer has been verified by the oracle operator |
| `timestamp` | `u64` | Unix epoch seconds when the oracle last updated this entry |

The ILN contract reads these fields via a single cross-contract call per query. The oracle contract is responsible for storing and updating them per-address.

---

## Update Mechanism

The oracle follows a **pull model**:

1. The oracle operator calls `update_verification(payer, verified)` on the oracle contract off-chain at any time (e.g., after completing a KYC check).
2. When the ILN contract needs to verify a payer, it calls `oracle.get_verification(payer)` and reads the current record.
3. There is no push or callback mechanism — the ILN contract reads on demand.

This design keeps the oracle stateless from ILN's perspective and avoids the complexity of push-based oracle patterns.

---

## Trust Model

- A single oracle address is stored in `Config.price_oracle` (shared with the price oracle slot for MVP simplicity), or, since Issue #532, resolved through the governance-controlled per-feed-type/per-token registry in `oracle_registry.rs` (see [ADR-010](adr/ADR-010-oracle-registry.md)).
- The oracle address is set by the ILN admin via `set_price_oracle(oracle_address)` (or `register_oracle`/`register_token_oracle`) in a separate transaction after `initialize()`.
- The oracle contract is fully trusted once registered — ILN does not validate oracle operator identity beyond the on-chain address, **unless** multiple price sources are registered for a feed type, in which case cross-source deviation checking applies — see [Multi-Source Price Deviation Checking](#multi-source-price-deviation-checking-price-feed) below.
- Full N-of-M consensus quorum (require agreement before accepting *any* value, as opposed to just filtering out disagreeing outliers) remains future work beyond what's described below.

---

## Failure Modes

| Scenario | ILN Behaviour |
|---|---|
| No oracle registered | Permissive — verification always passes |
| Oracle reports `verified = false` | Check fails; payer cannot proceed |
| Oracle data is stale (age > 7 days) | Treated as unverified; check fails |
| Oracle contract panics / traps | Treated as unverified (caller catches trap) |
| Oracle contract address is wrong / undeployed | Cross-contract call panics; ILN treats as unverified |

---

## Staleness Threshold

The default staleness threshold is **7 days** (604 800 seconds), defined as `ORACLE_STALENESS_THRESHOLD_SECS` in `oracle_interface.rs`.

Staleness is computed as:

```
now_seconds - oracle_timestamp > ORACLE_STALENESS_THRESHOLD_SECS
```

where `now_seconds` comes from `env.ledger().timestamp()`.

Oracle operators **must** refresh verification records at least every 7 days to keep payers active. The threshold can be tightened by changing the constant in a future contract upgrade if stricter freshness is required.

---

## Multi-Source Price Deviation Checking (Price Feed)

The single-oracle model above (one resolved address per feed type/token) has no defense against a single misbehaving or compromised oracle reporting a wildly incorrect **price** — there's nothing on-chain to compare it against. `oracle_registry.rs` addresses this with a separate, optional multi-source registration list specifically for numeric price data:

- `add_price_source(feed_type, oracle)` / `remove_price_source(feed_type, oracle)` (admin-gated) maintain a list of price-reporting sources per feed type, independent of the single-oracle `register_oracle`/`register_token_oracle` registry used for boolean payer verification.
- `get_verified_price(feed_type, token)` queries every registered source (via `get_price(token) -> i128`, dynamically invoked — a single non-responding or panicking source is excluded from the sample rather than failing the whole query) and:
  - **Zero sources, or all queries failed:** returns an error (`NoPriceSource`).
  - **Exactly one source:** returns its price **unchecked**.
  - **Two or more sources:** computes the median of all successfully-queried prices, excludes any source deviating from that median beyond `get_max_price_deviation_bps()` (emitting `PriceOutlierRejected` per exclusion), and returns the median of the survivors — or `AllPriceSourcesRejected` if nothing survives.
- The deviation threshold is governance-configurable via `set_max_price_deviation_bps` (default 500 bps / 5%), the same admin-gated, governance-controlled-in-production pattern used throughout this registry.

### The single-source risk is explicit, not silent

**If only one price source is registered for a feed type, there is no deviation protection, by construction — a lone source's report is trusted outright.** This is not an oversight or a gap to be closed later in this module; it is an accepted, unavoidable limitation of running a single-source price feed, identical in kind to the single-oracle payer-verification model's own single-point-of-failure risk (see [oracle-attack-economics.md](oracle-attack-economics.md) for that model's cost/benefit analysis, which compounds with this one). Deploying at least two independent, non-colluding price sources is what actually activates this section's protection. Governance and integrators should treat "only one price source registered" as a known, live risk to actively remediate (by registering additional sources), not a configuration that happens to work fine.

A closely related, less obvious limitation: **with exactly two sources, a single outlier cannot be selectively identified.** The median of two values is their average, which sits equidistant from both by construction — an honest source and a lying source deviate from that average by the *same* amount, so past the threshold both are rejected together (`AllPriceSourcesRejected`) rather than the bad one being picked out. This is the safer failure mode (refusing to guess which of two disagreeing sources is right) but it means two sources alone don't provide the selective-outlier-rejection this section is otherwise designed for — three or more independent sources are needed for that.

## Security Considerations

- The oracle is a **read-only dependency** — it cannot initiate token transfers or modify ILN state.
- The admin key that controls `set_price_oracle` is a single point of control. A compromised admin can register a malicious oracle. Governance timelock on oracle registration changes is recommended.
- Fail-open behaviour (no oracle → all pass) is appropriate for the MVP. High-security deployments should consider fail-closed defaults.
- Oracle address changes take effect immediately. There is no delay between registering a new oracle and it being used for checks. Consider adding a timelock.

---

## Migration Plan for TWAP Feeds

As part of the rollout of TWAP oracle feeds, clients and the protocol must migrate in a controlled manner:

1. **Deployment**: Upgrade the `invoice_liquidity` contract with the TWAP features. TWAP is disabled by default, meaning all feeds continue to operate using spot/boolean logic.
2. **Data Seeding**: An automated bot or indexer must invoke `record_twap_sample` for the desired `feed_type` and `token`. The buffer must reach the `MIN_TWAP_OBSERVATIONS` limit (default 2) spanning the configured window.
3. **Activation**: The admin invokes `set_twap_enabled(env, feed_type, true)`. Once enabled, price reads (`get_twap_aware_price`) strictly enforce the TWAP bounds and sample counts. If the minimum observations aren't met, an `InsufficientTwapObservations` error will be returned, refusing to silently fall back to spot prices.
4. **Monitoring**: Operators should monitor for `TwapInsufficientData` events to detect stalling feeds and ensure data automation continues submitting samples.
