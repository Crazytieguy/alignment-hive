import { ConvexHttpClient } from 'convex/browser';
import { api } from '../../../web/convex/_generated/api';
import { isAuthError, loadAuthData } from './auth';
import type { Id } from '../../../web/convex/_generated/dataModel';
import type { ProjectIdentifiers as _ProjectIdentifiers } from '@alignment-hive/session-data';

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

export type ProjectIdentifier = _ProjectIdentifiers;

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

export interface EnabledProject {
  directories: Array<string>;
  gitRemotes: Array<string>;
  sessionSharing: boolean;
  consentedAt: number;
}

export async function getEnabledProjects(): Promise<Array<EnabledProject>> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return [];
    return await client.query(api.consent.getEnabledProjects, {});
  } catch (error) {
    debugLog(`getEnabledProjects failed: ${error instanceof Error ? error.message : String(error)}`);
    return [];
  }
}

export async function getConsentHistory(identifiers: ProjectIdentifier): Promise<{
  global: Array<{ sessionSharing: boolean; consentedAt: number }>;
  project: Array<{ sessionSharing: boolean; consentedAt: number }>;
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

export async function enableProject(identifier: ProjectIdentifier): Promise<boolean> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return false;
    // Cast: callers always provide at least directory, satisfying the Convex union
    await client.mutation(api.consent.enableProject, { identifier } as never);
    return true;
  } catch (error) {
    debugLog(`enableProject failed: ${error instanceof Error ? error.message : String(error)}`);
    return false;
  }
}

export async function disableProject(identifier: ProjectIdentifier): Promise<boolean> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return false;
    // Cast: callers always provide at least directory, satisfying the Convex union
    await client.mutation(api.consent.disableProject, { identifier } as never);
    return true;
  } catch (error) {
    debugLog(`disableProject failed: ${error instanceof Error ? error.message : String(error)}`);
    return false;
  }
}

export { api };
