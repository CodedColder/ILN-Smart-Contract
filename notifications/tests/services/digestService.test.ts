import { describe, expect, it } from 'vitest';
import { DigestService, type DigestBatch, type DigestServiceOptions } from '../../src/services/digestService';
import { fakeClock } from '../helpers/clock';
import { makeLogger } from '../helpers/logger';
import { waitFor } from '../helpers/wait';

const WINDOW_MS = 5 * 60 * 1000;

function makeService(options: DigestServiceOptions = {}) {
  const clock = fakeClock();
  const { logger, entries } = makeLogger();
  const delivered: DigestBatch[] = [];
  const digest = new DigestService({
    now: clock.now,
    logger,
    deliver: (batch) => {
      delivered.push(batch);
    },
    ...options,
  });
  return { digest, clock, entries, delivered };
}

const notification = (invoiceId: number) => ({
  event: 'invoice.paid',
  invoiceId,
  payload: { amount: '100' },
});

describe('DigestService', () => {
  it('delivers in real time by default', () => {
    const { digest } = makeService();

    const result = digest.enqueue('ep_1', notification(1));

    expect(result.batched).toBe(false);
    expect(digest.mode('ep_1')).toBe('realtime');
    expect(digest.pending('ep_1')).toEqual([]);
    expect(digest.flushDue()).toEqual([]);
    expect(digest.list()).toEqual([]);
  });

  it('honours a digest default for subscribers that never chose', () => {
    const { digest } = makeService({ defaultMode: 'digest' });

    expect(digest.mode('ep_new')).toBe('digest');
    expect(digest.enqueue('ep_new', notification(1)).batched).toBe(true);
    expect(digest.pending('ep_new')).toHaveLength(1);
  });

  it('batches notifications for an opted-in subscriber over the window', () => {
    const { digest, clock } = makeService();
    digest.optIn('ep_1');

    expect(digest.mode('ep_1')).toBe('digest');
    expect(digest.enqueue('ep_1', notification(1)).batched).toBe(true);
    clock.advance(10);
    expect(digest.enqueue('ep_1', notification(2)).batched).toBe(true);

    const queued = digest.pending('ep_1');
    expect(queued.map((n) => n.invoiceId)).toEqual([1, 2]);
    expect(queued[0]?.queuedAt).toBe(clock.now() - 10);
    expect(digest.list()).toEqual([
      { subscriberId: 'ep_1', mode: 'digest', pending: 2, due: false },
    ]);

    // Not due until the window closes.
    const windowStart = clock.now() - 10;
    expect(digest.flushDue(windowStart + WINDOW_MS - 1)).toEqual([]);
    const batches = digest.flushDue(windowStart + WINDOW_MS);
    expect(batches).toHaveLength(1);
    expect(batches[0]?.notifications).toHaveLength(2);
    expect(batches[0]?.windowStart).toBe(windowStart);
    expect(digest.pending('ep_1')).toEqual([]);
    expect(digest.flushDue(windowStart + WINDOW_MS)).toEqual([]);
  });

  it('releases a batch as soon as it reaches maxBatchSize', () => {
    const { digest } = makeService({ windowMs: 600_000, maxBatchSize: 2 });
    digest.optIn('ep_1');

    digest.enqueue('ep_1', notification(1));
    expect(digest.flushDue()).toEqual([]);

    digest.enqueue('ep_1', notification(2));
    const batches = digest.flushDue();
    expect(batches).toHaveLength(1);
    expect(batches[0]?.notifications.map((n) => n.invoiceId)).toEqual([1, 2]);
    expect(digest.list()[0]).toMatchObject({ pending: 0, due: false });
  });

  it('sends an opted-out subscriber back to real time without dropping the queue', () => {
    const { digest, clock } = makeService();
    digest.optIn('ep_1');
    digest.enqueue('ep_1', notification(1));

    expect(digest.optOut('ep_1')).toBe('realtime');
    expect(digest.mode('ep_1')).toBe('realtime');
    expect(digest.enqueue('ep_1', notification(2)).batched).toBe(false);
    expect(digest.pending('ep_1')).toHaveLength(1);

    // What was already queued still ships at the end of its window.
    clock.advance(WINDOW_MS);
    const batches = digest.flushDue();
    expect(batches).toHaveLength(1);
    expect(batches[0]?.notifications.map((n) => n.invoiceId)).toEqual([1]);
  });

  it('tracks preferences it has been told about', () => {
    const { digest } = makeService();

    digest.setMode('ep_1', 'digest');
    digest.setMode('ep_1', 'realtime');
    digest.setMode('ep_2', 'digest');

    expect(digest.list()).toEqual([
      { subscriberId: 'ep_1', mode: 'realtime', pending: 0, due: false },
      { subscriberId: 'ep_2', mode: 'digest', pending: 0, due: false },
    ]);
    expect(digest.pending('unknown')).toEqual([]);
  });

  it('flushes due batches to the deliver sink on its timer', async () => {
    const { digest, clock, delivered } = makeService({
      windowMs: 20,
      flushIntervalMs: 5,
    });
    digest.optIn('ep_1');
    digest.enqueue('ep_1', notification(7));

    await digest.start();
    expect(digest.state).toBe('running');

    clock.advance(50);
    await waitFor(() => delivered.length === 1);
    expect(delivered[0]?.subscriberId).toBe('ep_1');
    expect(delivered[0]?.notifications).toHaveLength(1);

    await digest.stop();
    expect(digest.state).toBe('stopped');
  });

  it('keeps running when the sink throws', async () => {
    const { digest, clock, entries } = makeService({
      windowMs: 20,
      flushIntervalMs: 5,
      deliver: () => {
        throw new Error('downstream down');
      },
    });
    digest.optIn('ep_1');
    digest.enqueue('ep_1', notification(7));

    await digest.start();
    clock.advance(50);
    await waitFor(() => entries.some((e) => e.msg === 'service_operation_failed'));

    expect(digest.state).toBe('running');
    expect(entries.some((e) => e.fields.operation === 'digest_deliver')).toBe(true);

    await digest.stop();
  });

  it('starts without a timer when no deliver sink is configured', async () => {
    const { digest } = makeService({ deliver: undefined });
    digest.optIn('ep_1');
    digest.enqueue('ep_1', notification(1));

    await digest.start();
    expect(digest.state).toBe('running');
    await digest.stop();

    // Nothing to drain to, so the notification is still queued.
    expect(digest.pending('ep_1')).toHaveLength(1);
  });

  it('drains queued notifications on shutdown', async () => {
    const { digest, delivered } = makeService({ windowMs: 600_000 });
    digest.optIn('ep_1');
    digest.enqueue('ep_1', notification(1));
    digest.enqueue('ep_1', notification(2));

    await digest.start();
    await digest.stop();

    expect(delivered).toHaveLength(1);
    expect(delivered[0]?.notifications.map((n) => n.invoiceId)).toEqual([1, 2]);
    expect(digest.pending('ep_1')).toEqual([]);
  });
});
