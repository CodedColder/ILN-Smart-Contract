import express from 'express';
import { config } from './config.js';
import { createNotificationsDatabase } from './database.js';
import { SubscriptionStore } from './subscriptions/subscriptionStore.js';
import { EmailSubscriptionStore } from './subscriptions/emailSubscriptionStore.js';
import { WebhookDeliveryService } from './delivery/webhookDelivery.js';
import { EmailDeliveryService } from './delivery/emailDelivery.js';
import { createEmailClient } from './delivery/emailClient.js';
import { DeliveryHistoryStore } from './delivery/deliveryHistory.js';
import {
  DeliveryAnalyticsService,
  DigestService,
  ServiceRegistry,
  SubscriptionHealthService,
  createEmailHealthNotifier,
} from './services/index.js';
import { createWebhooksRouter } from './api/webhooks.js';
import { createSlackRouter } from './api/slack.js';
import { createTelegramRouter } from './api/telegram.js';
import { createEmailSubscriptionsRouter } from './api/email.js';
import { createEmailNotificationsRouter } from './api/emailNotifications.js';
import { createAnalyticsRouter } from './api/analytics.js';
import { createDigestRouter } from './api/digest.js';
import { createSubscriptionHealthRouter } from './api/subscriptionHealth.js';
import type { SlackSubscription } from './api/slack.js';
import type { TelegramSubscription } from './api/telegram.js';
import { logger } from './lib/logger.js';
import { createRequestIdMiddleware } from './lib/requestId.js';

const db = createNotificationsDatabase(config.dbPath);
const port = config.port;
const store = new SubscriptionStore(db);
const emailStore = new EmailSubscriptionStore(db);
// Retention policy (Issue #733): bodies are purged before full records so
// recipient PII is not retained indefinitely.
const historyStore = new DeliveryHistoryStore({
  bodyRetentionMs: config.deliveryBodyRetentionMs,
  recordRetentionMs: config.deliveryRecordRetentionMs,
});
const emailDelivery = new EmailDeliveryService(
  createEmailClient({
    apiKey: config.resendApiKey,
    from: config.emailFrom,
    logger: console,
  }),
  config.emailFrom,
);

// --- Services layer (Issue #870) --------------------------------------------
// Analytics is created before delivery because it *is* the event sink the
// delivery service reports to; health sits on top of analytics and feeds a
// suspension predicate back into delivery.
const analytics = new DeliveryAnalyticsService({
  retentionMs: config.analyticsRetentionMs,
});
const health = new SubscriptionHealthService({
  analytics,
  policy: {
    tripThreshold: config.healthTripThreshold,
    windowDays: config.healthWindowDays,
    autoSuspend: config.healthAutoSuspend,
  },
  // Health notices never use the failing webhook channel (Issue #873).
  notifier: createEmailHealthNotifier({
    emailDelivery,
    publicUrl: config.publicUrl,
    resolveRecipient: (subscriberId) =>
      store.getByEndpointId(subscriberId)?.contactEmail ?? null,
  }),
  publicUrl: config.publicUrl,
});
const delivery = new WebhookDeliveryService({
  http: async (url, init) => {
    const res = await fetch(url, init);
    return { status: res.status };
  },
  // Structured logging (Issue #776) — carries the request/event correlation id.
  logger: (msg) => logger.info(msg, { component: 'webhook-delivery' }),
  historyStore,
  events: analytics,
  isSuspended: (endpointId) => health.isSuspended(endpointId),
});
const digest = new DigestService({
  windowMs: config.digestWindowMs,
  maxBatchSize: config.digestMaxBatchSize,
  deliver: async (batch) => {
    const subscription = store.getByEndpointId(batch.subscriberId);
    if (!subscription) {
      logger.warn('digest_batch_dropped', {
        subscriberId: batch.subscriberId,
        reason: 'unknown_subscriber',
      });
      return;
    }
    const newest = batch.notifications[batch.notifications.length - 1];
    await delivery.deliver(
      { id: subscription.endpointId, url: subscription.url, secret: subscription.secret },
      {
        event: 'digest.batch',
        invoiceId: newest?.invoiceId ?? 0,
        data: { windowStart: batch.windowStart, notifications: batch.notifications },
        timestamp: new Date().toISOString(),
      },
      'digest.batch',
    );
  },
});

const services = new ServiceRegistry()
  .register(analytics)
  .register(health)
  .register(digest);
// Lifecycle is explicit so a SIGTERM drains digest batches before exit.
await services.startAll();

const slackStore = new Map<string, SlackSubscription>();
const telegramStore = new Map<string, TelegramSubscription>();

const app = express();
// Correlation-ID first so every downstream log line for the request is tagged
// (Issue #776).
app.use(createRequestIdMiddleware());
app.use(express.json());
app.use(createWebhooksRouter(store, delivery, historyStore));
app.use(createSlackRouter(slackStore));
app.use(createTelegramRouter(telegramStore));
app.use(
  createEmailSubscriptionsRouter(emailStore, emailDelivery, {
    tokenSecret: config.emailTokenSecret,
    publicUrl: config.publicUrl,
  })
);
app.use(
  createEmailNotificationsRouter(emailStore, emailDelivery, {
    tokenSecret: config.emailTokenSecret,
    publicUrl: config.publicUrl,
  })
);
app.use(createAnalyticsRouter(analytics));
app.use(createDigestRouter(digest, store));
app.use(createSubscriptionHealthRouter(health, store));
app.get('/health', (_req, res) => res.json({ status: 'ok' }));

const server = app.listen(port, () => {
  logger.info('ILN notifications service listening', { port });
});

async function shutdown(signal: string): Promise<void> {
  logger.info('shutdown_requested', { signal });
  server.close();
  try {
    await services.stopAll();
  } catch (err) {
    logger.error('shutdown_stop_failed', { error: String(err) });
  }
  process.exit(0);
}

process.once('SIGINT', () => void shutdown('SIGINT'));
process.once('SIGTERM', () => void shutdown('SIGTERM'));
