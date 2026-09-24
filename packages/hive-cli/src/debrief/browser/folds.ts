/**
 * G-D, the randomized fold check. For each diff spec, in each tab, click random fold steps until nothing is folded,
 * checking at every step that shown plus folded changes add up to the file's, that no fold is short or empty, and that
 * no replacement is ever partly shown. Runs in the page, where `window.DiffView` is the engine, so it is self-contained.
 */
export function checkFoldSteps(args: { specs: Array<Record<string, unknown>>; seed: number }): { specs: number; clicks: number } {
  interface Api { render: (host: HTMLElement, spec: object) => { element: HTMLElement }; countChanges: (spec: object) => { add: number; del: number } }
  let seed = args.seed, clicks = 0;
  const random = (n: number) => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed % n; };
  const check = (ok: unknown, message: string) => { if (!ok) throw Error(message); };
  const api = (window as unknown as { DiffView: Api }).DiffView;
  const numbersOf = (root: ParentNode, selector: string) => new Set(Array.from(root.querySelectorAll(selector)).map((n) => Number(n.textContent)));
  const leftOut = (panel: Element) => Array.from(panel.querySelectorAll('.dv-fold-line')).some((f) => /left out/.test(f.textContent || ''));
  /* The oracle needs no engine internals: the same spec rendered again, on All changes, every fold opened.
     There each maximal run of removed and added rows (label and bracket rows are transparent) holding both is a replacement. */
  function replacements(spec: object): Array<{ olds: Array<number>; news: Array<number> }> {
    const host = document.createElement('div'); document.body.append(host);
    const panel = api.render(host, spec).element;
    panel.querySelector<HTMLElement>('.dv-tab[data-dv-view="all"]')?.click();
    for (let b = panel.querySelector<HTMLElement>('.dv-gap-btn'), n = 0; b; b = panel.querySelector<HTMLElement>('.dv-gap-btn')) { b.click(); check(++n < 500, 'oracle expansion must terminate'); }
    const found: Array<{ olds: Array<number>; news: Array<number> }> = [];
    let run = { olds: [] as Array<number>, news: [] as Array<number> };
    const flush = () => { if (run.olds.length && run.news.length) found.push(run); run = { olds: [], news: [] }; };
    for (const row of Array.from(panel.querySelectorAll('.dv-rows > *'))) {
      if (!row.classList.contains('dv-row')) continue;
      if (row.classList.contains('dv-del')) run.olds.push(Number(row.querySelector('.dv-n-old')!.textContent));
      else if (row.classList.contains('dv-add')) run.news.push(Number(row.querySelector('.dv-n-new')!.textContent));
      else flush();
    }
    flush();
    host.remove();
    return found;
  }
  for (const spec of args.specs) {
    const name = String(spec.path);
    const whole = replacements(spec);
    const host = document.createElement('div'); document.body.append(host);
    const panel = api.render(host, spec).element, totals = api.countChanges(spec);
    const shown = () => ({ add: panel.querySelectorAll('.dv-row.dv-add').length, del: panel.querySelectorAll('.dv-row.dv-del').length });
    const folded = () => {
      let add = 0, del = 0;
      panel.querySelectorAll('.dv-fold-line').forEach((f) => { add += Number(f.querySelector('.dv-plus')?.textContent.slice(1) ?? 0); del += Number(f.querySelector('.dv-minus')?.textContent.slice(1) ?? 0); });
      return { add, del };
    };
    const noSplit = (where: string) => {
      const olds = numbersOf(panel, '.dv-row.dv-del .dv-n-old'), news = numbersOf(panel, '.dv-row.dv-add .dv-n-new');
      for (const r of whole) {
        const c = r.olds.filter((n) => olds.has(n)).length + r.news.filter((n) => news.has(n)).length;
        check(!c || c === r.olds.length + r.news.length, `split replacement in ${name} ${where}`);
      }
    };
    const relevantTab = panel.querySelector('.dv-tab[data-dv-view="relevant"]');
    if (relevantTab) {
      const s = shown(), n = (sel: string) => Number(relevantTab.querySelector(sel)!.textContent.slice(1));
      check(s.add === n('.dv-plus') && s.del === n('.dv-minus'), `Relevant tab count mismatch in ${name}`);
      check(leftOut(panel), `no left-out marker in ${name}`);
    }
    check(!panel.querySelector('.dv-tools, .dv-fold, .dv-expand-file, .dv-reset'), `extra control in ${name}`);
    for (const view of relevantTab ? ['relevant', 'all'] : ['only']) {
      if (view === 'all') {
        panel.querySelector<HTMLElement>('.dv-tab[data-dv-view="all"]')!.click();
        check(!leftOut(panel), `All changes left a change out in ${name}`);
      }
      for (let steps = 0; ; steps++) {
        const s = shown(), f = folded();
        check(s.add + f.add === totals.add && s.del + f.del === totals.del, `accounting mismatch in ${name} ${view} step ${steps}`);
        noSplit(`${view} step ${steps}`);
        panel.querySelectorAll('.dv-gap-label').forEach((l) => check(Number(/\d+/.exec(l.textContent || '')![0]) > 3, `short or empty fold in ${name}`));
        const buttons = panel.querySelectorAll<HTMLElement>('.dv-gap-btn');
        if (!buttons.length) break;
        buttons[random(buttons.length)].click(); clicks++;
        check(steps < 250, `nonterminating fold in ${name}`);
      }
      const s = shown();
      check(s.add === totals.add && s.del === totals.del, `final accounting in ${name} ${view}`);
    }
    host.remove();
  }
  return { specs: args.specs.length, clicks };
}
