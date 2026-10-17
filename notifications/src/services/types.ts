/**
 * Services layer contracts (Issue #870).
 *
 * The notifications service is split into four layers:
 *
 *   api/           Express routers: transport, validation, status codes.
 *   services/      Long-lived background work with a lifecycle: delivery
 *                  analytics, digests, subscription health.
 *   delivery/      Pure delivery primitives (webhooks, email, circuit
 *                  breaker, rate limiter, SSRF validation, history).
 *   queue/         Persistence (SQLite retry queue, subscription stores).
 *
 * plus `lib/` (structured logging, correlation ids) which every layer uses.
 *
 * Services sit *above* delivery and queue: they observe delivery through the
 * {@link DeliveryEventSink} they are handed, never by reaching back into the
 * routers. Routers depend on services, so a service can be started, stopped
 * and tested without an HTTP server.
 *
 * Every service:
 *   - extends `BaseService`, giving it an explicit `start()` / `stop()`
 *     lifecycle and a `state` that operators can observe;
 *   - logs through `lib/logger.ts` under `component: services/<name>`;
 *   - takes its clock (`now`) and logger as constructor options so tests are
 *     deterministic and silent;
 *   - never lets a background failure escape as an unhandled rejection —
 *     work goes through `runSafely`, which logs and continues.
 */

import type { Logger } from '../lib/logger.js';

/** Observable lifecycle state of a service. */
export type ServiceState = 'idle' | 'starting' | 'running' | 'stopping' | 'stopped' | 'failed';

/** Dependencies every service accepts. All are optional. */
export interface ServiceContext {
  /** Structured logger. Defaults to the root logger from `lib/logger.ts`. */
  logger?: Logger | undefined;
  /** Injectable clock. Defaults to `Date.now`. */
  now?: (() => number) | undefined;
}

/** The surface every service exposes to `ServiceRegistry` and the API layer. */
export interface Service {
  readonly name: string;
  readonly state: ServiceState;
  start(): Promise<void>;
  stop(): Promise<void>;
}
