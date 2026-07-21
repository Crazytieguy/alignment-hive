import { useEffect, useRef } from "react";
import type { ReactNode } from "react";
import "./kintsugi-hero.css";

/**
 * Kintsugi Murmuration (G10): a full-page field of small amber star-points
 * over the page's continuous dark ground. Points are 4-point sparkles with
 * gentle brightness variation (night sky meets comb). A single condensation
 * focus wanders the viewport when idle and lazily follows the cursor; cells
 * condense bead-by-bead into hexagons that read purely from where the stars
 * sit — no drawn seams. Locking stars keep their size and color and simply
 * glow a little brighter.
 *
 * The field covers the whole document: a canvas one viewport plus a scroll
 * margin tall (z-index -1 inside the page's isolated stacking context)
 * scrolls WITH the document and draws particles laid out in document
 * coordinates. Each simulation tick re-centers it over the viewport;
 * between ticks the compositor moves canvas and text together, so stars
 * never trail the text during native scrolling (a fixed canvas repainted
 * at ~30fps lags scroll by 1-2 frames).
 *
 * Lattice: a conformally-mapped honeycomb. The comb is generated as a
 * perfect regular hexagonal lattice in a flat pre-image domain (hex radius
 * 1) and every cell vertex and seam-bead position is pushed through a
 * conformal (or quasi-conformal) map into document coordinates, so cell
 * scale grows continuously down the page with no zone seams: hexagons keep
 * honest 120° junctions everywhere and comb rows bow into genuine circular
 * arcs around the map's pole above the page (curvature radius =
 * 1 / d(ln s)/dy, the exact e^(az) relation).
 *
 * Density and tone: the hero holds a plateau, then both grade smoothly
 * through anchors at the real section centers ([data-star-section] in
 * routes/index.tsx); section palettes are dithered star-by-star between
 * neighbors. The nav band above the hero keeps hero density but wears the
 * moonlight palette.
 *
 * Easing rates are dt-scaled (60fps-equivalent) so high refresh displays run
 * at the same speed. The seeded RNG keeps the composition stable. Canvas
 * work runs client-side only (SSR renders the static markup).
 */

const T = 90; // ambient star-drift loop, seconds (seamless; per-star motion is slow)
const TW = 80; // condensation-focus wander period, seconds (independent of T)
const W0 = (2 * Math.PI) / T;
const TAU = 2 * Math.PI;
const SEED = 1789;
const NCOL = 5;
const SPR = 96; // sprite canvas size (px)
const DK = 5; // draw size = sz * DK (star arm span); core stays ~sz
const SEC_F = 50; // section-boundary feather half-width (~100px soft edge)
// Painted band beyond the viewport, px each side. The compositor scrolls
// the canvas with the page between ~30fps repaints, so the margin only has
// to cover the scroll of one tick (~33ms): 200px ≈ a 6000px/s fling. Cost:
// ~(2·SCROLL_M)/vh more backing store and per-repaint draw work.
const SCROLL_M = 200;

// ---------- field configuration ----------
// The owner's tuned look (July 2026 live-tuning sessions). Density values
// anchor smooth gradients at the real section centers below the hero
// plateau; section colors are the directly authored SECTION_PALS below.
// The nav shares the hero's density but wears the moonlight palette.
const CFG = {
  dens: {
    hero: 1,
    testimonials: 0.7,
    workshops: 0.55,
    tools: 0.35,
    about: 0, // footer counts as about
  },
  rbMul: 1, // base hex radius multiplier
  growth: 2.667, // 0.9×Rb near the hero copy -> ~2.4×Rb by page bottom
  reach: 170, // condensation influence radius, px (fixed area everywhere)
  window: 0.7, // per-star flight window as fraction of a cell's fill
  kIn: 0.02, // per-frame (60fps) fill rate; relax-out is kIn/2
  glow: 0.5, // lock-glow strength multiplier
} as const;

function ss(e0: number, e1: number, x: number): number {
  let t = (x - e0) / (e1 - e0);
  t = t < 0 ? 0 : t > 1 ? 1 : t;
  return t * t * (3 - 2 * t);
}

// ---------- deterministic randomness ----------
function hash2(i: number, j: number): number {
  let n = (i * 374761393 + j * 668265263 + SEED * 974711) | 0;
  n = ((n ^ (n >>> 13)) * 1274126177) | 0;
  n = n ^ (n >>> 16);
  return (n >>> 0) / 4294967295;
}

function vnoise(x: number, y: number): number {
  const i = Math.floor(x);
  const j = Math.floor(y);
  let fx = x - i;
  let fy = y - j;
  fx = fx * fx * (3 - 2 * fx);
  fy = fy * fy * (3 - 2 * fy);
  const a = hash2(i, j);
  const b = hash2(i + 1, j);
  const c = hash2(i, j + 1);
  const d = hash2(i + 1, j + 1);
  return a + (b - a) * fx + (c - a) * fy + (a - b - c + d) * fx * fy;
}

function fbm(x: number, y: number): number {
  return (
    0.68 * vnoise(x / 240, y / 240) +
    0.32 * vnoise(x / 95 + 7.31, y / 95 + 2.97)
  );
}

// Locking stars keep their own color: the lock pass redraws the star (a
// short-arm variant of its own sprite) brighter, plus a soft same-hue halo —
// stars simply rearrange into hexagons and glow a little.
const HALO_A = 0.34; // lock-halo strength

// ---------- section star palettes (directly authored) ----------
// Each section's sky is five star colors: slots 0-1 are the brighter
// seam-bead colors, slots 2-4 the ambient population (slot 4 is the
// section's coolest color — see ambSlot's clumping). The hero is the
// showcase mix (amber + cream + moonlight); testimonials are pure warm
// gold; the page cools through silver-cream toward moonlight, and the nav
// band (which borrows the about palette) is coldest.
const SECTION_KEYS = [
  "hero",
  "testimonials",
  "workshops",
  "tools",
  "about",
] as const;
const NSEC = SECTION_KEYS.length;

