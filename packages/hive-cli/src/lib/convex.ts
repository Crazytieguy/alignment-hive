import { ConvexHttpClient } from 'convex/browser';
import { api } from '../../../web/convex/_generated/api';
import { isAuthError, loadAuthData } from './auth';

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

export async function heartbeatSession(session: {
  sessionId: string;
  checkoutId: string;
  project: string;
  lineCount: number;
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
    project: string;
    lineCount: number;
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

    await client.mutation(api.sessions.saveUpload, {
      sessionId,
      storageId: storageId as any,
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

export async function getEnabledProjects(): Promise<Array<{ project: string; sessionSharing: boolean; consentedAt: number }>> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return [];
    return await client.query(api.consent.getEnabledProjects, {});
  } catch (error) {
    debugLog(`getEnabledProjects failed: ${error instanceof Error ? error.message : String(error)}`);
    return [];
  }
}

export async function enableProject(project: string): Promise<boolean> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return false;
    await client.mutation(api.consent.enableProject, { project });
    return true;
  } catch (error) {
    debugLog(`enableProject failed: ${error instanceof Error ? error.message : String(error)}`);
    return false;
  }
}

export async function disableProject(project: string): Promise<boolean> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return false;
    await client.mutation(api.consent.disableProject, { project });
    return true;
  } catch (error) {
    debugLog(`disableProject failed: ${error instanceof Error ? error.message : String(error)}`);
    return false;
  }
}

export { api };
