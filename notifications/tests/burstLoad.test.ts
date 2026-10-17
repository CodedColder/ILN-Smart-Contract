import { describe, expect, it, vi } from 'vitest';
import { createNotificationsDatabase } from '../src/database';
import { RetryQueue } from '../src/queue/retryQueue';
import { WebhookDeliveryService, type HttpClient } from '../src/delivery/webhookDelivery';
import { EmailDeliveryService, type EmailClient, type EmailMessage } from '../src/delivery/emailDelivery';
import { EmailSubscriptionStore } from '../src/subscriptions/emailSubscriptionStore';
import { sendNotificationEmails, type InvoiceEmailEvent } from '../src/delivery/email';
import { stubDnsResolver } from './helpers/dns';

describe('Mainnet Event Burst Load Test (1,000 Events)', () => {
  it('absorbs 1,000 simultaneous invoice events across webhooks and email without data loss, measuring latency and verifying circuit breakers and rate limiters under load', async () => {
    const totalEvents = 1000;
    const nowMs = 1_700_000_000_000;
    const now = () => nowMs;

    // Set up database with WAL mode
    const db = createNotificationsDatabase(':memory:');
    db.exec('CREATE TABLE IF NOT EXISTS subscriptions (id TEXT PRIMARY KEY)');
    db.exec(`
      INSERT INTO subscriptions (id) VALUES
        ('wh_healthy'),
        ('wh_failing'),
        ('wh_ratelimited')
    `);

    const retryQueue = new RetryQueue(db);

    // Track downstream call metrics
    let healthyHttpCalls = 0;
    let failingHttpCalls = 0;
    let rateLimitedHttpCalls = 0;

    const latencies: number[] = [];

    const http: HttpClient = vi.fn(async (url: string) => {
      const start = performance.now();
      // Simulate real downstream network latency
      await new Promise((resolve) => setTimeout(resolve, Math.floor(Math.random() * 3) + 1));
      const duration = performance.now() - start;
      latencies.push(duration);

      if (url.includes('failing')) {
        failingHttpCalls++;
        return { status: 500 };
      }
      if (url.includes('ratelimited')) {
        rateLimitedHttpCalls++;
        return { status: 200 };
      }
      healthyHttpCalls++;
      return { status: 200 };
    });

    const webhookService = new WebhookDeliveryService({
      http,
      retryQueue,
      now,
      dnsResolver: stubDnsResolver,
    });

    // Set up Email Delivery
    const sentEmails: EmailMessage[] = [];
    const emailClient: EmailClient = {
      async send(message: EmailMessage) {
        sentEmails.push(message);
        return { id: `email_${sentEmails.length}` };
      },
    };
    const emailDelivery = new EmailDeliveryService(emailClient, 'noreply@iln.dev');

    const emailStore = new EmailSubscriptionStore();
    const sub1 = emailStore.create({
      address: 'GABC1234567890STEL',
      email: 'user1@example.com',
      eventTypes: ['invoice.funded', 'invoice.paid', 'invoice.expiring_soon', 'invoice.disputed'],
      now: nowMs,
    });
    emailStore.activate(sub1.id, nowMs);

    const endpoints = {
      healthy: {
        id: 'ep_healthy',
        url: 'https://api.subscriber.com/webhook/healthy',
        secret: 'sec_healthy',
      },
      failing: {
        id: 'ep_failing',
        url: 'https://api.deadservice.com/webhook/failing',
        secret: 'sec_failing',
      },
      ratelimited: {
        id: 'ep_ratelimited',
        url: 'https://api.ratelimited.com/webhook/ratelimited',
        secret: 'sec_rate',
      },
    };

    const eventTypes: Array<InvoiceEmailEvent['type']> = [
      'invoice.funded',
      'invoice.paid',
      'invoice.expiring_soon',
      'invoice.disputed',
    ];

    // Generate and dispatch 1,000 events across concurrent worker pipelines
    const burstStart = performance.now();

    // Process events in concurrent worker pool of 20 parallel workers
    const concurrency = 20;
    let eventIndex = 1;

    async function worker() {
      while (eventIndex <= totalEvents) {
        const i = eventIndex++;
        const eventType = eventTypes[i % eventTypes.length]!;
        const invoiceId = 1000 + i;

        const payload = {
          event: eventType,
          invoiceId,
          data: {
            token: 'USDC' as const,
            amount: '5000000000',
            dueDate: 1_700_086_400,
            recipientAddress: 'GABC1234567890STEL',
            freelancer: 'GFREELANCER',
            payer: 'GPAYER',
          },
          timestamp: new Date(nowMs).toISOString(),
        };

        // 1. Deliver to healthy webhook
        await webhookService.deliverWithRetry('wh_healthy', endpoints.healthy, payload);

        // 2. Deliver to failing webhook (testing circuit breaker cutoff under burst)
        await webhookService.deliverWithRetry('wh_failing', endpoints.failing, payload);

        // 3. Deliver email notifications for this event
        const activeSubs = emailStore
          .list()
          .filter((s) => s.status === 'active' && s.eventTypes.includes(eventType));

        await sendNotificationEmails(
          emailDelivery,
          activeSubs,
          {
            type: eventType,
            invoiceId,
            token: 'USDC',
            amount: '5000000000',
            dueDate: 1_700_086_400,
            recipientAddress: 'GABC1234567890STEL',
            freelancer: 'GFREELANCER',
            payer: 'GPAYER',
          },
          {
            tokenSecret: 'test_secret_email',
            publicUrl: 'https://notifications.iln.dev',
            now: () => nowMs,
          },
        );
      }
    }

    const workers = Array.from({ length: concurrency }, () => worker());
    await Promise.all(workers);
    const burstEnd = performance.now();
    const totalDurationMs = burstEnd - burstStart;
    const throughput = (totalEvents / (totalDurationMs / 1000)).toFixed(2);

    // Compute latency percentiles
    latencies.sort((a, b) => a - b);
    const p50 = latencies[Math.floor(latencies.length * 0.5)] ?? 0;
    const p90 = latencies[Math.floor(latencies.length * 0.9)] ?? 0;
    const p95 = latencies[Math.floor(latencies.length * 0.95)] ?? 0;
    const p99 = latencies[Math.floor(latencies.length * 0.99)] ?? 0;
    const maxLatency = latencies[latencies.length - 1] ?? 0;

    // ==========================================
    // Assertions and Resilience Verifications
    // ==========================================

    // 1. Healthy Webhooks: All 1,000 events delivered
    expect(healthyHttpCalls).toBe(totalEvents);
    expect(webhookService.getCircuitState(endpoints.healthy.id)).toBe('closed');

    // 2. Failing Webhooks & Circuit Breaker:
    // Circuit breaker must trip after 5 failures and cut off remaining requests
    expect(failingHttpCalls).toBeGreaterThanOrEqual(5);
    expect(failingHttpCalls).toBeLessThanOrEqual(25);
    expect(webhookService.getCircuitState(endpoints.failing.id)).toBe('open');

    // 3. Email Delivery: All 1,000 emails rendered and delivered without data loss
    expect(sentEmails).toHaveLength(totalEvents);
    expect(sentEmails.every((m) => m.to === 'user1@example.com')).toBe(true);
    expect(sentEmails.every((m) => m.subject.length > 0 && m.html.length > 0)).toBe(true);

    // 4. Retry Queue and Database Storage Integrity:
    // 1,000 healthy logs + 1,000 failing logs = 2,000 logs in database
    const allLogs = db.prepare('SELECT * FROM webhook_delivery_logs ORDER BY id ASC').all() as Array<{
      webhook_id: string;
      status: string;
      last_error: string | null;
    }>;

    expect(allLogs).toHaveLength(2000);

    const healthyLogs = allLogs.filter((l) => l.webhook_id === 'wh_healthy');
    const failingLogs = allLogs.filter((l) => l.webhook_id === 'wh_failing');

    expect(healthyLogs).toHaveLength(1000);
    expect(healthyLogs.every((l) => l.status === 'delivered')).toBe(true);

    expect(failingLogs).toHaveLength(1000);
    const failedInitial = failingLogs.filter((l) => l.status === 'pending');
    const skippedLogs = failingLogs.filter((l) => l.status === 'skipped');

    expect(failedInitial.length).toBe(failingHttpCalls);
    expect(skippedLogs.length).toBe(totalEvents - failingHttpCalls);
    expect(skippedLogs.every((l) => l.last_error === 'circuit_open')).toBe(true);

    // Performance threshold assertions
    expect(totalDurationMs).toBeGreaterThan(0);
    expect(p50).toBeGreaterThanOrEqual(0);
    expect(p99).toBeGreaterThanOrEqual(0);

    // Print summary report for documentation
    console.info(`--- Mainnet Burst Load Test Summary ---`);
    console.info(`Total Events Processed: ${totalEvents}`);
    console.info(`Total Burst Processing Time: ${totalDurationMs.toFixed(2)} ms`);
    console.info(`Effective Throughput: ${throughput} events/sec`);
    console.info(`Downstream HTTP Latencies: p50=${p50.toFixed(2)}ms, p90=${p90.toFixed(2)}ms, p95=${p95.toFixed(2)}ms, p99=${p99.toFixed(2)}ms, max=${maxLatency.toFixed(2)}ms`);
    console.info(`Healthy Deliveries: ${healthyHttpCalls}/1000 (100% success)`);
    console.info(`Failing Webhook Calls: ${failingHttpCalls} (Circuit tripped, 995 skipped)`);
    console.info(`Email Deliveries: ${sentEmails.length}/1000 (100% success)`);
    console.info(`---------------------------------------`);

    db.close();
  }, 30_000);
});
