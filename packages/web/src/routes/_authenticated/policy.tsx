import { createFileRoute, Link } from "@tanstack/react-router";
import { useSuspenseQuery } from "@tanstack/react-query";
import { convexQuery } from "@convex-dev/react-query";
import { api } from "../../../convex/_generated/api";
import {
  policySections,
  policyFooter,
} from "@/components/consent/policy-content";
import { PolicyParagraph } from "@/components/consent/policy-paragraph";
import { AccessList } from "@/components/consent/access-list";

export const Route = createFileRoute("/_authenticated/policy")({
  loader: async ({ context }) => {
    await context.queryClient.ensureQueryData(convexQuery(api.consent.getAccessList, {}));
  },
  component: PolicyPage,
});

function PolicyPage() {
  const { data: accessList } = useSuspenseQuery(convexQuery(api.consent.getAccessList, {}));

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

              {section.id === "access" && (
                <div className="mt-4">
                  <AccessList accessList={accessList} />
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

