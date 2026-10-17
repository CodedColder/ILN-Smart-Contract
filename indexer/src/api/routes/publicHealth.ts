import { Router } from 'express';
import type Database from 'better-sqlite3';
import type { ProtocolStatusService } from '../../services/protocolStatusService.js';
import { buildPublicHealth, type PublicHealthThresholds } from '../../services/publicHealthService.js';

export interface PublicHealthRouterOptions {
  insurancePoolContractId?: string;
  thresholds?: Partial<PublicHealthThresholds>;
}

/**
 * `GET /public/health` (Issue #892) — the curated, non-sensitive summary that
 * feeds the public status page. Always 200: a failed chain read is reported as
 * `overall: "unknown"` so the page generator can still render an honest state
 * instead of an error.
 */
export function createPublicHealthRouter(
  db: Database.Database,
  protocolStatusService: ProtocolStatusService,
  options: PublicHealthRouterOptions = {},
): Router {
  const router = Router();

  router.get('/public/health', async (_req, res) => {
    let snapshot;
    try {
      snapshot = await protocolStatusService.get();
    } catch {
      snapshot = { status: null, fetchedAt: null, stale: false, source: 'unavailable' as const };
    }

    try {
      const health = buildPublicHealth(db, {
        protocolStatus: snapshot,
        insurancePoolContractId: options.insurancePoolContractId,
        thresholds: options.thresholds,
      });
      res.set('Cache-Control', 'public, max-age=60');
      res.json(health);
    } catch (err) {
      res.status(500).json({
        error: `Failed to compute public health: ${err instanceof Error ? err.message : String(err)}`,
      });
    }
  });

  return router;
}