const SECTION_PALS: string[][] = [
  // hero: the mix
  [
    "rgba(227,166,60,0.95)",
    "rgba(198,143,42,0.85)",
    "rgba(227,166,60,0.72)",
    "rgba(239,217,168,0.5)",
    "rgba(159,178,196,0.52)",
  ],
  // testimonials: pure warm gold and copper, no cool stars
  [
    "rgba(255,217,143,0.95)",
    "rgba(242,176,74,0.85)",
    "rgba(242,176,74,0.72)",
    "rgba(217,143,43,0.52)",
    "rgba(201,169,106,0.42)",
  ],
  // workshops: softened warm, amber and cream
  [
    "rgba(227,166,60,0.95)",
    "rgba(198,143,42,0.85)",
    "rgba(217,179,106,0.66)",
    "rgba(205,188,156,0.5)",
    "rgba(227,166,60,0.4)",
  ],
  // tools: cool silver / cream / a breath of blue
  [
    "rgba(239,224,184,0.95)",
    "rgba(201,204,209,0.85)",
    "rgba(192,196,203,0.62)",
    "rgba(201,189,156,0.48)",
    "rgba(147,169,192,0.5)",
  ],
  // about + footer + nav band: moonlight
  [
    "rgba(201,211,224,0.95)",
    "rgba(159,178,196,0.85)",
    "rgba(140,163,187,0.66)",
    "rgba(159,178,196,0.5)",
    "rgba(94,113,134,0.5)",
  ],
];

// Ambient slot selection: how often a star takes the cool slot (4) vs the
// main slots (2-3), and how strongly a low-frequency noise field gathers
// cool stars into loose drifting neighborhoods (real skies clump; iid
// sampling reads as salt-and-pepper).
const MIX_BASE = [0.34, 0.15, 0.2, 0.4, 0.5];
const MIX_CLUMP = [0.45, 0.1, 0.15, 0.3, 0.15];

function coolP(sec: number, x: number, y: number): number {
  let pc = MIX_BASE[sec];
  const cl = MIX_CLUMP[sec];
  if (cl > 0) {
    const nse = vnoise(x / 340 + 31.7, y / 340 + 11.3);
    pc = Math.max(0, Math.min(1, pc + cl * (nse - 0.5) * 2));
  }
  return pc;
}

function ambSlot(
  sec: number,
  x: number,
  y: number,
  r1: number,
  r2: number,
): number {
  return r1 < coolP(sec, x, y) ? 4 : r2 < 0.55 ? 2 : 3;
}

// ---------- star sprites ----------
function parseRgba(c: string): [string, number] {
  const m = /rgba?\(([^)]+)\)/.exec(c);
  const parts = m
    ? m[1].split(",").map((s) => s.trim())
    : ["255", "255", "255"];
  const rgb = parts.slice(0, 3).join(",");
  const a = parts.length > 3 ? parseFloat(parts[3]) : 1;
  return [rgb, a];
}

// A subtle 4-point sparkle: thin concave arms (quadratics pulled to the
// center) with a radial falloff, plus a soft round core. Drawn once per
// palette color; particles blit it scaled to their (constant) size.
function starSprite(color: string, armR = 44): HTMLCanvasElement {
  const c = document.createElement("canvas");
  c.width = c.height = SPR;
  const g = c.getContext("2d");
  if (!g) return c;
  const [rgb, a] = parseRgba(color);
  const R = armR;
  g.beginPath();
  g.moveTo(48, 48 - R);
  g.quadraticCurveTo(48, 48, 48 + R, 48);
  g.quadraticCurveTo(48, 48, 48, 48 + R);
  g.quadraticCurveTo(48, 48, 48 - R, 48);
  g.quadraticCurveTo(48, 48, 48, 48 - R);
  g.closePath();
  const arm = g.createRadialGradient(48, 48, 2, 48, 48, R);
  arm.addColorStop(0, `rgba(${rgb},${(a * 0.8).toFixed(3)})`);
  arm.addColorStop(0.42, `rgba(${rgb},${(a * 0.18).toFixed(3)})`);
  arm.addColorStop(1, `rgba(${rgb},0)`);
  g.fillStyle = arm;
  g.fill();
  const core = g.createRadialGradient(48, 48, 0, 48, 48, 13);
  core.addColorStop(0, `rgba(${rgb},${a.toFixed(3)})`);
  core.addColorStop(0.55, `rgba(${rgb},${(a * 0.5).toFixed(3)})`);
  core.addColorStop(1, `rgba(${rgb},0)`);
  g.fillStyle = core;
  g.beginPath();
  g.arc(48, 48, 13, 0, TAU);
  g.fill();
  return c;
}

function haloSprite(color: string): HTMLCanvasElement {
  const c = document.createElement("canvas");
  c.width = c.height = SPR;
  const g = c.getContext("2d");
  if (!g) return c;
  const [rgb, a] = parseRgba(color);
  const grad = g.createRadialGradient(48, 48, 0, 48, 48, 46);
  grad.addColorStop(0, `rgba(${rgb},${a.toFixed(3)})`);
  grad.addColorStop(0.5, `rgba(${rgb},${(a * 0.32).toFixed(3)})`);
  grad.addColorStop(1, `rgba(${rgb},0)`);
  g.fillStyle = grad;
  g.fillRect(0, 0, SPR, SPR);
  return c;
}

interface Cell {
  x: number; // mapped center, document coords
  y: number;
  u: number; // flat pre-image lattice coords (hex radius 1)
  v: number;
  r: number; // local mapped hex radius (= map scale at the cell)
  k: number;
  damp: number;
  p0: number;
  p1: number;
}

