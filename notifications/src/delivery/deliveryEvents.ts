/**
 * Delivery signal events (Issue #872).
 *
 * `WebhookDeliveryService` emits these as it works so higher layers can
 * correlate them without re-implementing delivery logic:
 *
 *   - `rate_limited`   — the per-endpoint sliding window throttled a send;
 *   - `circuit_open`   — a circuit breaker *trip* (state transition into
 *                        `open`), not every send skipped by an open circuit;
 *   - `delivery_success` / `delivery_failure` — the HTTP outcome itself.
 *
 * `DeliveryAnalyticsService` implements {@link DeliveryEventSink} and is
 * handed to the delivery service, so the analytics layer observes delivery
 * passively instead of the delivery layer depending on analytics.
 */

export type DeliveryEventType =
  | 'circuit_open'
  | 'rate_limited'
  | 'delivery_success'
  | 'delivery_failure';

export interface DeliveryEvent {
  type: DeliveryEventType;
  /** Endpoint (subscriber) id the event belongs to. */
  endpointId: string;
  /** Epoch milliseconds — taken from the delivery service clock. */
  at: number;
  /** HTTP status of the attempt; `0` for network-level failures. */
  status?: number;
}

export interface DeliveryEventSink {
  record(event: DeliveryEvent): void;
}
