import { describe, expect, it, vi } from 'vitest';
import request from 'supertest';
import { createTestApp, createTestDb } from './helpers.js';

const publicPaths = [
  { name: 'leaderboard', method: 'get' as const, path: '/leaderboard' },
  { name: 'stats', method: 'get' as const, path: '/stats' },
  { name: 'invoices', method: 'get' as const, path: '/invoices' },
  {
    name: 'graphql',
    method: 'post' as const,
    path: '/graphql',
    body: { query: '{ __typename }' },
  },
];

describe('indexer rate-limit coverage', () => {
  for (const route of publicPaths) {
    it(`rate-limits anonymous traffic on ${route.name} under load`, async () => {
      const db = createTestDb();
      const app = createTestApp(db, {
        rateLimitAnonymousMax: 5,
        rateLimitApiKeyMax: 50,
        rateLimitWindowMs: 60_000,
        health: {
          horizonUrl: '',
          getChainTipLedger: async () => null,
        },
      });

      for (let i = 0; i < 5; i += 1) {
        const res =
          route.method === 'post'
            ? await request(app).post(route.path).send(route.body)
            : await request(app).get(route.path);
        expect(res.status).not.toBe(429);
      }

      const limited =
        route.method === 'post'
          ? await request(app).post(route.path).send(route.body)
          : await request(app).get(route.path);

      expect(limited.status).toBe(429);
      expect(limited.headers['retry-after']).toBeDefined();
      expect(limited.body.error).toContain('Too many requests');
    });
  }

  it('does not rate-limit /health after public routes are exhausted', async () => {
    const db = createTestDb();
    const app = createTestApp(db, {
      rateLimitAnonymousMax: 2,
      rateLimitApiKeyMax: 50,
      rateLimitWindowMs: 60_000,
      health: {
        horizonUrl: '',
        getChainTipLedger: async () => 1,
      },
    });

    expect((await request(app).get('/leaderboard')).status).toBe(200);
    expect((await request(app).get('/leaderboard')).status).toBe(200);
    expect((await request(app).get('/leaderboard')).status).toBe(429);

    const health = await request(app).get('/health');
    expect([200, 503]).toContain(health.status);
    expect(health.status).not.toBe(429);
  });

  it('gives API-key traffic a higher limit without unlimited bypass', async () => {
    const db = createTestDb();
    const app = createTestApp(db, {
      apiKeys: ['prod-client-key'],
      rateLimitAnonymousMax: 3,
      rateLimitApiKeyMax: 8,
      rateLimitWindowMs: 60_000,
      health: {
        horizonUrl: '',
        getChainTipLedger: async () => null,
      },
    });

    for (let i = 0; i < 8; i += 1) {
      const res = await request(app)
        .get('/leaderboard')
        .set('X-API-Key', 'prod-client-key');
      expect(res.status).toBe(200);
    }

    const limited = await request(app)
      .get('/leaderboard')
      .set('X-API-Key', 'prod-client-key');
    expect(limited.status).toBe(429);

    const anonApp = createTestApp(createTestDb(), {
      apiKeys: ['prod-client-key'],
      rateLimitAnonymousMax: 3,
      rateLimitApiKeyMax: 8,
      rateLimitWindowMs: 60_000,
      health: { horizonUrl: '', getChainTipLedger: async () => null },
    });
    for (let i = 0; i < 3; i += 1) {
      expect((await request(anonApp).get('/stats')).status).toBe(200);
    }
    expect((await request(anonApp).get('/stats')).status).toBe(429);
  });

  it('ignores spoofed X-Forwarded-For when no proxy is trusted', async () => {
    // express-rate-limit logs this exact misconfiguration; the assertion below
    // proves the header is ignored rather than trusted.
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const app = createTestApp(createTestDb(), {
      rateLimitAnonymousMax: 2,
      rateLimitApiKeyMax: 50,
      rateLimitWindowMs: 60_000,
      health: { horizonUrl: '', getChainTipLedger: async () => null },
    });

    expect(
      (await request(app).get('/stats').set('X-Forwarded-For', '198.51.100.1'))
        .status
    ).toBe(200);
    expect(
      (await request(app).get('/stats').set('X-Forwarded-For', '198.51.100.2'))
        .status
    ).toBe(200);

    const spoofed = await request(app)
      .get('/stats')
      .set('X-Forwarded-For', '198.51.100.3');
    expect(spoofed.status).toBe(429);
    consoleError.mockRestore();
  });

  it('separates forwarded client IPs when a trusted proxy is configured', async () => {
    const app = createTestApp(createTestDb(), {
      trustProxy: 1,
      rateLimitAnonymousMax: 2,
      rateLimitApiKeyMax: 50,
      rateLimitWindowMs: 60_000,
      health: { horizonUrl: '', getChainTipLedger: async () => null },
    });

    for (let i = 0; i < 2; i += 1) {
      expect(
        (await request(app).get('/stats').set('X-Forwarded-For', '203.0.113.10'))
          .status
      ).toBe(200);
    }
    expect(
      (await request(app).get('/stats').set('X-Forwarded-For', '203.0.113.10'))
        .status
    ).toBe(429);

    // A different client behind the same proxy gets its own bucket.
    expect(
      (await request(app).get('/stats').set('X-Forwarded-For', '203.0.113.11'))
        .status
    ).toBe(200);
  });

  it('normalises IPv6 address variants so they cannot reset the limit', async () => {
    const app = createTestApp(createTestDb(), {
      trustProxy: 1,
      rateLimitAnonymousMax: 1,
      rateLimitApiKeyMax: 50,
      rateLimitWindowMs: 60_000,
      health: { horizonUrl: '', getChainTipLedger: async () => null },
    });

    expect(
      (
        await request(app)
          .get('/stats')
          .set('X-Forwarded-For', '2001:db8::1')
      ).status
    ).toBe(200);

    // Same address in expanded form must share the bucket.
    const expanded = await request(app)
      .get('/stats')
      .set('X-Forwarded-For', '2001:0db8:0000:0000:0000:0000:0000:0001');
    expect(expanded.status).toBe(429);

    // A different /56 subnet is a different client.
    expect(
      (
        await request(app)
          .get('/stats')
          .set('X-Forwarded-For', '2001:db8:1::1')
      ).status
    ).toBe(200);
  });

  it('keeps counters per process — replicas need a shared store', async () => {
    const options = {
      rateLimitAnonymousMax: 2,
      rateLimitApiKeyMax: 50,
      rateLimitWindowMs: 60_000,
      health: { horizonUrl: '', getChainTipLedger: async () => null },
    };
    const replicaA = createTestApp(createTestDb(), options);
    const replicaB = createTestApp(createTestDb(), options);

    for (let i = 0; i < 2; i += 1) {
      expect((await request(replicaA).get('/stats')).status).toBe(200);
    }
    expect((await request(replicaA).get('/stats')).status).toBe(429);

    // In-memory counters are not shared between replicas; this is a documented
    // residual risk that needs an external store for fleet-wide enforcement.
    for (let i = 0; i < 2; i += 1) {
      expect((await request(replicaB).get('/stats')).status).toBe(200);
    }
    expect((await request(replicaB).get('/stats')).status).toBe(429);
  });

  it('limits each API key independently and leaves the anonymous bucket intact', async () => {
    const app = createTestApp(createTestDb(), {
      apiKeys: ['key-a', 'key-b'],
      rateLimitAnonymousMax: 2,
      rateLimitApiKeyMax: 3,
      rateLimitWindowMs: 60_000,
      health: { horizonUrl: '', getChainTipLedger: async () => null },
    });

    for (let i = 0; i < 3; i += 1) {
      expect(
        (await request(app).get('/stats').set('X-API-Key', 'key-a')).status
      ).toBe(200);
    }
    expect(
      (await request(app).get('/stats').set('X-API-Key', 'key-a')).status
    ).toBe(429);

    // key-b is a separate bucket.
    expect(
      (await request(app).get('/stats').set('X-API-Key', 'key-b')).status
    ).toBe(200);

    // Anonymous traffic still has its own full allowance.
    expect((await request(app).get('/stats')).status).toBe(200);
    expect((await request(app).get('/stats')).status).toBe(200);
    expect((await request(app).get('/stats')).status).toBe(429);
  });
});
