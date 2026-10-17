/**
 * Ledger reorg detection for the ingestion path (Issue #863).
 *
 * Every ledger the indexer ingests is verified against the canonical chain
 * before any of its transactions are written:
 *
 *  1. **Ledger hash continuity** — if a header for the same sequence is
 *     already stored and its hash differs from the chain's current hash at
 *     that height, the indexed rows for that ledger were produced by a chain
 *     that no longer exists.
 *  2. **Parent-hash continuity** — if the stored header for `sequence - 1`
 *     does not match the observed ledger's `parentHash`, the stored history
 *     stops linking to the canonical chain (the classic fork signature).
 *  3. **Sequence continuity** — a never-before-seen ledger *below* our stored
 *     tip whose stored successor does not chain back to it means the stored
 *     side is stale above that point.
 *
 * On any match the detector walks back through stored headers (bounded by
 * `forkMaxDepth`) to find the last ledger whose stored hash still equals the
 * canonical hash — the fork point — records a `ReorgDivergence` in
 * `indexer_state`, and latches a halt flag. Ingestion checks the halt flag
 * before persisting, so a divergent chain is never built on top of: the
 * divergence is *flagged*, not silently absorbed, and the recovery path
 * (`reorgRecovery.ts`) rolls back to the fork and replays forward.
 */

import type Database from 'better-sqlite3';
import type { LedgerHeader, LedgerHeaderSource } from './ledgerHeaders.js';

export type ReorgReason =
  | 'ledger_hash_mismatch'
  | 'parent_hash_mismatch'
  | 'sequence_regression';

export type ReorgOrigin = 'ingestion' | 'consistency_job';

export interface ReorgDivergence {
  /** First ledger on the new chain whose stored history is invalid. */
  divergenceLedger: number;
  /** Last ledger whose stored hash still equals the canonical hash (D - 1). */
  commonAncestorLedger: number;
  reason: ReorgReason;
  detectedBy: ReorgOrigin;
  detectedAt: string;
  /** Stored (now-invalid) hash at `divergenceLedger`, when history exists. */
  indexedHash: string | null;
  /** Canonical hash at `divergenceLedger`, when it could be read. */
  observedHash: string | null;
  /** Canonical parent hash that failed to link into stored history. */
  observedParentHash: string | null;
  /** Stored headers in the walk-back that failed to match the chain. */
  forkDepth: number;
  message: string;
}

/** `indexer_state` key holding the JSON-encoded pending divergence. */
export const PENDING_REORG_KEY = 'pending_reorg';
/** `indexer_state` key latching ingestion halt while a reorg is unresolved. */
export const REORG_HALTED_KEY = 'reorg_halted';
/** How far back the fork-point walk may reach before giving up. */
export const DEFAULT_FORK_MAX_DEPTH = 10;

export interface StoredLedgerHeader {
  sequence: number;
  hash: string;
  parent_hash: string;
}

export interface FindDivergenceOptions {
  db: Database.Database;
  source: LedgerHeaderSource;
  /** A sequence whose *stored* header is known to be stale. */
  staleSequence: number;
  reason: ReorgReason;
  detectedBy: ReorgOrigin;
  /** Canonical parent hash that failed the parent-hash check, if any. */
  observedParentHash?: string | null;
  forkMaxDepth?: number;
  clock?: () => number;
}

function readState(db: Database.Database, key: string): string | undefined {
  const row = db
    .prepare('SELECT state_value FROM indexer_state WHERE state_key = ?')
    .get(key) as { state_value: string } | undefined;
  return row?.state_value;
}

function writeState(db: Database.Database, key: string, value: string): void {
  db.prepare(
    `INSERT INTO indexer_state (state_key, state_value)
     VALUES (?, ?)
     ON CONFLICT(state_key) DO UPDATE SET state_value = excluded.state_value`
  ).run(key, value);
}

function deleteState(db: Database.Database, key: string): void {
  db.prepare('DELETE FROM indexer_state WHERE state_key = ?').run(key);
}

