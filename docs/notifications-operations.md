# Notifications Service Operational Runbook & Performance Guide

## Overview

The Invoice Liquidity Network (ILN) Notifications Service delivers real-time notifications for on-chain Soroban contract events (such as `invoice.funded`, `invoice.paid`, `invoice.disputed`, and `invoice.expiring_soon`) across multi-channel destinations including webhooks, email, Slack, and Telegram.

This document details the operational architecture, burst load test results under simulated mainnet conditions (1,000 concurrent events), fault tolerance mechanisms, security controls, capacity planning guidelines, monitoring runbooks, and the background services layer (`src/services/`) that powers delivery analytics, notification digests, and subscription health.

---

## 1. Architecture Under Burst Load

```
                  +-----------------------------------+
                  |   On-Chain / Contract Ingestion   |
                  +-----------------+-----------------+
                                    |
                                    v
                  +-----------------+-----------------+
                  |   Express Ingestion / Dispatch    |
                  +--------+-----------------+--------+
                           |                 |
          +----------------+                 +---------------+
          |                                                  |
          v                                                  v
+---------+--------------+                         +---------+--------------+
| Webhook Delivery Engine|                         | Email Delivery Engine  |
| - Sliding Window Rate  |                         | - Template Rendering   |
|   Limiter (1000/min)   |                         | - Header/URL Sanitize  |
| - Circuit Breaker      |                         | - Single-Use Auth Token|
| - HMAC-SHA256 Sign     |                         | - Resend API / Preview |
+---------+--------------+                         +---------+--------------+
          |                                                  |
          v                                                  v
+---------+--------------+                         +---------+--------------+
| SQLite (WAL Mode) DB   |                         | Downstream Mail Server |
| - webhook_delivery_logs|                         +------------------------+
| - email_subscriptions  |
+------------------------+
```

