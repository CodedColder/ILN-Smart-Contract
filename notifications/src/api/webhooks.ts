import { Router } from 'express';
import type { WebhookDeliveryService } from '../delivery/webhookDelivery.js';
import type { SubscriptionStore } from '../subscriptions/subscriptionStore.js';
import type { DeliveryHistoryStore } from '../delivery/deliveryHistory.js';

interface WebhookDeliveryOptions {
  http?: (url: string, init: any) => Promise<{ status: number }>;
}

const EMAIL_PATTERN = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

/** Operational contact address used for subscription-health notices. */
function isValidEmail(value: unknown): value is string {
  return typeof value === 'string' && EMAIL_PATTERN.test(value);
}

async function validateWebhookUrl(url: string, opts?: WebhookDeliveryOptions): Promise<boolean> {
  const httpClient = opts?.http;
  if (!httpClient) {
    try {
      const urlObj = new URL(url);
      if (urlObj.protocol !== 'http:' && urlObj.protocol !== 'https:') {
        return false;
      }
      return true;
    } catch {
      return false;
    }
  }

  try {
    const response = await httpClient(url, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ test: true }),
    });
    return response.status >= 200 && response.status < 500;
  } catch {
    return false;
  }
}

export function createWebhooksRouter(
  store: SubscriptionStore,
  delivery: WebhookDeliveryService,
  optsOrHistoryStore?: WebhookDeliveryOptions | DeliveryHistoryStore,
  historyStore?: DeliveryHistoryStore,
): Router {
  const router = Router();
  const resolvedHistoryStore = isDeliveryHistoryStore(optsOrHistoryStore)
    ? optsOrHistoryStore
    : historyStore;
  const opts = isDeliveryHistoryStore(optsOrHistoryStore) ? undefined : optsOrHistoryStore;

  function requireAuth(req: any, res: any, subId: string): boolean {
    const sub = store.get(subId);
    if (!sub) {
      res.status(404).json({ error: 'not_found' });
      return false;
    }
    const apiKey = req.headers['x-api-key'] as string | undefined;
    if (!apiKey || apiKey !== sub.secret) {
      res.status(401).json({ error: 'unauthorized' });
      return false;
    }
    return true;
  }

  router.get('/webhooks', (_req, res) => {
    const subs = store.list();
    res.json(
      subs.map((sub) => ({
        id: sub.id,
        url: sub.url,
        eventTypes: sub.eventTypes,
        circuitState: delivery.getCircuitState(sub.endpointId),
        createdAt: sub.createdAt,
      })),
    );
  });

  router.get('/webhooks/:id', (req, res) => {
    const sub = store.get(req.params.id);
    if (!sub) {
      res.status(404).json({ error: 'not_found' });
      return;
    }
    res.json({
      id: sub.id,
      url: sub.url,
      eventTypes: sub.eventTypes,
      circuitState: delivery.getCircuitState(sub.endpointId),
      createdAt: sub.createdAt,
    });
  });

  router.post('/webhooks', async (req, res) => {
    const { url, secret, eventTypes, endpointId, contactEmail } = req.body ?? {};
    if (!url || !secret || !Array.isArray(eventTypes) || eventTypes.length === 0) {
      res.status(400).json({ error: 'invalid_body' });
      return;
    }
    if (contactEmail !== undefined && contactEmail !== null && !isValidEmail(contactEmail)) {
      res.status(400).json({ error: 'invalid_contact_email' });
      return;
    }

    const isValid = await validateWebhookUrl(url, opts);
    if (!isValid) {
      res.status(400).json({ error: 'invalid_url' });
      return;
    }

    const sub = store.create({
      endpointId: endpointId ?? url,
      url,
      secret,
      eventTypes,
      contactEmail: contactEmail ?? null,
    });
    res.status(201).json({
      id: sub.id,
      url: sub.url,
      eventTypes: sub.eventTypes,
      createdAt: sub.createdAt,
    });
  });

  router.put('/webhooks/:id', async (req, res) => {
    const sub = store.get(req.params.id);
    if (!sub) {
      res.status(404).json({ error: 'not_found' });
      return;
    }

    const { url, secret, eventTypes, contactEmail } = req.body ?? {};
    const patch: any = {};

    if (url !== undefined) {
      const isValid = await validateWebhookUrl(url, opts);
      if (!isValid) {
        res.status(400).json({ error: 'invalid_url' });
        return;
      }
      patch.url = url;
    }

    if (contactEmail !== undefined) {
      // null clears the contact so the subscriber stops receiving notices.
      if (contactEmail !== null && !isValidEmail(contactEmail)) {
        res.status(400).json({ error: 'invalid_contact_email' });
        return;
      }
      patch.contactEmail = contactEmail ?? null;
    }

    if (secret !== undefined) {
      patch.secret = secret;
    }

    if (eventTypes !== undefined) {
      if (!Array.isArray(eventTypes) || eventTypes.length === 0) {
        res.status(400).json({ error: 'invalid_body' });
        return;
      }
      patch.eventTypes = eventTypes;
    }

    if (Object.keys(patch).length === 0) {
      res.status(400).json({ error: 'no_changes' });
      return;
    }

    const updated = store.update(req.params.id, patch);
    if (!updated) {
      res.status(404).json({ error: 'not_found' });
      return;
    }

    res.json({
      id: updated.id,
      url: updated.url,
      eventTypes: updated.eventTypes,
      createdAt: updated.createdAt,
    });
  });

  router.delete('/webhooks/:id', (req, res) => {
    const ok = store.delete(req.params.id);
    res.status(ok ? 204 : 404).end();
  });

  if (resolvedHistoryStore) {
    router.get('/webhooks/:id/deliveries', (req, res) => {
      const sub = store.get(req.params.id);
      if (!sub) {
        res.status(404).json({ error: 'not_found' });
        return;
      }

      if (!requireAuth(req, res, req.params.id)) return;

      const page = Math.max(1, parseInt(req.query.page as string, 10) || 1);
      const pageSize = Math.min(100, Math.max(1, parseInt(req.query.pageSize as string, 10) || 20));

      const result = resolvedHistoryStore.listByWebhook(sub.endpointId, page, pageSize);
      res.json(result);
    });
  }

  return router;
}

function isDeliveryHistoryStore(value: unknown): value is DeliveryHistoryStore {
  return typeof value === 'object' && value !== null && 'listByWebhook' in value;
}
