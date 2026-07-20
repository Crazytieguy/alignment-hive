import { createFileRoute, useSearch } from "@tanstack/react-router";
import { getAuth, getSignInUrl } from "@workos/authkit-tanstack-react-start";
import { z } from "zod";
import { Button } from "@alignment-hive/ui";
import { KintsugiHero } from "@/components/landing/kintsugi-hero";
import type { CSSProperties, ReactNode } from "react";
// Hero display font, self-hosted via fontsource (subsets beyond latin are
// declared with unicode-range and never downloaded for this page's copy).
import "@fontsource-variable/fraunces/opsz.css";

const searchSchema = z.object({
  error: z.string().optional().catch(undefined),
});

export const Route = createFileRoute("/")({
  component: Home,
  validateSearch: searchSchema,
  loader: async () => {
    const { user } = await getAuth();
    const signInUrl = await getSignInUrl();

    return { user: user ? { id: user.id } : null, signInUrl };
  },
});

const GITHUB_URL = "https://github.com/Crazytieguy/alignment-hive";
const CODEX_REPO_URL = "https://github.com/Crazytieguy/codex-plugin-cc";
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
const sectionHeading =
  "mb-7 text-[0.82rem] font-semibold uppercase tracking-[0.14em] text-muted-foreground md:mb-0 md:mt-1.5";
const inlineLink =
  "text-primary underline decoration-1 underline-offset-2 hover:opacity-80";
// the landing page is dark-locked (.theme-dark on the root), so accents use
// the dark-theme amber values unconditionally
const hexAmber = "text-[#C68F2A]";

// Hero H1 display treatment: Fraunces, tuned for this size (serif displays
// need less negative tracking than the sans).
const heroFontStyle: CSSProperties = {
  fontFamily: '"Fraunces Variable", Georgia, serif',
  fontWeight: 520,
  letterSpacing: "-0.006em",
  // keep the display cut classical: no softening, no wonky glyphs
  fontVariationSettings: '"SOFT" 0, "WONK" 0',
};

// ---------------------------------------------------------------------------
// Shared copy (single source for all layout variants — do not fork strings)
// ---------------------------------------------------------------------------

const TESTIMONIALS = [
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
] as const;

const WORKSHOPS_COPY =
  "Hands-on workshops for AI safety orgs and fellowships: Claude Code setup and best practices, agentic research workflows, and figuring out together what better tooling can do for your work. For a workshop, consulting, or a collaboration, email me.";

const TOOLS_INTRO =
  "Open source and shaped by concrete bottlenecks in real research work. Here are some of the most-used tools in the collection:";

const TOOLS = [
  {
    name: "Hive",
    description:
      "Alignment Hive entry point and memory for Claude Code: instead of relying on model-written memories, Claude searches your past sessions directly. Also home to the community's opt-in knowledge and data aggregation.",
  },
  {
    name: "Codex for Claude Code",
    href: CODEX_REPO_URL,
    description:
      "Codex reviews and task delegation without leaving Claude Code. Claude understands fuzzy constraints better; GPT is more careful and catches mistakes; together they're more than the sum of their parts.",
  },
  {
    name: "Remote Kernels",
    description:
      "Unblock Claude to run experiments for you autonomously by letting it operate cloud compute efficiently and reliably.",
  },
] as const;

const USED_BY =
  "Used by dozens of researchers across MATS, Far Labs, and other AI safety organizations.";

function ToolName({ name, href }: { name: string; href?: string }) {
  return href ? (
    <a
      href={href}
      className="underline decoration-border underline-offset-4 hover:decoration-current"
    >
      {name}
    </a>
  ) : (
    <>{name}</>
  );
}

