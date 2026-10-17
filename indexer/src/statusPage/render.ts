/**
 * Static HTML rendering for the public status page (Issue #892).
 *
 * Pure functions, no I/O, no external resources: the output is one
 * self-contained file that can be hosted anywhere static (GitHub Pages, S3,
 * IPFS). Every interpolated value is escaped — the input is our own API, but
 * the page is public and must not become an injection surface if that API
 * ever changes.
 */

import type { OverallStatus, PublicHealth, SolvencyLevel } from '../services/publicHealthService.js';
import type { Uptime } from './history.js';

export interface RenderOptions {
  title?: string;
  /** Link shown in the footer to the public docs. */
  docsUrl?: string | undefined;
  /** Page is considered stale after this many minutes without regeneration. */
  staleAfterMinutes?: number;
}

export function escapeHtml(value: unknown): string {
  return String(value)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

const OVERALL_LABEL: Record<OverallStatus, string> = {
  operational: 'All systems operational',
  degraded: 'Degraded performance',
  paused: 'Protocol paused',
  unknown: 'Status unavailable',
};

const SOLVENCY_LABEL: Record<SolvencyLevel, string> = {
  healthy: 'Healthy',
  watch: 'Watch',
  stressed: 'Stressed',
  unknown: 'Not enough data yet',
};

const pct = (v: number | null): string => (v === null ? 'n/a' : `${v}%`);

function metric(label: string, value: string, note?: string): string {
  return `<div class="metric"><dt>${escapeHtml(label)}</dt><dd>${escapeHtml(value)}</dd>${
    note ? `<p>${escapeHtml(note)}</p>` : ''
  }</div>`;
}

export function renderStatusPage(
  health: PublicHealth,
  uptime: Uptime,
  options: RenderOptions = {},
): string {
  const title = options.title ?? 'ILN Protocol Status';
  const staleAfter = options.staleAfterMinutes ?? 120;
  const s = health.solvency;
  const pool = s.insurancePool;

  const reasons = health.reasons.length
    ? `<ul class="reasons">${health.reasons.map((r) => `<li>${escapeHtml(r)}</li>`).join('')}</ul>`
    : '';

  const docs = options.docsUrl
    ? ` · <a href="${escapeHtml(options.docsUrl)}">About this page</a>`
    : '';

  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${escapeHtml(title)}</title>
<meta name="description" content="Public, non-sensitive health summary of the Invoice Liquidity Network.">
<style>
:root{--bg:#fff;--fg:#14171a;--muted:#5b6670;--card:#f5f7f9;--line:#dfe4e8;--ok:#0a7d3c;--warn:#a15c00;--bad:#b3261e;--unk:#5b6670}
@media (prefers-color-scheme:dark){:root{--bg:#0f1316;--fg:#e8ecef;--muted:#9aa5ae;--card:#171d22;--line:#29323a;--ok:#4cc17f;--warn:#e0a03a;--bad:#f2867e;--unk:#9aa5ae}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.5 system-ui,-apple-system,Segoe UI,Roboto,sans-serif}
main{max-width:760px;margin:0 auto;padding:24px 16px 48px}
h1{font-size:1.4rem;margin:0 0 16px}
h2{font-size:1.05rem;margin:32px 0 8px}
.banner{border:1px solid var(--line);border-left-width:6px;border-radius:8px;background:var(--card);padding:16px}
.banner strong{font-size:1.2rem}
.banner.operational{border-left-color:var(--ok)}.banner.degraded{border-left-color:var(--warn)}
.banner.paused{border-left-color:var(--bad)}.banner.unknown{border-left-color:var(--unk)}
.reasons{margin:8px 0 0;padding-left:20px;color:var(--muted)}
dl{display:grid;grid-template-columns:repeat(auto-fit,minmax(200px,1fr));gap:12px;margin:0}
.metric{border:1px solid var(--line);border-radius:8px;background:var(--card);padding:12px}
dt{font-size:.85rem;color:var(--muted)}dd{margin:2px 0 0;font-size:1.3rem;font-weight:600}
.metric p{margin:4px 0 0;font-size:.8rem;color:var(--muted)}
.stale{display:none;margin:0 0 16px;padding:12px;border:1px solid var(--warn);border-radius:8px;color:var(--warn)}
footer{margin-top:32px;font-size:.85rem;color:var(--muted)}
a{color:inherit}
</style>
</head>
<body>
<main>
<h1>${escapeHtml(title)}</h1>
<p id="stale" class="stale" role="status">This page has not been refreshed recently; the data below may be out of date.</p>
<section class="banner ${escapeHtml(health.overall)}" aria-live="polite">
<strong>${escapeHtml(OVERALL_LABEL[health.overall])}</strong>
${reasons}
</section>

<h2>Solvency &amp; credit health</h2>
<dl>
${metric('Overall credit health', SOLVENCY_LABEL[s.level])}
${metric('Default rate', pct(s.defaultRatePct), `${s.defaultedInvoices} defaulted of ${s.settledInvoices + s.defaultedInvoices} resolved`)}
${metric('Overdue share', pct(s.overdueSharePct), `${s.overdueFundedInvoices} of ${s.outstandingFundedInvoices} outstanding funded invoices`)}
${metric('Insurance pool payouts', pool ? pct(pool.claimsSharePct) : 'n/a', pool ? `${pool.enrolledLps} enrolled LPs; share of pool paid out in claims` : 'No insurance pool data')}
</dl>

<h2>Oracle &amp; availability (last ${escapeHtml(uptime.windowDays)} days)</h2>
<dl>
${metric('Oracle uptime', pct(uptime.oraclePct), `${uptime.oracleSamples} samples`)}
${metric('Protocol availability', pct(uptime.protocolPct), `${uptime.protocolSamples} samples`)}
${metric('Oracle circuit breakers tripped', health.oracle.circuitsTripped === null ? 'n/a' : String(health.oracle.circuitsTripped), 'Right now')}
</dl>

<footer>
<p>Last updated <time id="updated" datetime="${escapeHtml(health.generatedAt)}">${escapeHtml(health.generatedAt)}</time>. Refreshed on a schedule; this page is generated from indexed on-chain data and is not real-time.${docs}</p>
<p>Machine-readable: <a href="status.json">status.json</a></p>
</footer>
</main>
<script>
(function(){try{
var t=Date.parse(document.getElementById('updated').getAttribute('datetime'));
var ageMin=(Date.now()-t)/60000;
if(ageMin>${Number(staleAfter)}){document.getElementById('stale').style.display='block';}
}catch(e){}})();
</script>
</body>
</html>
`;
}
