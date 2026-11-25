// Behaviour shared by the whole post: tooltips, the copy buttons, and keeping a clicked <details> row in place while it
// opens or closes. Returns a cleanup.
export function initArticleInteractions(wrap: HTMLElement): () => void {
  const off: Array<() => void> = [];
  const on = <K extends keyof DocumentEventMap>(
    target: Document | Window,
    type: K,
    fn: (e: DocumentEventMap[K]) => void,
    opts?: AddEventListenerOptions,
  ) => {
    target.addEventListener(type, fn as EventListener, opts);
    off.push(() => target.removeEventListener(type, fn as EventListener, opts));
  };
  const closest = (e: Event, sel: string) =>
    e.target instanceof Element ? (e.target.closest(sel) as HTMLElement | null) : null;

  // ---- collapsibles: the row you click stays where it is ----
  on(document, "click", (e) => {
    const s = closest(e, "details>summary");
    if (!s || e.defaultPrevented || !wrap.contains(s)) return;
    const d = s.parentElement as HTMLDetailsElement;
    const top = s.getBoundingClientRect().top;
    if (d.open) wrap.style.minHeight = Math.max(wrap.offsetHeight, parseFloat(wrap.style.minHeight) || 0) + "px";
    const root = document.documentElement.style;
    root.overflowAnchor = "none";
    const fix = () => {
      const dy = s.getBoundingClientRect().top - top;
      if (Math.abs(dy) >= 0.5) window.scrollBy(0, dy);
    };
    setTimeout(fix, 0);
    requestAnimationFrame(() => {
      fix();
      requestAnimationFrame(() => {
        fix();
        root.overflowAnchor = "";
      });
    });
  });

  // ---- tooltips: open on hover, keyboard focus or tap; Escape or a tap elsewhere closes ----
  let tip: HTMLElement | null = null;
  const place = (tt: HTMLElement) => {
    const t = tt.querySelector<HTMLElement>(".tt-t");
    const b = tt.querySelector<HTMLElement>(".tt-b");
    if (!t || !b) return;
    const rs = t.getClientRects();
    const r = rs.length ? rs[0] : t.getBoundingClientRect();
    const vw = document.documentElement.clientWidth;
    const vh = window.innerHeight;
    const m = 16;
    b.style.left = "0px";
    b.style.top = "0px";
    const w = b.offsetWidth;
    const h = b.offsetHeight;
    const x = Math.min(Math.max(m, r.left + r.width / 2 - w / 2), Math.max(m, vw - m - w));
    let y = r.bottom + 6;
    if (y + h > vh - 8 && r.top - 6 - h >= 8) y = r.top - 6 - h;
    b.style.left = Math.round(x) + "px";
    b.style.top = Math.round(y) + "px";
  };
  const close = (tt: HTMLElement | null) => {
    if (!tt) return;
    tt.classList.remove("on");
    delete tt.dataset.how;
    if (tip === tt) tip = null;
  };
  const open = (tt: HTMLElement, how: string) => {
    if (tip && tip !== tt) close(tip);
    tip = tt;
    tt.classList.add("on");
    tt.dataset.how = how;
    place(tt);
  };
  on(document, "pointerover", (e) => {
    const tt = e.pointerType === "mouse" ? closest(e, ".tt") : null;
    if (tt && !tt.classList.contains("on")) open(tt, "hover");
  });
  on(document, "pointerout", (e) => {
    const tt = e.pointerType === "mouse" ? closest(e, ".tt") : null;
    if (tt && tt.dataset.how === "hover" && !tt.contains(e.relatedTarget as Node | null)) close(tt);
  });
  on(document, "focusin", (e) => {
    const tt = closest(e, ".tt");
    if (tt && !tt.classList.contains("on")) open(tt, "focus");
  });
  on(document, "focusout", (e) => {
    const tt = closest(e, ".tt");
    if (tt && !tt.contains(e.relatedTarget as Node | null)) close(tt);
  });
  on(document, "click", (e) => {
    const tt = closest(e, ".tt");
    if (!tt) {
      if (tip) close(tip);
      return;
    }
    if (tt.dataset.how === "tap") close(tt);
    else open(tt, "tap");
  });
  on(document, "keydown", (e) => {
    if (e.key === "Escape" && tip) {
      close(tip);
      return;
    }
    const el = e.target as HTMLElement;
    if (el.classList?.contains("tt-t") && (e.key === "Enter" || e.key === " ")) {
      e.preventDefault();
      const t = el.parentElement as HTMLElement;
      if (t.dataset.how === "tap") close(t);
      else open(t, "tap");
    }
  });
  on(window, "scroll", () => tip && place(tip), { passive: true });
  on(window, "resize", () => tip && place(tip));

  // ---- copy buttons ----
  const selectText = (el: Element) => {
    try {
      const r = document.createRange();
      r.selectNodeContents(el);
      const s = window.getSelection();
      s?.removeAllRanges();
      s?.addRange(r);
      return true;
    } catch {
      return false;
    }
  };
  const timers = new WeakMap<HTMLElement, number>();
  on(document, "click", (e) => {
    const b = closest(e, ".copy");
    if (!b || !wrap.contains(b)) return;
    const card = b.closest(".paste");
    const body = card?.querySelector<HTMLElement>(".pb");
    const label = b.querySelector(".cl");
    const live = card?.querySelector("[aria-live]");
    if (!body || !label) return;
    const text = body.getAttribute("data-copy") || body.innerText;
    const say = (msg: string, ok: boolean) => {
      b.classList.toggle("ok", ok);
      label.textContent = msg;
      if (live) live.textContent = ok ? "Copied" : msg;
      clearTimeout(timers.get(b));
      timers.set(
        b,
        window.setTimeout(() => {
          b.classList.remove("ok");
          label.textContent = "Copy";
          if (live) live.textContent = "";
        }, ok ? 2200 : 6000),
      );
    };
    const fallback = () => {
      const sel = selectText(body);
      let done = false;
      try {
        done = sel && document.execCommand("copy");
      } catch {
        done = false;
      }
      if (done) {
        window.getSelection()?.removeAllRanges();
        say("Copied", true);
      } else {
        say(sel ? "Selected. Press " + (/Mac|iPhone|iPad/.test(navigator.platform || "") ? "⌘C" : "Ctrl+C") : "Select the text to copy", false);
      }
    };
    try {
      if (navigator.clipboard?.writeText) navigator.clipboard.writeText(text).then(() => say("Copied", true), fallback);
      else fallback();
    } catch {
      fallback();
    }
  });

  return () => off.forEach((f) => f());
}
