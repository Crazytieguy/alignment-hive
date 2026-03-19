import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useMutation } from "convex/react";
import { useSuspenseQuery } from "@tanstack/react-query";
import { convexQuery } from "@convex-dev/react-query";
import { useLayoutEffect, useState } from "react";
import { api } from "../../../convex/_generated/api";
import type { ConsentQuestion } from "@/components/consent/policy-content";
import ConsentWizard, { WIZARD_STORAGE_KEY } from "@/components/consent/consent-wizard";
import ConsentSummary from "@/components/consent/consent-summary";
import { classifyLegacyProject } from "@alignment-hive/session-data";

export const Route = createFileRoute("/_authenticated/consent")({
  loader: async ({ context }) => {
    await Promise.all([
      context.queryClient.ensureQueryData(convexQuery(api.consent.getLatestConsent, {})),
      context.queryClient.ensureQueryData(convexQuery(api.consent.getAccessList, {})),
      context.queryClient.ensureQueryData(convexQuery(api.consent.getUserSessionProjects, {})),
    ]);
  },
  component: ConsentPage,
});

export interface ConsentChoices {
  sessionSharing: boolean | null;
  communityFeatures: boolean | null;
  publicationExcerpts: boolean | null;
  creditByName: boolean | null;
}

function ConsentPage() {
  const navigate = useNavigate();
  const { data: latestConsent } = useSuspenseQuery(convexQuery(api.consent.getLatestConsent, {}));
  const { data: accessList } = useSuspenseQuery(convexQuery(api.consent.getAccessList, {}));
  const { data: existingProjects } = useSuspenseQuery(convexQuery(api.consent.getUserSessionProjects, {}));
  const submitConsentMutation = useMutation(api.consent.submitConsent);
  const updateProjectSharingMutation = useMutation(api.consent.updateProjectSharing);

  // For first-time users, defer wizard rendering until client mount so
  // localStorage state can be restored without a hydration mismatch.
  const [wizardMounted, setWizardMounted] = useState(!!latestConsent);
  useLayoutEffect(() => {
    if (!wizardMounted) setWizardMounted(true);
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const [choices, setChoices] = useState<ConsentChoices>(() => {
    if (latestConsent) {
      return {
        sessionSharing: latestConsent.sessionSharing,
        communityFeatures: latestConsent.sessionSharing
          ? latestConsent.communityFeatures
          : null,
        publicationExcerpts: latestConsent.sessionSharing
          ? latestConsent.publicationExcerpts
          : null,
        creditByName: latestConsent.sessionSharing
          ? latestConsent.creditByName
          : null,
      };
    }
    // Restore from localStorage for first-time users (client only)
    if (typeof window !== "undefined") {
      try {
        const raw = JSON.parse(localStorage.getItem(WIZARD_STORAGE_KEY) ?? "null");
        if (raw?.choices) {
          const c = raw.choices;
          return {
            sessionSharing: typeof c.sessionSharing === "boolean" ? c.sessionSharing : null,
            communityFeatures: typeof c.communityFeatures === "boolean" ? c.communityFeatures : null,
            publicationExcerpts: typeof c.publicationExcerpts === "boolean" ? c.publicationExcerpts : null,
            creditByName: typeof c.creditByName === "boolean" ? c.creditByName : null,
          };
        }
      } catch { /* invalid localStorage */ }
    }
    return { sessionSharing: null, communityFeatures: null, publicationExcerpts: null, creditByName: null };
  });
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  // Track if user was returning at mount — used to suppress the wizard→preferences
  // flash for first-time users (whose mutation makes latestConsent non-null before navigate fires).
  const [wasReturning] = useState(() => latestConsent !== null);
  const isReturning = latestConsent !== null && (wasReturning || !isSubmitting);

  const handleChoice = (question: ConsentQuestion, value: boolean) => {
    setChoices((prev) => {
      const next = { ...prev, [question]: value };
      // When re-enabling sharing, restore sub-preferences from latest consent
      if (question === "sessionSharing" && value && latestConsent?.sessionSharing) {
        next.communityFeatures ??= latestConsent.communityFeatures;
        next.publicationExcerpts ??= latestConsent.publicationExcerpts;
        next.creditByName ??= latestConsent.creditByName;
      }
      return next;
    });
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
        if (selectedProjects && selectedProjects.size > 0) {
          const changes = [...selectedProjects].map((project) => {
            const ids = classifyLegacyProject(project);
            const identifier = ids as
              | { directory: string; gitRemote?: string }
              | { directory?: string; gitRemote: string };
            return { identifier, sessionSharing: true };
          });
          await updateProjectSharingMutation({ changes });
        }
      } else {
        await submitConsentMutation({
          consent: { sessionSharing: false },
        });
      }

      // First-time users go to install page; returning users stay on consent
      if (isReturning) {
        setIsSubmitting(false);
      } else {
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
        accessList={accessList}
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

  // First-time users see the wizard (deferred until client mount)
  if (!wizardMounted) return null;
  return (
    <ConsentWizard
      choices={choices}
      onChoice={handleChoice}
      onSubmit={handleSubmit}
      isSubmitting={isSubmitting}
      submitError={submitError}
      accessList={accessList}
      existingProjects={existingProjects}
    />
  );
}
