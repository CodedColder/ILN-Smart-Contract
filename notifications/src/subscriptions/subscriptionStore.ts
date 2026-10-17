import Database from 'better-sqlite3';

export interface Subscription {
  id: string;
  endpointId: string;
  url: string;
  secret: string;
  eventTypes: string[];
  /**
   * Operational contact address (Issue #873). Health notices — flag,
   * suspension, reactivation — are sent here, because the webhook endpoint is
   * the channel that is failing. Nullable: subscriptions without one simply
   * get logged notices instead of email.
   */
  contactEmail: string | null;
  createdAt: number;
}

export interface SubscriptionInput {
  endpointId: string;
  url: string;
  secret: string;
  eventTypes: string[];
  contactEmail?: string | null | undefined;
}

export class SubscriptionStore {
  constructor(private readonly db: Database.Database = new Database(':memory:')) {
    this.initSchema();
  }

  private initSchema(): void {
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS subscriptions (
        id TEXT PRIMARY KEY,
        endpoint_id TEXT NOT NULL,
        url TEXT NOT NULL,
        secret TEXT NOT NULL,
        event_types TEXT NOT NULL,
        contact_email TEXT,
        created_at INTEGER NOT NULL
      );

      CREATE INDEX IF NOT EXISTS idx_subscriptions_endpoint
        ON subscriptions(endpoint_id);
    `);

    // Databases created before contact_email existed (Issue #873) get the
    // column added in place; the check keeps re-opening idempotent.
    const columns = this.db.pragma('table_info(subscriptions)') as Array<{ name: string }>;
    if (!columns.some((column) => column.name === 'contact_email')) {
      this.db.exec('ALTER TABLE subscriptions ADD COLUMN contact_email TEXT');
    }
  }

  create(input: SubscriptionInput): Subscription {
    const id = `sub_${Date.now().toString(36)}_${Math.random().toString(36).slice(2)}`;
    const now = Date.now();
    const contactEmail = input.contactEmail ?? null;
    const stmt = this.db.prepare(`
      INSERT INTO subscriptions (id, endpoint_id, url, secret, event_types, contact_email, created_at)
      VALUES (?, ?, ?, ?, ?, ?, ?)
    `);

    stmt.run(
      id,
      input.endpointId,
      input.url,
      input.secret,
      JSON.stringify(input.eventTypes),
      contactEmail,
      now,
    );

    return {
      id,
      endpointId: input.endpointId,
      url: input.url,
      secret: input.secret,
      eventTypes: input.eventTypes,
      contactEmail,
      createdAt: now,
    };
  }

  get(id: string): Subscription | undefined {
    const stmt = this.db.prepare(`
      SELECT id, endpoint_id, url, secret, event_types, contact_email, created_at
      FROM subscriptions
      WHERE id = ?
    `);

    const row = stmt.get(id) as any;
    return row ? this.rowToSub(row) : undefined;
  }

  /** Lookup by the delivery-layer endpoint id (services key on this). */
  getByEndpointId(endpointId: string): Subscription | undefined {
    const stmt = this.db.prepare(`
      SELECT id, endpoint_id, url, secret, event_types, contact_email, created_at
      FROM subscriptions
      WHERE endpoint_id = ?
      ORDER BY created_at DESC
      LIMIT 1
    `);

    const row = stmt.get(endpointId) as any;
    return row ? this.rowToSub(row) : undefined;
  }

  list(): Subscription[] {
    const stmt = this.db.prepare(`
      SELECT id, endpoint_id, url, secret, event_types, contact_email, created_at
      FROM subscriptions
      ORDER BY created_at DESC
    `);

    const rows = stmt.all() as any[];
    return rows.map((r) => this.rowToSub(r));
  }

  update(id: string, patch: Partial<SubscriptionInput>): Subscription | undefined {
    const sub = this.get(id);
    if (!sub) return undefined;

    const updated: Subscription = {
      ...sub,
      ...patch,
      contactEmail: patch.contactEmail !== undefined ? patch.contactEmail : sub.contactEmail,
    };

    const stmt = this.db.prepare(`
      UPDATE subscriptions
      SET endpoint_id = ?, url = ?, secret = ?, event_types = ?, contact_email = ?
      WHERE id = ?
    `);

    stmt.run(
      updated.endpointId,
      updated.url,
      updated.secret,
      JSON.stringify(updated.eventTypes),
      updated.contactEmail,
      id,
    );

    return updated;
  }

  delete(id: string): boolean {
    const stmt = this.db.prepare(`DELETE FROM subscriptions WHERE id = ?`);
    const result = stmt.run(id);
    return (result.changes ?? 0) > 0;
  }

  private rowToSub(row: any): Subscription {
    return {
      id: row.id,
      endpointId: row.endpoint_id,
      url: row.url,
      secret: row.secret,
      eventTypes: JSON.parse(row.event_types),
      contactEmail: row.contact_email ?? null,
      createdAt: row.created_at,
    };
  }
}
