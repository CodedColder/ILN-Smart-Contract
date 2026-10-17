import { describe, expect, it, vi } from 'vitest';
import express from 'express';
import { createWebhooksRouter } from '../src/api/webhooks';
import { SubscriptionStore } from '../src/subscriptions/subscriptionStore';
import { WebhookDeliveryService } from '../src/delivery/webhookDelivery';
import { DeliveryHistoryStore } from '../src/delivery/deliveryHistory';

function makeApp() {
  const store = new SubscriptionStore();
  const delivery = new WebhookDeliveryService({
    http: vi.fn(async () => ({ status: 200 })),
  });
  const app = express();
  app.use(express.json());
  app.use(createWebhooksRouter(store, delivery));
  return { app, store, delivery };
}

function makeAppWithValidationHttp(status = 404) {
  const store = new SubscriptionStore();
  const delivery = new WebhookDeliveryService({
    http: vi.fn(async () => ({ status: 200 })),
  });
  const http = vi.fn(async () => ({ status }));
  const app = express();
  app.use(express.json());
  app.use(createWebhooksRouter(store, delivery, { http }));
  return { app, store, delivery, http };
}

function makeAppWithHistory() {
  const store = new SubscriptionStore();
  const historyStore = new DeliveryHistoryStore();
  const delivery = new WebhookDeliveryService({
    http: vi.fn(async () => ({ status: 200 })),
  });
  const app = express();
  app.use(express.json());
  app.use(createWebhooksRouter(store, delivery, historyStore));
  return { app, store, delivery, historyStore };
}

async function request(app: express.Express, method: string, path: string, body?: any) {
  const { default: http } = await import('node:http');
  return new Promise<{ status: number; body: any }>((resolve, reject) => {
    const server = http.createServer(app).listen(0, () => {
      const { port } = server.address() as any;
      const req = http.request(
        { host: '127.0.0.1', port, path, method, headers: { 'content-type': 'application/json' } },
        (res) => {
          const chunks: Buffer[] = [];
          res.on('data', (c) => chunks.push(c));
          res.on('end', () => {
            const text = Buffer.concat(chunks).toString();
            let parsed: any = null;
            try {
              parsed = text ? JSON.parse(text) : null;
            } catch {
              parsed = text;
            }
            server.close();
            resolve({ status: res.statusCode ?? 0, body: parsed });
          });
        },
      );
      req.on('error', reject);
      if (body) req.write(JSON.stringify(body));
      req.end();
    });
  });
}

