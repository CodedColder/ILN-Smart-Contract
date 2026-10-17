# Indexer Reorg Handling

## Detection design

The indexer treats a reorg as a ledger-chain integrity failure, not as routine drift. Two paths detect it:

1. Live ingestion (`LedgerReorgDetector`) verifies each newly ingested ledger against the canonical headers before persisting state. A hash mismatch, parent-hash mismatch, or sequence regression marks the first stale ledger and calls `findDivergence()` to walk back through stored headers until the last valid common ancestor is found.
2. The periodic reconciliation backstop (`detectReorgFromStoredHistory`) re-checks recently stored ledger headers against the canonical chain and raises `indexer_reorg_detected` when the stored history no longer matches the chain.

The resulting `ReorgDivergence` object records:

- `divergenceLedger`: first ledger on the invalid chain
- `commonAncestorLedger`: last ledger still matching canonical chain
- `reason`: `ledger_hash_mismatch`, `parent_hash_mismatch`, or `sequence_regression`
- `forkDepth`: how far the detector walked back before finding the common ancestor

After detection, the indexer latches `pending_reorg` and `reorg_halted` in `indexer_state`, preventing new writes while the database is repaired.

## Automatic recovery scope

The automatic recovery path is the shared `recoverFromReorg()` routine in `indexer/src/ingestion/reorgRecovery.ts`.

It does the following:

- serializes rollback-and-replay against live ingestion using the SQLite lease in `ingestionLock.ts`
- rolls back all rows created at or after the divergence ledger
- invalidates stored ledger headers from the divergence forward
- replays from the divergence ledger against canonical headers
- clears the halt only when replay completes without a second divergence

Automatic recovery is bounded by operational limits:

- it expects the divergence to be reachable from the current stored header history and within the configured fork-depth window
- it requires the canonical ledger source to remain available during replay
- a second divergence discovered during replay keeps the system halted and requires operator attention

This is considered a safe, fail-closed path: if replay cannot complete, the system remains halted instead of resuming on partially rolled-back state.

## Severity classification

Reorgs are classified by the depth of the fork detected:

- `shallow`: forkDepth <= 3
- `deep`: forkDepth > 3

The shared alert router emits operator alerts at `critical` severity for all reorgs, while including the `reorgSeverity` classification in the alert metadata so operators can prioritize deep reorgs immediately.

## Manual intervention for deep reorgs

A deep reorg or a replay failure means automatic recovery is not enough. Use this procedure:

1. Confirm the alert details and the divergence ledger:
   - `indexer_state.pending_reorg`
   - `indexer_state.reorg_halted`
   - the alert payload from `indexer_reorg_detected`
2. Stop the writer and any standby ingestors from writing to the SQLite DB.
3. Verify the canonical chain from the last known-good ancestor. Check the exact ledger at the common ancestor and the divergence ledger before making changes.
4. Roll back the database to the last valid ledger, or delete the rows created at or after the divergence ledger from the affected tables and reset the ledger header range.
5. Rebuild the ledger-header history from the rollback point and replay the affected ledger range from the divergence ledger upward.
6. Re-run the reconciliation or a focused replay check: verify there is no pending reorg, no reorg halt, and the stored headers match the canonical chain.
7. Restart ingestion only after the halt latch has been cleared by a successful recovery run.

In very deep or multi-fork outages, the safest path is a deliberate offline repair: take the system out of service, repair the database from the common ancestor, and then replay the affected range rather than trying to resume mid-fork.

## Operational runbook

### When an alert fires

- Confirm the alert is a real reorg and not a transient RHorizon outage or a stale detector state.
- Inspect the stored divergence record and determine whether the fork is shallow or deep.
- If the automatic rollback-and-replay is still running, allow it to finish. Do not clear the halt latch manually while replay is in progress.

### When automatic recovery fails

- Review the error log for the replay failure or second divergence.
- Check whether a newer divergence was detected during replay (`pending_reorg` was re-written) and whether the previous common ancestor is still valid.
- If the same divergence remains unresolved after the timeout or the fork is too deep for bounded replay, perform the manual reset above.

### Final confirmation

A reorg is resolved only when all of the following are true:

- `reorg_halted` is absent
- `pending_reorg` is absent
- the last processed ledger and ledger headers match the canonical chain
- a reconciliation run reports no `reorgDivergence` and no drift beyond tolerance

## Related docs

- [Indexer HA](indexer-ha.md)
- [Indexer Incident Runbook](indexer-incident-runbook.md)
- [Indexer Reconciliation](indexer-reconciliation.md)
