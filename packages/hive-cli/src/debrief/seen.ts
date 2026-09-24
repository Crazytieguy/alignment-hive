import type { SeenStore } from './rounds';

/** A seen mark records the item's hash when it was ticked: a later change to the item makes it unseen again. */
export interface SeenMark { hash: string | null; round: number | null; heading: string; at: string }
/** One document per viewer, `seen/<viewer id>` in the artifact database, mirrored in localStorage. */
export interface SeenDoc { review: string; items: Record<string, SeenMark>; visit?: SeenStore; migrated?: true }

type Listener = () => void;
interface DocRef {
  set: (data: Record<string, unknown>) => Promise<void>;
  onSnapshot: (next: (snap: { exists: boolean; data: () => Record<string, unknown> | undefined; metadata?: { hasPendingWrites?: boolean } }) => void, error?: (e: unknown) => void) => () => void;
}
export interface SeenEnv {
  reviewId: string;
  round: number | null;
  items: Record<string, { hash: string | null; heading: string }>;
  storage: Pick<Storage, 'getItem' | 'setItem'> | null;
  claude?: { use: (name: string) => Promise<unknown> };
  now: () => string;
  setTimeout: (fn: () => void, ms: number) => unknown;
  clearTimeout: (handle: unknown) => void;
  random: () => number;
}
export interface SeenApi {
  isSeen: (id: string) => boolean;
  /** Seen once, changed since: the page shows it as updated and unseen. */
  isStale: (id: string) => boolean;
  setSeen: (id: string, on: boolean) => void;
  visit: () => SeenStore | null;
  /** Records this visit; writes only when it differs from the stored one. */
  setVisit: (next: SeenStore) => void;
  /** After the database's copy replaced the local one. */
  onChange: (fn: Listener) => void;
  /** Once, when the database is reached; `remote` is true when it held this viewer's document. */
  onConnect: (fn: (remote: boolean) => void) => void;
}

/**
 * Seen marks and the last visit, per viewer. The first paint reads localStorage; when the page is served with the
 * `db` and `user` capabilities, the viewer's `seen/<id>` document becomes the truth and every change is written
 * back, coalesced. Without them, or after a refused write, it stays local. Self-contained, so its source is inlined
 * into the page and the same function runs in tests.
 */
