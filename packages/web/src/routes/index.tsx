// Public landing page (/). The kintsugi starfield behind it reads each section's
// data-star-section rect, so section order and markers here shape the field.
import { createFileRoute, useSearch } from "@tanstack/react-router";
import { getAuth } from "@workos/authkit-tanstack-react-start";
import { z } from "zod";
import { Button } from "@alignment-hive/ui";
import { ArrowRight, Check, Copy } from "lucide-react";
import { KintsugiHero } from "@/components/landing/kintsugi-hero";
import { TestimonialCarousel } from "@/components/landing/testimonial-carousel";
import { useEffect, useRef, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
// Hero display font: Fraunces at the H1's weight, Latin-1 glyphs only
// (regenerate with scripts/subset-hero-font.sh).
import frauncesHeroUrl from "@/assets/fraunces-hero.woff2?url";

const searchSchema = z.object({
  error: z.string().optional().catch(undefined),
});

// Declared here rather than through fontsource's stylesheet so the face can
// be preloaded and use font-display: optional: the H1 is then either
// Fraunces at first paint or a plain fallback for this page view — never
// fontsource's default swap from Georgia mid-read.
const frauncesFace = `@font-face{font-family:"Fraunces Hero";font-style:normal;font-weight:520;font-display:optional;src:url(${frauncesHeroUrl}) format("woff2-variations")}`;

export const Route = createFileRoute("/")({
  component: Home,
  validateSearch: searchSchema,
  head: () => ({
    links: [
      {
        rel: "preload",
        as: "font",
        type: "font/woff2",
        href: frauncesHeroUrl,
        crossOrigin: "anonymous",
      },
    ],
    styles: [{ children: frauncesFace }],
  }),
  loader: async () => {
    const { user } = await getAuth();

    return { user: user ? { id: user.id } : null };
  },
});

const GITHUB_URL = "https://github.com/Crazytieguy/alignment-hive";
const EMAIL = "yoav.tzfati@gmail.com";

/** Shared horizontal rhythm for every band of the page. */
const wrap = "mx-auto max-w-[66rem] px-[clamp(1.25rem,5vw,3rem)]";
const sectionPad = "py-[clamp(2.75rem,6vw,4rem)]";
/**
 * Label rail: on md+ every section places its small-caps label (and each
 * tool's name) in a shared left column, with prose in a shared content
 * column — one consistent spine instead of stacked left-hugging blocks.
 */
const railGrid =
  "md:grid md:grid-cols-[11rem_minmax(0,1fr)] md:gap-x-10 lg:grid-cols-[13rem_minmax(0,1fr)] lg:gap-x-14";
// Section headings in the hero's Fraunces, normal case, so the page reads in
// one editorial voice from the H1 down; amber is left to links and actions.
const sectionHeading =
  "mb-4 text-[1.375rem] leading-[1.25] text-foreground [text-wrap:balance] md:mb-0 md:text-[1.5rem] md:leading-[1.2]";
const sectionHeadingStyle: CSSProperties = {
  fontFamily: '"Fraunces Hero", Georgia, serif',
  fontWeight: 520,
};
const inlineLink =
  "text-primary underline decoration-primary/50 decoration-1 underline-offset-[3px] transition-colors hover:decoration-primary";
/** Body prose: one measure and size for every section's running text. */
const prose = "max-w-[38rem] leading-relaxed md:text-[1.0625rem]";
// Padding gives each link a ~44px tap target; the links' wrapper has a
// matching negative right margin, so the last link's text ends at the page edge.
const navLink =
  "px-1.5 py-2.5 text-[0.95rem] text-foreground/80 transition-colors hover:text-primary sm:px-2";
// Hero H1 display treatment: Fraunces, tuned for this size (serif displays
// need less negative tracking than the sans).
const heroFontStyle: CSSProperties = {
  fontFamily: '"Fraunces Hero", Georgia, serif',
  fontWeight: 520,
  letterSpacing: "-0.006em",
};

// Shown one at a time, in this order.
const TESTIMONIALS = [
  {
    attribution: "MATS fellow",
    quote:
      "a step function change in my research productivity. good to get 1) a productivity reflection and perspective with an expert like Yoav who's also familiar with eng research workflow 2) up-to-date on the latest agentic features",
  },
  {
    attribution: "MATS fellow",
    quote:
      "Fully using all the features while trying to hail Mary two first-author papers into COLM, and I'm just wondering how I ever let myself go without it. Alignment Hive is so good, thank you so much, Yoav",
  },
  {
    attribution: "researcher at Far Labs",
    quote:
      "Thanks for the Claude Code setup boost. It's so much fun talking and sharing workflows, especially when the advice comes from someone who has put in enough hours and not just hyping things up like influencers.",
  },
  {
    attribution: "MATS fellow",
    quote:
      "I very strongly recommend attending a session with Yoav. My workflow has leveled up massively since our session -- I just wish I had signed up sooner!",
  },
  {
    attribution: "MATS fellow",
    quote:
      "Yoav was uniquely helpful at MATS. He helped me make real technical headway in just a couple meetings, and ultimately led me to reconsider the mechanics underlying my core project.",
  },
] as const;

const WORKSHOPS_COPY =
  "Talks, workshops, and consulting for AI safety fellowships and research orgs: the state of the art in reliable agent delegation, the tools and techniques to get there, and personalized hands-on advice.";

const TOOLS_INTRO =
  "Open source and shaped by concrete bottlenecks in real research work. Here are some of the most-used tools in the collection. Get started:";

const TOOLS: { name: string; description: string }[] = [
  {
    name: "Hive",
    description:
      "The Alignment Hive entry point and memory for Claude Code: instead of relying on model-written memories, Claude searches your past sessions for relevant context as it works.",
  },
  {
    name: "Model Router",
    description:
      "GPT and other non-Claude models as native subagents in Claude Code.",
  },
  {
    name: "Debrief",
    description:
      "Reports on finished agent work, written for you to review and comment on.",
  },
  {
    name: "Remote Kernels",
    description:
      "Lets Claude run experiments autonomously on cloud compute, efficiently and reliably.",
  },
  {
    name: "tldr",
    description:
      "Collapses each long agent reply into a one-sentence summary you can expand.",
  },
];

const USED_BY =
  "Used by researchers across MATS, Far Labs, Iliad, and other AI safety organizations.";

const INSTALL_COMMAND =
  "curl -fsSL https://alignment-hive.com/install.sh | bash";

function InstallCommand() {
  const [copied, setCopied] = useState(false);
  // Where the command is cut off (narrow screens), fade its clipped edge(s)
  // so it reads as scrollable rather than truncated.
  const [fade, setFade] = useState<"" | "end" | "both" | "start">("");
  const codeRef = useRef<HTMLElement>(null);
  useEffect(() => {
    const el = codeRef.current;
    if (!el) return;
    const sync = (): void => {
      const max = el.scrollWidth - el.clientWidth;
      if (max <= 1) return setFade("");
      const atStart = el.scrollLeft <= 1;
      const atEnd = el.scrollLeft >= max - 1;
      setFade(atStart ? "end" : atEnd ? "start" : "both");
    };
    sync();
    el.addEventListener("scroll", sync, { passive: true });
    const ro = new ResizeObserver(sync);
    ro.observe(el);
    return () => {
      el.removeEventListener("scroll", sync);
      ro.disconnect();
    };
  }, []);
  return (
    <div className="mt-5 flex items-center gap-3 rounded-lg border border-border bg-card py-2 pl-4 pr-2 shadow-[inset_0_1px_0_rgb(255_255_255/0.03)]">
      <code
        ref={codeRef}
        data-fade={fade || undefined}
        className="install-code min-w-0 flex-1 overflow-x-auto whitespace-nowrap py-1 font-mono text-[0.8125rem] sm:text-sm"
      >
        <span
          aria-hidden="true"
          className="mr-2 select-none text-muted-foreground"
        >
          $
        </span>
        {INSTALL_COMMAND}
      </code>
      <Button
        size="sm"
        variant="outline"
        className="w-[5.5rem]"
        aria-label={copied ? "Copied" : "Copy install command"}
        onClick={() => {
          void navigator.clipboard.writeText(INSTALL_COMMAND).then(() => {
            setCopied(true);
            setTimeout(() => setCopied(false), 2000);
          });
        }}
      >
        {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
        {copied ? "Copied" : "Copy"}
      </Button>
    </div>
  );
}

/** A standalone "go somewhere" link: amber, with an arrow that nudges on hover. */
function ArrowLink({ href, children }: { href: string; children: ReactNode }) {
  return (
    <a
      href={href}
      className="group -my-1 inline-flex items-center gap-1.5 py-1 text-[0.95rem] font-medium text-primary underline-offset-4 hover:underline"
    >
      {children}
      <ArrowRight
        aria-hidden="true"
        className="size-4 transition-transform group-hover:translate-x-0.5 motion-reduce:transition-none"
      />
    </a>
  );
}

function AboutCopy() {
  return (
    <>
      Alignment Hive is built by Yoav Tzfati, a MATS alum (scalable oversight
      with NYU's Alignment Research Group, then the{" "}
      <a href="https://sl5.org/" className={inlineLink}>
        Security Level 5 task force
      </a>
      ). He previously built the infrastructure for the{" "}
      <a href="https://www.trading.camp/" className={inlineLink}>
        Arbor trading bootcamp
      </a>
      , helped teach it, and taught Claude Code at{" "}
      <a
        href="https://www.arborsummer.camp/branches/vibecoding"
        className={inlineLink}
      >
        Code Bloom
      </a>{" "}
      just four months after its public release.
    </>
  );
}

function Home() {
  const { user } = Route.useLoaderData();

  return (
    <div className="theme-dark landing-root relative isolate min-h-screen bg-background text-foreground">
      <header className="landing-nav-veil">
        <nav
          className={`${wrap} flex flex-wrap items-center justify-between gap-x-4 gap-y-1 py-[0.9375rem]`}
        >
          <a
            href="/"
            className="inline-flex items-baseline gap-2 text-[1.05rem] font-semibold tracking-[0.01em] transition-opacity hover:opacity-85"
          >
            {/* the favicon mark (public/favicon.svg): "Vertex Star" — one
              comb cell, its upper corner catching moonlight; keep in sync.
              The viewBox crops to the hexagon's painted bounds (stroke
              half-width 2.2 beyond the vertices) and the box is sized to
              the cap height and baseline-aligned, so the hexagon spans
              exactly the "A" while the star overflows to peek above it */}
            <svg
              viewBox="6.8 16.48 44.4 39.04"
              aria-hidden="true"
              className="h-[0.6656em] w-[0.757em] shrink-0 overflow-visible"
            >
              <defs>
                <linearGradient id="ah-cell" x1="0" y1="1" x2="0.35" y2="0">
                  <stop offset="0" stopColor="#D89B30" />
                  <stop offset="1" stopColor="#F0B44E" />
                </linearGradient>
              </defs>
              <path
                fill="none"
                stroke="url(#ah-cell)"
                strokeWidth="4.4"
                strokeLinecap="round"
                strokeLinejoin="round"
                d="M44.5 28.2 L49 36 L39 53.32 L19 53.32 L9 36 L19 18.68 L25.5 18.68"
              />
              <path
                fill="#BFD2E4"
                d="M39 7.18 Q41.1 16.6 50.5 18.68 Q41.1 20.8 39 30.18 Q36.9 20.8 27.5 18.68 Q36.9 16.6 39 7.18 Z"
              />
            </svg>
            Alignment Hive
          </a>
          <span className="-mr-1.5 flex items-center sm:-mr-2 sm:gap-3">
            <a href="/agent-advice" className={navLink}>
              Agent advice
            </a>
            <a href={GITHUB_URL} className={navLink}>
              GitHub
            </a>
            {user ? (
              <a href="/consent" className={navLink}>
                Preferences
              </a>
            ) : (
              <a href="/auth/sign-in" className={navLink}>
                Log in
              </a>
            )}
          </span>
        </nav>
      </header>

      <main>
        <KintsugiHero>
          <h1 className="kintsugi-h" style={heroFontStyle}>
            Ensuring AI alignment research keeps pace.
          </h1>
          <p className="kintsugi-s">
            As soft takeoff picks up, Alignment Hive helps the third-party
            alignment research community keep up through curated tools,
            training, and aggregated knowledge and data: the benefits of scale
            that frontier AI labs have.
          </p>
        </KintsugiHero>

        <LandingSections />
      </main>

      <div className={wrap}>
        <footer className="flex flex-wrap gap-x-7 gap-y-2 border-t border-border pb-10 pt-7 text-sm">
          <a
            href={`mailto:${EMAIL}`}
            className="-m-2 p-2 text-muted-foreground transition-colors hover:text-primary"
          >
            {EMAIL}
          </a>
          <a
            href={GITHUB_URL}
            className="-m-2 p-2 text-muted-foreground transition-colors hover:text-primary"
          >
            GitHub
          </a>
        </footer>
      </div>

      <ErrorBanner />
    </div>
  );
}

function LandingSections() {
  return (
    <>
      {/* data-star-section: the starfield reads these rects for its
          per-section density plateaus and cell-size profile (see
          kintsugi-hero.tsx and kintsugi-field.ts). The testimonial section additionally skews the
          field denser/warmer — no solid background. */}
      <section
        data-star-section="testimonials"
        className={`${wrap} ${sectionPad}`}
      >
        <TestimonialCarousel items={TESTIMONIALS} />
      </section>

      <section
        data-star-section="workshops"
        className={`${wrap} ${sectionPad} ${railGrid}`}
        aria-labelledby="work-h"
      >
        <h2 id="work-h" className={sectionHeading} style={sectionHeadingStyle}>
          Talks, workshops &amp; consulting
        </h2>
        <div>
          <p className={prose}>{WORKSHOPS_COPY}</p>
          <div className="mt-7 flex flex-wrap items-center gap-x-6 gap-y-4">
            {/* hover brightens rather than the default 90% opacity, which
                on this dark ground reads as the button dimming */}
            <Button
              asChild
              size="lg"
              className="h-11 px-7 text-[0.95rem] shadow-[inset_0_1px_0_rgb(255_255_255/0.25)] hover:bg-primary hover:brightness-110 active:brightness-95"
            >
              <a href={`mailto:${EMAIL}`}>Email me</a>
            </Button>
            <ArrowLink href="/agent-advice">Read my advice</ArrowLink>
          </div>
        </div>
      </section>

      <section
        data-star-section="tools"
        className={`${wrap} ${sectionPad}`}
        aria-labelledby="tools-h"
      >
        <div className={railGrid}>
          <h2
            id="tools-h"
            className={sectionHeading}
            style={sectionHeadingStyle}
          >
            Tools
          </h2>
          <div className="mb-8 max-w-[38rem]">
            <p className={prose}>{TOOLS_INTRO}</p>
            <InstallCommand />
          </div>
        </div>
        {TOOLS.map((t, i) => (
          <Tool
            key={t.name}
            name={t.name}
            last={i === TOOLS.length - 1}
          >
            {t.description}
          </Tool>
        ))}
        <div className={railGrid}>
          <div className="hidden md:block" />
          <div>
            <p className="mt-6 max-w-[38rem] text-sm text-muted-foreground md:text-[0.9375rem]">
              {USED_BY}
            </p>
            <p className="mt-5">
              <ArrowLink href={GITHUB_URL}>These and more on GitHub</ArrowLink>
            </p>
          </div>
        </div>
      </section>

      {/* footer counts as "about" for the starfield density map */}
      <section
        data-star-section="about"
        className={`${wrap} ${sectionPad} ${railGrid}`}
        aria-labelledby="about-h"
      >
        <h2 id="about-h" className={sectionHeading} style={sectionHeadingStyle}>
          About
        </h2>
        <p className={prose}>
          <AboutCopy />
        </p>
      </section>
    </>
  );
}

function Tool({
  name,
  last,
  children,
}: {
  name: string;
  last?: boolean;
  children: ReactNode;
}) {
  return (
    <div
      className={`${railGrid} border-t border-border py-5 ${
        last ? "border-b" : ""
      }`}
    >
      <h3 className="font-semibold md:text-[1.0625rem] md:leading-relaxed">
        {name}
      </h3>
      <p className={`mt-1 md:mt-0 ${prose}`}>{children}</p>
    </div>
  );
}

function ErrorBanner() {
  const { error } = useSearch({ from: "/" });

  if (!error) return null;

  const message =
    error === "auth_failed"
      ? "Authentication failed. Please try again."
      : "Something went wrong.";

  return (
    <div className="fixed bottom-4 right-4 max-w-sm p-4 bg-red-950/40 border border-red-800 rounded-lg shadow-lg">
      <p className="text-sm text-red-200">{message}</p>
    </div>
  );
}