### Key Subsystems:
1. **Webhook Delivery Service (`WebhookDeliveryService`)**: Manages per-endpoint lifecycle state with sliding window rate limiters (1,000 req/min default), circuit breakers (5 consecutive failure threshold, 10-minute cooldown), HMAC-SHA256 signature generation (`x-iln-signature`), and delivery history logging.
2. **Retry Queue (`RetryQueue`)**: SQLite-backed persistent queue for delivery attempts, implementing exponential backoff (1s, 5s, 30s) up to 3 attempts with status tracking (`pending`, `delivered`, `failed`, `skipped`).
3. **Email Subscriptions & Delivery (`EmailDeliveryService`, `EmailSubscriptionStore`, `emailToken`)**: Manages double opt-in email subscriptions, single-use HMAC token generation with 128-bit CSPRNG nonces, HTML/text rendering with CRLF header sanitization, HTML attribute escaping, and protocol whitelisting (`http://`, `https://`).
4. **Services Layer (`src/services/`)**: Long-running background services (delivery analytics, notification digest, subscription health) built on a shared `BaseService` lifecycle and managed by a `ServiceRegistry`. See [Section 1.1](#11-services-layer-architectureservices).

### 1.1. Services Layer (`src/services/`)

Background services sit between the API/delivery plumbing and the domain stores. They never import from `api/`; the `api/` routers call into them, and `delivery/` emits fire-and-forget events that services consume.

```
 +---------------------+        +--------------------------------------+
 |   api/ routers      | call   |            services/                 |
 | - analytics.ts      +------->| - deliveryAnalyticsService           |
 | - digest.ts         |        | - digestService                      |
 | - subscriptionHealth|        | - subscriptionHealthService          |
 +----------+----------+        |   (all extend BaseService,           |
            |                   |    registered in ServiceRegistry)    |
            | writes            +---------+--------------+-------------+
            v                             |              | reads summaries
 +---------------------+   events         v              v
 |  subscriptions/     |<-----------+  delivery/    (health policy:
 |  SubscriptionStore  |            |  deliveryEvents  N trips / M days)
 +---------------------+            |  webhookDelivery
                                    +---------------------+
        lib/logger.ts (child loggers), config.ts (env knobs)
```

**`BaseService` contract** (`baseService.ts`):

- **Lifecycle**: `idle → starting → running → stopping → stopped`, or `failed`. `start()` and `stop()` are idempotent; `stop()` on a non-running service resolves immediately.
- **Error handling**: every service body runs through `runSafely(op, fn)`, which catches exceptions, marks the service `failed` during startup, logs via the service's child logger, and — for a running service — keeps the service alive instead of crashing the process.
- **Logging**: each service owns `logger.child({ component: 'services/<name>' })`, so all service logs are greppable by component (see the structured logger in `src/lib/logger.ts`).
- **Health**: `status`, `lastError`, and `startedAt`/`stoppedAt` are exposed for the registry and ops endpoints.

**`ServiceRegistry` contract** (`serviceRegistry.ts`): starts services in registration order, stops them in reverse order, and rolls back already-started services if any start fails (no partially running stack). `src/index.ts` wires the registry into `SIGINT`/`SIGTERM` shutdown so buffered work is flushed before exit.

| Service | Responsibility | Key config (env) |
| :--- | :--- | :--- |
| **`deliveryAnalyticsService`** | Consumes `delivery/` events (`circuit_open`, `rate_limited`, `delivery_success`, `delivery_failure`) and maintains per-endpoint failure summaries (trip counts, throttle events, last failure) with rolling retention. Read via `GET /analytics/deliveries*`. | `NOTIFICATIONS_ANALYTICS_RETENTION_MS` (default 30 days) |
| **`digestService`** | Batches queued notifications per subscriber over a configurable window (default 5 min, max 100 per batch). Digest mode is **opt-in**; opted-out subscribers keep real-time delivery. Buffers are drained to the delivery sink on shutdown. | `NOTIFICATIONS_DIGEST_WINDOW_MS`, `NOTIFICATIONS_DIGEST_MAX_BATCH_SIZE` |
| **`subscriptionHealthService`** | Reads analytics summaries and applies the threshold policy (*N circuit trips over M days*) to flag or auto-suspend persistently failing endpoints, notifies the subscriber over a non-webhook channel (`contact_email`), and supports manual reactivation (`POST /subscriptions/health/:id/reactivate`). | `NOTIFICATIONS_HEALTH_TRIP_THRESHOLD` (3), `NOTIFICATIONS_HEALTH_WINDOW_DAYS` (1), `NOTIFICATIONS_HEALTH_AUTO_SUSPEND` (true) |

**API surface added by the services layer** (writes require the endpoint API key when a `SubscriptionStore` is configured):

- `GET /analytics/deliveries`, `GET /analytics/deliveries/:endpointId`, `GET /analytics/deliveries/:endpointId/counts?since=`
- `GET /subscriptions/digest`, `POST /subscriptions/digest/:subscriberId/opt-in`, `POST /subscriptions/digest/:subscriberId/opt-out`
- `GET /subscriptions/health`, `GET /subscriptions/health/:subscriberId`, `POST /subscriptions/health/evaluate`, `POST /subscriptions/health/:subscriberId/reactivate`

**Operating notes:**

- Suspended endpoints are skipped by delivery with `skippedReason: 'suspended'`; suspension is sticky and cleared only by explicit reactivation (which also resets the analytics counters for that endpoint). "Degraded" flags auto-clear once trips fall below the threshold.
- Digest and analytics state are in-memory by default: a process restart clears digests (undelivered buffered items are flushed on graceful shutdown) and analytics history. Alert on `services/*` `failed` status and on the `suspended` count (see Section 5).
- Webhook subscribers need a `contact_email` on the subscription for health notices; without one, the notice is logged and the suspension still applies.

---

## 2. Mainnet Event Burst Load Test Results

A burst load test was executed simulating a sudden spike of **1,000 concurrent invoice events** across a mixed fleet of downstream targets (healthy webhook endpoints, failing endpoints, and active email subscribers).

### Performance & Latency Metrics

| Metric | Measured Value | Mainnet Target | Status |
| :--- | :--- | :--- | :--- |
| **Total Events Processed** | 1,000 events | 1,000 events | Pass |
| **Total Burst Processing Time** | ~1,005 ms | < 5,000 ms | Optimal |
| **Effective Throughput** | ~994 events/sec | > 200 events/sec | Exceeds Target |
| **Downstream Latency (p50)** | 15.56 ms | < 50 ms | Optimal |
| **Downstream Latency (p90)** | 32.04 ms | < 100 ms | Optimal |
| **Downstream Latency (p95)** | 40.64 ms | < 150 ms | Optimal |
| **Downstream Latency (p99)** | 52.18 ms | < 250 ms | Optimal |
| **Max Latency** | 63.46 ms | < 500 ms | Optimal |
| **Data Loss Rate** | 0.00% (0 / 1,000 lost) | 0.00% | Zero Data Loss |

### Resilience & Downstream Isolation Findings

1. **Circuit Breaker Cutoff Under Burst**:
   - For a persistently failing destination, the circuit breaker tripped immediately upon reaching the 5-failure threshold.
   - Out of 1,000 events dispatched to the failing target, **995 were skipped without making downstream network calls** (`skippedReason: 'circuit_open'`), reducing network socket exhaustion by 99.5%.
   - Healthy webhook destinations achieved **1,000 / 1,000 successful deliveries (100%)** concurrently without latency degradation or crosstalk.
2. **Retry Queue Backlog Absorption**:
   - Skipped deliveries are recorded with `status = 'skipped'` and `last_error = 'circuit_open'` in SQLite.
   - `RetryQueue.getPending()` queries filter on `status IN ('pending', 'failed')`, ensuring skipped events do not flood background retry workers.
   - Failed initial deliveries (5 events) were properly scheduled for exponential retry.
3. **Email Delivery Throughput**:
   - 1,000 email messages were generated, rendered with XSS/injection sanitization, and delivered without dropped tasks or memory leaks.

---

## 3. Fault Tolerance & Security Controls

### Circuit Breaker Specification (`circuitBreaker.ts`)
- **Failure Threshold**: 5 consecutive errors (HTTP 5xx, timeouts, network drops).
- **Cooldown Interval**: 10 minutes (`600,000 ms`).
- **Half-Open Probing**: After cooldown, allows exactly 1 probe request.
  - If probe succeeds (HTTP 2xx): State transitions to `closed`, failure count resets to 0, and normal delivery resumes.
  - If probe fails: State transitions back to `open` immediately for another cooldown cycle.

### Email Injection & Spoofing Defense (`templates/common.ts`, `emailClient.ts`, `verificationEmail.ts`)
- **CRLF Header Injection Neutralization**: `sanitizeHeader()` strips `\r` and `\n` characters from email subject, sender (`from`), and recipient (`to`) fields, neutralizing SMTP/MIME header splitting attacks (`\r\nBcc: ...`).
- **HTML & XSS Content Escaping**: `escapeHtml()` and `escapeAttribute()` escape `&`, `<`, `>`, `"`, and single quotes `'` (`&#39;`) across all user/chain-supplied invoice fields (participant addresses, invoice IDs, token symbols, amounts).
- **URL Protocol Whitelisting**: `sanitizeUrl()` neutralizes `javascript:`, `data:`, and `vbscript:` pseudo-protocols into safe `#` fallbacks while preserving valid `https://` and `http://` destination links.

### Verification Token Security (`emailToken.ts`)
- **Cryptographic Entropy**: 128-bit CSPRNG nonces (`randomBytes(16).toString('hex')`) in every token payload ensure tokens cannot be derived from email addresses or timestamps.
- **HMAC-SHA256 Signatures**: Constant-time signature verification (`timingSafeEqual`) prevents timing attacks.
- **Expiry Enforcement**: Explicit TTL enforcement with future-timestamp rejection (clock-skew protection).
- **Single-Use Replay Protection**: Token consumption tracking (`tokenService.consume(token)`) invalidates tokens upon first activation, rejecting replay attempts with HTTP 400 `invalid_token`.

---

## 4. Production Capacity & Tuning Recommendations

### Database Concurrency (SQLite WAL Mode)
- Always enable Write-Ahead Logging (`PRAGMA journal_mode = WAL;`) and foreign keys (`PRAGMA foreign_keys = ON;`).
- Set `busy_timeout` to at least `5000` ms to prevent `SQLITE_BUSY` errors during concurrent bursts.
- Schedule periodic checkpointing (`PRAGMA wal_checkpoint(TRUNCATE);`) during off-peak windows.

### Worker Pool Sizing
- Recommended worker concurrency: **20 to 50 workers** per notifications service instance.
- Memory allocation: 512 MB to 1 GB RAM per instance is sufficient for 1,000+ events/sec burst workloads.

### Rate Limiting & Circuit Breaker Tuning Presets

| Parameter | Standard Tier | Enterprise / High-Volume |
| :--- | :--- | :--- |
| **Sliding Window Capacity** | 1,000 req / minute | 10,000 req / minute |
| **Circuit Breaker Threshold** | 5 consecutive failures | 10 consecutive failures |
| **Circuit Breaker Cooldown**| 10 minutes | 5 minutes |
| **Max Retry Attempts** | 3 (1s, 5s, 30s) | 5 (1s, 5s, 30s, 2m, 10m) |

---

## 5. Monitoring, Alerting & Incident Response

### Key SLIs & SLOs

| Service Level Indicator (SLI) | Target (SLO) | Alert Trigger |
| :--- | :--- | :--- |
| **Webhook Delivery Success Rate** | > 99.9% (excluding open circuits) | Error rate > 1% over 5m |
| **P95 Delivery Latency** | < 150 ms | P95 > 300 ms over 5m |
| **Retry Queue Pending Lag** | < 100 items pending | Backlog > 500 items |
| **Circuit Breaker Open State** | < 5% of active endpoints | > 10 endpoints open simultaneously |

### Common Alerts & Remediation

1. **`Alert: NotificationCircuitBreakerOpen`**:
   - *Impact*: Webhook destination is experiencing persistent downstream failures; deliveries are skipped.
   - *Action*: Check subscriber endpoint URL health, inspect `webhook_delivery_logs` for last error (e.g. HTTP 502/503), contact subscriber if external.
2. **`Alert: RetryQueueDepthHigh`**:
   - *Impact*: Retry backlog is growing due to downstream network transient issues.
   - *Action*: Check database I/O performance, verify network egress, scale background retry workers.
3. **`Alert: RateLimitExceededSpike`**:
   - *Impact*: Subscriber webhook is exceeding configured sliding window threshold (HTTP 429).
   - *Action*: Verify subscriber tier allocation and offer upgrade to dedicated enterprise rate limit.
4. **`Alert: SubscriptionAutoSuspended`**:
   - *Impact*: `subscriptionHealthService` suspended an endpoint after `NOTIFICATIONS_HEALTH_TRIP_THRESHOLD` circuit trips within `NOTIFICATIONS_HEALTH_WINDOW_DAYS`; deliveries are skipped (`skippedReason: 'suspended'`) and a notice was sent to the subscriber's `contact_email`.
   - *Action*: Check `GET /subscriptions/health/:subscriberId` for trip history, confirm the endpoint is fixed, then `POST /subscriptions/health/:subscriberId/reactivate` (resets counters and resumes delivery).
5. **`Alert: ServicesComponentFailed`**:
   - *Impact*: A `services/*` component transitioned to `failed` (startup error) or logged an unhandled runtime error; analytics/digest/health may be stale.
   - *Action*: Grep logs for `component=services/`, inspect `lastError` from the service status, restart the process if the registry did not recover.
