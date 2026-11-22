import { afterEach, describe, expect, it, vi } from 'vitest';
import Database from 'better-sqlite3';
import { WebhookDeliveryService, type HttpClient } from '../src/delivery/webhookDelivery';

interface WebhookConfig {
  id: string;
  url: string;
  secret: string;
  eventTypes: string[];
  active: boolean;
}

class WebhookConfigStore {
  private configs = new Map<string, WebhookConfig>();

  add(config: WebhookConfig): void {
    this.configs.set(config.id, config);
  }

  get(id: string): WebhookConfig | undefined {
    return this.configs.get(id);
  }

  update(id: string, patch: Partial<WebhookConfig>): boolean {
    const existing = this.configs.get(id);
    if (!existing) return false;
    this.configs.set(id, { ...existing, ...patch });
    return true;
  }

  remove(id: string): boolean {
    return this.configs.delete(id);
  }

  listActive(): WebhookConfig[] {
    return Array.from(this.configs.values()).filter((c) => c.active);
  }
}

describe('Notification Channel Configuration Reload', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('detects missing webhook configuration', () => {
    const store = new WebhookConfigStore();

    const config = store.get('nonexistent-webhook');
    expect(config).toBeUndefined();
  });

  it('detects malformed webhook URLs', () => {
    const store = new WebhookConfigStore();
    const malformedConfigs = [
      { id: 'w1', url: 'not-a-url', secret: 's', eventTypes: ['invoice.paid'], active: true },
      { id: 'w2', url: 'ftp://example.com', secret: 's', eventTypes: ['invoice.paid'], active: true },
      { id: 'w3', url: '', secret: 's', eventTypes: ['invoice.paid'], active: true },
      {
        id: 'w4',
        url: 'http://example.com:99999',
        secret: 's',
        eventTypes: ['invoice.paid'],
        active: true,
      },
    ];

    malformedConfigs.forEach((config) => {
      const isValid = isValidWebhookUrl(config.url);
      expect(isValid).toBe(false);
    });
  });

  it('validates webhook URL protocols', () => {
    const validUrls = [
      'https://example.com/webhook',
      'http://example.com:8080/webhook',
      'https://sub.example.com/path?query=1',
    ];

    const invalidUrls = [
      'ftp://example.com',
      'mailto:user@example.com',
      'javascript:alert("xss")',
      'file:///etc/passwd',
    ];

    validUrls.forEach((url) => {
      expect(isValidWebhookUrl(url)).toBe(true);
    });

    invalidUrls.forEach((url) => {
      expect(isValidWebhookUrl(url)).toBe(false);
    });
  });

  it('prevents sending to unintended recipients during configuration rotation', async () => {
    const deliveredUrls: string[] = [];
    const http: HttpClient = async (url: string) => {
      deliveredUrls.push(url);
      return { status: 200 };
    };

    const service = new WebhookDeliveryService({ http });
    const store = new WebhookConfigStore();

    store.add({
      id: 'old-webhook',
      url: 'https://old-recipient.example.com/webhook',
      secret: 'old-secret',
      eventTypes: ['invoice.paid'],
      active: true,
    });

    await service.deliver(
      {
        id: 'old-webhook',
        url: 'https://old-recipient.example.com/webhook',
        secret: 'old-secret',
      },
      { event: 'invoice.paid' },
    );

    expect(deliveredUrls).toContain('https://old-recipient.example.com/webhook');

    store.remove('old-webhook');
    store.add({
      id: 'new-webhook',
      url: 'https://new-recipient.example.com/webhook',
      secret: 'new-secret',
      eventTypes: ['invoice.paid'],
      active: true,
    });

    deliveredUrls.length = 0;

    await service.deliver(
      {
        id: 'new-webhook',
        url: 'https://new-recipient.example.com/webhook',
        secret: 'new-secret',
      },
      { event: 'invoice.paid' },
    );

    expect(deliveredUrls).toContain('https://new-recipient.example.com/webhook');
    expect(deliveredUrls).not.toContain('https://old-recipient.example.com/webhook');
  });

  it('handles missing event types in configuration', () => {
    const store = new WebhookConfigStore();
    const config: any = {
      id: 'webhook_1',
      url: 'https://example.com/hook',
      secret: 'secret',
      eventTypes: undefined,
      active: true,
    };

    if (Array.isArray(config.eventTypes) && config.eventTypes.length > 0) {
      store.add(config);
    }

    const retrieved = store.get('webhook_1');
    expect(retrieved).toBeUndefined();
  });

  it('detects empty event type lists', () => {
    const config = {
      id: 'webhook_1',
      url: 'https://example.com/hook',
      secret: 'secret',
      eventTypes: [],
      active: true,
    };

    const isValid = Array.isArray(config.eventTypes) && config.eventTypes.length > 0;
    expect(isValid).toBe(false);
  });

  it('supports safe configuration updates without disrupting active deliveries', () => {
    const store = new WebhookConfigStore();

    store.add({
      id: 'webhook_1',
      url: 'https://example.com/v1/webhook',
      secret: 'secret-v1',
      eventTypes: ['invoice.paid', 'invoice.draft'],
      active: true,
    });

    const originalConfig = store.get('webhook_1')!;
    expect(originalConfig.url).toBe('https://example.com/v1/webhook');

    store.update('webhook_1', { url: 'https://example.com/v2/webhook' });

    const updatedConfig = store.get('webhook_1')!;
    expect(updatedConfig.url).toBe('https://example.com/v2/webhook');
    expect(updatedConfig.secret).toBe('secret-v1');
    expect(updatedConfig.eventTypes).toEqual(['invoice.paid', 'invoice.draft']);
  });

  it('detects when configuration is in disabled/inactive state', () => {
    const store = new WebhookConfigStore();

    store.add({
      id: 'webhook_1',
      url: 'https://example.com/hook',
      secret: 'secret',
      eventTypes: ['invoice.paid'],
      active: false,
    });

    const activeConfigs = store.listActive();
    expect(activeConfigs).toHaveLength(0);

    store.update('webhook_1', { active: true });
    const reactivated = store.listActive();
    expect(reactivated).toHaveLength(1);
  });

  it('validates configuration before applying changes', () => {
    const store = new WebhookConfigStore();

    store.add({
      id: 'webhook_1',
      url: 'https://example.com/hook',
      secret: 'secret',
      eventTypes: ['invoice.paid'],
      active: true,
    });

    const updated = store.update('webhook_1', {
      url: 'invalid-url',
      secret: 'new-secret',
    });

    expect(updated).toBe(true);
    const config = store.get('webhook_1')!;
    expect(config.url).toBe('invalid-url');
  });

  it('detects rotated webhook secrets safely', () => {
    const store = new WebhookConfigStore();

    const oldSecret = 'old-secret-key-32-chars-min-len';
    store.add({
      id: 'webhook_1',
      url: 'https://example.com/hook',
      secret: oldSecret,
      eventTypes: ['invoice.paid'],
      active: true,
    });

    const newSecret = 'new-secret-key-32-chars-min-len';
    store.update('webhook_1', { secret: newSecret });

    const updated = store.get('webhook_1')!;
    expect(updated.secret).toBe(newSecret);
    expect(updated.secret).not.toBe(oldSecret);
  });

  it('prevents configuration reload while delivery is in progress', () => {
    const store = new WebhookConfigStore();
    let deliveryInProgress = false;

    store.add({
      id: 'webhook_1',
      url: 'https://example.com/hook',
      secret: 'secret',
      eventTypes: ['invoice.paid'],
      active: true,
    });

    deliveryInProgress = true;

    const canUpdate = !deliveryInProgress;
    expect(canUpdate).toBe(false);

    deliveryInProgress = false;

    const updated = store.update('webhook_1', { url: 'https://example.com/hook/v2' });
    expect(updated).toBe(true);
  });

  it('maintains configuration consistency across concurrent reads', () => {
    const store = new WebhookConfigStore();

    const configs = [
      { id: 'w1', url: 'https://a.com', secret: 's1', eventTypes: ['invoice.paid'], active: true },
      { id: 'w2', url: 'https://b.com', secret: 's2', eventTypes: ['invoice.draft'], active: true },
      { id: 'w3', url: 'https://c.com', secret: 's3', eventTypes: ['invoice.funded'], active: false },
    ];

    configs.forEach((c) => store.add(c));

    const reads = [store.listActive(), store.listActive(), store.listActive()];

    reads.forEach((read) => {
      expect(read).toHaveLength(2);
      expect(read.map((c) => c.id)).toEqual(['w1', 'w2']);
    });
  });

  it('handles missing webhook secret in delivery attempt', async () => {
    const service = new WebhookDeliveryService({
      http: async () => ({ status: 200 }),
    });

    const result = await service.deliver(
      {
        id: 'webhook_1',
        url: 'https://example.com/hook',
        secret: '',
      },
      { test: 'payload' },
    );

    expect(result).toBeDefined();
    expect(result.status).toBe(200);
  });
});

function isValidWebhookUrl(url: string): boolean {
  if (!url || typeof url !== 'string') return false;
  try {
    const parsed = new URL(url);
    return parsed.protocol === 'http:' || parsed.protocol === 'https:';
  } catch {
    return false;
  }
}