export function createSeenStore(env: SeenEnv): SeenApi {
  const LOCAL = 'review-seen:' + env.reviewId;
  const isRecord = (v: unknown): v is Record<string, unknown> => v !== null && typeof v === 'object' && !Array.isArray(v);
  const readJson = (key: string): unknown => {
    try { const raw = env.storage ? env.storage.getItem(key) : null; return typeof raw === 'string' ? JSON.parse(raw) : null; } catch { return null; }
  };
  const hashOf = (id: string) => (Object.hasOwn(env.items, id) ? env.items[id].hash : null);
  function validVisit(v: unknown): SeenStore | undefined {
    if (!isRecord(v) || v.reviewId !== env.reviewId || !Number.isSafeInteger(v.highestRound) || typeof v.savedAt !== 'string' || !isRecord(v.itemHashes)) return undefined;
    return Object.values(v.itemHashes).every((h) => typeof h === 'string') ? v as unknown as SeenStore : undefined;
  }
  /** A stored document of this review, keeping only well-formed marks; anything else is no document. */
  function valid(v: unknown): SeenDoc | null {
    if (!isRecord(v) || v.review !== env.reviewId || !isRecord(v.items)) return null;
    const items: Record<string, SeenMark> = {};
    for (const [id, m] of Object.entries(v.items)) {
      if (isRecord(m) && (typeof m.hash === 'string' || m.hash === null) && (typeof m.round === 'number' || m.round === null) && typeof m.heading === 'string' && typeof m.at === 'string') {
        items[id] = { hash: m.hash, round: m.round, heading: m.heading, at: m.at };
      }
    }
    const doc: SeenDoc = { review: env.reviewId, items };
    const visit = validVisit(v.visit);
    if (visit) doc.visit = visit;
    if (v.migrated === true) doc.migrated = true;
    return doc;
  }
  function markFor(id: string): SeenMark {
    return { hash: hashOf(id), round: env.round, heading: Object.hasOwn(env.items, id) ? env.items[id].heading : '', at: env.now() };
  }
  /** Before this store, marks were `{id: true}` in review-ui:<id> and the visit was review:<id>. */
  function legacy(): SeenDoc {
    const doc: SeenDoc = { review: env.reviewId, items: {} };
    const ui = readJson('review-ui:' + env.reviewId);
    if (isRecord(ui) && isRecord(ui.seen)) for (const id of Object.keys(ui.seen)) if (ui.seen[id] === true) doc.items[id] = markFor(id);
    const visit = validVisit(readJson('review:' + env.reviewId));
    if (visit) doc.visit = visit;
    return doc;
  }
  let doc: SeenDoc = valid(readJson(LOCAL)) || legacy();
  const saveLocal = () => { try { if (env.storage) env.storage.setItem(LOCAL, JSON.stringify(doc)); } catch { /* storage unavailable */ } };

  const changeListeners: Array<Listener> = [], connectListeners: Array<(remote: boolean) => void> = [];
  let ref: DocRef | null = null, connected = false, timer: unknown = null, inFlight = false, again = false, retried = false;
  /** Marks changed before the database answered, re-applied over its copy. */
  const pending: Record<string, SeenMark | null> = {};

  function write(): void {
    timer = null;
    if (!ref || !connected) return;
    if (inFlight) { again = true; return; }
    inFlight = true;
    const target = ref;
    target.set(JSON.parse(JSON.stringify(doc))).then(() => {
      inFlight = false; retried = false;
      if (again) { again = false; schedule(); }
    }, (error: unknown) => {
      inFlight = false;
      const code = isRecord(error) ? error.code : undefined;
      if (code === 'unavailable' && !retried) { retried = true; timer = env.setTimeout(write, 200 + Math.floor(env.random() * 800)); return; }
      // A view-only viewer, a revoked grant or a full store: seen marks are a convenience, so stay local.
      ref = null;
    });
  }
  function schedule(): void {
    if (timer !== null) env.clearTimeout(timer);
    timer = env.setTimeout(write, 400);
  }

  function adopt(remote: SeenDoc | null): void {
    if (remote) {
      doc = remote;
      for (const id of Object.keys(pending)) { const m = pending[id]; if (m) doc.items[id] = m; else delete doc.items[id]; }
    } else if (Object.keys(doc.items).length || doc.visit) doc.migrated = true;
    saveLocal();
    if (!remote ? (Object.keys(doc.items).length || doc.visit) : Object.keys(pending).length) schedule();
  }

  function connect(): void {
    const claude = env.claude;
    if (!claude || typeof claude.use !== 'function') return;
    Promise.all([claude.use('db'), claude.use('user')]).then(([db, user]) => {
      if (!isRecord(db) || !isRecord(user) || typeof db.doc !== 'function' || typeof user.id !== 'function') return null;
      return (user.id as () => Promise<unknown>)().then((uid) => {
        if (typeof uid !== 'string' || !uid) return;
        const target = (db.doc as (path: string) => DocRef)('seen/' + uid);
        ref = target;
        target.onSnapshot((snap) => {
          const remote = valid(snap.exists ? snap.data() : undefined);
          if (!connected) {
            connected = true;
            adopt(remote);
            connectListeners.forEach((fn) => fn(!!remote));
            changeListeners.forEach((fn) => fn());
            return;
          }
          // Another tab or device; this view's own pending write wins when it lands.
          if (!remote || timer !== null || inFlight || (snap.metadata && snap.metadata.hasPendingWrites)) return;
          if (JSON.stringify(remote) === JSON.stringify(doc)) return;
          doc = remote;
          saveLocal();
          changeListeners.forEach((fn) => fn());
        }, () => { ref = null; });
      });
    }).catch(() => { ref = null; });
  }
  connect();

  return {
    isSeen(id) {
      if (!Object.hasOwn(doc.items, id)) return false;
      const m = doc.items[id], h = hashOf(id);
      return h === null || m.hash === null || m.hash === h;
    },
    isStale(id) {
      if (!Object.hasOwn(doc.items, id)) return false;
      const m = doc.items[id], h = hashOf(id);
      return h !== null && m.hash !== null && m.hash !== h;
    },
    setSeen(id, on) {
      const m = on ? markFor(id) : null;
      if (m) doc.items[id] = m; else if (Object.hasOwn(doc.items, id)) delete doc.items[id]; else return;
      if (!connected) pending[id] = m;
      saveLocal();
      schedule();
    },
    visit() { return doc.visit || null; },
    setVisit(next) {
      const same = doc.visit && doc.visit.highestRound === next.highestRound && JSON.stringify(doc.visit.itemHashes) === JSON.stringify(next.itemHashes);
      if (same) return;
      doc.visit = next;
      saveLocal();
      schedule();
    },
    onChange(fn) { changeListeners.push(fn); },
    onConnect(fn) { connectListeners.push(fn); },
  };
}

/** The page's environment for the store: its review, round and items from the manifest, and the viewer's browser. */
export function pageSeenEnv(win: Window & { claude?: SeenEnv['claude'] }, document: Document): SeenEnv {
  const main = document.querySelector('main[data-review]');
  let reviewId = main ? main.getAttribute('data-review') || '' : '';
  const items: SeenEnv['items'] = {};
  document.querySelectorAll('[data-item]').forEach((el) => {
    const h = el.querySelector('.item-h');
    items[el.getAttribute('data-item')!] = { hash: null, heading: h ? (h.textContent || '').trim() : '' };
  });
  let round: number | null = null;
  const manifest = document.getElementById('review-manifest');
  if (manifest) {
    try {
      const parsed = JSON.parse(manifest.textContent || '') as { reviewId: string; round: number; items: Array<{ id: string; hash: string; heading: string }> };
      reviewId = parsed.reviewId;
      round = parsed.round;
      for (const item of parsed.items) items[item.id] = { hash: item.hash, heading: item.heading };
    } catch { /* no manifest: marks by id alone */ }
  }
  let storage: SeenEnv['storage'] = null;
  try { storage = win.localStorage; } catch { /* storage unavailable */ }
  return {
    reviewId, round, items, storage, claude: win.claude,
    now: () => new Date().toISOString(),
    // A test's window may lack timers: writes then never fire.
    setTimeout: (fn, ms) => ((win as Partial<Window>).setTimeout ? win.setTimeout(fn, ms) : null),
    clearTimeout: (handle) => { if ((win as Partial<Window>).clearTimeout) win.clearTimeout(handle as number); },
    random: () => Math.random(),
  };
}

/** Defines the page's one store, `debriefSeen`, before the page behaviour and the round overlay use it. */
export function seenStoreScript(): string {
  return `var debriefSeen=(${createSeenStore.toString()})((${pageSeenEnv.toString()})(window,document));`;
}
