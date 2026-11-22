/**
 * Replay CLI — re-process indexer events from an arbitrary ledger checkpoint.
 *
 *   tsx indexer/scripts/replay.ts --from-ledger 12345678
 *   tsx indexer/scripts/replay.ts --from-ledger 12345678 --to-ledger 12400000
 *   tsx indexer/scripts/replay.ts --from-ledger 12345678 --continue-on-error
 *
 * Range: [--from-ledger, --to-ledger) — start inclusive, end exclusive.
 *
 * Exit codes:
 *   0  range fully replayed
 *   1  replay stopped (Horizon retries exhausted, database failure, or an
 *      undecodable transaction without --continue-on-error); the printed
 *      resume ledger is safe to pass back as --from-ledger
 *   2  finished with skipped transactions (--continue-on-error) or halted
 *      by a pending ledger divergence — the range is NOT complete
 *
 * Environment:
 *   DB_PATH                        SQLite file to repair (default ./indexer.db)
 *   HORIZON_URL                    Horizon endpoint (default from src/config.ts)
 *   ILN_CONTRACT_ID / CONTRACT_ID  contract address filter (required unless --dry-run)
 *
 * Writes are idempotent: invoice rows are upserted, events deduplicate on
 * (transaction_hash, event_index), and each transaction's rows commit with
 * the cursor in one database transaction. See docs/indexer-operations.md §5.
 */

import fs from 'node:fs';
import Database from 'better-sqlite3';
import { config } from '../src/config.js';
import { createSqlEventRepository } from '../src/db/eventRepository.js';
import { decodeTransactionEventsFromMeta } from '../src/ingestion/eventListener.js';
import { ReplayIncompleteError, runReplay } from '../src/ingestion/replay.js';

function parseLedgerFlag(name: string): number | undefined {
  const index = process.argv.indexOf(name);
  if (index === -1) return undefined;
  const value = Number(process.argv[index + 1]);
  if (!Number.isFinite(value) || value < 0 || !Number.isInteger(value)) {
    console.error(`${name} requires a non-negative integer ledger sequence.`);
    process.exit(1);
  }
  return value;
}

const fromLedger = parseLedgerFlag('--from-ledger');
if (fromLedger === undefined) {
  console.error('Usage: replay.ts --from-ledger <ledger> [--to-ledger <ledger>]');
  process.exit(1);
}
const toLedger = parseLedgerFlag('--to-ledger');
const continueOnError = process.argv.includes('--continue-on-error');
if (toLedger !== undefined && toLedger <= fromLedger) {
  console.error('--to-ledger is exclusive and must be greater than --from-ledger.');
  process.exit(1);
}

if (!config.contractId) {
  console.error('ILN_CONTRACT_ID/CONTRACT_ID is required so only this contract\'s events are replayed.');
  process.exit(1);
}

const dbPath = config.dbPath;
if (!fs.existsSync(dbPath)) {
  console.error(`Database not found at ${dbPath}. Set DB_PATH.`);
  process.exit(1);
}

console.log(
  `Replaying ledgers ${fromLedger}${toLedger !== undefined ? `–${toLedger}` : '+ '} into ${dbPath}`
);

const db = new Database(dbPath);
const repository = createSqlEventRepository(db);

runReplay({
  repository,
  horizonUrl: config.horizonUrl,
  contractAddress: config.contractId,
  decodeTransactionEvents: decodeTransactionEventsFromMeta,
  fromLedger,
  continueOnError,
  ...(toLedger !== undefined ? { toLedger } : {}),
})
  .then((result) => {
    console.log(
      result.complete ? 'Replay complete:' : 'Replay finished INCOMPLETE:',
      JSON.stringify(result, null, 2)
    );
    if (result.failedTransactions > 0) {
      console.warn(
        `${result.failedTransactions} transaction(s) failed to decode and were skipped: ` +
          `${result.failedTransactionHashes.join(', ')}. The range is not complete.`
      );
    }
    if (result.haltedByReorg) {
      console.warn('Stopped on a flagged ledger divergence; run reorg recovery before replaying.');
    }
    db.close();
    process.exitCode = result.complete ? 0 : 2;
  })
  .catch((error) => {
    console.error(`Replay failed: ${error instanceof Error ? error.message : String(error)}`);
    if (error instanceof ReplayIncompleteError) {
      const end = toLedger !== undefined ? ` --to-ledger ${toLedger}` : '';
      console.error(
        `Everything before ledger ${error.resumeFromLedger} is committed. Resume with: ` +
          `--from-ledger ${error.resumeFromLedger}${end}`
      );
    }
    db.close();
    process.exitCode = 1;
  });
