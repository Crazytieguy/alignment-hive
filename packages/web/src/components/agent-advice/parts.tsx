// Building blocks of the agent-advice post. Class names match styles.css.
import { useId } from "react";
import type { ReactNode } from "react";

const Svg = ({ w, children }: { w: string; children: ReactNode }) => (
  <svg
    className="ic"
    viewBox="0 0 16 16"
    width="16"
    height="16"
    aria-hidden="true"
    fill="none"
    stroke="currentColor"
    strokeWidth={w}
    strokeLinecap="round"
    strokeLinejoin="round"
  >
    {children}
  </svg>
);

export const Icon = {
  check: <Svg w="2"><path d="M3.5 8.5l3 3 6-7" /></Svg>,
  cross: <Svg w="2"><path d="M4 4l8 8M12 4l-8 8" /></Svg>,
  chev: <Svg w="2"><path d="M6 3.5l4.5 4.5L6 12.5" /></Svg>,
  copy: (
    <Svg w="1.5">
      <rect x="5.5" y="5.5" width="8" height="8" rx="1.5" />
      <path d="M10.5 5.5v-2a1 1 0 0 0-1-1h-6a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2" />
    </Svg>
  ),
  clock: (
    <Svg w="1.5">
      <circle cx="8" cy="8" r="5.5" />
      <path d="M8 5v3.2l2 1.3" />
    </Svg>
  ),
  me: (
    <Svg w="1.5">
      <circle cx="8" cy="5.6" r="2.6" />
      <path d="M3 13.5c.6-2.6 2.6-4 5-4s4.4 1.4 5 4" />
    </Svg>
  ),
  agent: (
    <Svg w="1.6">
      <rect x="1.8" y="2.8" width="12.4" height="10.4" rx="2" />
      <path d="M4.6 6.4l2 1.7-2 1.7M8.4 10.2h3" />
    </Svg>
  ),
  recall: (
    <Svg w="1.5">
      <path d="M2.8 8a5.2 5.2 0 1 0 1.5-3.7" />
      <path d="M2.6 2.6v2.6h2.6" />
      <path d="M8 5.4V8l1.8 1.2" />
    </Svg>
  ),
  comment: <Svg w="1.5"><path d="M2.5 3.5h11v7h-6l-3 2.5v-2.5h-2z" /></Svg>,
};

// ---------- tooltips ----------
// The term is focusable; interactions.ts opens the explanation on hover, focus or tap.
export function Tip({ tip, children }: { tip: string; children: ReactNode }) {
  const id = useId();
  const term = typeof children === "string" ? children : "term";
  return (
    <span className="tt">
      <span
        className="tt-t"
        tabIndex={0}
        role="button"
        aria-describedby={id}
        data-track={`tooltip:${term.toLowerCase().replace(/[^a-z0-9]+/g, "-")}`}
      >
        {children}
      </span>
      <span className="tt-b" role="tooltip" id={id}>
        {tip}
      </span>
    </span>
  );
}

// ---------- headings with a shareable anchor ----------
export function Heading({ level, id, children }: { level: 2 | 3; id: string; children: ReactNode }) {
  const H = level === 2 ? "h2" : "h3";
  return (
    <H id={id} className="hx">
      {children}
      <a className="ha" href={`#${id}`} aria-label="Link to this section">
        #
      </a>
    </H>
  );
}

// ---------- collapsibles ----------
export function Aside({ summary, track, children }: { summary: ReactNode; track: string; children: ReactNode }) {
  return (
    <details className="aside" data-track={track}>
      <summary>
        <span className="chev">{Icon.chev}</span>
        <span className="q">{summary}</span>
        <span className="hint" aria-hidden="true" />
      </summary>
      <div className="db">{children}</div>
    </details>
  );
}

// ---------- copy cards (interactions.ts does the copying) ----------
function CopyButton({ label, track }: { label: string; track: string }) {
  return (
    <button type="button" className="copy" aria-label={label} data-track={track}>
      {Icon.copy}
      {Icon.check}
      <span className="cl">Copy</span>
    </button>
  );
}

