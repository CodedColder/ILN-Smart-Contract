# Indexer Data-Loss Incident Runbook

This runbook complements the backup/replay mechanics documentation by outlining the operational procedures during an indexer data-loss or corruption incident.

## 1. Detection

Incidents are detected through:
- **Monitoring Alerts**: Prometheus/Grafana alerts triggering on high `indexer_error_rate`, sudden drops in `indexer_ingestion_rate`, or database connection failures (as configured in monitoring setup).
- **Data Inconsistencies**: User reports or automated reconciliations flagging missing invoices, stalled states, or mismatched balances between the indexer API and the Stellar ledger.
- **Service Outages**: Complete failure of the indexer service to start or serve API requests.

## 2. Triage & Decision Tree

When data-loss or corruption is confirmed, choose the recovery path based on the scope of corruption and time since the last known good state.

### Option A: Restore from Backup (Database Snapshot)
**Criteria**: 
- The entire database is corrupted or lost.
- The latest automated snapshot is recent (e.g., < 2 hours old).
- The infrastructure is intact or quickly replaceable.

**Procedure**:
1. Pause the indexer service to prevent further writes.
2. Restore the indexer's SQLite database from the latest backup archive (see [Indexer Operations Guide](indexer-operations.md) §3 — checksum verification and integrity checks are part of the restore script).
3. Verify the API serves correct data (`tsx indexer/scripts/verify-restore.ts --db <restored path>`).
4. Resume the indexer. It will automatically catch up from the restored ledger cursor.

### Option B: Replay from Checkpoint (Selective Resync)
**Criteria**:
- A specific range of blocks was processed incorrectly (e.g., due to a bug).
- The database is mostly intact, but some tables or recent events are corrupted.
- Restoring a full snapshot is too disruptive or the snapshot is too old.

**Procedure**:
1. Pause the indexer.
2. Identify the last known good ledger checkpoint.
3. Delete corrupted data in the database newer than the checkpoint.
4. Update the indexer's sync state cursor to the checkpoint ledger.
5. Resume the indexer to replay events from the checkpoint.

### Option C: Full Resync (From Genesis)
**Criteria**:
- Unrecoverable database corruption with no viable backups.
- Major schema changes or bug fixes requiring a complete rebuild of the indexed state.

**Procedure**:
1. Provision a fresh database instance.
2. Point the indexer to the new database and start it from the contract deployment ledger.
3. (Optional) Run a secondary indexer instance to perform the resync, then switch traffic once caught up, avoiding downtime if the primary is still partially functional.

## 3. Estimated Recovery Times (at current volume)

- **Restore from Backup**: ~15-30 minutes (mostly cloud provider snapshot restore time).
- **Replay from Checkpoint**: ~5 minutes per 10,000 ledgers replayed.
- **Full Resync**: ~2-4 hours, depending on Horizon node limits and total historical event volume.

## 4. Communication

During a recovery operation (especially Full Resync or extended Option A/B), the API and dashboards may serve stale data. 

**User-Facing Template**:
> "We are currently experiencing degraded performance with the ILN dashboard data. On-chain operations are unaffected and smart contracts are fully operational. Our team is restoring the indexer service. Expected resolution in [Time]. Thank you for your patience."

## 5. Reorg-specific response

A reorg alert is a chain-integrity incident rather than a normal service outage. When `indexer_reorg_detected` fires, follow the deep replay workflow in [Indexer Reorg Handling](indexer-reorg-handling.md): confirm the fork depth and divergence ledger, verify the database is blocked by the halt latch, and only resume ingestion after rollback-and-replay completes without a second divergence.

If the automatic recovery path times out or the reorg is deeper than the live detector’s bounded walk, escalate to manual ledger rollback from the last known-good ancestor, then replay the affected ledger range before clearing `pending_reorg`.

## 6. Cross-References
- [Incident Response Runbook (Section 12)](incident-response-runbook.md)
- [Indexer Reorg Handling](indexer-reorg-handling.md)
- Backup and Replay Mechanics (Issues 78-79)
