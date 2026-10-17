/**
 * Admin-action anomaly alerting (issue #886).
 *
 * Monitors the admin-action event/log stream for unusual patterns:
 * rapid successive parameter changes, off-hours activity, and
 * high-frequency actions that may indicate compromise or misconfiguration.
 *
 * Routes anomalies through the shared AlertRouter infrastructure.
 */

import { AlertRouter, createAlert, getAlertRouter } from './alertRouter.js';
import { logger } from '../lib/logger.js';

// ── Types ────────────────────────────────────────────────────────────────────

export interface AdminActionEvent {
  action: string;
  timestamp: number;
  ledger: number;
  admin?: string;
  details?: Record<string, unknown>;
}

export interface AdminActionAnomaly {
  type: 'rapid_successive' | 'off_hours' | 'high_frequency';
  actions: AdminActionEvent[];
  windowMinutes: number;
  actionCount: number;
  description: string;
}

export interface AdminActionMonitorConfig {
  /** Number of actions within windowMinutes to trigger rapid-successive alert. */
  rapidSuccessiveThreshold: number;
  /** Window in minutes for rapid-successive detection. */
  rapidSuccessiveWindowMinutes: number;
  /** Hour (0-23) when admin actions are considered off-hours start. */
  offHoursStart: number;
  /** Hour (0-23) when admin actions are considered off-hours end. */
  offHoursEnd: number;
  /** Number of actions within highFrequencyWindowMinutes to trigger. */
  highFrequencyThreshold: number;
  /** Window in minutes for high-frequency detection. */
  highFrequencyWindowMinutes: number;
}

const DEFAULT_CONFIG: AdminActionMonitorConfig = {
  rapidSuccessiveThreshold: 3,
  rapidSuccessiveWindowMinutes: 30,
  offHoursStart: 22,
  offHoursEnd: 6,
  highFrequencyThreshold: 10,
  highFrequencyWindowMinutes: 60,
};

// ── Monitor ──────────────────────────────────────────────────────────────────

export class AdminActionMonitor {
  private router: AlertRouter;
  private config: AdminActionMonitorConfig;
  private recentActions: AdminActionEvent[] = [];

  constructor(
    config: Partial<AdminActionMonitorConfig> = {},
    router?: AlertRouter
  ) {
    this.config = { ...DEFAULT_CONFIG, ...config };
    this.router = router ?? getAlertRouter();
  }

  /**
   * Process an admin action event and check for anomalies.
   * Call this when an admin action is recorded on-chain.
   */
  async handleAdminAction(event: AdminActionEvent): Promise<AdminActionAnomaly[]> {
    this.recentActions.push(event);

    // Keep only recent actions within the largest monitoring window
    const maxWindowMs = this.config.highFrequencyWindowMinutes * 60 * 1000;
    const cutoff = Date.now() - maxWindowMs;
    this.recentActions = this.recentActions.filter(
      (a) => a.timestamp * 1000 >= cutoff
    );

    const anomalies: AdminActionAnomaly[] = [];

    // Check rapid successive actions
    const rapidAnomaly = this.checkRapidSuccessive(event);
    if (rapidAnomaly) {
      anomalies.push(rapidAnomaly);
      await this.routeAnomaly(rapidAnomaly);
    }

    // Check off-hours activity
    const offHoursAnomaly = this.checkOffHours(event);
    if (offHoursAnomaly) {
      anomalies.push(offHoursAnomaly);
      await this.routeAnomaly(offHoursAnomaly);
    }

    // Check high frequency
    const highFreqAnomaly = this.checkHighFrequency();
    if (highFreqAnomaly) {
      anomalies.push(highFreqAnomaly);
      await this.routeAnomaly(highFreqAnomaly);
    }

    return anomalies;
  }

