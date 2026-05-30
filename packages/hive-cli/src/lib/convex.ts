import { ConvexHttpClient } from 'convex/browser';
import { api } from '../../../web/convex/_generated/api';
import { getAuthData } from './auth';
import { getProjectIdentifiers, matchesProject } from './config';
import type { Id } from '../../../web/convex/_generated/dataModel';
import type { ProjectIdentifiers } from '@alignment-hive/session-data';

const CONVEX_URL = process.env.ALIGNMENT_HIVE_CONVEX_URL ?? 'https://grateful-warbler-176.convex.cloud';

function debugLog(message: string): void {
  if (process.env.DEBUG) {
    console.error(`[convex] ${message}`);
  }
}

let clientInstance: ConvexHttpClient | null = null;

export function getConvexClient(): ConvexHttpClient {
  if (!clientInstance) {
    clientInstance = new ConvexHttpClient(CONVEX_URL);
  }
  return clientInstance;
}

/** Get an authenticated Convex client, refreshing the token if needed. Returns null if not logged in. Throws on refresh failure. */
export async function getAuthenticatedClient(): Promise<ConvexHttpClient | null> {
  const authData = await getAuthData();
  if (!authData) return null;
  const client = getConvexClient();
  client.setAuth(authData.access_token);
  return client;
}

export async function pingCheckout(checkoutId: string): Promise<boolean> {
  try {
    const client = getConvexClient();
    await client.mutation(api.sessions.upsertCheckout, { checkoutId });
    return true;
  } catch (error) {
    debugLog(`pingCheckout failed: ${error instanceof Error ? error.message : String(error)}`);
    return false;
  }
}

export type { ProjectIdentifiers };

export async function heartbeatSession(session: {
  sessionId: string;
  checkoutId: string;
  project?: string;
  directory?: string;
  gitRemote?: string;
  lineCount: number;
  lastModified?: number;
  parentSessionId?: string;
}): Promise<boolean> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return false;

    await client.mutation(api.sessions.heartbeatSession, session);
    return true;
  } catch (error) {
    debugLog(`heartbeatSession failed: ${error instanceof Error ? error.message : String(error)}`);
    return false;
  }
}

export async function generateUploadUrls(
  sessionId: string,
  agentSessionIds: Array<string>,
  consentIdentifiers: {
    directory?: string;
    gitRemote?: string;
    lastModified?: number;
  },
  workflowRunIds: Array<string> = [],
): Promise<Record<string, string> | null> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return null;

    return await client.mutation(api.sessions.generateUploadUrls, {
      sessionId,
      agentSessionIds,
      workflowRunIds,
      ...consentIdentifiers,
    });
  } catch (error) {
    debugLog(`generateUploadUrls failed: ${error instanceof Error ? error.message : String(error)}`);
    return null;
  }
}

export async function saveUploads(
  parentSessionId: string,
  sessionMeta: {
    checkoutId: string;
    directory?: string;
    gitRemote?: string;
    lastModified?: number;
    sessionStartGitCommitHash?: string;
  },
  uploads: Array<{
    sessionId: string;
    storageId: string;
    summary?: string;
    lineCount: number;
    parentSessionId?: string;
    agentType?: string;
    workflowRunId?: string;
  }>,
): Promise<boolean> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return false;

    await client.mutation(api.sessions.saveUploads, {
      parentSessionId,
      ...sessionMeta,
      uploads: uploads.map((u) => ({
        ...u,
        storageId: u.storageId as unknown as Id<"_storage">,
      })),
    });
    return true;
  } catch (error) {
    debugLog(`saveUploads failed: ${error instanceof Error ? error.message : String(error)}`);
    return false;
  }
}

export interface WorkflowRunUpload {
  workflowRunId: string;
  runId: string;
  storageId: string;
  workflowName?: string;
  summary?: string;
  status?: string;
  totalTokens?: number;
  totalToolCalls?: number;
  agentCount?: number;
  durationMs?: number;
}

