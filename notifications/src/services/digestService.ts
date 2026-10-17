import { BaseService } from './baseService.js';
import type { ServiceContext } from './types.js';

/**
 * Delivery mode for one subscriber.
 *
 * `digest` buffers notifications and delivers them as one batch when the
 * window closes; `realtime` (the default) hands every notification straight
 * back to the caller — opting **out** of the digest never means going quiet
 * (Issue #871).
 */
export type DigestMode = 'realtime' | 'digest';

export interface DigestNotification {
  event: string;
  invoiceId: number;
  payload: unknown;
  queuedAt: number;
}

/** One batched delivery covering a subscriber's window. */
export interface DigestBatch {
  subscriberId: string;
  notifications: DigestNotification[];
  /** When the batch's window opened (first queued notification). */
  windowStart: number;
}

export interface EnqueueResult {
  /** false ⇒ deliver this notification immediately (real-time path). */
  batched: boolean;
}

export interface DigestSubscriptionView {
  subscriberId: string;
  mode: DigestMode;
  pending: number;
  due: boolean;
}

export type DigestDeliverFn = (batch: DigestBatch) => Promise<void> | void;

export interface DigestServiceOptions extends ServiceContext {
  /** Batching window. Defaults to 5 minutes. */
  windowMs?: number | undefined;
  /** A batch is released as soon as it reaches this many notifications. */
  maxBatchSize?: number | undefined;
  /** Mode for subscribers that never chose one. Defaults to `realtime`. */
  defaultMode?: DigestMode | undefined;
  /** How often due batches are checked. Defaults to half the window. */
  flushIntervalMs?: number | undefined;
  /** Sink that delivers a finished batch (typically `WebhookDeliveryService`). */
  deliver?: DigestDeliverFn | undefined;
}

const DEFAULT_WINDOW_MS = 5 * 60 * 1000;
const DEFAULT_MAX_BATCH_SIZE = 100;

interface DigestEntry {
  mode: DigestMode;
  notifications: DigestNotification[];
  windowStart: number;
}

/**
 * Batches queued notifications per subscriber over a configurable window.
 *
 * High-volume subscribers opt in to reduce delivery chatter; everyone else
 * keeps the real-time path. Buffers live in memory (same trade-off as
 * `DeliveryHistoryStore`) and are drained to the `deliver` sink on shutdown
 * so a restart does not silently drop queued notifications.
 */
export class DigestService extends BaseService {
  private readonly entries = new Map<string, DigestEntry>();
  private readonly windowMs: number;
  private readonly maxBatchSize: number;
  private readonly defaultMode: DigestMode;
  private readonly flushIntervalMs: number;
  private readonly deliver: DigestDeliverFn | undefined;
  private flushTimer: NodeJS.Timeout | null = null;

  constructor(options: DigestServiceOptions = {}) {
    super('digest', options);
    this.windowMs = options.windowMs ?? DEFAULT_WINDOW_MS;
    this.maxBatchSize = options.maxBatchSize ?? DEFAULT_MAX_BATCH_SIZE;
    this.defaultMode = options.defaultMode ?? 'realtime';
    this.flushIntervalMs =
      options.flushIntervalMs ?? Math.max(1, Math.floor(this.windowMs / 2));
    this.deliver = options.deliver;
  }

  /** Effective mode: the subscriber's choice, else the service default. */
  mode(subscriberId: string): DigestMode {
    return this.entries.get(subscriberId)?.mode ?? this.defaultMode;
  }

  optIn(subscriberId: string): DigestMode {
    return this.setMode(subscriberId, 'digest');
  }

  optOut(subscriberId: string): DigestMode {
    return this.setMode(subscriberId, 'realtime');
  }

  setMode(subscriberId: string, mode: DigestMode): DigestMode {
    const entry =
      this.entries.get(subscriberId) ??
      { mode: this.defaultMode, notifications: [], windowStart: 0 };
    entry.mode = mode;
    this.entries.set(subscriberId, entry);
    this.log.info('digest_mode_set', { subscriberId, mode });
    return mode;
  }

