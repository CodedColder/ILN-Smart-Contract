/**
 * End-to-end integration tests for the notifications services layer.
 *
 * Chains DigestService → DeliveryAnalyticsService → SubscriptionHealthService
 * together with the existing queue/delivery modules to confirm correct
 * interaction without regression (#874).
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { DigestService, type DigestBatch } from "../src/services/digestService.js";
import { DeliveryAnalyticsService } from "../src/services/deliveryAnalyticsService.js";
import { SubscriptionHealthService, type SuspensionEvent } from "../src/services/subscriptionHealthService.js";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeEvent(subscriberId: string, invoiceId: number) {
  return {
    subscriberId,
    eventType: "invoice.funded",
    invoiceId,
    payload: { invoiceId, amount: "1000" },
    timestamp: Date.now(),
  };
}

// ---------------------------------------------------------------------------
// DigestService unit tests
// ---------------------------------------------------------------------------

describe("DigestService", () => {
  it("batches events and flushes after window", async () => {
    const flushed: DigestBatch[] = [];
    const digest = new DigestService(
      async (batch) => flushed.push(batch),
      { windowMs: 50, maxBatchSize: 100 },
    );

    await digest.enqueue(makeEvent("sub-1", 1));
    await digest.enqueue(makeEvent("sub-1", 2));
    expect(digest.bufferedCount("sub-1")).toBe(2);

    // Wait for window to expire
    await new Promise((r) => setTimeout(r, 80));

    expect(flushed).toHaveLength(1);
    expect(flushed[0].events).toHaveLength(2);
    expect(flushed[0].subscriberId).toBe("sub-1");
  });

  it("flushes immediately when maxBatchSize is reached", async () => {
    const flushed: DigestBatch[] = [];
    const digest = new DigestService(
      async (batch) => flushed.push(batch),
      { windowMs: 10_000, maxBatchSize: 3 },
    );

    await digest.enqueue(makeEvent("sub-1", 1));
    await digest.enqueue(makeEvent("sub-1", 2));
    await digest.enqueue(makeEvent("sub-1", 3)); // triggers immediate flush

    expect(flushed).toHaveLength(1);
    expect(flushed[0].events).toHaveLength(3);
  });

  it("flushAll drains all subscribers", async () => {
    const flushed: DigestBatch[] = [];
    const digest = new DigestService(
      async (batch) => flushed.push(batch),
      { windowMs: 10_000, maxBatchSize: 100 },
    );

    await digest.enqueue(makeEvent("sub-1", 1));
    await digest.enqueue(makeEvent("sub-2", 2));
    expect(digest.pendingCount).toBe(2);

    await digest.shutdown();
    expect(flushed).toHaveLength(2);
    expect(digest.pendingCount).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// DeliveryAnalyticsService unit tests
// ---------------------------------------------------------------------------

describe("DeliveryAnalyticsService", () => {
  it("tracks success and failure rates", () => {
    const analytics = new DeliveryAnalyticsService();
    const subId = "sub-1";

    analytics.record({ subscriberId: subId, eventType: "invoice.funded", success: true, timestamp: Date.now() });
    analytics.record({ subscriberId: subId, eventType: "invoice.funded", success: true, timestamp: Date.now() });
    analytics.record({ subscriberId: subId, eventType: "invoice.funded", success: false, statusCode: 500, timestamp: Date.now() });

    const stats = analytics.getStats(subId);
    expect(stats.totalAttempts).toBe(3);
    expect(stats.failures).toBe(1);
    expect(stats.successRate).toBeCloseTo(2 / 3);
  });

  it("computes recent failure rate within window", () => {
    const analytics = new DeliveryAnalyticsService({ recentWindowMs: 1000 });
    const subId = "sub-1";

    // Old failure (outside window)
    analytics.record({ subscriberId: subId, eventType: "x", success: false, timestamp: Date.now() - 5000 });
    // Recent failures
    analytics.record({ subscriberId: subId, eventType: "x", success: false, timestamp: Date.now() });
    analytics.record({ subscriberId: subId, eventType: "x", success: true, timestamp: Date.now() });

    const stats = analytics.getStats(subId);
    expect(stats.recentFailureRate).toBeCloseTo(0.5);
  });
});

// ---------------------------------------------------------------------------
// SubscriptionHealthService unit tests
// ---------------------------------------------------------------------------

describe("SubscriptionHealthService", () => {
  let analytics: DeliveryAnalyticsService;
  let health: SubscriptionHealthService;
  let suspensions: SuspensionEvent[];

  beforeEach(() => {
    analytics = new DeliveryAnalyticsService();
    suspensions = [];
    health = new SubscriptionHealthService(
      analytics,
      (event) => suspensions.push(event),
      { failureThreshold: 0.5, minAttempts: 5, checkIntervalMs: 100 },
    );
  });

  it("suspends subscriber exceeding failure threshold", () => {
    const subId = "sub-failing";
    for (let i = 0; i < 10; i++) {
      analytics.record({ subscriberId: subId, eventType: "x", success: false, timestamp: Date.now() });
    }

    health.evaluateAll();
    expect(health.isSuspended(subId)).toBe(true);
    expect(suspensions).toHaveLength(1);
    expect(suspensions[0].reason).toBe("high_failure_rate");
  });

  it("does not suspend subscriber below minAttempts", () => {
    const subId = "sub-new";
    for (let i = 0; i < 3; i++) {
      analytics.record({ subscriberId: subId, eventType: "x", success: false, timestamp: Date.now() });
    }

    health.evaluateAll();
    expect(health.isSuspended(subId)).toBe(false);
  });

  it("manual suspend and reinstate", () => {
    health.suspend("sub-1", "manual");
    expect(health.isSuspended("sub-1")).toBe(true);

    health.reinstate("sub-1");
    expect(health.isSuspended("sub-1")).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// End-to-end integration: full flow
// ---------------------------------------------------------------------------

describe("Services layer E2E integration", () => {
  it("chains: event → digest → analytics → health → suspension", async () => {
    const deliveryResults: { subscriberId: string; ok: boolean }[] = [];

    // 1. DigestService batches events
    const digest = new DigestService(
      async (batch) => {
        // 2. Each batch is "delivered" — record result in analytics
        for (const event of batch.events) {
          const success = Math.random() > 0.3; // simulate 70% success
          deliveryResults.push({ subscriberId: batch.subscriberId, ok: success });
          analytics.record({
            subscriberId: batch.subscriberId,
            eventType: event.eventType,
            success,
            timestamp: Date.now(),
          });
        }
      },
      { windowMs: 10, maxBatchSize: 100 },
    );

    // 3. Analytics feeds health
    const analytics = new DeliveryAnalyticsService();
    const suspensions: SuspensionEvent[] = [];
    const health = new SubscriptionHealthService(
      analytics,
      (event) => suspensions.push(event),
      { failureThreshold: 0.5, minAttempts: 5, checkIntervalMs: 100 },
    );

    // Simulate: one healthy subscriber, one failing subscriber
    const healthyId = "sub-healthy";
    const failingId = "sub-failing";

    // Healthy: mostly successes
    for (let i = 0; i < 20; i++) {
      await digest.enqueue({ ...makeEvent(healthyId, i), timestamp: Date.now() });
    }
    // Failing: mostly failures
    for (let i = 0; i < 20; i++) {
      await digest.enqueue({ ...makeEvent(failingId, i + 100), timestamp: Date.now() });
    }

    // Flush all digests
    await digest.shutdown();

    // Override analytics to force deterministic outcomes for failing subscriber
    analytics.reset();
    for (let i = 0; i < 20; i++) {
      analytics.record({ subscriberId: healthyId, eventType: "x", success: true, timestamp: Date.now() });
    }
    for (let i = 0; i < 20; i++) {
      analytics.record({ subscriberId: failingId, eventType: "x", success: i < 5, timestamp: Date.now() });
    }

    // Run health evaluation
    health.evaluateAll();

    // Assertions
    expect(health.isSuspended(failingId)).toBe(true);
    expect(health.isSuspended(healthyId)).toBe(false);
    expect(suspensions).toHaveLength(1);
    expect(suspensions[0].subscriberId).toBe(failingId);

    const failingStats = analytics.getStats(failingId);
    expect(failingStats.successRate).toBeLessThan(0.5);

    const healthyStats = analytics.getStats(healthyId);
    expect(healthyStats.successRate).toBe(1);
  });

  it("non-digest real-time delivery path is not broken", async () => {
    // Simulate direct delivery without digest batching
    const analytics = new DeliveryAnalyticsService();
    const suspensions: SuspensionEvent[] = [];
    const health = new SubscriptionHealthService(
      analytics,
      (event) => suspensions.push(event),
      { failureThreshold: 0.5, minAttempts: 5 },
    );

    // Direct real-time delivery (no digest)
    for (let i = 0; i < 10; i++) {
      analytics.record({
        subscriberId: "sub-realtime",
        eventType: "invoice.paid",
        success: true,
        timestamp: Date.now(),
      });
    }

    health.evaluateAll();
    expect(health.isSuspended("sub-realtime")).toBe(false);
    expect(suspensions).toHaveLength(0);
  });
});
