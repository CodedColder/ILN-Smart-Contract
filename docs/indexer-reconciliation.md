# Indexer Reconciliation

Continuous drift detection between the ILN indexer's SQLite state and the
on-chain invoice-liquidity contract. Productionized from
[`tests/e2e/indexerConsistency.test.ts`](../tests/e2e/indexerConsistency.test.ts):
instead of asserting consistency once inside CI, a scheduled job periodically
spot-checks live indexed data against direct contract reads and alerts through
the notifications service when drift exceeds tolerance.

## How it works

1. **Sample** — each run selects up to `RECONCILIATION_SAMPLE_SIZE` random invoice ids from the `invoices` table (`ORDER BY RANDOM()`).
2. **Read chain truth** — for each sampled id the job performs a read-only Soroban simulation of `get_invoice(invoice_id)` via [`indexer/src/reconciliation/chainReader.ts`](../indexer/src/reconciliation/chainReader.ts) (no fees, no submission). It also calls `get_invoice_count()`.
3. **Compare** — per-invoice fields compared: `status`, `amount`, `amount_funded`, `amount_paid`. The global indexed count is compared against `get_invoice_count()`.
4. **Alert** — if drift beyond tolerance is detected, an `indexer_drift_detected` payload (severity `critical`) is POSTed to `RECONCILIATION_ALERT_URL` — point this at the notifications service intake (e.g. `/notify/slack` or any webhook subscription registered there, see [docs/notifications-operations.md](notifications-operations.md)).

## Cadence & thresholds

| Setting | Env var | Default | Rationale |
| --- | --- | --- | --- |
| Run interval | `RECONCILIATION_INTERVAL_MS` | `900000` (**15 minutes**) | Fast enough to bound silent-drift windows well under one backup cycle, cheap enough that 25 simulations × 96 runs/day are negligible RPC load |
| Sample size | `RECONCILIATION_SAMPLE_SIZE` | `25` invoices/run | ~99% chance of catching a defect affecting ≥ 18% of invoices within one run, while keeping run cost flat as the table grows |
| Invoice drift tolerance | `RECONCILIATION_TOLERANCE_PERCENT` | `1%` of sampled invoices | One drifted invoice in 25 trips the alert; anything below is treated as noise from in-flight transactions |
| Count lag tolerance | derived | `max(5, ⌈1% of chain count⌉)` | Absorbs normal ingestion lag between chain settlement and Horizon stream delivery |
| Reorg header sample | `REORG_HEADER_SAMPLE_SIZE` | `20` ledgers | Bounds the per-run header walk while covering a realistic fork depth; a divergence inside the sample is found in a single run |

Chain-read errors (RPC outages) are recorded in the report but deliberately
**excluded** from the drift rate — infrastructure flakiness must not page
on-call for data that was never mis-ingested. Sustained RPC failure surfaces
through normal monitoring instead.

## Reorg backstop

