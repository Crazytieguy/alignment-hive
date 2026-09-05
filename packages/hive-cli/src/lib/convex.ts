import { ConvexHttpClient } from 'convex/browser';
import { api } from '../../../web/convex/_generated/api';
import { getAuthData } from './auth';
import { getProjectIdentifiers, matchesProject } from './config';
import { hive } from './messages';
import type { ProjectIds } from './config';
import type { ConsentEvent, WorkflowRunRow } from '@alignment-hive/session-data';
import type { Id } from '../../../web/convex/_generated/dataModel';

let clientInstance: ConvexHttpClient | null = null;

function getConvexClient(): ConvexHttpClient {
  clientInstance ??= new ConvexHttpClient(
    process.env.ALIGNMENT_HIVE_CONVEX_URL ?? 'https://grateful-warbler-176.convex.cloud',
  );
  return clientInstance;
}

/**
 * Run a backend call as the logged-in user. Throws the not-authenticated message when there is no
 * login, and lets backend or network failures propagate: callers that must stay quiet catch.
 */
async function withClient<T>(fn: (client: ConvexHttpClient) => Promise<T>): Promise<T> {
  const authData = await getAuthData();
  if (!authData) throw new Error(hive.upload.notAuthenticated);
  const client = getConvexClient();
  client.setAuth(authData.access_token);
  return fn(client);
}

export async function pingCheckout(checkoutId: string): Promise<void> {
  await getConvexClient().mutation(api.sessions.upsertCheckout, { checkoutId });
}

/** A query result the backend only returns null for when it does not recognize the caller. */
function orNotAuthenticated<T>(value: T | null): T {
  if (value === null) throw new Error(hive.upload.notAuthenticated);
  return value;
}

export function heartbeatSession(session: {
  sessionId: string;
  checkoutId: string;
  directory?: string;
  gitRemote?: string;
  lineCount: number;
  lastModified?: number;
}): Promise<void> {
  return withClient(async (client) => {
    await client.mutation(api.sessions.heartbeatSession, session);
  });
}

export interface ConsentIdentifiers {
  directory?: string;
  gitRemote?: string;
  lastModified?: number;
}

export function generateUploadUrls(
  sessionId: string,
  agentSessionIds: Array<string>,
  consentIdentifiers: ConsentIdentifiers,
  workflowRunIds: Array<string> = [],
): Promise<Record<string, string>> {
  return withClient((client) =>
    client.mutation(api.sessions.generateUploadUrls, {
      sessionId,
      agentSessionIds,
      // Omit when empty (the arg is optional server-side): during a deploy-skew window an old
      // backend rejects unknown args, which would break EVERY upload instead of workflow ones.
      ...(workflowRunIds.length > 0 && { workflowRunIds }),
      ...consentIdentifiers,
    }),
  );
}

export interface UploadRecord {
  sessionId: string;
  storageId: Id<'_storage'>;
  summary?: string;
  lineCount: number;
  parentSessionId?: string;
  agentType?: string;
  workflowRunId?: string;
}

export function saveUploads(
  parentSessionId: string,
  sessionMeta: ConsentIdentifiers & { checkoutId: string; sessionStartGitCommitHash?: string },
  uploads: Array<UploadRecord>,
): Promise<void> {
  return withClient(async (client) => {
    await client.mutation(api.sessions.saveUploads, { parentSessionId, ...sessionMeta, uploads });
  });
}

export interface WorkflowRunUpload extends WorkflowRunRow {
  storageId: Id<'_storage'>;
}

export function saveWorkflowRuns(
  parentSessionId: string,
  meta: ConsentIdentifiers,
  runs: Array<WorkflowRunUpload>,
): Promise<void> {
  return withClient(async (client) => {
    await client.mutation(api.sessions.saveWorkflowRuns, { parentSessionId, ...meta, runs });
  });
}

export function getConsentStatus(): Promise<{ hasConsent: boolean; sessionSharing: boolean }> {
  return withClient(async (client) => orNotAuthenticated(await client.query(api.consent.getConsentStatus, {})));
}

export function getProjectSharing() {
  return withClient((client) => client.query(api.consent.getProjectSharing, {}));
}

export function getConsentHistory(
  identifiers: ProjectIds,
): Promise<{ global: Array<ConsentEvent>; project: Array<ConsentEvent> }> {
  return withClient(async (client) =>
    orNotAuthenticated(await client.query(api.consent.getConsentHistory, identifiers)),
  );
}

export function updateProjectSharing(
  changes: Array<{ identifier: ProjectIds; sessionSharing: boolean }>,
): Promise<void> {
  return withClient(async (client) => {
    await client.mutation(api.consent.updateProjectSharing, { changes });
  });
}

/** Display-only status: null when it cannot be determined. */
export async function getRepoLinkStatus(gitRemote: string): Promise<'linked' | 'not-linked' | null> {
  try {
    return await withClient((client) =>
      client.query(api.github.getRepoLinkStatus, { gitRemote: gitRemote.toLowerCase() }),
    );
  } catch {
    return null;
  }
}

/**
 * The consent gate for uploading from cwd. Throws a user-facing message when not logged in,
 * when global sharing is off, or when this project is not enabled.
 */
export async function resolveProjectConsent(cwd: string): Promise<{ consentMtime: number; ids: ProjectIds }> {
  const [consent, allProjects] = await Promise.all([getConsentStatus(), getProjectSharing()]);
  if (!consent.hasConsent || !consent.sessionSharing) throw new Error(hive.upload.noConsent);

  const ids = getProjectIdentifiers(cwd);
  const projectConsent = matchesProject(allProjects, ids);
  if (!projectConsent?.sessionSharing) throw new Error(hive.upload.noProjectConsent);

  return { consentMtime: projectConsent.latestAt, ids };
}
