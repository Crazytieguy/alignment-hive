import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useAction } from "convex/react";
import { useSuspenseQuery } from "@tanstack/react-query";
import { convexQuery } from "@convex-dev/react-query";
import { useState } from "react";
import { api } from "../../../convex/_generated/api";
import {
  agreementSections,
  agreementFooter,
  CURRENT_AGREEMENT_VERSION,
} from "../../../convex/lib/agreement";
import { PolicyParagraph } from "@/components/consent/policy-paragraph";
import { Button } from "@alignment-hive/ui";

export const Route = createFileRoute("/authorized/agreement")({
  loader: async ({ context }) => {
    await context.queryClient.ensureQueryData(
      convexQuery(api.agreement.getAgreementStatus, {}),
    );
  },
  component: AgreementPage,
});

function AgreementPage() {
  const { data } = useSuspenseQuery(
    convexQuery(api.agreement.getAgreementStatus, {}),
  );
  const submitAgreement = useAction(api.agreement.submitAgreement);
  const navigate = useNavigate();
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleAgree = async () => {
    setSubmitting(true);
    setError(null);
    try {
      await submitAgreement();
      navigate({ to: "/authorized" });
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to submit agreement");
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="max-w-2xl mx-auto">
      <h1 className="text-2xl font-semibold mb-2 tracking-tight">
        Data Accessor Agreement
      </h1>
      <p className="text-sm text-muted-foreground mb-8">
        Version: {CURRENT_AGREEMENT_VERSION}
      </p>

      {data.agreement && (
        <div className="mb-8 rounded-md border border-border bg-muted/50 px-4 py-3 text-sm">
          You agreed to this version on{" "}
          {new Date(data.agreement.agreedAt).toLocaleDateString("en-US", {
            year: "numeric",
            month: "long",
            day: "numeric",
          })}
          .
        </div>
      )}

      <div className="space-y-8">
        {agreementSections.map((section) => (
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
          </div>
        ))}
      </div>

      <p className="mt-10 text-sm text-muted-foreground">{agreementFooter}</p>

      {!data.agreement && (
        <div className="mt-8 pt-6 border-t space-y-3">
          {error && (
            <p className="text-sm text-destructive">{error}</p>
          )}
          <Button onClick={handleAgree} disabled={submitting}>
            {submitting ? "Submitting..." : "I agree"}
          </Button>
        </div>
      )}
    </div>
  );
}
