import { Router } from 'express';
import type { DeliveryAnalyticsService } from '../services/deliveryAnalyticsService.js';

/**
 * Read-only delivery analytics (Issue #872).
 *
 *   GET /analytics/deliveries                      all per-subscriber summaries
 *   GET /analytics/deliveries/:endpointId          one summary (404 if unseen)
 *   GET /analytics/deliveries/:endpointId/counts   windowed counts (?since=ms)
 */
export function createAnalyticsRouter(analytics: DeliveryAnalyticsService): Router {
  const router = Router();

  router.get('/analytics/deliveries', (_req, res) => {
    res.json({ items: analytics.summaries() });
  });

  router.get('/analytics/deliveries/:endpointId', (req, res) => {
    const summary = analytics.summary(req.params.endpointId);
    if (!summary) {
      res.status(404).json({ error: 'not_found' });
      return;
    }
    res.json(summary);
  });

  router.get('/analytics/deliveries/:endpointId/counts', (req, res) => {
    const endpointId = req.params.endpointId;
    if (!analytics.summary(endpointId)) {
      res.status(404).json({ error: 'not_found' });
      return;
    }

    const raw = req.query.since;
    let since = 0;
    if (raw !== undefined) {
      const parsed = Number(raw);
      if (!Number.isFinite(parsed)) {
        res.status(400).json({ error: 'invalid_since' });
        return;
      }
      since = parsed;
    }

    res.json(analytics.countsSince(endpointId, since));
  });

  return router;
}