interface HeroState {
  ctx: CanvasRenderingContext2D;
  w: number; // viewport (= document) width
  vh: number; // viewport height
  docH: number; // document height
  heroTop: number; // hero stage top, document coords
  heroH: number;
  dpr: number;
  scrollY: number;
  canvasTop: number; // painted band top, document coords (canvas scrolls with the page)
  canvasH: number; // painted band height = vh + 2·SCROLL_M
  curOn: boolean;
  curX: number;
  curY: number;
  fx: number;
  fy: number;
  fOn: boolean;
  secB: number[]; // section boundaries (tops of testimonials..about), doc coords
  secC: number[]; // section anchor centers (about's extends to the footer)
  cols: { x0: number; x1: number; y0: number; y1: number }[]; // text columns
  Rb: number; // base hex radius (hero zone, includes the rbMul tuning)
  // conformal-map lookup table, uniform in flat depth v (step MH):
  mv0: number; // v of table node 0
  myc: Float64Array; // center-line document y per node
  ms: Float64Array; // map scale per node
  mk: Float64Array; // arc curvature d(ln s)/dy per node
  cells: Cell[];
  N: number;
  bx: Float32Array;
  by: Float32Array;
  hx: Float32Array;
  hy: Float32Array;
  amp: Float32Array;
  sz: Float32Array;
  dsz: Float32Array;
  br: Float32Array;
  ordv: Float32Array;
  ci: Int32Array;
  px: Float32Array;
  py: Float32Array;
  ma: Float32Array;
  buckets: number[][]; // [section * NCOL + color] -> star indices
  kbuf: Float32Array;
  sprites: HTMLCanvasElement[][]; // [section][color]
  lockSpr: HTMLCanvasElement[][]; // short-arm brighten pass, same colors
  haloSpr: HTMLCanvasElement[][]; // soft same-hue halos
  pi: Uint8Array; // per bead star: palette index (section * NCOL + slot)
}

function makeSprites(st: HeroState): void {
  st.sprites = SECTION_PALS.map((pal) => pal.map((c) => starSprite(c)));
  // shorter arms on the locked star: neighbors on a seam sit close, so long
  // arms would fuse into a rope — the lock reads as a brighter core instead
  st.lockSpr = SECTION_PALS.map((pal) =>
    pal.map((c) => {
      const [rgb] = parseRgba(c);
      return starSprite(`rgba(${rgb},1)`, 30);
    }),
  );
  st.haloSpr = SECTION_PALS.map((pal) =>
    pal.map((c) => {
      const [rgb] = parseRgba(c);
      return haloSprite(`rgba(${rgb},0.5)`);
    }),
  );
}

// calm channel where the hero copy sits
function chan(st: HeroState, x: number, y: number): number {
  const ex = (x - st.w * 0.5) / (st.w * 0.345);
  const ey = (y - (st.heroTop + st.heroH * 0.5)) / (st.heroH * 0.315);
  const d2 = ex * ex + ey * ey;
  return 1 - 0.965 * Math.exp(-d2 * d2 * 1.15);
}

// section density: the hero holds a plateau, then density grades smoothly
// down the page — the slider values anchor the gradient at the real section
// centers (about's anchor extends toward the footer), linear between anchors
// (invisible kinks in a stochastic field), smoothstepped off the hero edge
function secDens(st: HeroState, y: number): number {
  const dv = CFG.dens;
  const b = st.secB;
  const c = st.secC;
  if (b.length < 4) return dv.hero;
  const vals = [dv.testimonials, dv.workshops, dv.tools, dv.about];
  if (y < c[0]) {
    // hero plateau easing into the first anchor
    return dv.hero + (vals[0] - dv.hero) * ss(b[0] - SEC_F, c[0], y);
  }
  for (let i = 0; i < 3; i++) {
    if (y < c[i + 1]) {
      const t = (y - c[i]) / (c[i + 1] - c[i]);
      return vals[i] + (vals[i + 1] - vals[i]) * t;
    }
  }
  return vals[3];
}

// section (tone) index at y: hero keeps a feathered edge; below it the
// caller's random draw dithers star-by-star between the two neighboring
// sections' sprite sets, weighted by position between their centers — the
// tone reads as a continuous gradient matching the density gradient
function secIndex(st: HeroState, y: number, r: number): number {
  const b = st.secB;
  const c = st.secC;
  if (b.length < 4) return 0;
  // nav band above the hero stage: footer's moonlight tone (index 4),
  // dithered into the hero tone across the stage's top edge
  const wn = ss(st.heroTop - SEC_F, st.heroTop + SEC_F, y);
  if (wn < 1) return r < wn ? 0 : 4;
  const w0 = ss(b[0] - SEC_F, b[0] + SEC_F, y);
  if (w0 <= 0) return 0;
  if (w0 < 1) return r < w0 ? 1 : 0;
  for (let i = 0; i < 3; i++) {
    if (y < c[i]) return i + 1;
    if (y < c[i + 1]) {
      const t = (y - c[i]) / (c[i + 1] - c[i]);
      return r < t ? i + 2 : i + 1;
    }
  }
  return 4;
}

// text-column damping (testimonial blockquotes, measured in resize()):
// density recovers quickly past the text edges so the field reads at full
// strength in the margins and the inter-column gutter
function colDamp(st: HeroState, x: number, y: number): number {
  let damp = 1;
  for (const c of st.cols) {
    const dx = Math.max(c.x0 - x, 0, x - c.x1);
    const dy = Math.max(c.y0 - y, 0, y - c.y1);
    const dl = 0.3 + 0.7 * ss(14, 85, Math.hypot(dx, dy));
    if (dl < damp) damp = dl;
  }
  return damp;
}

// ---------- conformal map ----------
// The honeycomb lives in a flat pre-image domain (perfect lattice, hex
// radius 1) and is pushed through a map into document coords. The map is
// defined by a scale profile s(y): a flat point (u, v) lands on the circular
// arc of its row — row center-line depth Y(v) solves dY/dv = s(Y), and the
// arc through it has curvature κ = d(ln s)/dy (the exact e^(az) relation:
// curvature radius = local scale / a). Where the profile is exponential in
// flat depth this reproduces e^(az) exactly (concentric arcs around the
// map's pole); where it is locally flat, rows are straight and the map is a
// pure similarity — angle error only appears at profile bends and stays
// ~cell-size / variation-scale.
const MH = 0.25; // table step in flat depth (≈ MH × local scale, in px)
const ROWV = 0.8660254; // flat row spacing for hex radius 1
// flat-top unit hexagon vertices (matches the k·60° loop of the old lattice)
const FVX = [1, 0.5, -0.5, -1, -0.5, 0.5];
const FVY = [0, ROWV, ROWV, 0, -ROWV, -ROWV];

