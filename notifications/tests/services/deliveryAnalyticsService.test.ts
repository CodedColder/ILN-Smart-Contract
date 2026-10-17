import { describe, expect, it } from 'vitest';
import {
  DeliveryAnalyticsService,
  type DeliveryAnalyticsOptions,
} from '../../src/services/deliveryAnalyticsService';
import { fakeClock } from '../helpers/clock';
import { makeLogger } from '../helpers/logger';
import { waitFor } from '../helpers/wait';

function makeService(options: DeliveryAnalyticsOptions = {}) {
  const clock = fakeClock();
  const { logger, entries } = makeLogger();
  const analytics = new DeliveryAnalyticsService({ now: clock.now, logger, ...options });
  return { analytics, clock, entries };
}

describe('DeliveryAnalyticsService', () => {
  it('correlates circuit trips and rate-limit throttles per subscriber', () => {
    const { analytics, clock } = makeService();

    analytics.record({ type: 'rate_limited', endpointId: 'ep_a', at: clock.now() });
    analytics.record({ type: 'delivery_failure', endpointId: 'ep_a', at: clock.now() + 1, status: 500 });
    clock.advance(10);
    analytics.record({ type: 'circuit_open', endpointId: 'ep_a', at: clock.now() });

    const summary = analytics.summary('ep_a');
    expect(summary).toBeDefined();
    expect(summary?.endpointId).toBe('ep_a');
    expect(summary?.rateLimitThrottles).toBe(1);
    expect(summary?.circuitTrips).toBe(1);
    expect(summary?.deliveryFailures).toBe(1);
    expect(summary?.deliverySuccesses).toBe(0);
    expect(summary?.firstEventAt).toBe(clock.now() - 10);
    expect(summary?.lastEventAt).toBe(clock.now());
    expect(summary?.lastCircuitTripAt).toBe(clock.now());
    expect(summary?.lastRateLimitAt).toBe(clock.now() - 10);
  });

  it('counts successful deliveries and keeps subscribers separate', () => {
    const { analytics, clock } = makeService();

    analytics.record({ type: 'delivery_success', endpointId: 'ep_a', at: clock.now(), status: 200 });
    analytics.record({ type: 'circuit_open', endpointId: 'ep_b', at: clock.now() });

    expect(analytics.summary('ep_a')?.deliverySuccesses).toBe(1);
    expect(analytics.summary('ep_a')?.circuitTrips).toBe(0);
    expect(analytics.summary('ep_b')?.circuitTrips).toBe(1);
    expect(analytics.summary('ep_b')?.lastCircuitTripAt).toBe(clock.now());
    expect(analytics.summary('ep_b')?.lastRateLimitAt).toBeNull();
    expect(analytics.summaries().map((s) => s.endpointId)).toEqual(['ep_a', 'ep_b']);
  });

  it('returns undefined for a subscriber it has never seen', () => {
    const { analytics } = makeService();
    expect(analytics.summary('nope')).toBeUndefined();
    expect(analytics.summaries()).toEqual([]);
  });

  it('restricts counts to the requested window', () => {
    const { analytics, clock } = makeService();

    analytics.record({ type: 'circuit_open', endpointId: 'ep_a', at: clock.now() });
    clock.advance(60_000);
    analytics.record({ type: 'circuit_open', endpointId: 'ep_a', at: clock.now() });
    analytics.record({ type: 'rate_limited', endpointId: 'ep_a', at: clock.now() });

    const all = analytics.countsSince('ep_a', 0);
    expect(all.circuitTrips).toBe(2);
    expect(all.rateLimitThrottles).toBe(1);

    const recent = analytics.countsSince('ep_a', clock.now() - 1_000);
    expect(recent.circuitTrips).toBe(1);
    expect(recent.deliverySuccesses).toBe(0);

    expect(analytics.countsSince('unknown', 0)).toEqual({
      circuitTrips: 0,
      rateLimitThrottles: 0,
      deliveryFailures: 0,
      deliverySuccesses: 0,
    });
  });

  it('resets a subscriber when its history must be discarded', () => {
    const { analytics, clock } = makeService();
    analytics.record({ type: 'circuit_open', endpointId: 'ep_a', at: clock.now() });

    expect(analytics.reset('ep_a')).toBe(true);
    expect(analytics.summary('ep_a')).toBeUndefined();
    expect(analytics.reset('ep_a')).toBe(false);
  });

  it('drops signals older than the retention window', () => {
    const { analytics, clock } = makeService({ retentionMs: 1_000 });

    analytics.record({ type: 'circuit_open', endpointId: 'ep_a', at: clock.now() });
    analytics.record({ type: 'rate_limited', endpointId: 'ep_a', at: clock.now() });
    expect(analytics.summary('ep_a')?.circuitTrips).toBe(1);

    clock.advance(5_000);
    expect(analytics.purge()).toBe(2);
    expect(analytics.summary('ep_a')).toBeUndefined();

    // Purging with nothing due is a no-op.
    expect(analytics.purge()).toBe(0);

    // A fresh signal re-creates the subscriber's summary.
    analytics.record({ type: 'delivery_success', endpointId: 'ep_a', at: clock.now(), status: 200 });
    expect(analytics.summary('ep_a')?.deliverySuccesses).toBe(1);
  });

  it('prunes on record once the clock passes the retention window', () => {
    const { analytics, clock } = makeService({ retentionMs: 1_000 });

    analytics.record({ type: 'circuit_open', endpointId: 'ep_a', at: clock.now() });
    clock.advance(5_000);
    analytics.record({ type: 'rate_limited', endpointId: 'ep_a', at: clock.now() });

    const summary = analytics.summary('ep_a');
    expect(summary?.circuitTrips).toBe(0);
    expect(summary?.rateLimitThrottles).toBe(1);
  });

  it('runs a retention sweep while started and stops it cleanly', async () => {
    const { analytics, clock } = makeService({ retentionMs: 1_000, pruneIntervalMs: 10 });

    expect(analytics.state).toBe('idle');
    await analytics.start();
    expect(analytics.state).toBe('running');

    analytics.record({ type: 'circuit_open', endpointId: 'ep_a', at: clock.now() });
    expect(analytics.summary('ep_a')).toBeDefined();

    clock.advance(5_000);
    await waitFor(() => analytics.summary('ep_a') === undefined);

    await analytics.stop();
    expect(analytics.state).toBe('stopped');
  });

  it('skips the sweep timer when pruning is disabled', async () => {
    const { analytics } = makeService({ pruneIntervalMs: 0 });

    await analytics.start();
    expect(analytics.state).toBe('running');
    await analytics.stop();
    expect(analytics.state).toBe('stopped');
  });
});
