// "The Coding Agent Advice I Keep Repeating": the post's page. The text is in components/agent-advice/post.tsx.
import { createFileRoute } from "@tanstack/react-router";
import { useEffect, useRef } from "react";
import aaCssUrl from "@/components/agent-advice/styles.css?url";
import { initArticleInteractions } from "@/components/agent-advice/interactions";
import { PostBody } from "@/components/agent-advice/post";
import { initArticleAnalytics } from "@/lib/article-analytics";

const URL = "https://www.alignment-hive.com/agent-advice";
const TITLE = "The Coding Agent Advice I Keep Repeating";
// No-break spaces keep the heading's lines on phrase boundaries: "The Coding Agent Advice / I Keep Repeating",
// or "The Coding / Agent Advice / I Keep Repeating" on the narrowest phones.
const HEADING = TITLE.replace("Agent Advice", "Agent Advice").replace("I Keep Repeating", "I Keep Repeating");
const DESC =
  "The advice I keep repeating to AI safety researchers who use coding agents: problems I've observed, and the best solutions I've found so far.";
const OG_IMAGE = "https://www.alignment-hive.com/og/agent-advice.png?v=2";
const OG_ALT =
  "The Coding Agent Advice I Keep Repeating: bottlenecks to effective research delegation, and my recommended fixes. The Claude, OpenAI and Gemini marks sit in three joined honeycomb cells.";
const GITHUB_URL = "https://github.com/Crazytieguy/alignment-hive";
const EMAIL = "yoav.tzfati@gmail.com";
const FONTS =
  "https://fonts.googleapis.com/css2?family=Fraunces:opsz,wght@9..144,500;9..144,600&family=JetBrains+Mono:wght@400;500&family=IBM+Plex+Mono:ital,wght@0,400;0,500;0,600;1,400&family=IBM+Plex+Sans:ital,wght@0,400;0,500;0,600;1,400&family=Newsreader:ital,opsz,wght@0,6..72,400;0,6..72,500;1,6..72,400&display=swap";

export const Route = createFileRoute("/agent-advice")({
  head: () => ({
    meta: [
      { title: `${TITLE} · Alignment Hive` },
      { name: "description", content: DESC },
      { property: "og:type", content: "article" },
      { property: "og:title", content: TITLE },
      { property: "og:description", content: DESC },
      { property: "og:url", content: URL },
      { property: "og:site_name", content: "Alignment Hive" },
      { property: "og:image", content: OG_IMAGE },
      { property: "og:image:alt", content: OG_ALT },
      { name: "twitter:image:alt", content: OG_ALT },
      { name: "twitter:card", content: "summary_large_image" },
      { name: "twitter:image", content: OG_IMAGE },
    ],
    links: [
      { rel: "canonical", href: URL },
      { rel: "preconnect", href: "https://fonts.googleapis.com" },
      { rel: "preconnect", href: "https://fonts.gstatic.com", crossOrigin: "anonymous" },
      { rel: "stylesheet", href: FONTS },
      { rel: "stylesheet", href: aaCssUrl },
    ],
  }),
  component: AgentAdvice,
});

function Mark() {
  // the site's favicon mark, as in the homepage nav
  return (
    <svg viewBox="6.8 16.48 44.4 39.04" aria-hidden="true" className="ah-mark">
      <defs>
        <linearGradient id="ah-cell-aa" x1="0" y1="1" x2="0.35" y2="0">
          <stop offset="0" stopColor="#D89B30" />
          <stop offset="1" stopColor="#F0B44E" />
        </linearGradient>
      </defs>
      <path
        fill="none"
        stroke="url(#ah-cell-aa)"
        strokeWidth="4.4"
        strokeLinecap="round"
        strokeLinejoin="round"
        d="M44.5 28.2 L49 36 L39 53.32 L19 53.32 L9 36 L19 18.68 L25.5 18.68"
      />
      <path fill="#BFD2E4" d="M39 7.18 Q41.1 16.6 50.5 18.68 Q41.1 20.8 39 30.18 Q36.9 20.8 27.5 18.68 Q36.9 16.6 39 7.18 Z" />
    </svg>
  );
}

function AgentAdvice() {
  const wrap = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const root = wrap.current;
    if (!root) return;
    const a = initArticleInteractions(root);
    // The whole page, so the nav and footer links are tracked too; its only h2/h3[id] are the post's.
    const b = initArticleAnalytics(root, "agent-advice");
    return () => {
      a();
      b();
    };
  }, []);

  return (
    <div className="aa">
      <div className="wrap" ref={wrap}>
        <nav className="ah-nav">
          <a href="/" className="ah-home">
            <Mark />
            <span>Alignment Hive</span>
          </a>
          <a href={GITHUB_URL} className="ah-link" data-track="link:github-nav">
            GitHub
          </a>
        </nav>
        <header className="top">
          <h1>{HEADING}</h1>
        </header>
        <section className="pane">
          <p className="by">
            Yoav Tzfati · Published November 25, 2026
          </p>
          <article>
            <PostBody email={EMAIL} />
          </article>
        </section>
        <footer className="ah-foot">
          <a href="/">Alignment Hive</a>
          <a href={GITHUB_URL} data-track="link:github-footer">
            GitHub
          </a>
        </footer>
      </div>
    </div>
  );
}
