# Oracle Manipulation Economic Attack Model

**Status:** Analysis — feeds the threat-model re-review tracked as Issue #39 (see [Cross-references](#cross-references)).
**Scope:** Quantifies attacker cost vs. maximum extractable value for manipulating the payer-verification (`Identity`) oracle at the parameter values currently shipped in `contracts/invoice_liquidity`, and recommends adjustments where the model shows a profitable attack at realistic invoice sizes.

---

## 1. Why this is a different kind of "oracle attack" than the DeFi-classic one

Most oracle-manipulation writeups (and most of [`docs/threat-model.md`](threat-model.md)'s existing "Oracle Manipulation" section) model a **price oracle backed by on-chain liquidity** — the attacker's cost is the capital needed to move a DEX price within one block (a flash loan), which scales with the pool's depth. ILN's registered oracle is not that. Per [`oracle-design.md`](oracle-design.md) and [`oracle-integration.md`](oracle-integration.md):

- The oracle is a **single attested address per feed type/token** (`register_oracle` / `register_token_oracle`), reporting an arbitrary off-chain-computed value (`get_payer_data` → `OracleVerificationResponse { is_verified, timestamp }`).
- There is **no on-chain liquidity, no stake, no bond, and no slashing** tied to that address. It is "fully trusted once registered" (`oracle-design.md`'s own words).
- There is **no multi-oracle quorum** — a single compromised or dishonest address is sufficient to falsify a result for any payer.

So the attack surface here is a **trust/permissions exploit**, not a capital-based market-manipulation exploit. The cost model below reflects that: it isn't "how much does it cost to move a price," it's "how much does it cost to make one attested address lie, and how much can be extracted before someone notices and revokes it."

---

## 2. Current parameters (as shipped)

| Parameter | Value | Source | Governable? |
|---|---|---|---|
| Minimum invoice amount | 1 whole token unit (e.g. $1 for a 6-decimal stablecoin, 1 XLM for the 7-decimal SAC) | `validate_invoice_terms_with_token`, `lib.rs:2977-2992` | No — fixed per token's registered decimals |
| **Maximum** invoice amount | **None** — no cap exists anywhere in the contract | (absence confirmed by search) | N/A |
| Max discount rate | 5 000 bps (50%) | `constants::MAX_DISCOUNT_RATE` | Yes — `UpdateMaxDiscountRate` governance action |
| Oracle staleness window actually enforced by `fund_invoice` | `max_oracle_age_ledgers`, default **17 280 ledgers ≈ 24 hours** at 5s/ledger | `DEFAULT_MAX_ORACLE_AGE_LEDGERS`, `lib.rs:83`; consulted in `fund_invoice`'s `require_oracle_verification` block | Yes — `set_max_oracle_age` (admin, rate-limited to 1 call / ~10 min) |
| Direction the staleness window is enforced in | both directions: a `timestamp` **ahead of** the current ledger sequence is treated as stale, not as age 0 (Issue #1050) | shared predicate `oracle_registry::is_oracle_data_stale`, called by `fund_invoice`'s gate and `record_oracle_health` | No — fixed rule (the `max_oracle_age_ledgers == 0` escape hatch still disables it) |
| Replay / rollback of a previously-seen oracle response | rejected: a `timestamp` strictly older than the last recorded for the **same** oracle on that `(feed type, token)` fails with `OracleTimestampRegression` (Issue #1052) | shared predicate `oracle_registry::is_oracle_data_timestamp_regression`, called by `fund_invoice`'s gate before the health snapshot is overwritten; equal timestamps and a different oracle's timeline are allowed | No — fixed rule; the baseline is the persisted `OracleHealthStatus.last_data_timestamp` (no new storage key) |
| Duplicate (unchanged) oracle updates — a "frozen" oracle that keeps re-serving the same `timestamp` | rejected: more than `MAX_CONSECUTIVE_DUPLICATE_UPDATES` (3) consecutive identical timestamps from the **same** oracle fails with `OracleDuplicateUpdate` (Issue #1053) | shared predicate `oracle_registry::is_oracle_data_duplicate_update`, called by `fund_invoice`'s gate after the regression check and before the health snapshot is overwritten; the streak resets on any advance or oracle failover, and a rejected funding call rolls it back | No — fixed rule; the streak is the persisted `OracleHealthStatus.consecutive_duplicate_count` (no new storage key) |
| Legacy 7-day staleness constant | `ORACLE_STALENESS_THRESHOLD_SECS` = 604 800s | `oracle_interface.rs:46` | **Dead code** — `is_fresh()`, the only function that reads it, is never called from `fund_invoice` or anywhere else in the live path. `oracle-integration.md`'s "Staleness Policy" section describes this constant as the enforced policy; it is not — see [§6](#6-a-stale-doc-this-model-corrects). |
| Oracle registration stake/bond | **None** | `oracle_registry::register_oracle` / `register_token_oracle` — `require_admin` only | N/A |
| Oracle removal | `remove_oracle` / `remove_token_oracle` — `require_admin` only, no rate limit, no timelock | `oracle_registry.rs` | Governable via `RegisterOracle`/`RemoveOracle`/`RegisterTokenOracle` proposal actions (added for the per-token case in this issue's companion e2e test), **or** a direct call if `admin` is still a fast key rather than the governance contract |
| Governance voting window | 3 days (259 200s) | `VOTING_PERIOD_SECS`, `iln_governance` | — |
| Governance execution delay (additional timelock after passing) | 0 by default | `set_execution_delay`, defaults via `unwrap_or(0)` | Yes |
| `pause()` | Instant, admin-only, no timelock, no rate limit; halts `fund_invoice` (and all other mutating entry points) immediately | `lib.rs`, confirmed in [access-control.md §7](access-control.md#7-pause-behavior--cross-contract-scope) | — |
| `require_oracle_verification` | **Opt-in per call** — the funding LP decides whether to pass `true` | `fund_invoice`'s 5th argument | — |
| `OracleFeedType::Price` (price-feed manipulation) | Registered but **not consumed anywhere** in `fund_invoice` or any other economic logic today | confirmed by search across `lib.rs`/`oracle_registry.rs` | N/A today |

Two things fall out of this table immediately, before any modeling: **there is no maximum invoice size**, and **there is no economic cost (stake/bond) attached to being the oracle**. Both are load-bearing for the result below.

---

## 3. Attack mechanics

The only oracle-driven economic lever live today is the `Identity` feed's `is_verified` boolean, consulted in `fund_invoice` only when the funding LP passes `require_oracle_verification=true`:

```
if require_oracle_verification {
    if let Some(oracle_addr) = oracle_registry::resolve_oracle(&env, OracleFeedType::Identity, &invoice.token) {
        let response = <cross-contract call to oracle_addr.get_payer_data(payer)>;
        // reject if stale (age >= max_oracle_age_ledgers)
        // reject if !response.is_verified
    }
}
```

**Fraud scenario:** an attacker controls both the invoice's `freelancer` (payout recipient) and `payer` (the identity being verified) roles — a fabricated invoice against a fabricated or complicit counterparty. If the registered `Identity` oracle for that token falsely reports `is_verified: true` for the payer, an honest LP who opted into oracle verification is induced to fund the invoice. The attacker collects the freelancer payout (`invoice.amount × (1 − discount_rate)`, paid immediately once `fund_invoice` fully funds the invoice — see `lib.rs:1641-1643`) and the payer simply never repays. The invoice eventually defaults; the LP absorbs the loss (net of whatever the insurance pool separately covers — the attacker's *extraction* is the freelancer payout regardless of what happens to the LP's downstream compensation).

**How the oracle gets to lie** — two distinct paths, both zero-stake:

1. **Vendor compromise.** A legitimately vetted, governance-registered oracle (see [oracle-provider-vetting.md](oracle-provider-vetting.md)) has its own off-chain infrastructure compromised — a stolen admin key, an insider, a supply-chain attack on whatever system computes `is_verified`. Nothing on-chain about the *ILN contract* changes; the attacker just makes the already-trusted address answer falsely.
2. **Vendor turns malicious ("rug").** A registered vendor themselves decides to defraud the protocol. Since there is no bond to forfeit and no on-chain penalty, this is a pure reputational bet for the vendor — cheap if they're pseudonymous or judgment-proof.

Neither path has an on-chain capital cost analogous to a flash-loan-funded price attack. The cost is entirely **off-chain security/trust cost**, which the model below treats as a rough, non-scaling estimate rather than a precise number — see §4.

---

## 4. Cost model

| Cost component | Estimate | Notes |
|---|---|---|
| On-chain cost to register as the oracle (if attacker *is* the vendor) | $0 | No stake, no fee beyond a normal transaction, gated only by governance's willingness to approve — a **process** failure (inadequate vetting per `oracle-provider-vetting.md`), not an economic one. |
| On-chain cost to falsify a response once registered | $0 per call | `get_payer_data` is a plain contract call the oracle operator controls; nothing is escrowed against its answers. |
| Off-chain cost to compromise a *legitimate* vendor's infrastructure | Highly variable, but **bounded and roughly fixed regardless of extraction size** — order of magnitude of a targeted infrastructure compromise or insider bribe (illustratively, low-to-mid five figures USD in comparable security-incident cost data; treat as an assumption to be revisited, not a derived figure). | Unlike a flash-loan attack, this cost does **not** scale with the amount extracted — compromising the key costs the same whether the attacker then extracts $10,000 or $10,000,000. |
| Off-chain cost if the attacker *is* the (malicious) vendor | Effectively $0 beyond reputational/legal exposure | No bond to lose. |

**Key finding of the cost side:** attack cost is a **fixed, one-time, off-chain quantity that does not scale with extracted value.** Any protocol where the cost side is flat and the benefit side is unbounded and scales with usage is, by construction, profitable past some invoice size — the only question is where that breakeven sits.

---

## 5. Benefit model — maximum extractable value

### 5.1 Per-invoice extraction

Extraction per fraudulent invoice ≈ `invoice.amount × (1 − discount_rate)`. Using a representative 3% factoring discount (300 bps, the value used throughout this repo's own test fixtures):

| Invoice size | Freelancer payout extracted (at 3% discount) |
|---|---|
| $1,000 | ~$970 |
| $10,000 | ~$9,700 |
| $100,000 | ~$97,000 |
| $1,000,000 | ~$970,000 |

There is **no per-invoice cap** (§2), so this table has no ceiling other than what a funding LP is willing to commit and what balance the LP actually holds.

### 5.2 Extraction over the detection-and-response window

A compromised or malicious oracle keeps answering `is_verified: true` (and can always report a fresh timestamp, since it controls the response — the 24-hour staleness window in §2 does not bound *this* attack; it only bounds the separate "stale-but-still-accepted" case where an honest oracle stops updating) for as long as it remains registered. Issue #1050 tightened the window's *direction* — a `timestamp` dated **ahead of** the current ledger is now rejected as stale rather than read as age 0 — but a malicious oracle lying with the *current* ledger sequence is still accepted, so this attack is unchanged. Removal requires *someone to notice and act*:

- **Fast path:** if `admin` is still a responsive key (not yet transferred to the governance contract), `remove_oracle`/`remove_token_oracle` is a single, un-rate-limited, no-timelock call — response time bounded by detection latency, potentially minutes to hours. `pause()` is available even faster (admin-only, instant, halts *all* funding including via other tokens/oracles) as a blunt stopgap while the specific oracle is investigated and removed.
- **Governance path (the intended decentralized end-state):** removal requires a full proposal — minimum **3-day voting window** plus any configured execution delay (0 by default, but governance can raise it) plus the time for someone to notice, draft, and pass the proposal in the first place. A realistic floor is **3–7 days** of continued exposure once an issue exists, even assuming detection is immediate.
- **Detection latency itself:** there is currently no monitoring runbook wiring `check_oracle_health`/`get_oracle_health` (the dedicated, non-erroring oracle-observation entrypoints — see [ADR-010](adr/ADR-010-oracle-registry.md)) into any alerting path (checked: no oracle references in `monitoring-runbook.md` or `indexer-incident-runbook.md`). Detection today is whatever ad-hoc process the team runs, not a designed SLA.

Nothing in the protocol throttles the **rate or volume** of oracle-gated funding — no per-token, per-oracle, or per-LP cap on how many invoices can be funded per unit time. The only two levers are the binary `pause()`/`remove_oracle()`, both reactive.

### 5.3 Worked example

Assume a compromised oracle operates for 3 days (fast, governance-path floor) before removal, during which the attacker gets 10 fraudulent $10,000 invoices funded (a modest rate — one every ~7 hours — well within what a single active LP relationship could plausibly fund without raising suspicion):

```
Extraction = 10 invoices × $10,000 × (1 − 0.03) ≈ $97,000
Attack cost ≈ fixed, low-to-mid five figures (vendor compromise) or ~$0 (malicious vendor)
```

Net profit is strongly positive under either cost assumption. Scaling either the invoice size, the invoice count, or the exposure window (governance-path floor of 3+ days) only widens the margin. **The model shows the attack is profitable well within realistic invoice sizes** — it doesn't require pathological invoice amounts to clear a near-zero, non-scaling cost.

### 5.4 The `Price` feed, for completeness

`OracleFeedType::Price` is registered and resolvable but not read by any economic logic today (§2) — extractable value from manipulating it is currently **$0**, because nothing consumes it. This is a forward-looking note, not a current finding: whenever `Price` is wired into USD normalization or payout math (per `ADR-010`'s stated future direction), this same cost/benefit asymmetry will apply to it unless the recommendations below (particularly staking and quorum) are in place first.

---

## 6. A stale doc this model corrects

While gathering the parameters above, `oracle-integration.md`'s "Staleness Policy" section was found to describe the **legacy, dead** `ORACLE_STALENESS_THRESHOLD_SECS` (7 days) as ILN's enforced staleness rule. The actual, live check in `fund_invoice`'s `require_oracle_verification` path uses the oracle-registry's `max_oracle_age_ledgers` (default ~24 hours, per-feed-type/per-token configurable) — a materially different number that matters directly to this model (§2, §5.2). `oracle-integration.md` has been updated to describe the actual mechanism and flag the legacy constant as unused.

---

## 7. Findings

1. **Attack cost is flat and near-zero; extractable value is unbounded and scales with invoice size, invoice count, and exposure-window length.** This is a structurally profitable configuration, not a marginal one — it doesn't depend on aggressive assumptions.
2. **The absence of a maximum invoice amount is the single biggest amplifier.** Every other parameter (discount rate, staleness window) only scales the extraction linearly or adjusts a secondary sub-case; the missing cap removes any ceiling at all.
3. **The governance path for oracle removal, in its intended decentralized end-state, has a multi-day floor** (§5.2) — a compromised oracle's damage window is bounded by governance latency, not by any protocol-level circuit breaker specific to oracles.
4. **`require_oracle_verification` being opt-in means this risk is currently borne per-LP, not protocol-wide** — but a protocol that markets oracle verification as a trust signal should not leave the LP unaware of how thin that signal actually is (single address, no stake, no quorum).

---

## 8. Recommended parameter and design adjustments

Ordered roughly by impact-to-effort:

1. **Add a maximum per-invoice funding amount** (a new governable parameter, mirroring `MAX_DISCOUNT_RATE`'s pattern). This directly caps §5.1's per-invoice extraction and is the single highest-leverage change identified here, since it's the only currently-uncapped input.
2. **Add oracle staking with slashing.** Require a bond posted at registration, forfeited on a governance-confirmed manipulation finding. This is the only change that converts the cost side from "flat/near-zero" to "scales with what's at stake," directly closing the asymmetry in §4 rather than just shrinking the benefit side.
3. **Move toward multi-oracle quorum (N-of-M) for the `Identity` feed**, as already flagged as future work in `oracle-design.md`. Raises attacker cost from "compromise one" to "compromise N of M," independent of invoice size.
4. **Add a per-oracle (or per-token) exposure cap or rate limit** — a ceiling on total value funded against a single oracle's attestations within a rolling window, bounding §5.2 regardless of how long removal takes.
5. **Wire `check_oracle_health`/`get_oracle_health` into the monitoring runbook** so detection latency (§5.2) becomes a designed SLA rather than an ad-hoc process — this shrinks the exposure window on the *fast* admin-removal path even before any of the above ship.
6. **Preserve a fast, non-governance emergency oracle-disable path** even after admin authority transfers to the governance contract (e.g., a scoped guardian/multisig empowered to call `pause()` or `remove_oracle` immediately, subject to after-the-fact governance review) — the 3-day-plus governance floor in §5.2 is acceptable for routine parameter changes but is a liability specifically for an active oracle compromise.
7. **Tighten `max_oracle_age_ledgers`** if realistic oracle refresh cadence allows it (see the freshness-SLA criterion in [oracle-provider-vetting.md](oracle-provider-vetting.md)). This only addresses the narrower "honest-oracle-goes-stale" sub-case, not a fully compromised oracle actively reporting fresh false data — lowest priority of the set above, included for completeness.

None of these are mutually exclusive; (1) and (2) address the two structural asymmetries identified in §4 and §7 directly and should be prioritized before mainnet launch given the worked example in §5.3.

---

## Cross-references

- [`docs/threat-model.md`](threat-model.md) — Section D ("Oracle Manipulation Attacks") predates the payer-verification oracle interface (Issue #93/#532) entirely and needs a full re-review incorporating this model; tracked as **Issue #39**. A pointer to this document has been added there.
- [`docs/oracle-provider-vetting.md`](oracle-provider-vetting.md) — the governance-side control (vendor vetting) this model assumes is the first line of defense; this document quantifies what's at stake when vetting fails or a vetted vendor is later compromised.
- [`docs/access-control.md#7-pause-behavior--cross-contract-scope`](access-control.md#7-pause-behavior--cross-contract-scope) — confirms `pause()`'s instant, admin-only semantics relied on in §5.2 and recommendation 6 of §8.
- [`docs/oracle-design.md`](oracle-design.md) and [`docs/adr/ADR-010-oracle-registry.md`](adr/ADR-010-oracle-registry.md) — trust model and registry architecture this model is built on.
- [`docs/oracle-integration.md`](oracle-integration.md) — corrected per §6.
