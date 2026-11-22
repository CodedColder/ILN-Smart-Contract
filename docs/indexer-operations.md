# Indexer Operations Guide

Operational procedures for the ILN Indexer's persistent state: scheduled
backups, restores, point-in-time replay, and continuous chain reconciliation.
This document is the companion to the
[Indexer Data-Loss Incident Runbook](indexer-incident-runbook.md) and defines
the RPO/RTO targets referenced there.

## 1. Storage engine note

The indexer persists to a **SQLite** database (via `better-sqlite3`) at the
path configured by `DB_PATH` (default `./indexer.db`), running in WAL mode.
The Postgres instance in `docker-compose.yml` belongs to the **notifications**
service, not the indexer — indexer backups therefore use SQLite's online
backup API rather than `pg_dump`. All tooling below ships inside
[`indexer/scripts/`](../indexer/scripts/) and runs against any SQLite snapshot
without stopping the live service.

## 2. Backup procedure

### 2.1 Manual / scheduled run

```bash
# Defaults: DB_PATH=./indexer.db BACKUP_DIR=./backups
pnpm --filter @iln/indexer backup

# Explicit paths
DB_PATH=/var/lib/iln/indexer.db BACKUP_DIR=/var/backups/iln-indexer \
  pnpm --filter @iln/indexer backup

# Cron example — hourly snapshots, retention applied automatically
0 * * * * cd /srv/iln && DB_PATH=/var/lib/iln/indexer.db \
  BACKUP_DIR=/var/backups/iln-indexer pnpm --filter @iln/indexer backup
```

Each run produces:

| Artifact | Purpose |
| --- | --- |
| `backups/indexer-backup-<ISO-timestamp>.db.gz` | Gzip-compressed transactionally consistent snapshot |
| `backups/indexer-backup-<ISO-timestamp>.db.gz.json` | Manifest: SHA-256 checksum, size, ingestion checkpoint (`last_processed_ledger`, `last_processed_cursor`), per-table row counts |

The snapshot uses better-sqlite3's **online backup API**, so it is safe to run
against a live WAL-mode database while the indexer keeps ingesting events —
no maintenance window required.

### 2.2 Retention schedule (grandfather-father-son)

Configured via environment variables on the backup job:

| Window | Env var | Default | Kept |
| --- | --- | --- | --- |
| Daily | `RETENTION_DAILY_DAYS` | `7` | Newest snapshot per day |
| Weekly | `RETENTION_WEEKLY_WEEKS` | `4` | Newest snapshot per ISO week |
| Monthly | `RETENTION_MONTHLY_MONTHS` | `3` | Newest snapshot per month |

The most recent snapshot is always retained regardless of window, so the RPO
floor is one backup interval even immediately after pruning. With the default
hourly cron this yields a worst-case recovery point of ~1 hour for the last
day, degrading gracefully to daily/weekly granularity further back, and a
maximum stored set of roughly 7 + 4 + 3 + current ≈ **15 archives**.

Backups are plain gzip'd SQLite files — replicate them off-host with your
existing object-storage sync (e.g. `rclone`/AWS S3 versioning). Verify
off-site copies by re-running the checksum step of the restore script against
downloaded artifacts monthly.

## 3. Restore procedure

### 3.1 Full restore into a fresh instance

```bash
# Restore the newest verified backup into ./restored/indexer.db
pnpm --filter @iln/indexer restore --latest

# Or an explicit archive, into an explicit target
pnpm --filter @iln/indexer restore --file /var/backups/iln-indexer/indexer-backup-2026-08-26T07-00-00-000Z.db.gz \
  --target /var/lib/iln-restored/indexer.db

# Replace a corrupted live database in place (stops nothing — stop the indexer first)
pnpm --filter @iln/indexer restore --latest --target /var/lib/iln/indexer.db --force-overwrite
```

The restore script:

1. Resolves the requested manifest and verifies the archive's **SHA-256** before touching disk.
2. Decompresses to a temporary file and runs SQLite `PRAGMA integrity_check`.
3. Cross-checks every table's row count against the backup manifest.
4. Atomically renames the verified file into the target path (refuses to clobber an existing target unless `--force-overwrite`).

### 3.2 Post-restore API verification

```bash
tsx indexer/scripts/verify-restore.ts --db ./restored/indexer.db
```

This boots the real express app against the restored file and exercises
`/health`, `/stats`, `/invoices/:id` (sampled, compared row-by-row),
`/events?address=…`, `/leaderboard`, and `/reputation/:address`, failing
non-zero on any mismatch between API output and restored rows. A restore is
only considered complete once this passes.

### 3.3 Resuming ingestion after restore