export async function saveWorkflowRuns(
  parentSessionId: string,
  meta: { directory?: string; gitRemote?: string; lastModified?: number },
  runs: Array<WorkflowRunUpload>,
): Promise<boolean> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return false;

    await client.mutation(api.sessions.saveWorkflowRuns, {
      parentSessionId,
      ...meta,
      runs: runs.map((r) => ({
        ...r,
        storageId: r.storageId as unknown as Id<"_storage">,
      })),
    });
    return true;
  } catch (error) {
    debugLog(`saveWorkflowRuns failed: ${error instanceof Error ? error.message : String(error)}`);
    return false;
  }
}

export async function getConsentStatus(): Promise<{ hasConsent: boolean; sessionSharing: boolean } | null> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return null;
    return await client.query(api.consent.getConsentStatus, {});
  } catch (error) {
    debugLog(`getConsentStatus failed: ${error instanceof Error ? error.message : String(error)}`);
    return null;
  }
}

export interface ProjectSharingState {
  directories: Array<string>;
  gitRemotes: Array<string>;
  sessionSharing: boolean;
  latestAt: number;
}

export async function getProjectSharing(): Promise<Array<ProjectSharingState>> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return [];
    return await client.query(api.consent.getProjectSharing, {});
  } catch (error) {
    debugLog(`getProjectSharing failed: ${error instanceof Error ? error.message : String(error)}`);
    return [];
  }
}

export async function getConsentHistory(identifiers: ProjectIdentifiers): Promise<{
  global: Array<{ sessionSharing: boolean; timestamp: number }>;
  project: Array<{ sessionSharing: boolean; timestamp: number }>;
} | null> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return null;
    return await client.query(api.consent.getConsentHistory, identifiers);
  } catch (error) {
    debugLog(`getConsentHistory failed: ${error instanceof Error ? error.message : String(error)}`);
    return null;
  }
}

/** Narrow ProjectIdentifiers (both optional) to the Convex union (at least one required). */
function narrowIdentifier(
  id: ProjectIdentifiers,
): { directory: string; gitRemote?: string } | { directory?: string; gitRemote: string } {
  if (id.directory) return { directory: id.directory, gitRemote: id.gitRemote };
  if (id.gitRemote) return { gitRemote: id.gitRemote };
  throw new Error('At least one of directory or gitRemote is required');
}

export async function updateProjectSharing(
  changes: Array<{ identifier: ProjectIdentifiers; sessionSharing: boolean }>,
): Promise<boolean> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return false;
    await client.mutation(api.consent.updateProjectSharing, {
      changes: changes.map(({ identifier, sessionSharing }) => ({
        identifier: narrowIdentifier(identifier),
        sessionSharing,
      })),
    });
    return true;
  } catch (error) {
    debugLog(`updateProjectSharing failed: ${error instanceof Error ? error.message : String(error)}`);
    return false;
  }
}

export type RepoLinkStatus = "linked" | "not-linked";

export async function getRepoLinkStatus(gitRemote: string): Promise<RepoLinkStatus | null> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return null;
    return await client.query(api.github.getRepoLinkStatus, { gitRemote: gitRemote.toLowerCase() });
  } catch (error) {
    debugLog(`getRepoLinkStatus failed: ${error instanceof Error ? error.message : String(error)}`);
    return null;
  }
}

/** Resolve project consent state for the current project. */
export async function resolveProjectConsent(cwd: string) {
  const authData = await getAuthData();
  if (!authData) return { error: 'not-authenticated' } as const;

  const [consent, allProjects] = await Promise.all([
    getConsentStatus(),
    getProjectSharing(),
  ]);

  if (!consent?.hasConsent || !consent.sessionSharing) return { error: 'no-consent' } as const;

  const ids = getProjectIdentifiers(cwd);
  const projectConsent = matchesProject(allProjects, ids);
  if (!projectConsent?.sessionSharing) return { error: 'no-project-consent' } as const;

  return { consentMtime: projectConsent.latestAt, ids } as const;
}

export { api };
