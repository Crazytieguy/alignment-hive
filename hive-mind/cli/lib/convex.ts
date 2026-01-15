import { ConvexHttpClient } from 'convex/browser';
import { api } from '../../../web/convex/_generated/api';
import { loadAuthData } from './auth';

const CONVEX_URL =
  process.env.CONVEX_URL ?? 'https://grateful-warbler-176.convex.cloud';

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
  const authData = await loadAuthData();
  if (!authData?.access_token) {
    return null;
  }

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
  sessionId: string
): Promise<string | null> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return null;

    return await client.mutation(api.sessions.generateUploadUrl, { sessionId });
  } catch (error) {
    debugLog(`generateUploadUrl failed: ${error instanceof Error ? error.message : String(error)}`);
    return null;
  }
}

export async function saveUpload(
  sessionId: string,
  storageId: string
): Promise<boolean> {
  try {
    const client = await getAuthenticatedClient();
    if (!client) return false;

    await client.mutation(api.sessions.saveUpload, {
      sessionId,
      storageId: storageId as any,
    });
    return true;
  } catch (error) {
    debugLog(`saveUpload failed: ${error instanceof Error ? error.message : String(error)}`);
    return false;
  }
}

export { api };