Start the indexer with `DB_PATH=<restored path>`. The listener resumes from
the `last_processed_cursor` stored inside the restored `indexer_state` table,
so no events are missed between snapshot time and now. For gaps caused by an
indexer bug rather than data loss, prefer [checkpoint replay](#5-checkpoint-replay-from-an-arbitrary-ledger).

## 4. RPO / RTO targets

| Metric | Target | How it is met |
| --- | --- | --- |
| **RPO** (max tolerable data loss) | ≤ 1 hour for recent state; ≤ 24 hours guaranteed floor | Hourly backup cron; newest archive always survives retention pruning |
| **RTO** (max tolerable restore time) | ≤ 30 minutes for full restore + verification | Restore is I/O-bound only (checksum → gunzip → integrity check → rename); verification samples ≤ 25 invoices |
| Backup success rate | ≥ 99% of scheduled runs alert on failure | Wrap cron in failure notification (see notifications service webhook contract below) |
| Restore drill frequency | Quarterly | Run §3.1–§3.2 against staging and record timings in the incident runbook |

Recovery time scales with database size (~seconds per 100 MB including
verification); the 30-minute RTO assumes databases under ~5 GB, which covers
projected multi-year mainnet volume at current event rates.

## 5. Checkpoint replay from an arbitrary ledger

When derived data was produced incorrectly by a buggy indexer (but the ledger
itself is fine), a full restore is unnecessary — replay events from before the
bad range instead ([incident runbook](indexer-incident-runbook.md) Option B):

```bash
# Re-process everything from ledger N onward (N itself included)
pnpm --filter @iln/indexer replay -- --from-ledger 12345678

# Repair only a bounded window, e.g. the ledgers a bad deploy touched.
# Range is [--from-ledger, --to-ledger): 12400000 itself is NOT replayed.
pnpm --filter @iln/indexer replay -- --from-ledger 12345678 --to-ledger 12400000
```

Exit codes: `0` range complete; `1` replay stopped (prints the resume ledger);
`2` finished but incomplete (`--continue-on-error` skipped undecodable
transactions, or a pending ledger divergence halted it).

Procedure:

1. **Stop live ingestion** (or run replay against a shadow copy of the DB) so the listener does not race the repair.
2. **Identify the checkpoint** — the last ledger known to produce correct data. `sqlite3 $DB_PATH "SELECT state_value FROM indexer_state WHERE state_key='last_processed_ledger'"` plus the reconciliation report's mismatch details help bound the range.
3. **Run the replay** command above against `DB_PATH`.
4. **Verify** — re-run the reconciliation job (`scripts/reconcile.ts --once`, exit code 0) and/or spot-check `/invoices/:id` for the affected ids.
5. **Resume ingestion** — replay leaves `last_processed_ledger`/`last_processed_cursor` at its final processed transaction, so the live listener continues exactly where replay ended.

Safety properties:

- Ingestion writes are idempotent: `invoices` upsert on conflict; `events`/`reputation_updates` deduplicate on `(transaction_hash, event_index)` — replays cannot create duplicate rows.
- **Atomic per transaction.** A transaction's invoice/event/reputation/insurance rows and the `last_processed_*` cursor commit in one SQLite transaction. A crash or commit failure leaves the database exactly at the previous transaction boundary — never rows without a cursor, never a cursor without rows.
- **Strict by default.** The first transaction that cannot be decoded or persisted stops the run (exit `1`). Everything before the printed `resume ledger` is committed; re-run with `--from-ledger <resume ledger>` (that ledger is re-processed idempotently). `--continue-on-error` restores the old log-and-skip behaviour for *decode* failures only, lists the skipped hashes, and exits `2`; database errors always stop the run.
- **Bounded retries.** Horizon 429/5xx/network errors are retried with exponential backoff (5 attempts by default); exhaustion stops the run like any other failure — it is never treated as "no more data".
- **Correct cursor.** Replay positions Horizon at the TOID of `--from-ledger` (`ledger << 32`), follows `_links.next`, and stops on an empty page (the chain tip — Horizon always returns a `next` link). Records repeated across pages are skipped and counted (`duplicatesSkipped`).
- **Out-of-order safe.** Re-covering an older window after newer events are stored does not roll invoices back: when an event older than an invoice's newest stored event arrives, the invoice is re-derived from its full event history in chain order.
- **Reorg aware.** If a ledger divergence is pending (or is detected mid-run by the attached detector), replay writes nothing further, returns `haltedByReorg: true` / `complete: false`, and the CLI exits `2`. Run [reorg recovery](indexer-reorg-handling.md) first.
- The parent invoice row is always written before its child event rows, so foreign keys stay satisfied with `foreign_keys=ON`.

#### Recovering from an interrupted or failed replay

| Symptom | Persisted state | Action |
| --- | --- | --- |
| Process killed / host crash mid-replay | Rows + cursor at the last fully committed transaction | `--from-ledger $(sqlite3 $DB_PATH "SELECT state_value FROM indexer_state WHERE state_key='last_processed_ledger'")` with the original `--to-ledger` |
| Exit `1` (`Replay failed … Resume with: …`) | Everything before the resume ledger committed | Fix the cause (Horizon, disk, decoder), then run the printed command |
| Exit `2`, `failedTransactions > 0` | Range NOT complete; listed hashes missing | Fix the decoder and replay the affected ledgers without `--continue-on-error` |
| Exit `2`, `haltedByReorg: true` | Nothing written at/after the fork | Run reorg recovery, then replay |

Overlapping or repeated ranges are always safe; there is no need to compute an exact non-overlapping window.

Regression tests: [`indexer/tests/replay.test.ts`](../indexer/tests/replay.test.ts) intentionally corrupts derived state, replays from a pre-corruption checkpoint, and asserts the correct state is restored without duplicate events. [`indexer/tests/crashRecovery.test.ts`](../indexer/tests/crashRecovery.test.ts) kills the process between a transaction's rows and its cursor write, aborts real SQLite commits with triggers, reopens the database file as a restarted process would, replays overlapping ranges, and checks that reconciliation and reorg state refuse to report replayed data as healthy/canonical.

## 5a. Historical catch-up (backfill)

Catch-up ingests a ledger range the live stream never saw — typically from
the contract's deployment ledger up to the first ledger the indexer streamed.
It runs under the ingestion lease, before live streaming starts:

```bash
CATCHUP_FROM_LEDGER=1200000   # deployment ledger (inclusive). Unset = catch-up disabled
CATCHUP_TO_LEDGER=1300000     # exclusive. Unset = earliest ledger already in `events`
CATCHUP_BATCH_SIZE=1000       # ledgers per checkpointed batch
```

Behaviour (`indexer/src/ingestion/catchup.ts`):

- **Range** is `[CATCHUP_FROM_LEDGER, CATCHUP_TO_LEDGER)`, split into batches of `CATCHUP_BATCH_SIZE` ledgers. Each batch reads Horizon `/transactions` from the TOID of its first ledger and follows continuation links until the batch end (or the chain tip).
- **Decoding and writes** are the live listener's (`EventListener.processTransaction`), with ledger-header reorg verification. Each transaction commits atomically. The **live cursor is never touched**.
- **Checkpoint** `catchup_checkpoint:<from>-<to>` in `indexer_state` (JSON: `nextLedger`, `status`) advances to a batch's end **only after every transaction in that batch committed**. On restart catch-up resumes at `nextLedger`; a partially written batch is simply re-covered (idempotent).
- **Gaps are reported, not skipped.** Before every batch the indexer reads Horizon's root (`history_elder_ledger`/`history_latest_ledger`). Any part of the batch Horizon does not serve (pruned history, or not yet closed) fails the run with `kind: gap` and the missing ranges; the checkpoint stays before it.
- **Failure kinds** (logged as `historical catch-up incomplete`): `fetch` (Horizon retries exhausted or a non-retryable 4xx), `ingestion` (decode or database failure, including the checkpoint write), `reorg` (divergence flagged — run recovery), `gap`, `aborted` (lease lost / shutdown). In every case `resumeFromLedger` is the first ledger not covered by a committed batch, and live streaming still starts.

Inspect progress:

```bash
sqlite3 $DB_PATH "SELECT state_key, state_value FROM indexer_state WHERE state_key LIKE 'catchup_checkpoint:%'"
```

Recovery:

- **Interrupted / crashed / `fetch` / `ingestion`** — fix the cause and restart the indexer with the same range; it resumes from `nextLedger`.
- **`gap`** — point `HORIZON_URL` at a full-history Horizon (or archive), or narrow the range to what is served and record the unrecoverable window in the incident log. Never mark it complete by hand.
- **`reorg`** — let the reorg recovery path clear the latch, then restart.
- **Re-run a completed range** — a `complete` checkpoint is a no-op; delete that `indexer_state` row (or call `executeCatchup({ …, restart: true })`) to re-cover it. Re-running is idempotent and does not roll back newer invoice state.

Tests: [`indexer/tests/catchup.test.ts`](../indexer/tests/catchup.test.ts) covers range boundaries, TOID cursors, continuation links with partial and overlapping pages, duplicate transactions, bounded and non-retryable failures, injected database and checkpoint-write failures, mid-batch process interruption with resume from a fresh instance, retention/tip gaps, reorg halts, and idempotent re-runs.

## 6. Continuous reconciliation

Backups protect against infrastructure loss; reconciliation detects *semantic*
drift between indexed data and on-chain truth. The scheduled consistency job
samples invoices/stats, compares them against direct contract reads, and
alerts through the notifications webhook channel when drift exceeds tolerance.
The same run re-verifies the stored ledger-header chain: a hash that no longer
matches the chain latches a halt, posts `indexer_reorg_detected`, and runs the
shared rollback-and-replay (the reorg the live detector could not see for
itself — Issue #866). Cadence, tolerance thresholds, and the recovery
guarantees are documented in
[docs/indexer-reconciliation.md](indexer-reconciliation.md).

## 7. Related documentation

- [Indexer Data-Loss Incident Runbook](indexer-incident-runbook.md) — decision tree for choosing restore vs replay vs full resync
- [Indexer Reconciliation](indexer-reconciliation.md) — continuous drift detection, reorg backstop, and rollback-and-replay recovery with alerting
- [CI/CD Guide](ci-cd.md) — deployment pipeline that consumes these scripts in post-deploy smoke checks
- [Events Reference](events.md) — event types handled during ingestion/replay
