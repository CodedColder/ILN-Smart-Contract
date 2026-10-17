import { logger as rootLogger, type Logger } from '../lib/logger.js';
import type { Service, ServiceContext, ServiceState } from './types.js';

/** Render an unknown thrown value as a loggable string. */
export function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * Shared base for every service in `services/`.
 *
 * Lifecycle: `idle → starting → running → stopping → stopped`, with `failed`
 * on any thrown `onStart` / `onStop`. `start()` is idempotent while running,
 * so wiring two owners (an app bootstrap and a test) cannot double-start work.
 *
 * Error handling contract: lifecycle failures are logged *and* rethrown (an
 * operator must know the service is down), while recurring background work is
 * run through {@link runSafely}, which logs and returns a fallback so one bad
 * tick can never produce an unhandled rejection or stop the timer.
 */
export abstract class BaseService implements Service {
  readonly name: string;
  /** Structured logger bound to `component: services/<name>`. */
  protected readonly log: Logger;
  /** Injectable clock; services schedule and window against this. */
  protected readonly now: () => number;

  private current: ServiceState = 'idle';

  constructor(name: string, context: ServiceContext = {}) {
    this.name = name;
    this.log = (context.logger ?? rootLogger).child({ component: `services/${name}` });
    this.now = context.now ?? Date.now;
  }

  get state(): ServiceState {
    return this.current;
  }

  async start(): Promise<void> {
    if (this.current === 'running' || this.current === 'starting') {
      this.log.warn('service_start_ignored', { state: this.current });
      return;
    }
    this.current = 'starting';
    try {
      await this.onStart();
      this.current = 'running';
      this.log.info('service_started');
    } catch (err) {
      this.current = 'failed';
      this.log.error('service_start_failed', { error: errorMessage(err) });
      throw err;
    }
  }

  async stop(): Promise<void> {
    if (this.current === 'idle' || this.current === 'stopped') {
      this.log.debug('service_stop_ignored', { state: this.current });
      return;
    }
    this.current = 'stopping';
    try {
      await this.onStop();
      this.current = 'stopped';
      this.log.info('service_stopped');
    } catch (err) {
      this.current = 'failed';
      this.log.error('service_stop_failed', { error: errorMessage(err) });
      throw err;
    }
  }

  /** Subclass hook run once per successful `start()`. */
  protected abstract onStart(): Promise<void> | void;

  /** Subclass hook run once per successful `stop()`. */
  protected abstract onStop(): Promise<void> | void;

  /**
   * Run one unit of recurring background work.
   *
   * Failures are logged with the operation name and replaced by `fallback`,
   * so a transient error (a notifier that is down, a flush that threw) is
   * visible in structured logs but does not kill the service.
   */
  protected async runSafely<T>(
    operation: string,
    fn: () => T | Promise<T>,
    fallback: T,
  ): Promise<T> {
    try {
      return await fn();
    } catch (err) {
      this.log.error('service_operation_failed', { operation, error: errorMessage(err) });
      return fallback;
    }
  }
}
