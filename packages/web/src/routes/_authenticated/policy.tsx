import { createFileRoute, Link } from "@tanstack/react-router";
import { Authenticated, AuthLoading } from "convex/react";
import { useQuery } from "convex/react";
import { api } from "../../../convex/_generated/api";
import {
  policySections,
  policyFooter,
} from "@/components/consent/policy-content";
import { PolicyParagraph } from "@/components/consent/policy-paragraph";

export const Route = createFileRoute("/_authenticated/policy")({
  component: () => (
    <>
      <AuthLoading>
        <div className="min-h-screen flex items-center justify-center">
          <p className="text-muted-foreground">Loading...</p>
        </div>
      </AuthLoading>
      <Authenticated>
        <PolicyPage />
      </Authenticated>
    </>
  ),
});

function PolicyPage() {
  const accessList = useQuery(api.consent.getAccessList);

  return (
    <div className="min-h-screen flex flex-col items-center pt-16 pb-24 px-4">
      <div className="w-full max-w-xl">
        <Link
          to="/consent"
          className="text-sm text-primary underline underline-offset-4 hover:text-primary/80 transition-colors mb-8 inline-flex items-center gap-1"
        >
          <svg
            width="16"
            height="16"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.5"
          >
            <path d="M10 4l-4 4 4 4" />
          </svg>
          Back to preferences
        </Link>

        <h1 className="text-3xl font-semibold mb-10 tracking-tight">
          Data sharing policy
        </h1>

        <div className="space-y-8">
          {policySections.map((section) => (
            <div key={section.id}>
              {section.title && (
                <h2 className="text-lg font-semibold mb-3 tracking-tight">
                  {section.title}
                </h2>
              )}
              <div className="space-y-3 text-[0.938rem] leading-relaxed text-foreground/90">
                {section.paragraphs.map((p, i) => (
                  <PolicyParagraph key={i} text={p} />
                ))}
              </div>

              {section.id === "access" && accessList && accessList.length > 0 && (
                <div className="mt-4 rounded-lg border px-5 py-4">
                  <p className="text-sm font-medium mb-3">
                    People with access to shared data
                  </p>
                  <ul className="space-y-1.5">
                    {accessList.map((person, i) => (
                      <li
                        key={i}
                        className="text-sm text-muted-foreground flex items-baseline gap-2"
                      >
                        <span className="size-1.5 rounded-full bg-primary/40 shrink-0 mt-1.5" />
                        {person.name ? (
                          <span>
                            {person.name}{" "}
                            <span className="text-muted-foreground/60">
                              ({person.email})
                            </span>
                          </span>
                        ) : (
                          <span>{person.email}</span>
                        )}
                      </li>
                    ))}
                  </ul>
                </div>
              )}
            </div>
          ))}
        </div>

        <p className="mt-10 text-sm text-muted-foreground">{policyFooter}</p>

        <div className="mt-8 pt-6 border-t">
          <Link
            to="/consent"
            className="text-sm text-primary underline underline-offset-4 hover:text-primary/80 transition-colors inline-flex items-center gap-1"
          >
            <svg
              width="16"
              height="16"
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
            >
              <path d="M10 4l-4 4 4 4" />
            </svg>
            Back to preferences
          </Link>
        </div>
      </div>
    </div>
  );
}

