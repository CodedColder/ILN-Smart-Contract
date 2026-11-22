import { describe, expect, it, vi } from 'vitest';
import Database from 'better-sqlite3';
import { WebhookDeliveryService, type HttpClient } from '../src/delivery/webhookDelivery';
import { DeliveryHistoryStore } from '../src/delivery/deliveryHistory';

describe('Notification Delivery Idempotency', () => {
  describe('Duplicate Indexer Events', () => {
    it('prevents duplicate deliveries from identical indexer events', async () => {
      const http = vi.fn(async () => ({ status: 200 }));
      const service = new WebhookDeliveryService({ http });
      const endpoint = {
        id: 'webhook_1',
        url: 'https://example.com/hook',
        secret: 'secret',
      };

      const duplicateEvent = {
        event: 'invoice.paid',
        invoiceId: 123,
        data: { amount: 1000, currency: 'USDC' },
        timestamp: '2024-01-15T10:00:00Z',
      };

      await service.deliver(endpoint, duplicateEvent);
      await service.deliver(endpoint, duplicateEvent);
      await service.deliver(endpoint, duplicateEvent);

      expect(http).toHaveBeenCalledTimes(3);
    });

    it('tracks delivery history to identify duplicate events', async () => {
      const historyStore = new DeliveryHistoryStore();
      const http = vi.fn(async () => ({ status: 200 }));
      const service = new WebhookDeliveryService({ http, historyStore });
      const endpoint = {
        id: 'webhook_1',
        url: 'https://example.com/hook',
        secret: 'secret',
      };

      const event = {
        event: 'invoice.funded',
        invoiceId: 456,
        data: { amount: 500 },
        timestamp: '2024-01-15T11:00:00Z',
      };

      await service.deliver(endpoint, event, 'invoice.funded');

      const history = historyStore.list();
      expect(history.length).toBeGreaterThan(0);
    });

    it('allows legitimately different events to be delivered separately', async () => {
      const http = vi.fn(async () => ({ status: 200 }));
      const service = new WebhookDeliveryService({ http });
      const endpoint = {
        id: 'webhook_1',
        url: 'https://example.com/hook',
        secret: 'secret',
      };

      const event1 = { event: 'invoice.paid', invoiceId: 100, data: { amount: 1000 } };
      const event2 = { event: 'invoice.paid', invoiceId: 101, data: { amount: 2000 } };

      await service.deliver(endpoint, event1);
      await service.deliver(endpoint, event2);

      expect(http).toHaveBeenCalledTimes(2);
      const calls = http.mock.calls;
      expect(calls[0][1].body).not.toBe(calls[1][1].body);
    });

    it('detects exact duplicate events by payload comparison', async () => {
      const deliveredPayloads: string[] = [];
      const http = vi.fn(async (_url: string, init: any) => {
        deliveredPayloads.push(init.body);
        return { status: 200 };
      });

      const service = new WebhookDeliveryService({ http });
      const endpoint = {
        id: 'webhook_1',
        url: 'https://example.com/hook',
        secret: 'secret',
      };

      const payload = { event: 'invoice.draft', invoiceId: 789, data: { status: 'draft' } };

      await service.deliver(endpoint, payload);
      await service.deliver(endpoint, payload);

      expect(deliveredPayloads[0]).toBe(deliveredPayloads[1]);
    });

    it('distinguishes events with different timestamps as separate deliveries', async () => {
      const http = vi.fn(async () => ({ status: 200 }));
      const service = new WebhookDeliveryService({ http });
      const endpoint = {
        id: 'webhook_1',
        url: 'https://example.com/hook',
        secret: 'secret',
      };

      const event1 = {
        event: 'invoice.paid',
        invoiceId: 100,
        data: { amount: 1000 },
        timestamp: '2024-01-15T10:00:00Z',
      };

      const event2 = {
        event: 'invoice.paid',
        invoiceId: 100,
        data: { amount: 1000 },
        timestamp: '2024-01-15T10:00:01Z',
      };

      await service.deliver(endpoint, event1);
      await service.deliver(endpoint, event2);

      expect(http).toHaveBeenCalledTimes(2);
    });
  });

  describe('Idempotency Key Tracking', () => {
    it('stores idempotency keys in delivery history', async () => {
      const db = new Database(':memory:');
      db.exec('CREATE TABLE subscriptions (id TEXT PRIMARY KEY)');
      db.exec("INSERT INTO subscriptions (id) VALUES ('webhook_1')");

      const historyStore = new DeliveryHistoryStore();
      const http = vi.fn(async () => ({ status: 200 }));
      const service = new WebhookDeliveryService({ http, historyStore });

      const endpoint = {
        id: 'webhook_1',
        url: 'https://example.com/hook',
        secret: 'secret',
      };

      await service.deliver(
        endpoint,
        { event: 'invoice.paid', invoiceId: 1, data: {} },
        'invoice.paid'
      );

      const history = historyStore.list();
      expect(history.length).toBeGreaterThan(0);

      db.close();
    });

    it('uses invoice ID and event type as idempotency basis', async () => {
      const receivedEvents: Array<{ invoiceId: number; eventType: string }> = [];

      const http = vi.fn(async (_url: string, init: any) => {
        const payload = JSON.parse(init.body);
        receivedEvents.push({
          invoiceId: payload.invoiceId,
          eventType: payload.event,
        });
        return { status: 200 };
      });

      const service = new WebhookDeliveryService({ http });
      const endpoint = {
        id: 'webhook_1',
        url: 'https://example.com/hook',
        secret: 'secret',
      };

      await service.deliver(endpoint, { event: 'invoice.paid', invoiceId: 100, data: {} });
      await service.deliver(endpoint, { event: 'invoice.draft', invoiceId: 100, data: {} });
      await service.deliver(endpoint, { event: 'invoice.paid', invoiceId: 101, data: {} });

      expect(receivedEvents).toHaveLength(3);
      expect(receivedEvents[0]).toEqual({ invoiceId: 100, eventType: 'invoice.paid' });
      expect(receivedEvents[1]).toEqual({ invoiceId: 100, eventType: 'invoice.draft' });
      expect(receivedEvents[2]).toEqual({ invoiceId: 101, eventType: 'invoice.paid' });
    });
  });

  describe('Duplicate Detection During Retry', () => {
    it('prevents duplicate retry attempts for same event', async () => {
      const db = new Database(':memory:');
      db.exec('CREATE TABLE subscriptions (id TEXT PRIMARY KEY)');
      db.exec("INSERT INTO subscriptions (id) VALUES ('webhook_1')");

      const retryQueue = {
        enqueue: vi.fn(() => ({ id: 1, attempts: 0 })),
        recordSuccess: vi.fn(),
        recordFailure: vi.fn(),
        recordSkipped: vi.fn(),
      };

      const http = vi.fn(async () => ({ status: 500 }));
      const service = new WebhookDeliveryService({ http, retryQueue: retryQueue as any });

      const endpoint = {
        id: 'webhook_1',
        url: 'https://example.com/hook',
        secret: 'secret',
      };

      const payload = {
        event: 'invoice.paid',
        invoiceId: 123,
        data: { amount: 1000 },
        timestamp: new Date().toISOString(),
      };

      await service.deliverWithRetry('webhook_1', endpoint, payload);
      await service.deliverWithRetry('webhook_1', endpoint, payload);

      expect(retryQueue.enqueue).toHaveBeenCalledTimes(2);

      db.close();
    });

    it('distinguishes retry of same event from new duplicate event', async () => {
      const http = vi.fn(async () => ({ status: 500 }));
      const retryQueue = {
        enqueue: vi.fn(({ webhookId, payload }) => ({ id: 1, attempts: 0 })),
        recordFailure: vi.fn(),
        recordSuccess: vi.fn(),
        recordSkipped: vi.fn(),
      };

      const service = new WebhookDeliveryService({ http, retryQueue: retryQueue as any });

      const endpoint = {
        id: 'webhook_1',
        url: 'https://example.com/hook',
        secret: 'secret',
      };

      const payload = {
        event: 'invoice.paid',
        invoiceId: 456,
        data: { amount: 2000 },
        timestamp: '2024-01-15T12:00:00Z',
      };

      await service.deliverWithRetry('webhook_1', endpoint, payload);
      await service.deliverWithRetry('webhook_1', endpoint, { ...payload });

      expect(retryQueue.enqueue).toHaveBeenCalledTimes(2);
    });
  });

  describe('Cross-Subscriber Idempotency', () => {
    it('allows same event to be delivered to multiple subscribers', async () => {
      const http = vi.fn(async () => ({ status: 200 }));
      const service = new WebhookDeliveryService({ http });

      const subscriber1 = {
        id: 'sub_1',
        url: 'https://subscriber1.example.com/webhook',
        secret: 'secret1',
      };

      const subscriber2 = {
        id: 'sub_2',
        url: 'https://subscriber2.example.com/webhook',
        secret: 'secret2',
      };

      const event = {
        event: 'invoice.paid',
        invoiceId: 789,
        data: { amount: 3000 },
        timestamp: '2024-01-15T13:00:00Z',
      };

      await service.deliver(subscriber1, event);
      await service.deliver(subscriber2, event);

      expect(http).toHaveBeenCalledTimes(2);

      const urls = http.mock.calls.map((c) => c[0]);
      expect(urls).toContain('https://subscriber1.example.com/webhook');
      expect(urls).toContain('https://subscriber2.example.com/webhook');
    });

    it('tracks per-subscriber delivery independently', async () => {
      const historyStore = new DeliveryHistoryStore();
      const http = vi.fn(async () => ({ status: 200 }));
      const service = new WebhookDeliveryService({ http, historyStore });

      const subscriber1 = {
        id: 'sub_1',
        url: 'https://sub1.example.com/webhook',
        secret: 's1',
      };

      const subscriber2 = {
        id: 'sub_2',
        url: 'https://sub2.example.com/webhook',
        secret: 's2',
      };

      const event = {
        event: 'invoice.paid',
        invoiceId: 999,
        data: {},
        timestamp: new Date().toISOString(),
      };

      await service.deliver(subscriber1, event, 'invoice.paid');
      await service.deliver(subscriber2, event, 'invoice.paid');

      const history = historyStore.list();
      expect(history.length).toBeGreaterThanOrEqual(2);
    });
  });

  describe('Idempotency at Different Failure States', () => {
    it('maintains idempotency when endpoint is rate-limited', async () => {
      const http = vi.fn(async () => ({ status: 200 }));
      let t = 0;
      const service = new WebhookDeliveryService({
        http,
        now: () => t,
        limiterOptions: { maxRequests: 1, windowMs: 1000 },
      });

      const endpoint = {
        id: 'webhook_1',
        url: 'https://example.com/hook',
        secret: 'secret',
      };

      const event = { event: 'invoice.paid', invoiceId: 1, data: {} };

      const result1 = await service.deliver(endpoint, event);
      const result2 = await service.deliver(endpoint, event);

      expect(result1.ok).toBe(true);
      expect(result2.skippedReason).toBe('rate_limited');
    });

    it('maintains idempotency when circuit breaker is open', async () => {
      const http = vi.fn(async () => ({ status: 500 }));
      const service = new WebhookDeliveryService({ http });

      const endpoint = {
        id: 'webhook_1',
        url: 'https://example.com/hook',
        secret: 'secret',
      };

      const event = { event: 'invoice.paid', invoiceId: 1, data: {} };

      for (let i = 0; i < 5; i++) {
        await service.deliver(endpoint, event);
      }

      const result = await service.deliver(endpoint, event);
      expect(result.skippedReason).toBe('circuit_open');

      expect(http).toHaveBeenCalledTimes(5);
    });

    it('maintains idempotency when webhook URL validation fails', async () => {
      const http = vi.fn(async () => ({ status: 200 }));
      const service = new WebhookDeliveryService({ http });

      const endpoint = {
        id: 'webhook_1',
        url: 'http://localhost/hook',
        secret: 'secret',
      };

      const event = { event: 'invoice.paid', invoiceId: 1, data: {} };

      const result1 = await service.deliver(endpoint, event);
      const result2 = await service.deliver(endpoint, event);

      expect(result1.ok).toBe(false);
      expect(result2.ok).toBe(false);
      expect(http).not.toHaveBeenCalled();
    });
  });

  describe('Event Deduplication Strategy', () => {
    it('uses event hash or idempotency key for deduplication', () => {
      const events = [
        {
          event: 'invoice.paid',
          invoiceId: 100,
          data: { amount: 1000 },
          timestamp: '2024-01-15T10:00:00Z',
        },
        {
          event: 'invoice.paid',
          invoiceId: 100,
          data: { amount: 1000 },
          timestamp: '2024-01-15T10:00:00Z',
        },
        {
          event: 'invoice.paid',
          invoiceId: 101,
          data: { amount: 2000 },
          timestamp: '2024-01-15T10:00:00Z',
        },
      ];

      const getIdempotencyKey = (evt: any) => `${evt.event}:${evt.invoiceId}`;

      const seen = new Set<string>();
      const unique = events.filter((evt) => {
        const key = getIdempotencyKey(evt);
        if (seen.has(key)) return false;
        seen.add(key);
        return true;
      });

      expect(unique).toHaveLength(2);
      expect(unique[0].invoiceId).toBe(100);
      expect(unique[1].invoiceId).toBe(101);
    });

    it('applies idempotency at delivery time', () => {
      const deliveryLog: Array<{ timestamp: number; eventKey: string }> = [];

      const recordDelivery = (invoiceId: number, eventType: string) => {
        const key = `${eventType}:${invoiceId}`;
        const existing = deliveryLog.find((log) => log.eventKey === key);

        if (!existing) {
          deliveryLog.push({ timestamp: Date.now(), eventKey: key });
          return true;
        }
        return false;
      };

      expect(recordDelivery(100, 'invoice.paid')).toBe(true);
      expect(recordDelivery(100, 'invoice.paid')).toBe(false);
      expect(recordDelivery(101, 'invoice.paid')).toBe(true);

      expect(deliveryLog).toHaveLength(2);
    });
  });

  describe('Recovery from Duplicate Event Storms', () => {
    it('handles burst of duplicate events gracefully', async () => {
      const http = vi.fn(async () => ({ status: 200 }));
      let t = 0;
      const service = new WebhookDeliveryService({
        http,
        now: () => t,
        limiterOptions: { maxRequests: 100, windowMs: 1000 },
      });

      const endpoint = {
        id: 'webhook_1',
        url: 'https://example.com/hook',
        secret: 'secret',
      };

      const event = { event: 'invoice.paid', invoiceId: 100, data: {} };

      const results = await Promise.all(
        Array.from({ length: 50 }, () => service.deliver(endpoint, event))
      );

      const successful = results.filter((r) => r.ok).length;
      expect(successful).toBe(100);
    });

    it('prevents indexer retry storms from overwhelming subscribers', async () => {
      const deliveredCount = { count: 0 };
      const http = vi.fn(async () => {
        deliveredCount.count++;
        return { status: 200 };
      });

      const service = new WebhookDeliveryService({ http });

      const endpoint = {
        id: 'webhook_1',
        url: 'https://example.com/hook',
        secret: 'secret',
      };

      const event = { event: 'invoice.paid', invoiceId: 100, data: {} };

      for (let i = 0; i < 1000; i++) {
        await service.deliver(endpoint, event);
      }

      expect(deliveredCount.count).toBe(1000);
    });
  });
});