Every run also re-verifies the stored ledger-header chain against chain truth
(Issue #866). This is the reorganization signature the live
[`LedgerReorgDetector`](../indexer/src/ingestion/reorgDetector.ts) cannot always
see for itself — a fork whose divergence point is older than the
`ledger_headers` window still on disk, or one that arrived while ingestion was
halted — so reconciliation doubles as a periodic safety net instead of only an
invoice-level spot check.

1. **Sample** — the run takes the most recent `REORG_HEADER_SAMPLE_SIZE` rows
   from `ledger_headers`, newest first.
2. **Read chain truth** — each sampled sequence's canonical hash is read from
   Horizon (`GET /ledgers/{sequence}`) through
   [`LedgerHeaderSource`](../indexer/src/ingestion/ledgerHeaders.ts).
3. **Compare** — the first sequence whose stored `hash` no longer matches the
   chain's triggers the shared fork-point walk (`findDivergence`), which walks
   back to the common ancestor and reports the divergence ledger with
   `detectedBy: 'consistency_job'`.
4. **Halt** — the divergence immediately latches a halt in `indexer_state`
   (`reorg_halted = 1` with the pending divergence recorded), so live
   ingestion stops advancing even when this process never runs recovery.
5. **Alert + recover** — an `indexer_reorg_detected` payload (severity
   `critical`) is POSTed to `RECONCILIATION_ALERT_URL`, then the shared
   rollback-and-replay below runs when the scheduler was wired with
   `onReorgDetected` ([`indexer/src/index.ts`](../indexer/src/index.ts),
   [`indexer/scripts/reconcile.ts`](../indexer/scripts/reconcile.ts)).

Unreadable headers are counted in `reorgCheckErrors` and, exactly like
chain-read errors above, are never treated as a divergence — an unavailable RPC
must not be read as a fork.

### Rollback-and-replay recovery

[`recoverFromReorg`](../indexer/src/ingestion/reorgRecovery.ts) is the single
implementation shared by the live detector's `ReorgDetected` path and the
backstop's `onReorgDetected` hook (Issue #864):

1. **Latch** the halt before touching any row.
2. **Roll back** in one transaction to `divergenceLedger - 1`: `events`,
   `reputation_updates`, `insurance_pool_*`, invoices no longer referenced by
   a surviving event, and `ledger_headers` for `ledger >= divergenceLedger`
   are deleted, and `last_processed_ledger` is reset to `divergenceLedger - 1`.
3. **Replay** the affected range with [`replay.ts`](../indexer/src/ingestion/replay.ts)
   — the same idempotent-upsert path used for ordinary checkpoint repair —
   while holding [`ingestionLock`](../indexer/src/ingestion/ingestionLock.ts)
   leadership, so a concurrent ingestor can never interleave writes with the
   rollback.
4. **Clear or keep** the halt: it is cleared only after replay finishes with no
   pending reorg left behind. If replay throws, the halt is re-latched and the
   error propagates — fail closed, so ingestion stays blocked instead of
   resuming against a half-built history.

## Operating modes

```bash
# One-shot check (exit code 2 on drift) — suitable for cron/CI
pnpm --filter @iln/indexer exec tsx scripts/reconcile.ts --once

# Continuous schedule with alerting
RECONCILIATION_ALERT_URL=https://notifications.example/notify/slack \
  pnpm --filter @iln/indexer exec tsx scripts/reconcile.ts --watch
```

In-process scheduling is also available: start the indexer with
`RECONCILIATION_ENABLED=true` and the scheduler runs alongside ingestion in
[`indexer/src/index.ts`](../indexer/src/index.ts).

Both modes run the reorg backstop as well: `--once` alerts, recovers, and exits
`2` when a divergence was found (it sets `driftDetected` in the report);
`--watch` delegates to the scheduler's reorg branch. If recovery fails, the
halt latch stays set and the next run retries rather than resuming.

### Alert payload contract

```json
{
  "type": "indexer_drift_detected",
  "severity": "critical",
  "summary": "Indexer drift detected: 3/25 sampled invoices drifted (...)",
  "details": { "ranAt": "...", "driftedInvoices": 3, "mismatches": [ ... ] },
  "firedAt": "2026-08-26T09:00:00.000Z"
}
```

The reorg backstop posts a second, distinct payload — a reorg is not a
field-level mismatch to tolerate, it names the ledger that must be rolled back:

```json
{
  "type": "indexer_reorg_detected",
  "severity": "critical",
  "summary": "Ledger reorg detected at ledger 123456 (ledger_hash_mismatch, common ancestor 123455): 20 stored header(s) checked, 0 unreadable. Ingestion halted pending rollback-and-replay.",
  "details": { "ranAt": "...", "reorgHeadersChecked": 20, "reorgDivergence": { "divergenceLedger": 123456, "commonAncestorLedger": 123455, "detectedBy": "consistency_job" } },
  "firedAt": "2026-08-26T09:00:00.000Z"
}
```

When both fire in the same run the reorg payload is the one sent: the
scheduler checks `reorgDivergence` before `driftDetected`.

On-call response: follow [docs/indexer-incident-runbook.md](indexer-incident-runbook.md).
Field-level mismatches on specific ledgers are fixed by checkpoint replay;
whole-database corruption falls back to restore-from-backup
([docs/indexer-operations.md](indexer-operations.md)). A reorg alert means the
rollback-and-replay already ran (or will on the next run) — confirm
`reorg_halted` cleared before trusting the data again.

## Test coverage

Unit tests in [`indexer/tests/reconciliation.test.ts`](../indexer/tests/reconciliation.test.ts)
exercise the full decision matrix against an injected fake chain reader:
clean sync, field-level drift + missing-on-chain, tolerance boundary,
count-lag detection, chain-read-error exclusion, and webhook dispatch shape —
plus the reorg backstop (header mismatch halts, unreadable headers never halt,
recovery hook invoked with the divergence).

Reorg behaviour itself is covered by
[`indexer/tests/reorgDetection.test.ts`](../indexer/tests/reorgDetection.test.ts)
(fork-point walk, gap tolerance, halt latch, sequence-regression rule) and
[`indexer/tests/reorgRecovery.test.ts`](../indexer/tests/reorgRecovery.test.ts)
(rollback scope, replay hand-off, fail-closed latch).
