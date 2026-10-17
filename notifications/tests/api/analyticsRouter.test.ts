import express from 'express';
import request from 'supertest';
import { describe, expect, it } from 'vitest';
import { createAnalyticsRouter } from '../../src/api/analytics';
import { DeliveryAnalyticsService } from '../../src/services/deliveryAnalyticsService';
import { fakeClock } from '../helpers/clock';

function makeApp() {
  const clock = fakeClock();
  const analytics = new DeliveryAnalyticsService({ now: clock.now });
  const app = express();
  app.use(express.json());
  app.use(createAnalyticsRouter(analytics));
  return { app, analytics, clock };
}

describe('analytics router', () => {
  it('lists no summaries before anything is recorded', async () => {
    const { app } = makeApp();
    const res = await request(app).get('/analytics/deliveries');

    expect(res.status).toBe(200);
    expect(res.body).toEqual({ items: [] });
  });

  it('lists per-subscriber failure summaries', async () => {
    const { app, analytics, clock } = makeApp();
    analytics.record({ type: 'circuit_open', endpointId: 'ep_1', at: clock.now() });
    analytics.record({ type: 'rate_limited', endpointId: 'ep_1', at: clock.now() });

    const res = await request(app).get('/analytics/deliveries');

    expect(res.status).toBe(200);
    expect(res.body.items).toHaveLength(1);
    expect(res.body.items[0]).toMatchObject({
      endpointId: 'ep_1',
      circuitTrips: 1,
      rateLimitThrottles: 1,
    });
  });

  it('returns a single summary or 404', async () => {
    const { app, analytics, clock } = makeApp();
    analytics.record({ type: 'delivery_success', endpointId: 'ep_1', at: clock.now(), status: 200 });

    const missing = await request(app).get('/analytics/deliveries/nope');
    expect(missing.status).toBe(404);
    expect(missing.body).toEqual({ error: 'not_found' });

    const found = await request(app).get('/analytics/deliveries/ep_1');
    expect(found.status).toBe(200);
    expect(found.body.endpointId).toBe('ep_1');
    expect(found.body.deliverySuccesses).toBe(1);
  });

  it('returns all-time counts when no window is requested', async () => {
    const { app, analytics, clock } = makeApp();
    analytics.record({ type: 'circuit_open', endpointId: 'ep_1', at: clock.now() });
    analytics.record({ type: 'rate_limited', endpointId: 'ep_1', at: clock.now() });

    const res = await request(app).get('/analytics/deliveries/ep_1/counts');

    expect(res.status).toBe(200);
    expect(res.body).toEqual({
      circuitTrips: 1,
      rateLimitThrottles: 1,
      deliveryFailures: 0,
      deliverySuccesses: 0,
    });
  });

  it('restricts counts to the requested window', async () => {
    const { app, analytics, clock } = makeApp();
    analytics.record({ type: 'circuit_open', endpointId: 'ep_1', at: clock.now() });
    const cutoff = clock.now() + 1_000;
    analytics.record({ type: 'circuit_open', endpointId: 'ep_1', at: cutoff });

    const res = await request(app).get(`/analytics/deliveries/ep_1/counts?since=${cutoff}`);

    expect(res.status).toBe(200);
    expect(res.body.circuitTrips).toBe(1);
  });

  it('rejects a malformed window and unknown subscribers', async () => {
    const { app, analytics, clock } = makeApp();
    analytics.record({ type: 'circuit_open', endpointId: 'ep_1', at: clock.now() });

    const bad = await request(app).get('/analytics/deliveries/ep_1/counts?since=yesterday');
    expect(bad.status).toBe(400);
    expect(bad.body).toEqual({ error: 'invalid_since' });

    const missing = await request(app).get('/analytics/deliveries/nope/counts');
    expect(missing.status).toBe(404);
    expect(missing.body).toEqual({ error: 'not_found' });
  });
});
