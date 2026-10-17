import { BaseService } from './baseService.js';
import type { DeliveryEvent, DeliveryEventSink } from '../delivery/deliveryEvents.js';
import type { ServiceContext } from './types.js';

const DAY_MS = 24 * 60 * 60 * 1000;
const DEFAULT_RETENTION_MS = 30 * DAY_MS;
const DEFAULT_PRUNE_INTERVAL_MS = 60 * 60 * 1000;

/** Windowed counts for one subscriber. */
export interface DeliveryCounts {
  circuitTrips: number;
  rateLimitThrottles: number;
  deliveryFailures: number;
  deliverySuccesses: number;
}

/**
 * Per-subscriber failure summary produced by correlating circuit-breaker
 * trips with rate-limiter throttle events (Issue #872).
 */
export interface SubscriberFailureSummary extends DeliveryCounts {
  endpointId: string;
  firstEventAt: number;
  lastEventAt: number;
  lastCircuitTripAt: number | null;
  lastRateLimitAt: number | null;
}

export interface DeliveryAnalyticsOptions extends ServiceContext {
  /**
   * How long raw delivery signals are retained before being pruned.
   * Defaults to 30 days, matching the delivery-history record retention.
   */
  retentionMs?: number | undefined;
  /**
   * How often the retention sweep runs. `0` disables the timer entirely
   * (tests, or a caller that drives `purge()` itself).
   */
  pruneIntervalMs?: number | undefined;
}

function emptyCounts(): DeliveryCounts {
  return {
    circuitTrips: 0,
    rateLimitThrottles: 0,
    deliveryFailures: 0,
    deliverySuccesses: 0,
  };
}

function applyEvent(counts: DeliveryCounts, event: DeliveryEvent): void {
  switch (event.type) {
    case 'circuit_open':
      counts.circuitTrips += 1;
      break;
    case 'rate_limited':
      counts.rateLimitThrottles += 1;
      break;
    case 'delivery_success':
      counts.deliverySuccesses += 1;
      break;
    case 'delivery_failure':
      counts.deliveryFailures += 1;
      break;
  }
}

/**
 * Aggregates delivery signals into per-subscriber failure summaries.
 *
 * The service is the `DeliveryEventSink` handed to `WebhookDeliveryService`,
 * so it observes trips and throttles as they happen without the delivery
 * layer knowing anything about analytics. Signals are kept in memory for
 * `retentionMs` (in-memory store, same trade-off as `DeliveryHistoryStore`),
 * which is what the subscription-health policy windows against.
 */
export class DeliveryAnalyticsService extends BaseService implements DeliveryEventSink {
  private readonly events = new Map<string, DeliveryEvent[]>();
  private readonly retentionMs: number;
  private readonly pruneIntervalMs: number;
  private pruneTimer: NodeJS.Timeout | null = null;

  constructor(options: DeliveryAnalyticsOptions = {}) {
    super('delivery-analytics', options);
    this.retentionMs = options.retentionMs ?? DEFAULT_RETENTION_MS;
    this.pruneIntervalMs = options.pruneIntervalMs ?? DEFAULT_PRUNE_INTERVAL_MS;
  }

  /** Record one delivery signal. Called synchronously from the delivery path. */
  record(event: DeliveryEvent): void {
    const existing = this.events.get(event.endpointId);
    if (existing) {
      existing.push(event);
      // A just-recorded event cannot itself be expired, so only the older
      // ones in this list need the retention check.
      this.pruneList(existing, this.now() - this.retentionMs);
    } else {
      this.events.set(event.endpointId, [event]);
    }
  }

  summary(endpointId: string): SubscriberFailureSummary | undefined {
    const list = this.events.get(endpointId);
    if (!list) {
      return undefined;
    }
    return this.summarize(endpointId, list);
  }

  /** Summaries for every subscriber with at least one retained signal. */
  summaries(): SubscriberFailureSummary[] {
    const out: SubscriberFailureSummary[] = [];
    for (const [endpointId, list] of this.events) {
      out.push(this.summarize(endpointId, list));
    }
    return out;
  }

  /**
   * Counts for one subscriber restricted to `[since, now]`.
   *
   * This is the windowing primitive the subscription-health policy uses to
   * ask "how many circuit trips in the last M days?".
   */
  countsSince(endpointId: string, since: number): DeliveryCounts {
    const list = this.events.get(endpointId) ?? [];
    const counts = emptyCounts();
    for (const event of list) {
      if (event.at >= since) {
        applyEvent(counts, event);
      }
    }
    return counts;
  }

  /**
   * Drop every retained signal for a subscriber.
   *
   * Used when a subscription is manually reactivated (Issue #873): old trips
   * must not instantly re-trigger the threshold that caused the suspension.
   *
   * @returns true when there was something to drop.
   */
  reset(endpointId: string): boolean {
    return this.events.delete(endpointId);
  }

  /** Apply the retention policy now. @returns how many signals were dropped. */
  purge(): number {
    let removed = 0;
    const cutoff = this.now() - this.retentionMs;
    for (const [endpointId, list] of [...this.events]) {
      removed += this.pruneList(list, cutoff);
      if (list.length === 0) {
        this.events.delete(endpointId);
      }
    }
    return removed;
  }

  protected onStart(): void {
    if (this.pruneIntervalMs <= 0) {
      return;
    }
    this.pruneTimer = setInterval(
      () => {
        void this.runSafely('analytics_purge', () => this.purge(), 0);
      },
      this.pruneIntervalMs,
    );
    // Never hold the process open for a maintenance sweep.
    this.pruneTimer.unref();
  }

  protected onStop(): void {
    if (this.pruneTimer) {
      clearInterval(this.pruneTimer);
      this.pruneTimer = null;
    }
  }

  private summarize(endpointId: string, list: DeliveryEvent[]): SubscriberFailureSummary {
    const counts = emptyCounts();
    let firstEventAt = Number.POSITIVE_INFINITY;
    let lastEventAt = 0;
    let lastCircuitTripAt: number | null = null;
    let lastRateLimitAt: number | null = null;

    for (const event of list) {
      applyEvent(counts, event);
      firstEventAt = Math.min(firstEventAt, event.at);
      lastEventAt = Math.max(lastEventAt, event.at);
      if (event.type === 'circuit_open') {
        lastCircuitTripAt = Math.max(lastCircuitTripAt ?? 0, event.at);
      }
      if (event.type === 'rate_limited') {
        lastRateLimitAt = Math.max(lastRateLimitAt ?? 0, event.at);
      }
    }

    return {
      endpointId,
      ...counts,
      firstEventAt,
      lastEventAt,
      lastCircuitTripAt,
      lastRateLimitAt,
    };
  }

  private pruneList(list: DeliveryEvent[], cutoff: number): number {
    let kept = 0;
    for (const event of list) {
      if (event.at >= cutoff) {
        list[kept] = event;
        kept += 1;
      }
    }
    const removed = list.length - kept;
    list.length = kept;
    return removed;
  }
}