const rowBuf = new Float64Array(3); // [yc, s, k] lerp scratch
function rowAt(st: HeroState, v: number): void {
  const n = st.myc.length;
  const t = (v - st.mv0) / MH;
  let i = Math.floor(t);
  if (i < 0) i = 0;
  if (i > n - 2) i = n - 2;
  const f = t - i; // unclamped: linear extrapolation just past the table
  rowBuf[0] = st.myc[i] + (st.myc[i + 1] - st.myc[i]) * f;
  rowBuf[1] = st.ms[i] + (st.ms[i + 1] - st.ms[i]) * f;
  rowBuf[2] = st.mk[i] + (st.mk[i + 1] - st.mk[i]) * f;
}

const mapPt = { x: 0, y: 0 }; // mapPoint scratch (build is single-threaded)
function mapPoint(st: HeroState, u: number, v: number): void {
  rowAt(st, v);
  const yc = rowBuf[0];
  const s = rowBuf[1];
  const k = rowBuf[2];
  const L = u * s; // arc length along the row
  const th = L * k;
  if (Math.abs(th) < 1e-4) {
    // κ→0 limit (straight row), series-stable
    mapPt.x = st.w * 0.5 + L * (1 - (th * th) / 6);
    mapPt.y = yc - 0.5 * L * L * k;
  } else {
    const rho = 1 / k;
    const sh = Math.sin(0.5 * th);
    mapPt.x = st.w * 0.5 + rho * Math.sin(th);
    mapPt.y = yc - 2 * rho * sh * sh; // = yc - rho·(1 - cos th), stable form
  }
}


