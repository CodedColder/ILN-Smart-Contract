import express from 'express';
import request from 'supertest';
import { describe, expect, it } from 'vitest';
import { createDigestRouter } from '../../src/api/digest';
import { DigestService } from '../../src/services/digestService';
import { SubscriptionStore } from '../../src/subscriptions/subscriptionStore';
import { fakeClock } from '../helpers/clock';
import { makeLogger } from '../helpers/logger';

function makeApp(withStore = false) {
  const clock = fakeClock();
  const { logger } = makeLogger();
  const digest = new DigestService({ now: clock.now, logger });
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
  app.use(createDigestRouter(digest, store));
  return { app, digest };
}

describe('digest router', () => {
  it('lists preferences', async () => {
    const { app, digest } = makeApp();

    const empty = await request(app).get('/subscriptions/digest');
    expect(empty.status).toBe(200);
    expect(empty.body).toEqual({ items: [] });

    digest.optIn('ep_1');

    const populated = await request(app).get('/subscriptions/digest');
    expect(populated.status).toBe(200);
    expect(populated.body.items).toEqual([
      { subscriberId: 'ep_1', mode: 'digest', pending: 0, due: false },
    ]);
  });

  it('opts a subscriber in and back out', async () => {
    const { app, digest } = makeApp();

    const optedIn = await request(app).post('/subscriptions/digest/ep_1/opt-in');
    expect(optedIn.status).toBe(200);
    expect(optedIn.body).toEqual({ subscriberId: 'ep_1', mode: 'digest' });
    expect(digest.mode('ep_1')).toBe('digest');
    expect(digest.enqueue('ep_1', { event: 'invoice.paid', invoiceId: 1, payload: {} }).batched).toBe(true);

    const optedOut = await request(app).post('/subscriptions/digest/ep_1/opt-out');
    expect(optedOut.status).toBe(200);
    expect(optedOut.body).toEqual({ subscriberId: 'ep_1', mode: 'realtime' });
    expect(digest.mode('ep_1')).toBe('realtime');
    expect(digest.enqueue('ep_1', { event: 'invoice.paid', invoiceId: 2, payload: {} }).batched).toBe(false);
  });

  it('authorises preference changes against the subscriber API key', async () => {
    const { app, digest } = makeApp(true);

    const unknown = await request(app)
      .post('/subscriptions/digest/nope/opt-in')
      .set('x-api-key', 'secret_1');
    expect(unknown.status).toBe(404);
    expect(unknown.body).toEqual({ error: 'not_found' });

    const wrongKey = await request(app)
      .post('/subscriptions/digest/ep_1/opt-in')
      .set('x-api-key', 'wrong');
    expect(wrongKey.status).toBe(401);
    expect(wrongKey.body).toEqual({ error: 'unauthorized' });
    expect(digest.mode('ep_1')).toBe('realtime');

    const missingKey = await request(app).post('/subscriptions/digest/ep_1/opt-in');
    expect(missingKey.status).toBe(401);

    const authorised = await request(app)
      .post('/subscriptions/digest/ep_1/opt-in')
      .set('x-api-key', 'secret_1');
    expect(authorised.status).toBe(200);
    expect(digest.mode('ep_1')).toBe('digest');

    const authorisedOut = await request(app)
      .post('/subscriptions/digest/ep_1/opt-out')
      .set('x-api-key', 'secret_1');
    expect(authorisedOut.status).toBe(200);
    expect(digest.mode('ep_1')).toBe('realtime');
  });
});
