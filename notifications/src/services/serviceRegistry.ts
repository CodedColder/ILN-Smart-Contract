import { errorMessage } from './baseService.js';

/** Structural alias so the registry does not depend on the concrete base. */
export type BaseServiceLike = {
  readonly name: string;
  start(): Promise<void>;
  stop(): Promise<void>;
};

/**
 * Owns the lifecycle of a group of services so they start in registration
 * order and stop in reverse (Issue #870).
 *
 * Start is transactional: if one service fails to start, everything already
 * started is stopped again, so the process never runs with half a stack.
 * Stop attempts every service even when one throws — the first failure is
 * rethrown after cleanup so it still surfaces to the operator.
 */
export class ServiceRegistry {
  private readonly services: BaseServiceLike[] = [];

  register(service: BaseServiceLike): this {
    this.services.push(service);
    return this;
  }

  list(): readonly BaseServiceLike[] {
    return this.services;
  }

  async startAll(): Promise<void> {
    const started: BaseServiceLike[] = [];
    for (const service of this.services) {
      try {
        await service.start();
        started.push(service);
      } catch (err) {
        for (const already of started.reverse()) {
          try {
            await already.stop();
          } catch {
            // The start failure is the actionable error; a failed rollback is
            // already logged by the service itself.
          }
        }
        throw err;
      }
    }
  }

  async stopAll(): Promise<void> {
    let firstFailure: unknown = null;
    for (const service of [...this.services].reverse()) {
      try {
        await service.stop();
      } catch (err) {
        firstFailure = firstFailure ?? err;
      }
    }
    if (firstFailure !== null) {
      throw new Error(`service_stop_failed: ${errorMessage(firstFailure)}`);
    }
  }
}
