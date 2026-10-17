import { describe, expect, it } from 'vitest';
import Database from 'better-sqlite3';
import { SubscriptionStore } from '../src/subscriptions/subscriptionStore';

describe('SubscriptionStore', () => {
  it('creates and lists subscriptions', () => {
    const s = new SubscriptionStore();
    const a = s.create({ endpointId: 'e1', url: 'https://x', secret: 'k', eventTypes: ['A'] });
    const b = s.create({ endpointId: 'e2', url: 'https://y', secret: 'k', eventTypes: ['B'] });
    expect(s.list()).toHaveLength(2);
    expect(s.get(a.id)).toEqual(a);
    expect(s.get(b.id)?.eventTypes).toEqual(['B']);
  });

  it('updates fields when provided', () => {
    const s = new SubscriptionStore();
    const sub = s.create({ endpointId: 'e1', url: 'https://x', secret: 'k', eventTypes: ['A'] });
    const updated = s.update(sub.id, { url: 'https://z', eventTypes: ['A', 'B'] });
    expect(updated?.url).toBe('https://z');
    expect(updated?.eventTypes).toEqual(['A', 'B']);
  });

  it('returns undefined when updating a missing subscription', () => {
    const s = new SubscriptionStore();
    expect(s.update('nope', { url: 'x' })).toBeUndefined();
  });

  it('stores contact emails and looks subscriptions up by endpoint id', () => {
    const s = new SubscriptionStore();
    const withContact = s.create({
      endpointId: 'e1',
      url: 'https://x',
      secret: 'k',
      eventTypes: ['A'],
      contactEmail: 'ops@example.com',
    });
    const withoutContact = s.create({
      endpointId: 'e2',
      url: 'https://y',
      secret: 'k',
      eventTypes: ['B'],
    });

    expect(withContact.contactEmail).toBe('ops@example.com');
    expect(withoutContact.contactEmail).toBeNull();
    expect(s.getByEndpointId('e1')?.id).toBe(withContact.id);
    expect(s.getByEndpointId('nope')).toBeUndefined();

    const updated = s.update(withContact.id, { contactEmail: null });
    expect(updated?.contactEmail).toBeNull();
    expect(s.get(withContact.id)?.contactEmail).toBeNull();
  });

  it('adds the contact_email column to databases created before it existed', () => {
    const db = new Database(':memory:');
    db.exec(`
      CREATE TABLE subscriptions (
        id TEXT PRIMARY KEY,
        endpoint_id TEXT NOT NULL,
        url TEXT NOT NULL,
        secret TEXT NOT NULL,
        event_types TEXT NOT NULL,
        created_at INTEGER NOT NULL
      );
    `);
    db.prepare(
      `INSERT INTO subscriptions (id, endpoint_id, url, secret, event_types, created_at)
       VALUES ('sub_legacy', 'e1', 'https://x', 'k', '["A"]', 1)`,
    ).run();

    const s = new SubscriptionStore(db);
    expect(s.getByEndpointId('e1')?.contactEmail).toBeNull();

    const created = s.create({
      endpointId: 'e2',
      url: 'https://y',
      secret: 'k',
      eventTypes: ['B'],
      contactEmail: 'ops@example.com',
    });
    expect(created.contactEmail).toBe('ops@example.com');
    db.close();
  });

  it('deletes subscriptions', () => {
    const s = new SubscriptionStore();
    const sub = s.create({ endpointId: 'e1', url: 'https://x', secret: 'k', eventTypes: ['A'] });
    expect(s.delete(sub.id)).toBe(true);
    expect(s.delete(sub.id)).toBe(false);
    expect(s.get(sub.id)).toBeUndefined();
  });
});
