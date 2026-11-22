import { afterEach, describe, expect, it, vi } from 'vitest';
import Database from 'better-sqlite3';
import { RetryQueue } from '../src/queue/retryQueue';

describe('RetryQueue - Transport Failure and Dead-Letter Behavior', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('handles transport failure with exponential backoff', () => {
    let now = 1_700_000_000_000;
    vi.spyOn(Date, 'now').mockImplementation(() => now);

    const db = new Database(':memory:');
    db.exec('CREATE TABLE subscriptions (id TEXT PRIMARY KEY)');
    db.exec("INSERT INTO subscriptions (id) VALUES ('webhook_1')");
    const queue = new RetryQueue(db);

    const log = queue.enqueue('webhook_1', 'invoice.funded', 42, { amount: '1000' });
    expect(log.status).toBe('pending');

    queue.recordFailure(log.id, 'Connection timeout');
    let updated = queue.get(log.id)!;
    expect(updated.status).toBe('pending');
    expect(updated.attempts).toBe(1);
    expect(updated.nextRetryAt).toBe(now + 1000);
    expect(updated.lastError).toBe('Connection timeout');

    now += 1000;
    queue.recordFailure(log.id, 'Connection timeout (retry 2)');
    updated = queue.get(log.id)!;
    expect(updated.attempts).toBe(2);
    expect(updated.nextRetryAt).toBe(now + 5000);

    now += 5000;
    queue.recordFailure(log.id, 'Connection timeout (final)');
    updated = queue.get(log.id)!;
    expect(updated.status).toBe('failed');
    expect(updated.attempts).toBe(3);
    expect(updated.nextRetryAt).toBe(now);

    db.close();
  });

  it('exhausts retries and moves to dead-letter state', () => {
    let now = 1_700_000_000_000;
    vi.spyOn(Date, 'now').mockImplementation(() => now);

    const db = new Database(':memory:');
    db.exec('CREATE TABLE subscriptions (id TEXT PRIMARY KEY)');
    db.exec("INSERT INTO subscriptions (id) VALUES ('webhook_1')");
    const queue = new RetryQueue(db);

    const log = queue.enqueue('webhook_1', 'invoice.paid', 1, {});
    queue.recordFailure(log.id, 'HTTP 503');
    queue.recordFailure(log.id, 'HTTP 503');
    queue.recordFailure(log.id, 'HTTP 503');

    const failed = queue.get(log.id)!;
    expect(failed.status).toBe('failed');
    expect(failed.attempts).toBe(3);
    expect(failed.maxAttempts).toBe(3);
    expect(failed.lastError).toBe('HTTP 503');

    db.close();
  });

  it('persists delivery failure history for audit', () => {
    const db = new Database(':memory:');
    db.exec('CREATE TABLE subscriptions (id TEXT PRIMARY KEY)');
    db.exec("INSERT INTO subscriptions (id) VALUES ('webhook_1')");
    const queue = new RetryQueue(db);

    const log = queue.enqueue('webhook_1', 'invoice.draft', 5, { data: 'test' });
    queue.recordFailure(log.id, 'Network unreachable');

    const retrieved = queue.get(log.id)!;
    expect(retrieved.payload).toBe(JSON.stringify({ data: 'test' }));
    expect(retrieved.lastError).toBe('Network unreachable');
    expect(retrieved.createdAt).toBeLessThanOrEqual(retrieved.updatedAt);

    db.close();
  });

  it('maintains pending queue for retryable failures', () => {
    let now = 1_700_000_000_000;
    vi.spyOn(Date, 'now').mockImplementation(() => now);

    const db = new Database(':memory:');
    db.exec('CREATE TABLE subscriptions (id TEXT PRIMARY KEY)');
    db.exec("INSERT INTO subscriptions (id) VALUES ('webhook_1')");
    const queue = new RetryQueue(db);

    const log1 = queue.enqueue('webhook_1', 'invoice.paid', 1, {});
    const log2 = queue.enqueue('webhook_1', 'invoice.draft', 2, {});
    const log3 = queue.enqueue('webhook_1', 'invoice.funded', 3, {});

    queue.recordFailure(log1.id, 'timeout');
    queue.recordSuccess(log2.id);
    queue.recordFailure(log3.id, 'timeout');

    const pending = queue.getPending();
    expect(pending).toHaveLength(2);
    expect(pending.map((p) => p.id)).toContain(log1.id);
    expect(pending.map((p) => p.id)).toContain(log3.id);
    expect(pending.map((p) => p.id)).not.toContain(log2.id);

    db.close();
  });

  it('prevents retry storm when backend becomes temporarily unavailable', () => {
    let now = 1_700_000_000_000;
    vi.spyOn(Date, 'now').mockImplementation(() => now);

    const db = new Database(':memory:');
    db.exec('CREATE TABLE subscriptions (id TEXT PRIMARY KEY)');
    db.exec("INSERT INTO subscriptions (id) VALUES ('webhook_1')");
    const queue = new RetryQueue(db);

    const logs = Array.from({ length: 100 }, (_, i) =>
      queue.enqueue('webhook_1', 'invoice.funded', i, { index: i }),
    );

    logs.forEach((log) => queue.recordFailure(log.id, 'Service unavailable'));

    const pending = queue.getPending(50);
    expect(pending).toHaveLength(50);
    expect(pending[0].nextRetryAt).toBe(now + 1000);

    now += 2000;
    const readyForRetry = queue.getPending(100);
    expect(readyForRetry).toHaveLength(100);

    db.close();
  });

  it('recovers from resumable delivery after client reconnection', () => {
    let now = 1_700_000_000_000;
    vi.spyOn(Date, 'now').mockImplementation(() => now);

    const db = new Database(':memory:');
    db.exec('CREATE TABLE subscriptions (id TEXT PRIMARY KEY)');
    db.exec("INSERT INTO subscriptions (id) VALUES ('webhook_1')");
    const queue = new RetryQueue(db);

    const log = queue.enqueue('webhook_1', 'invoice.paid', 42, { amount: '500' });
    queue.recordFailure(log.id, 'Connection reset by peer');

    let updated = queue.get(log.id)!;
    expect(updated.status).toBe('pending');
    expect(updated.attempts).toBe(1);

    now += 1500;
    expect(queue.getPending()).toHaveLength(1);

    queue.recordSuccess(log.id);
    updated = queue.get(log.id)!;
    expect(updated.status).toBe('delivered');
    expect(updated.attempts).toBe(1);

    db.close();
  });

  it('distinguishes between transient and permanent failures', () => {
    const db = new Database(':memory:');
    db.exec('CREATE TABLE subscriptions (id TEXT PRIMARY KEY)');
    db.exec("INSERT INTO subscriptions (id) VALUES ('webhook_1')");
    const queue = new RetryQueue(db);

    const transient = queue.enqueue('webhook_1', 'invoice.paid', 1, {});
    queue.recordFailure(transient.id, 'HTTP 503 Service Unavailable');

    const permanent = queue.enqueue('webhook_1', 'invoice.paid', 2, {});
    queue.recordFailure(permanent.id, 'HTTP 400 Bad Request');

    const transientLog = queue.get(transient.id)!;
    expect(transientLog.status).toBe('pending');
    expect(transientLog.lastError).toContain('503');

    const permanentLog = queue.get(permanent.id)!;
    expect(permanentLog.status).toBe('pending');
    expect(permanentLog.lastError).toContain('400');

    db.close();
  });
});