function AboutCopy() {
  return (
    <>
      Alignment Hive is built by Yoav Tzfati: MATS alum (scalable oversight
      with NYU's Alignment Research Group, then the{" "}
      <a href="https://sl5.org/" className={inlineLink}>
        Security Level 5 task force
      </a>
      ). Previously built the infrastructure for the{" "}
      <a href="https://www.trading.camp/" className={inlineLink}>
        Arbor trading bootcamp
      </a>{" "}
      and helped teach it, and taught Claude Code at{" "}
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
  const { user, signInUrl } = Route.useLoaderData();

  return (
    <div className="theme-dark relative isolate min-h-screen bg-background text-foreground">
      <header className={wrap}>
        <nav className="flex items-center justify-between gap-4 py-6">
          <a
            href="/"
            className="inline-flex items-center gap-2 text-[1.05rem] font-semibold tracking-[0.01em]"
          >
            {/* the favicon mark (public/favicon.svg): "Vertex Star" — one
                comb cell, its upper corner catching moonlight; keep in sync */}
            <svg
              width="21"
              height="21"
              viewBox="0 0 64 64"
              aria-hidden="true"
              className="-mt-px shrink-0"
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
          <span className="flex items-center gap-6">
            <a
              href={GITHUB_URL}
              className="text-[0.95rem] text-primary hover:underline"
            >
              GitHub
            </a>
            {user ? (
              <a
                href="/consent"
                className="text-[0.95rem] text-muted-foreground hover:underline"
              >
                Preferences
              </a>
            ) : (
              <a
                href={signInUrl}
                className="text-[0.95rem] text-muted-foreground hover:underline"
              >
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
            As soft takeoff picks up, Alignment Hive empowers the third-party
            alignment research community to keep up through curated tools,
            training, and aggregated knowledge and data: the benefits of scale
            that frontier AI labs have.
          </p>
        </KintsugiHero>

        <LandingSections />
      </main>

      <div className={wrap}>
        <footer className="flex flex-wrap gap-7 border-t border-border py-7 text-sm">
          <a href={`mailto:${EMAIL}`} className="text-primary hover:underline">
            {EMAIL}
          </a>
          <a href={GITHUB_URL} className="text-primary hover:underline">
            GitHub
          </a>
        </footer>
      </div>

      <ErrorBanner />

    </div>
  );
}

// ---------------------------------------------------------------------------
// Page sections — label rail
// ---------------------------------------------------------------------------

function LandingSections() {
  return (
    <>
      {/* data-star-section: the starfield reads these rects for its
          per-section density plateaus and cell-size profile (see
          kintsugi-hero.tsx). The testimonial section additionally skews the
          field denser/warmer — no solid background. */}
      <section
        data-star-section="testimonials"
        className={`${wrap} ${sectionPad}`}
      >
        <div className="grid grid-cols-1 gap-[clamp(2rem,5vw,4rem)] md:grid-cols-2">
          {TESTIMONIALS.map((t) => (
            <Testimonial key={t.attribution} attribution={t.attribution}>
              {t.quote}
            </Testimonial>
          ))}
        </div>
      </section>

      <section
        data-star-section="workshops"
        className={`${wrap} ${sectionPad} ${railGrid}`}
        aria-labelledby="work-h"
      >
        <h2 id="work-h" className={sectionHeading}>
          Workshops &amp; consulting
        </h2>
        <div>
          <p className="max-w-[38rem] leading-relaxed">{WORKSHOPS_COPY}</p>
          <Button asChild size="lg" className="mt-6">
            <a href={`mailto:${EMAIL}`}>Email me</a>
          </Button>
        </div>
      </section>

      <section
        data-star-section="tools"
        className={`${wrap} ${sectionPad}`}
        aria-labelledby="tools-h"
      >
        <div className={railGrid}>
          <h2 id="tools-h" className={sectionHeading}>
            Tools
          </h2>
          <p className="mb-6 max-w-[38rem] leading-relaxed">{TOOLS_INTRO}</p>
        </div>
        {TOOLS.map((t, i) => (
          <Tool
            key={t.name}
            name={t.name}
            href={"href" in t ? t.href : undefined}
            last={i === TOOLS.length - 1}
          >
            {t.description}
          </Tool>
        ))}
        <div className={railGrid}>
          <div className="hidden md:block" />
          <div>
            <p className="mt-6 max-w-[38rem] text-sm text-muted-foreground">
              {USED_BY}
            </p>
            <p className="mt-6 text-[0.95rem]">
              <a href={GITHUB_URL} className={inlineLink}>
                These and more on GitHub
              </a>
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
        <h2 id="about-h" className={sectionHeading}>
          About
        </h2>
        <p className="max-w-[38rem] leading-relaxed">
          <AboutCopy />
        </p>
      </section>
    </>
  );
}

function Testimonial({
  attribution,
  children,
}: {
  attribution: string;
  children: ReactNode;
}) {
  return (
    <blockquote className="relative pt-6 [text-wrap:pretty]">
      <span
        aria-hidden="true"
        className={`absolute -top-2 -left-0.5 text-[3.2rem] leading-none ${hexAmber}`}
      >
        &ldquo;
      </span>
      <p className="italic leading-relaxed">{children}</p>
      <footer className="mt-3 text-[0.85rem] tracking-[0.04em] text-muted-foreground">
        &mdash; {attribution}
      </footer>
    </blockquote>
  );
}

function Tool({
  name,
  href,
  last,
  children,
}: {
  name: string;
  href?: string;
  last?: boolean;
  children: ReactNode;
}) {
  return (
    <div
      className={`${railGrid} border-t border-border py-5 ${
        last ? "border-b" : ""
      }`}
    >
      <h3 className="text-base font-semibold md:mt-0.5">
        <ToolName name={name} href={href} />
      </h3>
      <p className="mt-1 max-w-[38rem] leading-relaxed md:mt-0">{children}</p>
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

