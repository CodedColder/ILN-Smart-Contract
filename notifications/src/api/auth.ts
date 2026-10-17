import type { Request, Response } from 'express';
import type { SubscriptionStore } from '../subscriptions/subscriptionStore.js';

/**
 * Shared API-key guard for subscription-scoped routes.
 *
 * Services (digest, health) are keyed by *endpoint id*, the same identifier
 * the delivery layer emits analytics for, so a request is authorised against
 * the subscription that owns that endpoint — the same `x-api-key` contract
 * the webhook delivery-history routes use.
 *
 * @returns true when the caller may proceed; otherwise a status has already
 * been written and the route must return.
 */
export function requireEndpointApiKey(
  store: SubscriptionStore,
  endpointId: string,
  req: Request,
  res: Response,
): boolean {
  const subscription = store.getByEndpointId(endpointId);
  if (!subscription) {
    res.status(404).json({ error: 'not_found' });
    return false;
  }
  const apiKey = req.headers['x-api-key'];
  if (!apiKey || apiKey !== subscription.secret) {
    res.status(401).json({ error: 'unauthorized' });
    return false;
  }
  return true;
}