  private checkRapidSuccessive(latest: AdminActionEvent): AdminActionAnomaly | null {
    const windowSeconds = this.config.rapidSuccessiveWindowMinutes * 60;
    const cutoff = latest.timestamp - windowSeconds;

    const recentInWindow = this.recentActions.filter(
      (a) => a.timestamp >= cutoff
    );

    if (recentInWindow.length >= this.config.rapidSuccessiveThreshold) {
      return {
        type: 'rapid_successive',
        actions: recentInWindow,
        windowMinutes: this.config.rapidSuccessiveWindowMinutes,
        actionCount: recentInWindow.length,
        description:
          `${recentInWindow.length} admin actions within ` +
          `${this.config.rapidSuccessiveWindowMinutes} minutes ` +
          `(threshold: ${this.config.rapidSuccessiveThreshold}). ` +
          `Actions: ${recentInWindow.map((a) => a.action).join(', ')}`,
      };
    }

    return null;
  }

  private checkOffHours(event: AdminActionEvent): AdminActionAnomaly | null {
    const date = new Date(event.timestamp * 1000);
    const hour = date.getUTCHours();

    const { offHoursStart, offHoursEnd } = this.config;
    const isOffHours =
      offHoursStart > offHoursEnd
        ? hour >= offHoursStart || hour < offHoursEnd
        : hour >= offHoursStart && hour < offHoursEnd;

    if (isOffHours) {
      return {
        type: 'off_hours',
        actions: [event],
        windowMinutes: 0,
        actionCount: 1,
        description:
          `Admin action "${event.action}" executed at ` +
          `${date.toISOString()} (hour ${hour}), ` +
          `outside the ${offHoursStart}:00–${offHoursEnd}:00 UTC ` +
          `maintenance window.`,
      };
    }

    return null;
  }

  private checkHighFrequency(): AdminActionAnomaly | null {
    const windowSeconds = this.config.highFrequencyWindowMinutes * 60;
    const cutoff = Math.floor(Date.now() / 1000) - windowSeconds;

    const recentInWindow = this.recentActions.filter(
      (a) => a.timestamp >= cutoff
    );

    if (recentInWindow.length >= this.config.highFrequencyThreshold) {
      return {
        type: 'high_frequency',
        actions: recentInWindow,
        windowMinutes: this.config.highFrequencyWindowMinutes,
        actionCount: recentInWindow.length,
        description:
          `${recentInWindow.length} admin actions within ` +
          `${this.config.highFrequencyWindowMinutes} minutes ` +
          `(threshold: ${this.config.highFrequencyThreshold}).`,
      };
    }

    return null;
  }

  private async routeAnomaly(anomaly: AdminActionAnomaly): Promise<void> {
    const severity =
      anomaly.type === 'rapid_successive' ? 'critical' : 'warning';

    const alert = createAlert(
      'admin_action_anomaly',
      severity,
      `Admin action anomaly: ${anomaly.type.replace('_', ' ')}`,
      anomaly.description,
      {
        anomalyType: anomaly.type,
        actionCount: anomaly.actionCount,
        windowMinutes: anomaly.windowMinutes,
        actions: anomaly.actions.map((a) => ({
          action: a.action,
          timestamp: a.timestamp,
          ledger: a.ledger,
        })),
      }
    );

    logger.warn('Admin action anomaly detected', {
      type: anomaly.type,
      actionCount: anomaly.actionCount,
      description: anomaly.description,
    });

    await this.router.route(alert);
  }

  /** Get the count of recent actions being tracked. */
  get recentActionCount(): number {
    return this.recentActions.length;
  }

  /** Reset the action buffer (e.g. for testing). */
  resetBuffer(): void {
    this.recentActions = [];
  }
}

// ── Singleton ────────────────────────────────────────────────────────────────

let defaultMonitor: AdminActionMonitor | null = null;

export function getAdminActionMonitor(): AdminActionMonitor {
  if (!defaultMonitor) {
    defaultMonitor = new AdminActionMonitor();
  }
  return defaultMonitor;
}
