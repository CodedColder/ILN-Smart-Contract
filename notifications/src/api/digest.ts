import { Router } from 'express';
import type { DigestService } from '../services/digestService.js';
import type { SubscriptionStore } from '../subscriptions/subscriptionStore.js';
import { requireEndpointApiKey } from './auth.js';

/**
 * Subscriber opt-in / opt-out for notification digests (Issue #871).
 *
 *   GET  /subscriptions/digest                          preferences + pending
 *   POST /subscriptions/digest/:subscriberId/opt-in     batch over the window
 *   POST /subscriptions/digest/:subscriberId/opt-out    back to real-time
 *
 * Opting out keeps notifications flowing — it switches the subscriber back to
 * the real-time path; it never silences them. Writes are authorised with the
 * subscriber's API key when a subscription store is supplied.
 *
 * `subscriberId` is the webhook endpoint id — the same key the delivery
 * layer emits analytics under (see `services/types.ts` for the layer model).
 */
export function createDigestRouter(
  digest: DigestService,
  subscriptions?: SubscriptionStore,
): Router {
  const router = Router();

  router.get('/subscriptions/digest', (_req, res) => {
    res.json({ items: digest.list() });
  });

  router.post('/subscriptions/digest/:subscriberId/opt-in', (req, res) => {
    const subscriberId = req.params.subscriberId;
    if (subscriptions && !requireEndpointApiKey(subscriptions, subscriberId, req, res)) {
      return;
    }
    res.json({ subscriberId, mode: digest.optIn(subscriberId) });
  });

  router.post('/subscriptions/digest/:subscriberId/opt-out', (req, res) => {
    const subscriberId = req.params.subscriberId;
    if (subscriptions && !requireEndpointApiKey(subscriptions, subscriberId, req, res)) {
      return;
    }
    res.json({ subscriberId, mode: digest.optOut(subscriberId) });
  });

  return router;
}
