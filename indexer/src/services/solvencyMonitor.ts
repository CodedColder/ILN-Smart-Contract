/**
 * Insurance pool solvency monitoring (issue #888).
 *
 * Consumes SolvencyCircuitTripped events and routes them through the shared
 * alert infrastructure at critical severity, since solvency breaches directly
 * affect user funds.
 */

import { AlertRouter, createAlert, getAlertRouter } from './alertRouter.js';
import { logger } from '../lib/logger.js';

// ── Types ────────────────────────────────────────────────────────────────────

export interface SolvencySnapshot {
  poolId: string;
  totalAssets: bigint;
  totalLiabilities: bigint;
  solvencyRatio: number;
  isSolvencyOk: boolean;
  trippedAt: number | null;
  resumedAt: number | null;
}

export interface SolvencyCircuitEvent {
  poolId: string;
  type: 'tripped' | 'resumed';
  timestamp: number;
  solvencyRatio: number;
  totalAssets: bigint;
  totalLiabilities: bigint;
}

// ── Monitor ──────────────────────────────────────────────────────────────────

export class SolvencyMonitor {
  private router: AlertRouter;
  private lastTrippedAt: Map<string, number> = new Map();

  constructor(router?: AlertRouter) {
    this.router = router ?? getAlertRouter();
  }

  /**
   * Process a solvency circuit-breaker event and route through alerting.
   * Call this when a SolvencyCircuitTripped or SolvencyCircuitResumed event
   * is emitted on-chain.
   */
  async handleCircuitEvent(event: SolvencyCircuitEvent): Promise<void> {
    const { poolId, type, solvencyRatio, totalAssets, totalLiabilities } = event;

    if (type === 'tripped') {
      this.lastTrippedAt.set(poolId, event.timestamp);

      const alert = createAlert(
        'solvency_circuit_tripped',
        'critical',
        `Insurance pool solvency circuit tripped — pool ${poolId.slice(0, 12)}…`,
        `Solvency ratio dropped to ${(solvencyRatio * 100).toFixed(2)}% ` +
          `(assets: ${totalAssets}, liabilities: ${totalLiabilities}). ` +
          `New claims are blocked until solvency is restored.`,
        {
          poolId,
          solvencyRatio,
          totalAssets: totalAssets.toString(),
          totalLiabilities: totalLiabilities.toString(),
          trippedAt: event.timestamp,
        }
      );

      logger.error('Solvency circuit tripped', {
        poolId,
        solvencyRatio,
        totalAssets: totalAssets.toString(),
        totalLiabilities: totalLiabilities.toString(),
      });

      await this.router.route(alert);
    } else {
      // resumed
      const trippedAt = this.lastTrippedAt.get(poolId);
      const downtimeMs = trippedAt
        ? (event.timestamp - trippedAt) * 1000
        : undefined;

      this.lastTrippedAt.delete(poolId);

      const alert = createAlert(
        'solvency_circuit_resumed',
        'info',
        `Insurance pool solvency restored — pool ${poolId.slice(0, 12)}…`,
        `Solvency ratio recovered to ${(solvencyRatio * 100).toFixed(2)}%.` +
          (downtimeMs !== undefined
            ? ` Pool was tripped for ${Math.round(downtimeMs / 1000)}s.`
            : ''),
        {
          poolId,
          solvencyRatio,
          totalAssets: totalAssets.toString(),
          totalLiabilities: totalLiabilities.toString(),
          resumedAt: event.timestamp,
          downtimeMs,
        }
      );

      logger.info('Solvency circuit resumed', {
        poolId,
        solvencyRatio,
        downtimeMs,
      });

      await this.router.route(alert);
    }
  }

  /** Get the last time a pool's solvency circuit was tripped. */
  getLastTrippedAt(poolId: string): number | undefined {
    return this.lastTrippedAt.get(poolId);
  }
}

// ── Singleton ────────────────────────────────────────────────────────────────

let defaultMonitor: SolvencyMonitor | null = null;

export function getSolvencyMonitor(): SolvencyMonitor {
  if (!defaultMonitor) {
    defaultMonitor = new SolvencyMonitor();
  }
  return defaultMonitor;
}
