import { BaseService } from './baseService.js';
import type { DeliveryAnalyticsService } from './deliveryAnalyticsService.js';
import type { HealthNotice, HealthNotifier } from './healthNotifier.js';
import type { ServiceContext } from './types.js';

const DAY_MS = 24 * 60 * 60 * 1000;

export type SubscriptionHealthStatus = 'healthy' | 'flagged' | 'suspended';

/** Health record for one subscriber, produced by the threshold policy. */
export interface SubscriptionHealth {
  subscriberId: string;
  status: SubscriptionHealthStatus;
  /** Circuit trips counted inside the policy window at last evaluation. */
  circuitTrips: number;
  tripThreshold: number;
  windowDays: number;
  reason: string | null;
  flaggedAt: number | null;
  suspendedAt: number | null;
  updatedAt: number;
}

/** "N circuit trips over M days" (Issue #873). */
export interface HealthPolicy {
  tripThreshold: number;
  windowDays: number;
  /** Escalate a met threshold straight to `suspended`. */
  autoSuspend: boolean;
}

export interface SubscriptionHealthOptions extends ServiceContext {
  /** Source of circuit-trip counts — the analytics summaries. */
  analytics: DeliveryAnalyticsService;
  /** Threshold policy; unset fields fall back to the defaults below. */
  policy?: Partial<HealthPolicy> | undefined;
  /** Non-webhook channel used to tell the subscriber about a decision. */
  notifier?: HealthNotifier | undefined;
  /** Base URL used to build the reactivation link in notices. */
  publicUrl?: string | undefined;
  /** How often the policy sweeps all subscribers. `0` disables the sweep. */
  evaluateIntervalMs?: number | undefined;
}

const DEFAULT_TRIP_THRESHOLD = 3;
const DEFAULT_WINDOW_DAYS = 1;
const DEFAULT_EVALUATE_INTERVAL_MS = 5 * 60 * 1000;

/**
 * Turns delivery analytics into subscription state.
 *
 * Policy: `tripThreshold` circuit trips within `windowDays` flags the
 * subscription, and suspends it when `autoSuspend` is on. Suspension is
 * sticky — only {@link reactivate} clears it — so a subscriber cannot flip
 * back to healthy the moment the window slides, and reactivation resets the
 * analytics counters so the same historical trips do not re-trigger.
 */
export class SubscriptionHealthService extends BaseService {
  private readonly records = new Map<string, SubscriptionHealth>();
  private readonly analytics: DeliveryAnalyticsService;
  private readonly policyValue: HealthPolicy;
  private readonly notifier: HealthNotifier | undefined;
  private readonly publicUrl: string | undefined;
  private readonly evaluateIntervalMs: number;
  private sweepTimer: NodeJS.Timeout | null = null;

  constructor(options: SubscriptionHealthOptions) {
    super('subscription-health', options);
    this.analytics = options.analytics;
    this.policyValue = {
      tripThreshold: options.policy?.tripThreshold ?? DEFAULT_TRIP_THRESHOLD,
      windowDays: options.policy?.windowDays ?? DEFAULT_WINDOW_DAYS,
      autoSuspend: options.policy?.autoSuspend ?? true,
    };
    this.notifier = options.notifier;
    this.publicUrl = options.publicUrl;
    this.evaluateIntervalMs =
      options.evaluateIntervalMs ?? DEFAULT_EVALUATE_INTERVAL_MS;
  }

  get policy(): HealthPolicy {
    return { ...this.policyValue };
  }

  get(subscriberId: string): SubscriptionHealth | undefined {
    return this.records.get(subscriberId);
  }

  list(): SubscriptionHealth[] {
    return [...this.records.values()];
  }

  /** Consulted by `WebhookDeliveryService` before it spends any budget. */
  isSuspended(subscriberId: string): boolean {
    return this.records.get(subscriberId)?.status === 'suspended';
  }

