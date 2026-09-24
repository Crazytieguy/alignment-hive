/* ============================================================
   DiffView v8 \u2014 a self-contained diff component.

   v8 changelog (DIFF-V8-SPEC.md):
   1. Honest change/unchanged folds and label disclosure: discloseRows,
      renderRows, renderProse, focused; change groups retained by codeRows.
   2. Shared row text metrics: lineRowEl and .dv-row > * in diff.css.
   3. One outer 3px change rule: .dv-row / .dv-add / .dv-del in diff.css.
   4. Verbatim annotation bands: attachLabels and .dv-label-row in diff.css.
   5. Quiet tools/header controls: render.build. One scope control per
      diff: the Relevant | All changes tabs when a focus leaves changes
      out, hairline folds otherwise. No per-diff Viewed box.
   6. Optional title leading the header: render and .dv-title in diff.css.
   7. Empty disclosure fallback: discloseRows and render.build.
   Visual review: no moved-code glyph (lineRowEl), neutral zero totals
   (render), page colour bridges, syntax colour/contrast, stable fold
   controls and narrow-screen source scrolling (diff.css).

   An authored review page embeds three things: this file, the
   contents of review-diff.css, and four pinned CDN scripts, in order \u2014
   jsdiff 9.0.0, highlight.js 11.12.0, diff-match-patch 1.0.5
   (preceded by a one-line `var module = { exports: {} };` shim),
   markdown-it 15.0.1. The file declares reviewDiff(global, copy); the
   page calls it once with window and its copy (reviewDiffMessages in
   messages.ts: `{name}` placeholders, {one, other} pairs by count),
   which defines global.DiffView. Then, once per diff:

     DiffView.render(document.getElementById("hunk-3"), {
       path: "src/lib.rs",       // drives language + prose/source choice
       status: "modified",       // modified | added | deleted
       old:  "<whole old file>",
       new:  "<whole new file>",
       scope: "review-42"        // namespaces remembered panel state
     });

   Only those five fields are needed. Whole file contents rather than
   a patch, because collapse and expand-to-context need the lines a patch
   throws away. Panels start folded to the header row.

   Optional fields (v7 matching and prose rendering are retained):
     focus: [start, end],       // head, 1-based inclusive full-source lines,
                                //   or a list: [[14, 14], [279, 279]]
     oldFocus: [start, end],    // base, same coordinates; independent of focus
     labels: [{ line, where }], // head source line + caller-authored annotation
     title: "the caller side", // optional header lead, before the muted path
     stateKey: "item-3/diff-1" // remember tab, folds, source view, collapse
   Whole focused change groups include three context
   lines per side; gaps smaller than four rows stay visible. Without focus,
   all change hunks are shown. Labels always disclose their head row/block.
   Rendered prose remains indivisible, including front matter and fences;
   its content, matching and marks retain the v7 rendering rules.
   Markdown remains rendered unless the reader selects its source view.
   Each label follows its head row or prose block. A panel with a focus
   that leaves some changes out gets two header tabs, "Relevant +a -b" and
   "All changes +c -d" (counts from the diff), and no other scope control:
   Relevant marks each left-out stretch with a hairline, All changes shows
   every change and brackets each relevant stretch with hairlines (the
   first labelled "relevant"), with the other changes' tints washed out.
   A focus list is relevant wherever any range is. Every hidden stretch, in
   code or rendered Markdown, is one hairline fold stepped open from either
   end, in both tabs; Markdown's source view is a header toggle. The chosen
   tab and the folds are kept with stateKey (localStorage
   "dv-state:<scope>:<stateKey>"). The returned handle is { element, rerender, file };
   DiffView.countChanges(file) gives the +/- totals for a file. The
   review-page generator validates Markdown image destinations before
   embedding files; this component preserves the original Markdown
   rendering behavior.

   hljs and markdownit are optional \u2014 without them code loses colour
   and markdown falls back to a source diff.
   ============================================================ */