  /**
   * Queue one notification.
   *
   * @returns `{ batched: false }` when the subscriber is on the real-time
   * path — the caller must deliver immediately. `{ batched: true }` means the
   * notification is buffered and will be released by `flushDue`.
   */
  enqueue(
    subscriberId: string,
    input: { event: string; invoiceId: number; payload: unknown },
  ): EnqueueResult {
    const existing = this.entries.get(subscriberId);
    const mode = existing?.mode ?? this.defaultMode;
    if (mode !== 'digest') {
      this.log.debug('digest_bypassed', { subscriberId, mode });
      return { batched: false };
    }

    const entry = existing ?? { mode: 'digest', notifications: [], windowStart: 0 };
    if (!existing) {
      this.entries.set(subscriberId, entry);
    }
    if (entry.notifications.length === 0) {
      entry.windowStart = this.now();
    }
    entry.notifications.push({ ...input, queuedAt: this.now() });
    this.log.debug('digest_queued', {
      subscriberId,
      pending: entry.notifications.length,
    });
    return { batched: true };
  }

  /** Buffered notifications for one subscriber (a copy; safe to mutate). */
  pending(subscriberId: string): DigestNotification[] {
    return [...(this.entries.get(subscriberId)?.notifications ?? [])];
  }

  /** Every subscriber the service has a preference or buffer for. */
  list(): DigestSubscriptionView[] {
    const out: DigestSubscriptionView[] = [];
    for (const [subscriberId, entry] of this.entries) {
      out.push({
        subscriberId,
        mode: entry.mode,
        pending: entry.notifications.length,
        due: this.isReady(entry, this.now()),
      });
    }
    return out;
  }

  /**
   * Release every batch whose window has closed (or that reached
   * `maxBatchSize`). Entries are cleared as they are taken, so a batch is
   * never delivered twice.
   */
  flushDue(now?: number): DigestBatch[] {
    const at = now ?? this.now();
    const batches: DigestBatch[] = [];
    for (const [subscriberId, entry] of [...this.entries]) {
      if (this.isReady(entry, at)) {
        batches.push({
          subscriberId,
          notifications: entry.notifications,
          windowStart: entry.windowStart,
        });
        entry.notifications = [];
        entry.windowStart = 0;
      }
    }
    if (batches.length > 0) {
      this.log.debug('digest_batches_released', { batches: batches.length });
    }
    return batches;
  }

  protected onStart(): void {
    const deliver = this.deliver;
    if (!deliver) {
      this.log.debug('digest_timer_skipped', { reason: 'no_deliver_sink' });
      return;
    }
    this.flushTimer = setInterval(() => {
      void this.flushAndDeliver(this.flushDue(), deliver);
    }, this.flushIntervalMs);
    // Never hold the process open for a batching window.
    this.flushTimer.unref();
  }

  protected async onStop(): Promise<void> {
    if (this.flushTimer) {
      clearInterval(this.flushTimer);
      this.flushTimer = null;
    }
    // Drain what is queued rather than dropping it on shutdown — but only
    // when there is somewhere to drain to.
    if (this.deliver) {
      await this.flushAndDeliver(this.drain(), this.deliver);
    }
  }

  private isReady(entry: DigestEntry, at: number): boolean {
    if (entry.notifications.length === 0) {
      return false;
    }
    return (
      entry.notifications.length >= this.maxBatchSize ||
      at - entry.windowStart >= this.windowMs
    );
  }

  private drain(): DigestBatch[] {
    const batches: DigestBatch[] = [];
    for (const [subscriberId, entry] of this.entries) {
      if (entry.notifications.length === 0) {
        continue;
      }
      batches.push({
        subscriberId,
        notifications: entry.notifications,
        windowStart: entry.windowStart,
      });
      entry.notifications = [];
      entry.windowStart = 0;
    }
    return batches;
  }

  private async flushAndDeliver(batches: DigestBatch[], deliver: DigestDeliverFn): Promise<void> {
    for (const batch of batches) {
      await this.runSafely('digest_deliver', () => deliver(batch), undefined);
    }
  }
}
