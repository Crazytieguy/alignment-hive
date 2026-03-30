import { useMemo } from "react";
import { Link } from "@tanstack/react-router";
import {
  questionConfigs,
  policyFooter,
  type ConsentQuestion,
} from "@/components/consent/policy-content";
import { CollapsibleAccessList } from "@/components/consent/access-list";
import type { ConsentChoices } from "@/routes/_authenticated/consent";
import { Button, cn } from "@alignment-hive/ui";

interface ConsentSummaryProps {
  choices: ConsentChoices;
  onChoice: (question: ConsentQuestion, value: boolean) => void;
  onSubmit: () => void;
  isSubmitting: boolean;
  submitError: string | null;
  accessList: Array<{ name: string | null; email: string }>;
  existingConsent: {
    sessionSharing: boolean;
    communityFeatures?: boolean;
    publicationExcerpts?: boolean;
    creditByName?: boolean;
  } | null;
}

export default function ConsentSummary({
  choices,
  onChoice,
  onSubmit,
  isSubmitting,
  submitError,
  accessList,
  existingConsent,
}: ConsentSummaryProps) {
  const sharingConfig = questionConfigs.find((q) => q.id === "sessionSharing")!;
  const subConfigs = questionConfigs.filter((q) => q.id !== "sessionSharing");

  const allAnswered =
    choices.sessionSharing !== null &&
    (!choices.sessionSharing ||
      (choices.communityFeatures !== null &&
        choices.publicationExcerpts !== null &&
        choices.creditByName !== null));

  // Check if selections differ from existing consent
  const hasChanges = useMemo(() => {
    if (!existingConsent) return true;
    if (choices.sessionSharing === null) return false;
    if (choices.sessionSharing !== existingConsent.sessionSharing) return true;
    if (!choices.sessionSharing) return false;
    return (
      choices.communityFeatures !== (existingConsent.communityFeatures ?? null) ||
      choices.publicationExcerpts !== (existingConsent.publicationExcerpts ?? null) ||
      choices.creditByName !== (existingConsent.creditByName ?? null)
    );
  }, [choices, existingConsent]);

  return (
    <div className="min-h-screen flex flex-col items-center pt-16 pb-24 px-4">
      <div className="w-full max-w-lg">
        <div className="mb-10">
          <h1 className="text-3xl font-semibold tracking-tight">
            Data sharing preferences
          </h1>
          <div className="flex items-baseline justify-between mt-3 gap-4">
            <p className="text-sm text-muted-foreground">
              Changes apply to new data only.
            </p>
            <div className="flex gap-4 shrink-0 text-sm">
              <Link
                to="/policy"
                className="text-primary underline underline-offset-4 hover:text-primary/80 transition-colors"
              >
                Full policy
              </Link>
              <Link
                to="/consent/projects"
                className="text-primary underline underline-offset-4 hover:text-primary/80 transition-colors"
              >
                Manage projects
              </Link>
              <Link
                to="/install"
                className="text-primary underline underline-offset-4 hover:text-primary/80 transition-colors"
              >
                Install
              </Link>
            </div>
          </div>
        </div>

        {/* Session sharing — always shown */}
        <div className="rounded-lg border p-4">
          {sharingConfig.label && (
            <p className="text-sm font-medium mb-3">{sharingConfig.label}</p>
          )}
          <div className="grid grid-cols-2 gap-2">
            <ChoiceButton
              label={sharingConfig.yesLabel}
              selected={choices.sessionSharing === true}
              onClick={() => onChoice(sharingConfig.id, true)}
              variant="positive"
            />
            <ChoiceButton
              label={sharingConfig.noLabel}
              selected={choices.sessionSharing === false}
              onClick={() => onChoice(sharingConfig.id, false)}
              variant="negative"
            />
          </div>
        </div>

        {/* Sub-preferences — grouped with left border */}
        {choices.sessionSharing && (
          <div className="mt-4 border-l-2 border-primary/30 pl-4 space-y-3">
            {subConfigs.map((config) => (
              <div key={config.id} className="rounded-lg border p-4">
                <p className="text-sm font-medium mb-3">{config.label}</p>
                <div className="grid grid-cols-2 gap-2">
                  <ChoiceButton
                    label={config.yesLabel}
                    selected={choices[config.id] === true}
                    onClick={() => onChoice(config.id, true)}
                    variant="positive"
                  />
                  <ChoiceButton
                    label={config.noLabel}
                    selected={choices[config.id] === false}
                    onClick={() => onChoice(config.id, false)}
                    variant="negative"
                  />
                </div>
              </div>
            ))}
          </div>
        )}

        {/* Access list */}
        <CollapsibleAccessList accessList={accessList} className="mt-6" />

        {/* Submit */}
        <div className="mt-8 flex flex-col items-stretch gap-2">
          <Button
            size="lg"
            onClick={onSubmit}
            disabled={!allAnswered || !hasChanges || isSubmitting}
            className="w-full"
          >
            {isSubmitting ? "Saving..." : "Update preferences"}
          </Button>
          {submitError && (
            <p className="text-sm text-destructive text-center">
              {submitError}
            </p>
          )}
        </div>

        <p className="mt-10 text-sm text-muted-foreground">{policyFooter}</p>
      </div>
    </div>
  );
}

function ChoiceButton({
  label,
  selected,
  onClick,
  variant,
}: {
  label: string;
  selected: boolean;
  onClick: () => void;
  variant: "positive" | "negative";
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "rounded-md border px-3 py-2 text-sm font-medium transition-all cursor-pointer",
        selected && variant === "positive" &&
          "border-primary bg-primary text-primary-foreground",
        selected && variant === "negative" &&
          "border-foreground/60 bg-foreground/10",
        !selected &&
          "border-border hover:border-primary/40 hover:bg-primary/5",
      )}
    >
      {label}
    </button>
  );
}
