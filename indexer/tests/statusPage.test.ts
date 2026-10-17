import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { PublicHealth } from '../src/services/publicHealthService.js';
import {
  HISTORY_RETENTION_DAYS,
  appendSample,
  computeUptime,
  parseHistory,
  type HistorySample,
} from '../src/statusPage/history.js';
import { escapeHtml, renderStatusPage } from '../src/statusPage/render.js';
import { assertPublicHealth, generateStatusSite } from '../scripts/generate-status-page.js';

const NOW = Date.parse('2027-01-15T12:00:00Z');
const DAY = 86_400_000;

function health(over: Partial<PublicHealth> = {}): PublicHealth {
  return {
    schemaVersion: 1,
    generatedAt: new Date(NOW).toISOString(),
    overall: 'operational',
    reasons: [],
    protocol: { paused: false, stale: false },
    solvency: {
      level: 'healthy',
      fundedInvoices: 120,
      settledInvoices: 110,
      defaultedInvoices: 2,
      defaultRatePct: 1.8,
      outstandingFundedInvoices: 8,
      overdueFundedInvoices: 0,
      overdueSharePct: 0,
      insurancePool: { enrolledLps: 12, claimsSharePct: 4.2 },
    },
    oracle: { circuitsTripped: 0, healthy: true },
    ...over,
  };
}

const sample = (daysAgo: number, over: Partial<HistorySample> = {}): HistorySample => ({
  t: new Date(NOW - daysAgo * DAY).toISOString(),
  overall: 'operational',
  paused: false,
  oracleHealthy: true,
  ...over,
});

describe('history', () => {
  it('parseHistory drops malformed entries and tolerates non-arrays', () => {
    expect(parseHistory(null)).toEqual([]);
    expect(parseHistory({})).toEqual([]);
    const parsed = parseHistory([
      sample(1),
      { t: 'not a date', overall: 'operational' },
      { t: sample(2).t, overall: 'bogus' },
      'x',
      { t: sample(3).t, overall: 'unknown', paused: 'yes', oracleHealthy: null },
    ]);
    expect(parsed).toHaveLength(2);
    expect(parsed[1]).toMatchObject({ overall: 'unknown', paused: null, oracleHealthy: null });
  });

  it('appendSample adds the new sample, sorts, prunes old and dedupes the same instant', () => {
    const old = sample(HISTORY_RETENTION_DAYS + 1);
    const recent = sample(2);
    const dup = sample(0);
    const next = appendSample([recent, old, dup], health(), NOW);
    expect(next.map((s) => s.t)).toEqual([recent.t, dup.t]);
    expect(next).toHaveLength(2);
  });

  it('appendSample records only booleans and the overall label', () => {
    const [s] = appendSample([], health({ overall: 'degraded', oracle: { circuitsTripped: 1, healthy: false } }), NOW);
    expect(Object.keys(s).sort()).toEqual(['oracleHealthy', 'overall', 'paused', 't']);
    expect(s.oracleHealthy).toBe(false);
  });

  it('computeUptime counts only known samples inside the window', () => {
    const history = [
      sample(40, { oracleHealthy: false }), // outside window
      sample(10),
      sample(9, { oracleHealthy: false }),
      sample(8, { oracleHealthy: null, paused: null }), // unknown: excluded
      sample(7, { paused: true }),
      sample(1),
    ];
    const u = computeUptime(history, 30, NOW);
    expect(u.oracleSamples).toBe(4);
    expect(u.oraclePct).toBe(75);
    expect(u.protocolSamples).toBe(4);
    expect(u.protocolPct).toBe(75);
  });

  it('computeUptime returns null percentages with no data', () => {
    const u = computeUptime([], 30, NOW);
    expect(u).toMatchObject({ oraclePct: null, protocolPct: null, oracleSamples: 0 });
  });
});

