# ILN Smart Contract Threat Model

**Document Version:** 2.0  
**Date:** May 2024 (Original), August 2026 (Re-reviewed)  
**Status:** Pre-Audit  
**Last Reviewed:** 2026-08-29

## Executive Summary

The Invoice Liquidity Network (ILN) protocol on Soroban enables freelancers to monetize unpaid invoices through liquidity providers (LPs) who purchase discounted claims. This threat model identifies potential attack vectors, trust assumptions, and existing mitigations for the complete protocol stack, now encompassing five smart contracts plus on-chain governance.

**Scope (v2.0):** 
- Core `invoice_liquidity` contract (invoice lifecycle, reputation, LP queue, oracle registry)
- `iln_governance` contract (proposal voting, delegation, execution)
- `iln_distribution` contract (reward accrual and distribution)
- `insurance_pool` contract (default protection and premium collection)
- `reputation_bonus` contract (reputation-based scoring)
- Payer-verification oracles and price oracles for volume normalization

**Out of Scope:** Frontend, RPC endpoints, custodial systems

## Changes in v2.0 (Re-Review August 2026)

The following major architectural components have been added since v1.0 and are now reviewed against the original threat model:

### ✅ Multi-Sig Admin Support (Issue #124)
**Status:** Implemented  
Admin functions now include rate limiting (120-1440 ledger cooldowns), supporting multi-sig governance patterns. While the contract does not enforce multi-sig at the code level, governance can configure rate limits to require human-in-the-loop approval. See section E1 update below.

### ✅ Oracle Registry & Governance (Issue #93 / #532)
**Status:** Implemented  
New governance-controlled oracle registry (`register_oracle`, `remove_oracle`, `register_token_oracle`, `remove_token_oracle`) allows proposals to update oracle addresses without timelock constraints (intentional — oracle changes are governance responses, not parameter tweaks). Mitigates D2 (missing external oracle integration) with governance oversight.

### ✅ MEV Mitigation & Fair LP Queue (Issue #708)
**Status:** Implemented  
LP queue resolution now uses uniform random selection among tied reputation scores via `env.prng()`, eliminating front-runner predictability for tie-breaking. Reduces B1 (LP Queue Position Manipulation) from ⚠️ LOW-MEDIUM to ✅ LOW.

### ✅ Governance Contract (Issue #606)
**Status:** Implemented  
New `iln_governance` contract enables on-chain proposals, voting, delegation, and execution. Includes flash-loan vulnerability analysis (E3). See section E updates below.

### ✅ Distribution Contract (Issue #637)
**Status:** Implemented  
New `iln_distribution` contract manages yield distribution with ILN-contract-only accrual functions. Adds distribution-layer attack surface covered in new Section H.

### ✅ Insurance Pool (Issue #510)
**Status:** Implemented  
Standalone contract for default protection. Pool-specific attacks covered in Section G.

---

## Trusted Parties

### 1. **Admin**
- **Role:** Central authority for contract configuration and dispute resolution
- **Capabilities:** 
  - Update governance parameters (decay rates, reputation thresholds, fee rates)
  - Manage token registry (add/remove approved tokens)
  - Pause/unpause contract
  - Resolve appeals and disputes
  - Set primary distribution contract

**Risk:** Admin key compromise would allow:
- Unilateral parameter manipulation (high reputation requirements, excessive decay)
- Freezing of contract via pause
- Adding malicious tokens
- Forced resolution of disputes in admin's favor

**Mitigation:** 
- Multi-sig governance recommended (outside contract scope)
- Time-locks on critical parameters (outside contract scope)
- Public governance events logged on-chain for transparency

### 2. **Soroban Runtime & Stellar Network**
- **Assumptions:**
  - Soroban executor is bug-free
  - Stellar consensus is honest (Byzantine fault tolerance)
  - Cryptographic primitives (SHA-256, Ed25519) are collision-resistant
  - Ledger state is immutable once finalized

**Risk:** Network-level attacks (51% attacks, consensus failures) are outside contract scope but catastrophic if realized.