export function getStoredHeader(
  db: Database.Database,
  sequence: number
): StoredLedgerHeader | undefined {
  return db
    .prepare('SELECT sequence, hash, parent_hash FROM ledger_headers WHERE sequence = ?')
    .get(sequence) as StoredLedgerHeader | undefined;
}

export function getMaxStoredSequence(db: Database.Database): number | null {
  const row = db
    .prepare('SELECT MAX(sequence) AS max_sequence FROM ledger_headers')
    .get() as { max_sequence: number | null };
  return row.max_sequence ?? null;
}

export function storeLedgerHeader(db: Database.Database, header: LedgerHeader): void {
  db.prepare(
    `INSERT INTO ledger_headers (sequence, hash, parent_hash, ingested_at)
     VALUES (?, ?, ?, ?)
     ON CONFLICT(sequence) DO UPDATE SET
       hash = excluded.hash,
       parent_hash = excluded.parent_hash,
       ingested_at = excluded.ingested_at`
  ).run(header.sequence, header.hash, header.parentHash, Math.floor(Date.now() / 1000));
}

export function invalidateLedgerHeadersFrom(db: Database.Database, fromSequence: number): number {
  const result = db
    .prepare('DELETE FROM ledger_headers WHERE sequence >= ?')
    .run(fromSequence);
  return result.changes;
}

export function readPendingReorg(db: Database.Database): ReorgDivergence | null {
  const raw = readState(db, PENDING_REORG_KEY);
  if (!raw) {
    return null;
  }
  try {
    const parsed = JSON.parse(raw) as ReorgDivergence;
    return typeof parsed?.divergenceLedger === 'number' ? parsed : null;
  } catch {
    return null;
  }
}

export function writePendingReorg(db: Database.Database, divergence: ReorgDivergence): void {
  writeState(db, PENDING_REORG_KEY, JSON.stringify(divergence));
}

/** Drop the recorded divergence (the halt latch is cleared separately). */
export function clearPendingReorg(db: Database.Database): void {
  deleteState(db, PENDING_REORG_KEY);
}

/** Clear the ingestion halt latch, resuming writes. */
export function clearHaltLatch(db: Database.Database): void {
  deleteState(db, REORG_HALTED_KEY);
}

/** Clear both the recorded divergence and the halt latch. */
export function clearReorgState(db: Database.Database): void {
  clearPendingReorg(db);
  clearHaltLatch(db);
}

/**
 * Latch the halt flag and persist the divergence.
 *
 * Shared by the live detector and the consistency-job backstop (Issue #866),
 * which detects divergences the ingestion path never saw: whichever
 * component finds one, every process sharing the DB stops ingesting until
 * recovery clears the latch.
 */
export function latchHalt(db: Database.Database, divergence: ReorgDivergence): void {
  writePendingReorg(db, divergence);
  writeState(db, REORG_HALTED_KEY, '1');
}

/** Deepest stored header at or below `sequence`, skipping never-ingested gaps. */
export function maxStoredSequenceBelow(
  db: Database.Database,
  sequence: number
): number | null {
  const row = db
    .prepare('SELECT MAX(sequence) AS s FROM ledger_headers WHERE sequence <= ?')
    .get(sequence) as { s: number | null };
  return row.s ?? null;
}

/**
 * Walk stored headers back from a known-stale sequence until one still
 * matches the canonical chain, returning the first stale ledger (the fork
 * point + 1).
 *
 * The walk steps to the next *stored* header below, so ledgers that never
 * carried a transaction (and therefore have no header) are skipped instead of
 * terminating the search — a sparse header history must not blind the check.
 * It is bounded by `forkMaxDepth` so a fully-diverged history cannot
 * degenerate into a genesis replay, and a header-read failure stops the walk
 * at the last verifiable ledger: infrastructure noise must roll back *less*,
 * never more.
 */
