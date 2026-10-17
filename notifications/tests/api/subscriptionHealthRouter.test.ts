import express from 'express';
import request from 'supertest';
import { describe, expect, it } from 'vitest';
import { createSubscriptionHealthRouter } from '../../src/api/subscriptionHealth';
import { DeliveryAnalyticsService } from '../../src/services/deliveryAnalyticsService';
import { SubscriptionHealthService } from '../../src/services/subscriptionHealthService';
import type { HealthNotice } from '../../src/services/healthNotifier';
import { SubscriptionStore } from '../../src/subscriptions/subscriptionStore';
import { fakeClock } from '../helpers/clock';
import { makeLogger } from '../helpers/logger';

function makeApp(withStore = false) {
  const clock = fakeClock();
  const { logger } = makeLogger();
  const analytics = new DeliveryAnalyticsService({ now: clock.now, logger });
  const notices: HealthNotice[] = [];
  const health = new SubscriptionHealthService({
    analytics,
    logger,
    now: clock.now,
    notifier: {
      channel: 'email',
      send: async (notice) => {
        notices.push(notice);
        return { ok: true };
      },
    },
  });
  const store = withStore ? new SubscriptionStore() : undefined;
  if (store) {
    store.create({
      endpointId: 'ep_1',
      url: 'https://example.com/hook',
      secret: 'secret_1',
      eventTypes: ['invoice.paid'],
    });
  }
  const app = express();
  app.use(express.json());
  app.use(createSubscriptionHealthRouter(health, store));
  return { app, analytics, health, clock, notices };
}

describe('subscription health router', () => {
  it('lists health records', async () => {
    const { app, health } = makeApp();

    const empty = await request(app).get('/subscriptions/health');
    expect(empty.status).toBe(200);
    expect(empty.body).toEqual({ items: [] });

    await health.evaluate('ep_1');

    const populated = await request(app).get('/subscriptions/health');
    expect(populated.status).toBe(200);
    expect(populated.body.items).toHaveLength(1);
    expect(populated.body.items[0]).toMatchObject({ subscriberId: 'ep_1', status: 'healthy' });
  });

  it('returns one record or 404', async () => {
    const { app, health } = makeApp();
    await health.evaluate('ep_1');

    const missing = await request(app).get('/subscriptions/health/nope');
    expect(missing.status).toBe(404);
    expect(missing.body).toEqual({ error: 'not_found' });

    const found = await request(app).get('/subscriptions/health/ep_1');
    expect(found.status).toBe(200);
    expect(found.body.subscriberId).toBe('ep_1');
  });

  it('runs the policy on demand and reports the decisions', async () => {
    const { app, analytics, clock, notices } = makeApp();
    analytics.record({ type: 'circuit_open', endpointId: 'ep_1', at: clock.now() });
    analytics.record({ type: 'circuit_open', endpointId: 'ep_1', at: clock.now() });
    analytics.record({ type: 'circuit_open', endpointId: 'ep_1', at: clock.now() });

    const res = await request(app).post('/subscriptions/health/evaluate');

    expect(res.status).toBe(200);
    expect(res.body.items).toHaveLength(1);
    expect(res.body.items[0]).toMatchObject({
      subscriberId: 'ep_1',
      status: 'suspended',
      circuitTrips: 3,
    });
    expect(notices).toHaveLength(1);
  });

  it('reactivates a suspended subscription without a store', async () => {
    const { app, analytics, health, clock } = makeApp();
    for (let i = 0; i < 3; i++) {
      analytics.record({ type: 'circuit_open', endpointId: 'ep_1', at: clock.now() });
    }
    await health.evaluate('ep_1');
    expect(health.isSuspended('ep_1')).toBe(true);

    const res = await request(app).post('/subscriptions/health/ep_1/reactivate');

    expect(res.status).toBe(200);
    expect(res.body).toMatchObject({ subscriberId: 'ep_1', status: 'healthy' });
    expect(health.isSuspended('ep_1')).toBe(false);

    const unknown = await request(app).post('/subscriptions/health/nope/reactivate');
    expect(unknown.status).toBe(404);
    expect(unknown.body).toEqual({ error: 'not_found' });
  });

  it('requires the subscriber API key to reactivate', async () => {
    const { app, health } = makeApp(true);
    await health.evaluate('ep_1');

    const unknown = await request(app)
      .post('/subscriptions/health/nope/reactivate')
      .set('x-api-key', 'secret_1');
    expect(unknown.status).toBe(404);

    const wrongKey = await request(app)
      .post('/subscriptions/health/ep_1/reactivate')
      .set('x-api-key', 'wrong');
    expect(wrongKey.status).toBe(401);
    expect(wrongKey.body).toEqual({ error: 'unauthorized' });

    const missingKey = await request(app).post('/subscriptions/health/ep_1/reactivate');
    expect(missingKey.status).toBe(401);

    const authorised = await request(app)
      .post('/subscriptions/health/ep_1/reactivate')
      .set('x-api-key', 'secret_1');
    expect(authorised.status).toBe(200);
    expect(authorised.body.status).toBe('healthy');
  });
});