describe('renderStatusPage', () => {
  const uptime = computeUptime([sample(1), sample(2)], 30, NOW);

  it('renders overall status, metrics and last-updated time', () => {
    const html = renderStatusPage(health(), uptime);
    expect(html).toContain('All systems operational');
    expect(html).toContain('1.8%');
    expect(html).toContain('100%');
    expect(html).toContain('2027-01-15T12:00:00.000Z');
    expect(html).toContain('href="status.json"');
  });

  it.each([
    ['degraded', 'Degraded performance'],
    ['paused', 'Protocol paused'],
    ['unknown', 'Status unavailable'],
  ] as const)('renders %s', (overall, label) => {
    expect(renderStatusPage(health({ overall }), uptime)).toContain(label);
  });

  it('shows n/a rather than fabricating numbers when data is missing', () => {
    const html = renderStatusPage(
      health({
        solvency: { ...health().solvency, defaultRatePct: null, overdueSharePct: null, insurancePool: null, level: 'unknown' },
      }),
      computeUptime([], 30, NOW),
    );
    expect(html).toContain('n/a');
    expect(html).toContain('Not enough data yet');
  });

  it('escapes interpolated content', () => {
    const evil = '<script>alert(1)</script>';
    const html = renderStatusPage(health({ reasons: [evil] }), uptime, { title: evil, docsUrl: '"><img src=x>' });
    expect(html).not.toContain(evil);
    expect(html).not.toContain('"><img');
    expect(html).toContain('&lt;script&gt;');
  });

  it('is self-contained: no external scripts, styles or images', () => {
    const html = renderStatusPage(health(), uptime);
    expect(html).not.toMatch(/<script[^>]+src=/i);
    expect(html).not.toMatch(/<link[^>]+href=/i);
    expect(html).not.toMatch(/<img/i);
    expect(html).not.toMatch(/https?:\/\/(?!$)/);
  });

  it('escapeHtml handles all five characters', () => {
    expect(escapeHtml(`<&>"'`)).toBe('&lt;&amp;&gt;&quot;&#39;');
  });
});

describe('generateStatusSite', () => {
  let dir: string;
  beforeEach(async () => {
    dir = await mkdtemp(join(tmpdir(), 'status-'));
  });
  afterEach(async () => {
    await rm(dir, { recursive: true, force: true });
  });

  const jsonResponse = (body: unknown, status = 200) =>
    new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });

  it('writes index.html, status.json and history.json and grows history across runs', async () => {
    const urls: string[] = [];
    const fetchImpl = (async (url: string) => {
      urls.push(url);
      if (url.endsWith('/public/health')) return jsonResponse(health());
      return jsonResponse([sample(1)]);
    }) as unknown as typeof fetch;

    const result = await generateStatusSite({
      indexerUrl: 'https://indexer.example/',
      historyUrl: 'https://status.example/history.json',
      outDir: dir,
      fetchImpl,
      now: () => NOW,
    });

    expect(urls).toEqual(['https://indexer.example/public/health', 'https://status.example/history.json']);
    expect(result.files.sort()).toEqual(['history.json', 'index.html', 'status.json']);
    const history = JSON.parse(await readFile(join(dir, 'history.json'), 'utf8'));
    expect(history).toHaveLength(2);
    const status = JSON.parse(await readFile(join(dir, 'status.json'), 'utf8'));
    expect(status.overall).toBe('operational');
    expect(status.uptime.oraclePct).toBe(100);
    expect(await readFile(join(dir, 'index.html'), 'utf8')).toContain('All systems operational');
  });

  it('starts a fresh history when the previous one cannot be fetched', async () => {
    const fetchImpl = (async (url: string) =>
      url.endsWith('/public/health') ? jsonResponse(health()) : jsonResponse({}, 404)) as unknown as typeof fetch;
    await generateStatusSite({
      indexerUrl: 'https://indexer.example',
      historyUrl: 'https://status.example/history.json',
      outDir: dir,
      fetchImpl,
      now: () => NOW,
    });
    expect(JSON.parse(await readFile(join(dir, 'history.json'), 'utf8'))).toHaveLength(1);
  });

  it('fails and writes nothing when the indexer is unreachable', async () => {
    const fetchImpl = (async () => jsonResponse({}, 503)) as unknown as typeof fetch;
    await expect(
      generateStatusSite({ indexerUrl: 'https://indexer.example', outDir: join(dir, 'out'), fetchImpl, now: () => NOW }),
    ).rejects.toThrow(/HTTP 503/);
    await expect(readFile(join(dir, 'out', 'index.html'), 'utf8')).rejects.toThrow();
  });

  it('rejects an unexpected response shape', async () => {
    expect(() => assertPublicHealth({ overall: 'operational' })).toThrow(/shape/);
    expect(() => assertPublicHealth(null)).toThrow(/shape/);
    expect(assertPublicHealth(health()).overall).toBe('operational');
  });
});
