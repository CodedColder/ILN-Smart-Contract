# Audit: top-payer ranking determinism

Scope: `contracts/invoice_liquidity/src/top_payers.rs`, its single caller
`set_payer_score` / `get_payer_score` in `invoice.rs`, and the public
`get_top_payers`. Tests: `tests_top_payers.rs`.

## Data model

- `DataKey::TopPayersHeap`: persistent `Vec<TopPayerEntry { address, score }>`,
  at most `TOP_PAYERS_CAPACITY` (50) unique addresses, stored as a binary
  min-heap whose root is the next eviction victim.
- Source of scores: the payer reputation score (`0..=100` via
  `set_payer_score`, capped there). `update_top_payers_on_score_change` itself
  accepts any `u32`.
- No authorization: the module is internal. Scores change only through
  contract flows (`mark_paid`, default handling, etc.) and lazy decay.
  `get_top_payers` is a public read.

## Verified findings

| # | Finding | Status |
|---|---------|--------|
| 1 | **Ties were history-dependent.** The heap compared by score only, so among equal scores the returned order and, at capacity, *which* tied payer was evicted depended on insertion/update history. Two contracts with identical payers and scores could return different rankings. | Fixed |
| 2 | **Stale ranking after decay.** Lazy reputation decay (`get_payer_score`) rewrote the payer's score and profile but not the heap, so an idle payer stayed ranked at its pre-decay score until its next explicit change. | Fixed |
| 3 | No overflow risk: index math saturates, the heap length is bounded by the capacity, `limit` is clamped to the heap size, and extreme `u32` scores rank correctly. | Verified (tests) |
| 4 | Replaying the same update is idempotent: the payer is removed then re-inserted, so there are never duplicates and the set/order is unchanged. | Verified (tests) |

## Ranking order (new, total)

1. Higher score first.
2. On equal score, **lower address first** (Soroban `Address` ordering).

Eviction at capacity removes the lowest-ranked entry under this order; a
newcomer is admitted only if it strictly outranks that entry.

### Compatibility

No storage-key or type change. A heap persisted before this change was ordered
by score only; its tied entries may not satisfy the new tie-break until they are
next touched. This is self-healing (each update removes and reinserts one
payer) and cannot lose or duplicate entries. No migration is required.

## Behaviour by case

| Case | Result |
|------|--------|
| Normal | Entry added/updated; list is `(score desc, address asc)`. |
| Repeated / replayed event | No change in contents or order. |
| Stale profile (idle decay) | Ranking follows the decayed score on the next score read. |
| Overflow / extremes | `u32::MAX` scores and `limit = u32::MAX` handled; output clamped. |
| Interrupted | Heap and score are written in the same invocation; a failed call rolls both back. |

## Residual risks (not changed)

1. **Evicted payers are never re-promoted.** The heap only learns of a payer
   when that payer's own score changes. If a member later drops below an
   outsider that was rejected earlier, the outsider stays out
   (`test_evicted_payer_is_not_promoted_when_a_member_drops`). An exact top-N
   would need a full index or a scan.
2. **Payers at the default score are not tracked** until their score first
   changes.
3. **Decay of non-reading payers.** Decay is applied lazily, so the ranking
   only reflects it after the payer's score is read (e.g. `payer_score`,
   invoice submission/funding paths). A fully idle payer who is never read keeps
   its stored score and rank.
4. `get_top_payers` is O(n²) in the heap size (n ≤ 50); acceptable at the cap
   and covered by the existing benchmark.
