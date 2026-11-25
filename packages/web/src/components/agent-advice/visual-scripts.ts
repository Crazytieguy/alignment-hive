// @ts-nocheck: the visuals' scripts, plain untyped JavaScript. Each takes its figure as its root element.

export function initTldr(root: HTMLElement): void {
(function(){
    const r = root.querySelector('.reply'), more = root.querySelector('.more'), less = root.querySelector('.less');
  // As in the plugin: [ more ] swaps the sentence for the full reply, [ less ] under it swaps back.
  more.addEventListener('click', function(){
    r.dataset.state = 'open'; more.setAttribute('aria-expanded', 'true');
    root.querySelector('.opened').focus({preventScroll:true});
  });
  less.addEventListener('click', function(){
    r.dataset.state = 'closed'; more.setAttribute('aria-expanded', 'false');
    more.focus({preventScroll:true});
    const b = root.getBoundingClientRect();
    if (b.top < 0 || b.bottom > innerHeight) root.scrollIntoView({block:'nearest'});
  });
})();}

export function initDebrief(root: HTMLElement): void {
(() => {
    // Layout only: no interaction, no state. With room for a margin (CSS decides), each comment sits beside the report level
  // with what it is on. This slides crowded ones apart, as little as possible, and draws each one's line the same way: from
  // the card to just past the text's right edge at the anchor's height, then along to the ring round what it is on, ending
  // on it with a dot. For a whole line the ring's right side is at the text's edge; for selected words the line runs in the
  // gap between two lines of text (above or below the words, whichever has less text beside it) to the ring's corner.
  const sheet = root.querySelector('.vdt-p1'), probe = root.querySelector('.vdt-probe');
  const top = root.querySelector('.vdt-labels'), below = root.querySelector('.vdt-labels2');
  const notes = sheet ? Array.from(sheet.querySelectorAll('.vdt-note')) : [];
  if (!probe || !top || !below || !notes.length) return;
  root.insertAdjacentHTML('beforeend', '<svg class="vdt-links" aria-hidden="true" focusable="false"></svg>');
  const svg = root.lastElementChild;
  root.classList.add('vdt-js');
  const GAP = 6, PIN = 17, OUT = 6;   // px between stacked cards; card top to where its line meets it; the line's bend past the text

  // stack the cards in order, each as near its .want as it can be without overlapping the one above (sets .y)
  function stack(items, lo, hi) {
    const blocks = [];
    items.forEach(it => {
      it.off = 0;
      blocks.push({ items: [it], h: it.h, sum: it.want, y: 0 });
      for (;;) {
        const b = blocks[blocks.length - 1], p = blocks[blocks.length - 2];
        b.y = Math.max(lo, Math.min(b.sum / b.items.length, hi - b.h));
        if (!p || p.y + p.h + GAP <= b.y) break;
        b.items.forEach(x => { x.off += p.h + GAP; p.sum += x.want - x.off; p.items.push(x); });
        p.h += GAP + b.h;
        blocks.pop();
      }
    });
    blocks.forEach(b => b.items.forEach(x => { x.y = b.y + x.off; }));
  }

  function lay() {
    notes.forEach(n => n.style.removeProperty('--vdt-dy'));
    svg.innerHTML = '';
    if (getComputedStyle(notes[0]).position !== 'absolute') return;   // narrow: the comments stay in the flow, under their lines
    // the report may be drawn with CSS zoom; browsers differ on whether rectangles inside it come back zoomed, so measure it
    const z = parseFloat(getComputedStyle(sheet).getPropertyValue('--vdt-z')) || 1;
    const k = z / (probe.getBoundingClientRect().width / 100 || z);
    const R = root.getBoundingClientRect();
    const box = q => ({ l: q.left * k - R.left, r: q.right * k - R.left, t: q.top * k - R.top, b: q.bottom * k - R.top });
    const lo = top.getBoundingClientRect().bottom - R.top + 2, hi = below.getBoundingClientRect().top - R.top - 12;
    // every text fragment in the sheet, to tell which gap beside a ringed word has less text along it
    const frags = [];
    const tw = document.createTreeWalker(sheet, NodeFilter.SHOW_TEXT);
    const rg = document.createRange();
    for (let t; (t = tw.nextNode());) {
      if (!t.data.trim() || t.parentElement.closest('.vdt-note')) continue;
      rg.selectNodeContents(t);
      for (const q of rg.getClientRects()) frags.push(box(q));
    }
    const items = notes.map(n => {
      const wrap = n.parentElement, c = n.getBoundingClientRect();
      const it = { n, y0: c.top - R.top, h: c.height, cx: c.left - R.left };
      const on = n.dataset.on && wrap.querySelector('.vdt-on[data-k="' + n.dataset.on + '"]');
      if (on) {
        const block = on.closest('.dv-code, p'), rs = Array.from(on.getClientRects()).map(box), w = rs[rs.length - 1];
        const edge = box(block.getBoundingClientRect()).r;
        const lh = parseFloat(getComputedStyle(block).lineHeight) || (w.b - w.t + 6), half = Math.max(1.5, (lh - (w.b - w.t)) / 2);
        const beside = (y0, y1) => frags.reduce((s, f) => {
          const cy = (f.t + f.b) / 2;
          return s + (cy > y0 && cy < y1 ? Math.max(0, Math.min(f.r, edge) - Math.max(f.l, w.r)) : 0);
        }, 0);
        const up = beside(w.t - lh, w.t) <= beside(w.b, w.b + lh);
        it.kind = 'words';
        it.ex = w.r + 0.5; it.ey = up ? w.t - 0.5 : w.b + 0.5;     // the ring's corner (the ring is 1px outside the box)
        it.mid = up ? w.t - half : w.b + half;                      // the middle of that gap
        it.ax = edge + OUT;
      } else {
        const code = box(wrap.querySelector('.dv-code').getBoundingClientRect());
        it.kind = 'line';
        it.ex = code.r; it.ey = it.mid = (code.t + code.b) / 2;     // the ring's right side
        it.ax = code.r + OUT;
      }
      it.want = it.mid - PIN;
      return it;
    });
    stack(items, lo, hi);
    let out = '';
    items.forEach(it => {
      it.n.style.setProperty('--vdt-dy', (it.y - it.y0).toFixed(1) + 'px');
      const cy = it.y + PIN, f = v => v.toFixed(1);
      out += '<path d="M' + f(it.cx) + ',' + f(cy) + 'L' + f(it.ax) + ',' + f(it.mid) + 'H' + f(it.ex)
        + (it.ey !== it.mid ? 'V' + f(it.ey) : '') + '"/><circle cx="' + f(it.ex) + '" cy="' + f(it.ey) + '" r="2"/>';
    });
    svg.innerHTML = out;
  }

  let queued = 0;
  const later = () => { if (!queued) queued = requestAnimationFrame(() => { queued = 0; lay(); }); };
  lay();
  if (window.ResizeObserver) { const ro = new ResizeObserver(later); ro.observe(root); ro.observe(sheet); }
  addEventListener('resize', later);
  addEventListener('load', later);
  if (document.fonts) {
    if (document.fonts.ready) document.fonts.ready.then(later);
    if (document.fonts.addEventListener) document.fonts.addEventListener('loadingdone', later);
  }
})();}

export function initLadder(root: HTMLElement): void {
(function(){
    const inputs = root.querySelectorAll('.v-ladder-row input');
  const panels = root.querySelectorAll('.v-ladder-panel');
  const group = 'v-ladder-level-' + Math.random().toString(36).slice(2);
  function sync(){
    let level = null;
    inputs.forEach(function(i){ if (i.checked) level = i.value; });
    panels.forEach(function(p){ p.setAttribute('data-state', p.getAttribute('data-level') === level ? 'open' : 'closed'); });
  }
  inputs.forEach(function(i){ i.name = group; i.addEventListener('change', sync); });
  sync();
})();}
