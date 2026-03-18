import { useEffect, useState } from "react";
import { createFileRoute, Link } from "@tanstack/react-router";
import { useQuery, useMutation } from "convex/react";
import { api } from "../../../convex/_generated/api";
import { Button, cn } from "@alignment-hive/ui";
import { useGithubStatus } from "@/hooks/use-github-status";
import { GITHUB_APP_INSTALL_URL } from "@/lib/constants";
import { projectSharingNote } from "@/components/consent/policy-content";

export const Route = createFileRoute("/_authenticated/consent_/projects")({
  component: ProjectsPage,
});

async function isRepoPublic(gitRemote: string): Promise<boolean> {
  const repoPath = gitRemote.replace(/^github\.com\//, "");
  try {
    const res = await fetch(`https://api.github.com/repos/${repoPath}`);
    return res.status === 200;
  } catch {
    return false;
  }
}

function ProjectsPage() {
  const allProjects = useQuery(api.consent.getAllProjects);
  const enableProject = useMutation(api.consent.enableProject);
  const disableProject = useMutation(api.consent.disableProject);
  const githubStatus = useGithubStatus();

  // Local toggle state — tracks pending changes before save
  const [pendingState, setPendingState] = useState<Map<number, boolean>>(
    new Map(),
  );
  const [isSaving, setIsSaving] = useState(false);

  // Reset pending state when server data changes
  useEffect(() => {
    setPendingState(new Map());
  }, [allProjects]);

  const getEffectiveSharing = (i: number, serverSharing: boolean) =>
    pendingState.has(i) ? pendingState.get(i)! : serverSharing;

  const hasChanges =
    allProjects?.some(
      (p, i) => pendingState.has(i) && pendingState.get(i) !== p.sessionSharing,
    ) ?? false;

  const toggleProject = (i: number, currentServerState: boolean) => {
    setPendingState((prev) => {
      const next = new Map(prev);
      const currentEffective = next.has(i) ? next.get(i)! : currentServerState;
      const newValue = !currentEffective;
      // If toggling back to server state, remove from pending
      if (newValue === currentServerState) {
        next.delete(i);
      } else {
        next.set(i, newValue);
      }
      return next;
    });
  };

  const saveChanges = async () => {
    if (!allProjects) return;
    setIsSaving(true);
    try {
      for (const [i, shouldShare] of pendingState.entries()) {
        const project = allProjects[i];
        if (shouldShare === project.sessionSharing) continue;

        const githubRemote = project.gitRemotes.find((r) =>
          r.startsWith("github.com/"),
        );
        const dir = project.directories[0];
        const identifier = dir
          ? { directory: dir, gitRemote: githubRemote }
          : githubRemote
            ? { gitRemote: githubRemote }
            : null;
        if (!identifier) continue;

        if (shouldShare) {
          await enableProject({ identifier });
        } else {
          await disableProject({ identifier });
        }
      }
      setPendingState(new Map());
    } finally {
      setIsSaving(false);
    }
  };

  // Batch check link status for GitHub remotes
  const githubRemotes =
    allProjects
      ?.flatMap((p) => p.gitRemotes)
      .filter((r) => r.startsWith("github.com/")) ?? [];

  const linkedRemotes = useQuery(
    api.github.getLinkedRemotesFromList,
    githubRemotes.length > 0 ? { gitRemotes: githubRemotes } : "skip",
  );
  const linkedSet = new Set(linkedRemotes ?? []);

  // Check visibility for unlinked GitHub repos
  const [visibilityMap, setVisibilityMap] = useState<
    Map<string, "public" | "private">
  >(new Map());

  useEffect(() => {
    if (!allProjects || !linkedRemotes) return;

    const unlinkedRemotes = githubRemotes.filter((r) => !linkedSet.has(r));
    if (unlinkedRemotes.length === 0) return;

    const toCheck = unlinkedRemotes.filter((r) => !visibilityMap.has(r));
    if (toCheck.length === 0) return;

    Promise.all(
      toCheck.map(async (remote) => {
        const pub = await isRepoPublic(remote);
        return [remote, pub ? "public" : "private"] as const;
      }),
    ).then((results) => {
      setVisibilityMap((prev) => {
        const next = new Map(prev);
        for (const [remote, vis] of results) {
          next.set(remote, vis);
        }
        return next;
      });
    });
  }, [allProjects, linkedRemotes]); // eslint-disable-line react-hooks/exhaustive-deps

  const hasPrivateUnlinked =
    allProjects?.some((p) =>
      p.gitRemotes.some((r) => {
        if (!r.startsWith("github.com/")) return false;
        if (linkedSet.has(r)) return false;
        return visibilityMap.get(r) === "private";
      }),
    ) ?? false;

  return (
    <div className="min-h-screen flex flex-col items-center pt-16 pb-24 px-4">
      <div className="w-full max-w-lg">
        <div className="mb-10">
          <h1 className="text-3xl font-semibold tracking-tight">Projects</h1>
          <div className="flex items-baseline justify-between mt-3 gap-4">
            <p className="text-sm text-muted-foreground">
              Manage which projects share sessions.
            </p>
            <Link
              to="/consent"
              className="text-sm text-primary underline underline-offset-4 hover:text-primary/80 transition-colors shrink-0"
            >
              Data sharing preferences
            </Link>
          </div>
        </div>

        {/* Status banners */}
        {githubStatus === "installed" && (
          <div className="rounded-lg border-2 border-green-500/30 bg-green-500/5 px-5 py-4 mb-6">
            <p className="text-sm font-medium text-green-700 dark:text-green-400">
              Code context enabled! The repos you selected will be available
              to researchers viewing your sessions.
            </p>
          </div>
        )}

        {githubStatus === "requested" && (
          <div className="rounded-lg border-2 border-yellow-500/30 bg-yellow-500/5 px-5 py-4 mb-6">
            <p className="text-sm font-medium text-yellow-700 dark:text-yellow-400">
              Your org admin has been notified. Repos will appear as linked
              once approved.
            </p>
          </div>
        )}

        {/* GitHub App section */}
        {hasPrivateUnlinked ? (
          <div className="rounded-lg border border-primary/20 bg-primary/[0.03] px-5 py-4 mb-6">
            <p className="text-sm text-foreground/80 mb-3">
              Some private repos aren't linked for code context. Grant repo
              access so researchers can see the code your sessions reference.
            </p>
            <div className="flex gap-3">
              <Button size="sm" asChild>
                <a href={GITHUB_APP_INSTALL_URL}>Grant repo access</a>
              </Button>
              <a
                href="https://github.com/settings/installations"
                target="_blank"
                rel="noopener noreferrer"
                className="text-xs text-muted-foreground underline underline-offset-4 self-center"
              >
                Manage existing
              </a>
            </div>
          </div>
        ) : (
          <div className="flex gap-4 mb-6 text-sm">
            <a
              href={GITHUB_APP_INSTALL_URL}
              className="text-muted-foreground underline underline-offset-4 hover:text-foreground transition-colors"
            >
              Grant repo access
            </a>
            <a
              href="https://github.com/settings/installations"
              target="_blank"
              rel="noopener noreferrer"
              className="text-muted-foreground underline underline-offset-4 hover:text-foreground transition-colors"
            >
              Manage repo access
            </a>
          </div>
        )}

        {/* Project list */}
        {allProjects && allProjects.length > 0 ? (
          <>
            <div className="space-y-2">
              {allProjects.map((project, i) => {
                const displayName =
                  project.gitRemotes[0] ??
                  project.directories[0] ??
                  "Unknown";

                const githubRemote = project.gitRemotes.find((r) =>
                  r.startsWith("github.com/"),
                );

                const isLinked = githubRemote
                  ? linkedSet.has(githubRemote)
                  : false;
                const visibility = githubRemote
                  ? visibilityMap.get(githubRemote)
                  : undefined;
                const showLinkBadge =
                  githubRemote && visibility === "private";

                const effectiveSharing = getEffectiveSharing(
                  i,
                  project.sessionSharing,
                );
                const isChanged =
                  pendingState.has(i) &&
                  pendingState.get(i) !== project.sessionSharing;

                return (
                  <button
                    key={displayName}
                    type="button"
                    onClick={() => toggleProject(i, project.sessionSharing)}
                    className={cn(
                      "w-full flex items-center justify-between rounded-md border-2 px-4 py-3 text-left transition-all duration-200",
                      effectiveSharing
                        ? "border-primary/40 bg-primary/[0.04]"
                        : "border-border hover:border-foreground/20",
                      isChanged && "ring-2 ring-primary/20",
                    )}
                  >
                    <div className="flex items-center gap-3 min-w-0">
                      <div
                        className={cn(
                          "size-5 rounded border-2 flex items-center justify-center transition-all duration-200",
                          effectiveSharing
                            ? "border-primary bg-primary"
                            : "border-border",
                        )}
                      >
                        {effectiveSharing && (
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
                      <span className="font-mono text-sm truncate">
                        {displayName}
                      </span>
                      {showLinkBadge && (
                        <span
                          className={cn(
                            "text-[0.7rem] px-1.5 py-0.5 rounded shrink-0",
                            isLinked
                              ? "bg-green-500/10 text-green-700 dark:text-green-400"
                              : "bg-yellow-500/10 text-yellow-700 dark:text-yellow-400",
                          )}
                        >
                          {isLinked ? "linked" : "not linked"}
                        </span>
                      )}
                    </div>
                  </button>
                );
              })}
            </div>

            {/* Info + save */}
            <p className="mt-6 text-sm text-muted-foreground">
              {projectSharingNote}
            </p>
            <div className="mt-4 flex gap-3">
              <Button
                onClick={saveChanges}
                disabled={!hasChanges || isSaving}
              >
                {isSaving ? "Saving..." : "Save changes"}
              </Button>
              {hasChanges && (
                <Button
                  variant="ghost"
                  onClick={() => setPendingState(new Map())}
                  disabled={isSaving}
                >
                  Discard
                </Button>
              )}
            </div>
          </>
        ) : allProjects ? (
          <p className="text-sm text-muted-foreground">
            No projects configured yet. Enable sharing for projects using the
            CLI: <code className="text-xs">hive consent setup</code>
          </p>
        ) : null}
      </div>
    </div>
  );
}
