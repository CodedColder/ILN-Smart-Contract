# Verified-Price Source Precedence

**Status:** Documented and test-backed (Issue #1193).

**Scope:** `contracts/invoice_liquidity/src/oracle_registry.rs` and the tests that
exercise it (`tests_oracle_registry.rs`, `tests_price_deviation.rs`).

**Related:** [`oracle-design.md`](oracle-design.md) (per-mechanism rationale),
[`formal-verification-twap.md`](formal-verification-twap.md) (the *separate*
stateful accumulator), [`oracle-attack-economics.md`](oracle-attack-economics.md)
(the economic argument behind the TWAP window bounds).

---

## 1. What this document pins down

`oracle_registry.rs` can answer a price in more than one way — a windowed TWAP
average, a cross-validated median of one or more direct price-feeder calls, or
(a boolean feed) a multi-feeder identity quorum versus a single resolved oracle
— and several of those layers have their own failure/fallback behaviour. Each
mechanism is described on its own in [`oracle-design.md`](oracle-design.md);
this document states the **precedence** between them — which layer wins, which
layer is consulted only as a fallback, and which error is returned when more
than one layer would reject.

It follows the audit convention of separating **verified findings** (behaviour
backed by a code path and a compiled test) from **residual risk** (behaviour
that is accepted and not enforced on-chain).

> Nothing here changes behaviour. No entry point, storage key, event, or error
> was modified — this is a documentation and test addition only (per the
> Issue's "preserve existing API and storage compatibility").

---

## 2. The three independent sources (they never alias)

Three distinct registries live on three distinct `DataKey`s, so a write to one
can never change the input to another (this is the #1190 "list separation"
invariant; see [`oracle-design.md`](oracle-design.md) § *Registration, Removal &
Feeder-Removal Invariants*):

| Registry | Storage keys | Sole consumer |
|---|---|---|
| Direct price sources | `PriceSources(feed)` (persistent list) | `get_verified_price` / `get_twap_aware_price` (spot path) |
| TWAP samples | `TwapSamples(feed, token)` + `TwapEnabled(feed)` | `get_twap_price` (TWAP path) |
| Identity feeders | `IdentityOracles(feed)` (quorum) **and** `TokenOracle(feed, token)` / `OracleRegistry(feed)` / `config.price_oracle` (single-oracle chain) | `evaluate_identity_verification` |

Data sources and authorization:

* **Direct price sources** — registered by admin via `add_price_source` /
  `remove_price_source` (admin-gated, rate-limited, capped at
  `MAX_PRICE_SOURCES_PER_FEED`). Each is queried dynamically as
  `get_price(token) -> i128` through the **non-panicking** `query_price`; a
  source that reverts, traps, or returns an undecodable payload is dropped from
  the sample rather than failing the read.
* **TWAP samples** — pushed by an admin/keeper via `record_twap_sample`
  (admin-gated, deliberately **not** rate-limited; the sampling cadence *is* the
  protection). The window is global (`get_twap_window_ledgers`, default 720
  ledgers) and so is `get_min_twap_observations` (default 2); only the enable
  flag `is_twap_enabled(feed)` is per feed.
* **Identity feeders** — the quorum list is set by admin via
  `add_identity_oracle` / `remove_identity_oracle` with `set_identity_quorum`
  (admin-gated, rate-limited, capped at `MAX_IDENTITY_ORACLES_PER_FEED`); the
  single-oracle chain is set by admin via `register_oracle` /
  `register_token_oracle` / `set_price_oracle`. Feeders are classified with the
  non-panicking `try_invoke_contract` ("disagreement fails closed").

---

## 3. Numeric `Price` read — precedence in `get_verified_price`

`get_verified_price` is the public numeric read (exposed as the contract
entry point of the same name). Layers, **highest precedence first**:

| # | Layer | Condition to *win* | Error / note |
|---|---|---|---|
| 1 | **Health gate** (`require_healthy_price_feed`) | circuit not tripped **and** last recorded snapshot not stale (no snapshot → passes, fail-open) | `OracleCircuitOpen` if tripped; else `OracleDataStale`. Fails closed **before any layer below.** |
| 2 | **TWAP** (`get_twap_price`) | `is_twap_enabled(feed)` **and** ≥ `get_min_twap_observations()` samples **and** an in-window pair overlaps (`twap_average` returns `Some`) | returns the windowed average. `get_twap_price` re-runs the health gate itself and collapses a rejection to `None`. |
| 3 | **Spot (direct feeds)** | layer 2 did not return a value (TWAP disabled, or enabled but unavailable) | `NoPriceSource` if none answered; a single answer returned **unchecked**; ≥2 → median with outlier rejection. |
| 3a | — zero priced sources | | `NoPriceSource` |
| 3b | — exactly one priced source | | returned unchecked (documented single-source risk) |
| 3c | — ≥2 priced sources | median of all, drop any deviating > `get_max_price_deviation_bps()` (default 500 bps), return median of survivors | `AllPriceSourcesRejected` if none survive |

Key precedence consequences (all verified):

* **The health gate is outermost.** A tripped circuit or a recorded-stale
  snapshot rejects the read even when a perfectly good TWAP window *and* spot
  sources exist — neither is consulted. (Test:
  `test_1193_health_gate_precedes_twap_and_spot`; and the pre-existing
  `test_get_verified_price_rejects_tripped_circuit` /
  `test_get_verified_price_rejects_stale_health`.)
* **TWAP beats spot when available.** With TWAP enabled and enough in-window
  samples, the direct price sources are not consulted at all.
  (Test: `test_1193_twap_precedes_direct_price_sources`.)
* **`get_verified_price` has a *fallback to spot*.** When TWAP is enabled but
  cannot produce a value (too few samples / no in-window pair), the read
  **silently falls through** to the spot path so a freshly-enabled feed stays
  live while keepers backfill. This is the "fallback value" the Issue names, and
  it is by design ("documented, not silent — callers can distinguish it via
  `get_twap_price` returning `None`"). (Test:
  `test_1193_twap_enabled_insufficient_samples_falls_back_to_spot`; and the
  pre-existing `test_twap_disabled_feed_uses_spot_even_with_samples`,
  `test_twap_enabled_feed_reads_windowed_average`.)

---

## 4. Strict TWAP read — `get_twap_aware_price` (no spot fallback)

`get_twap_aware_price` is the strict numeric read: **it does not fall back to
spot.**

| TWAP state | Behaviour |
|---|---|
| disabled | delegates to `get_verified_price` (which may itself serve spot) |
| enabled | health gate errors propagate (`OracleCircuitOpen` / `OracleDataStale`); then `get_twap_price` — if `Some`, return it; if `None`, `Err(InsufficientTwapObservations)` |

So the two numeric entry points differ **only** in the enabled-but-unavailable
case: `get_verified_price` falls back to spot; `get_twap_aware_price` fails with
`InsufficientTwapObservations`. A caller that requires an *actually-TWAP* price
must use `get_twap_aware_price`. (Test:
`test_1193_twap_aware_price_has_no_spot_fallback`.)

> **VF-2 (doc defect):** the inline doc comment on `get_twap_aware_price`
> currently claims that when fewer than two in-window samples exist it
> "otherwise falls back to the spot path". The code does **not** — it returns
> `Err(InsufficientTwapObservations)`. The code is authoritative; the comment is
> stale. This document records the *observed* behaviour.

---

## 5. Boolean `Identity` gate — precedence in `evaluate_identity_verification`

`evaluate_identity_verification` is the single implementation behind both
`fund_invoice`'s `require_oracle_verification` block and the permissionless
`check_oracle_verification` diagnostic. Layers, **highest precedence first**:

| # | Layer | Outcome |
|---|---|---|
| 1 | **Identity quorum** (`check_identity_quorum`) — only when `IdentityOracles(Identity)` is **non-empty** | `Verified` (≥ `get_identity_quorum()` feeders fresh **and** `is_verified`) → gate `Verified`; otherwise emit `oracle_feeder_disagreement` → `QuorumDisagreement`. **Short-circuits the whole single-oracle chain below.** |
| 1b | quorum list **empty** | `NotConfigured` → fall through to layer 2 (unchanged legacy path) |
| 2 | **Single-oracle resolution** (`resolve_oracle_for_verification`, circuit-aware) | `TokenOracle(feed, token)` → `OracleRegistry(feed)` → (Identity only) `config.price_oracle`; skips the address recorded as having tripped the breaker |
| 2a | every candidate excluded (tripped) | `CircuitOpen` |
| 2b | nothing registered | `Verified` (legacy fail-open) |
| 2c | a candidate resolves | invoke its `get_payer_data` (panicking `invoke_contract`) and continue to layer 3 |
| 3 | **Post-response checks** (single-oracle path only) | `TimestampRegression` (#1052) → `DuplicateUpdate` (#1053) → `record_oracle_health` (#532) → `Stale` (#1050, emits `oracle_stale_data_rejected`) → `Unverified` (`is_verified == false`) → `Verified` |

**Identity error precedence** for the funding path (maps to `ContractError`
in `fund_invoice`):

```
QuorumDisagreement        -> PayerUnverified
CircuitOpen               -> OracleCircuitOpen
TimestampRegression       -> OracleTimestampRegression
DuplicateUpdate           -> OracleDuplicateUpdate
Stale                     -> OracleDataStale
Unverified               -> PayerUnverified
```

Key precedence consequences (all verified):

* **A non-empty quorum short-circuits the single-oracle chain.** A satisfied
  quorum makes the gate `Verified` **without ever consulting** the resolved
  single oracle — even one that would have returned `is_verified = false`.
  (Test: `test_1193_identity_quorum_verified_precedes_single_oracle`.)
* **Quorum disagreement fails closed and does *not* fall back** to a valid,
  verified single oracle. (Test:
  `test_1193_identity_quorum_disagreement_precedes_single_oracle`; and the
  pre-existing `test_identity_quorum_disagreement_fails_closed`.)
* **Removal/empty-list fallback is the only path back to the single oracle**
  (pre-existing `test_empty_identity_quorum_list_uses_single_oracle_path`).

---

## 6. Authorization rules (summary)

| Entry point | Access | Rate-limited |
|---|---|---|
| `get_verified_price`, `get_twap_price`, `get_twap_aware_price`, `check_oracle_verification`, `check_identity_quorum_health`, `get_oracle_status` | Anyone (read-only) | no |
| `add_price_source` / `remove_price_source` | Admin | yes |
| `register_oracle` / `register_token_oracle` / `remove_oracle` / `remove_token_oracle` | Admin | yes (+ interface-version handshake on register) |
| `add_identity_oracle` / `remove_identity_oracle` / `set_identity_quorum` | Admin | yes (no handshake — feeders are queried dynamically & non-panicking) |
| `set_twap_enabled`, `set_twap_window` | Admin | yes |
| `record_twap_sample` | Admin | **no** (documented exemption — sampling cadence is the protection) |
| `set_min_twap_observations` | Admin | **no** (unbounded; see RR-4) |

In production the stored admin is the governance contract, so each mutation is a
proposal (see [`oracle-design.md`](oracle-design.md)).

---

## 7. Verified findings (VF)

| ID | Finding | Backed by |
|---|---|---|
| VF-1 | `get_verified_price` precedence is health gate → TWAP (if enabled & available) → spot median with outlier rejection; a single spot source is returned unchecked. | `get_verified_price`; `test_1193_twap_precedes_direct_price_sources`, `test_1193_health_gate_precedes_twap_and_spot`, `test_get_verified_price_single_source_returns_unchecked` |
| VF-2 | `get_twap_aware_price` does **not** silently fall back to spot; it errors `InsufficientTwapObservations`. Its doc comment contradicts this (doc defect). | `get_twap_aware_price`; `test_1193_twap_aware_price_has_no_spot_fallback` |
| VF-3 | `get_verified_price` (TWAP enabled, unavailable) *does* fall back to spot. | `test_1193_twap_enabled_insufficient_samples_falls_back_to_spot`, `test_twap_disabled_feed_uses_spot_even_with_samples` |
| VF-4 | A non-empty Identity quorum short-circuits the single-oracle chain (including its circuit/timestamp/duplicate checks), in both the verified and the disagreement direction. | `evaluate_identity_verification`; `test_1193_identity_quorum_verified_precedes_single_oracle`, `test_1193_identity_quorum_disagreement_precedes_single_oracle` |
| VF-5 | Identity error precedence is quorum → circuit → regression → duplicate → stale → unverified. | `evaluate_identity_verification` (order preserved from `fund_invoice`), documented in its doc comment |

---

## 8. Residual risk (RR)

| ID | Residual risk | Why it is not enforced on-chain |
|---|---|---|
| RR-1 | A single direct price source is trusted **unchecked**. | Nothing to cross-check against; needs ≥2 independent, non-colluding sources (≥3 to single out one outlier). See `oracle-design.md` § "The single-source risk is explicit, not silent". |
| RR-2 | `get_verified_price` with TWAP enabled but insufficient samples **silently** serves spot. | Deliberate "stay live" fallback; a strict-TWAP caller must use `get_twap_aware_price` (VF-2). The distinction is observable only by also calling `get_twap_price`. |
| RR-3 | The Identity quorum path never consults the single-oracle circuit breaker. | The breaker models a *single* oracle per `(feed, token)`; in quorum mode there is no single oracle to trip. An open single-oracle circuit therefore does not stop quorum-gated funding. Remediation is governance (adjust the quorum / feeders). |
| RR-4 | The TWAP window and `get_min_twap_observations` are **global**, not per feed/token, and `set_min_twap_observations` is unbounded (no admin rate-limit, no upper bound). | A governance misconfiguration affects every feed; out-of-range windows are rejected by `set_twap_window` but `min_twap_observations` is not range-checked. |
| RR-5 | A compromised **majority** of Identity feeders satisfies the quorum, indistinguishably from honest agreement. | Only governance `remove_identity_oracle` can remediate; not catchable on-chain. |

---

## 9. Test coverage map

Added for Issue #1193 (all in `tests_oracle_registry.rs`):

| Test | Precedence boundary it pins |
|---|---|
| `test_1193_twap_precedes_direct_price_sources` | TWAP > direct price sources |
| `test_1193_twap_enabled_insufficient_samples_falls_back_to_spot` | fallback value: TWAP-unavailable → spot |
| `test_1193_twap_aware_price_has_no_spot_fallback` | strict TWAP read: no silent spot fallback |
| `test_1193_health_gate_precedes_twap_and_spot` | health gate > {TWAP, spot} |
| `test_1193_identity_quorum_verified_precedes_single_oracle` | quorum (verified) > single-oracle chain |
| `test_1193_identity_quorum_disagreement_precedes_single_oracle` | quorum (disagreement) fails closed, no single-oracle fallback |

Pre-existing tests that already pin adjacent behaviour:
`test_twap_disabled_by_default_spot_unchanged`,
`test_twap_enabled_feed_reads_windowed_average`,
`test_twap_disabled_feed_uses_spot_even_with_samples`,
`test_get_twap_price_none_when_health_degraded`,
`test_get_verified_price_rejects_tripped_circuit`,
`test_get_verified_price_rejects_stale_health`,
`test_identity_quorum_disagreement_fails_closed`,
`test_empty_identity_quorum_list_uses_single_oracle_path`.

---

## 10. Operator guidance

* A numeric price read returns the **TWAP average** whenever
  `is_twap_enabled(feed)` is set and enough in-window samples exist; otherwise
  it returns the **spot median**. If you need to be certain a value is a TWAP
  average and not a spot fallback, call `get_twap_price` (it returns `None` on
  fallback) or use the strict `get_twap_aware_price`.
* Enabling TWAP on the `Identity` feed is stored but has **no effect** — TWAP
  applies to numeric `Price` reads only.
* Enable TWAP only once keepers are reliably pushing samples: while samples
  backfill, `get_verified_price` serves spot (RR-2).
* To activate outlier protection, register **at least two** independent price
  sources (RR-1), and prefer **three** so a single outlier can be singled out.
* In quorum mode the single-oracle circuit breaker is not consulted (RR-3);
  make a feeder unhealthy by ensuring the quorum tolerates it (`N-of-M`).
