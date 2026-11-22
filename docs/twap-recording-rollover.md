# TWAP Recording & Window Rollover

**Issue:** #1192 — Audit TWAP recording and window rollover
**Scope:** `contracts/invoice_liquidity/src/twap.rs` and its consumers in
`contracts/invoice_liquidity/src/oracle_registry.rs`.
**Status:** Behavior verified against the shipped code. This document records
what the code does today; it does **not** assert that a vulnerability exists.
Verified findings and residual risk are separated (§6 / §7).

---

## 1. Scope & module map

Two TWAP accumulators exist in this crate and must not be conflated:

| Module | Shape | Production role |
|---|---|---|
| [`twap.rs`](../contracts/invoice_liquidity/src/twap.rs) | Pure, stateless functions over a `Vec<TwapSample>` | **This is the price path.** `oracle_registry::{record_twap_sample, get_twap_price}` persist and read the buffer, and `get_twap_aware_price` routes numeric `Price` reads through it. |
| [`twap_accumulator.rs`](../contracts/invoice_liquidity/src/twap_accumulator.rs) | Stateful accumulator (`record_observation` / `get_twap`, its own ledger-entry key) | Target of [`formal-verification-twap.md`](./formal-verification-twap.md); not invoked by the contract's price entry points. Its invariants (I1–I4) are **separate** from the INV-TWAP set below. |

This document covers `twap.rs` only. In particular the saturating-arithmetic
claim in `formal-verification-twap.md` §6/A4 applies to
`twap_accumulator.rs` and does **not** hold for every operation in `twap.rs`
(see VF-4).

Nothing in `twap.rs` runs unless a feed explicitly opts in
(`set_twap_enabled`); the default remains raw spot.

---

## 2. Recording path — `oracle_registry::record_twap_sample`

```
record_twap_sample(env, feed_type, token, price):
    require_admin(env)?                      // admin / governance only
    if price <= 0: return InvalidAmount      // lower bound only
    key = DataKey::TwapSamples(feed_type, token)
    samples = persistent.get(key).unwrap_or(Vec::new)
    twap::push_sample(&mut samples,
                      TwapSample { timestamp: env.ledger().timestamp(), price },
                      twap::MAX_TWAP_SAMPLES)
    persistent.set(key, &samples)
```

- **Authorization:** admin only (`require_admin`), the same governance-as-admin
  convention used across the registry.
