/**
 * WorkOS API key validation for HTTP API endpoints.
 * Called directly in Hono middleware — not a Convex action.
 */

interface ValidatedApiKey {
  orgId: string;
  keyId: string;
  permissions: string[];
}

export async function validateApiKey(
  apiKeyValue: string,
): Promise<ValidatedApiKey | null> {
  const workosApiKey = process.env.WORKOS_API_KEY;
  if (!workosApiKey) {
    console.error("WORKOS_API_KEY environment variable not set");
    return null;
  }

  const resp = await fetch("https://api.workos.com/api_keys/validations", {
    method: "POST",
    headers: {
      Authorization: `Bearer ${workosApiKey}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ value: apiKeyValue }),
  });

  if (!resp.ok) {
    console.error(`WorkOS API key validation failed: ${resp.status}`);
    return null;
  }

  const data = (await resp.json()) as {
    api_key: {
      id: string;
      owner: { type: string; id: string };
      permissions: string[];
    } | null;
  };

  // WorkOS returns { api_key: null } for invalid keys (200 OK)
  if (!data.api_key) return null;

  return {
    orgId: data.api_key.owner.id,
    keyId: data.api_key.id,
    permissions: data.api_key.permissions ?? [],
  };
}
