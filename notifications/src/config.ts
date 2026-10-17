import { tmpdir } from 'node:os';
import { join } from 'node:path';

const port = Number(process.env.PORT ?? 3001);

const DAY_MS = 24 * 60 * 60 * 1000;

export const config = {
  port,
  dbPath: process.env.NOTIFICATIONS_DB_PATH ?? join(tmpdir(), 'iln-notifications.db'),
  publicUrl: process.env.NOTIFICATIONS_PUBLIC_URL ?? `http://localhost:${port}`,
  emailFrom: process.env.EMAIL_FROM ?? 'ILN Notifications <noreply@iln.dev>',
  emailTokenSecret: process.env.EMAIL_TOKEN_SECRET ?? 'iln-notifications-email-secret',
  resendApiKey: process.env.RESEND_API_KEY ?? '',
  // Delivery history retention (Issue #733): response bodies are purged
  // before full records so PII (recipient emails, message content) is kept
  // for the shortest practical window while debugging metadata survives.
  deliveryBodyRetentionMs: Number(
    process.env.NOTIFICATIONS_DELIVERY_BODY_RETENTION_MS ?? 7 * DAY_MS,
  ),
  deliveryRecordRetentionMs: Number(
    process.env.NOTIFICATIONS_DELIVERY_RECORD_RETENTION_MS ?? 90 * DAY_MS,
  ),
  // --- Services layer (Issue #870) ---------------------------------------
  // Digest batching window for subscribers that opted in (Issue #871).
  digestWindowMs: Number(process.env.NOTIFICATIONS_DIGEST_WINDOW_MS ?? 5 * 60 * 1000),
  digestMaxBatchSize: Number(process.env.NOTIFICATIONS_DIGEST_MAX_BATCH_SIZE ?? 100),
  // How long delivery signals stay available to analytics (Issue #872).
  analyticsRetentionMs: Number(
    process.env.NOTIFICATIONS_ANALYTICS_RETENTION_MS ?? 30 * DAY_MS,
  ),
  // "N circuit trips over M days" subscription-health policy (Issue #873).
  healthTripThreshold: Number(process.env.NOTIFICATIONS_HEALTH_TRIP_THRESHOLD ?? 3),
  healthWindowDays: Number(process.env.NOTIFICATIONS_HEALTH_WINDOW_DAYS ?? 1),
  healthAutoSuspend: (process.env.NOTIFICATIONS_HEALTH_AUTO_SUSPEND ?? 'true') !== 'false',
};
