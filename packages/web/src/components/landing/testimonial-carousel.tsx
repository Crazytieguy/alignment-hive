import { Button, cn } from "@alignment-hive/ui";
import { ChevronLeft, ChevronRight } from "lucide-react";
import { useEffect, useRef, useState } from "react";

export interface TestimonialItem {
  attribution: string;
  quote: string;
}

// Each quote stays up long enough to read it: a floor, plus reading time for
// the longer ones (~200 wpm).
const MIN_DWELL_MS = 8000;
const MS_PER_WORD = 300;
const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";

function dwellMs(quote: string): number {
  return Math.max(MIN_DWELL_MS, quote.split(/\s+/).length * MS_PER_WORD);
}

/**
 * One centered quote at a time, sliding horizontally. Beneath it, one
 * centered control cluster: prev arrow, dots, next arrow.
 * Auto-advances after a per-quote dwell with the active dot filling as a
 * progress bar; pauses while hovered, focused, or scrolled out of view, and
 * stops entirely under prefers-reduced-motion. Every slide stays in the flex
 * track, so the tallest quote sets the height and it never jumps.
 */
export function TestimonialCarousel({
  items,
}: {
  items: readonly TestimonialItem[];
}) {
  const [reducedMotion, setReducedMotion] = useState(false);
  const [current, setCurrent] = useState(0);
  const count = items.length;

  const rootRef = useRef<HTMLDivElement>(null);
  const fillRef = useRef<HTMLSpanElement>(null);
  // each of these pauses autoplay on its own
  const hoveredRef = useRef(false);
  const focusedRef = useRef(false);
  const offscreenRef = useRef(true);
  const progressRef = useRef(0);
  // the live region speaks only for user-initiated changes, never for the
  // auto-rotation (which would otherwise announce every 8-15s for the page's life)
  const [announced, setAnnounced] = useState("");

  useEffect(() => {
    const reduced = matchMedia(REDUCED_MOTION_QUERY);
    const sync = (): void => setReducedMotion(reduced.matches);
    sync();
    reduced.addEventListener("change", sync);
    return () => reduced.removeEventListener("change", sync);
  }, []);

  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const io = new IntersectionObserver(
      ([e]) => {
        offscreenRef.current = !e?.isIntersecting;
      },
      { threshold: 0.5 },
    );
    io.observe(root);
    return () => io.disconnect();
  }, []);

  const go = (next: number): void => {
    const i = ((next % count) + count) % count;
    progressRef.current = 0;
    // a new index remounts the fill; the same index needs the reset by hand
    if (i === current) fillRef.current?.style.setProperty("--p", "0");
    setCurrent(i);
    setAnnounced(`Showing testimonial ${i + 1} of ${count}`);
  };

  // Autoplay: restarts (progress 0) whenever the slide changes.
  useEffect(() => {
    if (reducedMotion || count < 2) return;
    const dwell = dwellMs(items[current]?.quote ?? "");
    progressRef.current = 0;
    let raf = 0;
    let last: number | null = null;
    const tick = (now: number): void => {
      raf = requestAnimationFrame(tick);
      if (hoveredRef.current || focusedRef.current || offscreenRef.current) {
        last = null;
        return;
      }
      if (last === null) {
        last = now;
        return;
      }
      // clamp so a backgrounded tab or a long stall doesn't skip ahead on resume
      progressRef.current += Math.min(now - last, 100) / dwell;
      last = now;
      if (progressRef.current >= 1) {
        setCurrent((current + 1) % count);
        return;
      }
      // per frame on purpose: quantizing the 28px fill reads as visible steps
      fillRef.current?.style.setProperty("--p", String(progressRef.current));
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [reducedMotion, count, items, current]);

  if (count === 0) return null;

  const arrow = (dir: -1 | 1) => (
    <Button
      variant="outline"
      size="icon-lg"
      // round and quieter than the default square outline: a hairline ring
      // that warms to amber on hover, so the arrows don't compete with the quote
      className="rounded-full border-border bg-transparent text-foreground/75 shadow-none hover:border-primary/50 hover:bg-primary/10 hover:text-primary dark:border-border dark:bg-transparent dark:hover:bg-primary/10"
      aria-label={dir < 0 ? "Previous testimonial" : "Next testimonial"}
      onClick={() => go(current + dir)}
    >
      {dir < 0 ? <ChevronLeft /> : <ChevronRight />}
    </Button>
  );

  return (
    <div
      ref={rootRef}
      role="region"
      aria-roledescription="carousel"
      aria-label="Testimonials"
      onPointerEnter={() => {
        hoveredRef.current = true;
      }}
      onPointerLeave={() => {
        hoveredRef.current = false;
      }}
      onFocus={(e) => {
        // keyboard focus pauses; the focus a mouse click leaves on a button
        // must not, or one click would stop the rotation for good
        focusedRef.current = e.target.matches(":focus-visible");
      }}
      onBlur={(e) => {
        if (!rootRef.current?.contains(e.relatedTarget))
          focusedRef.current = false;
      }}
    >
      {/* A centered pull quote on a 42rem measure, with one compact control
          cluster (arrow, dots, arrow) centered beneath it. */}
      <div className="mx-auto max-w-[42rem]">
        {/* Slides carry a little side padding that the track cancels, so the
            quote mark's slight overhang isn't clipped and a page is still
            exactly one track width. data-star-column tells the starfield where
            the text sits (kintsugi-hero.tsx). */}
        <div className="overflow-x-clip" data-star-column="">
          <div
            className="-mx-2 flex items-start transition-transform duration-[1400ms] ease-[cubic-bezier(0.32,0.72,0,1)] motion-reduce:transition-none"
            style={{ transform: `translateX(-${current * 100}%)` }}
          >
            {items.map((t, i) => (
              <div
                key={t.quote}
                role="group"
                aria-label={`${i + 1} of ${count}`}
                inert={i !== current}
                // the outgoing slide fades quickly and the incoming one only
                // after most of its (front-loaded) slide, so neither shows a
                // hard-clipped edge mid-transition
                className={cn(
                  "w-full shrink-0 px-2 transition-opacity motion-reduce:transition-none",
                  i === current
                    ? "delay-[450ms] duration-[700ms]"
                    : "opacity-0 duration-[350ms]",
                )}
              >
                <Testimonial {...t} />
              </div>
            ))}
          </div>
        </div>

        {count > 1 && (
          <div className="mt-6 flex items-center justify-center gap-3">
            {arrow(-1)}
            {/* plain buttons (APG basic carousel), not a tablist: no roving
                focus or panel pairing to keep honest */}
            {/* each dot is a 6px pill inside a taller button, so it can be
                tapped; the button padding makes the visible 8px gaps */}
            <div className="flex items-center">
              {items.map((t, i) => {
                const active = i === current;
                return (
                  <button
                    key={t.quote}
                    type="button"
                    aria-current={active}
                    aria-label={`Testimonial ${i + 1} of ${count}`}
                    onClick={() => go(i)}
                    className="group flex h-8 items-center rounded-full px-1"
                  >
                    <span
                      className={cn(
                        "relative block h-1.5 overflow-hidden rounded-full transition-[width,background-color] duration-300 motion-reduce:transition-none",
                        active
                          ? "w-7 bg-primary/30"
                          : "w-1.5 bg-muted-foreground/60 group-hover:bg-muted-foreground",
                      )}
                    >
                      {active && (
                        <span
                          ref={fillRef}
                          aria-hidden="true"
                          className="absolute inset-y-0 left-0 w-[calc(var(--p,0)*100%)] rounded-full bg-primary motion-reduce:w-full"
                        />
                      )}
                    </span>
                  </button>
                );
              })}
            </div>
            {arrow(1)}
          </div>
        )}
      </div>
      <p className="sr-only" aria-live="polite">
        {announced}
      </p>
    </div>
  );
}

function Testimonial({ attribution, quote }: TestimonialItem) {
  return (
    <blockquote className="text-center [text-wrap:balance]">
      {/* the mark's ink sits in the top half of its line box; the fixed
          height trims the empty lower half so it rests just above the text */}
      <span
        aria-hidden="true"
        className="block h-8 text-[3.4rem] leading-none text-primary/85"
      >
        &ldquo;
      </span>
      {/* a centered, balanced pull quote, never justified: no stretched
          word gaps */}
      <p className="text-[1.0625rem] italic leading-[1.6] md:text-[1.1875rem]">
        {quote}
      </p>
      <footer className="mt-4 text-[0.85rem] tracking-[0.04em] text-muted-foreground">
        &mdash; {attribution}
      </footer>
    </blockquote>
  );
}
