#!/usr/bin/env node
/**
 * generate-status-page.ts — builds the static public protocol-health page
 * (Issue #892).
 *
 *   1. GET  <STATUS_INDEXER_URL>/public/health     (curated, non-sensitive JSON)
 *   2. GET  <STATUS_HISTORY_URL>                   (previous history.json; optional)
 *   3. append a sample, compute 30-day uptime
 *   4. write <STATUS_OUT_DIR>/{index.html,status.json,history.json}
 *
 * If the indexer cannot be reached or returns something unexpected the script
 * exits non-zero and writes nothing, so a scheduled job leaves the previously
 * published page in place (its "last updated" time and staleness banner tell
 * readers it is old) instead of publishing a misleading empty page.
 *
 * Environment:
 *   STATUS_INDEXER_URL   Indexer base URL (required)
 *   STATUS_HISTORY_URL   URL of the previously published history.json (optional)
 *   STATUS_OUT_DIR       Output directory (default: status-site)
 *   STATUS_DOCS_URL      Link shown in the footer (optional)
 */

import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import type { PublicHealth } from '../src/services/publicHealthService.js';
import { appendSample, computeUptime, parseHistory, type Uptime } from '../src/statusPage/history.js';
import { renderStatusPage } from '../src/statusPage/render.js';

export const UPTIME_WINDOW_DAYS = 30;

export interface GenerateOptions {
  indexerUrl: string;
  historyUrl?: string | undefined;
  outDir: string;
  docsUrl?: string | undefined;
  fetchImpl?: typeof fetch;
  now?: () => number;
}

export interface GenerateResult {
  health: PublicHealth;
  uptime: Uptime;
  files: string[];
}

const TIMEOUT_MS = 10_000;

async function getJson(fetchImpl: typeof fetch, url: string): Promise<unknown> {
  const res = await fetchImpl(url, { signal: AbortSignal.timeout(TIMEOUT_MS) });
  if (!res.ok) throw new Error(`GET ${url} -> HTTP ${res.status}`);
  return res.json();
}

export function assertPublicHealth(value: unknown): PublicHealth {
  const v = value as Partial<PublicHealth> | null;
  if (
    !v ||
    v.schemaVersion !== 1 ||
    typeof v.generatedAt !== 'string' ||
    typeof v.overall !== 'string' ||
    !v.protocol ||
    !v.solvency ||
    !v.oracle
  ) {
    throw new Error('Unexpected /public/health response shape');
  }
  return v as PublicHealth;
}

export async function generateStatusSite(options: GenerateOptions): Promise<GenerateResult> {
  const fetchImpl = options.fetchImpl ?? fetch;
  const nowMs = (options.now ?? Date.now)();
  const base = options.indexerUrl.replace(/\/+$/, '');

  const health = assertPublicHealth(await getJson(fetchImpl, `${base}/public/health`));

  let previous: unknown = [];
  if (options.historyUrl) {
    try {
      previous = await getJson(fetchImpl, options.historyUrl);
    } catch {
      // First run, or the previous page is unreachable: start a fresh history.
      previous = [];
    }
  }

  const history = appendSample(parseHistory(previous), health, nowMs);
  const uptime = computeUptime(history, UPTIME_WINDOW_DAYS, nowMs);

  const html = renderStatusPage(health, uptime, { docsUrl: options.docsUrl });
  const statusJson = JSON.stringify({ ...health, uptime }, null, 2);

  await mkdir(options.outDir, { recursive: true });
  const files = {
    'index.html': html,
    'status.json': statusJson,
    'history.json': JSON.stringify(history),
  };
  for (const [name, content] of Object.entries(files)) {
    await writeFile(join(options.outDir, name), content, 'utf8');
  }

  return { health, uptime, files: Object.keys(files) };
}

async function main(): Promise<void> {
  const indexerUrl = process.env.STATUS_INDEXER_URL;
  if (!indexerUrl) {
    console.error('STATUS_INDEXER_URL is required');
    process.exit(2);
  }
  const result = await generateStatusSite({
    indexerUrl,
    historyUrl: process.env.STATUS_HISTORY_URL || undefined,
    outDir: resolve(process.env.STATUS_OUT_DIR ?? 'status-site'),
    docsUrl: process.env.STATUS_DOCS_URL || undefined,
  });
  console.log(`status page generated: ${result.health.overall} (${result.files.join(', ')})`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((err) => {
    console.error(err instanceof Error ? err.message : err);
    process.exit(1);
  });
}
