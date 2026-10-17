import express from 'express';
import request from 'supertest';
import { describe, expect, it, vi } from 'vitest';
import { CORRELATION_HEADER, createRequestIdMiddleware } from '../src/lib/requestId';
import { getCorrelationId, logger } from '../src/lib/logger';

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

function makeApp() {
  const app = express();
  app.use(createRequestIdMiddleware());
  app.get('/probe', (_req, res) => {
    logger.error('probe_failed');
    res.json({ correlationId: getCorrelationId() ?? null });
  });
  return app;
}

describe('createRequestIdMiddleware', () => {
  it('mints a correlation id when the caller supplied none', async () => {
    const res = await request(makeApp()).get('/probe');

    expect(res.status).toBe(200);
    expect(res.headers[CORRELATION_HEADER]).toMatch(UUID);
    expect(res.body.correlationId).toBe(res.headers[CORRELATION_HEADER]);
  });

  it('echoes a safe inbound id end to end', async () => {
    const res = await request(makeApp())
      .get('/probe')
      .set(CORRELATION_HEADER, 'ledger-1234_evt.9');

    expect(res.headers[CORRELATION_HEADER]).toBe('ledger-1234_evt.9');
    expect(res.body.correlationId).toBe('ledger-1234_evt.9');
  });

  it('falls back to the x-request-id header', async () => {
    const res = await request(makeApp()).get('/probe').set('x-request-id', 'upstream-1');

    expect(res.headers[CORRELATION_HEADER]).toBe('upstream-1');
  });

  it('replaces ids that fail the safe-shape check', async () => {
    const res = await request(makeApp())
      .get('/probe')
      .set(CORRELATION_HEADER, 'not a safe id');

    expect(res.headers[CORRELATION_HEADER]).toMatch(UUID);
    expect(res.body.correlationId).not.toBe('not a safe id');
  });

  it('runs the handler inside the logging context', async () => {
    const errSpy = vi.spyOn(process.stderr, 'write');
    try {
      const res = await request(makeApp())
        .get('/probe')
        .set(CORRELATION_HEADER, 'trace-me');
      expect(res.body.correlationId).toBe('trace-me');

      const lines = errSpy.mock.calls
        .map((call) => String(call[0]))
        .filter((line) => line.includes('probe_failed'))
        .map((line) => JSON.parse(line));
      expect(lines).toHaveLength(1);
      expect(lines[0]).toMatchObject({ correlationId: 'trace-me', path: '/probe' });
    } finally {
      errSpy.mockRestore();
    }
  });
});
