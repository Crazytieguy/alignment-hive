import { useState, useCallback, useMemo, useEffect } from "react";
import { CollapsibleAccessList } from "@/components/consent/access-list";
import {
  policySections,
  policyFooter,
  questionConfigs,
  type ConsentQuestion,
  type QuestionConfig,
  type SectionContent,
} from "@/components/consent/policy-content";
import type { ConsentChoices } from "@/routes/_authenticated/consent";
import { PolicyParagraph } from "@/components/consent/policy-paragraph";
import { Button, cn } from "@alignment-hive/ui";
import { useGithubStatus } from "@/hooks/use-github-status";
import { GITHUB_APP_INSTALL_URL } from "@/lib/constants";
import { codeContextExplanation } from "@/components/consent/policy-content";

export const WIZARD_STORAGE_KEY = "alignment-hive-consent-step";

interface ConsentWizardProps {
  choices: ConsentChoices;
  onChoice: (question: ConsentQuestion, value: boolean) => void;
  onSubmit: (selectedProjects?: Set<string>) => void;
  isSubmitting: boolean;
  submitError: string | null;
  accessList: Array<{ name: string | null; email: string }>;
  existingProjects: Array<{ project: string; count: number }>;
}

export default function ConsentWizard({
  choices,
  onChoice,
  onSubmit,
  isSubmitting,
  submitError,
  accessList,
  existingProjects,
}: ConsentWizardProps) {
  const [selectedProjects, setSelectedProjects] = useState<Set<string>>(
    () => new Set(existingProjects.map((p) => p.project)),
  );

  const toggleProject = (project: string) => {
    setSelectedProjects((prev) => {
      const next = new Set(prev);
      if (next.has(project)) {
        next.delete(project);
      } else {
        next.add(project);
      }
      return next;
    });
  };

  type Step =
    | {
        kind: "section";
        section: (typeof policySections)[number];
        questions: QuestionConfig[];
      }
    | { kind: "connect-github" }
    | { kind: "existing-projects" };

  const sectionSteps = useMemo(() => {
    return policySections.map((section): Step => {
      const questions = section.content
        .filter(
          (c): c is Extract<SectionContent, { kind: "question" }> =>
            c.kind === "question",
        )
        .map((c) => questionConfigs.find((q) => q.id === c.id))
        .filter((q): q is QuestionConfig => q != null);
      return { kind: "section", section, questions };
    });
  }, []);

  const sessionSharingStepIndex = sectionSteps.findIndex(
    (s) =>
      s.kind === "section" && s.questions.some((q) => q.id === "sessionSharing"),
  );

  const declinedSharing = choices.sessionSharing === false;
  const showExistingProjects =
    choices.sessionSharing === true && existingProjects.length > 0;

  const effectiveSteps = useMemo(() => {
    if (declinedSharing) {
      return sectionSteps.slice(0, sessionSharingStepIndex + 1);
    }
    const result: Array<Step> = [];
    for (let i = 0; i < sectionSteps.length; i++) {
      result.push(sectionSteps[i]);
      if (i === sessionSharingStepIndex && choices.sessionSharing) {
        result.push({ kind: "connect-github" });
        if (showExistingProjects) {
          result.push({ kind: "existing-projects" });
        }
      }
    }
    return result;
  }, [
    choices.sessionSharing,
    declinedSharing,
    sectionSteps,
    sessionSharingStepIndex,
    showExistingProjects,
  ]);

  // Safe to read localStorage here — wizard only renders on the client
  const [currentStep, setCurrentStep] = useState(() => {
    try {
      const raw = JSON.parse(localStorage.getItem(WIZARD_STORAGE_KEY) ?? "null");
      if (raw && typeof raw === "object" && typeof raw.step === "number") {
        return Math.max(0, raw.step);
      }
    } catch { /* invalid localStorage */ }
    return 0;
  });

  useEffect(() => {
    localStorage.setItem(
      WIZARD_STORAGE_KEY,
      JSON.stringify({ step: currentStep, choices }),
    );
  }, [currentStep, choices]);

  const handleSubmit = () => {
    localStorage.removeItem(WIZARD_STORAGE_KEY);
    onSubmit(showExistingProjects ? selectedProjects : undefined);
  };

  const clampedStep = Math.min(currentStep, effectiveSteps.length - 1);

  const totalSteps = effectiveSteps.length;
  const step = effectiveSteps[clampedStep] ?? effectiveSteps[0];
  const isLastStep = clampedStep === totalSteps - 1;

  const canAdvance = useCallback(() => {
    if (!step) return false;
    if (step.kind !== "section") return true;
    return step.questions.every((q) => choices[q.id] !== null);
  }, [step, choices]);

  const allAnswered =
    choices.sessionSharing !== null &&
    (!choices.sessionSharing ||
      (choices.communityFeatures !== null &&
        choices.publicationExcerpts !== null &&
        choices.creditByName !== null));

  return (
    <div className="min-h-screen flex flex-col items-center justify-start pt-16 pb-24 px-4">
      {/* Step indicator */}
      <div className="w-full max-w-xl mb-10">
        <div className="flex items-center gap-1">
          {effectiveSteps.map((_, i) => (
            <div
              key={i}
              className={cn(
                "h-1 flex-1 rounded-full transition-colors duration-500",
                i < clampedStep
                  ? "bg-primary"
                  : i === clampedStep
                    ? "bg-primary/60"
                    : "bg-border",
              )}
            />
          ))}
        </div>
        <div className="flex justify-between mt-2">
          <span className="text-xs text-muted-foreground">
            Step {clampedStep + 1} of {totalSteps}
          </span>
          {step.kind === "section" && step.section.title && (
            <span className="text-xs text-muted-foreground font-medium">
              {step.section.title}
            </span>
          )}
          {step.kind === "connect-github" && (
            <span className="text-xs text-muted-foreground font-medium">
              GitHub
            </span>
          )}
          {step.kind === "existing-projects" && (
            <span className="text-xs text-muted-foreground font-medium">
              Existing sessions
            </span>
          )}
        </div>
      </div>

      {/* Content */}
      <div className="w-full max-w-xl">
        <div
          key={clampedStep}
          className="animate-in fade-in slide-in-from-right-4 duration-300"
        >
          {step.kind === "connect-github" ? (
            <ConnectGitHubStep />
          ) : step.kind === "existing-projects" ? (
            <ExistingProjectsStep
              existingProjects={existingProjects}
              selectedProjects={selectedProjects}
              toggleProject={toggleProject}
              setSelectedProjects={setSelectedProjects}
            />
          ) : step.kind === "section" ? (
            <SectionStep
              step={step}
              currentStep={clampedStep}
              isLastStep={isLastStep}
              choices={choices}
              onChoice={onChoice}
              accessList={accessList}
            />
          ) : null}
        </div>

        {/* Navigation */}
        <div className="flex items-center justify-between mt-8 pt-6 border-t">
          <Button
            variant="ghost"
            onClick={() => setCurrentStep((s) => Math.max(0, s - 1))}
            disabled={clampedStep === 0}
            className="text-muted-foreground"
          >
            Back
          </Button>

          <div className="flex flex-col items-end gap-2">
            {isLastStep ? (
              <Button
                size="lg"
                onClick={handleSubmit}
                disabled={!allAnswered || isSubmitting}
                className="min-w-[180px]"
              >
                {isSubmitting ? "Saving..." : "Save preferences"}
              </Button>
            ) : (
              <Button
                onClick={() =>
                  setCurrentStep((s) => Math.min(totalSteps - 1, s + 1))
                }
                disabled={!canAdvance()}
              >
                Continue
              </Button>
            )}
            {submitError && isLastStep && (
              <p className="text-sm text-destructive">{submitError}</p>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function SectionStep({
  step,
  currentStep,
  isLastStep,
  choices,
  onChoice,
  accessList,
}: {
  step: {
    kind: "section";
    section: (typeof policySections)[number];
    questions: QuestionConfig[];
  };
  currentStep: number;
  isLastStep: boolean;
  choices: ConsentChoices;
  onChoice: (q: ConsentQuestion, v: boolean) => void;
  accessList: Array<{ name: string | null; email: string }>;
}) {
  return (
    <>
      {step.section.title && (
        <h2 className="text-2xl font-semibold mb-6 tracking-tight">
          {step.section.title}
        </h2>
      )}
      {!step.section.title && currentStep === 0 && (
        <h1 className="text-3xl font-semibold mb-6 tracking-tight">
          Data sharing preferences
        </h1>
      )}

      {step.section.content.map((item, i) => {
        const prev = step.section.content[i - 1];
        if (item.kind === "paragraph") {
          return (
            <div
              key={i}
              className={cn(
                "text-[0.938rem] leading-relaxed text-foreground/90",
                i === 0 ? "" : prev?.kind === "paragraph" ? "mt-4" : "mt-6",
              )}
            >
              <PolicyParagraph text={item.text} />
            </div>
          );
        }
        const config = step.questions.find((q) => q.id === item.id);
        if (!config) return null;
        return (
          <QuestionBlock
            key={i}
            config={config}
            value={choices[config.id]}
            onChoice={onChoice}
          />
        );
      })}

      {step.section.id === "access" && (
        <CollapsibleAccessList accessList={accessList} className="mt-6" />
      )}

      {isLastStep && (
        <p className="mt-8 text-sm text-muted-foreground">{policyFooter}</p>
      )}
    </>
  );
}

function ConnectGitHubStep() {
  const githubStatus = useGithubStatus();

  return (
    <>
      <h2 className="text-2xl font-semibold mb-6 tracking-tight">
        Code context for private repos
      </h2>
      <p className="text-[0.938rem] leading-relaxed text-foreground/90 mb-6">
        {codeContextExplanation}
      </p>

      {githubStatus === "installed" && (
        <div className="rounded-lg border-2 border-green-500/30 bg-green-500/5 px-5 py-4 mb-6">
          <p className="text-sm font-medium text-green-700 dark:text-green-400">
            Code context enabled! The repos you selected will be available to
            researchers viewing your sessions.
          </p>
        </div>
      )}

      {githubStatus === "requested" && (
        <div className="rounded-lg border-2 border-yellow-500/30 bg-yellow-500/5 px-5 py-4 mb-6">
          <p className="text-sm font-medium text-yellow-700 dark:text-yellow-400">
            Your org admin has been notified. Repos will appear once approved.
            You can continue setup now and link repos later.
          </p>
        </div>
      )}

      {!githubStatus && (
        <div className="space-y-3">
          <Button asChild>
            <a href={GITHUB_APP_INSTALL_URL}>Grant repo access</a>
          </Button>
          <p className="text-sm text-muted-foreground">
            You can also do this later at alignment-hive.com/consent/projects.
          </p>
        </div>
      )}
    </>
  );
}

function ExistingProjectsStep({
  existingProjects,
  selectedProjects,
  toggleProject,
  setSelectedProjects,
}: {
  existingProjects: Array<{ project: string; count: number }>;
  selectedProjects: Set<string>;
  toggleProject: (project: string) => void;
  setSelectedProjects: (s: Set<string>) => void;
}) {
  return (
    <>
      <h2 className="text-2xl font-semibold mb-6 tracking-tight">
        Existing sessions
      </h2>
      <p className="text-[0.938rem] leading-relaxed text-foreground/90 mb-6">
        The following projects already have sessions. Choose which to include
        under these consent terms.
      </p>
      <div className="space-y-2">
        {existingProjects.map((proj) => {
          const isSelected = selectedProjects.has(proj.project);
          return (
            <button
              key={proj.project}
              type="button"
              onClick={() => toggleProject(proj.project)}
              className={cn(
                "w-full flex items-center justify-between rounded-lg border-2 px-5 py-3 text-left transition-all duration-200",
                isSelected
                  ? "border-primary/40 bg-primary/[0.04]"
                  : "border-border hover:border-foreground/20",
              )}
            >
              <div className="flex items-center gap-3">
                <div
                  className={cn(
                    "size-5 rounded border-2 flex items-center justify-center transition-all duration-200",
                    isSelected
                      ? "border-primary bg-primary"
                      : "border-border",
                  )}
                >
                  {isSelected && (
                    <svg
                      width="12"
                      height="12"
                      viewBox="0 0 12 12"
                      fill="none"
                      stroke="currentColor"
                      strokeWidth="2"
                      strokeLinecap="round"
                      strokeLinejoin="round"
                      className="text-primary-foreground"
                    >
                      <path d="M2.5 6l2.5 2.5 4.5-5" />
                    </svg>
                  )}
                </div>
                <span className="font-mono text-sm">{proj.project}</span>
              </div>
              <span className="text-xs text-muted-foreground">
                {proj.count} session{proj.count !== 1 ? "s" : ""}
              </span>
            </button>
          );
        })}
      </div>
      <div className="mt-4 flex gap-3">
        <button
          type="button"
          onClick={() =>
            setSelectedProjects(
              new Set(existingProjects.map((p) => p.project)),
            )
          }
          className="text-xs text-muted-foreground hover:text-foreground transition-colors"
        >
          Select all
        </button>
        <button
          type="button"
          onClick={() => setSelectedProjects(new Set())}
          className="text-xs text-muted-foreground hover:text-foreground transition-colors"
        >
          Select none
        </button>
      </div>
    </>
  );
}

function QuestionBlock({
  config,
  value,
  onChoice,
}: {
  config: QuestionConfig;
  value: boolean | null;
  onChoice: (q: ConsentQuestion, v: boolean) => void;
}) {
  return (
    <div className="mt-6 rounded-lg border-2 border-primary/15 bg-primary/[0.03] px-5 py-4">
      {config.label && (
        <p className="font-medium text-sm mb-3">{config.label}</p>
      )}
      <div className="grid grid-cols-2 gap-2">
        <button
          type="button"
          onClick={() => onChoice(config.id, true)}
          className={cn(
            "rounded-md border-2 px-3 py-2 text-sm font-medium transition-all duration-200 cursor-pointer",
            value === true
              ? "border-primary bg-primary text-primary-foreground"
              : "border-border hover:border-primary/40 hover:bg-primary/5",
          )}
        >
          {config.yesLabel}
        </button>
        <button
          type="button"
          onClick={() => onChoice(config.id, false)}
          className={cn(
            "rounded-md border-2 px-3 py-2 text-sm font-medium transition-all duration-200 cursor-pointer",
            value === false
              ? "border-foreground/80 bg-foreground/10 text-foreground"
              : "border-border hover:border-foreground/30 hover:bg-foreground/5",
          )}
        >
          {config.noLabel}
        </button>
      </div>
    </div>
  );
}
