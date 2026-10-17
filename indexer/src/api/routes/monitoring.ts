/**
 * Monitoring & Analytics API routes (issues #884–#887).
 *
 * Exposes dashboard endpoints for:
 * - #884: Reputation audit trail with anomaly detection
 * - #885: LP queue depth, funding velocity, default rate
 * - #886: Admin action anomaly monitoring
 * - #887: Oracle health monitoring
 */

import { Router } from 'express';
import type Database from 'better-sqlite3';
import {
  getReputationHistory,
  getReputationAuditSummary,
  detectReputationAnomalies,
} from '../../services/reputationAuditService.js';
import {
  getProtocolHealthDashboard,
} from '../../services/protocolHealthService.js';
import { getAdminActionMonitor } from '../../services/adminActionMonitor.js';
import {
  getOracleHealthDashboard,
} from '../../services/oracleHealthService.js';

export function createMonitoringRouter(db: Database.Database): Router {
  const router = Router();

  // ── #884: Reputation Audit Trail ─────────────────────────────────────────

  /**
   * GET /monitoring/reputation/audit
   *
   * Full reputation audit summary: total updates, unique addresses,
   * anomalies, and recent updates across all addresses.
   */
  router.get('/reputation/audit', (_req, res) => {
    try {
      const summary = getReputationAuditSummary(db);
      res.json(summary);
    } catch (err) {
      res.status(500).json({
        error: 'Failed to compute reputation audit summary',
        message: err instanceof Error ? err.message : String(err),
      });
    }
  });

  /**
   * GET /monitoring/reputation/history/:address
   *
   * Reputation change history for a specific address with anomaly detection.
   */
  router.get('/reputation/history/:address', (req, res) => {
    try {
      const { address } = req.params;
      const limit = Math.min(parseInt(req.query.limit as string) || 100, 500);

      const history = getReputationHistory(db, address, limit);
      const anomalies = detectReputationAnomalies(history);

      res.json({
        address,
        history,
        anomalies,
        totalEntries: history.length,
      });
    } catch (err) {
      res.status(500).json({
        error: 'Failed to fetch reputation history',
        message: err instanceof Error ? err.message : String(err),
      });
    }
  });

  // ── #885: Protocol Health Dashboard ──────────────────────────────────────

  /**
   * GET /monitoring/protocol/health
   *
   * Full protocol health dashboard: queue depth, funding velocity,
   * default rate, time-to-fund distribution, and healthy ranges.
   */
  router.get('/protocol/health', (_req, res) => {
    try {
      const dashboard = getProtocolHealthDashboard(db);
      res.json(dashboard);
    } catch (err) {
      res.status(500).json({
        error: 'Failed to compute protocol health dashboard',
        message: err instanceof Error ? err.message : String(err),
      });
    }
  });

  // ── #886: Admin Action Monitoring ────────────────────────────────────────

  /**
   * GET /monitoring/admin/actions
   *
   * Current state of the admin action monitor: recent action count
   * and buffer status.
   */
  router.get('/admin/actions', (_req, res) => {
    try {
      const monitor = getAdminActionMonitor();
      res.json({
        recentActionCount: monitor.recentActionCount,
        status: 'monitoring',
      });
    } catch (err) {
      res.status(500).json({
        error: 'Failed to read admin action monitor',
        message: err instanceof Error ? err.message : String(err),
      });
    }
  });

  // ── #887: Oracle Health Dashboard ────────────────────────────────────────

  /**
   * GET /monitoring/oracle/health
   *
   * Full oracle health dashboard: feed health, aggregate metrics,
   * recent trips, and healthy ranges.
   */
  router.get('/oracle/health', (_req, res) => {
    try {
      const dashboard = getOracleHealthDashboard(db);
      res.json(dashboard);
    } catch (err) {
      res.status(500).json({
        error: 'Failed to compute oracle health dashboard',
        message: err instanceof Error ? err.message : String(err),
      });
    }
  });

  return router;
}
