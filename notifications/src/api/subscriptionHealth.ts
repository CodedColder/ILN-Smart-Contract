import { Router } from 'express';
import type { SubscriptionHealthService } from '../services/subscriptionHealthService.js';
import type { SubscriptionStore } from '../subscriptions/subscriptionStore.js';
import { requireEndpointApiKey } from './auth.js';

/**
 * Subscription-health API (Issue #873).
 *
 *   GET  /subscriptions/health                          every health record
 *   GET  /subscriptions/health/:subscriberId            one record (404 if unseen)
 *   POST /subscriptions/health/evaluate                 run the policy now
 *   POST /subscriptions/health/:subscriberId/reactivate manual reactivation
 *
 * Reactivation requires the subscriber's API key (when a store is supplied);
 * it clears the suspension *and* resets the analytics window that caused it.
 */
export function createSubscriptionHealthRouter(
  health: SubscriptionHealthService,
  subscriptions?: SubscriptionStore,
): Router {
  const router = Router();

  router.get('/subscriptions/health', (_req, res) => {
    res.json({ items: health.list() });
  });

  router.get('/subscriptions/health/:subscriberId', (req, res) => {
    const record = health.get(req.params.subscriberId);
    if (!record) {
      res.status(404).json({ error: 'not_found' });
      return;
    }
    res.json(record);
  });

  router.post('/subscriptions/health/evaluate', async (_req, res) => {
    const items = await health.evaluateAll();
    res.json({ items });
  });

  router.post('/subscriptions/health/:subscriberId/reactivate', async (req, res) => {
    const subscriberId = req.params.subscriberId;
    if (subscriptions && !requireEndpointApiKey(subscriptions, subscriberId, req, res)) {
      return;
    }
    const record = await health.reactivate(subscriberId);
    if (!record) {
      res.status(404).json({ error: 'not_found' });
      return;
    }
    res.json(record);
  });

  return router;
}
