/**
 * Shared alert-routing infrastructure (issue #889).
 *
 * General-purpose typed alert router that accepts alerts with severity levels
 * and routes them to configured channels (Slack, PagerDuty, webhook). This
 * closes the loop between on-chain event triggers and operator visibility.
 *
 * Designed to be consumed by: insurance-pool solvency (#888), reorg alerts,
 * admin-action anomaly detection, oracle health monitoring.
 */

import { logger } from '../lib/logger.js';

// ── Types ────────────────────────────────────────────────────────────────────

export type AlertSeverity = 'critical' | 'warning' | 'info';
export type ReorgDepthClass = 'shallow' | 'deep';

export function classifyReorgDepth(forkDepth: number): ReorgDepthClass {
  return forkDepth > 3 ? 'deep' : 'shallow';
}

export function classifyReorgSeverity(_forkDepth: number): AlertSeverity {
  return 'critical';
}

export type AlertCategory =
  | 'solvency_circuit_tripped'
  | 'solvency_circuit_resumed'
  | 'reorg_detected'
  | 'admin_action_anomaly'
  | 'oracle_health_degraded'
  | 'canary_failure';

export interface Alert {
  id: string;
  category: AlertCategory;
  severity: AlertSeverity;
  title: string;
  message: string;
  timestamp: string;
  metadata: Record<string, unknown>;
}

export interface AlertChannel {
  name: string;
  severityFilter: AlertSeverity[];
  send: (alert: Alert) => Promise<void>;
}

export interface AlertRouterConfig {
  /** Minimum severity to route (below this, alerts are dropped). */
  minSeverity: AlertSeverity;
  /** Cooldown per category: same category won't re-alert within this window. */
  cooldownMs: number;
}

// ── Severity ordering ────────────────────────────────────────────────────────

const SEVERITY_ORDER: Record<AlertSeverity, number> = {
  info: 0,
  warning: 1,
  critical: 2,
};

function meetsSeverityThreshold(
  alert: Alert,
  minSeverity: AlertSeverity
): boolean {
  return SEVERITY_ORDER[alert.severity] >= SEVERITY_ORDER[minSeverity];
}

// ── Alert Router ─────────────────────────────────────────────────────────────

export class AlertRouter {
  private channels: AlertChannel[] = [];
  private config: AlertRouterConfig;
  private cooldowns = new Map<string, number>();

  constructor(config: Partial<AlertRouterConfig> = {}) {
    this.config = {
      minSeverity: config.minSeverity ?? 'warning',
      cooldownMs: config.cooldownMs ?? 5 * 60 * 1000, // 5 minutes
    };
  }

  /** Register a channel for alert delivery. */
  addChannel(channel: AlertChannel): void {
    this.channels.push(channel);
  }

  /** Route an alert to all matching channels. */
  async route(alert: Alert): Promise<void> {
    if (!meetsSeverityThreshold(alert, this.config.minSeverity)) {
      logger.debug('Alert below severity threshold, skipping', {
        alertId: alert.id,
        severity: alert.severity,
        category: alert.category,
      });
      return;
    }

    // Cooldown check: prevent alert storms for the same category
    const lastFired = this.cooldowns.get(alert.category) ?? 0;
    if (Date.now() - lastFired < this.config.cooldownMs) {
      logger.debug('Alert in cooldown period', {
        alertId: alert.id,
        category: alert.category,
        cooldownRemainingMs: this.config.cooldownMs - (Date.now() - lastFired),
      });
      return;
    }

    this.cooldowns.set(alert.category, Date.now());

    const matchingChannels = this.channels.filter((ch) =>
      ch.severityFilter.includes(alert.severity)
    );

    if (matchingChannels.length === 0) {
      logger.warn('No channels configured for alert severity', {
        alertId: alert.id,
        severity: alert.severity,
      });
      return;
    }

    const results = await Promise.allSettled(
      matchingChannels.map(async (ch) => {
        try {
          await ch.send(alert);
          logger.info('Alert delivered', {
            alertId: alert.id,
            channel: ch.name,
            severity: alert.severity,
            category: alert.category,
          });
        } catch (err) {
          logger.error('Alert delivery failed', {
            alertId: alert.id,
            channel: ch.name,
            error: err instanceof Error ? err.message : String(err),
          });
        }
      })
    );

    const failures = results.filter((r) => r.status === 'rejected');
    if (failures.length > 0) {
      logger.warn('Some alert channels failed', {
        alertId: alert.id,
        failedCount: failures.length,
        totalCount: matchingChannels.length,
      });
    }
  }

  /** Get the number of registered channels. */
  get channelCount(): number {
    return this.channels.length;
  }
}

// ── Factory ──────────────────────────────────────────────────────────────────

let defaultRouter: AlertRouter | null = null;

/** Get or create the singleton alert router. */
export function getAlertRouter(): AlertRouter {
  if (!defaultRouter) {
    defaultRouter = new AlertRouter();
  }
  return defaultRouter;
}

/** Create an alert with auto-generated id and timestamp. */
export function createAlert(
  category: AlertCategory,
  severity: AlertSeverity,
  title: string,
  message: string,
  metadata: Record<string, unknown> = {}
): Alert {
  return {
    id: `alert-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    category,
    severity,
    title,
    message,
    timestamp: new Date().toISOString(),
    metadata,
  };
}

// ── Channel implementations ──────────────────────────────────────────────────

/** Webhook channel for sending alerts to a URL. */
export class WebhookAlertChannel implements AlertChannel {
  name: string;
  severityFilter: AlertSeverity[];
  private url: string;

  constructor(
    name: string,
    url: string,
    severityFilter: AlertSeverity[] = ['critical', 'warning']
  ) {
    this.name = name;
    this.url = url;
    this.severityFilter = severityFilter;
  }

  async send(alert: Alert): Promise<void> {
    const response = await fetch(this.url, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        text: `[${alert.severity.toUpperCase()}] ${alert.title}: ${alert.message}`,
        alert,
      }),
    });
    if (!response.ok) {
      throw new Error(`Webhook returned HTTP ${response.status}`);
    }
  }
}

/** Console channel for development/testing. */
export class ConsoleAlertChannel implements AlertChannel {
  name = 'console';
  severityFilter: AlertSeverity[];

  constructor(severityFilter: AlertSeverity[] = ['critical', 'warning', 'info']) {
    this.severityFilter = severityFilter;
  }

  async send(alert: Alert): Promise<void> {
    const prefix =
      alert.severity === 'critical'
        ? '🚨 CRITICAL'
        : alert.severity === 'warning'
          ? '⚠️  WARNING'
          : 'ℹ️  INFO';
    console.log(`${prefix}: ${alert.title} — ${alert.message}`);
    if (Object.keys(alert.metadata).length > 0) {
      console.log('  metadata:', JSON.stringify(alert.metadata, null, 2));
    }
  }
}