// ---------- build: conformal hex lattice + point cloud ----------
function build(st: HeroState): void {
  makeSprites(st);
  let rngState = SEED;
  const rnd = (): number => {
    rngState = (rngState * 1103515245 + 12345) & 0x7fffffff;
    return rngState / 0x7fffffff;
  };

  const P = CFG;
  const w = st.w;
  const docH = st.docH;
  st.Rb = Math.max(31, Math.min(44, w / 33)) * P.rbMul;
  const sH = 0.9 * st.Rb; // map scale (= hex radius) at the hero copy
  const G = Math.max(1.001, P.growth);
  const heroC = st.heroTop + st.heroH * 0.5;

  // scale profile s(y) and its log-derivative (= row arc curvature):
  // exact conformal e^(az) — scale linear in distance from the pole, which
  // sits above the page; rows are concentric circular arcs around it
  const y1 = Math.max(heroC + 400, docH * 0.96);
  let rH = (y1 - heroC) / (G - 1);
  // keep the pole safely above the document top (short pages / extreme
  // growth would otherwise pull it into view and collapse the top rows)
  if (rH < heroC + 150) rH = heroC + 150;
  const cy = heroC - rH;
  const sOf = (y: number): number => (sH * (y - cy)) / rH;
  const gpOf = (y: number): number => 1 / (y - cy);

  // integrate the row-depth table dY/dv = s(Y) (midpoint steps) both ways
  // from the hero anchor; downward continues past docH because arcs bow up
  // at the edges — bottom corners are filled by rows whose centers overshoot
  const dnY: number[] = [];
  let yAcc = heroC;
  for (let i = 0; i < 6000; i++) {
    const s0 = sOf(yAcc);
    dnY.push(yAcc);
    if (yAcc > docH + 200 + (w * w * gpOf(yAcc)) / 8 + 3 * s0) break;
    yAcc += MH * sOf(yAcc + 0.5 * MH * s0);
  }
  const upY: number[] = [];
  yAcc = heroC;
  for (let i = 0; i < 4000; i++) {
    const s0 = sOf(yAcc);
    yAcc -= MH * sOf(yAcc - 0.5 * MH * s0);
    if (yAcc < -2.5 * sOf(yAcc)) break;
    upY.push(yAcc);
  }
  const nodes = upY.length + dnY.length;
  st.mv0 = -MH * upY.length;
  st.myc = new Float64Array(nodes);
  st.ms = new Float64Array(nodes);
  st.mk = new Float64Array(nodes);
  for (let i = 0; i < nodes; i++) {
    const y = i < upY.length ? upY[upY.length - 1 - i] : dnY[i - upY.length];
    st.myc[i] = y;
    st.ms[i] = sOf(y);
    st.mk[i] = Math.max(0, gpOf(y));
  }

  // lattice: perfect flat-domain comb, every cell mapped; cull only cells
  // whose mapped hex lies fully outside the document (no zone/seam culling —
  // the map grades cell size continuously, so there are no seams to hide)
  const cells: Cell[] = [];
  const jMin = Math.ceil(st.mv0 / ROWV);
  const jMax = Math.floor((st.mv0 + (nodes - 1) * MH) / ROWV);
  for (let j = jMin; j <= jMax; j++) {
    const v = j * ROWV;
    rowAt(st, v);
    const yc = rowBuf[0];
    const s = rowBuf[1];
    const kv = rowBuf[2];
    if (yc < -2.2 * s) continue;
    // widest u still mapping inside the page (+ margin); on a curved row the
    // needed arc angle is asin(halfW·κ), capped at π/2 (deeper rows cover
    // the corners such an arc can't reach)
    const halfW = w / 2 + 2.5 * s;
    let uMax: number;
    if (kv > 1e-9) {
      const sn = halfW * kv;
      const th = sn >= 1 ? Math.PI / 2 : Math.asin(sn);
      uMax = th / (kv * s);
    } else uMax = halfW / s;
    const off = j % 2 ? 1.5 : 0;
    const q0 = Math.ceil((-uMax - off) / 3);
    const q1 = Math.floor((uMax - off) / 3);
    for (let q = q0; q <= q1; q++) {
      const u = off + 3 * q;
      mapPoint(st, u, v);
      const cxm = mapPt.x;
      const cym = mapPt.y;
      let inside =
        cxm >= -8 && cxm <= w + 8 && cym >= -8 && cym <= docH + 8;
      for (let k6 = 0; k6 < 6 && !inside; k6++) {
        mapPoint(st, u + FVX[k6], v + FVY[k6]);
        inside =
          mapPt.x >= -8 &&
          mapPt.x <= w + 8 &&
          mapPt.y >= -8 &&
          mapPt.y <= docH + 8;
      }
      if (!inside) continue;
      cells.push({
        x: cxm,
        y: cym,
        u,
        v,
        r: s,
        k: 0,
        damp: 0.15 + 0.85 * chan(st, cxm, cym),
        p0: 0,
        p1: 0,
      });
    }
  }
  st.cells = cells;

  let beadCap = 0;
  for (const c of cells) beadCap += 6 * Math.max(4, Math.round(c.r / 9));
  // ambient flock: fixed candidate count with a single accept roll each, so
  // the per-section density levels set absolute counts (not just shares)
  const attempts = Math.round((w * docH) / 90);
  const ambCap = Math.min(9000, attempts);
  const cap = beadCap + ambCap + 64;
  const bx = new Float32Array(cap);
  const by = new Float32Array(cap);
  const hx = new Float32Array(cap);
  const hy = new Float32Array(cap);
  const amp = new Float32Array(cap);
  const sz = new Float32Array(cap);
  const dsz = new Float32Array(cap);
  const br = new Float32Array(cap);
  const ordv = new Float32Array(cap);
  const ci = new Int32Array(cap);
  const px = new Float32Array(cap);
  const py = new Float32Array(cap);
  const ma = new Float32Array(cap);
  const pi = new Uint8Array(cap);
  const buckets: number[][] = [];
  for (let c = 0; c < NSEC * NCOL; c++) buckets.push([]);
  let n = 0;

  // seam beads: every cell can crystallize; its beads live scattered in the
  // flock. Beads are laid along the *mapped* edges (each bead's flat
  // position pushed through the map), so seams curve with their rows.
  for (let cidx = 0; cidx < cells.length; cidx++) {
    const cell = cells[cidx];
    cell.p0 = n;
    // ~9px bead spacing: stars stay distinct when locked
    const nb = Math.max(4, Math.round(cell.r / 9));
    const phase = rnd(); // where this cell's seam starts being laid
    for (let e = 0; e < 6; e++) {
      const fx0 = FVX[e];
      const fy0 = FVY[e];
      const fx1 = FVX[(e + 1) % 6];
      const fy1 = FVY[(e + 1) % 6];
      for (let b = 0; b < nb; b++) {
        const tt = (b + 0.5 + (rnd() - 0.5) * 0.5) / nb;
        mapPoint(
          st,
          cell.u + fx0 + (fx1 - fx0) * tt,
          cell.v + fy0 + (fy1 - fy0) * tt,
        );
        const xx = mapPt.x;
        const yy = mapPt.y;
        if (xx < -8 || xx > w + 8 || yy < -8 || yy > docH + 8) continue;
        // calm channel + flock clumping + per-section density (floored so a
        // condensed hexagon still reads as a dotted outline deep down);
        // densest regions thinned a touch so formations stay quiet
        const dl = secDens(st, yy);
        if (
          rnd() >
          (0.92 - 0.09 * ss(0.7, 1, dl)) *
            (0.08 + 0.92 * chan(st, xx, yy)) *
            (0.42 + 0.85 * fbm(xx, yy)) *
            (0.18 + 0.82 * dl * colDamp(st, xx, yy))
        )
          continue;
        if (n >= cap) break;
        hx[n] = xx + (rnd() - 0.5) * 2.4;
        hy[n] = yy + (rnd() - 0.5) * 2.4;
        let per = (e + tt) / 6 - phase;
        per -= Math.floor(per);
        // lay order around the perimeter; scaled by (1 - flight window) at
        // frame time so spread + window always sum to 1 and the last star
        // lands exactly when the cell reaches k=1
        ordv[n] = per;
        const ang = rnd() * TAU;
        const rad = 14 + 56 * rnd();
        bx[n] = xx + Math.cos(ang) * rad;
        by[n] = yy + Math.sin(ang) * rad;
        amp[n] = 6 + 8 * rnd();
        sz[n] = 1.15 + 1.1 * rnd();
        dsz[n] = sz[n] * DK;
        br[n] = 0.4 + 0.6 * Math.pow(rnd(), 1.7);
        ci[n] = cidx;
        // section palette: position-hashed blend inside boundary feathers
        const sec = secIndex(st, yy, rnd());
        // beads sample the section's mix too (clump-biased like the ambient
        // flock, slightly tempered), so formations inherit the local sky's
        // colors instead of being all-warm
        const cool = rnd() < 0.8 * coolP(sec, xx, yy);
        const cb = cool ? 4 : rnd() < 0.5 ? 0 : rnd() < 0.62 ? 1 : 2;
        pi[n] = sec * NCOL + cb;
        buckets[sec * NCOL + cb].push(n);
        n++;
      }
    }
    cell.p1 = n;
  }

  // ambient flock: unstructured speckle, never condenses; per-section
  // density plateaus (feathered at the real boundaries) set the local level
  let got = 0;
  for (let a2 = 0; a2 < attempts && n < cap && got < ambCap; a2++) {
    const x = rnd() * w;
    const yy = rnd() * docH;
    const v = fbm(x, yy);
    if (
      rnd() >
      chan(st, x, yy) *
        (0.09 + 0.91 * Math.pow(v, 1.8)) *
        secDens(st, yy) *
        colDamp(st, x, yy)
    )
      continue;
    bx[n] = x;
    by[n] = yy;
    hx[n] = x;
    hy[n] = yy;
    ordv[n] = 9;
    ci[n] = -1;
    amp[n] = 9 + 11 * rnd();
    sz[n] = 0.95 + 1.2 * rnd();
    dsz[n] = sz[n] * DK;
    br[n] = 0.4 + 0.6 * Math.pow(rnd(), 1.7);
    const sec = secIndex(st, yy, rnd());
    buckets[sec * NCOL + ambSlot(sec, x, yy, rnd(), rnd())].push(n);
    n++;
    got++;
  }

  st.N = n;
  st.bx = bx;
  st.by = by;
  st.hx = hx;
  st.hy = hy;
  st.amp = amp;
  st.sz = sz;
  st.dsz = dsz;
  st.br = br;
  st.ordv = ordv;
  st.ci = ci;
  st.px = px;
  st.py = py;
  st.ma = ma;
  st.buckets = buckets;
  st.pi = pi;
  st.kbuf = new Float32Array(cells.length);
}