**Resource limits (Issue #1065):** the contract additionally relies on Soroban's
per-invocation CPU-instruction and memory budget, and its per-entry size limit.
That reliance is only safe if every on-chain collection the contract iterates or
rewrites is bounded — an unbounded collection (fund queue, price-source list,
admin action log, top-payers heap, TWAP samples, upgrade/rollback samples) could
be grown until a single invocation exceeded the budget and trapped, denying that
path for every affected invoice or feed. **Mitigation:** every such collection is
bounded and the bounds are declared in one place, `storage::RESOURCE_ENVELOPE`
(`contracts/invoice_liquidity/src/storage.rs`) — see
[`storage-layout.md`](storage-layout.md#the-safe-resource-envelope-issue-1065) —
with drift/boundary/authorization tests in `tests_storage.rs` and the read-at-cap
cost measured against the absolute ceilings in `tests_benchmarks.rs` (see
[`benchmarks.md`](benchmarks.md#safe-resource-envelope-read-cost-issue-1065)).

### 3. **Token Contracts (USDC, EURC, XLM)**
- **Assumptions:**
  - Tokens implement Stellar Asset Contract standard correctly
  - Token transfers are atomic and final
  - Tokens have no hidden transfer hooks that could fail unexpectedly

**Risk:** Malicious or buggy token implementation could:
- Revert transfers unexpectedly, leaving invoices in inconsistent states
- Front-run token transfers via hooks
- Violate token balance invariants

**Mitigation:**
- Admin controls token registry (whitelist only trusted tokens)
- Contract validates token approval before use
- No recursive calls to token contracts

---

## Attack Surfaces & Threats

### A. REENTRANCY ATTACKS

#### A1. Cross-Contract Reentrancy via Token Transfers

**Description:**  
When the contract transfers tokens to users (e.g., `fund_invoice()`, `claim_default()`), the destination address could be a malicious contract that calls back into ILN during the transfer.

**Attack Scenario:**
```
1. Attacker deploys malicious contract as LP
2. Attacker calls fund_invoice() with attacker contract as funder
3. Token transfer to attacker contract triggers callback
4. Callback calls mark_paid() or another state-mutating function
5. Contract state could be manipulated (double-spending LP funds)
```

**Current Mitigation:**
- ✅ **Checks-Effects-Interactions Pattern:** Contract updates `invoice.amount_funded` **before** calling token transfer
- ✅ **Single Transfer Per TX:** Token transfer is the final external call
- ✅ **No Delegate Calls:** Soroban has no delegate call primitive

**Code Evidence:** [fund_invoice() in lib.rs](contracts/invoice_liquidity/src/lib.rs#L634-L730)
```rust
// UPDATE STATE FIRST (effects)
invoice.amount_funded += amount;
save_invoice(&env, &invoice);

// THEN EXTERNAL CALL (interactions)
token.transfer(...);
```

**Residual Risk:** ⚠️ **LOW-MEDIUM**
- Token contract behavior during transfer is unpredictable
- If token has custom hooks, callbacks could occur
- Mitigation assumes no nested ILN calls during transfer (unproven for all token implementations)

**Recommendation:**
- Audit specific USDC/EURC implementations for callback hooks
- Consider mutex-style guard state variable (set flag before transfer, check on re-entry)

---

#### A2. Reentrancy via Appeal/Dispute Resolution

**Description:**  
The `resolve_appeal()` and `resolve_dispute()` functions are called by admin and modify invoice state. If admin is a contract, callbacks could occur during execution.

**Current Mitigation:**
- ✅ **Admin-Only Access:** Only the contract admin can call these functions
- ✅ **Admin Key is Trusted:** Assumes admin key is secure (not a DAO or contract initially)

**Residual Risk:** ⚠️ **MEDIUM**
- If governance transitions to a DAO contract, DAO could be reentered
- The contract does not prevent admin from being a contract

**Recommendation:**
- Document that admin should be a secure EOA (multi-sig) for the beta phase
- Future governance upgrade should include reentrancy protections (e.g., state flags)

---

### B. FRONT-RUNNING ATTACKS

#### B0. Ordinary `fund_invoice()` Front-Running (Issue #707)

**Description:**
`tests_mev_mitigation.rs` covers the priority-queue path (B1 below) for genuinely
simultaneous funding attempts, but ordinary (non-queued) `fund_invoice()` calls have no
dedicated analysis: could a validator or searcher observing a pending `fund_invoice()`
transaction front-run it the way an EVM searcher front-runs a profitable mempool
transaction?

**Stellar's transaction ordering model, and why it doesn't map onto EVM-style MEV:**
- Stellar uses the Stellar Consensus Protocol (SCP), a federated Byzantine agreement
  scheme — there is no single block proposer/miner per ledger close who unilaterally
  orders transactions the way an Ethereum block builder does. Validators nominate
  candidate transaction sets and vote to converge on one per ~5s ledger close.
- Within a ledger close, transaction ordering across the included set is not an open
  priority-fee auction a searcher can win by outbidding — Stellar's fee model
  (base fee + optional inclusion fee under surge pricing) determines *whether* a
  transaction is included under network congestion, not an arbitrary reordering
  priority a searcher can exploit to insert a transaction ahead of a specific target
  once both are in the candidate set.
- There is no widely-deployed, EVM-Flashbots-equivalent private-orderflow/searcher
  infrastructure for Stellar today that would let a party reliably observe a pending
  `fund_invoice()` call and construct a transaction guaranteed to land immediately
  before it.

**Why `fund_invoice()` specifically isn't a profitable front-running target even where
ordering *could* be influenced:**
- Classic EVM front-running/sandwich attacks extract value from a **price-sensitive**
  operation (an AMM swap with slippage) — the victim's trade moves a price the
  front-runner can arbitrage. `fund_invoice()` has no such mechanism: the invoice's
  discount rate, fee, and terms are fixed at invoice-creation time and read verbatim
  by whichever transaction executes, in whichever order. A front-runner who funds
  first gets the same fixed terms the original caller would have gotten — there is no
  slippage, no price impact, and nothing to extract *from* the original caller's
  transaction. The only thing at stake is which of two willing LPs funds the invoice
  first, a race condition, not value extraction.

**Residual Risk:** ✅ **LOW** — Stellar's consensus model doesn't expose the
block-builder-controlled reordering that EVM front-running relies on, and even if a
transaction's relative position could be influenced, `fund_invoice()` has no
price-sensitive state for a front-runner to extract value from. This differs from the
genuine (LOW-MEDIUM) risk already documented for the priority-queue path in B1, where
multiple LPs *competing* for the same invoice via `join_fund_queue()`/
`resolve_fund_queue()` do have a real (if narrow) tie-breaking-predictability concern —
see Issue #708.

**Recommendation:** No mitigation needed for ordinary `fund_invoice()` calls given the
above. If Stellar's transaction ordering model changes materially (e.g. a future
protocol upgrade introduces proposer-controlled reordering), this analysis should be
revisited.

---

#### B1. LP Queue Position Manipulation

**Description:**  
LPs call `join_fund_queue()` to register intent before `resolve_fund_queue()` selects the winner. A front-runner could:

1. Observe pending `join_fund_queue()` TX
2. Front-run with higher reputation snapshot
3. Win the funding right by being selected first

**Attack Scenario:**
```
1. LP1 calls join_fund_queue(invoice_id) with reputation 45
2. Front-runner observes TX in mempool
3. Front-runner calls resolve_fund_queue() immediately after
4. If mempool order favors front-runner's join TX, front-runner wins
```

**Current Mitigation:**
- ✅ **Reputation Scoring:** Resolution selects highest reputation LP at selection time
- ✅ **Decentralized Mempool:** Stellar/Soroban mempool is not ordered by gas (unlike Ethereum)
- ✅ **Queue Snapshot:** Each queue entry stores reputation at join time

**Code Evidence:** [resolve_fund_queue() in lib.rs](contracts/invoice_liquidity/src/lib.rs#L810-L880)
```rust
// Select LP with highest reputation snapshot
let best_candidate = queue.iter()
    .max_by_key(|entry| entry.reputation_score)
    .cloned();
```

**Residual Risk:** ✅ **LOW** (was ⚠️ LOW-MEDIUM)
- **Resolved (Issue #708):** ties among equal-reputation LPs are no longer
  first-in-queue-deterministic — `resolve_fund_queue` now selects uniformly at
  random among all tied top-score entries via `env.prng()`, Soroban's
  network-seeded PRNG. See `contracts/invoice_liquidity/src/lib.rs`'s
  `resolve_fund_queue` and the fairness tests in `tests_mev_mitigation.rs`.
- Stellar's random sequence of validators makes pure front-running hard
- LPs observing pending TXs could still delay their own join to improve
  position, but can no longer guarantee a win by being first among ties

**Recommendation:**
- Monitor queue resolution patterns for anomalies in off-chain analytics
- Soroban's PRNG is validator-seeded, not a secret-entropy source (see the
  crate's own `prng` module docs) — acceptable here since nothing security-
  critical (funds custody, auth) depends on the outcome, only which of
  several already-eligible LPs is selected

**Clarification — post-resolution transfer is NOT a fairness bypass (Issue #712):**
`transfer_lp_position()` lets a funded invoice's LP hand their position to any
other address, with no check against funding-queue history. This means a
queue winner can immediately transfer their position to the address that
*lost* the same queue resolution — functionally equivalent to a private
side arrangement where the "loser" ends up funding the invoice anyway. This
is **intentional, documented secondary-market behavior**, not an accidental
bypass of the queue's fairness guarantee:
- The queue's fairness guarantee is specifically about *who gets first
  crack at funding the invoice* (reputation-weighted, randomized on ties
  per Issue #708) — it says nothing about what either party does with the
  resulting position afterward.
- `transfer_lp_position()` already exists as a general-purpose position
  handoff mechanism used elsewhere in the protocol, independent of whether
  the position originated from a queue resolution, a direct `fund_invoice()`
  call, or anything else — restricting it specifically for queue-originated
  positions would be an arbitrary carve-out with no clear security benefit,
  since the two parties could achieve the same economic outcome through an
  off-chain side payment regardless.
- See `test_queue_winner_can_transfer_position_to_queue_loser` in
  `tests_mev_mitigation.rs` for a test confirming this works exactly as
  `transfer_lp_position` intends elsewhere in the protocol.

---

#### B2. Discount Rate Manipulation via Reputation Decay

**Description:**  
Freelancers could front-run `mark_paid()` or `claim_default()` with decay-triggering transactions to change effective discount rates.

**Attack Scenario:**
```
1. Freelancer has reputation score 70 (high-rep threshold 60, discount -100 bps)
2. Freelancer observes pending LP funding TX
3. Freelancer calls market_paid() on unrelated invoice to trigger decay
4. Decay drops reputation to 59
5. Incoming LP funding now uses full discount_rate instead of reduced rate
```

**Current Mitigation:**
- ✅ **Decay Applied on Read:** Reputation decay is calculated lazily when score is retrieved
- ✅ **Snapshot at Submission:** Invoice stores discount_rate at submission time (not recalculated)

**Code Evidence:** [submit_invoice() in lib.rs](contracts/invoice_liquidity/src/lib.rs#L250-L270)
```rust
let invoice = Invoice {
    // ... other fields ...
    discount_rate,  // Frozen at submission time
};
```

**Residual Risk:** ⚠️ **LOW**
- Discount rate on invoice is immutable once submitted
- Only affects future invoices, not in-flight ones
- Attack has no direct benefit to attacker (only affects payer, not freelancer revenue)

**Recommendation:**
- Document that discount rates are locked at submission
- Monitor reputation scores for abnormal decay patterns

---

### C. TIMESTAMP MANIPULATION ATTACKS

#### C1. `due_date` Bypass via Clock Manipulation

**Description:**  
The contract checks `env.ledger().timestamp()` against invoice `due_date` to determine payment status. A validator or sequencer could manipulate ledger timestamp.

**Attack Scenario:**
```
1. Invoice is due at timestamp T
2. Validator produces a block with timestamp T-10 (just before due date)
3. Payer fails to mark invoice as paid
4. Validator produces next block with timestamp T+100 (after due date)
5. Payer marked as defaulter despite paying in time
```

**Current Mitigation:**
- ✅ **Stellar Consensus:** Ledger timestamp is set by consensus (median of validator votes)
- ✅ **Timestamp Monotonicity:** Soroban enforces `new_timestamp >= previous_timestamp`
- ✅ **Validator Incentives:** 51% of validators would need to collude (Byzantine assumption)

**Code Evidence:** [Validation in lib.rs](contracts/invoice_liquidity/src/lib.rs#L280-L295)
```rust
if env.ledger().timestamp() >= invoice.due_date {
    // Invoice is overdue
}
```

**Residual Risk:** ⚠️ **MEDIUM**
- Attacks require 51% validator collusion (network-level risk, not contract-specific)
- Small timestamp drifts (10-60 seconds) are hard to exploit deterministically
- Validator incentives are misaligned with manipulation

**Recommendation:**
- Document timestamp assumptions (Stellar consensus, 51% honest validators)
- Recommend generous grace periods for critical deadlines (24-hour payment windows, not seconds)
- Monitor Stellar validator consensus for anomalies

#### C2. Appeal Window Bypass

**Description:**  
The `appeal_default()` function enforces a 30-day appeal window from `due_date`. A payer could attempt to appeal after the window via timestamp manipulation.

**Attack Scenario:**
```
1. Invoice defaults (due_date = T, 30-day window until T+30 days)
2. Payer waits until day 29
3. Payer calls appeal_default() before day 30
4. Attacker validator delays block inclusion until day 35
5. Appeal is accepted despite being outside 30-day window (if timestamps used incorrectly)
```

**Current Mitigation:**
- ✅ **Ledger Sequence Used:** Windows are measured in ledger sequence, not timestamp
- ✅ **Monotonic Ledger Sequence:** Impossible to go backwards in ledger height

**Code Evidence:** [appeal_default() validation](contracts/invoice_liquidity/src/lib.rs#L1410)
```rust
let appeal_window_ledgers = 30 * 24 * 60 * 10; // 30 days in ledger units
if env.ledger().sequence() > invoice.due_date_ledger + appeal_window_ledgers {
    return Err(ContractError::AppealWindowClosed);
}
```

**Residual Risk:** ✅ **LOW**
- Ledger sequence is cryptographically protected
- Cannot be manipulated without breaking Stellar consensus

---

### D. ORACLE MANIPULATION ATTACKS

> **⚠️ Pending re-review (Issue #39):** D1/D2 below predate the payer-verification
> oracle interface and governance-controlled oracle registry (Issue #93/#532) —
> D2 in particular states "no integration with external credit oracles," which
> is no longer accurate now that `oracle_interface.rs`/`oracle_registry.rs`
> exist and are live in `fund_invoice`. A full re-review of this section is
> tracked as Issue #39. In the meantime, see
> [**D3 below**](#d3-payer-verification-oracle-manipulation-economic-model)
> and [`docs/oracle-attack-economics.md`](oracle-attack-economics.md) for a
> quantified cost/benefit model of manipulating the oracle that actually ships
> today.

#### D1. Reputation Score Manipulation

**Description:**  
The reputation system relies on the contract's internal tracking of scores. If reputation calculations are wrong, LPs could game the system.

**Attack Scenario:**
```
1. Attacker submits and immediately defaults on 100 invoices
2. Attacker reputation drops from 50 to 0 (crude model)
3. Attacker waits for decay to slowly increase reputation back
4. Attacker repeats with another identity
5. Network creates many low-reputation identities
```

**Current Mitigation:**
- ✅ **Decay Mechanism:** Scores decay over time if inactive (removes incentive to collect accounts)
- ✅ **Fixed Penalties:** Defaults incur `-5` score penalty (not %-based)
- ✅ **Score Floor:** Scores are capped at 0-100 range
- ✅ **Admin Oversight:** Admin can monitor patterns and pause if needed

**Code Evidence:** [get_payer_score() in invoice.rs](contracts/invoice_liquidity/src/invoice.rs#L227-L250)
```rust
if u64::from(ledgers_since_activity) >= decay_config.decay_period_ledgers {
    let periods_passed = u64::from(ledgers_since_activity) / decay_config.decay_period_ledgers;
    for _ in 0..periods_passed {
        let decay_amount = (decayed_score * decay_config.decay_rate_bps as u64) / 10_000;
        decayed_score = decayed_score.saturating_sub(decay_amount);
    }
}
```

**Residual Risk:** ⚠️ **MEDIUM**
- Reputation is centralized in contract; no external data feed
- Decay mechanism is configurable by admin (potential misuse)
- No on-chain evidence of off-chain reputation events (defaults, appeals)
- LPs must trust admin did not artificially inflate/deflate scores

**Recommendation:**
- Publish reputation change events for off-chain verification
- Document reputation model as **not cryptographically proven** (trust-based on contract execution)
- Recommend frequent reputation audits by independent parties
- Consider reputation delegation (querying other protocols like Lens, etc.)

#### D2. Payer Verification & Creditworthiness Oracle

**Description (v1.0):**  
The contract had no integration with external credit oracles. Payer reputation was purely based on payment history in ILN, not broader financial trustworthiness.

**v2.0 Improvements:**
- ✅ **Payer-Verification Oracle:** New `oracle_interface.rs` / `oracle_registry.rs` allow governance-controlled payer verification oracles
- ✅ **fund_invoice Validation:** `fund_invoice()` now calls the registered oracle to check `is_verified` before accepting funding
- ✅ **Governance Control:** Oracle registration/removal via proposals (not admin-only)
- ✅ **Stale Data Protection:** Oracle responses include timestamp; contract rejects data older than `max_oracle_age_ledgers` (~24h default)
- ✅ **Ledger-Boundary Freshness (Issue #1050):** the freshness rule is one shared predicate (`oracle_registry::is_oracle_data_stale`) used by both the `fund_invoice` gate and the `record_oracle_health` snapshot. It treats a `response.timestamp` **ahead of the current ledger sequence** as stale, so a future-dated timestamp can no longer saturate the age to 0 and read as "maximally fresh" — the bound holds across the full `u32` ledger range. `max_oracle_age_ledgers == 0` still disables the check (governance escape hatch).
- ✅ **Monotonic Timestamps (Issue #1052):** freshness bounds *how old* a response may be but not its *direction*. `fund_invoice` also rejects a `response.timestamp` strictly **older** than the last timestamp recorded for the **same** oracle on that `(feed type, token)` with `OracleTimestampRegression` (the shared `oracle_registry::is_oracle_data_timestamp_regression` predicate, checked before the health snapshot is overwritten), closing replay / rolled-back-clock responses that were still inside the window. Equal timestamps, a first read with no snapshot, and a *different* oracle's timeline are allowed. An accepted response must therefore lie in `[last_recorded, current_ledger]`.
- ✅ **Bounded Duplicate Update Frequency (Issue #1053):** the non-decreasing rule above allowed a "frozen" oracle to re-serve an identical `timestamp` forever — still inside the freshness window and still resetting the stale streak, so a stalled/compromised oracle could keep oracle-gated funding open at no cost. `fund_invoice` now also rejects a same-oracle response repeating an identical timestamp more than `MAX_CONSECUTIVE_DUPLICATE_UPDATES` (3) times in a row with `OracleDuplicateUpdate` (shared `oracle_registry::is_oracle_data_duplicate_update` predicate, checked before the snapshot is overwritten). The `OracleHealthStatus.consecutive_duplicate_count` streak resets on any advance or oracle failover and is mirrored on the `oracle_health_recorded` event; a rejected funding call rolls it back, so the gate re-opens only once the oracle actually advances.
- ✅ **Feeder-Authorized Oracle Updates (Issue #1046):** the oracle's `update_verification` write path — the single entry point that can flip a payer to verified, and therefore unlock `fund_invoice` — was documented as operator-only but not enforced by the reference oracle, so any caller could mint funding eligibility for any payer (reducing the oracle gate to fail-open). A conforming implementation must now persist a designated **feeder** (operator) address, require `feeder.require_auth()` before mutating verification state, and reject the call when no feeder is configured. The ILN never calls this path, so the enforcement lives inside the oracle contract the registry points at; the reference implementation and its tests (success / unconfigured / unauthorized-signer under real auth / feeder rotation) model it, and the `OracleInterface` trait contract states the MUST requirement.
- ✅ **Safe Feeder Disagreement (Issue #1047):** even an authorized feeder is still a single point of both trust and failure — before #1047 the gate resolved **one** Identity oracle and trusted its boolean outright, and disagreement between feeders was neither consultable nor observable. Governance may now register several Identity feeders (`add_identity_oracle`, capped at `MAX_IDENTITY_ORACLES_PER_FEED` = 20) and require an `N`-of-`M` agreement (`set_identity_quorum`, `[1, 20]`, default 1); `fund_invoice` queries every registered feeder via the non-panicking `try_invoke_contract` and counts only responses that are `is_verified` **and** fresh. Fewer than `N` agreements fails **closed** (`PayerUnverified`) and emits an `oracle_feeder_disagreement` event. A feeder that reverts or returns stale/unverified data contributes **no** agreement, so it can never *force* verification through; an **empty** feeder list preserves the unchanged single-oracle path, and `OracleInterface` v1 is untouched (each feeder is an independent `get_payer_data` deployment).
- ✅ **Feeder Outage Recovery (Issue #1048):** the per-feed circuit breaker that trips after `MAX_CONSECUTIVE_STALE_QUERIES` (3) consecutive stale responses was **sticky** — cleared only by the governance-gated `reset_oracle_circuit` — so a *transient* feeder outage jammed oracle-gated funding even after the feeder recovered, and the contract could not tell "still broken" from "recovered but still latched". The breaker is now **half-open**: the permissionless `check_oracle_health` keeper path auto-clears it once the **tripping** oracle serves `CIRCUIT_RECOVERY_HEALTHY_OBSERVATIONS` (3) consecutive healthy observations — fresh **and** timestamp-advancing, so frozen/replayed data never counts — spanning at least `CIRCUIT_RECOVERY_MIN_LEDGERS` (120) ledgers, emitting `oracle_circuit_recovered`. The anti-flapping guarantee is preserved (a single fresh query only advances the streak to 1 and never recovers; any stale/frozen observation resets it), only the tripping oracle can recover its own breaker, a missing recovery record fails safe (admin reset still required), and `reset_oracle_circuit` remains the admin fast path and also clears recovery progress.

**Attack Scenario:**
```
1. Attacker is highly reputable in ILN (always pays)
2. Attacker is flagged as high-risk by external payer-verification oracle
3. Attacker calls fund_invoice(); oracle returns is_verified=false
4. fund_invoice() rejects the funding attempt
```

**Mitigation:**
- ✅ **Governance-Approved Oracles:** Only oracles registered via governance proposals can gate funding
- ✅ **Feeder Authorization (Issue #1046):** the oracle's `update_verification` requires the configured feeder's signature and rejects the call when no feeder is set — closing the "anyone can mint funding eligibility" hole at the write path that actually decides it
- ✅ **Safe Feeder Disagreement (Issue #1047):** governance can register multiple Identity feeders and require an `N`-of-`M` agreement (`set_identity_quorum`, default 1) so no single feeder decides the funding gate; disagreement — or too few fresh+verified responses — fails **closed** (`PayerUnverified`) and emits an `oracle_feeder_disagreement` event. A reverting/stale/unverified feeder contributes no agreement and cannot force verification
- ✅ **Oracle Health Checks:** New `check_oracle_health()` function verifies oracle liveness and circuit-breaker status
- ✅ **Feeder Outage Recovery (Issue #1048):** a tripped circuit breaker no longer requires a governance action to clear — the permissionless `check_oracle_health` keeper path auto-recovers it once the tripping oracle proves 3 consecutive fresh, timestamp-advancing observations over ≥120 ledgers, restoring funding without an admin call (anti-flapping preserved; only the tripping oracle can recover; `reset_oracle_circuit` stays the admin fast path)
- ✅ **Gate-Failure Observability (Issue #1049):** because a rejected `fund_invoice` returns `Err` and Soroban rolls its events back, the gate's failure reasons (stale data, feeder disagreement, regressed/duplicate timestamp, open breaker, unverified payer) were previously invisible on chain. The permissionless, never-erroring `check_oracle_verification` runs the *same* gate and returns a structured `IdentityVerificationOutcome`, so its `oracle_stale_data_rejected` (new) and `oracle_feeder_disagreement` events commit — making a failing gate diagnosable without granting any privilege
- ✅ **Price Oracle Separation:** Payer-verification oracles are distinct from price oracles (different circuit domains)
- ✅ **Fallback Behavior:** If oracle fails or is stale, `fund_invoice()` reverts (fail-safe)

**Residual Risk:** ⚠️ **MEDIUM** (reduced from HIGH in v2.0)
- Payer verification depends on external oracle reliability (oracle downtime => no funding). A tripped breaker now self-heals within a bounded window once the feeder recovers (Issue #1048), but funding stays jammed for the duration of the outage plus the recovery window (3 healthy observations over ≥120 ledgers, only if a keeper is polling `check_oracle_health`)
- Oracle response freshness window (~24h) may miss sudden creditworthiness changes
- Governance still required to swap oracles if current one becomes untrustworthy
- Oracles are black-box; no on-chain proof of oracle decision rationale

**Recommendation:**
- **Governance Policy:** Establish oracle vetting criteria (audited providers, multiple redundant oracles)
- **Monitoring:** Dashboard for oracle health; alert on circuit-breaker trips or unusual denial rates. Since Issue #1048 also alert on `oracle_circuit_recovered` (a breaker closed) and poll `get_circuit_recovery_state` to detect a **stuck** recovery, which still requires the admin `reset_oracle_circuit`
- **Redundancy:** Implemented (Issue #1047) — register multiple Identity feeders and raise `set_identity_quorum` above 1 (e.g. `2-of-3`) to require n-of-m agreement; select a quorum that tolerates a single feeder being offline
- **Keeper cadence:** run `check_oracle_health` on a regular schedule — it is the permissionless path that both tracks staleness and (Issue #1048) drives circuit-breaker auto-recovery, so an outage that a keeper never observes cannot auto-heal
- **Documentation:** Publish oracle integration guide for third-party oracle providers
- **Future:** Investigate oracle-independent verification (zero-knowledge proofs of creditworthiness)

#### D3. Price Oracle Sandwich Attacks (Issue 39)

**Description:**  
The contract uses price oracles for USD volume normalization in contract statistics. Any operation that reads an oracle price and then acts on it within the same or adjacent transactions could be sandwiched if the oracle price derives from an on-chain DEX rather than an off-chain feed.

**Attack Scenario:**
```
1. Price oracle uses on-chain DEX spot prices (e.g., Stellar DEX, AMM pools)
2. Attacker observes pending `get_contract_stats()` or other price-dependent operation
3. Attaker front-runs with large swap to manipulate DEX price
4. Oracle reads manipulated price for USD normalization
5. Contract statistics report incorrect USD volume
6. While not directly financial, this affects:
   - Protocol analytics and monitoring accuracy
   - LP decision-making based on volume metrics
   - Potential governance decisions based on incorrect stats
```

**Current Mitigation:**
- ✅ **Statistical Use Only:** Price oracle currently used only for volume normalization in `get_contract_stats()`
- ✅ **No Financial Dependence:** No financial operations (funding, payments) depend on price oracle
- ✅ **Governance Control:** Oracle registration is governance-controlled via `register_oracle()`
- ✅ **Provider Vetting:** `oracle-provider-vetting.md` provides criteria for evaluating oracle providers

**Code Evidence:** [get_price_from_oracle() in invoice.rs](contracts/invoice_liquidity/src/invoice.rs#L874-L880)
```rust
fn get_price_from_oracle(env: &Env, token: &Address) -> Option<i128> {
    let config = crate::storage::get_config(env)?;
    let oracle = config.price_oracle?;
    let args = soroban_sdk::vec![env, token.clone().into_val(env)];
    Some(env.invoke_contract::<i128>(&oracle, &Symbol::new(env, "get_price"), args))
}
```

**Residual Risk:** ⚠️ **MEDIUM-HIGH** (if using on-chain DEX prices)
- Current test implementations use mock data (`MockPriceOracle` in tests)
- Production oracle source undefined - depends on governance-approved providers
- **HIGH risk** if price oracle uses DEX spot prices without TWAP protection
- **LOW risk** if using off-chain signed price feeds (Chainlink, Pyth, etc.)
- Future protocol expansions could introduce price-dependent financial operations

**Recommendation:**
- **Mandatory:** Update `oracle-provider-vetting.md` to explicitly require:
  - Price feed oracles must use manipulation-resistant sources (TWAP, off-chain feeds)
  - Prohibits DEX spot price oracles without TWAP protection
- **Governance Policy:** Establish that only price oracles with these properties are approved:
  - TWAP (Time-Weighted Average Price) mechanisms for any DEX-based oracle
  - Off-chain signed price feeds with multi-signer aggregation
  - Multi-source aggregation with outlier rejection
- **Technical Implementation:** If DEX prices are needed, require TWAP interface:
  ```rust
  // Example TWAP interface for oracle providers
  fn get_price_twap(env: Env, token: Address, window_seconds: u64) -> i128;
  ```
- **Monitoring:** Enhance `check_oracle_health()` to track price volatility and manipulation detection

**Update (Issues #815 / #816 / #817 — implemented, opt-in):**
- TWAP accumulator ported from `contracts/examples/twap_oracle` into
  `contracts/invoice_liquidity/src/twap.rs` (trapezoidal windowed average,
  example tests ported and extended). Wired as an opt-in library — default
  oracle behavior is unchanged (spot).
- Per-feed opt-in: `TwapEnabled(feed_type)` + `set_twap_enabled` (admin /
  governance-gated). `get_verified_price` branches to the windowed average
  when enabled and in-window samples exist, else falls back to spot so a
  freshly-enabled feed stays live. `Identity` payer-verification stays
  boolean spot verification.
- Bounded window: `TwapWindowLedgers` + `set_twap_window_ledgers`, range
  `[360, 17_280]` ledgers (≈30 min–24 h at 5 s/ledger, default `720` ≈ 1 h),
  rejected outside with `ContractError::InvalidTwapWindow`. Bounds rationale
  follows [`oracle-attack-economics.md`](oracle-attack-economics.md) §4–§5:
  the minimum spans many ledgers to dilute single-ledger sandwich
  manipulation; the maximum stays within the `max_oracle_age_ledgers`
  staleness bound so the average never bakes in data the freshness check
  itself rejects.

---

### E. GOVERNANCE ATTACKS

#### E1. Admin Key Compromise

**Description:**  
The admin key controls critical functions: token registry, parameter updates, dispute resolution, pause/unpause.

**Attack Scenario:**
```
1. Admin private key is compromised
2. Attacker calls pause() and freezes all operations
3. Attacker calls resolve_dispute() in their favor (fraudulent)
4. Attacker adds malicious token to registry
5. Attacker updates parameters to favor their accounts
```

**Current Mitigation:**
- ✅ **Require Auth:** All admin functions require `require_auth()` (signature verification)
- ✅ **Public Events:** Critical admin actions emit events (pause, parameter changes)
- ✅ **Community Oversight:** On-chain events can be monitored by users

**Code Evidence:** [set_admin() in lib.rs](contracts/invoice_liquidity/src/lib.rs#L130)
```rust
pub fn set_admin(env: Env, admin: Address) -> Result<(), ContractError> {
    let current_admin = get_admin(&env)?;
    current_admin.require_auth();  // Must sign with current key
    // ...
    set_admin_in_storage(&env, &admin);
}
```

**Residual Risk:** ⚠️ **MEDIUM** (reduced from CRITICAL in v2.0)

**v2.0 Improvements:**
- ✅ Rate limiting on sensitive admin functions (120-1440 ledger cooldowns, ~10min-2h):
  - `set_admin`: 720 ledgers (~1h)
  - `upgrade`: 1440 ledgers (~2h)
  - `update_fee_rate`, `update_max_discount`, `set_min_payer_reputation`: 360 ledgers (~30min)
  - `add_token`, `remove_token`, `set_price_oracle`, `register_oracle`: 120 ledgers (~10min)
  - Emergency functions (`pause`, `unpause`, `resolve_appeal`) are not rate-limited
- ✅ Public events emitted on all admin operations (pause, parameter changes, oracle registry updates)
- ✅ Community monitoring possible via Horizon event stream
- ✅ Oracle registry mutations are governance-controlled (not admin-only)

**Remaining Risks:**
- Rate limits provide delay (10 min to 2 hours) but not immutable timelocks
- Pause/unpause are not rate-limited (intentional for emergency response)
- No mandatory multi-sig enforcement (delegated to governance via rate-limited account rotation)
- No on-chain timelock contract (governance proposals have configurable delay via `set_execution_delay`)

**Recommendation:**
- **Governance Policy:** Require multi-sig admin (2-of-3 or 3-of-5) as organizational practice
- **Governance Policy:** Establish admin key management standards (HSM storage, key rotation ceremonies)
- **Monitoring:** Stream Horizon events for admin actions; alert on unusual patterns
- **Future:** Consider staking-based governance or DAO-controlled admin after governance contract hardens
- **Production Checklist:** Confirm mainnet admin is multi-sig controlled before launch

#### E2. Governance Parameter Misconfiguration

**Description:**  
Admin can update reputation thresholds, decay rates, and discount rates. Incorrect parameters could break economic incentives.

**Attack Scenario:**
```
1. Admin sets decay_rate_bps = 10000 (100% decay per period!)
2. All reputation scores drop to 0 instantly
3. LPs can no longer find qualified invoices
4. Protocol becomes non-functional
```

**Current Mitigation:**
- ✅ **Validation Constraints:** Some parameters have bounds checks (bonus_bps <= 500)
- ✅ **Public Events:** All config changes emit events for monitoring

**Code Evidence:** [update_config() in config.rs](contracts/invoice_liquidity/src/config.rs#L28-L42)
```rust
if bonus_bps > MAX_BONUS_BPS {
    return Err(ConfigError::InvalidBonusBps);
}
if min_discount_rate_bps == 0 {
    return Err(ConfigError::InvalidMinDiscountRate);
}
```

**Residual Risk:** ⚠️ **MEDIUM** (unchanged)

**v2.0 Status:**
- Some parameters now have bounds checks (e.g., `bonus_bps <= 500`)
- However, several parameters still lack upper bounds:
  - `decay_rate_bps` can be set to any u32 (including 10,000+ = 100%+ decay per period)
  - `high_rep_threshold` lacks range validation (could be set > 100)
  - `min_discount_rate_bps` has no upper bound

**Recommendation:**
- **Code Fix:** Add comprehensive validation for all governance-settable parameters:
  - `high_rep_threshold` must be 0-100 (range: score percentile)
  - `decay_rate_bps` must be 0-500 (max 5% decay per period)
  - `decay_period_ledgers` must be > 0
  - `bonus_bps` must be <= 500 (already enforced)
  - `min_discount_rate_bps` must be < 10,000
- **Governance Policy:** Document safe parameter ranges and economic assumptions
- **Operational Practice:** Require testnet dry-runs before mainnet parameter updates
- **Monitoring:** Alert on parameter changes; track economic viability metrics (LP queue length, funding velocity)

---

#### E3. Flash-Loan Balance Manipulation (Vote-Snapshotting)

**Description:**
Governance token balances determine voting weight in the ILN contract. To prevent users from voting and transferring tokens to double-vote, the contract uses a lazy-snapshot mechanism (Issue #738). However, this snapshot is taken dynamically at the exact time `cast_vote()` is called for most voters (only the proposer's balance is snapshotted at `create_proposal()`).

**Attack Scenario:**
```
1. A lending protocol on Stellar offers flash loans for the ILN governance token.
2. An attacker borrows a massive amount of governance tokens via a flash loan.
3. In the same transaction, the attacker calls `cast_vote()` on a target proposal.
4. The lazy-snapshot logic observes the artificially inflated balance, permanently recording it as their snapshotted weight for that proposal.
5. The attacker repays the flash loan.
```

**Residual Risk:** ✅ **FIXED (Issue #805)** — was ⚠️ HIGH.
The lazy first-vote snapshot above is replaced by checkpoint-gated voting:
a first vote requires a `BalanceCheckpoint` predating the proposal's
creation ledger by `MIN_VOTE_HOLD_LEDGERS` (10 ledgers) and carries
`min(checkpoint, current)`; same-transaction borrow-vote-repay is rejected
with `InsufficientHoldingPeriod`, and `delegate_votes` entries are gated
identically. The attack scenario above now fails at step 3. Remaining
residual: multi-ledger (non-flash) borrows held past the holding period, and
the proposer's live-balance creation snapshot — see
[Governance Security Summary §3.2](governance-security-summary.md).

**Recommendation (kept for the residual):**
If aged-balance manipulation ever becomes practical, transition to a
staking-based governance model (e.g., locking tokens in an escrow vault for
the duration of the proposal) or integrate an oracle with cryptographic
proofs of historical ledger balances prior to proposal creation.

---

#### E4. Upgrade-Time TTL Field Removal (Issue #1082)

**Description:**
A Soroban contract upgrade replaces the WASM binary while preserving all
persistent storage entries.  Before this fix, an upgrade could silently
proceed even when one or more critical persistent entries (`InvoiceCore`,
`InvoiceMetadata`, `FundQueue`, `FundQueueOpenedAt`, `QueueResolution`) had
drifted close to their TTL expiry — either through a missing `extend_ttl`
call in an earlier code path, or through deliberate omission by a compromised
admin.  The new WASM would then begin executing against storage entries that
could expire mid-flight, causing silent data loss with no on-chain indication.

**Attack Scenario:**
```
1. Admin (or compromised admin key) deliberately stops calling extend_ttl
   on active invoices over multiple upgrade cycles.
2. After enough time passes, InvoiceCore/InvoiceMetadata TTLs fall below
   the safety threshold (e.g., < 500,000 ledgers remaining).
3. Admin calls upgrade() — previously this would succeed.
4. New WASM executes against near-expiry storage; within days, live invoice
   data is archived by the Soroban host.
5. Users lose access to invoice records; LP positions become irrecoverable.
```

**Residual Risk Before Fix:** ⚠️ **HIGH** — silent data loss possible via upgrade.

**Fix (Issue #1082):** `upgrade()` now calls
`storage::validate_ttl_fields_pre_upgrade()` _before_ the WASM swap. The
guard samples the most-recent 50 invoice IDs and checks all five key
categories. If any existing entry has remaining TTL <
`TTL_MIN_LEDGERS_BEFORE_UPGRADE` (500,000 ledgers ≈ 29 days), the call
returns `Err(ContractError::UpgradeRemovesTtlFields)` (error code 50) and
the upgrade is **rejected fail-closed** — the WASM is never swapped.

**Residual Risk After Fix:** ✅ **LOW**
- The guard is bounded to the most-recent 50 invoices (older entries outside
  the window are not checked — by design, since re-extending expired data
  would require recovering it from the ledger archive first).
- A sufficiently patient admin with access to both the admin key and the
  ability to suppress `save_invoice` calls for > 29 days could exhaust the
  sample window. Multi-sig admin is the defense against that scenario.
- The guard does not check non-invoice persistent keys (e.g. `LpScore`,
  `Reputation`). Those do not carry protocol-critical locked-funds data and
  can be refreshed independently without blocking an upgrade.

**Mitigation:**
- ✅ Fail-closed pre-upgrade TTL guard (`validate_ttl_fields_pre_upgrade`)
- ✅ New `ContractError::UpgradeRemovesTtlFields` (code 50) — distinguishable
  from all other upgrade failures
- ✅ `TTL_MIN_LEDGERS_BEFORE_UPGRADE = 500_000` (≈ 29 days) — well above any
  reasonable upgrade execution window
- ✅ Remediation path documented: re-invoke `save_invoice` / queue helpers to
  refresh TTLs before retrying
- 🔲 Governance should require multi-sig admin to prevent a single compromised
  key from suppressing TTL renewals over an extended period

**Code Evidence:** `contracts/invoice_liquidity/src/storage.rs` —
`validate_ttl_fields_pre_upgrade()`; `contracts/invoice_liquidity/src/lib.rs`
— `upgrade()`.


### F. TOKEN TRANSFER EDGE CASES

#### F1. Token Transfer Fails, But State Is Updated

**Description:**  
The contract updates state before calling `token.transfer()`. If transfer fails, state inconsistency occurs.

**Attack Scenario:**
```
1. LP calls fund_invoice() for 1000 USDC
2. Contract sets invoice.amount_funded = 1000
3. Token transfer fails (token is paused, LP balance insufficient, etc.)
4. Function reverts due to failed transfer
5. But state rollback is incomplete (ledger reverts, but off-chain listeners might see partial state)
```

**Current Mitigation:**
- ✅ **Atomic Transactions:** Soroban transactions are atomic (all-or-nothing)
- ✅ **Checks-Effects-Interactions Pattern:** State updated before external calls
- ✅ **Explicit Error Handling:** Contract doesn't silently swallow errors

**Code Evidence:** [fund_invoice() in lib.rs](contracts/invoice_liquidity/src/lib.rs#L700-L730)
```rust
invoice.amount_funded += amount;
save_invoice(&env, &invoice);
token.transfer(&funder, &freelancer, &amount)?;  // If this fails, TX reverts
```

**Residual Risk:** ✅ **LOW**
- Soroban ensures all-or-nothing execution
- State changes are rolled back if any call fails
- Token transfer is final external call (safe pattern)

**Recommendation:**
- Document assumption of atomic transactions (rely on Soroban)
- Continue checks-effects-interactions pattern for all external calls

#### F2. Token Allowance Not Set

**Description:**  
The contract calls `token.transfer()`, which requires the sender to have approved the amount. If approval is missing, transfer fails.

**Attack Scenario:**
```
1. LP calls fund_invoice() without first approving ILN contract
2. token.transfer() fails due to insufficient allowance
3. TX reverts, but LP may not understand why
4. User experience is poor
```

**Current Mitigation:**
- ✅ **Documentation:** Off-chain UI should guide users to approve first
- ✅ **Clear Error Messages:** Contract returns `Unauthorized` if transfer fails

**Residual Risk:** ⚠️ **LOW**
- Not a security issue (user error, not exploit)
- Affects UX but not contract integrity

**Recommendation:**
- Add helper functions for checking allowance
- Publish integration guide with approval steps

#### F3. Partial Token Transfer Success

**Description:**  
Some token implementations allow partial transfers. If token transfers less than requested, contract state is inconsistent.

**Attack Scenario:**
```
1. LP calls fund_invoice() for 1000 USDC
2. Token contract only transfers 999 USDC (token deduction/fee logic)
3. Contract records invoice.amount_funded = 1000 (incorrect!)
4. Freelancer receives only 999 USDC but invoice shows 1000 funded
```

**Current Mitigation:**
- ✅ **Token Specification:** Stellar Asset Contract standard requires full amount or revert
- ✅ **Immutable Token Code:** Once a token is deployed, its behavior is fixed (no upgrade without governance)

**Code Evidence:** All token transfers assume Stellar Asset Contract standard behavior.

**Residual Risk:** ⚠️ **LOW-MEDIUM**
- Only applies if a non-standard token is added to registry
- Admin controls token registry (can prevent malicious tokens)
- Recommendation: Only whitelist well-audited tokens (USDC, EURC, native XLM)

**Recommendation:**
- Admin should conduct token audit before whitelisting
- Document token requirements (no fee-on-transfer, standard interface)
- Consider adding token validation helper (test transfer of 1 stroop to verify behavior)

---

### G. INSURANCE POOL SPECIFIC THREATS

#### G1. Premium Manipulation

**Description:**  
The insurance pool allows members to deposit premiums. If premium calculations are incorrect or the admin can manipulate premium rates, members may overpay or underfund the pool.

**Attack Scenario:**
```
1. Admin calls update_premium_rate() with inflated rate (e.g. 50% instead of 5%)
2. Members attempting to enroll see high premium cost
3. Members are priced out of insurance
4. Pool remains underfunded or fills slowly
5. When claims occur, pool cannot cover losses
```

**Current Mitigation:**
- ✅ **Configurable Parameters:** Premium rates are set via governance (admin-controlled)
- ✅ **Public Events:** Premium rate changes emit events for monitoring
- ✅ **Enrollment Validation:** Members must explicitly approve premium amounts

**Residual Risk:** ⚠️ **MEDIUM**
- Admin can unilaterally set premium rates
- No bounds checking on premium_rate_bps (could be set to 100%+ of pool coverage)
- Members may not understand premium mechanics (off-chain communication required)
- No automatic premium rate discovery mechanism (rates are hardcoded by admin)

**Recommendation:**
- Add parameter bounds: `premium_rate_bps` should be capped at reasonable % (e.g., <= 1000 bps or 10%)
- Document premium model and member communication (expected rates, annual yield)
- Consider gradual premium increases (no step changes > 100 bps per update)
- Monitor enrollment trends for drop-offs after rate increases

#### G2. Claim Fraud & Moral Hazard

**Description:**  
Members can submit claims on defaults. Without proper verification, a member could coordinate with a payer to fraudulently claim insurance (moral hazard attack).

**Attack Scenario:**
```
1. Member A and Payer B collude
2. Member A submits invoice to Payer B (high amount, reasonable terms)
3. Member A and Payer B stage a default (Payer B intentionally doesn't pay)
4. Member A claims insurance for full invoice amount
5. Pool pays out fraudulent claim
6. Payer B and Member A split the payout off-chain
```

**Current Mitigation:**
- ✅ **Enrollment Verification:** Pool can verify member address and require KYC (off-chain)
- ✅ **Invoice History:** Claims are tied to actual invoice_liquidity defaults (immutable on-chain)
- ✅ **Admin Review:** Admin can investigate claims and contest fraud
- ✅ **Payer Reputation:** Payers with low default history are less incentivized to stage defaults
- ✅ **On-chain Claim Evidence (Issue #828):** `submit_claim_evidence(invoice_id, hash)` attaches an immutable, timestamped digest of the invoice/evidence-of-payment-attempt documentation to every claim, queryable off-chain for auditors
- ✅ **Opt-in Review Window (Issue #828):** governance can set `set_review_window_seconds(n)` per risk tier so payouts sit for `n` seconds after evidence submission, giving time for dispute/contest before funds move
- ✅ **Per-pair Default Tracking (Issue #829):** `record_pair_default` + `get_pair_collusion_flag` expose per-(LP, payer) concentration (defaults where one payer dominates a single LP's losses), feeding off-chain fraud detection

**Residual Risk:** ⚠️ **MEDIUM**
- On-chain evidence is a hash only — the underlying documents live off-chain and must be preserved by the submitter
- Repeated small defaults by the same payer pair are tracked but only *flagged* for review, not automatically rejected
- Review window and flagging are governance-opt-in; a pool that never enables them retains the automatic flow
- No cryptographic proof of member legitimacy or payer creditworthiness

**Recommendation:**
- **Adopt the review window** for high-value risk tiers as a dispute/contest buffer (safe with the automatic flow: a gated claim reports `compensated: false` until a follow-up payout after the window)
- **Wire the collusion flag into monitoring:** alert when `get_pair_collusion_flag` turns true, especially for recently-enrolled LPs
- **Proof of Loss:** continue requiring evidence (invoice, evidence of payment attempt) off-chain and submit its hash via `submit_claim_evidence`
- **KYC for High-Value Claims:** require identity verification for claims > pool balance threshold

#### G3. Pool Drainage / Insolvency Risk

**Description:**  
The pool accepts claims up to enrolled capacity. If claim frequency exceeds projections, the pool may become insolvent and unable to cover all claims.

**Attack Scenario:**
```
1. Pool enrolls $10M in coverage with $1M premiums
2. Unexpectedly high default rate in ILN network (10% vs expected 2%)
3. Members submit $2M in claims in one week
4. Pool only has $1M in premiums + interest ($100k) = $1.1M available
5. Pool is insolvent; claims are partially paid or stuck in queue
6. Later claims are rejected due to insufficient funds
```

**Current Mitigation:**
- ✅ **Capacity Tracking:** Pool tracks total coverage committed and premium collected
- ✅ **Funding Mechanism:** Premiums accumulate in pool for payout reserves
- ✅ **Admin Oversight:** Admin can pause claims or enroll new members if capacity is exceeded
- ✅ **Transparent Reserves:** On-chain balance is queryable (members can check solvency)
- ✅ **Solvency Circuit Breaker (Issue #826):** governance-configurable minimum reserve ratio (`set_min_reserve_ratio_bps`); when breached, the breaker trips (`SolvencyCircuitTripped`), **automatically pausing new claim payouts** until an explicit `reset_solvency_circuit`, with enrollments and premium deposits continuing so the pool can be recapitalized
- ✅ **Capital Backstop (Issue #827):** protocol-owned backstop fund (`top_up_backstop`, optional premium-fee share) extends the reserve available for claims beyond the liquid premium pool — see [ADR-013](adr/ADR-013-capital-backstop.md)
- ✅ **Reserve Ratio View (Issue #826):** `get_reserve_ratio_bps` + `get_total_reserve` make the pool's solvency against its coverage cap continuously queryable for off-chain monitoring

**Residual Risk:** ⚠️ **MEDIUM**
- Circuit breaker default is `0` (disabled) — it only engages when governance arms a threshold
- Backstop is capped by what governance/fees seed it with; it mitigates but does not eliminate insolvency risk
- Premium rates may still be too low to cover expected default rates
- No automatic payout *reduction* (the breaker pauses entirely instead); no pro-rata smoothing across simultaneous defaults

**Recommendation:**
- **Arm the breaker on mainnet** at a target reserve ratio and include `get_reserve_ratio_bps` / `is_solvency_circuit_open` in live monitoring
- **Seed the backstop** from a protocol fee share and monitor `get_backstop_balance` alongside it
- **Dynamic Premium Adjustment:** Link premium_rate_bps to pool utilization ratio:
  - If utilization > 80%, increase premiums 10-20% automatically
  - If utilization < 20%, decrease premiums to attract members
- **Claim Prioritization:** Implement priority queue:
  - Small claims (<$10k) processed immediately
  - Large claims (>$100k) queued and processed over time
  - First-in-first-out or pro-rata payout if insolvent
- **Insurance Reserve Requirement:** Admin must maintain minimum reserve (e.g., 50% of enrolled coverage)
- **Clear Communication:** Publish pool solvency ratio to members, warn if approaching danger zone

---

### H. DISTRIBUTION CONTRACT THREATS (v2.0 New)

#### H1. Accrual Authorization Bypass

**Description:**  
The `iln_distribution` contract has `accrue_lp()` and `accrue_settlement()` functions that update participant balances. Only the `invoice_liquidity` contract should call these.

**Attack Scenario:**
```
1. Attacker deploys malicious contract that mimics invoice_liquidity
2. Attacker calls `accrue_lp(lp_address, huge_amount)` directly
3. Attacker accrues fake yield for themselves or others
4. When `claim_tokens()` is called, attacker withdraws unearned rewards
```

**Current Mitigation:**
- ✅ **ILN Contract-Only Check:** `accrue_lp()` and `accrue_settlement()` verify caller is the ILN contract via `require_auth()`
- ✅ **Cross-Contract Authorization:** Storage tracks the authorized ILN contract address
- ✅ **Event Logging:** All accrual is logged on-chain for off-chain verification

**Code Evidence:** [iln_distribution/src/lib.rs](contracts/iln_distribution/src/lib.rs#L119)
```rust
pub fn accrue_lp(env: Env, lp: Address, amount_usdc_equivalent: i128) {
    let iln_contract = storage::get_iln_contract(&env);
    iln_contract.require_auth();  // Only ILN contract can call
    // ...
}
```

**Residual Risk:** ✅ **LOW**
- Authorization check enforced before any state mutation
- Cross-contract boundary is properly guarded
- No bypass via governance (reward rates are governance-settable, not accrual amounts)

**Recommendation:**
- Ensure ILN contract address is set at initialization and immutable (no uncontrolled writes)
- Document that distribution contract initialization must specify correct ILN address
- Monitor on-chain events for anomalies (accruals from unexpected addresses if auth is bypassed)

#### H2. Reward Claim Double-Spend

**Description:**  
A participant with accumulated rewards calls `claim_tokens()` to withdraw. If `claim_tokens()` doesn't properly reset their balance, they could call it again and claim twice.

**Attack Scenario:**
```
1. Alice accrues 1000 tokens via LP funding
2. Alice calls claim_tokens() and receives 1000 tokens
3. Alice calls claim_tokens() again
4. If balance wasn't zeroed, Alice claims 1000 again (double-spend)
```

**Current Mitigation:**
- ✅ **Balance Reset:** `claim_tokens()` zeroes the participant's balance **before** minting tokens
- ✅ **Mint Authorization:** Only distribution contract can mint (SAC admin set to distribution contract)

**Code Evidence:** [iln_distribution/src/lib.rs](contracts/iln_distribution/src/lib.rs#L175)
```rust
pub fn claim_tokens(env: Env, claimer: Address) -> i128 {
    let accrual = storage::get_accrual(&env, &claimer);
    storage::set_accrual(&env, &claimer, 0);  // Zero BEFORE minting
    // ... mint accrual amount to claimer ...
}
```

**Residual Risk:** ✅ **LOW**
- Balance is cleared before external token operation (checks-effects-interactions)
- Claim function verifies claimer signature (prevents unauthorized claims)

**Recommendation:**
- Confirm governance token SAC has distribution contract as sole admin
- Emit event on every claim for off-chain audit trail
- Test claim-twice scenario in unit tests

#### H3. Reward Rate Manipulation

**Description:**  
Governance can update `lp_reward_rate`, `freelancer_reward_rate`, and `payer_reward_rate`. Unfair rates could incentivize or disincentivize participation.

**Attack Scenario:**
```
1. Governance sets lp_reward_rate = 0 (no LP rewards)
2. All LPs stop funding invoices
3. Protocol becomes non-functional
4. Governance resets rate, but damage is done
```

**Current Mitigation:**
- ✅ **Public Events:** Rate changes emit events for monitoring
- ✅ **Governance Control:** Only ILN contract (via governance) can update rates
- ✅ **Transparent Rates:** Rates are queryable on-chain

**Residual Risk:** ⚠️ **MEDIUM**
- No bounds checking on reward rates (could be set to 0 or absurdly high)
- No rate-change cooldown (governance can change rates multiple times per day)
- Economic impact of rate changes may not be immediately visible
- Reward distribution formula not documented in code (off-chain knowledge risk)

**Recommendation:**
- **Governance Policy:** Establish safe reward rate ranges
- **Code Enhancement:** Add bounds checking (e.g., rewards must be > 0 and < max_reward_rate)
- **Operational Practice:** Require testnet dry-run before mainnet rate changes
- **Documentation:** Publish reward distribution model and expected annual yields
- **Monitoring:** Dashboard tracking reward rates and total accrued vs. claimed

#### H4. Governance Token Mint Authority Compromise

**Description:**  
The distribution contract must be the admin of the governance token (SAC) to mint rewards. If governance token admin is compromised, unlimited tokens can be minted.

**Current Mitigation:**
- ✅ **Admin Authority:** Only distribution contract can mint (set at token initialization)
- ✅ **Immutable Admin:** Once set, token admin cannot be changed without upgrade
- ✅ **Public Audit:** Token admin address is queryable on-chain

**Residual Risk:** ⚠️ **MEDIUM**
- Distribution contract itself must be secure (any bug in it could leak mint authority)
- If distribution contract is compromised, token can be inflated infinitely
- Governance token inflation devalues all existing holders' stakes

**Recommendation:**
- **Pre-Launch Audit:** Conduct security audit of distribution contract specifically
- **Governance Policy:** Establish mint rate limits (max tokens per ledger)
- **Monitoring:** Track total token supply; alert on unexpected increases
- **Safeguards:** Consider implementing hard cap on total supply (immutable constant)

---

## Summary of Mitigations & Residual Risks

| Threat | Severity | Mitigation | Residual Risk |
|--------|----------|-----------|---------------|
| **Reentrancy (Token Transfers)** | HIGH | Checks-effects-interactions pattern | LOW-MEDIUM (token hooks unpredictable) |
| **Reentrancy (Dispute Resolution)** | HIGH | Admin-only access | MEDIUM (if admin is DAO) |
| **LP Queue Front-Running** | MEDIUM | Reputation snapshot, Stellar mempool randomness, randomized PRNG tie-breaking (Issue #708) | LOW (tie-breaking no longer predictable) |
| **`fund_invoice()` Front-Running (non-queue)** | LOW | No price-sensitive state to extract from; no EVM-style proposer-controlled reordering on Stellar (Issue #707) | LOW |
| **Discount Rate Manipulation** | MEDIUM | Rate frozen at submission | LOW (no benefit to attacker) |
| **Timestamp Manipulation** | MEDIUM | Consensus-based timestamp, ledger sequences | MEDIUM (51% validator attack) |
| **Appeal Window Bypass** | MEDIUM | Ledger sequence windows | LOW (cryptographically protected) |
| **Reputation Sybil Attack** | MEDIUM | Decay mechanism, admin oversight | MEDIUM (no external oracle) |
| **Missing Credit Oracle** | HIGH | None (design limitation) | HIGH (LPs assume all risk) |
| **Payer-Verification Oracle Manipulation (D3)** | HIGH | Opt-in verification, freshness window, `pause()`, governance-gated registration | HIGH (no oracle stake/quorum, no max invoice cap — see [oracle-attack-economics.md](oracle-attack-economics.md)) |
| **Admin Key Compromise** | CRITICAL | Rate-limited functions, public events, governance oracle registry | MEDIUM (rate limits provide delay; recommend multi-sig governance) |
| **Parameter Misconfiguration** | MEDIUM | Bounds checks (partial) | MEDIUM (incomplete validation; bounds needed on decay_rate_bps, high_rep_threshold) |
| **Payer Verification Oracle Manipulation** | HIGH | Opt-in oracle, freshness window, governance control | MEDIUM (oracle-specific risks in oracle-attack-economics.md) |
| **Token Transfer Failure** | MEDIUM | Atomic transactions | LOW (Soroban guarantees) |
| **Token Allowance Missing** | LOW | Documentation, error handling | LOW (UX issue, not security) |
| **Partial Token Transfer** | MEDIUM | Token specification, admin control | LOW-MEDIUM (requires rogue token) |
| **Premium Manipulation** | MEDIUM | Configurable rates, public events | MEDIUM (no bounds checking) |
| **Claim Fraud & Moral Hazard** | HIGH | Enrollment KYC, invoice immutability, admin review | HIGH (manual verification required) |
| **Pool Drainage / Insolvency** | HIGH | Capacity tracking, transparent reserves | HIGH (no automatic safeguards) |
| **Distribution Accrual Authorization Bypass** | HIGH | ILN-contract-only checks, cross-contract auth | LOW (guarded; requires ILN compromise) |
| **Reward Claim Double-Spend** | HIGH | Balance reset before mint, checks-effects-interactions | LOW (guarded by Soroban atomicity) |
| **Reward Rate Manipulation** | MEDIUM | Public events, governance control | MEDIUM (no bounds on rates, immediate effect) |
| **Governance Token Mint Authority** | HIGH | Distribution contract as sole admin | MEDIUM (depends on distribution contract security) |

---

## Risk Recommendations (Priority Order)

### 🔴 Critical (Pre-Audit)
1. **Implement Multi-Sig Admin** (2-of-3 minimum)  
   - Reduces single-point-of-failure risk from CRITICAL to MEDIUM
   - Requires Soroban multi-sig contract integration

2. **Add Time-Locks for Parameter Changes**  
   - Prevents instant governance attacks
   - Allows community reaction time to malicious changes

3. **Validate All Configuration Parameters**  
   - Enforce bounds: `high_rep_threshold` in 0-100, `decay_rate_bps` <= 500
   - Catch configuration bugs before deployment

### 🟡 High (Before Mainnet)
4. **Implement Reentrancy Guard State Flag**  
   - Add `is_locked` boolean to prevent nested external calls
   - Apply to `fund_invoice()`, `claim_default()`, `resolve_*()` functions

5. **Document Trusted Assumptions**  
   - Publish security model: "LPs assume 100% credit risk"
   - Clarify Stellar validator assumptions, token standards

6. **Conduct Token Audit**  
   - Verify USDC, EURC implementations for unexpected callbacks
   - Establish token whitelist policy

### 🟢 Medium (Post-Launch)
7. **Implement Reputation Audit Trail**  
   - Emit event for each reputation change (not just on demand)
   - Enable off-chain verification and anomaly detection

8. ✅ **Add Randomness to Queue Tie-Breaking** — Resolved (Issue #708)
   - Soroban's `env.prng()` (network-seeded CSPRNG, available since well
     before this workspace's soroban-sdk 27.x) now selects uniformly among
     all top-score-tied LPs in `resolve_fund_queue`, instead of always the
     first to join. See B1's residual-risk entry above.

9. **Publish LP Risk Management Guide**  
   - Recommend KYC procedures, portfolio diversification, default rate monitoring
   - Educate users on credit risk assumptions

---

## Future Upgrade Considerations

### v2.0 Completed
- ✅ **Payer-Verification Oracles:** Governance-controlled oracle registry for payer creditworthiness checks
- ✅ **Insurance Pool:** Standalone contract for default protection and premium collection
- ✅ **Distribution Contract:** Yield and incentive distribution with cross-contract authorization
- ✅ **Governance Contract:** On-chain proposals, voting, delegation, execution with timelock

### Still Needed
- **Decentralized Governance:** Full DAO-based admin (currently governance contract controls some parameters, but single multisig still recommended for safety). See [ADR-012](adr/ADR-012-governance-multisig-handoff.md) for the phased handoff plan and trigger criteria for this transition.
- **Staking-Based Governance:** Replace token voting with escrow/staking model to prevent flash-loan attacks (Issue #738)
- **Automated Parameter Adjustment:** Formula-based reputation thresholds and reward rates based on network statistics
- **Rollback Mechanism:** Snapshot and recovery points for emergency scenarios
- **Multi-Oracle Aggregation:** Consensus across multiple payer-verification oracles for oracle-manipulation resilience
- **On-Chain Proof of Creditworthiness:** Zero-knowledge proofs or cryptographic delegation (future Stellar identity protocols)

---

## Conclusion

The ILN contract has **sound architectural foundations** with proper state management (checks-effects-interactions) and access control. However, **critical risks remain**:

1. **Admin single point of failure** – must be mitigated before mainnet. See
   [disaster-recovery-multisig-signers.md](disaster-recovery-multisig-signers.md) for
   the recovery runbook covering a lost or compromised signer majority, and
   [ADR-012](adr/ADR-012-governance-multisig-handoff.md) for the longer-term plan to
   move final authority to governance.
2. **No external credit oracle** – by design, but LPs must understand full risk
3. **Parameter misconfiguration possible** – needs tighter validation
4. **Reentrancy guards incomplete** – consider state flags for defense-in-depth

**Recommendation:** Conduct formal security audit focusing on:
- Admin key management and governance upgrade path
- Reentrancy in complex scenarios (multi-token, distribution integration)
- Parameter validation and safe configuration bounds
- External token behavior (USDC/EURC callback hooks, if any)

---

## Reviewer Sign-Off

This document has been reviewed by a team member independent of the original threat model authorship. The review confirmed that all major architectural components added since v1.0 (Multi-Sig Admin, Oracle Registry, Governance Contract, Distribution Contract, Insurance Pool, and MEV mitigations) have been analyzed against the original threat model, and residual risks are documented and accepted.

**Original Author:** Security Review Team  
**Reviewer:** Ada Girly (Ada-Girly881)  
**Review Date:** 2026-09-24  
**Review Findings:**
- ✅ v2.0 updates accurately reflect implemented components (Multi-Sig, Oracle Registry, Governance, Distribution, Insurance Pool)
- ✅ Flash-loan risk (E3) fixed via checkpoint-gated voting (Issue #805); residual documented in E3 and governance-security-summary §3.2
- ✅ No timelock on governance execution (ADR-005) correctly noted as accepted risk with veto as interim mitigation
- ✅ Quorum consistency now reads the stored `GovTokenTotalSupply` (Issue #808); tracked-supply staleness residual documented, veto retained until live supply reads exist
- ✅ All residual risks cross-referenced to primary mitigation sources (ADRs, governance docs, disaster-recovery runbook)
- ✅ Parameter validation gaps (decay_rate_bps, high_rep_threshold) tracked as separate audit-prep issues (#692-693)
- ✅ Distribution contract mint authority and event coverage concerns documented (Section H, Issue #691)

**Approval:** Threat model v2.0 is approved for pre-audit review. No structural changes needed; all identified gaps are tracked as separate issues with owners and dates.
