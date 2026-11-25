// The four visuals. Their markup is in visual-markup.ts and their scripts in visual-scripts.ts; React renders the
// markup once and each script then runs on its own figure.
import { useEffect, useRef } from "react";
import { visualMarkup } from "./visual-markup";
import { initDebrief, initLadder, initTldr } from "./visual-scripts";

const INIT: Record<string, ((root: HTMLElement) => void) | undefined> = {
  tldr: initTldr,
  "debrief-tldr": initDebrief,
  ladder: initLadder,
};

export function Visual({ name }: { name: string }) {
  const ref = useRef<HTMLElement>(null);
  useEffect(() => {
    const el = ref.current;
    const init = INIT[name];
    if (!el || !init || el.dataset.ready) return;
    el.dataset.ready = "1";
    init(el);
  }, [name]);
  return (
    <div className="piece">
      <figure ref={ref} className={`v-${name}`} dangerouslySetInnerHTML={{ __html: visualMarkup[name] }} />
    </div>
  );
}
