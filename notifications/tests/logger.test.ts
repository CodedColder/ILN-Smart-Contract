import { afterEach, describe, expect, it, vi } from 'vitest';

type LoggerModule = typeof import('../src/lib/logger');

/**
 * The logger reads `LOG_LEVEL` at import time, so each case loads a fresh
 * module instance with the level it needs (there is no static import here on
 * purpose).
 */
async function loadLogger(level: string): Promise<LoggerModule> {
  vi.resetModules();
  vi.stubEnv('LOG_LEVEL', level);
  const mod: LoggerModule = await import('../src/lib/logger');
  return mod;
}

function capture(fn: () => void): { out: string[]; err: string[] } {
  const out: string[] = [];
  const err: string[] = [];
  const outSpy = vi.spyOn(process.stdout, 'write');
  const errSpy = vi.spyOn(process.stderr, 'write');
  try {
    fn();
    for (const call of outSpy.mock.calls) out.push(String(call[0]));
    for (const call of errSpy.mock.calls) err.push(String(call[0]));
  } finally {
    outSpy.mockRestore();
    errSpy.mockRestore();
  }
  return { out, err };
}

function parse(lines: string[]): Array<Record<string, any>> {
  return lines.map((line) => JSON.parse(line));
}

afterEach(() => {
  vi.unstubAllEnvs();
  vi.resetModules();
});

describe('structured logger', () => {
  it('writes one JSON object per emitted level', async () => {
    const { logger } = await loadLogger('debug');

    const { out, err } = capture(() => {
      logger.debug('debug line');
      logger.info('info line', { endpointId: 'ep_1' });
      logger.warn('warn line');
      logger.error('error line', { attempts: 3 });
    });

    expect(out).toHaveLength(2);
    expect(err).toHaveLength(2);

    const lines = parse([...out, ...err]);
    expect(lines.map((l) => l.msg)).toEqual([
      'debug line',
      'info line',
      'warn line',
      'error line',
    ]);
    expect(lines.map((l) => l.level)).toEqual(['debug', 'info', 'warn', 'error']);
    for (const line of lines) {
      expect(typeof line.ts).toBe('string');
      expect(line.service).toBe('notifications');
      expect(new Date(line.ts).toISOString()).toBe(line.ts);
    }
    expect(lines[1]).toMatchObject({ endpointId: 'ep_1' });
    expect(lines[3]).toMatchObject({ attempts: 3 });
    expect(lines[3].correlationId).toBeUndefined();
  });

  it('suppresses everything below the configured threshold', async () => {
    const { logger } = await loadLogger('error');

    const { out, err } = capture(() => {
      logger.debug('dropped');
      logger.info('dropped');
      logger.warn('dropped');
      logger.error('kept');
    });

    expect(out).toHaveLength(0);
    expect(err).toHaveLength(1);
    expect(parse(err)[0]).toMatchObject({ level: 'error', msg: 'kept' });
  });

  it('falls back to the info threshold for an unknown level', async () => {
    const { logger } = await loadLogger('chatty');

    const { out, err } = capture(() => {
      logger.debug('dropped');
      logger.info('kept');
      logger.error('kept');
    });

    expect(out).toHaveLength(1);
    expect(err).toHaveLength(1);
  });

  it('carries the correlation context and child bindings into every line', async () => {
    const { logger, runWithContext, bindContext, getCorrelationId, newCorrelationId } =
      await loadLogger('debug');

    const id = newCorrelationId();
    expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    expect(getCorrelationId()).toBeUndefined();
    expect(runWithContext({ correlationId: id }, () => getCorrelationId())).toBe(id);

    const child = logger.child({ component: 'services/digest' });
    const { err: inScope } = capture(() => {
      runWithContext({ correlationId: id, ledger: 42 }, () => {
        bindContext({ method: 'POST' });
        child.error('inside scope');
      });
    });
    expect(parse(inScope)[0]).toMatchObject({
      correlationId: id,
      ledger: 42,
      method: 'POST',
      component: 'services/digest',
      msg: 'inside scope',
    });

    const { err: outside } = capture(() => logger.error('outside scope'));
    expect(parse(outside)[0].correlationId).toBeUndefined();
  });

  it('ignores bindContext outside any context', async () => {
    const { bindContext, getCorrelationId } = await loadLogger('info');

    expect(() => bindContext({ ignored: true })).not.toThrow();
    expect(getCorrelationId()).toBeUndefined();
  });
});