export async function findDivergence(options: FindDivergenceOptions): Promise<ReorgDivergence> {
  const { db, source, staleSequence, reason, detectedBy } = options;
  const forkMaxDepth = options.forkMaxDepth ?? DEFAULT_FORK_MAX_DEPTH;
  const now = options.clock ?? Date.now;

  let cursor = Math.max(staleSequence, 0);
  let forkDepth = 0;
  let deepestStale = cursor;
  let commonAncestor: number | null = null;
  const fetched = new Map<number, LedgerHeader>();

  for (let step = 0; step < forkMaxDepth; step += 1) {
    const stored = getStoredHeader(db, cursor);
    if (stored) {
      let canonical: LedgerHeader;
      try {
        canonical = await source.getLedgerHeader(cursor);
      } catch {
        commonAncestor = cursor;
        break;
      }

      fetched.set(cursor, canonical);
      if (canonical.hash === stored.hash) {
        commonAncestor = cursor;
        break;
      }

      deepestStale = cursor;
      forkDepth += 1;
    }

    const below = maxStoredSequenceBelow(db, cursor - 1);
    if (below === null) {
      // Walked off the bottom of the stored history with every header stale.
      break;
    }
    cursor = below;
  }

  if (commonAncestor === null) {
    commonAncestor = deepestStale - 1;
  }

  const divergenceLedger = Math.max(commonAncestor + 1, 0);
  const indexedHash = getStoredHeader(db, divergenceLedger)?.hash ?? null;

  let observedHash = fetched.get(divergenceLedger)?.hash ?? null;
  if (observedHash === null) {
    try {
      observedHash = (await source.getLedgerHeader(divergenceLedger)).hash;
    } catch {
      observedHash = null;
    }
  }

  const message =
    `ReorgDetected at ledger ${divergenceLedger} (${reason}, detected by ${detectedBy}): ` +
    `indexed hash=${indexedHash ?? 'none'} chain hash=${observedHash ?? 'unreadable'}` +
    (options.observedParentHash
      ? ` chain parent=${options.observedParentHash} indexed parent=${
          getStoredHeader(db, divergenceLedger - 1)?.hash ?? 'none'
        }`
      : '') +
    ` forkDepth=${forkDepth}`;

  return {
    divergenceLedger,
    commonAncestorLedger: Math.max(commonAncestor, -1),
    reason,
    detectedBy,
    detectedAt: new Date(now()).toISOString(),
    indexedHash,
    observedHash,
    observedParentHash: options.observedParentHash ?? null,
    forkDepth,
    message,
  };
}

export interface LedgerReorgDetectorOptions {
  db: Database.Database;
  source: LedgerHeaderSource;
  logger?: Pick<Console, 'info' | 'warn' | 'error'>;
  /**
   * Verify sequence continuity against stored history. Live ingestion leaves
   * this on; checkpoint replay walks backwards over already-indexed ledgers
   * by design, so it turns the check off and relies on the hash/parent checks
   * (which still catch a genuine fork).
   */
  strictSequence?: boolean;
  forkMaxDepth?: number;
  detectedBy?: ReorgOrigin;
  clock?: () => number;
  /**
   * Ignore the persisted halt latch (Issue #864).
   *
   * Recovery replays from the divergence ledger *while* the latch still
   * blocks live ingestion. The replay's detector therefore has to keep
   * verifying and recording headers despite the latch; if it finds a second,
   * newer divergence it still writes the latch, so live ingestion stays
   * blocked afterwards.
   */
  ignoreHaltLatch?: boolean;
}

/**
 * Verifies each newly-ingested ledger against stored history and latches a
 * halt on divergence. One instance per process is expected: the halt flag
 * lives in `indexer_state`, so it survives across instances sharing the DB.
 */
export class LedgerReorgDetector {
  private readonly db: Database.Database;
  private readonly source: LedgerHeaderSource;
  private readonly logger: Pick<Console, 'info' | 'warn' | 'error'>;
  private readonly strictSequence: boolean;
  private readonly forkMaxDepth: number;
  private readonly detectedBy: ReorgOrigin;
  private readonly clock: () => number;
  private readonly ignoreHaltLatch: boolean;
  /** Single-entry memo so a ledger with many transactions is read once. */
  private lastHeader: LedgerHeader | null = null;

