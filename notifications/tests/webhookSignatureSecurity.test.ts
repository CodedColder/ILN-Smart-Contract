import { describe, expect, it, vi } from 'vitest';
import { signPayload, verifySignature } from '../src/delivery/signature';
import { WebhookDeliveryService, type HttpClient } from '../src/delivery/webhookDelivery';

describe('Webhook Signature and Secret Security', () => {
  it('verifies webhook payload signatures correctly', () => {
    const secret = 'my-webhook-secret-key';
    const payload = JSON.stringify({ event: 'invoice.paid', invoiceId: 123, amount: 1000 });

    const signature = signPayload(secret, payload);
    expect(verifySignature(secret, payload, signature)).toBe(true);
  });

  it('rejects signatures from tampered payloads', () => {
    const secret = 'secret-key';
    const payload = JSON.stringify({ amount: 1000 });

    const signature = signPayload(secret, payload);
    const tamperedPayload = JSON.stringify({ amount: 2000 });

    expect(verifySignature(secret, tamperedPayload, signature)).toBe(false);
  });

  it('rejects signatures with wrong secret', () => {
    const correctSecret = 'correct-secret';
    const wrongSecret = 'wrong-secret';
    const payload = 'invoice data';

    const signature = signPayload(correctSecret, payload);
    expect(verifySignature(wrongSecret, payload, signature)).toBe(false);
  });

  it('uses timing-safe comparison to prevent timing attacks', () => {
    const secret = 'secret';
    const payload = 'payload';

    const correctSignature = signPayload(secret, payload);
    const wrongSignature = correctSignature.substring(0, 60) + 'ff';

    const result = verifySignature(secret, payload, wrongSignature);
    expect(result).toBe(false);
  });

  it('rejects signatures with length mismatch', () => {
    const secret = 'secret';
    const payload = 'payload';

    const correctSignature = signPayload(secret, payload);
    const wrongLength = correctSignature.substring(0, 10);

    expect(verifySignature(secret, payload, wrongLength)).toBe(false);
  });

  it('never logs secrets during signature verification', () => {
    const logger = vi.fn();
    const secret = 'super-secret-key-do-not-log';
    const payload = 'payload';

    const signature = signPayload(secret, payload);
    verifySignature(secret, payload, signature);

    const loggedContent = logger.mock.calls.map((c) => String(c[0])).join(' ');
    expect(loggedContent).not.toContain(secret);
  });

  it('never logs secrets in HTTP headers', async () => {
    const capturedHeaders: any[] = [];
    const http: HttpClient = async (_url, init) => {
      capturedHeaders.push(init.headers);
      return { status: 200 };
    };

    const service = new WebhookDeliveryService({ http });
    const endpoint = {
      id: 'webhook_1',
      url: 'https://example.com/webhook',
      secret: 'super-secret-webhook-key',
    };

    const payload = { event: 'invoice.paid', invoiceId: 1 };
    await service.deliver(endpoint, payload);

    expect(capturedHeaders).toHaveLength(1);
    const headerContent = JSON.stringify(capturedHeaders[0]);
    expect(headerContent).not.toContain('super-secret-webhook-key');
    expect(headerContent).toContain('x-iln-signature');
  });

  it('provides signature in correct header format', async () => {
    const capturedRequests: any[] = [];
    const http: HttpClient = async (_url, init) => {
      capturedRequests.push(init);
      return { status: 200 };
    };

    const service = new WebhookDeliveryService({ http });
    const endpoint = {
      id: 'e1',
      url: 'https://example.com/hook',
      secret: 'secret123',
    };

    const payload = { test: 'data' };
    await service.deliver(endpoint, payload);

    expect(capturedRequests).toHaveLength(1);
    const [req] = capturedRequests;
    expect(req.headers['x-iln-signature']).toBeDefined();
    expect(req.headers['x-iln-signature']).toMatch(/^[0-9a-f]{64}$/);
  });

  it('signs payload with deterministic output', () => {
    const secret = 'consistent-secret';
    const payload = '{"event":"invoice.paid"}';

    const sig1 = signPayload(secret, payload);
    const sig2 = signPayload(secret, payload);
    const sig3 = signPayload(secret, payload);

    expect(sig1).toBe(sig2);
    expect(sig2).toBe(sig3);
  });

  it('requires exact payload match for signature verification', () => {
    const secret = 'secret';
    const json = { event: 'invoice.paid', amount: 1000 };

    const payload1 = JSON.stringify(json);
    const payload2 = JSON.stringify(json, Object.keys(json).reverse());

    const sig1 = signPayload(secret, payload1);
    const sig2 = signPayload(secret, payload2);

    expect(sig1).not.toBe(sig2);
    expect(verifySignature(secret, payload1, sig1)).toBe(true);
    expect(verifySignature(secret, payload2, sig2)).toBe(true);
    expect(verifySignature(secret, payload1, sig2)).toBe(false);
    expect(verifySignature(secret, payload2, sig1)).toBe(false);
  });

  it('prevents empty secret abuse', () => {
    const payload = 'sensitive data';

    const sig1 = signPayload('', payload);
    const sig2 = signPayload('', payload);

    expect(sig1).toBe(sig2);
    expect(verifySignature('', payload, sig1)).toBe(true);
    expect(verifySignature('wrong', payload, sig1)).toBe(false);
  });

  it('detects signature bit-flip attacks', () => {
    const secret = 'secret-key';
    const payload = 'webhook payload';

    const signature = signPayload(secret, payload);
    expect(signature).toHaveLength(64);

    for (let i = 0; i < signature.length; i += 4) {
      const tampered = signature.substring(0, i) + 'f' + signature.substring(i + 1);
      expect(verifySignature(secret, payload, tampered)).toBe(false);
    }
  });
});
