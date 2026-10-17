import { describe, expect, it } from 'vitest';
import { DeliveryAnalyticsService } from '../../src/services/deliveryAnalyticsService';
import {
  SubscriptionHealthService,
  type HealthPolicy,
} from '../../src/services/subscriptionHealthService';
import type { HealthNotice, HealthNotifier } from '../../src/services/healthNotifier';
import { fakeClock, DAY_MS } from '../helpers/clock';
import { makeLogger, type LogEntry } from '../helpers/logger';
import { waitFor } from '../helpers/wait';

interface Harness {
  health: SubscriptionHealthService;
  analytics: DeliveryAnalyticsService;
  clock: ReturnType<typeof fakeClock>;
  notices: HealthNotice[];
  entries: LogEntry[];
}

interface HarnessOptions {
  policy?: Partial<HealthPolicy>;
  /** `false` wires no notifier at all; otherwise a stub or custom one. */
  notifier?: HealthNotifier | false;
  publicUrl?: string;
  evaluateIntervalMs?: number;
}

function makeHarness(options: HarnessOptions = {}): Harness {
  const clock = fakeClock();
  const { logger, entries } = makeLogger();
  const analytics = new DeliveryAnalyticsService({ now: clock.now, logger });
  const notices: HealthNotice[] = [];
  const stub: HealthNotifier = {
    channel: 'email',
    send: async (notice) => {
      notices.push(notice);
      return { ok: true };
    },
  };
  const notifier = options.notifier === false ? undefined : options.notifier ?? stub;

  const health = new SubscriptionHealthService({
    analytics,
    logger,
    now: clock.now,
    notifier,
    ...(options.policy ? { policy: options.policy } : {}),
    ...(options.publicUrl !== undefined ? { publicUrl: options.publicUrl } : {}),
    ...(options.evaluateIntervalMs !== undefined
      ? { evaluateIntervalMs: options.evaluateIntervalMs }
      : {}),
  });

  return { health, analytics, clock, notices, entries };
}

function recordTrips(analytics: DeliveryAnalyticsService, endpointId: string, count: number, at: number): void {
  for (let i = 0; i < count; i++) {
    analytics.record({ type: 'circuit_open', endpointId, at });
  }
}

describe('SubscriptionHealthService policy', () => {
  it('exposes the default "3 trips over 1 day" policy', () => {
    const { health } = makeHarness();
    expect(health.policy).toEqual({
      tripThreshold: 3,
      windowDays: 1,
      autoSuspend: true,
    });
  });

  it('keeps a subscriber healthy below the threshold', async () => {
    const { health, analytics, clock, notices } = makeHarness({
      policy: { tripThreshold: 3, windowDays: 1 },
    });
    recordTrips(analytics, 'ep_1', 2, clock.now());

    const record = await health.evaluate('ep_1');

    expect(record.status).toBe('healthy');
    expect(record.circuitTrips).toBe(2);
    expect(record.reason).toBeNull();
    expect(record.flaggedAt).toBeNull();
    expect(record.suspendedAt).toBeNull();
    expect(health.get('ep_1')).toBe(record);
    expect(health.list()).toEqual([record]);
    expect(health.isSuspended('ep_1')).toBe(false);
    expect(notices).toEqual([]);
  });

  it('flags (but does not suspend) when auto-suspend is disabled', async () => {
    const { health, analytics, clock, notices } = makeHarness({
      policy: { tripThreshold: 3, windowDays: 1, autoSuspend: false },
    });
    const at = clock.now();
    recordTrips(analytics, 'ep_1', 3, at);

    const record = await health.evaluate('ep_1');

    expect(record.status).toBe('flagged');
    expect(record.reason).toBe('circuit_trips 3 >= 3 over 1d');
    expect(record.flaggedAt).toBe(at);
    expect(record.suspendedAt).toBeNull();
    expect(notices).toHaveLength(1);
    expect(notices[0]).toMatchObject({
      subscriberId: 'ep_1',
      status: 'flagged',
      circuitTrips: 3,
      tripThreshold: 3,
      windowDays: 1,
      // No publicUrl configured, so the notifier builds the link itself.
      reactivationUrl: null,
    });

    // Re-evaluating a stable subscription does not re-notify.
    await health.evaluate('ep_1');
    expect(notices).toHaveLength(1);
  });

  it('suspends, notifies once, and builds the reactivation link', async () => {
    const { health, analytics, clock, notices } = makeHarness({
      publicUrl: 'https://notifications.iln.dev',
    });
    const at = clock.now();
    recordTrips(analytics, 'ep_1', 3, at);

    const record = await health.evaluate('ep_1');

    expect(record.status).toBe('suspended');
    expect(health.isSuspended('ep_1')).toBe(true);
    expect(record.flaggedAt).toBe(at);
    expect(record.suspendedAt).toBe(at);
    expect(notices).toHaveLength(1);
    expect(notices[0]?.status).toBe('suspended');
    expect(notices[0]?.reactivationUrl).toBe(
      'https://notifications.iln.dev/subscriptions/health/ep_1/reactivate',
    );

    await health.evaluate('ep_1');
    expect(notices).toHaveLength(1);
  });

  it('stays suspended until reactivated, even after trips age out', async () => {
    const { health, analytics, clock, notices } = makeHarness({
      publicUrl: 'https://notifications.iln.dev',
    });
    recordTrips(analytics, 'ep_1', 3, clock.now());
    const suspended = await health.evaluate('ep_1');
    expect(suspended.status).toBe('suspended');

    clock.advance(2 * DAY_MS);
    const later = await health.evaluate('ep_1');

    expect(later.status).toBe('suspended');
    expect(later.circuitTrips).toBe(0);
    expect(later.suspendedAt).toBe(suspended.suspendedAt);
    expect(later.flaggedAt).toBe(suspended.flaggedAt);
    // The original trigger is preserved for operators.
    expect(later.reason).toBe('circuit_trips 3 >= 3 over 1d');
    expect(notices).toHaveLength(1);
  });

  it('reactivates a suspended subscription and forgets the old trips', async () => {
    const { health, analytics, clock, notices } = makeHarness();
    recordTrips(analytics, 'ep_1', 3, clock.now());
    await health.evaluate('ep_1');
    expect(health.isSuspended('ep_1')).toBe(true);

    const reactivated = await health.reactivate('ep_1');

    expect(reactivated?.status).toBe('healthy');
    expect(reactivated?.circuitTrips).toBe(0);
    expect(reactivated?.flaggedAt).toBeNull();
    expect(reactivated?.suspendedAt).toBeNull();
    expect(health.isSuspended('ep_1')).toBe(false);
    expect(analytics.summary('ep_1')).toBeUndefined();

    // History is gone, so the very next evaluation starts from zero.
    const next = await health.evaluate('ep_1');
    expect(next.status).toBe('healthy');
    expect(next.circuitTrips).toBe(0);
    expect(notices).toHaveLength(1);
  });

  it('returns undefined when reactivating an unknown subscriber', async () => {
    const { health } = makeHarness();
    expect(await health.reactivate('nope')).toBeUndefined();
  });

  it('evaluates every subscriber analytics has seen plus its own records', async () => {
    const { health, analytics, clock } = makeHarness();
    recordTrips(analytics, 'ep_1', 1, clock.now());
    await health.evaluate('ep_2');

    const records = await health.evaluateAll();

    expect(records.map((r) => r.subscriberId).sort()).toEqual(['ep_1', 'ep_2']);
    expect(records.find((r) => r.subscriberId === 'ep_1')?.circuitTrips).toBe(1);
    expect(records.find((r) => r.subscriberId === 'ep_2')?.status).toBe('healthy');
  });
});

