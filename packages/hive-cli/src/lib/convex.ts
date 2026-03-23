import { ConvexHttpClient } from 'convex/browser';
import { api } from '../../../web/convex/_generated/api';
import { isAuthError, loadAuthData } from './auth';
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

export async function getAuthenticatedClient(): Promise<ConvexHttpClient | null> {
  const authResult = await loadAuthData();
  if (!authResult || isAuthError(authResult)) {
    return null;
  }

  const client = getConvexClient();
  client.setAuth(authResult.access_token);
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

export async function generateUploadUrl(
  sessionId: string,
  heartbeat?: {
    checkoutId: string;
    project?: string;
    directory?: string;
    gitRemote?: string;
    lineCount: number;
    lastModified?: number;
    parentSessionId?: string;
    sessionStartGitCommitHash?: string;
  },
): Promise<string | null> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return null;

    return await client.mutation(api.sessions.generateUploadUrl, {
      sessionId,
      ...heartbeat,
    });
  } catch (error) {
    debugLog(`generateUploadUrl failed: ${error instanceof Error ? error.message : String(error)}`);
    return null;
  }
}

export async function generateUploadUrls(
  sessionId: string,
  agentSessionIds: Array<string>,
  heartbeat: {
    checkoutId: string;
    project?: string;
    directory?: string;
    gitRemote?: string;
    lineCount: number;
    lastModified?: number;
    sessionStartGitCommitHash?: string;
  },
): Promise<Record<string, string> | null> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return null;

    return await client.mutation(api.sessions.generateUploadUrls, {
      sessionId,
      agentSessionIds,
      ...heartbeat,
    });
  } catch (error) {
    debugLog(`generateUploadUrls failed: ${error instanceof Error ? error.message : String(error)}`);
    return null;
  }
}

export async function saveUploads(
  parentSessionId: string,
  uploads: Array<{ sessionId: string; storageId: string; summary?: string }>,
): Promise<boolean> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return false;

    await client.mutation(api.sessions.saveUploads, {
      parentSessionId,
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

export async function saveUpload(sessionId: string, storageId: string, summary?: string): Promise<boolean> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return false;

    // storageId is returned by Convex storage upload as a string but the mutation expects Id<"_storage">
    await client.mutation(api.sessions.saveUpload, {
      sessionId,
      storageId: storageId as unknown as Id<"_storage">,
      summary,
    });
    return true;
  } catch (error) {
    debugLog(`saveUpload failed: ${error instanceof Error ? error.message : String(error)}`);
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

export { api };
