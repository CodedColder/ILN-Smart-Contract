import { escapeHtml, sanitizeHeader } from '../templates/common.js';
import type { EmailDeliveryService } from '../delivery/emailDelivery.js';

/**
 * Non-webhook notification of a subscription-health decision (Issue #873).
 *
 * The webhook endpoint is precisely what is failing, so health notices are
 * never sent over it: the service takes a {@link HealthNotifier} and the app
 * wires the email channel.
 */
export interface HealthNotice {
  subscriberId: string;
  status: 'flagged' | 'suspended';
  /** Human-readable explanation of the threshold that was met. */
  reason: string;
  circuitTrips: number;
  tripThreshold: number;
  windowDays: number;
  at: number;
  /** Where the subscriber can resume delivery (null when unknown). */
  reactivationUrl: string | null;
}

export interface HealthNotifyResult {
  ok: boolean;
  error?: string | undefined;
}

export interface HealthNotifier {
  /** Channel identifier, surfaced in logs (`health_notice_channel`). */
  readonly channel: string;
  send(notice: HealthNotice): Promise<HealthNotifyResult>;
}

export interface EmailHealthNotifierOptions {
  emailDelivery: EmailDeliveryService;
  /** Contact email for a subscriber, or null when none is on file. */
  resolveRecipient: (subscriberId: string) => string | null;
  /** Base URL used to build the reactivation link. */
  publicUrl: string;
}

/**
 * Renders a health notice as an operational email.
 *
 * Every subscriber-supplied value is escaped and header-safe
 * (`sanitizeHeader` strips CRLF), matching the treatment used by the invoice
 * notification templates.
 */
export function createEmailHealthNotifier(
  options: EmailHealthNotifierOptions,
): HealthNotifier {
  return {
    channel: 'email',
    async send(notice: HealthNotice): Promise<HealthNotifyResult> {
      const to = options.resolveRecipient(notice.subscriberId);
      if (!to) {
        return { ok: false, error: 'no_contact_on_file' };
      }

      const subject = sanitizeHeader(
        `ILN webhook subscription ${notice.status}: action required`,
      );
      const reactivationUrl =
        notice.reactivationUrl ??
        `${options.publicUrl}/subscriptions/health/${encodeURIComponent(
          notice.subscriberId,
        )}/reactivate`;

      const text = [
        `Your webhook subscription ${notice.subscriberId} has been ${notice.status}.`,
        '',
        `Reason: ${notice.reason}`,
        `Circuit-breaker trips: ${notice.circuitTrips} (threshold ${notice.tripThreshold} over ${notice.windowDays} day(s))`,
        '',
        notice.status === 'suspended'
          ? 'Deliveries to your endpoint are paused. Once your endpoint is healthy again, reactivate it by sending POST to:'
          : 'Deliveries are still attempted, but this subscription is flagged for review. You can clear the flag by sending POST to:',
        reactivationUrl,
        'with the header `x-api-key: <your subscription secret>`.',
        '',
        'This is an operational notice; replies are not monitored.',
      ].join('\n');

      const html = [
        '<p>Your webhook subscription <strong>',
        escapeHtml(notice.subscriberId),
        '</strong> has been <strong>',
        escapeHtml(notice.status),
        '</strong>.</p>',
        '<p>Reason: ',
        escapeHtml(notice.reason),
        '</p>',
        `<p>Circuit-breaker trips: ${notice.circuitTrips} (threshold ${notice.tripThreshold} over ${notice.windowDays} day(s))</p>`,
        '<p>',
        notice.status === 'suspended'
          ? 'Deliveries to your endpoint are paused.'
          : 'Deliveries are still attempted, but this subscription is flagged for review.',
        '</p>',
        '<p>Reactivate with <code>POST ',
        escapeHtml(reactivationUrl),
        '</code> and the header <code>x-api-key: &lt;your subscription secret&gt;</code>.</p>',
      ].join('');

      const result = await options.emailDelivery.send({ to, subject, html, text });
      if (result.ok) {
        return { ok: true };
      }
      return { ok: false, error: result.error };
    },
  };
}