export function PromptCard({ text, children }: { text: string; children: ReactNode }) {
  return (
    <div className="paste" role="group" aria-label="Prompt to paste">
      <div className="ph">
        <span className="ptw">
          <span className="pt">Prompt to paste</span>
          <span className="psub">into your own agent</span>
        </span>
        <CopyButton label="Copy this prompt" track="copy-prompt" />
      </div>
      <div className="pb" data-copy={text}>
        {children}
      </div>
      <span className="sr" aria-live="polite" />
    </div>
  );
}

export const INSTALL_COMMAND = "curl -fsSL https://alignment-hive.com/install.sh | bash";

export function InstallCommand() {
  return (
    <div className="paste cmd" role="group" aria-label="Install command">
      <div className="ph">
        <code className="pb" data-copy={INSTALL_COMMAND}>
          {INSTALL_COMMAND}
        </code>
        <CopyButton label="Copy this command" track="copy-install" />
      </div>
      <span className="sr" aria-live="polite" />
    </div>
  );
}

// ---------- threads of verbatim messages ----------
export type Message = {
  kind: string; // me | agent | recall | comment | none
  role: "avoid" | "better" | null;
  speaker: string;
  when: string;
  tags: string[];
  html: string; // the verbatim body, rendered as HTML so the text stays exact
};

const MONO = new Set(["me", "comment", "none"]);
const RECALL_HEAD = "Recalled from earlier sessions";
const isElapsed = (when: string) => /\blater$/.test(when);

// `seam`: the better alternative right after a prompt to avoid. It isn't a later message, so it
// gets no elapsed-time chip.
function MessageView({ m, first, seam }: { m: Message; first: boolean; seam: boolean }) {
  const recall = m.kind === "recall";
  const cls = ["m", `m-${m.kind}`, MONO.has(m.kind) ? "mono" : "sans"];
  if (m.role) cls.push(`m-${m.role}`);
  let when = m.when;
  let gap: ReactNode = null;
  if (!seam && !first && !recall && isElapsed(when)) {
    gap = (
      <div className="gap">
        <span>
          {Icon.clock}
          {when}
        </span>
      </div>
    );
    when = "";
    cls.push("has-gap");
  }
  const icon = !m.role ? (Icon as Record<string, ReactNode>)[m.kind] : null;
  return (
    <div className={cls.join(" ")}>
      {gap}
      <div className="mh">
        {m.role && (
          <span className={`role role-${m.role}`}>
            <span className="badge">{m.role === "avoid" ? Icon.cross : Icon.check}</span>
            {m.role === "avoid" ? "Avoid" : "Better"}
          </span>
        )}
        {m.speaker && (
          <span className="sp">
            {icon}
            {recall ? RECALL_HEAD : m.speaker}
          </span>
        )}
        {when && <span className="when">{when}</span>}
        {m.tags.map((t) =>
          t.length <= 24 ? (
            <span key={t} className="tag">{t}</span>
          ) : (
            <span key={t} className="lnote">{t}</span>
          ),
        )}
      </div>
      <div className="mb" dangerouslySetInnerHTML={{ __html: m.html }} />
    </div>
  );
}

export function Thread({ messages }: { messages: Message[] }) {
  const pair = messages.some((m, k) => k > 0 && messages[k - 1].role === "avoid" && m.role === "better");
  const cls = ["thread"];
  if (pair) cls.push("pair");
  const label = pair ? "A prompt to avoid, then a better one" : "Conversation excerpt";
  return (
    <div className={cls.join(" ")} role="group" aria-label={label}>
      {messages.map((m, k) => (
        <MessageView key={k} m={m} first={k === 0} seam={k > 0 && m.role === "better" && messages[k - 1].role === "avoid"} />
      ))}
    </div>
  );
}