// ---------- condensation focus + targets ----------
// One focus for the whole piece: with no cursor it wanders the current
// viewport on a slow closed loop (the entire show on touch devices); a
// cursor pulls it over with heavy lag; on pointer leave it drifts back into
// the wander from wherever it is. `dt` is seconds since the previous frame;
// the mockup's per-frame easing rates (assumed 60fps) are converted with
// 1-(1-r)^(dt*60) so speed is frame-rate independent.
function targets(st: HeroState, tsec: number, dt: number): void {
  const P = CFG;
  const w = st.w;
  const vh = st.vh;
  const th = (TAU * (tsec % TW)) / TW;
  const ax =
    w *
    (0.5 +
      0.34 * Math.cos(th) +
      0.11 * Math.cos(2 * th + 2.1) +
      0.05 * Math.cos(5 * th + 0.7));
  const ay =
    st.scrollY +
    vh *
      (0.5 +
        0.3 * Math.sin(th) +
        0.11 * Math.sin(3 * th + 1.15) +
        0.05 * Math.sin(4 * th + 2.4));
  if (!st.fOn) {
    st.fx = ax;
    st.fy = ay;
    st.fOn = true;
  }
  const tx = st.curOn ? st.curX : ax;
  const ty = st.curOn ? st.curY + st.scrollY : ay;
  const dt60 = dt * 60;
  const er = st.curOn ? 0.022 : 0.012; // lazy pull toward cursor, lazier drift back
  const ef = 1 - Math.pow(1 - er, dt60);
  st.fx += (tx - st.fx) * ef;
  st.fy += (ty - st.fy) * ef;
  const kIn = 1 - Math.pow(1 - P.kIn, dt60); // gradual consent in
  const kOut = 1 - Math.pow(1 - P.kIn / 2, dt60); // slower relax out
  const cells = st.cells;
  const yLo = st.scrollY - 700;
  const yHi = st.scrollY + st.vh + 700;
  for (let i = 0; i < cells.length; i++) {
    const cell = cells[i];
    // dormant cells far off-screen need no update: no decay to run, and the
    // focus (wander or cursor) is always near the viewport
    if (cell.k === 0 && (cell.y < yLo || cell.y > yHi)) {
      st.kbuf[i] = 0;
      continue;
    }
    const dx = cell.x - st.fx;
    const dy = cell.y - st.fy;
    // fixed-pixel focus reach: the formation covers a constant area
    // everywhere, so it spans many small cells near the hero and only a
    // couple of the large deep-page cells
    const sc = P.reach;
    let tg = ss(0.05, 0.55, Math.exp(-(dx * dx + dy * dy) / (2 * sc * sc)));
    tg *= cell.damp;
    cell.k += (tg - cell.k) * (tg > cell.k ? kIn : kOut);
    if (cell.k < 0.0005) cell.k = 0;
    st.kbuf[i] = cell.k;
  }
}

// ---------- per-frame positions ----------
function positions(st: HeroState, t: number, ymin: number, ymax: number): void {
  const { bx, by, hx, hy, amp, ordv, ci, px, py, ma, kbuf } = st;
  // wide flight window (default 0.7): each star's glide onto its seat spans
  // most of the cell's fill, so flights are slow and overlapping rather than
  // sequential snaps; lay-order spread is 1-window so the two always sum to 1
  const win = CFG.window;
  const spread = 1 - win;
  for (let i = 0; i < st.N; i++) {
    const x = bx[i];
    const y = by[i];
    if (y < ymin || y > ymax) {
      py[i] = -1e9;
      ma[i] = 0;
      continue;
    }
    const A = amp[i];
    const dx =
      A *
      (Math.sin(W0 * t + x * 0.0057 + y * 0.0034) +
        0.55 * Math.sin(2 * W0 * t + y * 0.0079 - x * 0.0026 + 1.7) +
        0.3 * Math.sin(3 * W0 * t + x * 0.0041 + 4.1));
    const dy =
      A *
      (Math.cos(W0 * t + x * 0.0038 - y * 0.0061 + 0.9) +
        0.55 * Math.sin(2 * W0 * t + x * 0.0068 + 2.6) +
        0.3 * Math.cos(3 * W0 * t + y * 0.0052 + 0.5));
    let m = 0;
    const c = ci[i];
    if (c >= 0) {
      const k = kbuf[c];
      if (k > 0.001) {
        const o = ordv[i] * spread;
        m = ss(o, o + win, k);
      }
    }
    ma[i] = m;
    const fx = x + dx;
    const fy = y + dy;
    if (m > 0) {
      const em = m * m * (3 - 2 * m);
      px[i] = fx + (hx[i] - fx) * em;
      py[i] = fy + (hy[i] - fy) * em;
    } else {
      px[i] = fx;
      py[i] = fy;
    }
  }
}

