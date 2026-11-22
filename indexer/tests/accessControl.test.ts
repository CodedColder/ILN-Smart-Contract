import { afterEach, describe, expect, it, vi } from 'vitest';
import request from 'supertest';
import { createTestApp, createTestDb } from './helpers.js';

describe('indexer access control', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('rejects an unknown API key', async () => {
    const db = createTestDb();
    const app = createTestApp(db, {
      apiKeys: ['known-client-key'],
      health: { horizonUrl: '', getChainTipLedger: async () => null },
    });

    const res = await request(app)
      .get('/leaderboard')
      .set('X-API-Key', 'wrong-key');

    expect(res.status).toBe(401);
    expect(res.body.error).toBe('Invalid API key');
  });

  it('accepts a known API key on public routes', async () => {
    const db = createTestDb();
    const app = createTestApp(db, {
      apiKeys: ['known-client-key'],
      rateLimitAnonymousMax: 2,
      rateLimitApiKeyMax: 20,
      health: { horizonUrl: '', getChainTipLedger: async () => null },
    });

    const res = await request(app)
      .get('/leaderboard')
      .set('X-API-Key', 'known-client-key');

    expect(res.status).toBe(200);
  });

  it('does not rate-limit /health so monitors stay reliable', async () => {
    const db = createTestDb();
    const app = createTestApp(db, {
      rateLimitAnonymousMax: 2,
      rateLimitApiKeyMax: 2,
      rateLimitWindowMs: 60_000,
      health: { horizonUrl: '', getChainTipLedger: async () => 100 },
    });

    for (let i = 0; i < 5; i += 1) {
      const res = await request(app).get('/health');
      expect(res.status).toBe(200);
      expect(res.body.status).toBe('ok');
    }
  });

  it('supports key rotation with old and new keys, then rejects the retired key', async () => {
    const db = createTestDb();
    const rotating = createTestApp(db, {
      apiKeys: ['old-key', 'new-key'],
      health: { horizonUrl: '', getChainTipLedger: async () => null },
    });

    expect(
      (await request(rotating).get('/stats').set('X-API-Key', 'old-key')).status
    ).toBe(200);
    expect(
      (await request(rotating).get('/stats').set('X-API-Key', 'new-key')).status
    ).toBe(200);

    const rotated = createTestApp(db, {
      apiKeys: ['new-key'],
      health: { horizonUrl: '', getChainTipLedger: async () => null },
    });

    expect(
      (await request(rotated).get('/stats').set('X-API-Key', 'old-key')).status
    ).toBe(401);
    expect(
      (await request(rotated).get('/stats').set('X-API-Key', 'new-key')).status
    ).toBe(200);
  });

  it('treats a missing key as anonymous and never as authenticated', async () => {
    const app = createTestApp(createTestDb(), {
      apiKeys: ['known-client-key'],
      rateLimitAnonymousMax: 2,
      rateLimitApiKeyMax: 50,
      rateLimitWindowMs: 60_000,
      health: { horizonUrl: '', getChainTipLedger: async () => null },
    });

    for (let i = 0; i < 2; i += 1) {
      expect((await request(app).get('/stats')).status).toBe(200);
    }
    expect((await request(app).get('/stats')).status).toBe(429);
  });

  it('stays public (fail-open by design) when no keys are configured', async () => {
    const app = createTestApp(createTestDb(), {
      apiKeys: [],
      health: { horizonUrl: '', getChainTipLedger: async () => null },
    });

    expect(
      (await request(app).get('/stats').set('X-API-Key', 'anything')).status
    ).toBe(200);
    expect((await request(app).get('/stats')).status).toBe(200);
  });

  it('requires every replica to share the same API_KEYS configuration', async () => {
    const options = {
      health: { horizonUrl: '', getChainTipLedger: async () => null },
    };
    const replicaA = createTestApp(createTestDb(), {
      ...options,
      apiKeys: ['shared-secret'],
    });
    const replicaB = createTestApp(createTestDb(), {
      ...options,
      apiKeys: ['shared-secret'],
    });
    const misconfigured = createTestApp(createTestDb(), {
      ...options,
      apiKeys: ['different-secret'],
    });

    expect(
      (await request(replicaA).get('/stats').set('X-API-Key', 'shared-secret'))
        .status
    ).toBe(200);
    expect(
      (await request(replicaB).get('/stats').set('X-API-Key', 'shared-secret'))
        .status
    ).toBe(200);

    // A replica with a divergent key set fails closed rather than accepting it.
    expect(
      (
        await request(misconfigured)
          .get('/stats')
          .set('X-API-Key', 'shared-secret')
      ).status
    ).toBe(401);
  });

  it('never grants the API-key tier to invalid keys', async () => {
    const app = createTestApp(createTestDb(), {
      apiKeys: ['known-client-key'],
      rateLimitAnonymousMax: 50,
      rateLimitApiKeyMax: 1,
      rateLimitWindowMs: 60_000,
      health: { horizonUrl: '', getChainTipLedger: async () => null },
    });

    for (const key of ['wrong-key', 'known-client-ke', 'KNOWN-CLIENT-KEY']) {
      const res = await request(app).get('/stats').set('X-API-Key', key);
      expect(res.status).toBe(401);
      expect(res.body.error).toBe('Invalid API key');
    }
  });
});