describe('webhooks router', () => {
  it('creates a subscription', async () => {
    const { app } = makeApp();
    const res = await request(app, 'POST', '/webhooks', {
      url: 'https://x',
      secret: 'k',
      eventTypes: ['A'],
    });
    expect(res.status).toBe(201);
    expect(res.body.url).toBe('https://x');
  });

  it('rejects invalid bodies', async () => {
    const { app } = makeApp();
    const res = await request(app, 'POST', '/webhooks', { url: 'x' });
    expect(res.status).toBe(400);
  });

  it('rejects invalid webhook URLs', async () => {
    const { app } = makeApp();
    const res = await request(app, 'POST', '/webhooks', {
      url: 'ftp://example.com',
      secret: 'k',
      eventTypes: ['A'],
    });
    expect(res.status).toBe(400);
    expect(res.body.error).toBe('invalid_url');
  });

  it('uses the injected validator http client when present', async () => {
    const { app, http } = makeAppWithValidationHttp(404);
    const res = await request(app, 'POST', '/webhooks', {
      url: 'https://example.com/hooks',
      secret: 'k',
      eventTypes: ['A'],
    });
    expect(res.status).toBe(201);
    expect(http).toHaveBeenCalledTimes(1);
  });

  it('returns circuit state on GET', async () => {
    const { app, store } = makeApp();
    const sub = store.create({ endpointId: 'e1', url: 'https://x', secret: 'k', eventTypes: ['A'] });
    const res = await request(app, 'GET', `/webhooks/${sub.id}`);
    expect(res.status).toBe(200);
    expect(res.body.circuitState).toBe('closed');
  });

  it('404s for unknown subscriptions', async () => {
    const { app } = makeApp();
    const res = await request(app, 'GET', '/webhooks/nope');
    expect(res.status).toBe(404);
  });

  it('deletes a subscription', async () => {
    const { app, store } = makeApp();
    const sub = store.create({ endpointId: 'e1', url: 'https://x', secret: 'k', eventTypes: ['A'] });
    const res = await request(app, 'DELETE', `/webhooks/${sub.id}`);
    expect(res.status).toBe(204);
  });

  it('returns 404 when deleting an unknown subscription', async () => {
    const { app } = makeApp();
    const res = await request(app, 'DELETE', '/webhooks/missing');
    expect(res.status).toBe(404);
  });

  it('rejects updates without changes and invalid URLs', async () => {
    const { app, store } = makeApp();
    const sub = store.create({ endpointId: 'e1', url: 'https://x', secret: 'k', eventTypes: ['A'] });

    const noChanges = await request(app, 'PUT', `/webhooks/${sub.id}`, {});
    expect(noChanges.status).toBe(400);
    expect(noChanges.body.error).toBe('no_changes');

    const invalidUrl = await request(app, 'PUT', `/webhooks/${sub.id}`, {
      url: 'bad-url',
    });
    expect(invalidUrl.status).toBe(400);
    expect(invalidUrl.body.error).toBe('invalid_url');
  });

  it('rejects empty eventTypes and unknown update targets', async () => {
    const { app, store } = makeApp();
    const sub = store.create({ endpointId: 'e1', url: 'https://x', secret: 'k', eventTypes: ['A'] });

    const emptyEvents = await request(app, 'PUT', `/webhooks/${sub.id}`, {
      eventTypes: [],
    });
    expect(emptyEvents.status).toBe(400);
    expect(emptyEvents.body.error).toBe('invalid_body');

    const missing = await request(app, 'PUT', '/webhooks/missing', {
      secret: 'new-secret',
    });
    expect(missing.status).toBe(404);
  });
});