  /**
   * Apply the threshold policy to one subscriber.
   *
   * The returned record is always up to date; a transition into `flagged` or
   * `suspended` is announced once, over the non-webhook channel.
   */
  async evaluate(subscriberId: string): Promise<SubscriptionHealth> {
    const now = this.now();
    const { tripThreshold, windowDays, autoSuspend } = this.policyValue;
    const counts = this.analytics.countsSince(
      subscriberId,
      now - windowDays * DAY_MS,
    );
    const previous = this.records.get(subscriberId);
    const thresholdMet = counts.circuitTrips >= tripThreshold;
    const reason = `circuit_trips ${counts.circuitTrips} >= ${tripThreshold} over ${windowDays}d`;

    let status: SubscriptionHealthStatus;
    if (previous?.status === 'suspended') {
      // Sticky: only manual reactivation clears a suspension.
      status = 'suspended';
    } else if (thresholdMet && autoSuspend) {
      status = 'suspended';
    } else if (thresholdMet) {
      status = 'flagged';
    } else {
      status = 'healthy';
    }

    const record: SubscriptionHealth = {
      subscriberId,
      status,
      circuitTrips: counts.circuitTrips,
      tripThreshold,
      windowDays,
      reason: thresholdMet
        ? reason
        : // A sticky suspension whose window has rolled over keeps the
          // trigger that caused it (`previous` is always set on that path).
          status === 'suspended'
          ? previous!.reason
          : null,
      flaggedAt: status === 'healthy' ? null : previous?.flaggedAt ?? now,
      suspendedAt:
        status === 'suspended' ? previous?.suspendedAt ?? now : null,
      updatedAt: now,
    };
    this.records.set(subscriberId, record);

    const previousStatus = previous?.status ?? 'healthy';
    if (previousStatus !== status && status !== 'healthy') {
      this.log.warn('subscription_flagged', {
        subscriberId,
        status,
        circuitTrips: counts.circuitTrips,
        tripThreshold,
        windowDays,
      });
      await this.sendNotice({
        subscriberId,
        status,
        reason,
        circuitTrips: counts.circuitTrips,
        tripThreshold,
        windowDays,
        at: now,
        reactivationUrl: this.reactivationUrlFor(subscriberId),
      });
    }

    return record;
  }

  /** Evaluate every subscriber the analytics layer has seen. */
  async evaluateAll(): Promise<SubscriptionHealth[]> {
    const ids = new Set<string>(this.records.keys());
    for (const summary of this.analytics.summaries()) {
      ids.add(summary.endpointId);
    }
    const out: SubscriptionHealth[] = [];
    for (const id of ids) {
      out.push(await this.evaluate(id));
    }
    return out;
  }

  /**
   * Manual reactivation (Issue #873): clears the flag/suspension and drops
   * the trips that caused it, so a subscriber that fixed its endpoint starts
   * from a clean window instead of being re-suspended by history.
   *
   * @returns the updated record, or undefined when the subscriber is unknown.
   */
  async reactivate(subscriberId: string): Promise<SubscriptionHealth | undefined> {
    const previous = this.records.get(subscriberId);
    if (!previous) {
      return undefined;
    }
    this.analytics.reset(subscriberId);
    const next: SubscriptionHealth = {
      ...previous,
      status: 'healthy',
      circuitTrips: 0,
      reason: null,
      flaggedAt: null,
      suspendedAt: null,
      updatedAt: this.now(),
    };
    this.records.set(subscriberId, next);
    this.log.info('subscription_reactivated', {
      subscriberId,
      previousStatus: previous.status,
    });
    return next;
  }

  protected onStart(): void {
    if (this.evaluateIntervalMs <= 0) {
      return;
    }
    this.sweepTimer = setInterval(
      () => {
        void this.runSafely('health_sweep', () => this.evaluateAll(), []);
      },
      this.evaluateIntervalMs,
    );
    // Never hold the process open for a policy sweep.
    this.sweepTimer.unref();
  }

  protected onStop(): void {
    if (this.sweepTimer) {
      clearInterval(this.sweepTimer);
      this.sweepTimer = null;
    }
  }

  private reactivationUrlFor(subscriberId: string): string | null {
    if (!this.publicUrl) {
      return null;
    }
    return `${this.publicUrl}/subscriptions/health/${encodeURIComponent(
      subscriberId,
    )}/reactivate`;
  }

  private async sendNotice(notice: HealthNotice): Promise<void> {
    const notifier = this.notifier;
    if (!notifier) {
      this.log.warn('health_notice_skipped', {
        subscriberId: notice.subscriberId,
        status: notice.status,
        reason: 'no_notifier_configured',
      });
      return;
    }

    const result = await this.runSafely(
      'health_notice',
      () => notifier.send(notice),
      { ok: false, error: 'notice_failed' },
    );
    if (result.ok) {
      this.log.info('health_notice_sent', {
        subscriberId: notice.subscriberId,
        status: notice.status,
        channel: notifier.channel,
      });
    } else {
      this.log.warn('health_notice_failed', {
        subscriberId: notice.subscriberId,
        status: notice.status,
        channel: notifier.channel,
        error: result.error,
      });
    }
  }
}
