import { useEffect, useMemo } from "react";
import { Link } from "@tanstack/react-router";
import {
  questionConfigs,
  policyFooter,
  type ConsentQuestion,
} from "@/components/consent/policy-content";
import type { ConsentChoices } from "@/routes/_authenticated/consent";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

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
  // Pre-fill from existing consent
  useEffect(() => {
    if (existingConsent && choices.sessionSharing === null) {
      onChoice("sessionSharing", existingConsent.sessionSharing);
      if (existingConsent.sessionSharing) {
        if (existingConsent.communityFeatures !== undefined)
          onChoice("communityFeatures", existingConsent.communityFeatures);
        if (existingConsent.publicationExcerpts !== undefined)
          onChoice("publicationExcerpts", existingConsent.publicationExcerpts);
        if (existingConsent.creditByName !== undefined)
          onChoice("creditByName", existingConsent.creditByName);
      }
    }
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

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
                to="/install"
                className="text-primary underline underline-offset-4 hover:text-primary/80 transition-colors"
              >
                Install
              </Link>
            </div>
          </div>
        </div>

        {/* Session sharing — always shown */}
        {(() => {
          const config = questionConfigs[0];
          return (
            <div className="rounded-lg border p-4">
              <p className="text-sm font-medium mb-3">{config.label}</p>
              <div className="grid grid-cols-2 gap-2">
                <ChoiceButton
                  label={config.yesLabel}
                  selected={choices.sessionSharing === true}
                  onClick={() => onChoice(config.id, true)}
                  variant="positive"
                />
                <ChoiceButton
                  label={config.noLabel}
                  selected={choices.sessionSharing === false}
                  onClick={() => onChoice(config.id, false)}
                  variant="negative"
                />
              </div>
            </div>
          );
        })()}

        {/* Sub-preferences — grouped with left border */}
        {choices.sessionSharing && (
          <div className="mt-4 border-l-2 border-primary/30 pl-4 space-y-3">
            {questionConfigs.slice(1).map((config) => (
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
        {accessList.length > 0 && (
          <div className="mt-6 rounded-lg border px-5 py-4">
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
