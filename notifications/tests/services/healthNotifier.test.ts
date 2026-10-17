import { describe, expect, it } from 'vitest';
import { EmailDeliveryService } from '../../src/delivery/emailDelivery';
import type { EmailMessage } from '../../src/delivery/emailDelivery';
import { createEmailHealthNotifier, type HealthNotice } from '../../src/services/healthNotifier';

function makeNotice(overrides: Partial<HealthNotice> = {}): HealthNotice {
  return {
    subscriberId: 'ep_1',
    status: 'suspended',
    reason: 'circuit_trips 3 >= 3 over 1d',
    circuitTrips: 3,
    tripThreshold: 3,
    windowDays: 1,
    at: 1_700_000_000_000,
    reactivationUrl: 'https://notifications.iln.dev/subscriptions/health/ep_1/reactivate',
    ...overrides,
  };
}

function harness(resolveRecipient: (id: string) => string | null, shouldThrow = false) {
  const sent: EmailMessage[] = [];
  const emailDelivery = new EmailDeliveryService(
    {
      send: async (message: EmailMessage) => {
        if (shouldThrow) {
          throw new Error('smtp down');
        }
        sent.push(message);
        return { id: `msg_${sent.length}` };
      },
    },
    'noreply@iln.dev',
  );
  const notifier = createEmailHealthNotifier({
    emailDelivery,
    resolveRecipient,
    publicUrl: 'https://notifications.iln.dev',
  });
  return { notifier, sent };
}

describe('createEmailHealthNotifier', () => {
  it('notifies the contact on file with the reason and reactivation link', async () => {
    const { notifier, sent } = harness(() => 'ops@example.com');

    expect(notifier.channel).toBe('email');
    const result = await notifier.send(makeNotice());

    expect(result).toEqual({ ok: true });
    expect(sent).toHaveLength(1);
    expect(sent[0]?.to).toBe('ops@example.com');
    expect(sent[0]?.subject).toBe('ILN webhook subscription suspended: action required');
    expect(sent[0]?.text).toContain('circuit_trips 3 >= 3 over 1d');
    expect(sent[0]?.text).toContain(
      'https://notifications.iln.dev/subscriptions/health/ep_1/reactivate',
    );
    expect(sent[0]?.html).toContain('has been <strong>suspended</strong>');
    expect(sent[0]?.html).toContain(
      'https://notifications.iln.dev/subscriptions/health/ep_1/reactivate',
    );
  });

  it('uses the configured public URL when the notice carries no link', async () => {
    const { notifier, sent } = harness(() => 'ops@example.com');

    await notifier.send(makeNotice({ reactivationUrl: null }));

    expect(sent[0]?.text).toContain(
      'https://notifications.iln.dev/subscriptions/health/ep_1/reactivate',
    );
  });

  it('explains a flag differently from a suspension', async () => {
    const { notifier, sent } = harness(() => 'ops@example.com');

    await notifier.send(makeNotice({ status: 'flagged' }));

    expect(sent[0]?.subject).toBe('ILN webhook subscription flagged: action required');
    expect(sent[0]?.text).toContain('Deliveries are still attempted');
    expect(sent[0]?.html).toContain('flagged for review');
  });

  it('reports a subscriber with no contact on file without sending', async () => {
    const { notifier, sent } = harness(() => null);

    const result = await notifier.send(makeNotice());

    expect(result).toEqual({ ok: false, error: 'no_contact_on_file' });
    expect(sent).toHaveLength(0);
  });

  it('propagates a delivery failure', async () => {
    const { notifier, sent } = harness(() => 'ops@example.com', true);

    const result = await notifier.send(makeNotice());

    expect(result.ok).toBe(false);
    expect(result.error).toBe('smtp down');
    expect(sent).toHaveLength(0);
  });

  it('escapes subscriber-controlled values in the HTML body', async () => {
    const { notifier, sent } = harness(() => 'ops@example.com');

    await notifier.send(makeNotice({ subscriberId: '<script>alert(1)</script>' }));

    expect(sent[0]?.html).not.toContain('<script>alert(1)</script>');
    expect(sent[0]?.html).toContain('&lt;script&gt;alert(1)&lt;/script&gt;');
  });
});
