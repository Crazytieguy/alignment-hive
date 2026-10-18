// The landing-page hero stage and its full-page starfield canvas. The simulation and drawing
// live in kintsugi-field.ts; this component measures the page (section rects, testimonial text
// columns, document height), feeds the field pointer, touch and scroll input, and runs the
// ~30fps animation loop (or a single still frame under prefers-reduced-motion).
import { useEffect, useRef } from "react";
import type { ReactNode } from "react";
import "./kintsugi-hero.css";
import {
  type HeroState,
  SCROLL_M,
  T,
  build,
  createHeroState,
  paint,
  positions,
  still,
  targets,
} from "./kintsugi-field";

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

    const st = createHeroState(ctx);

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
      const scY = window.scrollY;
      // Document height is the body's own box, not scrollHeight: the parked
      // canvas (absolute + translate3d, clamped to the previous docH) sits
      // in scrollHeight, which would ratchet docH — it could grow but never
      // shrink after a reflow. (Hiding the canvas for the measurement
      // instead passes it through display:none, which restarts its fade-in
      // animation — visibly, on every mobile URL-bar resize.)
      const docH = Math.max(
        vh,
        document.body.getBoundingClientRect().bottom + scY,
      );
      const rect = stage.getBoundingClientRect();
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
          // text columns to damp under: the section marks them with
          // [data-star-column] (the carousel's visible slot, not its slides)
          for (const q of el.querySelectorAll("[data-star-column]")) {
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
    // first frame is on the canvas: fade it up (see .kintsugi-canvas in the css)
    canvas.classList.add("is-lit");

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

    // viewport coords: the focus target follows the pointer through scrolls
    // (converted to document coords at frame time, not event time)
    let touchRelease = 0;
    const follow = (x: number, y: number): void => {
      st.curX = x;
      st.curY = y;
      st.curOn = true;
      if (touchRelease) {
        clearTimeout(touchRelease);
        touchRelease = 0;
      }
    };
    // Pointer events steer for the mouse only; touch and pen go through the
    // touch events below (pens fire compat touch events), so a pen lift or a
    // stray tap on a hybrid device can't drop focus out from under the mouse.
    const onPointerMove = (ev: PointerEvent): void => {
      if (ev.pointerType !== "mouse") return;
      follow(ev.clientX, ev.clientY);
    };
    // Letting go: the wander resumes from where the focus is, not from where
    // the curve would have been — shift the curve to pass through the focus.
    const release = (): void => {
      st.wox = st.fx - st.ax;
      st.woy = st.fy - st.ay;
      st.curOn = false;
    };
    const onPointerOut = (ev: PointerEvent): void => {
      if (ev.pointerType !== "mouse") return;
      if (!ev.relatedTarget) release(); // left the window
    };
    // Touch: taps and drags pull the focus like a cursor does. Touch events
    // (not pointer events) so a drag keeps steering while the page scrolls;
    // after the finger lifts the focus lingers a few seconds, then drifts
    // back into the wander.
    const onTouch = (ev: TouchEvent): void => {
      const t = ev.touches[0];
      if (t) follow(t.clientX, t.clientY);
    };
    const onTouchEnd = (ev: TouchEvent): void => {
      if (ev.touches.length > 0) return; // another finger is still down
      if (touchRelease) clearTimeout(touchRelease);
      touchRelease = window.setTimeout(() => {
        release();
        touchRelease = 0;
      }, 3000);
    };
    const onBlur = release;

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
      window.addEventListener("touchstart", onTouch, { passive: true });
      window.addEventListener("touchmove", onTouch, { passive: true });
      window.addEventListener("touchend", onTouchEnd);
      window.addEventListener("touchcancel", onTouchEnd);

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
      window.removeEventListener("touchstart", onTouch);
      window.removeEventListener("touchmove", onTouch);
      window.removeEventListener("touchend", onTouchEnd);
      window.removeEventListener("touchcancel", onTouchEnd);
      if (touchRelease) clearTimeout(touchRelease);
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