describe('SubscriptionHealthService notifications', () => {
  it('logs but does not fail when no notifier is configured', async () => {
    const { health, analytics, clock, entries } = makeHarness({ notifier: false });
    recordTrips(analytics, 'ep_1', 3, clock.now());

    const record = await health.evaluate('ep_1');

    expect(record.status).toBe('suspended');
    expect(entries).toContainEqual(
      expect.objectContaining({ level: 'warn', msg: 'subscription_flagged' }),
    );
    expect(entries).toContainEqual(
      expect.objectContaining({ level: 'warn', msg: 'health_notice_skipped' }),
    );
  });

  it('keeps the decision when the notifier throws', async () => {
    const failing: HealthNotifier = {
      channel: 'email',
      send: async () => {
        throw new Error('smtp down');
      },
    };
    const { health, analytics, clock, entries } = makeHarness({ notifier: failing });
    recordTrips(analytics, 'ep_1', 3, clock.now());
    await health.start();

    const record = await health.evaluate('ep_1');

    expect(record.status).toBe('suspended');
    expect(health.state).toBe('running');
    expect(entries).toContainEqual(
      expect.objectContaining({
        level: 'error',
        msg: 'service_operation_failed',
        fields: expect.objectContaining({ operation: 'health_notice' }),
      }),
    );
    await health.stop();
  });

  it('logs a rejected notice without failing the evaluation', async () => {
    const rejecting: HealthNotifier = {
      channel: 'email',
      send: async () => ({ ok: false, error: 'no_contact_on_file' }),
    };
    const { health, analytics, clock, entries } = makeHarness({ notifier: rejecting });
    recordTrips(analytics, 'ep_1', 3, clock.now());

    await health.evaluate('ep_1');

    expect(entries).toContainEqual(
      expect.objectContaining({
        level: 'warn',
        msg: 'health_notice_failed',
        fields: expect.objectContaining({ error: 'no_contact_on_file' }),
      }),
    );
  });
});

describe('SubscriptionHealthService lifecycle', () => {
  it('sweeps subscribers on its timer until stopped', async () => {
    const { health, analytics, clock } = makeHarness({ evaluateIntervalMs: 5 });
    recordTrips(analytics, 'ep_1', 3, clock.now());

    expect(health.state).toBe('idle');
    await health.start();
    expect(health.state).toBe('running');

    await waitFor(() => health.get('ep_1') !== undefined);
    expect(health.get('ep_1')?.status).toBe('suspended');

    await health.stop();
    expect(health.state).toBe('stopped');
  });

  it('skips the sweep timer when evaluation is disabled', async () => {
    const { health } = makeHarness({ evaluateIntervalMs: 0 });

    await health.start();
    expect(health.state).toBe('running');
    await health.stop();
    expect(health.state).toBe('stopped');
  });
});
