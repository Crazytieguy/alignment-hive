import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { Authenticated, AuthLoading } from "convex/react";
import { useQuery, useMutation } from "convex/react";
import { useState } from "react";
import { api } from "../../../convex/_generated/api";
import type { ConsentQuestion } from "@/components/consent/policy-content";
import ConsentWizard from "@/components/consent/consent-wizard";
import ConsentSummary from "@/components/consent/consent-summary";

export const Route = createFileRoute("/_authenticated/consent")({
  component: ConsentGate,
});

export interface ConsentChoices {
  sessionSharing: boolean | null;
  communityFeatures: boolean | null;
  publicationExcerpts: boolean | null;
  creditByName: boolean | null;
}

function ConsentGate() {
  return (
    <>
      <AuthLoading>
        <div className="min-h-screen flex items-center justify-center">
          <p className="text-muted-foreground">Loading...</p>
        </div>
      </AuthLoading>
      <Authenticated>
        <ConsentPage />
      </Authenticated>
    </>
  );
}

function ConsentPage() {
  const navigate = useNavigate();
  const latestConsent = useQuery(api.consent.getLatestConsent);
  const accessList = useQuery(api.consent.getAccessList);
  const existingProjects = useQuery(api.consent.getUserSessionProjects);
  const submitConsentMutation = useMutation(api.consent.submitConsent);
  const enableProjectMutation = useMutation(api.consent.enableProject);

  const [choices, setChoices] = useState<ConsentChoices>({
    sessionSharing: null,
    communityFeatures: null,
    publicationExcerpts: null,
    creditByName: null,
  });
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  // Still loading
  if (
    latestConsent === undefined ||
    accessList === undefined ||
    existingProjects === undefined
  ) {
    return (
      <div className="min-h-screen flex items-center justify-center">
        <p className="text-muted-foreground">Loading...</p>
      </div>
    );
  }

  const isReturning = latestConsent !== null;

  const handleChoice = (question: ConsentQuestion, value: boolean) => {
    setChoices((prev) => ({ ...prev, [question]: value }));
  };

  const handleSubmit = async (selectedProjects?: Set<string>) => {
    if (choices.sessionSharing === null) return;

    setIsSubmitting(true);
    setSubmitError(null);
    try {
      if (choices.sessionSharing) {
        if (
          choices.communityFeatures === null ||
          choices.publicationExcerpts === null ||
          choices.creditByName === null
        ) {
          setIsSubmitting(false);
          return;
        }
        await submitConsentMutation({
          consent: {
            sessionSharing: true,
            communityFeatures: choices.communityFeatures,
            publicationExcerpts: choices.publicationExcerpts,
            creditByName: choices.creditByName,
          },
        });

        // Create project consent entries for selected existing projects
        if (selectedProjects) {
          const promises = [...selectedProjects].map((project) =>
            enableProjectMutation({ project }).catch(() => {}),
          );
          await Promise.all(promises);
        }
      } else {
        await submitConsentMutation({
          consent: { sessionSharing: false },
        });
      }

      // First-time users go to install page; returning users stay on consent
      if (isReturning) {
        // Force re-render with updated Convex data
        setIsSubmitting(false);
        setChoices({
          sessionSharing: null,
          communityFeatures: null,
          publicationExcerpts: null,
          creditByName: null,
        });
      } else {
        // Navigate before clearing isSubmitting to avoid flash of consent page
        navigate({ to: "/install" });
      }
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error);
      console.error("Failed to submit consent:", msg);
      setSubmitError(msg);
      setIsSubmitting(false);
    }
  };

  // Returning users see the summary view
  if (isReturning) {
    return (
      <ConsentSummary
        choices={choices}
        onChoice={handleChoice}
        onSubmit={() => handleSubmit()}
        isSubmitting={isSubmitting}
        submitError={submitError}
        accessList={accessList ?? []}
        existingConsent={
          latestConsent
            ? {
                sessionSharing: latestConsent.sessionSharing,
                ...(latestConsent.sessionSharing
                  ? {
                      communityFeatures: latestConsent.communityFeatures,
                      publicationExcerpts: latestConsent.publicationExcerpts,
                      creditByName: latestConsent.creditByName,
                    }
                  : {}),
              }
            : null
        }
      />
    );
  }

  // First-time users see the wizard
  return (
    <ConsentWizard
      choices={choices}
      onChoice={handleChoice}
      onSubmit={handleSubmit}
      isSubmitting={isSubmitting}
      submitError={submitError}
      accessList={accessList ?? []}
      existingProjects={existingProjects ?? []}
    />
  );
}
