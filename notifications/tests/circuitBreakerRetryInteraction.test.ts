import { describe, expect, it, vi } from 'vitest';
import { createNotificationsDatabase } from '../src/database';
import { RetryQueue } from '../src/queue/retryQueue';
import { WebhookDeliveryService, type HttpClient } from '../src/delivery/webhookDelivery';
import { stubDnsResolver } from './helpers/dns';

describe('Circuit Breaker and Retry Queue interaction', () => {
  it('prevents persistently failing endpoints from exhausting retry queue resources while healthy destinations operate normally', async () => {
    let nowMs = 1_700_000_000_000;
    const now = () => nowMs;
    const dateSpy = vi.spyOn(Date, 'now').mockImplementation(() => nowMs);

    const db = createNotificationsDatabase(':memory:');
    db.exec('CREATE TABLE IF NOT EXISTS subscriptions (id TEXT PRIMARY KEY)');
    db.exec("INSERT INTO subscriptions (id) VALUES ('wh_failing'), ('wh_healthy')");
    const retryQueue = new RetryQueue(db);

    const failingCalls: string[] = [];
    const healthyCalls: string[] = [];

    const http: HttpClient = vi.fn(async (url: string) => {
      if (url.includes('failing')) {
        failingCalls.push(url);
        return { status: 500 };
      }
      healthyCalls.push(url);
      return { status: 200 };
    });

    const svc = new WebhookDeliveryService({
      http,
      retryQueue,
      now,
      dnsResolver: stubDnsResolver,
    });

    const failingEndpoint = {
      id: 'ep_failing',
      url: 'https://api.failing-subscriber.com/webhook',
      secret: 'secret_fail',
    };

    const healthyEndpoint = {
      id: 'ep_healthy',
      url: 'https://api.healthy-subscriber.com/webhook',
      secret: 'secret_healthy',
    };

    // Send 50 events to the failing endpoint and 50 events to the healthy endpoint
    for (let i = 1; i <= 50; i++) {
      await svc.deliverWithRetry('wh_failing', failingEndpoint, {
        event: 'invoice.paid',
        invoiceId: i,
        data: { amount: '100' },
        timestamp: new Date(nowMs).toISOString(),
      });

      await svc.deliverWithRetry('wh_healthy', healthyEndpoint, {
        event: 'invoice.paid',
        invoiceId: i,
        data: { amount: '100' },
        timestamp: new Date(nowMs).toISOString(),
      });
    }

    // Circuit breaker for failing endpoint should be open after threshold of 5 failures
    expect(svc.getCircuitState(failingEndpoint.id)).toBe('open');
    expect(svc.getCircuitState(healthyEndpoint.id)).toBe('closed');

    // Failing endpoint should only have received 5 HTTP calls before circuit tripped
    expect(failingCalls).toHaveLength(5);

    // Healthy endpoint should have received all 50 HTTP calls successfully
    expect(healthyCalls).toHaveLength(50);

    // Verify database state in webhook_delivery_logs
    const allLogs = db.prepare('SELECT * FROM webhook_delivery_logs ORDER BY id ASC').all() as Array<{
      webhook_id: string;
      status: string;
      last_error: string | null;
      attempts: number;
    }>;

    const failingLogs = allLogs.filter((l) => l.webhook_id === 'wh_failing');
    const healthyLogs = allLogs.filter((l) => l.webhook_id === 'wh_healthy');

    expect(failingLogs).toHaveLength(50);
    expect(healthyLogs).toHaveLength(50);

    // Healthy logs should all be status = 'delivered'
    expect(healthyLogs.every((l) => l.status === 'delivered')).toBe(true);

    // First 5 failing logs are pending retry, the remaining 45 were skipped due to circuit_open
    const failedInitial = failingLogs.filter((l) => l.status === 'pending' || l.status === 'failed');
    const skippedSubsequent = failingLogs.filter((l) => l.status === 'skipped');

    expect(failedInitial).toHaveLength(5);
    expect(skippedSubsequent).toHaveLength(45);
    expect(skippedSubsequent.every((l) => l.last_error === 'circuit_open')).toBe(true);

    // Advance time by 1000ms (the backoff delay for attempt 1) so pending retries are ready
    nowMs += 1000;

    // Verify retryQueue.getPending() includes only the 5 failed events, and NOT the 45 skipped events
    const pendingRetries = retryQueue.getPending(100);
    const pendingFailingIds = pendingRetries.filter((l) => l.webhookId === 'wh_failing');
    // Only the initial 5 failed requests are eligible for retry, not the 45 skipped ones
    expect(pendingFailingIds).toHaveLength(5);

    // ==========================================
    // Half-Open Probe Behavior Verification
    // ==========================================
    // Advance time past the 10-minute cooldown (600_000 ms)
    nowMs += 600_000;

    // Circuit state transitions to half-open
    expect(svc.getCircuitState(failingEndpoint.id)).toBe('half-open');

    // Next request to failing endpoint acts as a single probe attempt
    await svc.deliverWithRetry('wh_failing', failingEndpoint, {
      event: 'invoice.funded',
      invoiceId: 101,
      data: {},
      timestamp: new Date(nowMs).toISOString(),
    });

    // Exactly 1 probe HTTP call was made (total failingCalls: 5 + 1 = 6)
    expect(failingCalls).toHaveLength(6);

    // Since probe returned 500, circuit re-opens immediately
    expect(svc.getCircuitState(failingEndpoint.id)).toBe('open');

    // Subsequent request is immediately skipped without HTTP attempt
    await svc.deliverWithRetry('wh_failing', failingEndpoint, {
      event: 'invoice.funded',
      invoiceId: 102,
      data: {},
      timestamp: new Date(nowMs).toISOString(),
    });

    expect(failingCalls).toHaveLength(6);

    // Meanwhile, healthy endpoint continues delivering normally
    await svc.deliverWithRetry('wh_healthy', healthyEndpoint, {
      event: 'invoice.funded',
      invoiceId: 103,
      data: {},
      timestamp: new Date(nowMs).toISOString(),
    });

    expect(healthyCalls).toHaveLength(51);

    dateSpy.mockRestore();
    db.close();
  });

  it('resumes normal delivery and closes circuit after successful half-open probe', async () => {
    let nowMs = 1_700_000_000_000;
    const now = () => nowMs;
    const dateSpy = vi.spyOn(Date, 'now').mockImplementation(() => nowMs);

    const db = createNotificationsDatabase(':memory:');
    db.exec('CREATE TABLE IF NOT EXISTS subscriptions (id TEXT PRIMARY KEY)');
    db.exec("INSERT INTO subscriptions (id) VALUES ('wh_recovering')");
    const retryQueue = new RetryQueue(db);

    let failHttp = true;
    const http: HttpClient = vi.fn(async () => {
      if (failHttp) {
        return { status: 500 };
      }
      return { status: 200 };
    });

    const endpoint = {
      id: 'ep_recovering',
      url: 'https://api.subscriber.com/webhook',
      secret: 'secret_rec',
    };

    const svc = new WebhookDeliveryService({
      http,
      retryQueue,
      now,
      dnsResolver: stubDnsResolver,
    });

    // Trip the circuit with 5 failures
    for (let i = 0; i < 5; i++) {
      await svc.deliverWithRetry('wh_recovering', endpoint, {
        event: 'invoice.paid',
        invoiceId: i,
        data: {},
        timestamp: new Date(nowMs).toISOString(),
      });
    }

    expect(svc.getCircuitState(endpoint.id)).toBe('open');

    // Advance past cooldown
    nowMs += 600_000;
    expect(svc.getCircuitState(endpoint.id)).toBe('half-open');

    // Downstream service recovers
    failHttp = false;

    // Send probe event
    await svc.deliverWithRetry('wh_recovering', endpoint, {
      event: 'invoice.paid',
      invoiceId: 99,
      data: {},
      timestamp: new Date(nowMs).toISOString(),
    });

    // Circuit should now be closed again
    expect(svc.getCircuitState(endpoint.id)).toBe('closed');

    // Next event delivers normally without any skipping
    const result = await svc.deliver(endpoint, {
      event: 'invoice.paid',
      invoiceId: 100,
      data: {},
      timestamp: new Date(nowMs).toISOString(),
    });

    expect(result.ok).toBe(true);
    expect(result.skippedReason).toBeUndefined();

    dateSpy.mockRestore();
    db.close();
  });
});