// ---------- paint ----------
// Stars only — no seam strokes, halos on edges, or vertex glints. A formed
// hexagon reads purely from where the stars sit; locking stars keep their
// size and instead brighten with a warm glow.
function paint(st: HeroState): void {
  const { ctx, px, py, dsz, br, ma } = st;
  const ct = st.canvasTop;
  ctx.setTransform(st.dpr, 0, 0, st.dpr, 0, -ct * st.dpr);
  ctx.clearRect(0, ct, st.w, st.canvasH);
  const ymin = ct - 40;
  const ymax = ct + st.canvasH + 40;
  for (let s = 0; s < NSEC; s++) {
    for (let c = 0; c < NCOL; c++) {
      const spr = st.sprites[s][c];
      const bk = st.buckets[s * NCOL + c];
      for (let k2 = 0; k2 < bk.length; k2++) {
        const i = bk[k2];
        const y = py[i];
        if (y < ymin || y > ymax) continue;
        const d = dsz[i];
        ctx.globalAlpha = br[i];
        ctx.drawImage(spr, px[i] - d * 0.5, y - d * 0.5, d, d);
      }
    }
  }
  // condensing beads: constant size and their own color — a locked star
  // just glows a little brighter than a drifting one, so the stars read as
  // rearranging into hexagons rather than changing identity
  const cells = st.cells;
  if (!st.lockSpr.length || !st.haloSpr.length) {
    ctx.globalAlpha = 1;
    return;
  }
  const pi = st.pi;
  const glow = CFG.glow;
  for (let ci2 = 0; ci2 < cells.length; ci2++) {
    const cell = cells[ci2];
    if (cell.k <= 0.02) continue;
    if (cell.y < ymin - 2 * cell.r || cell.y > ymax + 2 * cell.r) continue;
    // deep cells still crystallize crisply, but brighten quietly (their
    // section's density level dims the glow) so they never fight the lower
    // text sections
    const dim = 0.3 + 0.7 * secDens(st, cell.y);
    for (let j = cell.p0; j < cell.p1; j++) {
      const m = ma[j];
      if (m <= 0.22) continue;
      const y = py[j];
      if (y < ymin || y > ymax) continue;
      const d = dsz[j];
      const g = 0.72 * glow * ss(0.22, 0.85, m) * dim; // softened ~30%: quieter formations
      if (g <= 0.004) continue;
      const ps = (pi[j] / NCOL) | 0;
      const pc = pi[j] % NCOL;
      if (HALO_A > 0) {
        const hd = d * 2;
        ctx.globalAlpha = HALO_A * g;
        ctx.drawImage(st.haloSpr[ps][pc], px[j] - hd * 0.5, y - hd * 0.5, hd, hd);
      }
      ctx.globalAlpha = g;
      ctx.drawImage(st.lockSpr[ps][pc], px[j] - d * 0.5, y - d * 0.5, d, d);
    }
  }
  ctx.globalAlpha = 1;
}

// ---------- reduced-motion still: frozen flock, a few formed cells ----------
function still(st: HeroState): void {
  const hb = st.heroTop + st.heroH;
  const pres: [number, number, number][] = [
    [st.w * 0.15, st.heroTop + st.heroH * 0.3, 1],
    [st.w * 0.84, st.heroTop + st.heroH * 0.6, 1],
    [st.w * 0.26, st.heroTop + st.heroH * 0.78, 0.85],
    [st.w * 0.77, st.heroTop + st.heroH * 0.22, 0.72],
    [st.w * 0.62, Math.min(st.docH - 200, hb + st.vh * 0.55), 0.8],
  ];
  const s2 = 2 * Math.pow(0.9 * st.Rb * 1.7320508, 2);
  for (let i = 0; i < st.cells.length; i++) {
    const cell = st.cells[i];
    let k = 0;
    for (let p = 0; p < pres.length; p++) {
      const dx = cell.x - pres[p][0];
      const dy = cell.y - pres[p][1];
      const e =
        pres[p][2] * Math.min(1, 2.2 * Math.exp(-(dx * dx + dy * dy) / s2));
      if (e > k) k = e;
    }
    cell.k = Math.min(1, k) * cell.damp;
    st.kbuf[i] = cell.k;
  }
  positions(st, 13.4, -1e9, 1e9);
  paint(st);
}