- **Rate limiting:** deliberately exempt (Issue #859). The sampling cadence is
  the anti-manipulation property; capping writes would leave the window sparse
  and easy to skew.
- **Timestamp source:** the ledger timestamp at call time. Within one ledger
  the timestamp is constant, so two writes in the same ledger share a
  timestamp (see §5.2).
- **Value bound:** only `price <= 0` is rejected. There is **no upper bound**
  (see RR-1).

---

## 3. Read path — `oracle_registry::get_twap_price`

```
get_twap_price(env, feed_type, token):
    require_healthy_price_feed(env, feed_type, token).ok()?  // circuit + staleness
    samples = persistent.get(DataKey::TwapSamples(feed_type, token)).unwrap_or(empty)
    if samples.len() < get_min_twap_observations(env):        // default 2
        emit TwapInsufficientData
        return None
    window_seconds = get_twap_window_ledgers(env) * LEDGER_SECONDS   // 5s/ledger
    window_start   = now - window_seconds
    twap::twap_average(&samples, window_start, now)
```

- The **window is global** — a single `DataKey::TwapWindowLedgers` and a single
  `DataKey::MinTwapObservations`, not per feed/token (see RR-4).
- `window_start` is recomputed on **every** read, which is what makes rollover
  work without mutating the buffer (§5.5).
- The health gate collapses both a stale snapshot and an open circuit to `None`
  (the `Option` signature has no error channel); `get_twap_aware_price`
  surfaces the specific error first and maps a remaining `None` to
  `InsufficientTwapObservations`.

---

## 4. Invariants

| Id | Statement |
|---|---|
| INV-TWAP1 | `push_sample` appends in call order and never re-sorts; `twap_average` consumes samples in storage order and assumes non-decreasing timestamps. An out-of-order pair contributes **no** weight. |
| INV-TWAP2 | The buffer is bounded by `max_samples` (`MAX_TWAP_SAMPLES` = 100 when 0), evicting the oldest-inserted first. Retention is by **count**, not age. |
| INV-TWAP3 | Only pairs overlapping `[window_start, current_time]` contribute; a partially overlapping pair is clipped to the window, carrying a boundary sample's price forward to `window_start`. |
| INV-TWAP4 | `twap_average` returns `None` when no pair overlaps the window (empty, single sample, all outside, or a degenerate `current_time <= window_start`). |
| INV-TWAP5 | Weight and accumulation updates saturate at the `i128`/`u64` bound. The **sole** non-saturating operation is the pairwise sum `s1.price + s2.price`. |
| INV-TWAP6 | `(p1 + p2) / 2` and the final `total_weighted / total_weight` truncate toward zero. |
| INV-TWAP7 | Both functions are pure: no storage, no authorization, no cross-contract calls. Persistence and access control live in the caller. |

---

## 5. Verified edge behaviors

### 5.1 Sample ordering
`push_sample` performs no ordering check and no sort. `twap_average` walks
consecutive storage-order pairs; an out-of-order pair yields
`seg_end < seg_start`, is skipped, and contributes no weight. Net effect: an
out-of-order sample is **silently ignored** rather than mis-ordering the
average. The shipped writer stamps samples with the ledger timestamp, which is
non-decreasing, so the ordering assumption holds for the production path
(RR-3).

### 5.2 Duplicate ledgers
Two samples with equal timestamps form a zero-duration pair and contribute no
weight; they are not de-duplicated and each consumes a buffer slot. Two
same-ledger writes are therefore neutral to the average but spend budget
against `MAX_TWAP_SAMPLES`.

### 5.3 Stale samples
Retention is by count, not age: a sample older than the read window stays in
storage until the 100-slot cap pushes it out. Staleness is applied **only** at
read time by `twap_average`'s window filter. A pair fully before
`window_start` is skipped; a pair that straddles `window_start` is clipped, so
the older sample's price is carried forward and participates at the window's
left edge (INV-TWAP3). This is the standard carry-forward assumption and is a
deliberate consequence of clipping, not a bug — but it means a single ancient
sample adjacent to a fresh one can carry half the influence across the clipped
window.

### 5.4 Overflow
`total_weighted`, `total_weight`, and `avg_price * duration` use
`saturating_*` and never wrap. The **pairwise sum** `s1.price + s2.price`
(→ `avg_price`) is plain arithmetic: two prices whose sum exceeds `i128::MAX`
trap (`overflow-checks = true` in the release profile and the default dev/test
profile). Because `record_twap_sample` has no upper price bound, this is
reachable by an admin writer (RR-1). The module's own comment that "integration
math mirrors the example crate exactly" holds; the example did not bound prices
either.

### 5.5 Window rollover
`get_twap_price` recomputes `window_start = now - window_ledgers * 5` per read.
As time advances the window slides forward; pairs that no longer overlap drop
out and the average shifts, with no mutation or compaction of the buffer.
Changing `TwapWindowLedgers` re-filters **all** feeds at once (the window is
global).

---

## 6. Verified findings

| Id | Finding |
|---|---|
| VF-1 | Samples are consumed in storage order and never sorted; out-of-order data is ignored, not corrupted. The production writer's non-decreasing ledger timestamps satisfy the assumption. |
| VF-2 | Duplicate-timestamp (same-ledger) samples contribute zero duration/weight and are retained, spending buffer budget without affecting the average. |
| VF-3 | No time-based eviction: stale samples persist to the count cap and are filtered only at read time. Boundary clipping carries a pre-window sample's price into the window. |
| VF-4 | Accumulation saturates, but the pairwise price sum is non-saturating and traps when two prices sum above `i128::MAX`; `record_twap_sample` does not bound price above. |
| VF-5 | Window rollover is read-time only; the buffer is never compacted by age, and the window/min-observations settings are global rather than per feed/token. |

---

## 7. Residual risk

| Id | Risk | Mitigation today |
|---|---|---|
| RR-1 | `record_twap_sample` bounds price only below (`> 0`); two near-`i128::MAX` prices trap the non-saturating pair sum (VF-4). | Admin-only writer; a real price feed is nowhere near `i128::MAX`. A future hardening could saturate the pair sum or add an upper price bound. |
| RR-2 | Count-not-age retention keeps up to 100 samples regardless of window size; a small window holds many samples older than the window. | Bounded storage; read-time filtering keeps stale samples out of the weighted sum except as clipping boundaries. |
| RR-3 | No ordering or de-duplication guard in `push_sample`; correctness relies on callers feeding non-decreasing, distinct timestamps. | Holds for the shipped writer (`record_twap_sample` uses the ledger timestamp). |
| RR-4 | The window and `min_observations` are **global**, not per feed/token; `set_min_twap_observations` accepts any `u32` with no upper bound (and is not rate-limited). A value above `MAX_TWAP_SAMPLES` (100) makes `get_twap_price` permanently return `None` (`InsufficientTwapObservations`). | Admin-only. No automated guard against a misconfiguration that silently disables every TWAP feed. |
| RR-5 | `record_twap_sample` writes `DataKey::TwapSamples` without extending the entry's TTL; retention depends on keeper cadence (and any separate TTL maintenance), not on this function. | Keeper is expected to re-record continuously; a stalled keeper both stops sampling and risks entry expiry. |

---

## 8. Test coverage

Pinned by the tests added to the inline `mod tests` in
[`twap.rs`](../contracts/invoice_liquidity/src/twap.rs) (Issue #1192):

| Test | Invariant / behavior |
|---|---|
| `test_push_sample_preserves_insertion_order_not_sorted` | INV-TWAP1 (no sort) |
| `test_out_of_order_pair_contributes_no_weight` | INV-TWAP1 (out-of-order ignored) |
| `test_push_sample_zero_cap_uses_default_bound` | INV-TWAP2 (default cap) |
| `test_duplicate_ledger_timestamp_contributes_no_weight` | INV-TWAP2 / INV-TWAP6 (§5.2) |
| `test_stale_pair_before_window_is_skipped` | INV-TWAP3 (§5.3) |
| `test_stale_boundary_sample_is_carried_into_window` | INV-TWAP3 (carry-forward clipping) |
| `test_accumulation_saturates_instead_of_panicking` | INV-TWAP5 (saturating accumulation) |
| `test_pairwise_price_sum_overflows_above_i128_max` | INV-TWAP5 exception / RR-1 (traps) |
| `test_window_rollover_drops_aged_pairs` | INV-TWAP3 / INV-TWAP4 (§5.5) |

Existing coverage in `tests_oracle_registry.rs` exercises the wiring
(default-off, enabled windowed average, window bounds, health gate → `None`).

---

## 9. Operator guidance

- Record samples on a steady cadence well inside the configured window so the
  window stays dense; the cadence is the anti-manipulation property.
- Do not rely on stale samples being dropped from storage — they are filtered
  only at read time and still occupy the 100-slot budget.
- Keep `min_observations` ≤ `MAX_TWAP_SAMPLES` (100); a larger value silently
  disables the feed's TWAP path (RR-4).
- Treat the window and `min_observations` as protocol-wide settings: changing
  either affects every TWAP-enabled feed immediately.
