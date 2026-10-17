import { describe, expect, it } from 'vitest';
// Imported through the services barrel: it is the documented entry point for
// the layer (Issue #870) and the import keeps it exercised.
import { BaseService, errorMessage, ServiceRegistry } from '../../src/services/index';
import { makeLogger } from '../helpers/logger';
import type { Logger } from '../../src/lib/logger';

class TestService extends BaseService {
  readonly calls: string[] = [];
  failOnStart = false;
  failOnStop = false;

  constructor(logger: Logger) {
    super('test-service', { logger });
  }

  protected onStart(): void {
    this.calls.push('start');
    if (this.failOnStart) {
      throw new Error('boom-start');
    }
  }

  protected onStop(): void {
    this.calls.push('stop');
    if (this.failOnStop) {
      throw new Error('boom-stop');
    }
  }

  runOperation(operation: string, fn: () => unknown): Promise<unknown> {
    return this.runSafely(operation, fn, 'fallback');
  }
}

describe('BaseService lifecycle', () => {
  it('moves idle → running → stopped and logs each transition', async () => {
    const { logger, entries } = makeLogger();
    const svc = new TestService(logger);

    expect(svc.state).toBe('idle');
    await svc.start();

    expect(svc.state).toBe('running');
    expect(svc.calls).toEqual(['start']);
    expect(entries).toContainEqual({
      level: 'info',
      msg: 'service_started',
      fields: { component: 'services/test-service' },
    });

    await svc.stop();
    expect(svc.state).toBe('stopped');
    expect(svc.calls).toEqual(['start', 'stop']);
    expect(entries.map((e) => e.msg)).toContain('service_stopped');
  });

  it('ignores a second start while running', async () => {
    const { logger, entries } = makeLogger();
    const svc = new TestService(logger);

    await svc.start();
    await svc.start();

    expect(svc.calls).toEqual(['start']);
    expect(entries.filter((e) => e.msg === 'service_start_ignored')).toHaveLength(1);
    expect(entries.some((e) => e.msg === 'service_start_ignored' && e.level === 'warn')).toBe(true);
  });

  it('can be restarted after a stop', async () => {
    const { logger } = makeLogger();
    const svc = new TestService(logger);

    await svc.start();
    await svc.stop();
    await svc.start();

    expect(svc.state).toBe('running');
    expect(svc.calls).toEqual(['start', 'stop', 'start']);
  });

  it('ignores stop before start', async () => {
    const { logger, entries } = makeLogger();
    const svc = new TestService(logger);

    await svc.stop();

    expect(svc.state).toBe('idle');
    expect(svc.calls).toEqual([]);
    expect(entries.some((e) => e.msg === 'service_stop_ignored')).toBe(true);
  });

  it('ignores repeated stops', async () => {
    const { logger } = makeLogger();
    const svc = new TestService(logger);

    await svc.start();
    await svc.stop();
    await svc.stop();

    expect(svc.calls).toEqual(['start', 'stop']);
    expect(svc.state).toBe('stopped');
  });

  it('fails to `failed` and rethrows when onStart throws', async () => {
    const { logger, entries } = makeLogger();
    const svc = new TestService(logger);
    svc.failOnStart = true;

    await expect(svc.start()).rejects.toThrow('boom-start');
    expect(svc.state).toBe('failed');
    expect(entries).toContainEqual(
      expect.objectContaining({ level: 'error', msg: 'service_start_failed' }),
    );
  });

  it('fails to `failed` and rethrows when onStop throws', async () => {
    const { logger, entries } = makeLogger();
    const svc = new TestService(logger);

    await svc.start();
    svc.failOnStop = true;

    await expect(svc.stop()).rejects.toThrow('boom-stop');
    expect(svc.state).toBe('failed');
    expect(entries).toContainEqual(
      expect.objectContaining({ level: 'error', msg: 'service_stop_failed' }),
    );
  });
});

describe('BaseService.runSafely', () => {
  it('returns the value of successful work', async () => {
    const { logger } = makeLogger();
    const svc = new TestService(logger);

    await expect(svc.runOperation('ok', () => 42)).resolves.toBe(42);
  });

  it('logs the failing operation and returns the fallback', async () => {
    const { logger, entries } = makeLogger();
    const svc = new TestService(logger);

    await expect(
      svc.runOperation('flush', () => {
        throw new Error('downstream down');
      }),
    ).resolves.toBe('fallback');

    expect(entries).toContainEqual(
      expect.objectContaining({
        level: 'error',
        msg: 'service_operation_failed',
        fields: expect.objectContaining({
          operation: 'flush',
          error: 'downstream down',
        }),
      }),
    );
  });

  it('reports async failures the same way', async () => {
    const { logger, entries } = makeLogger();
    const svc = new TestService(logger);

    await expect(
      svc.runOperation('sweep', async () => {
        throw new Error('async boom');
      }),
    ).resolves.toBe('fallback');
    expect(entries.some((e) => e.msg === 'service_operation_failed')).toBe(true);
  });
});

describe('errorMessage', () => {
  it('uses the message of an Error', () => {
    expect(errorMessage(new Error('kaboom'))).toBe('kaboom');
  });

  it('stringifies anything else', () => {
    expect(errorMessage('plain string')).toBe('plain string');
    expect(errorMessage(404)).toBe('404');
  });
});

describe('ServiceRegistry', () => {
  function tracked(
    order: string[],
    name: string,
    opts: { failStart?: boolean; failStop?: boolean } = {},
  ) {
    return {
      name,
      async start(): Promise<void> {
        order.push(`start:${name}`);
        if (opts.failStart) {
          throw new Error(`${name} start failed`);
        }
      },
      async stop(): Promise<void> {
        order.push(`stop:${name}`);
        if (opts.failStop) {
          throw new Error(`${name} stop failed`);
        }
      },
    };
  }

  it('starts in registration order and stops in reverse', async () => {
    const order: string[] = [];
    const registry = new ServiceRegistry()
      .register(tracked(order, 'analytics'))
      .register(tracked(order, 'health'))
      .register(tracked(order, 'digest'));

    expect(registry.list().map((s) => s.name)).toEqual(['analytics', 'health', 'digest']);

    await registry.startAll();
    await registry.stopAll();

    expect(order).toEqual([
      'start:analytics',
      'start:health',
      'start:digest',
      'stop:digest',
      'stop:health',
      'stop:analytics',
    ]);
  });

  it('rolls back already-started services when one fails to start', async () => {
    const order: string[] = [];
    const registry = new ServiceRegistry()
      .register(tracked(order, 'analytics'))
      .register(tracked(order, 'health', { failStart: true }));

    await expect(registry.startAll()).rejects.toThrow('health start failed');
    expect(order).toEqual(['start:analytics', 'start:health', 'stop:analytics']);
  });

  it('stops everything even when a stop throws, and reports the first failure', async () => {
    const order: string[] = [];
    const registry = new ServiceRegistry()
      .register(tracked(order, 'analytics', { failStop: true }))
      .register(tracked(order, 'digest'));

    await expect(registry.stopAll()).rejects.toThrow(
      'service_stop_failed: analytics stop failed',
    );
    expect(order).toEqual(['stop:digest', 'stop:analytics']);
  });
});
