import { reviewRoundMessages as msg } from '../lib/messages';
import { escapeHtml, escapeJson } from './html';
import { reviewError } from './parse';
import type { ReviewManifest } from './manifest';
import type { SeenApi } from './seen';

export interface RoundComparison { states: Record<string, 'new' | 'updated' | 'unchanged'>; footerHtml: string; warnings: Array<string> }
/** Comparisons consume immutable manifests only; no filesystem or transcript readers. */
export function compareRounds(current: ReviewManifest, previous?: ReviewManifest): RoundComparison {
  if (!previous) {
    if (current.round !== 1) throw reviewError('page', 1, msg.previousRequired);
    return { states: Object.fromEntries(current.items.map((item) => [item.id, 'new'])), footerHtml: '', warnings: [] };
  }
  if (previous.session !== current.session || previous.reviewId !== current.reviewId || previous.round !== current.round - 1) throw reviewError('page', 1, msg.wrongPrevious);
  const before = new Map(previous.items.map((item) => [item.id, item]));
  const after = new Map(current.items.map((item) => [item.id, item]));
  const consumed = new Set<string>();
  const states: RoundComparison['states'] = {};
  for (const item of current.items) {
    const oldId = item.was ?? item.id;
    if (item.was && (!before.has(oldId) || after.has(oldId) || before.has(item.id) || consumed.has(oldId))) throw reviewError(item.id, 1, msg.invalidRename(oldId));
    const prior = before.get(oldId);
    if (prior) consumed.add(oldId);
    states[item.id] = !prior ? 'new' : prior.hash === item.hash ? 'unchanged' : 'updated';
  }
  for (const [id, disposition] of Object.entries(current.dispositions)) {
    if (!before.has(id) || consumed.has(id)) throw reviewError(id, 1, msg.invalidDisposition(id));
    if (disposition.startsWith('superseded: ') && !after.has(disposition.slice('superseded: '.length))) throw reviewError(id, 1, msg.invalidSuccessor(disposition));
  }
  const footer: Array<string> = [], warnings: Array<string> = [];
  for (const prior of previous.items) {
    if (consumed.has(prior.id)) continue;
    const disposition = Object.hasOwn(current.dispositions, prior.id) ? current.dispositions[prior.id] : undefined;
    if (!disposition) {
      const warning = msg.undisposed(prior.heading);
      warnings.push(reviewError(prior.id, 1, warning).message);
      footer.push(`<p class="review-warning" data-disposition="missing">${escapeHtml(warning)}</p>`);
    } else if (disposition.startsWith('superseded: ')) {
      const successor = after.get(disposition.slice('superseded: '.length))!;
      footer.push(`<p data-disposition="superseded"><a href="#item-${successor.id}">${escapeHtml(msg.superseded(prior.heading, successor.heading))}</a></p>`);
    } else footer.push(`<p data-disposition="${disposition}">${escapeHtml(msg.disposed(prior.heading, disposition))}</p>`);
  }
  return { states, footerHtml: footer.join('\n'), warnings };
}

export interface SeenStore { reviewId: string; highestRound: number; itemHashes: Record<string, string>; savedAt: string }
export type SeenPage = Pick<ReviewManifest, 'reviewId' | 'round' | 'items' | 'history'>;
/** `unseen`: items new or changed since the last visit; `added`: those of them the last visit did not have. */
export interface SeenUpdate { unseen: Array<string>; added?: Array<string>; next?: SeenStore }

/** Self-contained for identical use in tests and the bundled browser overlay. */
export function computeSeen(page: SeenPage, stored: unknown, savedAt: string): SeenUpdate {
  const next: SeenStore = { reviewId: page.reviewId, highestRound: page.round, itemHashes: Object.fromEntries(page.items.map((item) => [item.id, item.hash])), savedAt };
  if (!stored || typeof stored !== 'object' || Array.isArray(stored)) return { unseen: [], next };
  const state = stored as Partial<SeenStore>;
  if (state.reviewId !== page.reviewId || !Number.isSafeInteger(state.highestRound) || state.highestRound! < 1 || !state.itemHashes || typeof state.itemHashes !== 'object' || Array.isArray(state.itemHashes) || !Object.values(state.itemHashes).every((hash) => typeof hash === 'string')) return { unseen: [], next };
  if (state.highestRound! > page.round) return { unseen: [] };
  // History supplies the skipped-round identity map, never a substitute for
  // hashes actually seen before a later rerender of that same round.
  const history = Object.hasOwn(page.history, String(state.highestRound)) ? page.history[String(state.highestRound)] : undefined;
  const prior = (item: SeenPage['items'][number]) => !Object.hasOwn(state.itemHashes!, item.id) && item.was && history && Object.hasOwn(history, item.was) ? item.was : item.id;
  const unseen = page.items.filter((item) => !Object.hasOwn(state.itemHashes!, prior(item)) || state.itemHashes![prior(item)] !== item.hash).map((item) => item.id);
  const added = page.items.filter((item) => !Object.hasOwn(state.itemHashes!, prior(item))).map((item) => item.id);
  return { unseen, added, next };
}

/** This function has no captured values, so toString remains safe when bundled/minified. */
function installSeenOverlay(page: SeenPage, compute: typeof computeSeen, labels: Record<'changed' | 'added', { text: string; title: string }>, store: Pick<SeenApi, 'visit' | 'setVisit' | 'onConnect'> | undefined): void {
  if (!store) return;
  const apply = () => {
    const elements = [...document.querySelectorAll<HTMLElement>('[data-item]')];
    for (const element of elements) {
      element.removeAttribute('data-unseen');
      element.querySelectorAll('.review-unseen').forEach((chip) => chip.remove());
    }
    // Compared with the visit stored before this one is recorded.
    const update = compute(page, store.visit(), new Date().toISOString());
    if (update.next) store.setVisit(update.next);
    const unseen = new Set(update.unseen), added = new Set(update.added);
    for (const element of elements) {
      const id = element.getAttribute('data-item')!;
      if (!unseen.has(id)) continue;
      const label = added.has(id) ? labels.added : labels.changed;
      element.setAttribute('data-unseen', 'true');
      const chip = document.createElement('span');
      chip.className = 'tag review-unseen';
      chip.textContent = label.text;
      chip.title = label.title;
      element.querySelector('.item-meta')?.appendChild(chip);
    }
  };
  apply();
  // The database's visit, from any device, replaces the local one once; later changes elsewhere don't re-mark this view.
  store.onConnect((remote) => { if (remote) apply(); });
  window.addEventListener('pageshow', (event) => { if (event.persisted) apply(); });
}

export function seenOverlayScript(): string {
  return `(${installSeenOverlay.toString()})(JSON.parse(document.getElementById('review-manifest').textContent),${computeSeen.toString()},${escapeJson({ changed: { text: msg.unseen, title: msg.unseenTitle }, added: { text: msg.unseenNew, title: msg.unseenNewTitle } })},typeof debriefSeen==='undefined'?undefined:debriefSeen);`;
}