export function KintsugiHero({ children }: { children: ReactNode }) {
  const stageRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const stage = stageRef.current;
    const canvas = canvasRef.current;
    if (!stage || !canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;

    const st: HeroState = {
      ctx,
      w: 0,
      vh: 0,
      docH: 0,
      heroTop: 0,
      heroH: 0,
      dpr: 1,
      scrollY: 0,
      canvasTop: 0,
      canvasH: 0,
      curOn: false,
      curX: 0,
      curY: 0,
      fx: 0,
      fy: 0,
      fOn: false,
      secB: [],
      secC: [],
      cols: [],
      Rb: 0,
      mv0: 0,
      myc: new Float64Array(0),
      ms: new Float64Array(0),
      mk: new Float64Array(0),
      cells: [],
      N: 0,
      bx: new Float32Array(0),
      by: new Float32Array(0),
      hx: new Float32Array(0),
      hy: new Float32Array(0),
      amp: new Float32Array(0),
      sz: new Float32Array(0),
      dsz: new Float32Array(0),
      br: new Float32Array(0),
      ordv: new Float32Array(0),
      ci: new Int32Array(0),
      px: new Float32Array(0),
      py: new Float32Array(0),
      ma: new Float32Array(0),
      buckets: [],
      kbuf: new Float32Array(0),
      sprites: [],
      lockSpr: [],
      haloSpr: [],
      pi: new Uint8Array(0),
    };

    // Re-center the painted band over the viewport. The canvas is absolute
    // in the stage (document top = heroTop), so the compositor scrolls it
    // with the text between repaints; the transform is only rewritten when
    // the band actually moves. Returns whether it moved (caller repaints).
    let placedY: number | null = null;
    const place = (): boolean => {
      const top = Math.max(
        0,
        Math.min(st.scrollY - SCROLL_M, st.docH - st.canvasH),
      );
      const ty = top - st.heroTop;
      if (top === st.canvasTop && ty === placedY) return false;
      st.canvasTop = top;
      placedY = ty;
      canvas.style.transform = `translate3d(0, ${ty}px, 0)`;
      return true;
    };

    function resize(): void {
      if (!stage || !canvas) return;
      const vw = Math.max(320, window.innerWidth);
      const vh = Math.max(200, window.innerHeight);
      // The parked canvas's own footprint (absolute + translate3d, clamped
      // to the previous docH) extends scrollHeight, which would ratchet
      // docH — it could grow but never shrink after a reflow. Hide the
      // canvas for the measurement; nothing paints mid-task, so the toggle
      // is invisible.
      canvas.style.display = "none";
      const docH = Math.max(vh, document.documentElement.scrollHeight);
      canvas.style.display = "";
      const rect = stage.getBoundingClientRect();
      const scY = window.scrollY;
      const heroTop = rect.top + scY;
      const heroH = Math.max(200, rect.height);
      // per-section geometry: real section rects + testimonial text columns,
      // in document coords (scroll-invariant; re-measured on any reflow)
      const secNames = ["testimonials", "workshops", "tools", "about"];
      const secB: number[] = [];
      const secC: number[] = [];
      const cols: HeroState["cols"] = [];
      for (const name of secNames) {
        const el = document.querySelector(`[data-star-section="${name}"]`);
        if (!el) break;
        const r = el.getBoundingClientRect();
        secB.push(r.top + scY);
        secC.push((r.top + r.bottom) / 2 + scY);
        if (name === "testimonials") {
          for (const q of Array.from(el.querySelectorAll("blockquote"))) {
            const qr = q.getBoundingClientRect();
            cols.push({
              x0: qr.left,
              x1: qr.right,
              y0: qr.top + scY,
              y1: qr.bottom + scY,
            });
          }
        }
      }
      // footer counts as about: its anchor spans to the document bottom
      if (secC.length === 4) secC[3] = (secB[3] + docH) / 2;
      let secSame = secB.length === st.secB.length;
      if (secSame)
        for (let i = 0; i < secB.length; i++)
          if (Math.abs(secB[i] - st.secB[i]) >= 8) secSame = false;
      // the field lattice lives in document coords and only depends on the
      // structural measures below — viewport height is not one of them, so a
      // mobile URL-bar show/hide (which changes innerHeight on every scroll
      // direction flip) must not re-seed the formation
      const structural =
        vw !== st.w ||
        Math.abs(docH - st.docH) >= 24 ||
        Math.abs(heroTop - st.heroTop) >= 8 ||
        Math.abs(heroH - st.heroH) >= 8 ||
        !secSame;
      const dpr = Math.min(2, window.devicePixelRatio || 1);
      if (!structural && vh === st.vh && dpr === st.dpr) return;
      st.w = vw;
      st.vh = vh;
      st.dpr = dpr;
      st.canvasH = vh + 2 * SCROLL_M;
      canvas.width = vw * st.dpr;
      canvas.height = st.canvasH * st.dpr;
      canvas.style.height = `${st.canvasH}px`;
      st.scrollY = window.scrollY;
      if (structural) {
        st.docH = docH;
        st.heroTop = heroTop;
        st.heroH = heroH;
        st.secB = secB;
        st.secC = secC;
        st.cols = cols;
        build(st);
      }
      place();
      if (reduced) still(st);
      else if (st.N) paint(st); // backing-store resize cleared the canvas
    }

    resize();

    let pendingResize = 0;
    const queueResize = (): void => {
      if (pendingResize) return;
      pendingResize = requestAnimationFrame(() => {
        pendingResize = 0;
        resize();
      });
    };
    // body height changes (fonts, responsive reflow) reshape the field
    const observer = new ResizeObserver(queueResize);
    observer.observe(document.body);
    window.addEventListener("resize", queueResize);

    const onPointerMove = (ev: PointerEvent): void => {
      if (ev.pointerType && ev.pointerType !== "mouse") return; // touch: wander is the show
      // viewport coords: the focus target follows the cursor through scrolls
      // (converted to document coords at frame time, not event time)
      st.curX = ev.clientX;
      st.curY = ev.clientY;
      st.curOn = true;
    };
    const onPointerOut = (ev: PointerEvent): void => {
      if (!ev.relatedTarget) st.curOn = false; // left the window
    };
    const onBlur = (): void => {
      st.curOn = false;
    };

    let raf = 0;
    let pendingScroll = 0;
    // The compositor scrolls the canvas with the page, so ordinary scrolling
    // needs no work here. Jumps (PageDown, scrollbar drags, anchor links,
    // flings past SCROLL_M per tick) can move the viewport off the painted
    // band between simulation ticks — catch those on the scroll event and
    // re-center immediately rather than showing a starless band until the
    // next tick.
    const onScroll = (): void => {
      if (pendingScroll) return;
      pendingScroll = requestAnimationFrame(() => {
        pendingScroll = 0;
        st.scrollY = window.scrollY;
        if (reduced) {
          // re-center (and repaint — still() positioned every star) only
          // when the viewport nears the painted band's edge
          if (
            st.scrollY - st.canvasTop < 40 ||
            st.scrollY + st.vh > st.canvasTop + st.canvasH - 40
          ) {
            if (place()) paint(st);
          }
          return;
        }
        if (
          st.N &&
          (st.scrollY < st.canvasTop ||
            st.scrollY + st.vh > st.canvasTop + st.canvasH) &&
          place()
        ) {
          positions(
            st,
            (performance.now() / 1000) % T,
            st.canvasTop - 160,
            st.canvasTop + st.canvasH + 160,
          );
          paint(st);
        }
      });
    };
    window.addEventListener("scroll", onScroll, { passive: true });

    if (!reduced) {
      window.addEventListener("pointermove", onPointerMove);
      window.addEventListener("pointerout", onPointerOut);
      window.addEventListener("blur", onBlur);

      let lastNow: number | null = null;
      const frame = (now: number): void => {
        // the field is slow by design; rendering above ~30fps is wasted CPU
        // (a 120Hz display would otherwise quadruple the work). dt-scaled
        // easing keeps the motion identical at any effective rate.
        if (lastNow !== null && now - lastNow < 1000 / 30 - 3) {
          raf = requestAnimationFrame(frame);
          return;
        }
        const tsec = now / 1000;
        const t = tsec % T;
        // clamp dt so a backgrounded tab doesn't jump on resume
        const dt =
          lastNow === null
            ? 1 / 60
            : Math.min(0.1, Math.max(0, (now - lastNow) / 1000));
        lastNow = now;
        if (st.N) {
          st.scrollY = window.scrollY;
          place();
          targets(st, tsec, dt);
          positions(st, t, st.canvasTop - 160, st.canvasTop + st.canvasH + 160);
          paint(st);
        }
        raf = requestAnimationFrame(frame);
      };
      raf = requestAnimationFrame(frame);
    }

    return () => {
      if (raf) cancelAnimationFrame(raf);
      if (pendingResize) cancelAnimationFrame(pendingResize);
      if (pendingScroll) cancelAnimationFrame(pendingScroll);
      observer.disconnect();
      window.removeEventListener("resize", queueResize);
      window.removeEventListener("scroll", onScroll);
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerout", onPointerOut);
      window.removeEventListener("blur", onBlur);
    };
  }, []);

  return (
    <div ref={stageRef} className="kintsugi-stage">
      <canvas ref={canvasRef} className="kintsugi-canvas" aria-hidden="true" />
      <div className="kintsugi-scrim" aria-hidden="true" />
      <div className="kintsugi-copy">{children}</div>
    </div>
  );
}
