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

# Repair only a bounded window, e.g. the ledgers a bad deploy touched
pnpm --filter @iln/indexer replay -- --from-ledger 12345678 --to-ledger 12400000
```

Procedure:

1. **Stop live ingestion** (or run replay against a shadow copy of the DB) so the listener does not race the repair.
2. **Identify the checkpoint** — the last ledger known to produce correct data. `sqlite3 $DB_PATH "SELECT state_value FROM indexer_state WHERE state_key='last_processed_ledger'"` plus the reconciliation report's mismatch details help bound the range.
3. **Run the replay** command above against `DB_PATH`.
4. **Verify** — re-run the reconciliation job (`scripts/reconcile.ts --once`, exit code 0) and/or spot-check `/invoices/:id` for the affected ids.
5. **Resume ingestion** — replay leaves `last_processed_ledger`/`last_processed_cursor` at its final processed transaction, so the live listener continues exactly where replay ended.

Safety properties:

- Ingestion writes are idempotent: `invoices` upsert on conflict; `events`/`reputation_updates` deduplicate on `(transaction_hash, event_index)` — replays cannot create duplicate rows.
- Per-transaction failures are logged and skipped (`failedTransactions` in the summary); exit code 2 signals partial success for alerting wrappers.
- The parent invoice row is always written before its child event rows, so foreign keys stay satisfied with `foreign_keys=ON`.

Regression tests: [`indexer/tests/replay.test.ts`](../indexer/tests/replay.test.ts) intentionally corrupts derived state, replays from a pre-corruption checkpoint, and asserts the correct state is restored without duplicate events.

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
