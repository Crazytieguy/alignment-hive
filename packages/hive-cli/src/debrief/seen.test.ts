import { describe, expect, test } from 'bun:test';
import { createSeenStore } from './seen';
import type { SeenApi, SeenEnv } from './seen';

const ID = 'review-1';
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

function storage(initial: Record<string, string> = {}) {
  const values = new Map(Object.entries(initial));
  return { values, getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
}
/** Timers the test fires by hand, so coalescing is visible. */
function timers() {
  let next = 1;
  const pending = new Map<number, () => void>();
  return {
    pending,
    setTimeout: (fn: () => void) => { const id = next++; pending.set(id, fn); return id; },
    clearTimeout: (id: unknown) => { pending.delete(id as number); },
    run: () => { const due = [...pending.values()]; pending.clear(); due.forEach((fn) => fn()); },
  };
}
type Snap = { exists: boolean; data: () => Record<string, unknown> | undefined; metadata: { hasPendingWrites: boolean } };
/** One artifact database shared by every "device"; `fail` makes the next writes reject with a code. */
function database(initial?: Record<string, unknown>) {
  const docs = new Map<string, Record<string, unknown>>(initial ? [['seen/viewer', initial]] : []);
  const listeners = new Map<string, Array<(snap: Snap) => void>>();
  const writes: Array<Record<string, unknown>> = [];
  const failures: Array<string> = [];
  const snap = (path: string): Snap => ({ exists: docs.has(path), data: () => docs.get(path), metadata: { hasPendingWrites: false } });
  const db = {
    doc: (path: string) => ({
      set: (data: Record<string, unknown>) => {
        const code = failures.shift();
        if (code) return Promise.reject({ code, message: code });
        writes.push(data); docs.set(path, data);
        return Promise.resolve().then(() => (listeners.get(path) ?? []).forEach((fn) => fn(snap(path))));
      },
      onSnapshot: (next: (snap: Snap) => void) => {
        listeners.set(path, [...(listeners.get(path) ?? []), next]);
        void Promise.resolve().then(() => next(snap(path)));
        return () => {};
      },
    }),
  };
  return { db, docs, writes, failures };
}
function env(options: { hashes?: Record<string, string | null>; local?: ReturnType<typeof storage>; db?: ReturnType<typeof database>['db'] | null; clock?: ReturnType<typeof timers> } = {}): SeenEnv & { clock: ReturnType<typeof timers>; local: ReturnType<typeof storage> } {
  const clock = options.clock ?? timers(), local = options.local ?? storage();
  const hashes = options.hashes ?? { a: 'A', b: 'B' };
  const items = Object.fromEntries(Object.entries(hashes).map(([id, hash]) => [id, { hash, heading: `Heading ${id}` }]));
  const claude = options.db === undefined ? undefined : { use: (name: string) => Promise.resolve(name === 'db' ? options.db : name === 'user' ? { id: () => Promise.resolve('viewer') } : null) };
  return { reviewId: ID, round: 2, items, storage: local, claude, now: () => '2026-09-30T00:00:00.000Z', setTimeout: clock.setTimeout, clearTimeout: clock.clearTimeout, random: () => 0, clock, local };
}
const cached = (local: ReturnType<typeof storage>) => JSON.parse(local.values.get(`review-seen:${ID}`)!) as { items: Record<string, { hash: string }>; visit?: unknown; migrated?: true };
const visit = (hashes: Record<string, string>, round = 2) => ({ reviewId: ID, highestRound: round, itemHashes: hashes, savedAt: 'then' });

describe('seen marks without a database', () => {
  test('a mark records the item hash, survives a reload, and an item changed since is unseen and stale', () => {
    const first = env();
    const store = createSeenStore(first);
    store.setSeen('a', true);
    expect([store.isSeen('a'), store.isSeen('b'), store.isStale('a')]).toEqual([true, false, false]);
    expect(cached(first.local).items.a).toMatchObject({ hash: 'A', round: 2, heading: 'Heading a' });
    expect(createSeenStore(env({ local: first.local })).isSeen('a')).toBe(true);
    const changed = createSeenStore(env({ local: first.local, hashes: { a: 'A2', b: 'B' } }));
    expect([changed.isSeen('a'), changed.isStale('a')]).toEqual([false, true]);
    changed.setSeen('a', true);
    expect([changed.isSeen('a'), changed.isStale('a')]).toEqual([true, false]);
    changed.setSeen('a', false);
    expect([changed.isSeen('a'), changed.isStale('a')]).toEqual([false, false]);
  });

  test('a page without a manifest marks by id alone', () => {
    const store = createSeenStore(env({ hashes: { a: null } }));
    store.setSeen('a', true);
    expect([store.isSeen('a'), store.isStale('a')]).toEqual([true, false]);
  });

  test('the earlier local keys become the first document: review-ui seen flags and the review: visit', () => {
    const local = storage({ [`review-ui:${ID}`]: JSON.stringify({ theme: 'dark', seen: { a: true, b: 'yes' } }), [`review:${ID}`]: JSON.stringify(visit({ a: 'A' }, 1)) });
    const store = createSeenStore(env({ local }));
    expect([store.isSeen('a'), store.isSeen('b')]).toEqual([true, false]);
    expect(store.visit()).toMatchObject({ highestRound: 1 });
  });

  test('malformed or foreign documents and marks are ignored', () => {
    for (const value of ['{bad', 'null', '[]', JSON.stringify({ review: 'other', items: { a: { hash: 'A', round: 1, heading: '', at: '' } } })]) {
      expect(createSeenStore(env({ local: storage({ [`review-seen:${ID}`]: value }) })).isSeen('a')).toBe(false);
    }
    const mixed = JSON.stringify({ review: ID, items: { a: { hash: 'A', round: 1, heading: 'x', at: 't' }, b: { hash: 5 }, c: true }, visit: { reviewId: 'other' } });
    const store = createSeenStore(env({ local: storage({ [`review-seen:${ID}`]: mixed }) }));
    expect([store.isSeen('a'), store.isSeen('b'), store.visit()]).toEqual([true, false, null]);
  });

  test('denied storage keeps marks for the visit and never throws', () => {
    const denied = () => { throw new Error('denied'); };
    const store = createSeenStore({ ...env(), storage: { getItem: denied, setItem: denied } });
    store.setSeen('a', true);
    expect(store.isSeen('a')).toBe(true);
  });

  test('use() resolving null leaves the store local, with no writes attempted', async () => {
    const e = env({ db: null });
    const store = createSeenStore(e);
    await settle();
    store.setSeen('a', true); e.clock.run(); await settle();
    expect(store.isSeen('a')).toBe(true);
  });
});

describe('seen marks in the artifact database', () => {
  test('an empty database takes the local marks once, flagged as migrated', async () => {
    const local = storage({ [`review-ui:${ID}`]: JSON.stringify({ seen: { a: true } }) });
    const { db, writes } = database();
    const e = env({ local, db });
    createSeenStore(e);
    await settle(); e.clock.run(); await settle();
    expect(writes).toHaveLength(1);
    expect(writes[0]).toMatchObject({ review: ID, migrated: true, items: { a: { hash: 'A' } } });
  });

  test('an empty database and no local state write nothing', async () => {
    const { db, writes } = database();
    const e = env({ db });
    createSeenStore(e);
    await settle(); e.clock.run(); await settle();
    expect(writes).toEqual([]);
  });

  test('the database document is the truth: marks and visit replace local ones, and the cache mirrors it', async () => {
    const local = storage({ [`review-ui:${ID}`]: JSON.stringify({ seen: { b: true } }) });
    const remote = { review: ID, items: { a: { hash: 'A', round: 1, heading: 'h', at: 't' } }, visit: visit({ a: 'A' }, 1) };
    const { db, writes } = database(remote);
    const e = env({ local, db });
    const store = createSeenStore(e);
    const connected: Array<boolean> = []; let changes = 0;
    store.onConnect((remoteDoc) => connected.push(remoteDoc)); store.onChange(() => { changes++; });
    expect([store.isSeen('a'), store.isSeen('b')]).toEqual([false, true]);
    await settle();
    expect([store.isSeen('a'), store.isSeen('b'), connected, changes]).toEqual([true, false, [true], 1]);
    expect(cached(local).items).toEqual(remote.items);
    e.clock.run(); await settle();
    expect(writes).toEqual([]);
  });

  test('a tick made before the database answers is kept on top of its document', async () => {
    const { db, writes } = database({ review: ID, items: { a: { hash: 'A', round: 1, heading: 'h', at: 't' } } });
    const e = env({ db });
    const store = createSeenStore(e);
    store.setSeen('b', true);
    await settle(); e.clock.run(); await settle();
    expect([store.isSeen('a'), store.isSeen('b')]).toEqual([true, true]);
    expect(Object.keys((writes.at(-1) as { items: object }).items).sort()).toEqual(['a', 'b']);
  });

  test('rapid toggles coalesce into one write; a change during a write gets one more', async () => {
    const { db, writes } = database({ review: ID, items: {} });
    const e = env({ db });
    const store = createSeenStore(e);
    await settle();
    store.setSeen('a', true); store.setSeen('b', true); store.setSeen('a', false);
    expect(e.clock.pending.size).toBe(1);
    e.clock.run();
    store.setSeen('a', true);
    e.clock.run();
    await settle(); e.clock.run(); await settle();
    expect(writes.map((w) => Object.keys((w as { items: object }).items).sort())).toEqual([['b'], ['a', 'b']]);
  });

  test('the visit is written only when it changes', async () => {
    const { db, writes } = database({ review: ID, items: {}, visit: visit({ a: 'A' }) });
    const e = env({ db });
    const store = createSeenStore(e);
    await settle();
    store.setVisit(visit({ a: 'A' })); e.clock.run(); await settle();
    expect(writes).toHaveLength(0);
    store.setVisit(visit({ a: 'A2' })); e.clock.run(); await settle();
    expect(writes).toHaveLength(1);
  });

  test('a refused write (view-only) stops writing; unavailable retries once', async () => {
    const refused = database({ review: ID, items: {} });
    const e = env({ db: refused.db });
    const store = createSeenStore(e);
    await settle();
    refused.failures.push('invalid_argument');
    store.setSeen('a', true); e.clock.run(); await settle();
    store.setSeen('b', true); e.clock.run(); await settle();
    expect([refused.writes.length, store.isSeen('a'), store.isSeen('b')]).toEqual([0, true, true]);

    const flaky = database({ review: ID, items: {} });
    const f = env({ db: flaky.db });
    createSeenStore(f).setSeen('a', true);
    await settle();
    flaky.failures.push('unavailable');
    f.clock.run(); await settle();
    expect(f.clock.pending.size).toBe(1);
    f.clock.run(); await settle();
    expect(flaky.writes).toHaveLength(1);
  });

  test('a second device with a fresh browser sees the marks, and a later change arrives live', async () => {
    const shared = database();
    const one = env({ db: shared.db });
    const first = createSeenStore(one);
    await settle();
    first.setSeen('a', true); one.clock.run(); await settle();
    const two = env({ db: shared.db });
    const second: SeenApi = createSeenStore(two);
    expect(second.isSeen('a')).toBe(false);
    await settle();
    expect(second.isSeen('a')).toBe(true);
    let changed = false; second.onChange(() => { changed = true; });
    first.setSeen('b', true); one.clock.run(); await settle();
    expect([changed, second.isSeen('b')]).toEqual([true, true]);
  });
});
