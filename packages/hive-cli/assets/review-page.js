/* Review page behaviour: times, theme, contents rail, seen, clipped outputs, file-view source toggles,
   and the viewer's UI state kept across reloads and re-renders.
   `copy` is the renderer's page copy; `times` is the renderer's own formatter, run here in the viewer's zone;
   `seen` is the viewer's seen marks (debrief/seen.ts), kept apart from the UI state below. */
function reviewPage(copy, times, seen) {
  "use strict";

  /* ---- saved UI state: per viewer, per review; apart from the round overlay's review:<id> ---- */
  var KEY = "review-ui:" + document.querySelector("main[data-review]").getAttribute("data-review");
  function isRecord(value) { return value !== null && typeof value === "object" && !Array.isArray(value); }
  function flags(value) {
    var out = {};
    if (isRecord(value)) Object.keys(value).forEach(function (k) { if (value[k] === true) out[k] = true; });
    return out;
  }
  /* Storage may be absent, blocked or hold anything: keep only well-formed fields. */
  function load() {
    var raw = null, parsed = null;
    try { raw = localStorage.getItem(KEY); } catch (e) { /* storage unavailable */ }
    if (typeof raw === "string") { try { parsed = JSON.parse(raw); } catch (e) { /* not JSON */ } }
    var v = isRecord(parsed) ? parsed : {};
    var position = function (n) { return typeof n === "number" && isFinite(n) && n > 0 ? n : undefined; };
    return {
      theme: v.theme === "light" || v.theme === "dark" ? v.theme : undefined,
      open: flags(v.open), expanded: flags(v.expanded), source: flags(v.source),
      scrollY: position(v.scrollY), railScroll: position(v.railScroll),
      railOpen: v.railOpen === true || undefined, asksAll: v.asksAll === true || undefined, askOpen: flags(v.askOpen),
      lastHash: typeof v.lastHash === "string" && v.lastHash.charAt(0) === "#" && v.lastHash.length <= 512 ? v.lastHash : undefined
    };
  }
  function save() { try { localStorage.setItem(KEY, JSON.stringify(state)); } catch (e) { /* storage unavailable */ } }
  var state = load();

  /* Each fold carries what it shows in data-key, so state follows the evidence when edits reorder it. A fold is
     filed under its item plus that key, with an occurrence number when one item shows the same source twice;
     outputs and toggles inside a fold, under the fold. */
  var foldKey = new Map();
  document.querySelectorAll(".item").forEach(function (item) {
    var id = item.getAttribute("data-item"), count = Object.create(null);
    item.querySelectorAll("[data-key]").forEach(function (fold) {
      var key = fold.getAttribute("data-key"), n = count[key] = (count[key] || 0) + 1;
      foldKey.set(fold, id + "|" + key + (n > 1 ? "#" + (n - 1) : ""));
    });
  });
  function wrapKey(wrap) {
    var fold = wrap.closest("[data-key]");
    var key = fold && foldKey.get(fold);
    return key ? key + "|out|" + Array.prototype.indexOf.call(fold.querySelectorAll(".tr-wrap"), wrap) : null;
  }

  /* `copy` holds templates like "{k} of {n} seen". */
  function say(template, values) { return template.replace(/\{(\w+)\}/g, function (m, k) { return String(values[k]); }); }

  /* ---- times, in the viewer's zone ---- */
  var format = times();
  var at = function (t) { return t.getAttribute("datetime"); };
  document.querySelectorAll("time.tr-time, time.shot-time").forEach(function (t) { t.textContent = format.stamp(at(t)); });
  document.querySelectorAll(".foryou time").forEach(function (t) { t.textContent = format.day(at(t)); });

  /* ---- theme ---- */
  var root = document.documentElement;
  function applyTheme(value) {
    if (value === "dark" || value === "light") root.setAttribute("data-theme", value);
    else root.removeAttribute("data-theme");
    document.querySelectorAll("[data-theme-pick]").forEach(function (b) {
      b.setAttribute("aria-pressed", String(b.getAttribute("data-theme-pick") === (value || "auto")));
    });
  }
  applyTheme(state.theme);
  document.querySelectorAll("[data-theme-pick]").forEach(function (b) {
    b.addEventListener("click", function () {
      var v = b.getAttribute("data-theme-pick");
      state.theme = v === "auto" ? undefined : v;
      save(); applyTheme(state.theme);
    });
  });

  /* ---- contents rail ---- */
  var sections = Array.prototype.slice.call(document.querySelectorAll(".sec[data-sec]"));
  var nav = document.getElementById("nav");
  var fold = document.getElementById("rail-fold");
  var narrow = window.matchMedia ? window.matchMedia("(max-width: 960px)") : null;
  /* On a phone the Contents fold keeps the viewer's choice; on a desktop it is always open. */
  function foldRail() { if (fold) fold.open = narrow && narrow.matches ? !!state.railOpen : true; }
  foldRail();
  if (narrow && narrow.addEventListener) narrow.addEventListener("change", foldRail);
  if (fold) fold.addEventListener("toggle", function () {
    if (narrow && narrow.matches) { state.railOpen = fold.open || undefined; save(); }
  });
  /* The rail's own scroll position, like the page's. */
  var railEl = document.querySelector(".rail");
  if (railEl) {
    var railSave = null;
    railEl.addEventListener("scroll", function () {
      clearTimeout(railSave);
      railSave = setTimeout(function () { state.railScroll = Math.round(railEl.scrollTop) || undefined; save(); }, 250);
    }, { passive: true });
    window.addEventListener("load", function () { if (state.railScroll) railEl.scrollTop = state.railScroll; });
  }
  function navLink(item) {
    var a = document.createElement("a");
    a.href = "#" + item.id;
    var text = document.createElement("span"); text.className = "nav-text";
    var h = item.querySelector(".item-h");
    text.textContent = h.getAttribute("data-nav") || h.textContent;
    a.appendChild(text);
    a.title = text.textContent;
    if (item.hasAttribute("data-attn")) a.setAttribute("data-attn", "");
    if (item.classList.contains("is-seen")) a.classList.add("is-seen");
    a.addEventListener("click", function () { item.open = true; setActive(item.id); });
    return a;
  }
  function buildNav() {
    nav.innerHTML = "";
    sections.forEach(function (sec) {
      var box = document.createElement("div"); box.className = "nav-sec";
      var t = document.createElement("p"); t.className = "nav-sec-t";
      var ta = document.createElement("a"); ta.href = "#" + sec.id; ta.textContent = sec.getAttribute("data-sec");
      t.appendChild(ta); box.appendChild(t);
      var ul = document.createElement("ul"); ul.className = "nav-items";
      sec.querySelectorAll(":scope > .item").forEach(function (item) {
        var li = document.createElement("li"); li.appendChild(navLink(item)); ul.appendChild(li);
      });
      /* A section's lower-priority items share one line, so the rail shows where its main items end. */
      var tray = sec.querySelector(":scope > .rest");
      if (tray) {
        var li = document.createElement("li"), a = document.createElement("a");
        a.href = "#" + tray.id; a.className = "nav-rest";
        a.addEventListener("click", function () { tray.open = true; });
        var text = document.createElement("span"); text.className = "nav-text";
        text.textContent = say(copy.restNav, { n: tray.getAttribute("data-count") });
        a.appendChild(text); li.appendChild(a); ul.appendChild(li);
      }
      box.appendChild(ul); nav.appendChild(box);
    });
    updateActive();
  }
  var vis = Object.create(null);
  var activeId = null;
  function setActive(id) {
    activeId = id;
    nav.querySelectorAll("a.active").forEach(function (a) { a.classList.remove("active"); });
    if (!activeId) return;
    var a = nav.querySelector('a[href="#' + activeId + '"]');
    /* A lower-priority item lights its tray's line. */
    var tray = !a && document.getElementById(activeId).closest(".rest");
    if (tray) a = nav.querySelector('a[href="#' + tray.id + '"]');
    if (a) a.classList.add("active");
  }
  function updateActive(refresh) {
    var items = document.querySelectorAll(".item"), hit = null;
    var top = innerHeight * .1, bottom = innerHeight * .3;
    for (var i = 0; i < items.length; i++) {
      if (refresh !== false) {
        var rect = items[i].getBoundingClientRect();
        vis[items[i].id] = rect.height > 0 && rect.bottom > top && rect.top < bottom;
      }
      if (!hit && vis[items[i].id]) hit = items[i];
    }
    if (items.length && scrollY + innerHeight >= document.body.scrollHeight - 2) hit = items[items.length - 1];
    setActive(hit ? hit.id : null);
  }
  var observer;
  function observeItems() {
    if (observer) observer.disconnect();
    if ("IntersectionObserver" in window) {
      observer = new IntersectionObserver(function (entries) {
        entries.forEach(function (e) { vis[e.target.id] = e.isIntersecting; });
        updateActive(false);
      }, { rootMargin: (-innerHeight * .1) + "px 0px " + (-innerHeight * .7) + "px 0px" });
      document.querySelectorAll(".item").forEach(function (item) { observer.observe(item); });
    }
    updateActive();
  }
  /* One scroll listener: the active rail entry per frame, and the saved position once scrolling settles. */
  var scrollFrame = null, scrollSave = null;
  window.addEventListener("scroll", function () {
    clearTimeout(scrollSave);
    scrollSave = setTimeout(function () { state.scrollY = Math.round(scrollY) || undefined; save(); }, 250);
    if (scrollFrame !== null) return;
    scrollFrame = requestAnimationFrame(function () { scrollFrame = null; updateActive(); });
  }, { passive: true });
  window.addEventListener("resize", observeItems);
  observeItems();

  /* ---- seen: a mark holds the item as it was when ticked; an item changed since is unseen again, marked updated ---- */
  var paints = [];
  document.querySelectorAll(".item").forEach(function (item) {
    var box = item.querySelector(".seen input");
    if (!box) return;
    var id = item.getAttribute("data-item");
    function paint() {
      var on = seen.isSeen(id), stale = seen.isStale(id), chip = item.querySelector(".item-meta > .review-stale");
      item.classList.toggle("is-seen", on); box.checked = on;
      if (stale && !chip && !item.querySelector('.item-meta > .tag-round')) {
        chip = document.createElement("span"); chip.className = "tag tag-round review-stale";
        chip.textContent = copy.stale; chip.title = copy.staleTitle;
        item.querySelector(".item-meta").appendChild(chip);
      } else if (!stale && chip) chip.remove();
    }
    paints.push(paint);
    paint();
    box.addEventListener("click", function (e) { e.stopPropagation(); });
    box.addEventListener("change", function () { seen.setSeen(id, box.checked); paint(); paintTrays(); buildNav(); });
  });
  seen.onChange(function () { paints.forEach(function (p) { p(); }); paintTrays(); buildNav(); });
  /* A folded tray says how many of its items are seen, so skipping it stays visible; there is no "mark all". */
  function paintTrays() {
    document.querySelectorAll(".rest").forEach(function (tray) {
      var items = tray.querySelectorAll(".item"), dots = tray.querySelectorAll(".rest-dots i"), k = 0;
      items.forEach(function (item, i) {
        var seen = item.classList.contains("is-seen");
        if (seen) k++;
        if (dots[i]) dots[i].classList.toggle("on", seen);
      });
      tray.querySelector(".rest-seen-t").textContent = say(copy.restSeen, { k: k, n: items.length });
      tray.classList.toggle("rest-done", k === items.length);
    });
  }
  paintTrays();
  document.querySelectorAll(".seen").forEach(function (l) { l.addEventListener("click", function (e) { e.stopPropagation(); }); });

  /* ---- your messages: the ones that set direction, or all of them; one line each, the day shown where it
     changes among the rows shown; a clipped line opens ---- */
  var asksAll = document.querySelector(".asks-all");
  function paintAsks() {
    var all = !!state.asksAll, lastDay = "";
    document.querySelectorAll(".asks .ask-rest").forEach(function (li) { li.hidden = !all; });
    document.querySelectorAll(".asks li").forEach(function (li) {
      var t = li.querySelector("time");
      if (!t) return;
      var d = format.dayTime(at(t)), day = t.querySelector(".ask-day");
      day.textContent = d.day + " "; t.lastChild.textContent = d.time;
      /* A hidden row keeps its day; only the rows shown share one. */
      if (li.hidden) { day.hidden = false; return; }
      day.hidden = d.key === lastDay;
      lastDay = d.key;
    });
    if (!asksAll) return;
    asksAll.setAttribute("aria-expanded", String(all));
    asksAll.textContent = asksAll.getAttribute(all ? "data-less" : "data-more");
  }
  paintAsks();
  if (asksAll) asksAll.addEventListener("click", function () { state.asksAll = !state.asksAll || undefined; save(); paintAsks(); measureAsks(); });
  function measureAsks() {
    document.querySelectorAll(".asks li").forEach(function (li) {
      if (li.hidden) return;
      var q = li.querySelector("q"), open = li.classList.contains("ask-open");
      var clipped = open || q.scrollWidth > q.clientWidth + 1;
      li.classList.toggle("ask-clipped", clipped);
      if (clipped) { li.tabIndex = 0; li.setAttribute("role", "button"); li.setAttribute("aria-expanded", String(open)); }
      else { li.removeAttribute("tabindex"); li.removeAttribute("role"); li.removeAttribute("aria-expanded"); }
    });
  }
  document.querySelectorAll(".asks li[data-key]").forEach(function (li) {
    var key = li.getAttribute("data-key");
    li.classList.toggle("ask-open", !!state.askOpen[key]);
    function toggle() {
      if (!li.classList.contains("ask-clipped")) return;
      var open = !li.classList.contains("ask-open");
      li.classList.toggle("ask-open", open); li.setAttribute("aria-expanded", String(open));
      state.askOpen[key] = open || undefined; save();
    }
    /* Selecting text in a line is reading it, not opening it. */
    li.addEventListener("click", function () { if (!(window.getSelection && String(window.getSelection()))) toggle(); });
    li.addEventListener("keydown", function (e) { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); toggle(); } });
  });
  measureAsks();
  window.addEventListener("resize", measureAsks);
  if (document.fonts && document.fonts.ready) document.fonts.ready.then(measureAsks);

  /* ---- clipped outputs and replies ---- */
  document.querySelectorAll(".tr-wrap").forEach(function (wrap) {
    var out = wrap.querySelector(".clip"), btn = wrap.querySelector(".ev-more");
    if (!out || !btn) return;
    var key = wrapKey(wrap);
    function paint(expanded) {
      out.classList.toggle("clip", !expanded);
      btn.textContent = btn.getAttribute(expanded ? "data-less" : "data-more");
    }
    if (key && state.expanded[key]) paint(true);
    btn.addEventListener("click", function () {
      var expanded = out.classList.contains("clip");
      if (key) { state.expanded[key] = expanded || undefined; save(); }
      paint(expanded);
      /* Collapsing a long block can leave the reader far below it; bring its button back. */
      if (!expanded && btn.getBoundingClientRect().top < 0) btn.scrollIntoView({ block: "center" });
    });
  });

  /* ---- images wider than their column: click, Enter or Space for full size, again to fit ---- */
  document.querySelectorAll(".shot-body img").forEach(function (img) {
    /* Only an image the column squeezes below its own (or its preferred) width zooms; one shown at the
       author's width has nothing to gain, and zooming it would only move it. Inside a closed fold it isn't
       shown, so it waits for the folds around it to open. */
    function check() {
      var preferred = Number(img.getAttribute("width")) || Infinity, shown = !img.checkVisibility || img.checkVisibility();
      var zoomable = shown && img.clientWidth + 2 < Math.min(img.naturalWidth, preferred) || img.classList.contains("zoomed");
      img.classList.toggle("zoomable", zoomable);
      img.tabIndex = zoomable ? 0 : -1;
      if (zoomable) { img.setAttribute("role", "button"); img.setAttribute("aria-pressed", String(img.classList.contains("zoomed"))); }
      else { img.removeAttribute("role"); img.removeAttribute("aria-pressed"); }
    }
    function zoom() {
      if (!img.classList.contains("zoomable")) return;
      img.classList.toggle("zoomed");
      img.parentNode.classList.toggle("zoomed", img.classList.contains("zoomed"));
      img.setAttribute("aria-pressed", String(img.classList.contains("zoomed")));
    }
    img.addEventListener("load", check);
    for (var fold = img.closest("details"); fold; fold = fold.parentElement.closest("details")) fold.addEventListener("toggle", check);
    if (img.complete) check();
    img.addEventListener("click", zoom);
    img.addEventListener("keydown", function (e) { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); zoom(); } });
  });

  /* ---- file view: rendered / source ---- */
  document.querySelectorAll(".fv").forEach(function (fv) {
    var btn = fv.querySelector(".fv-toggle");
    if (!btn) return;
    var key = foldKey.get(fv);
    function paint(source) {
      fv.classList.toggle("fv-show-src", source);
      fv.querySelector(".fv-md").hidden = source;
      fv.querySelector(".fv-src").hidden = !source;
      btn.textContent = source ? copy.viewRendered : copy.viewSource;
      btn.setAttribute("data-alt", source ? copy.viewSource : copy.viewRendered);
    }
    if (key && state.source[key]) paint(true);
    btn.addEventListener("click", function (e) {
      e.preventDefault(); /* the button sits in the fold's summary: switch view, don't fold */
      var source = !fv.classList.contains("fv-show-src");
      if (key) { state.source[key] = source || undefined; save(); }
      paint(source);
    });
  });

  /* ---- open items and folds survive reloads and re-renders ---- */
  document.querySelectorAll("details.item:not(.item-empty)").forEach(function (d) { track(d, "item|" + d.getAttribute("data-item")); });
  document.querySelectorAll("details.rest").forEach(function (d) { track(d, "rest|" + d.id.slice("rest-".length)); });
  foldKey.forEach(function (key, d) { track(d, key); });
  function track(d, key) {
    if (state.open[key]) d.open = true;
    d.addEventListener("toggle", function () { state.open[key] = d.open || undefined; save(); });
  }

  /* ---- the hash target opens, and on a fresh visit wins over the saved scroll position ---- */
  function hashTarget() {
    var id = location.hash.slice(1);
    try { id = decodeURIComponent(id); } catch (e) { /* use it as written */ }
    return id ? document.getElementById(id) : null;
  }
  function openHash() {
    var el = hashTarget();
    if (!el) return;
    var tray = el.closest(".rest");
    if (tray) tray.open = true;
    var item = el.closest(".item");
    if (item) { item.open = true; setActive(item.id); }
    if (el.classList.contains("ev-file")) { var view = el.querySelector("details.fv"); if (view) view.open = true; }
    el.scrollIntoView();
    state.lastHash = location.hash; save();
  }
  window.addEventListener("hashchange", openHash);
  /* A link to the hash already in the address bar fires no hashchange; it opens its target all the same. */
  document.addEventListener("click", function (e) {
    var a = e.target.closest && e.target.closest('a[href^="#"]');
    if (a && a.getAttribute("href") === location.hash) openHash();
  });
  /* A reload or back/forward to the hash already handled keeps what the viewer opened, closed and scrolled since. */
  var arrival = window.performance && performance.getEntriesByType ? performance.getEntriesByType("navigation")[0] : null;
  var revisit = !!arrival && (arrival.type === "reload" || arrival.type === "back_forward") && !!location.hash && location.hash === state.lastHash;

  /* An item with nothing behind its summary is a plain row: only its seen box and links respond. */
  document.querySelectorAll(".item-empty > summary").forEach(function (s) {
    s.addEventListener("click", function (e) { if (!e.target.closest("a[href], label, button, input, select, textarea")) e.preventDefault(); });
  });

  buildNav();
  if (!revisit) openHash();

  /* The saved scroll position, unless a fresh link names a target or the browser already placed the page. */
  window.addEventListener("load", function () {
    var el = revisit ? null : hashTarget();
    if (el) el.scrollIntoView();
    else if (scrollY < 4 && state.scrollY) scrollTo(0, state.scrollY);
  });
}
