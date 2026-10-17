import { describe, expect, it, vi } from 'vitest';
import { WebhookDeliveryService, type HttpClient } from '../src/delivery/webhookDelivery';
import { DeliveryHistoryStore } from '../src/delivery/deliveryHistory';
import { stubDnsResolver } from './helpers/dns';

function makeService(http: HttpClient, now?: () => number) {
  return new WebhookDeliveryService({ http, now, dnsResolver: stubDnsResolver });
}

describe('WebhookDeliveryService', () => {
  const endpoint = { id: 'e1', url: 'http://8.8.8.8/abc', secret: 's' };

  it('signs and delivers on success', async () => {
    const http = vi.fn(async () => ({ status: 200 }));
    const svc = makeService(http);
    const res = await svc.deliver(endpoint, { hello: 'world' });
    expect(res.ok).toBe(true);
    expect(http).toHaveBeenCalledTimes(1);
    const [, init] = http.mock.calls[0]!;
    expect(init.headers['x-iln-signature']).toMatch(/^[0-9a-f]{64}$/);
  });

  it('records failures and opens circuit after threshold', async () => {
    const http = vi.fn(async () => ({ status: 500 }));
    const svc = makeService(http);
    for (let i = 0; i < 5; i++) {
      await svc.deliver(endpoint, { i });
    }
    expect(svc.getCircuitState(endpoint.id)).toBe('open');
    const result = await svc.deliver(endpoint, { i: 6 });
    expect(result.skippedReason).toBe('circuit_open');
  });

  it('blocks once the rate limit is hit', async () => {
    const http = vi.fn(async () => ({ status: 200 }));
    let t = 0;
    const svc = new WebhookDeliveryService({ http, now: () => t });
    for (let i = 0; i < 1000; i++) {
      const r = await svc.deliver(endpoint, { i });
      expect(r.ok).toBe(true);
    }
    const blocked = await svc.deliver(endpoint, { i: 'over' });
    expect(blocked.skippedReason).toBe('rate_limited');
    expect(blocked.status).toBe(429);
  });

  it('scopes rate limits per endpoint: one exhausted recipient does not affect another', async () => {
    const http = vi.fn(async () => ({ status: 200 }));
    let t = 0;
    const svc = new WebhookDeliveryService({
      http,
      now: () => t,
      limiterOptions: { maxRequests: 2, windowMs: 1000 },
      dnsResolver: stubDnsResolver,
    });
    const noisy = { id: 'noisy', url: 'https://hook.example/noisy', secret: 's' };
    const quiet = { id: 'quiet', url: 'https://hook.example/quiet', secret: 's' };

    // The noisy endpoint exhausts its own budget...
    expect((await svc.deliver(noisy, {})).ok).toBe(true);
    expect((await svc.deliver(noisy, {})).ok).toBe(true);
    expect((await svc.deliver(noisy, {})).skippedReason).toBe('rate_limited');

    // ...while a different endpoint is unaffected.
    expect((await svc.deliver(quiet, {})).ok).toBe(true);
    expect((await svc.deliver(quiet, {})).ok).toBe(true);
    expect((await svc.deliver(quiet, {})).skippedReason).toBe('rate_limited');

    // Both endpoints delivered only within their own budgets.
    expect(http).toHaveBeenCalledTimes(4);
  });

  it('treats http errors as failures', async () => {
    const http = vi.fn(async () => {
      throw new Error('network');
    });
    const svc = makeService(http);
    const res = await svc.deliver(endpoint, {});
    expect(res.ok).toBe(false);
    expect(res.status).toBe(0);
  });

  it('logs when deliverWithRetry is called without a retry queue', async () => {
    const http = vi.fn(async () => ({ status: 200 }));
    const logger = vi.fn();
    const svc = new WebhookDeliveryService({ http, logger });
    await svc.deliverWithRetry('webhook_1', endpoint, {
      event: 'invoice.paid',
      invoiceId: 1,
      data: {},
      timestamp: new Date().toISOString(),
    });
    expect(logger).toHaveBeenCalledWith('retryQueue not configured');
  });

  it('records retry success and failure outcomes', async () => {
    const http = vi.fn(async (url: string, init: any) => {
      const payload = JSON.parse(init.body);
      return { status: payload.event === 'invoice.paid' ? 200 : 500 };
    });
    const retryQueue = {
      enqueue: vi.fn(() => ({ id: 1, attempts: 0 })),
      recordSuccess: vi.fn(),
      recordFailure: vi.fn(),
      recordSkipped: vi.fn(),
    };
    const logger = vi.fn();
    const svc = new WebhookDeliveryService({ http, retryQueue: retryQueue as any, logger });

    await svc.deliverWithRetry('webhook_1', endpoint, {
      event: 'invoice.paid',
      invoiceId: 1,
      data: {},
      timestamp: new Date().toISOString(),
    });
    expect(retryQueue.enqueue).toHaveBeenCalledTimes(1);
    expect(retryQueue.recordSuccess).toHaveBeenCalledWith(1);

    await svc.deliverWithRetry('webhook_1', endpoint, {
      event: 'invoice.funded',
      invoiceId: 2,
      data: {},
      timestamp: new Date().toISOString(),
    });
    expect(retryQueue.recordFailure).toHaveBeenCalledWith(1, 'HTTP 500');
    expect(logger).toHaveBeenCalledWith('webhook_delivered webhook_id=webhook_1 event=invoice.paid');
  });

  it('records skipped retries when the circuit is open', async () => {
    const http = vi.fn(async () => ({ status: 500 }));
    const retryQueue = {
      enqueue: vi.fn(() => ({ id: 1, attempts: 0 })),
      recordSuccess: vi.fn(),
      recordFailure: vi.fn(),
      recordSkipped: vi.fn(),
    };
    const logger = vi.fn();
    const svc = new WebhookDeliveryService({ http, retryQueue: retryQueue as any, logger });

    for (let i = 0; i < 5; i++) {
      await svc.deliver(endpoint, {
        event: 'invoice.paid',
        invoiceId: i,
        data: {},
        timestamp: new Date().toISOString(),
      });
    }

    await svc.deliverWithRetry('webhook_1', endpoint, {
      event: 'invoice.paid',
      invoiceId: 99,
      data: {},
      timestamp: new Date().toISOString(),
    });

    expect(retryQueue.recordSkipped).toHaveBeenCalledWith(1, 'circuit_open');
    expect(logger).toHaveBeenCalledWith('webhook_skipped webhook_id=webhook_1 reason=circuit_open');
  });

  it('records delivery history when a history store is configured', async () => {
    const http = vi.fn(async () => ({ status: 200 }));
    const historyStore = new DeliveryHistoryStore();
    const addSpy = vi.spyOn(historyStore, 'add');
    const svc = new WebhookDeliveryService({
      http,
      historyStore,
    });

    await svc.deliver(endpoint, {
      event: 'invoice.paid',
      invoiceId: 7,
      data: { token: 'USDC' },
      timestamp: new Date().toISOString(),
    }, 'invoice.paid');

    expect(addSpy).toHaveBeenCalled();
  });

  it('rejects delivery to private IP addresses (SSRF hardening)', async () => {
    const http = vi.fn(async () => ({ status: 200 }));
    const svc = makeService(http);
    
    const resLocalhost = await svc.deliver({ id: 'e2', url: 'http://localhost/test', secret: 's' }, {});
    expect(resLocalhost.ok).toBe(false);
    
    const resPrivate = await svc.deliver({ id: 'e3', url: 'http://10.0.0.1/test', secret: 's' }, {});
    expect(resPrivate.ok).toBe(false);
    
    expect(http).not.toHaveBeenCalled();
  });
});