describe('GET /webhooks/:id/deliveries', () => {
  it('returns 401 without API key', async () => {
    const { app, store } = makeAppWithHistory();
    const sub = store.create({ endpointId: 'e1', url: 'https://x', secret: 'k', eventTypes: ['A'] });
    const res = await request(app, 'GET', `/webhooks/${sub.id}/deliveries`);
    expect(res.status).toBe(401);
  });

  it('returns 401 with wrong API key', async () => {
    const { app, store } = makeAppWithHistory();
    const sub = store.create({ endpointId: 'e1', url: 'https://x', secret: 'k', eventTypes: ['A'] });
    const { default: http } = await import('node:http');
    const res = await fetchApp(app, 'GET', `/webhooks/${sub.id}/deliveries`, undefined, { 'x-api-key': 'wrong' });
    expect(res.status).toBe(401);
  });

  it('returns empty deliveries when no records exist', async () => {
    const { app, store } = makeAppWithHistory();
    const sub = store.create({ endpointId: 'e1', url: 'https://x', secret: 'test-secret', eventTypes: ['A'] });
    const res = await fetchApp(app, 'GET', `/webhooks/${sub.id}/deliveries`, undefined, { 'x-api-key': 'test-secret' });
    expect(res.status).toBe(200);
    expect(res.body.items).toEqual([]);
    expect(res.body.total).toBe(0);
  });

  it('returns delivery records with pagination', async () => {
    const { app, store, historyStore } = makeAppWithHistory();
    const sub = store.create({ endpointId: 'e1', url: 'https://x', secret: 'test-secret', eventTypes: ['A'] });

    for (let i = 0; i < 5; i++) {
      historyStore.add({
        webhookId: 'e1',
        eventType: 'invoice.submitted',
        deliveredAt: Date.now() + i * 1000,
        statusCode: 200,
        responseBody: 'ok',
        attemptCount: 1,
        nextRetryAt: null,
      });
    }

    const res = await fetchApp(app, 'GET', `/webhooks/${sub.id}/deliveries?page=1&pageSize=3`, undefined, { 'x-api-key': 'test-secret' });
    expect(res.status).toBe(200);
    expect(res.body.items).toHaveLength(3);
    expect(res.body.total).toBe(5);
    expect(res.body.page).toBe(1);
    expect(res.body.pageSize).toBe(3);
  });

  it('respects pageSize param with upper cap', async () => {
    const { app, store, historyStore } = makeAppWithHistory();
    const sub = store.create({ endpointId: 'e1', url: 'https://x', secret: 'test-secret', eventTypes: ['A'] });

    for (let i = 0; i < 150; i++) {
      historyStore.add({
        webhookId: 'e1',
        eventType: 'invoice.submitted',
        deliveredAt: Date.now() + i,
        statusCode: i % 2 === 0 ? 200 : 500,
        responseBody: i % 2 === 0 ? 'ok' : 'error',
        attemptCount: 1,
        nextRetryAt: null,
      });
    }

    const res = await fetchApp(app, 'GET', `/webhooks/${sub.id}/deliveries?pageSize=200`, undefined, { 'x-api-key': 'test-secret' });
    expect(res.status).toBe(200);
    expect(res.body.items).toHaveLength(100);
    expect(res.body.total).toBe(150);
  });

  it('returns 404 for unknown webhook ID', async () => {
    const { app } = makeAppWithHistory();
    const res = await fetchApp(app, 'GET', '/webhooks/unknown/deliveries');
    expect(res.status).toBe(404);
  });

  it('validates and stores an optional contact email without echoing it', async () => {
    const { app, store } = makeApp();

    const invalid = await request(app, 'POST', '/webhooks', {
      url: 'https://example.com/hook',
      secret: 'k',
      eventTypes: ['A'],
      contactEmail: 'not-an-email',
    });
    expect(invalid.status).toBe(400);
    expect(invalid.body).toEqual({ error: 'invalid_contact_email' });
    expect(store.list()).toHaveLength(0);

    const created = await request(app, 'POST', '/webhooks', {
      url: 'https://example.com/hook',
      secret: 'k',
      eventTypes: ['A'],
      contactEmail: 'ops@example.com',
    });
    expect(created.status).toBe(201);
    expect(created.body.contactEmail).toBeUndefined();
    expect(store.list()[0]?.contactEmail).toBe('ops@example.com');
  });

  it('updates or clears the contact email through PUT', async () => {
    const { app, store } = makeApp();
    const created = await request(app, 'POST', '/webhooks', {
      url: 'https://example.com/hook',
      secret: 'k',
      eventTypes: ['A'],
      contactEmail: 'ops@example.com',
    });
    const id = created.body.id as string;

    const invalid = await request(app, 'PUT', `/webhooks/${id}`, { contactEmail: 'nope' });
    expect(invalid.status).toBe(400);
    expect(store.get(id)?.contactEmail).toBe('ops@example.com');

    const cleared = await request(app, 'PUT', `/webhooks/${id}`, { contactEmail: null });
    expect(cleared.status).toBe(200);
    expect(store.get(id)?.contactEmail).toBeNull();

    const replaced = await request(app, 'PUT', `/webhooks/${id}`, {
      contactEmail: 'oncall@example.com',
    });
    expect(replaced.status).toBe(200);
    expect(store.get(id)?.contactEmail).toBe('oncall@example.com');
  });
});

async function fetchApp(app: express.Express, method: string, path: string, body?: any, headers?: Record<string, string>) {
  const { default: http } = await import('node:http');
  return new Promise<{ status: number; body: any }>((resolve, reject) => {
    const server = http.createServer(app).listen(0, () => {
      const { port } = server.address() as any;
      const req = http.request(
        {
          host: '127.0.0.1',
          port,
          path,
          method,
          headers: { 'content-type': 'application/json', ...headers },
        },
        (res) => {
          const chunks: Buffer[] = [];
          res.on('data', (c) => chunks.push(c));
          res.on('end', () => {
            const text = Buffer.concat(chunks).toString();
            let parsed: any = null;
            try {
              parsed = text ? JSON.parse(text) : null;
            } catch {
              parsed = text;
            }
            server.close();
            resolve({ status: res.statusCode ?? 0, body: parsed });
          });
        },
      );
      req.on('error', reject);
      if (body) req.write(JSON.stringify(body));
      req.end();
    });
  });
}
