import { CircuitBreaker, type CircuitState } from './circuitBreaker.js';
import { SlidingWindowRateLimiter, type RateLimiterOptions } from './rateLimiter.js';
import { signPayload } from './signature.js';
import { validateWebhookUrl, type DnsResolver } from './ssrfValidator.js';
import type { DeliveryEventSink } from './deliveryEvents.js';
import type { RetryQueue } from '../queue/retryQueue.js';
import type { DeliveryHistoryStore } from './deliveryHistory.js';

export interface WebhookEndpoint {
  id: string;
  url: string;
  secret: string;
}

export interface DeliveryResult {
  ok: boolean;
  status: number;
  skippedReason?: 'circuit_open' | 'rate_limited' | 'suspended';
}

export type HttpClient = (
  url: string,
  init: { method: string; headers: Record<string, string>; body: string },
) => Promise<{ status: number }>;

export interface WebhookDeliveryOptions {
  http: HttpClient;
  logger?: (msg: string) => void;
  now?: () => number;
  retryQueue?: RetryQueue;
  historyStore?: DeliveryHistoryStore;
  /**
   * Per-endpoint rate-limit policy. Limits are already scoped per webhook
   * endpoint (one limiter per endpoint id / URL), so a misconfigured
   * high-volume subscriber can never starve delivery to other subscribers
   * (Issue #728). These options only tune the budget for each endpoint.
   */
  limiterOptions?: RateLimiterOptions;
  /**
   * DNS resolver used by the SSRF pre-flight check. Injectable so tests never
   * hit real DNS; production leaves it unset and gets the system resolver.
   */
  dnsResolver?: DnsResolver | undefined;
  /**
   * Sink for delivery signals (trips, throttles, HTTP outcomes) consumed by
   * the delivery-analytics service (Issue #872).
   */
  events?: DeliveryEventSink | undefined;
  /**
   * Predicate supplied by the subscription-health service (Issue #873):
   * endpoints it reports as suspended are skipped before any budget or
   * network work is done.
   */
  isSuspended?: ((endpointId: string) => boolean) | undefined;
}

interface EndpointState {
  breaker: CircuitBreaker;
  limiter: SlidingWindowRateLimiter;
}

export interface WebhookPayload {
  event: string;
  invoiceId: number;
  data: unknown;
  timestamp: string;
}

export class WebhookDeliveryService {
  private readonly endpoints = new Map<string, EndpointState>();

  constructor(private readonly opts: WebhookDeliveryOptions) {}

  getCircuitState(endpointId: string): CircuitState {
    return this.stateFor(endpointId).breaker.getState();
  }

  async deliver(
    endpoint: WebhookEndpoint,
    payload: unknown,
    eventType?: string,
  ): Promise<DeliveryResult> {
    const state = this.stateFor(endpoint.id);
    if (this.opts.isSuspended?.(endpoint.id)) {
      this.opts.logger?.(`webhook_suspended endpoint=${endpoint.id}`);
      return { ok: false, status: 0, skippedReason: 'suspended' };
    }
    if (!state.limiter.tryConsume()) {
      this.opts.logger?.(`webhook_rate_limited endpoint=${endpoint.id}`);
      this.opts.events?.record({
        type: 'rate_limited',
        endpointId: endpoint.id,
        at: this.now(),
      });
      return { ok: false, status: 429, skippedReason: 'rate_limited' };
    }
    if (!state.breaker.canAttempt()) {
      this.opts.logger?.(`webhook_circuit_open endpoint=${endpoint.id}`);
      return { ok: false, status: 0, skippedReason: 'circuit_open' };
    }

    const body = JSON.stringify(payload);
    const signature = signPayload(endpoint.secret, body);
    try {
      await validateWebhookUrl(endpoint.url, this.opts.dnsResolver);
    } catch (err) {
      this.opts.logger?.(`webhook_ssrf_blocked url=${endpoint.url} err=${err}`);
      return { ok: false, status: 0 };
    }
    let statusCode = 0;
    let responseBody = '';
    try {
      const res = await this.opts.http(endpoint.url, {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          'x-iln-signature': signature,
        },
        body,
      });
      statusCode = res.status;
      if (res.status >= 200 && res.status < 300) {
        state.breaker.recordSuccess();
      } else {
        state.breaker.recordFailure(this.opts.logger);
      }
    } catch (err) {
      state.breaker.recordFailure(this.opts.logger);
      statusCode = 0;
      responseBody = err instanceof Error ? err.message : String(err);
    }

    const delivered = statusCode >= 200 && statusCode < 300;
    this.opts.events?.record({
      type: delivered ? 'delivery_success' : 'delivery_failure',
      endpointId: endpoint.id,
      at: this.now(),
      status: statusCode,
    });

    if (this.opts.historyStore && eventType) {
      this.opts.historyStore.add({
        webhookId: endpoint.id,
        eventType,
        deliveredAt: Date.now(),
        statusCode,
        responseBody,
        attemptCount: 1,
        nextRetryAt: statusCode >= 500 ? Date.now() + 60000 : null,
      });
    }

    return {
      ok: delivered,
      status: statusCode,
    };
  }

  async deliverWithRetry(
    webhookId: string,
    endpoint: WebhookEndpoint,
    payload: WebhookPayload,
  ): Promise<void> {
    if (!this.opts.retryQueue) {
      this.opts.logger?.('retryQueue not configured');
      return;
    }

    const log = this.opts.retryQueue.enqueue(
      webhookId,
      payload.event,
      payload.invoiceId,
      payload,
    );

    const result = await this.deliver(endpoint, payload, payload.event);

    if (result.ok) {
      this.opts.retryQueue.recordSuccess(log.id);
      this.opts.logger?.(`webhook_delivered webhook_id=${webhookId} event=${payload.event}`);
    } else if (result.skippedReason) {
      this.opts.retryQueue.recordSkipped(log.id, result.skippedReason);
      this.opts.logger?.(`webhook_skipped webhook_id=${webhookId} reason=${result.skippedReason}`);
    } else {
      this.opts.retryQueue.recordFailure(log.id, `HTTP ${result.status}`);
      this.opts.logger?.(`webhook_failed webhook_id=${webhookId} attempt=${log.attempts + 1}`);
    }
  }

  private now(): number {
    return (this.opts.now ?? Date.now)();
  }

  private stateFor(endpointId: string): EndpointState {
    let s = this.endpoints.get(endpointId);
    if (!s) {
      s = {
        breaker: new CircuitBreaker({
          now: this.opts.now,
          onTrip: () =>
            this.opts.events?.record({
              type: 'circuit_open',
              endpointId,
              at: this.now(),
            }),
        }),
        limiter: new SlidingWindowRateLimiter({ now: this.opts.now, ...this.opts.limiterOptions }),
      };
      this.endpoints.set(endpointId, s);
    }
    return s;
  }
}