  constructor(options: LedgerReorgDetectorOptions) {
    this.db = options.db;
    this.source = options.source;
    this.logger = options.logger ?? console;
    this.strictSequence = options.strictSequence ?? true;
    this.forkMaxDepth = options.forkMaxDepth ?? DEFAULT_FORK_MAX_DEPTH;
    this.detectedBy = options.detectedBy ?? 'ingestion';
    this.clock = options.clock ?? Date.now;
    this.ignoreHaltLatch = options.ignoreHaltLatch ?? false;
  }

  /**
   * Whether ingestion must stay halted pending recovery. Always false for an
   * `ignoreHaltLatch` instance (recovery's own replay), which must keep
   * writing while the live path is blocked.
   */
  isHalted(): boolean {
    if (this.ignoreHaltLatch) {
      return false;
    }
    return readState(this.db, REORG_HALTED_KEY) === '1';
  }

  pendingReorg(): ReorgDivergence | null {
    return readPendingReorg(this.db);
  }

  /**
   * Latch the halt flag and persist the divergence so any other process
   * sharing the DB stops ingesting too.
   */
  halt(divergence: ReorgDivergence): void {
    latchHalt(this.db, divergence);
  }

  /** The chain-truth source this detector verifies against. */
  get headerSource(): LedgerHeaderSource {
    return this.source;
  }

  /** Clear the recorded divergence and the halt latch. */
  clear(): void {
    clearReorgState(this.db);
  }

  getStoredHeader(sequence: number): StoredLedgerHeader | undefined {
    return getStoredHeader(this.db, sequence);
  }

  storeHeader(header: LedgerHeader): void {
    storeLedgerHeader(this.db, header);
  }

  invalidateHeadersFrom(sequence: number): number {
    return invalidateLedgerHeadersFrom(this.db, sequence);
  }

  /**
   * Verify `sequence` against stored history and record its canonical header.
   *
   * Returns the divergence (already halted + persisted) when the ledger does
   * not chain onto stored history, or `null` when it does. Header-read
   * failures are logged and treated as "cannot verify" — an RPC blip must not
   * halt ingestion; the periodic consistency job is the backstop for anything
   * this pass cannot see (Issue #866).
   */
  async observe(sequence: number): Promise<ReorgDivergence | null> {
    if (this.isHalted()) {
      return null;
    }

    let header: LedgerHeader;
    try {
      header = await this.readHeader(sequence);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.logger.warn(
        `reorg check skipped for ledger ${sequence}: header unavailable (${message})`
      );
      return null;
    }

    const stored = this.getStoredHeader(sequence);
    const previous = sequence > 0 ? this.getStoredHeader(sequence - 1) : undefined;

    let staleSequence: number | null = null;
    let reason: ReorgReason | null = null;
    let observedParentHash: string | undefined;

    if (stored && stored.hash !== header.hash) {
      staleSequence = sequence;
      reason = 'ledger_hash_mismatch';
    } else if (previous && previous.hash !== header.parentHash) {
      staleSequence = sequence - 1;
      reason = 'parent_hash_mismatch';
      observedParentHash = header.parentHash;
    } else if (!stored && this.strictSequence) {
      const maxStored = getMaxStoredSequence(this.db);
      if (maxStored !== null && sequence < maxStored) {
        const next = this.getStoredHeader(sequence + 1);
        // Only a genuine chain break: an older ledger that still links to
        // the stored header above it is a normal checkpoint replay.
        if (next && next.parent_hash !== header.hash) {
          staleSequence = sequence + 1;
          reason = 'sequence_regression';
        }
      }
    }

    if (staleSequence === null || reason === null) {
      this.storeHeader(header);
      return null;
    }

    const divergence = await findDivergence({
      db: this.db,
      source: this.source,
      staleSequence,
      reason,
      detectedBy: this.detectedBy,
      observedParentHash: observedParentHash ?? null,
      forkMaxDepth: this.forkMaxDepth,
      clock: this.clock,
    });

    this.halt(divergence);
    return divergence;
  }

  private async readHeader(sequence: number): Promise<LedgerHeader> {
    if (this.lastHeader && this.lastHeader.sequence === sequence) {
      return this.lastHeader;
    }
    const header = await this.source.getLedgerHeader(sequence);
    this.lastHeader = header;
    return header;
  }
}