function reviewDiff(global, copy) {
  "use strict";

  /* ---------------- tuning ---------------- */

  var CTX = 3;                   // context lines kept around a change
  var STEP = 10;                 // lines revealed per expander click
  var PROSE_STEP = 3;            // rendered Markdown blocks revealed per click
  var MAX_LINE_DISTANCE = 0.3;   // delta's gate before any intraline marks
  var ALIGN_DICE = 0.5;          // prose blocks below this are not a pair
  var INLINE_MAX = 0.12;          // a block changed this little gets inline word marks
  var STACK_MAX = 0.30;           // marked stacks
  var REFINE_MAX_LEN = 140;      // char-level refinement only on short runs
  var REFINE_MIN_SAME = 0.4;

  /* ---------------- small utilities ---------------- */

  function splitLines(t) {
    if (!t) return [];
    var l = t.split("\n");
    if (l.length && l[l.length - 1] === "") l.pop();
    return l;
  }

  function escHtml(s) {
    return String(s).replace(/[&<>]/g, function (c) {
      return c === "&" ? "&amp;" : c === "<" ? "&lt;" : "&gt;";
    });
  }

  function isBlank(s) { return /^\s*$/.test(s); }

  /* The page's copy: fill `{name}` placeholders; pick {one, other} by n. */
  function say(template, values) {
    return template.replace(/\{(\w+)\}/g, function (m, k) { return String(values[k]); });
  }
  function plural(forms, n) { return say(n === 1 ? forms.one : forms.other, { n: n }); }

  function normWs(s) { return s.replace(/\s+/g, " ").trim(); }

  function intersects(range, start, end) {
    return !!range && start != null && range[0] <= end && range[1] >= start;
  }

  /* focus / oldFocus: one [start, end] or a list of them; null when empty. */
  function rangeList(r) {
    if (!r || !r.length) return null;
    return Array.isArray(r[0]) ? r : [r];
  }

  function inRanges(r, start, end) {
    var list = rangeList(r);
    return !!list && list.some(function (x) { return intersects(x, start, end); });
  }

  function hasFocusRanges(file) { return !!(rangeList(file.focus) || rangeList(file.oldFocus)); }

  function focused(file, oldRange, newRange) {
    return (oldRange && inRanges(file.oldFocus, oldRange[0], oldRange[1])) ||
      (newRange && (inRanges(file.focus, newRange[0], newRange[1]) ||
        (file.labels || []).some(function (label) {
          return intersects(newRange, label.line, label.line);
        })));
  }

  function attachLabels(node, file, range) {
    if (!range) return node;
    (file.labels || []).forEach(function (label) {
      if (!intersects(range, label.line, label.line)) return;
      var chip = el("div", "dv-review-label dv-label-row", "\u2192 " + label.where);
      chip.setAttribute("data-dv-label-line", String(label.line));
      node.appendChild(chip);
    });
    return node;
  }

  /* Plan disclosure from full rows, never from already folded output. Groups
     are indivisible (including old-only rows and absorbed blank lines).
     Elsewhere expansion records group keys, so revealing one region cannot
     rename or accidentally reveal a neighbouring region. */
  function disclosurePlan(rows, file, opened, prefix, every) {
    var visible = [], groups = [], byKey = Object.create(null);
    var hasFocus = hasFocusRanges(file);
    function isFocused(r) {
      return focused(file, r.oldNo == null ? null : [r.oldNo, r.oldNo],
        r.newNo == null ? null : [r.newNo, r.newNo]);
    }
    function reveal(start, end) {
      for (var i = Math.max(0, start - CTX); i < Math.min(rows.length, end + CTX); i++) {
        visible[i] = true;
      }
    }
    rows.forEach(function (r, i) {
      if (isFocused(r)) reveal(i, i + 1);
      if (!r.group) return;
      var key = prefix + r.group, g = byKey[key];
      if (!g) { g = byKey[key] = { key: key, start: i, end: i + 1, focused: false, add: false, del: false }; groups.push(g); }
      g.end = i + 1;
      g.focused = g.focused || isFocused(r);
      if (r.cls === "add") g.add = true;
      if (r.cls === "del") g.del = true;
    });
    /* Only a replacement (removed and added lines together) is indivisible. A run
       of pure additions or deletions, such as a whole new file, can be cut down to
       the focused lines and their context. */
    function whole(g) { return g.add && g.del; }
    groups.forEach(function (g) {
      if (!hasFocus || every || (g.focused && whole(g)) || (opened[g.key] && opened[g.key].all)) reveal(g.start, g.end);
    });
    /* Context must not expose half a neighbouring replacement. */
    var wholeAt = [];
    groups.forEach(function (g) {
      if (!whole(g)) return;
      for (var i = g.start; i < g.end; i++) wholeAt[i] = g;
      if (visible.slice(g.start, g.end).some(Boolean)) {
        for (var k = g.start; k < g.end; k++) visible[k] = true;
      }
    });
    return { visible: visible, groups: groups, hasFocus: hasFocus, wholeAt: wholeAt };
  }

  function isChange(r) { return r.cls === "add" || r.cls === "del"; }

  /* The rows the Relevant tab shows: the author's focus with its whole change
     groups, short gaps filled in. Falls back to every change if the focus
     discloses no row. */
  function excerptMask(rows, file, prefix) {
    var v = disclosurePlan(rows, file, {}, prefix, false).visible, i = 0;
    if (!v.some(Boolean)) v = disclosurePlan(rows, file, {}, prefix, true).visible;
    while (i < rows.length) {
      if (v[i]) { i++; continue; }
      var start = i;
      while (i < rows.length && !v[i]) i++;
      if (i - start <= 3) for (var k = start; k < i; k++) v[k] = true;
    }
    return v;
  }

  /* The Relevant tab's +/- counts, from the same rows it shows. */
  /* Added and removed rows among `rows`. */
  function changeCounts(rows) {
    var c = { add: 0, del: 0 };
    rows.forEach(function (r) { if (r.cls === "add") c.add++; if (r.cls === "del") c.del++; });
    return c;
  }
  function relevantCounts(file) {
    var rows = codeRows(prepareCode(file), {}, { context: Infinity });
    var mask = excerptMask(rows, file, "r");
    return changeCounts(rows.filter(function (r, i) { return mask[i]; }));
  }

  /* A focused review panel has two views, chosen by its tabs: "relevant"
     (the author's excerpt; stretches holding other changes are left out
     with a marker) and "all" (every change; rows of the excerpt are
     flagged `core`). Unchanged stretches fold the usual way in both. */
  function discloseRows(rows, file, opened, all, prefix, view) {
    if (all) return rows;
    var plan = disclosurePlan(rows, file, opened, prefix, view === "all");
    var visible = plan.visible, hasFocus = plan.hasFocus;
    var core = hasFocus ? excerptMask(rows, file, prefix) : null;
    var result = [], i = 0;
    function take(start, end) {
      for (var k = start; k < end; k++) {
        result.push(core ? Object.assign({}, rows[k], { core: !!core[k] }) : rows[k]);
      }
    }
    while (i < rows.length) {
      if (visible[i]) { take(i, i + 1); i++; continue; }
      var start = i;
      while (i < rows.length && !visible[i]) i++;
      var n = i - start;
      if (n <= 3) { take(start, i); continue; }
      var changed = rows.slice(start, i).some(isChange);
      var key = prefix + (changed ? "gap" : "eq") + start;
      var st = opened[key] || { top: 0, bottom: 0, all: false };
      /* A step from either end never stops inside a replacement (removed and
         added lines together), which would show old lines as deletions while
         their new lines stay folded. The taken span runs on to the group's far
         edge; pure additions or removals may still be cut. The fold row carries
         the final top/bottom so the next step starts from there. */
      var top = Math.min(st.top, n), bottom = Math.min(st.bottom, n);
      var cut = start + top, g = plan.wholeAt[cut];
      if (top && g && g === plan.wholeAt[cut - 1]) top = Math.min(g.end, i) - start;
      cut = i - bottom; g = plan.wholeAt[cut];
      if (bottom && g && g === plan.wholeAt[cut - 1]) bottom = i - Math.max(g.start, start);
      if (st.all || n - top - bottom <= 3) {
        take(start, i);
        continue;
      }
      take(start, start + top);
      var still = rows.slice(start + top, i - bottom), counts = changeCounts(still);
      result.push({ type: "fold", key: key, hidden: still.length, add: counts.add, del: counts.del,
        top: top, bottom: bottom, canTop: start > 0, canBottom: i < rows.length });
      if (bottom) take(i - bottom, i);
    }
    if (hasFocus && !result.some(function (r) { return r.type === "line"; }) &&
        rows.some(isChange)) {
      return discloseRows(rows, Object.assign({}, file, { focus: null, oldFocus: null }), opened, false, prefix, view);
    }
    return result;
  }

  /* All changes brackets each relevant stretch with the same hairline:
     labelled "relevant" above it, plain below it. */
  function bracketMarker(open) {
    var b = el("div", "dv-bracket " + (open ? "dv-bracket-open" : "dv-bracket-close"));
    b.setAttribute("role", "separator");
    b.setAttribute("aria-label", open ? copy.relevantStart : copy.relevantEnd);
    if (open) b.appendChild(el("span", null, copy.relevant));
    return b;
  }

  /* words only: emphasis, ticks, heading marks and bullets dropped, so two
     blocks equal under this changed formatting, not content */
  function proseNorm(s) {
    return s
      .replace(/[*_`~]/g, "")
      .replace(/^\s*(?:[-+*]|\d+\.)\s+/gm, "")
      .replace(/^\s*#+\s*/gm, "")
      .replace(/\s+/g, " ")
      .trim();
  }

  function extOf(path) {
    var m = /\.([A-Za-z0-9]+)$/.exec(path || "");
    return m ? m[1].toLowerCase() : "";
  }

  var LANG_BY_EXT = {
    rs: "rust", ts: "typescript", tsx: "typescript", js: "javascript",
    mjs: "javascript", json: "json", sh: "bash", bash: "bash", zsh: "bash",
    toml: "ini", yml: "yaml", yaml: "yaml", md: "markdown", css: "css",
    html: "xml", py: "python", sql: "sql", lock: "ini"
  };

  function langOf(path) { return LANG_BY_EXT[extOf(path)] || null; }
  function isMarkdown(path) { return extOf(path) === "md"; }

  function el(tag, cls, text) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }

  function pathHtml(p) {
    var i = p.lastIndexOf("/");
    /* A long path breaks only after a slash; the file name stays whole. */
    if (i === -1) return '<span class="dv-fname">' + escHtml(p) + "</span>";
    return '<span class="dv-dir">' + escHtml(p.slice(0, i + 1)).replace(/\//g, "/<wbr>") + "</span>" +
      '<span class="dv-fname">' + escHtml(p.slice(i + 1)) + "</span>";
  }

  /* ---------------- whole-file highlighting, split per line ----------------
     Highlighting each diff line on its own cannot track multi-line
     constructs. Highlight the file once, cut at newlines, close and
     reopen the open <span> stack. */

  function highlightLines(text, lang) {
    var plain = function () { return splitLines(text).map(escHtml); };
    if (!lang || !global.hljs || !global.hljs.getLanguage(lang)) return plain();
    var html;
    try {
      html = global.hljs.highlight(text, { language: lang, ignoreIllegals: true }).value;
    } catch (e) { return plain(); }
    var out = [], stack = [], cur = "";
    var re = /<span [^>]*>|<\/span>|\n|[^<\n]+|</g, m;
    while ((m = re.exec(html)) !== null) {
      var tok = m[0];
      if (tok === "\n") {
        out.push(cur + repeat("</span>", stack.length));
        cur = stack.join("");
      } else if (tok === "</span>") {
        stack.pop();
        cur += tok;
      } else if (tok.charAt(0) === "<" && tok !== "<") {
        stack.push(tok);
        cur += tok;
      } else {
        cur += tok;
      }
    }
    out.push(cur + repeat("</span>", stack.length));
    if (out.length && out[out.length - 1] === "") out.pop();
    var want = splitLines(text).length;
    while (out.length < want) out.push("");
    return out;
  }

  function repeat(s, n) {
    var o = "";
    for (var i = 0; i < n; i++) o += s;
    return o;
  }

  /* Prose punctuation and whitespace are separate tokens. Whitespace runs
     compare equal across reflow; semantic cleanup on encoded tokens would
     erase valid word matches, so prose uses the minimal token edit script. */
  var CODE_TOKEN_RE = /\s+|[A-Za-z0-9_]+|[^\sA-Za-z0-9_]/g;
  var PROSE_TOKEN_RE = /\s+|[\p{L}\p{N}\p{M}_]+|[^\s\p{L}\p{N}\p{M}_]/gu;

  function codeTokens(s) { return s.match(CODE_TOKEN_RE) || []; }
  function proseTokens(s) { return s.match(PROSE_TOKEN_RE) || []; }

  function proseDiff(a, b) {
    var parts = [], changed = 0, total = 0;
    global.Diff.diffArrays(proseTokens(a), proseTokens(b), {
      comparator: function (a, b) { return a === b || (isBlank(a) && isBlank(b)); }
    }).forEach(function (p) {
      var op = p.removed ? -1 : p.added ? 1 : 0;
      var count = p.value.filter(function (t) { return !isBlank(t); }).length;
      parts.push([op, p.value.join(""), count]);
      total += count;
      if (op) changed += count;
    });
    /* Keep a phrase replacement together across equal whitespace, without
       absorbing any equal word or changing the fraction. */
    var grouped = [], oldRun = "", newRun = "";
    function flush() {
      if (oldRun) grouped.push([-1, oldRun]);
      if (newRun) grouped.push([1, newRun]);
      oldRun = newRun = "";
    }
    parts.forEach(function (p, i) {
      if (p[0] === -1) oldRun += p[1];
      else if (p[0] === 1) newRun += p[1];
      else if ((oldRun || newRun) && isBlank(p[1]) && parts[i + 1] && parts[i + 1][0]) {
        oldRun += p[1]; newRun += p[1];
      } else { flush(); grouped.push(p); }
    });
    flush();
    return { parts: grouped, changed: changed, total: total };
  }

  var dmp = new global.diff_match_patch();
  dmp.Diff_Timeout = 1.0;

  /* Map each distinct token to one code point below the surrogate range,
     diff the encoded strings, cleanupSemantic (never cleanupEfficiency),
     map back. Returns parts plus token counts. */
  function tokenDiff(at, bt) {
    var total = at.length + bt.length;
    if (total > 50000) {
      return {
        parts: [[-1, at.join(""), at.length], [1, bt.join(""), bt.length]],
        changed: total, total: total
      };
    }
    var map = Object.create(null), list = [];
    var enc = function (arr) {
      var s = "";
      for (var i = 0; i < arr.length; i++) {
        var t = arr[i], c = map[" " + t];
        if (c === undefined) {
          if (list.length >= 0xd000) return null;
          c = String.fromCharCode(list.length);
          map[" " + t] = c;
          list.push(t);
        }
        s += c;
      }
      return s;
    };
    var ea = enc(at), eb = ea === null ? null : enc(bt);
    if (ea === null || eb === null) {
      return {
        parts: [[-1, at.join(""), at.length], [1, bt.join(""), bt.length]],
        changed: total, total: total
      };
    }
    var d = dmp.diff_main(ea, eb, false);
    dmp.diff_cleanupSemantic(d);
    var parts = [], changed = 0, seen = 0;
    for (var i = 0; i < d.length; i++) {
      var op = d[i][0], chars = d[i][1], s = "";
      for (var j = 0; j < chars.length; j++) s += list[chars.charCodeAt(j)];
      parts.push([op, s, chars.length]);
      seen += chars.length;
      if (op !== 0) changed += chars.length;
    }
    return { parts: parts, changed: changed, total: seen };
  }

  function codeWordDiff(a, b) { return tokenDiff(codeTokens(a), codeTokens(b)).parts; }

  /* delta's line-similarity metric: unchanged runs count double in the
     denominator, so the measure is deliberately forgiving. 0 = identical. */
  function lineDistance(a, b) {
    if (a === b) return 0;
    var parts = codeWordDiff(a, b), numer = 0, denom = 0;
    for (var i = 0; i < parts.length; i++) {
      var w = parts[i][1].length;
      if (parts[i][0] === 0) denom += 2 * w;
      else { numer += w; denom += w; }
    }
    return denom === 0 ? 0 : numer / denom;
  }

  /* ---------------- line diff, grouped ---------------- */

  function diffOps(oldLines, newLines) {
    var parts = global.Diff.diffArrays(oldLines, newLines);
    var ops = [], oi = 0, ni = 0;
    for (var i = 0; i < parts.length; i++) {
      var p = parts[i];
      if (p.added) {
        ops.push({ kind: "add", lines: p.value, oldStart: oi, newStart: ni });
        ni += p.value.length;
      } else if (p.removed) {
        ops.push({ kind: "del", lines: p.value, oldStart: oi, newStart: ni });
        oi += p.value.length;
      } else {
        ops.push({ kind: "eq", lines: p.value, oldStart: oi, newStart: ni });
        oi += p.value.length;
        ni += p.value.length;
      }
    }
    return ops;
  }

  /* Group every change into "all the removals, then all the additions".
     jsdiff can emit del/add/del/add across the blank lines that separate
     paragraphs, which is what made round 1's prose hunks read as removed
     and added lines shuffled together; a blank-only equal run of one or
     two lines between two changes is absorbed into the group instead of
     splitting it. */
  function buildGroups(ops) {
    var out = [], i = 0;
    while (i < ops.length) {
      var op = ops[i];
      var last = out.length ? out[out.length - 1] : null;
      if (op.kind === "eq") {
        var absorb = last && last.type === "change" && op.lines.length <= 2 &&
          op.lines.every(isBlank) && ops[i + 1] && ops[i + 1].kind !== "eq";
        if (absorb) {
          for (var k = 0; k < op.lines.length; k++) {
            last.mids.push({
              text: op.lines[k],
              oldNo: op.oldStart + k + 1,
              newNo: op.newStart + k + 1
            });
          }
          last.open = true;
          i++;
          continue;
        }
        out.push({ type: "eq", op: op, key: "e" + i });
        i++;
        continue;
      }
      var g;
      if (last && last.type === "change" && last.open) { g = last; g.open = false; }
      else { g = { type: "change", dels: [], adds: [], mids: [], key: "c" + i }; out.push(g); }
      while (i < ops.length && ops[i].kind !== "eq") {
        var o = ops[i];
        for (var j = 0; j < o.lines.length; j++) {
          if (o.kind === "del") {
            g.dels.push({ text: o.lines[j], oldNo: o.oldStart + j + 1 });
          } else {
            g.adds.push({ text: o.lines[j], newNo: o.newStart + j + 1 });
          }
        }
        i++;
      }
    }
    return out;
  }

  /* ---------------- code rows ---------------- */

  var CODE_CACHE = new WeakMap();

  function prepareCode(file) {
    var c = CODE_CACHE.get(file);
    if (c) return c;
    var lang = langOf(file.path);
    var oldLines = (file.old || "").match(/[^\n]*\n|[^\n]+$/g) || [];
    var newLines = (file.new || "").match(/[^\n]*\n|[^\n]+$/g) || [];
    var ops = diffOps(oldLines, newLines);
    ops.forEach(function (op) { op.lines = op.lines.map(function (line) { return line.replace(/\n$/, ""); }); });
    c = {
      oldMissingNewline: file.new.endsWith("\n") && file.old && !file.old.endsWith("\n") ? oldLines.length : null,
      newMissingNewline: file.old.endsWith("\n") && file.new && !file.new.endsWith("\n") ? newLines.length : null,
      ops: ops,
      groups: buildGroups(ops),
      oldHi: highlightLines(file.old || "", lang),
      newHi: highlightLines(file.new || "", lang)
    };
    CODE_CACHE.set(file, c);
    return c;
  }

  function countChanges(file) {
    var ops = prepareCode(file).ops, add = 0, del = 0;
    ops.forEach(function (op) {
      if (op.kind === "add") add += op.lines.length;
      if (op.kind === "del") del += op.lines.length;
    });
    return { add: add, del: del };
  }

  /* Rows for one prepared file. `opened` maps a gap key to how far it has
     been expanded. Removals always precede additions inside a group; a
     removal is paired with an addition for intraline marks only when the
     two are close enough under delta's gate. */
  function codeRows(prep, opened, options) {
    var groups = prep.groups, oldHi = prep.oldHi, newHi = prep.newHi;
    var rows = [];
    var ctx = options && options.context != null ? options.context : CTX;

    groups.forEach(function (g, gi) {
      if (g.type === "eq") {
        var op = g.op, n = op.lines.length;
        var head = gi === 0 ? 0 : ctx;
        var tail = gi === groups.length - 1 ? 0 : ctx;
        var st = opened[g.key] || { top: 0, bottom: 0, all: false };
        var topShown = head + st.top, bottomShown = tail + st.bottom;
        if (ctx === Infinity || st.all || n <= head + tail + 1 || topShown + bottomShown >= n) {
          for (var k = 0; k < n; k++) rows.push(eqRow(op, k));
          return;
        }
        for (var t = 0; t < topShown; t++) rows.push(eqRow(op, t));
        rows.push({
          type: "fold", key: g.key,
          hidden: n - topShown - bottomShown,
          canTop: gi !== 0,
          canBottom: gi !== groups.length - 1
        });
        for (var b = n - bottomShown; b < n; b++) rows.push(eqRow(op, b));
        return;
      }

      var D = g.dels, A = g.adds, pairs = [];
      var lim = Math.min(D.length, A.length);
      for (var i = 0; i < lim; i++) {
        if (D[i].text === A[i].text) continue;
        if (lineDistance(D[i].text, A[i].text) < MAX_LINE_DISTANCE) {
          pairs[i] = tokenDiff(codeTokens(D[i].text), codeTokens(A[i].text)).parts;
        }
      }

      D.forEach(function (d, i) {
        rows.push({
          type: "line", cls: "del", group: g.key,
          oldNo: d.oldNo, newNo: null,
          html: pairs[i] ? markUpLine(d.text, pairs[i], -1, oldHi[d.oldNo - 1]) || oldHi[d.oldNo - 1]
                         : oldHi[d.oldNo - 1]
        });
      });
      g.mids.forEach(function (m) {
        rows.push({
          type: "line", cls: "eq", group: g.key,
          oldNo: m.oldNo, newNo: m.newNo,
          html: oldHi[m.oldNo - 1]
        });
      });
      A.forEach(function (a, i) {
        rows.push({
          type: "line", cls: "add", group: g.key,
          oldNo: null, newNo: a.newNo,
          html: pairs[i] ? markUpLine(a.text, pairs[i], 1, newHi[a.newNo - 1]) || newHi[a.newNo - 1]
                         : newHi[a.newNo - 1]
        });
      });
    });

    function eqRow(op, k) {
      return {
        type: "line", cls: "eq",
        oldNo: op.oldStart + k + 1,
        newNo: op.newStart + k + 1,
        html: oldHi[op.oldStart + k]
      };
    }

    rows.forEach(function (r) {
      r.missingNewline = (r.oldNo != null && r.oldNo === prep.oldMissingNewline) ||
        (r.newNo != null && r.newNo === prep.newMissingNewline);
    });
    return rows;
  }

  /* Overlay marks on an already highlighted line: walk the highlighted
     HTML tracking plain-text offset and wrap the changed ranges. */
  function markUpLine(plain, parts, side, hiHtml) {
    var ranges = [], pos = 0;
    for (var i = 0; i < parts.length; i++) {
      var op = parts[i][0], s = parts[i][1];
      if (op === 0) { pos += s.length; continue; }
      if (op === side) { ranges.push([pos, pos + s.length]); pos += s.length; }
    }
    if (!ranges.length) return null;
    var cls = side === 1 ? "dv-ins" : "dv-del-mark";
    var src = hiHtml != null ? hiHtml : escHtml(plain);
    var root = el("div"), off = 0, ri = 0;
    root.innerHTML = src;
    var walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT), nodes = [], node;
    while ((node = walker.nextNode())) nodes.push(node);
    nodes.forEach(function (text) {
      var value = text.nodeValue, local = 0, fragment = document.createDocumentFragment();
      while (local < value.length) {
        while (ri < ranges.length && off >= ranges[ri][1]) ri++;
        var inRange = ri < ranges.length && off >= ranges[ri][0];
        var end = ri < ranges.length ? ranges[ri][inRange ? 1 : 0] : Infinity;
        var length = Math.min(value.length - local, end - off);
        var piece = value.slice(local, local + length);
        fragment.appendChild(inRange ? el("span", cls, piece) : document.createTextNode(piece));
        local += length; off += length;
      }
      text.parentNode.replaceChild(fragment, text);
    });
    return root.innerHTML;
  }

  /* ---------------- markdown ---------------- */

  var md = global.markdownit
    ? global.markdownit({ html: false, linkify: false, breaks: false })
    : null;

  /* Private-use sentinels, built at runtime so this source never carries
     the raw characters. They survive markdown rendering as ordinary text
     and cannot create invalid nesting. */
  var S_INS_O = String.fromCharCode(0xe000);
  var S_INS_C = String.fromCharCode(0xe001);
  var S_DEL_O = String.fromCharCode(0xe002);
  var S_DEL_C = String.fromCharCode(0xe003);
  var SENTINEL_RE = new RegExp("[" + S_INS_O + "-" + S_DEL_C + "]");
  var SENTINEL_G = new RegExp("[" + S_INS_O + "-" + S_DEL_C + "]", "g");

  function frontMatter(src) {
    if (!src || src.slice(0, 4) !== "---\n") return null;
    var end = src.indexOf("\n---", 3);
    if (end === -1) return null;
    var stop = src.indexOf("\n", end + 1);
    return { src: src.slice(0, stop === -1 ? src.length : stop + 1), end: stop === -1 ? src.length - 1 : stop };
  }

  function mdBlocks(src) {
    var blocks = [];
    if (!src) return blocks;
    var body = src, offset = 0;
    var fm = frontMatter(src);
    if (fm) {
      offset = splitLines(fm.src).length;
      blocks.push({ src: fm.src.replace(/\n$/, ""), kind: "frontmatter", range: [1, offset] });
      body = src.slice(fm.end + 1);
    }
    if (!md) {
      body.split(/\n{2,}/).forEach(function (s) {
        if (s.trim()) blocks.push({ src: s, kind: "para" });
      });
      return finish(blocks);
    }
    var lines = body.split("\n"), tokens, env = {};
    try { tokens = md.parse(body, env); } catch (e) { tokens = []; }
    var fences = tokens.filter(function (t) { return t.type === "fence" && t.map; });
    var slice = function (map) { return lines.slice(map[0], map[1]).join("\n").replace(/\s+$/, ""); };
    function sourceBlock(map, kind) {
      blocks.push({ src: slice(map), kind: kind, range: [offset + map[0] + 1, offset + map[1]] });
    }
    /* Fences nested in list items or quotes are independent source blocks,
       not prose tokens. Use markdown-it's content to remove container syntax. */
    function segments(map, kind) {
      var cursor = map[0];
      var nested = fences.filter(function (t) {
        return t.map[0] >= map[0] && t.map[1] <= map[1];
      });
      nested.forEach(function (t) {
        if (cursor < t.map[0]) sourceBlock([cursor, t.map[0]], kind);
        blocks.push({ src: t.markup + t.info + "\n" + t.content + t.markup, kind: "code",
          range: [offset + t.map[0] + 1, offset + t.map[1]],
          contentStart: offset + t.map[0] + 2 });
        cursor = t.map[1];
      });
      if (cursor < map[1]) sourceBlock([cursor, map[1]], kind);
    }
    for (var i = 0; i < tokens.length; i++) {
      var t = tokens[i];
      if (t.level !== 0 || !t.map || t.nesting === -1) continue;
      if (t.type === "bullet_list_open" || t.type === "ordered_list_open") {
        var depth = 0;
        for (var j = i + 1; j < tokens.length; j++) {
          var u = tokens[j];
          if (u.type === "bullet_list_open" || u.type === "ordered_list_open") depth++;
          if (u.type === "bullet_list_close" || u.type === "ordered_list_close") {
            if (depth === 0) { i = j; break; }
            depth--;
          }
          if (depth === 0 && u.type === "list_item_open" && u.map) {
            segments(u.map, "listitem");
          }
        }
        continue;
      }
      var kind = (t.type === "fence" || t.type === "code_block") ? "code"
        : t.type === "table_open" ? "table"
        : t.type === "heading_open" ? "heading" : "para";
      segments(t.map, kind);
      if (t.nesting === 1) {
        var end = t.map[1];
        while (i + 1 < tokens.length) {
          var v = tokens[i + 1];
          if (v.level === 0 && v.map && v.map[0] >= end) break;
          i++;
        }
      }
    }
    // Definitions have no display tokens. Keep uncovered source as evidence.
    var covered = [];
    blocks.forEach(function (b) {
      for (var line = b.range[0] - offset - 1; line < b.range[1] - offset; line++) covered[line] = true;
    });
    for (var line = 0; line < lines.length; line++) {
      if (covered[line] || !lines[line].trim()) continue;
      var start = line;
      while (line + 1 < lines.length && !covered[line + 1] && lines[line + 1].trim()) line++;
      sourceBlock([start, line + 1], "source");
    }
    blocks.sort(function (a, b) { return a.range[0] - b.range[0]; });
    blocks.forEach(function (b) { b.env = env; });
    return finish(blocks);
  }

  function finish(blocks) {
    return blocks.filter(function (b) { return b.src.trim() !== ""; }).map(function (b, i) {
      b.i = i;
      b.norm = normWs(b.src.replace(/[*_`#>\-]/g, " "));
      b.exact = b.src;
      return b;
    });
  }

  function dice(a, b) {
    if (a === b) return 1;
    if (a.length < 2 || b.length < 2) return 0;
    var bg = function (s) {
      var m = Object.create(null);
      for (var i = 0; i < s.length - 1; i++) {
        var k = " " + s.slice(i, i + 2);
        m[k] = (m[k] || 0) + 1;
      }
      return m;
    };
    var A = bg(a), B = bg(b), inter = 0, total = 0, k;
    for (k in A) total += A[k];
    for (k in B) { total += B[k]; if (A[k]) inter += Math.min(B[k], A[k]); }
    return total === 0 ? 0 : (2 * inter) / total;
  }

  /* LCS over exact block text, then a similarity pass over what LCS left
     unmatched. Never pair blocks by array index. */
  var MAX_ALIGNMENT_CELLS = 250000;
  function alignBlocks(oldB, newB) {
    var n = oldB.length, m = newB.length, i, j;
    if ((n + 1) * (m + 1) > MAX_ALIGNMENT_CELLS) return null;
    var L = [];
    for (i = 0; i <= n; i++) L.push(new Int32Array(m + 1));
    for (i = n - 1; i >= 0; i--) {
      for (j = m - 1; j >= 0; j--) {
        L[i][j] = oldB[i].exact === newB[j].exact
          ? L[i + 1][j + 1] + 1
          : Math.max(L[i + 1][j], L[i][j + 1]);
      }
    }
    var anchors = [];
    i = 0; j = 0;
    while (i < n && j < m) {
      if (oldB[i].exact === newB[j].exact) { anchors.push([i, j]); i++; j++; }
      else if (L[i + 1][j] >= L[i][j + 1]) i++;
      else j++;
    }

    var pairs = [], pi = 0, pj = 0;
    var gapPass = function (oi0, oi1, nj0, nj1) {
      var cand = [], x, y;
      for (x = oi0; x < oi1; x++) {
        for (y = nj0; y < nj1; y++) {
          var s = dice(oldB[x].norm, newB[y].norm);
          if (oldB[x].kind === newB[y].kind && (s >= ALIGN_DICE || oldB[x].kind === "frontmatter")) cand.push({ x: x, y: y, s: s });
        }
      }
      cand.sort(function (a, b) { return b.s - a.s; });
      var ux = Object.create(null), uy = Object.create(null), matched = [];
      cand.forEach(function (c) {
        if (ux[c.x] || uy[c.y] || matched.some(function (p) {
          return (p.x - c.x) * (p.y - c.y) < 0;
        })) return;
        ux[c.x] = uy[c.y] = true;
        matched.push(c);
      });
      matched.sort(function (a, b) { return a.x - b.x || a.y - b.y; });
      var cx = oi0, cy = nj0;
      matched.forEach(function (c) {
        for (x = cx; x < c.x; x++) if (!ux[x]) pairs.push({ old: oldB[x], new: null });
        for (y = cy; y < c.y; y++) if (!uy[y]) pairs.push({ old: null, new: newB[y] });
        pairs.push({ old: oldB[c.x], new: newB[c.y], changed: true });
        cx = c.x + 1; cy = c.y + 1;
      });
      for (x = cx; x < oi1; x++) if (!ux[x]) pairs.push({ old: oldB[x], new: null });
      for (y = cy; y < nj1; y++) if (!uy[y]) pairs.push({ old: null, new: newB[y] });
    };

    anchors.forEach(function (a) {
      gapPass(pi, a[0], pj, a[1]);
      pairs.push({ old: oldB[a[0]], new: newB[a[1]], same: true });
      pi = a[0] + 1; pj = a[1] + 1;
    });
    gapPass(pi, n, pj, m);

    /* verbatim relocation: an unpaired removal whose text reappears as an
       unpaired addition is a move, rendered once with a quiet chip */
    var byText = Object.create(null);
    pairs.forEach(function (p) {
      if (p.new && !p.old) (byText[" " + p.new.exact] || (byText[" " + p.new.exact] = [])).push(p);
    });
    pairs.forEach(function (p) {
      if (!p.old || p.new || p.moved) return;
      var hits = byText[" " + p.old.exact];
      if (!hits || !hits.length) return;
      var partner = hits.shift();
      if (!partner || partner.moved) return;
      p.moved = "gone";
      partner.moved = "here";
      partner.movedOld = p.old;
    });
    return pairs.filter(function (p) { return p.moved !== "gone"; });
  }

  /* Character-level refinement of a removed/added run pair, so a small
     edit inside a longer run reads as the few characters it touched. */
  function refine(delText, insText) {
    if (delText.length > REFINE_MAX_LEN || insText.length > REFINE_MAX_LEN) return null;
    var d = dmp.diff_main(delText, insText);
    dmp.diff_cleanupSemantic(d);
    var same = 0, total = 0;
    d.forEach(function (p) { total += p[1].length; if (p[0] === 0) same += p[1].length; });
    if (!total || same / total < REFINE_MIN_SAME) return null;
    return d;
  }

  /* A mark must never be, start with, or end with whitespace: a lone
     highlighted space is what made round 1's character diffs look as
     though a space had failed to match a space. Edge whitespace is
     pushed out of the mark, whitespace-only marks become plain text,
     and a gap of one to three spaces between two marks is closed so a
     highlight stays continuous instead of speckling. */
  function tidy(items) {
    var pushed = [];
    items.forEach(function (it) {
      if (!it.t) return;
      if (!it.m) { pushed.push(it); return; }
      var m = /^(\s*)([\s\S]*?)(\s*)$/.exec(it.t);
      if (!m[2]) { pushed.push({ m: false, t: it.t }); return; }
      if (m[1]) pushed.push({ m: false, t: m[1] });
      pushed.push({ m: it.m, t: m[2] });
      if (m[3]) pushed.push({ m: false, t: m[3] });
    });
    var runs = joinRuns(pushed);
    for (var i = 1; i + 1 < runs.length; i++) {
      if (!runs[i].m && runs[i - 1].m && runs[i - 1].m === runs[i + 1].m &&
          /^\s{1,3}$/.test(runs[i].t)) {
        runs[i].m = runs[i - 1].m;
      }
    }
    return joinRuns(runs);
  }

  function joinRuns(items) {
    var out = [];
    items.forEach(function (it) {
      if (!it.t) return;
      var last = out.length ? out[out.length - 1] : null;
      if (last && last.m === it.m) last.t += it.t;
      else out.push({ m: it.m, t: it.t });
    });
    return out;
  }

  function emitMarks(items) {
    var s = "";
    items.forEach(function (it) {
      if (it.m === "ins") s += S_INS_O + it.t + S_INS_C;
      else if (it.m === "del") s += S_DEL_O + it.t + S_DEL_C;
      else s += it.t;
    });
    return s;
  }

  /* Build the three sentinel-marked sources a changed block can need:
     `both` for the inline rendering, `oldOnly` / `newOnly` for the
     stacked before/after rendering with character-level marks. */
  function markedSources(parts) {
    var bothItems = [], oldItems = [], newItems = [];
    for (var i = 0; i < parts.length; i++) {
      var op = parts[i][0], s = parts[i][1];
      if (op === 0) {
        bothItems.push({ m: false, t: s });
        oldItems.push({ m: false, t: s });
        newItems.push({ m: false, t: s });
        continue;
      }
      if (op === 1) {
        bothItems.push({ m: "ins", t: s });
        newItems.push({ m: "ins", t: s });
        continue;
      }
      var nxt = parts[i + 1];
      bothItems.push({ m: "del", t: s });
      if (!nxt || nxt[0] !== 1) {
        oldItems.push({ m: "del", t: s });
        continue;
      }
      bothItems.push({ m: "ins", t: nxt[1] });
      var r = refine(s, nxt[1]);
      if (r) {
        r.forEach(function (c) {
          if (c[0] === 0) {
            oldItems.push({ m: false, t: c[1] });
            newItems.push({ m: false, t: c[1] });
          } else if (c[0] === -1) {
            oldItems.push({ m: "del", t: c[1] });
          } else {
            newItems.push({ m: "ins", t: c[1] });
          }
        });
      } else {
        oldItems.push({ m: "del", t: s });
        newItems.push({ m: "ins", t: nxt[1] });
      }
      i++;
    }
    return {
      both: emitMarks(tidy(bothItems)),
      oldOnly: emitMarks(tidy(oldItems)),
      newOnly: emitMarks(tidy(newItems))
    };
  }

  function renderMd(src, env) {
    if (!md) return "<pre>" + escHtml(src) + "</pre>";
    try { return md.render(src, env); } catch (e) { return "<pre>" + escHtml(src) + "</pre>"; }
  }

  /* Turn sentinels into real elements by walking text nodes only, so an
     href or an alt attribute can never be corrupted. */
  function materialize(root) {
    var mode = null;
    var walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, null);
    var texts = [];
    while (walker.nextNode()) texts.push(walker.currentNode);
    texts.forEach(function (tn) {
      var s = tn.data;
      if (!SENTINEL_RE.test(s) && !mode) return;
      var frag = document.createDocumentFragment(), buf = "";
      var flush = function () {
        if (!buf) return;
        if (mode) {
          var e = document.createElement("span");
          e.className = mode === "ins" ? "dv-ins" : "dv-del-mark";
          e.textContent = buf;
          frag.appendChild(e);
        } else {
          frag.appendChild(document.createTextNode(buf));
        }
        buf = "";
      };
      for (var i = 0; i < s.length; i++) {
        var ch = s.charAt(i);
        if (ch === S_INS_O) { flush(); mode = "ins"; }
        else if (ch === S_INS_C) { flush(); mode = null; }
        else if (ch === S_DEL_O) { flush(); mode = "del"; }
        else if (ch === S_DEL_C) { flush(); mode = null; }
        else buf += ch;
      }
      flush();
      if (tn.parentNode) tn.parentNode.replaceChild(frag, tn);
    });
    Array.prototype.forEach.call(root.querySelectorAll("*"), function (e) {
      Array.prototype.forEach.call(e.attributes, function (a) {
        if (SENTINEL_RE.test(a.value)) e.setAttribute(a.name, a.value.replace(SENTINEL_G, ""));
      });
    });
    return continueMarks(root);
  }

  /* Bridge equal whitespace at inline boundaries, then lift a fully marked
     inline element into the same highlight as its surrounding prose. Its
     code background must not paint over that continuous run. */
  function continueMarks(root) {
    var walk = document.createTreeWalker(root, NodeFilter.SHOW_TEXT), texts = [];
    while (walk.nextNode()) texts.push(walk.currentNode);
    function mode(t) {
      var mark = t.parentElement.closest(".dv-ins,.dv-del-mark");
      return mark && mark.className;
    }
    texts.forEach(function (t, i) {
      if (!/^\s+$/.test(t.data) || mode(t) || !i || i + 1 === texts.length) return;
      var cls = mode(texts[i - 1]);
      if (!cls || cls !== mode(texts[i + 1])) return;
      if (texts[i - 1].parentElement.closest("p,li,h1,h2,h3,dd") !==
          texts[i + 1].parentElement.closest("p,li,h1,h2,h3,dd")) return;
      var span = el("span", cls);
      t.replaceWith(span); span.appendChild(t);
    });
    Array.from(root.querySelectorAll("code,em,strong,a,s")).reverse().forEach(function (node) {
      var walker = document.createTreeWalker(node, NodeFilter.SHOW_TEXT), cls = null, all = true;
      while (walker.nextNode()) {
        var t = walker.currentNode, m = mode(t);
        if (!t.data) continue;
        if (!m || (cls && cls !== m)) all = false;
        cls = cls || m;
      }
      if (!all || !cls || !node.textContent) return;
      var span = el("span", cls);
      node.replaceWith(span); span.appendChild(node);
    });
    return root;
  }

  function proseFragment(src, env) {
    var div = el("div", "dv-prose");
    div.innerHTML = renderMd(src, env);
    materialize(div);
    return div;
  }

  /* ---------------- prose model ----------------
     Level is chosen at render time from the share of words that changed,
     so flipping the threshold re-renders without recomputing the diff. */

  var PROSE_CACHE = new WeakMap();

  function proseBlocks(file) {
    if (PROSE_CACHE.has(file)) return PROSE_CACHE.get(file);
    // Bound parsing/segmentation as well as the quadratic alignment matrix.
    if ((splitLines(file.old).length + 1) * (splitLines(file.new).length + 1) > MAX_ALIGNMENT_CELLS) {
      PROSE_CACHE.set(file, null);
      return null;
    }
    var oldBlocks = mdBlocks(file.old), newBlocks = mdBlocks(file.new);
    function covers(blocks, ranges) {
      return (rangeList(ranges) || []).every(function (range) {
        var next = range[0];
        blocks.forEach(function (b) {
          if (b.range && b.range[0] <= next && b.range[1] >= next) next = b.range[1] + 1;
        });
        return next > range[1];
      });
    }
    var pairs = alignBlocks(oldBlocks, newBlocks);
    if (!pairs) { PROSE_CACHE.set(file, null); return null; }
    var out = pairs.map(function (p) {
      if (p.same) return { state: "same", src: p.new.exact, kind: p.new.kind };
      if (p.moved === "here") return { state: "moved", src: p.new.exact, kind: p.new.kind };
      if (p.old && !p.new) return { state: "removed", src: p.old.exact, kind: p.old.kind };
      if (p.new && !p.old) return { state: "added", src: p.new.exact, kind: p.new.kind };

      var o = p.old.exact, n = p.new.exact;
      var structural = p.old.kind === "frontmatter" || p.new.kind === "frontmatter" ||
        p.old.kind === "code" || p.new.kind === "code" ||
        p.old.kind === "source" || p.new.kind === "source" ||
        p.old.kind === "table" || p.new.kind === "table";
      if (structural) {
        return { state: "structured", oldSrc: o, newSrc: n, kind: p.new.kind };
      }
      if (proseNorm(o) === proseNorm(n)) {
        return { state: "formatting", src: n, kind: p.new.kind };
      }
      var td = proseDiff(o, n);
      var marked = markedSources(td.parts);
      return {
        state: "edited", kind: p.new.kind,
        oldSrc: o, newSrc: n,
        fraction: td.total ? td.changed / td.total : 1,
        both: marked.both, oldOnly: marked.oldOnly, newOnly: marked.newOnly
      };
    });
    out.forEach(function (b, i) {
      var p = pairs[i], old = p.old || p.movedOld;
      b.oldRange = old && old.range;
      b.newRange = p.new && p.new.range;
      b.oldContentStart = old && old.contentStart;
      b.newContentStart = p.new && p.new.contentStart;
      b.oldEnv = old && old.env;
      b.env = p.new ? p.new.env : b.oldEnv;
      b.key = "b" + i;
    });
    out.unmappedFocus = !covers(oldBlocks, file.oldFocus) || !covers(newBlocks, file.focus);
    PROSE_CACHE.set(file, out);
    return out;
  }

  function levelFor(block) {
    if (block.fraction <= INLINE_MAX) return "inline";
    if (block.fraction <= STACK_MAX) return "stack";
    return "stack-plain";
  }

  /* ---------------- rendering ---------------- */

  function labelEl(cls, mark, text) {
    var l = el("div", "dv-label" + (cls ? " " + cls : ""));
    if (mark) l.appendChild(el("span", "dv-mk", mark));
    l.appendChild(el("span", null, text));
    return l;
  }

  function lineRowEl(r) {
    var d = el("div", "dv-row dv-" + r.cls);
    var o = el("span", "dv-n dv-n-old", r.oldNo == null ? "" : String(r.oldNo));
    var n = el("span", "dv-n dv-n-new", r.newNo == null ? "" : String(r.newNo));
    d.appendChild(o);
    d.appendChild(n);
    d.appendChild(el("span", "dv-sign", r.cls === "add" ? "+" : r.cls === "del" ? "\u2212" : ""));
    var code = el("code", "dv-code");
    var html = r.html;
    if (html == null || html === "") html = "&#8203;";
    code.innerHTML = html;
    d.appendChild(code);
    if (r.missingNewline) code.appendChild(el("span", "dv-no-newline", "\n" + copy.noNewline));
    return d;
  }

  /* Every hidden stretch looks the same: a hairline whose label says what is
     inside, with partial expansion from either end. Other changes revealed
     this way carry the not-relevant cue. Rendered Markdown counts hidden
     blocks and steps PROSE_STEP of them; its +/- counts stay in source lines,
     like the tabs. Each button carries data-dv-ctl so a rerender can hand
     keyboard focus back to it. */
  function foldLine(r, onExpand, blocks) {
    var forms = blocks
      ? { unchanged: copy.unchangedBlocks, leftOut: copy.blocksLeftOut, down: copy.stepDownBlocksTitle, up: copy.stepUpBlocksTitle }
      : { unchanged: copy.unchangedLines, leftOut: copy.linesLeftOut, down: copy.stepDownTitle, up: copy.stepUpTitle };
    var step = blocks ? PROSE_STEP : STEP;
    var g = el("div", "dv-gap dv-fold-line");
    var lab = el("span", "dv-gap-label");
    if (r.add || r.del) {
      lab.appendChild(document.createTextNode(plural(forms.leftOut, r.hidden) + copy.countsSeparator));
      lab.appendChild(el("span", "dv-plus", "+" + r.add));
      lab.appendChild(document.createTextNode(" "));
      lab.appendChild(el("span", "dv-minus", "\u2212" + r.del));
    } else {
      lab.textContent = plural(forms.unchanged, r.hidden);
    }
    [["canTop", say(copy.stepDown, { n: step }), "top", say(forms.down, { n: step })],
     ["canBottom", say(copy.stepUp, { n: step }), "bottom", say(forms.up, { n: step })]].forEach(function (a) {
      if (!r[a[0]]) return;
      var b = el("button", "dv-gap-btn", a[1]);
      b.type = "button";
      b.title = a[3];
      b.setAttribute("data-dv-ctl", "fold:" + r.key + ":" + a[2]);
      b.addEventListener("click", function () { onExpand(r.key, a[2], r[a[2]]); });
      lab.appendChild(b);
    });
    g.appendChild(lab);
    return g;
  }

  function renderRows(host, rows, opts, onExpand) {
    var wrap = el("div", "dv-rows" + (opts && opts.numbers === false ? " dv-nonum" : ""));
    var brackets = !!(opts && opts.brackets), inCore = false;
    rows.forEach(function (r) {
      /* All changes: a hairline labelled "relevant" opens each stretch of
         the excerpt and a plain one closes it (none at the very end). */
      var isCore = r.type === "line" && !!r.core;
      if (brackets && isCore !== inCore) {
        wrap.appendChild(bracketMarker(isCore));
        inCore = isCore;
      }
      if (r.type === "line") {
        var row = lineRowEl(r);
        if (r.core) row.classList.add("dv-core");
        else if (r.core === false) row.classList.add("dv-other");
        wrap.appendChild(row);
        if (opts && opts.file && r.newNo != null) {
          attachLabels(wrap, opts.file, [r.newNo, r.newNo]);
        }
        return;
      }
      wrap.appendChild(foldLine(r, onExpand));
    });
    host.appendChild(wrap);
  }

  /* a structured block inside a prose file: front matter, a fenced code
     block, a table \u2014 a small card, never the file's own source diff */
  function structuredCard(oldSrc, newSrc, kind, review) {
    var card = el("div", kind === "frontmatter" ? "dv-source-block" : "dv-card");
    if (kind !== "frontmatter") card.appendChild(el("div", "dv-card-head", copy.cards[kind === "source" || kind === "table" ? kind : "code"]));
    var lang = kind === "frontmatter" ? "yaml" : "markdown";
    var oldText = oldSrc ? oldSrc + "\n" : "";
    var newText = newSrc ? newSrc + "\n" : "";
    var oldOffset = review && review.block.oldRange ? review.block.oldRange[0] - 1 : 0;
    var newOffset = review && review.block.newRange ? review.block.newRange[0] - 1 : 0;
    if (kind === "code" && md) {
      var ot = md.parse(oldText, {})[0], nt = md.parse(newText, {})[0];
      var codeToken = function (t) { return !t || t.type === "fence" || t.type === "code_block"; };
      if (codeToken(ot) && codeToken(nt) && (!ot || !nt || ot.info === nt.info)) {
        oldText = ot ? ot.content : "";
        newText = nt ? nt.content : "";
        if (review) {
          if (ot && ot.type === "fence") oldOffset = (review.block.oldContentStart || oldOffset + 2) - 1;
          if (nt && nt.type === "fence") newOffset = (review.block.newContentStart || newOffset + 2) - 1;
        }
        lang = ((nt || ot || {}).info || "").trim().split(/\s+/)[0];
      }
    }
    var ops = diffOps(splitLines(oldText), splitLines(newText));
    var prep = {
      ops: ops,
      groups: buildGroups(ops),
      oldHi: highlightLines(oldText, lang),
      newHi: highlightLines(newText, lang)
    };
    var rows = codeRows(prep, {}, { context: Infinity });
    if (review) {
      rows.forEach(function (r) {
        if (r.oldNo != null) r.oldNo += oldOffset;
        if (r.newNo != null) r.newNo += newOffset;
      });
      var opened = Object.assign({}, review.st.gaps);
      /* The outer disclosure decides which source groups show. Transfer only disclosed
         changed rows into this nested card's local group keys, not its whole
         block, so remote unchanged code stays folded too. */
      var visibleOld = new Set(), visibleNew = new Set();
      (review.disclosed || []).forEach(function (r) {
        if (r.type !== "line" || r.cls === "eq") return;
        if (r.oldNo != null) visibleOld.add(r.oldNo);
        if (r.newNo != null) visibleNew.add(r.newNo);
      });
      rows.forEach(function (r) {
        if (r.group && (visibleOld.has(r.oldNo) || visibleNew.has(r.newNo))) {
          opened[review.block.key + "r" + r.group] = { top: 0, bottom: 0, all: true };
        }
      });
      /* A block revealed by a fold step (review.all) shows whole. */
      rows = discloseRows(rows, review.file, opened,
        review.all || review.st.pgaps[review.block.key] === true, review.block.key + "r", review.view);
    }
    renderRows(card, rows, { numbers: false, file: review && review.file }, review ? review.expand : function () {});
    return card;
  }

  /* Flat, single-line values read as prose. Do not pretend to parse complex
     YAML: changed mappings outside this subset use the full source view. */
  function frontValues(src) {
    if (!src) return [];
    var lines = splitLines(src), result = [], keys = Object.create(null);
    if (lines.shift() !== "---" || lines.pop() !== "---") return null;
    for (var i = 0; i < lines.length; i++) {
      if (!lines[i].trim()) continue;
      var m = /^([\w-]+):[ \t]+(.+)$/.exec(lines[i]);
      if (!m || /^[|>\[\]{&*!#]/.test(m[2]) || keys[m[1]]) return null;
      keys[m[1]] = true;
      result.push({ key: m[1], value: m[2] });
    }
    return result;
  }

  function frontMatterBlock(oldSrc, newSrc, review) {
    var oldValues = frontValues(oldSrc), newValues = frontValues(newSrc);
    if (!oldValues || !newValues) return structuredCard(oldSrc, newSrc, "frontmatter", review);
    var block = el("div", "dv-pb dv-meta"), list = el("dl", "dv-meta-list");
    var keys = oldValues.map(function (v) { return v.key; });
    newValues.forEach(function (v) { if (keys.indexOf(v.key) < 0) keys.push(v.key); });
    function value(src, cls) {
      var p = el("p", "dv-prose" + (cls ? " dv-meta-" + cls : ""));
      if (cls) p.appendChild(el("span", "dv-meta-sign", cls === "add" ? "+" : "\u2212"));
      var text = el("span", null, src);
      materialize(text); p.appendChild(text);
      return p;
    }
    keys.forEach(function (key) {
      var o = oldValues.find(function (v) { return v.key === key; });
      var n = newValues.find(function (v) { return v.key === key; });
      var row = el("div", "dv-meta-entry"), dd = el("dd");
      row.appendChild(el("dt", null, key));
      if (!o || !n) {
        var side = n ? "add" : "del";
        row.classList.add("dv-pb", "dv-p-" + side);
        row.firstChild.appendChild(el("span", "dv-meta-sign", n ? "+" : "\u2212"));
        dd.appendChild(value((n || o).value));
      }
      else if (o.value === n.value) dd.appendChild(value(n.value));
      else {
        var diff = proseDiff(o.value, n.value), marks = markedSources(diff.parts);
        var level = levelFor({ fraction: diff.total ? diff.changed / diff.total : 1 });
        if (level === "inline") dd.appendChild(value(marks.both));
        else {
          dd.appendChild(value(level === "stack" ? marks.oldOnly : o.value, "del"));
          dd.appendChild(value(level === "stack" ? marks.newOnly : n.value, "add"));
        }
      }
      row.appendChild(dd); list.appendChild(row);
    });
    block.appendChild(list);
    return block;
  }

  function proseBlockEl(b, review) {
    if (b.kind === "source") {
      return structuredCard(b.oldSrc != null ? b.oldSrc : b.state === "added" ? "" : b.src,
        b.newSrc != null ? b.newSrc : b.state === "removed" ? "" : b.src, b.kind, review);
    }
    /* Whole additions/removals belong to the block, not its fields or lines. */
    if (b.state === "added" || b.state === "removed") {
      var side = b.state === "added" ? "add" : "del";
      var whole = b.kind === "frontmatter"
        ? frontMatterBlock(b.src, b.src, review) : el("div", "dv-pb");
      whole.classList.add("dv-p-" + side);
      whole.insertBefore(labelEl("dv-l-" + side, side === "add" ? "+" : "\u2212", copy.blocks[b.state]), whole.firstChild);
      if (b.kind !== "frontmatter") whole.appendChild(proseFragment(b.src, b.env));
      return whole;
    }
    if (b.kind === "frontmatter") {
      return frontMatterBlock(
        b.oldSrc != null ? b.oldSrc : b.state === "added" ? "" : b.src,
        b.newSrc != null ? b.newSrc : b.state === "removed" ? "" : b.src, review);
    }
    if (b.kind === "code") {
      return structuredCard(
        b.oldSrc != null ? b.oldSrc : b.state === "added" ? "" : b.src,
        b.newSrc != null ? b.newSrc : b.state === "removed" ? "" : b.src,
        b.kind, review);
    }
    if (b.state === "same") {
      var s = el("div", "dv-pb");
      s.appendChild(proseFragment(b.src, b.env));
      return s;
    }
    if (b.state === "moved") {
      var mv = el("div", "dv-pb dv-p-moved");
      mv.appendChild(labelEl("", "\u21c5", copy.blocks.moved));
      mv.appendChild(proseFragment(b.src, b.env));
      return mv;
    }
    if (b.state === "formatting") {
      var f = el("div", "dv-pb dv-p-fmt");
      f.appendChild(labelEl("", "\u2022\u2022", copy.blocks.formatting));
      f.appendChild(proseFragment(b.src, b.env));
      return f;
    }
    if (b.state === "structured") {
      var c = el("div", "dv-pb");
      c.appendChild(labelEl("", "\u00b1", copy.blocks.changed));
      c.appendChild(structuredCard(b.oldSrc, b.newSrc, b.kind, review));
      return c;
    }

    var level = JSON.stringify(b.oldEnv) === JSON.stringify(b.env) ? levelFor(b) : "stack-plain";
    if (level === "inline") {
      var i = el("div", "dv-pb");
      i.appendChild(labelEl("", "\u00b1", say(copy.blocks.edited, { pct: pct(b.fraction) })));
      i.appendChild(proseFragment(b.both, b.env));
      return i;
    }
    var box = el("div", "dv-pb");
    box.style.padding = "0";
    var before = el("div", "dv-side dv-s-old");
    before.appendChild(labelEl("dv-l-del", "\u2212", copy.blocks.before));
    before.appendChild(proseFragment(level === "stack" ? b.oldOnly : b.oldSrc, b.oldEnv));
    var after = el("div", "dv-side dv-s-new");
    after.appendChild(labelEl("dv-l-add", "+", copy.blocks.after));
    after.appendChild(proseFragment(level === "stack" ? b.newOnly : b.newSrc, b.env));
    box.appendChild(before);
    box.appendChild(after);
    return box;
  }

  function pct(f) {
    var v = Math.round(f * 100);
    return (v < 1 ? "<1" : v) + "%";
  }

  /* Rendered Markdown follows the code diff's rules. Every hidden run of
     blocks is the same one-hairline fold as a code gap ("9 unchanged blocks",
     or "4 blocks left out, +12 -3" in source lines), stepped open PROSE_STEP
     blocks at a time from either end. Blocks a step reveals show whole, and
     changes among them carry the not-relevant cue, as code does. */
  function renderProse(host, file, st, expand, expandBlocks) {
    var blocks = proseBlocks(file);
    if (blocks.unmappedFocus) host.appendChild(el("div", "dv-note", copy.unmappedFocus));
    var wrap = el("div", "dv-pblocks");
    var sourceRows = codeRows(prepareCode(file), {}, { context: Infinity });
    var disclosed = discloseRows(sourceRows, file, st.gaps, false, "r", st.view);
    function belongs(r, b) {
      return (r.oldNo != null && intersects(b.oldRange, r.oldNo, r.oldNo)) ||
        (r.newNo != null && intersects(b.newRange, r.newNo, r.newNo));
    }
    function rowsOf(list) {
      return sourceRows.filter(function (r) {
        return list.some(function (b) { return belongs(r, b); });
      });
    }
    function hidden(b) {
      if (st.pgaps[b.key] === true) return false;
      return !disclosed.some(function (r) { return r.type === "line" && belongs(r, b); }) &&
        !focused(file, b.oldRange, b.newRange);
    }
    /* Decide what shows first: blocks (flagged when a step revealed them) and folds. */
    var items = [], i = 0;
    while (i < blocks.length) {
      if (!hidden(blocks[i])) { items.push({ block: blocks[i] }); i++; continue; }
      var j = i;
      while (j < blocks.length && hidden(blocks[j])) j++;
      var key = "p" + i, n = j - i, kept = st.pgaps[key], top = 0, bottom = 0;
      if (kept === true) top = n;
      else if (kept) { top = Math.min(kept.top || 0, n); bottom = Math.min(kept.bottom || 0, n - top); }
      var still = blocks.slice(i + top, j - bottom), stillRows = rowsOf(still);
      /* A short unchanged run is shown rather than folded. One holding a change
         stays folded, so Relevant never shows more than its tab counts. */
      if (stillRows.length <= 3 && !stillRows.some(isChange)) {
        top = n; bottom = 0; still = [];
      }
      for (var k = i; k < i + top; k++) items.push({ block: blocks[k], revealed: true });
      if (still.length) {
        var counts = changeCounts(stillRows);
        items.push({ fold: { key: key, hidden: still.length, add: counts.add, del: counts.del, top: top, bottom: bottom,
          canTop: i > 0, canBottom: j < blocks.length } });
        for (k = j - bottom; k < j; k++) items.push({ block: blocks[k], revealed: true });
      }
      i = j;
    }
    /* All changes brackets the excerpt's blocks like its rows. Relevant, once a step has revealed other
       changes, gives those the quiet cue but no brackets: a bracket arriving with a step would push down
       everything above the fold that was stepped. */
    var cues = hasFocusRanges(file) && (st.view === "all" || items.some(function (it) {
      return it.revealed && it.block.state !== "same";
    }));
    var brackets = cues && st.view === "all";
    var inCore = false;
    function core(b) {
      return cues && (focused(file, b.oldRange, b.newRange) ||
        disclosed.some(function (r) { return r.core && belongs(r, b); }));
    }
    function mark(isCore) {
      if (!brackets || isCore === inCore) return;
      wrap.appendChild(bracketMarker(isCore));
      inCore = isCore;
    }
    items.forEach(function (it) {
      if (it.fold) {
        mark(false);
        wrap.appendChild(foldLine(it.fold, expandBlocks, true));
        return;
      }
      var b = it.block;
      var node = proseBlockEl(b, { file: file, block: b, st: st, view: st.view, expand: expand,
        disclosed: disclosed, all: !!it.revealed });
      if (!node.querySelector('.dv-rows')) attachLabels(node, file, b.newRange);
      var isCore = core(b);
      mark(isCore);
      if (isCore) node.classList.add("dv-core");
      else if (cues && b.state !== "same") node.classList.add("dv-other");
      wrap.appendChild(node);
    });
    host.appendChild(wrap);
  }

  /* ---------------- remembered panel state ---------------- */

  /* Saved state is untrusted: keep only well-formed fold entries, so a
     corrupt or foreign value can never break a panel or hide its rows. */
  function plainObject(v) { return !!v && typeof v === "object" && !Array.isArray(v); }
  /* Fold runs by key, each stepped { top, bottom } in whole lines or blocks from 0. A line gap may also be
     shown whole (all); a block run may be true, shown whole. */
  function keptRuns(v, blocks) {
    var out = {};
    if (!plainObject(v)) return out;
    Object.keys(v).forEach(function (k) {
      var g = v[k];
      if (k === "__proto__") return;
      if (blocks && g === true) { out[k] = true; return; }
      if (!plainObject(g) || !Number.isInteger(g.top) || !Number.isInteger(g.bottom) || g.top < 0 || g.bottom < 0) return;
      out[k] = blocks ? { top: g.top, bottom: g.bottom } : { top: g.top, bottom: g.bottom, all: g.all === true };
    });
    return out;
  }

  /* ---------------- the public entry point ---------------- */

  function render(container, spec) {
    if (!container) return null;
    /* the spec object itself is the cache key, so a review page that hands
       the same file object to several calls pays for the diff once */
    var file = spec;
    if (file.old == null) file.old = "";
    if (file["new"] == null) file["new"] = "";
    var status = file.status || "modified";
    var scope = spec.scope || "";
    var counts = countChanges(file);
    /* A focused review panel gets the Relevant | All changes tabs, unless the
       excerpt already holds every change. */
    var split = hasFocusRanges(file) ? relevantCounts(file) : null;
    if (split && split.add === counts.add && split.del === counts.del) split = null;
    var st = {
      gaps: {},
      pgaps: {},
      source: false,
      scrollX: 0,
      view: split ? "relevant" : null,
      collapsed: true
    };
    /* A page that passes stateKey keeps each panel's folds across reloads and republishes. */
    var stateKey = spec.stateKey ? "dv-state:" + scope + ":" + spec.stateKey : null;
    if (stateKey) {
      try {
        var kept = JSON.parse(global.localStorage.getItem(stateKey) || "null");
        if (kept && typeof kept === "object") {
          st.source = kept.source === true;
          if (typeof kept.collapsed === "boolean") st.collapsed = kept.collapsed;
          var x = Number(kept.scrollX);
          if (isFinite(x) && x > 0) st.scrollX = x;
          /* Folds saved before the tabs existed belong to a view that is gone. */
          if (!split || kept.view === "relevant" || kept.view === "all") {
            st.gaps = keptRuns(kept.gaps, false); st.pgaps = keptRuns(kept.pgaps, true);
            if (split) st.view = kept.view;
          }
        }
      } catch (e) { /* unreadable or corrupt state: start from the defaults */ }
    }
    /* scrollX is the body's horizontal scroll (phones scroll long code
       sideways), kept with the rest of the panel's state. */
    function keepState() {
      if (!stateKey) return;
      try { global.localStorage.setItem(stateKey, JSON.stringify({ gaps: st.gaps, pgaps: st.pgaps, view: st.view, source: st.source, collapsed: st.collapsed, scrollX: st.scrollX })); } catch (e) {}
    }

    var panel = el("div", "dv-file");

    var head = el("div", "dv-head");
    var fold = el("button", "dv-collapse", "\u25be");
    fold.type = "button";
    function paintCollapsed() {
      panel.classList.toggle("dv-is-collapsed", st.collapsed);
      fold.setAttribute("aria-expanded", String(!st.collapsed));
      fold.title = st.collapsed ? copy.showDiff : copy.collapseDiff;
      fold.setAttribute("aria-label", fold.title);
    }
    function toggleCollapsed() { st.collapsed = !st.collapsed; keepState(); paintCollapsed(); }
    fold.addEventListener("click", function (e) { e.stopPropagation(); toggleCollapsed(); });
    /* The whole header row toggles too, except its own controls. */
    head.addEventListener("click", function (e) {
      if (e.target.closest("button, input, label, a")) return;
      if (global.getSelection && String(global.getSelection())) return;
      toggleCollapsed();
    });
    paintCollapsed();
    head.appendChild(fold);
    /* A title leads the header: what this diff shows, before where it is. */
    if (typeof spec.title === "string" && spec.title) {
      head.classList.add("dv-has-title");
      head.appendChild(el("span", "dv-title", spec.title));
    }
    var pathEl = el("div", "dv-path");
    pathEl.innerHTML = pathHtml(file.path);
    head.appendChild(pathEl);
    if (status !== "modified") {
      head.appendChild(el("span", "dv-chip dv-" + status, copy.status[status]));
    }
    var markdown = isMarkdown(file.path) && md;
    /* A changed final newline shows only in the source diff; an added or
       deleted file has no other side to compare. */
    if (markdown && (!proseBlocks(file) || (status === "modified" && file.old.endsWith("\n") !== file.new.endsWith("\n")))) markdown = false;
    /* The Markdown source switch is a small header toggle, like the file
       view's "View source". The header is not rebuilt, so the toggle keeps
       keyboard focus across rerenders. */
    var sourceToggle = null;
    if (markdown) {
      sourceToggle = el("button", "dv-src-toggle");
      sourceToggle.type = "button";
      sourceToggle.setAttribute("data-dv-ctl", "source");
      sourceToggle.addEventListener("click", function () { st.source = !st.source; rerender(); });
      head.appendChild(sourceToggle);
    }
    function totalsEl(cls, c) {
      var t = el("span", cls);
      t.innerHTML = '<span class="dv-plus">+' + c.add + '</span>' +
        '<span class="dv-minus">\u2212' + c.del + "</span>";
      if (!c.add) t.querySelector(".dv-plus").classList.add("dv-zero");
      if (!c.del) t.querySelector(".dv-minus").classList.add("dv-zero");
      return t;
    }
    var tabs = null;
    if (split) {
      /* The one control between the two views; each tab carries its counts. */
      tabs = el("span", "dv-tabs");
      tabs.setAttribute("role", "tablist");
      tabs.setAttribute("aria-label", copy.tabs.label);
      [["relevant", copy.tabs.relevant, split], ["all", copy.tabs.all, counts]].forEach(function (t) {
        var b = el("button", "dv-tab");
        b.type = "button";
        b.setAttribute("role", "tab");
        b.setAttribute("data-dv-view", t[0]);
        var name = el("span", "dv-tab-name", t[1]);
        name.setAttribute("data-label", t[1]); /* reserves the chosen (bold) width, so choosing never moves the header */
        b.appendChild(name);
        b.appendChild(totalsEl("dv-totals", t[2]));
        b.addEventListener("click", function () { showView(t[0]); });
        tabs.appendChild(b);
      });
      tabs.addEventListener("keydown", function (e) {
        if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
        e.preventDefault();
        showView(st.view === "all" ? "relevant" : "all");
        tabs.querySelector('[aria-selected="true"]').focus();
      });
      head.appendChild(tabs);
    } else head.appendChild(totalsEl("dv-totals", counts));
    function paintTabs() {
      if (!tabs) return;
      tabs.querySelectorAll(".dv-tab").forEach(function (b) {
        var on = b.getAttribute("data-dv-view") === st.view;
        b.setAttribute("aria-selected", String(on));
        b.tabIndex = on ? 0 : -1;
      });
    }
    function showView(v) {
      var wasCollapsed = st.collapsed;
      st.collapsed = false;
      paintCollapsed();
      if (st.view === v) { if (wasCollapsed) keepState(); return; }
      st.view = v; st.gaps = {}; st.pgaps = {};
      paintTabs();
      rerender();
      /* Leaving a long All changes view from deep inside: bring the panel's top back. */
      if (panel.getBoundingClientRect().top < 0 && panel.scrollIntoView) panel.scrollIntoView({ block: "start" });
    }
    paintTabs();

    panel.appendChild(head);

    var body = el("div", "dv-body");
    panel.appendChild(body);
    container.appendChild(panel);

    function rerender() {
      /* Build off-DOM: briefly emptying a tall body clamps the document's
         scroll position before its replacement can restore the height. */
      var previous = body, x = global.scrollX, y = global.scrollY;
      /* Replacing the body would drop keyboard focus to <body>. Remember the
         focused control (data-dv-ctl, else its place among the body's buttons)
         and hand focus back afterwards. */
      var active = document.activeElement, ctl = null, place = -1;
      if (active && previous.contains(active)) {
        ctl = active.getAttribute("data-dv-ctl");
        place = Array.prototype.indexOf.call(previous.querySelectorAll("button"), active);
      }
      body = el("div", "dv-body");
      keepState();
      build();
      previous.replaceWith(body);
      if (global.scrollTo && (global.scrollX !== x || global.scrollY !== y)) {
        global.scrollTo({ left: x, top: y, behavior: "instant" });
      }
      if (sizeWatch) sizeWatch.unobserve(previous);
      trackScrollX();
      if (ctl || place >= 0) refocus(ctl, place);
    }

    /* The same control if it survived (same fold, same direction); otherwise the
       button now at its place in the body (the next fold down, or the last one),
       otherwise the panel's collapse chevron. */
    function refocus(ctl, place) {
      var buttons = body.querySelectorAll("button"), target = null;
      if (ctl) {
        target = Array.prototype.find.call(buttons, function (b) {
          return b.getAttribute("data-dv-ctl") === ctl;
        });
      }
      if (!target && buttons.length) target = buttons[Math.min(Math.max(place, 0), buttons.length - 1)];
      (target || fold).focus({ preventScroll: true });
      /* When the control that had focus is gone and its stand-in is off-screen,
         focus the panel's collapse chevron, which sits in the sticky header, instead. */
      if (target && (!ctl || target.getAttribute("data-dv-ctl") !== ctl)) {
        var r = target.getBoundingClientRect();
        if (r.top < 0 || r.bottom > (global.innerHeight || 0)) fold.focus({ preventScroll: true });
      }
    }

    /* Horizontal scroll persists per stateKey. The body only scrolls sideways
       on narrow screens, and a hidden body (collapsed panel, closed item) has
       no scroll position, so the saved offset is applied after every render
       and again whenever the body gets a size. */
    var scrollSave = null;
    var sizeWatch = global.ResizeObserver ? new global.ResizeObserver(applyScrollX) : null;
    function applyScrollX() {
      if (st.scrollX && body.clientWidth && body.scrollLeft !== st.scrollX) body.scrollLeft = st.scrollX;
    }
    function trackScrollX() {
      var b = body;
      b.addEventListener("scroll", function () {
        if (b !== body || !b.clientWidth) return;
        st.scrollX = b.scrollLeft;
        clearTimeout(scrollSave);
        scrollSave = setTimeout(keepState, 200);
      }, { passive: true });
      if (sizeWatch) sizeWatch.observe(b);
      applyScrollX();
    }

    /* A fold step reveals `n` more lines (st.gaps) or, in rendered Markdown, blocks (st.pgaps) from one
       end. `from` is how far the fold row already shows from that end (a step may have run on to the edge
       of a replacement), so each click reveals new ones instead of re-requesting ones already shown. */
    function stepper(name, n) {
      return function (key, dir, from) {
        var g = st[name][key];
        if (!g || g === true) g = { top: 0, bottom: 0 };
        g[dir] = Math.max(g[dir] || 0, from || 0) + n;
        st[name][key] = g;
        rerender();
      };
    }
    var expand = stepper("gaps", STEP), expandBlocks = stepper("pgaps", PROSE_STEP);

    /* The tabs (when a focus leaves changes out) and the fold hairlines are
       the body's only controls, in code and in rendered Markdown. */
    function build() {
      if (sourceToggle) {
        sourceToggle.textContent = st.source ? copy.viewRendered : copy.viewSource;
        /* the other label, hidden, reserves the wider width (page CSS) */
        sourceToggle.setAttribute("data-alt", st.source ? copy.viewSource : copy.viewRendered);
      }
      if (markdown && !st.source) {
        renderProse(body, file, st, expand, expandBlocks);
        return;
      }
      var prep = prepareCode(file);
      var rows = codeRows(prep, st.gaps, { context: Infinity });
      rows = discloseRows(rows, file, st.gaps, false, "r", st.view);
      if ((!rows.length || !rows.some(function (r) { return r.type === "line"; })) &&
          (counts.add || counts.del)) rows = codeRows(prep, st.gaps);
      if (!rows.length) {
        body.appendChild(el("div", "dv-note", copy.noChanges));
        return;
      }
      /* Only All changes brackets: on Relevant a bracket would arrive with the step that revealed another
         change and push down everything above that fold. The rows' own cue still marks the other changes. */
      renderRows(body, rows, { file: file, brackets: st.view === "all" }, expand);
    }

    build();
    trackScrollX();
    return { element: panel, rerender: rerender, file: file };
  }

  global.DiffView = { render: render, countChanges: countChanges };
}
