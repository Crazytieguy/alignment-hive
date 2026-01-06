import { describe, expect, test } from "bun:test";
import { detectSecrets, sanitizeDeep, sanitizeString } from "./sanitize";

// Use high-entropy test tokens (repeated characters fail entropy checks)
// GitHub PAT tokens are ghp_ + 36 alphanumeric chars
const TEST_GITHUB_TOKEN = "ghp_1a2B3c4D5e6F7g8H9i0J1k2L3m4N5o6P7qRs";

describe("detectSecrets", () => {
  test("detects GitHub token", () => {
    // GitHub personal access tokens (classic) start with ghp_
    // When standalone, matches github-pat; with context, may match generic-api-key
    const secrets = detectSecrets(TEST_GITHUB_TOKEN);
    expect(secrets.length).toBeGreaterThan(0);
    expect(secrets[0].ruleId).toBe("github-pat");
  });

  test("detects Slack webhook", async () => {
    // Slack webhooks are reliably detected
    const content =
      "https://hooks.slack.com/services/T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX";
    const secrets = await detectSecrets(content);
    expect(secrets.length).toBeGreaterThan(0);
  });

  test("returns empty for clean content", async () => {
    const content = "This is just normal text without any secrets.";
    const secrets = await detectSecrets(content);
    expect(secrets).toHaveLength(0);
  });
});

describe("sanitizeString", () => {
  test("replaces GitHub token with redaction marker", () => {
    const content = `token = ${TEST_GITHUB_TOKEN}`;
    const sanitized = sanitizeString(content);
    expect(sanitized).not.toContain("ghp_");
    expect(sanitized).toContain("[REDACTED:");
  });

  test("preserves non-secret content", () => {
    const content = "Hello, this is a normal message.";
    const sanitized = sanitizeString(content);
    expect(sanitized).toBe(content);
  });

  test("handles token at end of string", () => {
    const content = `my token is ${TEST_GITHUB_TOKEN}`;
    const sanitized = sanitizeString(content);
    expect(sanitized).not.toContain("ghp_");
    expect(sanitized).toContain("[REDACTED:");
    expect(sanitized).toContain("my token is");
  });
});

const TEST_GITHUB_TOKEN_2 = "ghp_9z8Y7x6W5v4U3t2S1r0Q9p8O7n6M5l4K3jTu";

describe("sanitizeDeep", () => {
  test("sanitizes strings in nested objects", async () => {
    const input = {
      level1: {
        level2: {
          secret: TEST_GITHUB_TOKEN,
          normal: "hello",
        },
      },
    };
    const sanitized = await sanitizeDeep(input);
    expect(sanitized.level1.level2.secret).not.toContain("ghp_");
    expect(sanitized.level1.level2.secret).toContain("[REDACTED:");
    expect(sanitized.level1.level2.normal).toBe("hello");
  });

  test("sanitizes strings in arrays", async () => {
    const input = ["normal", TEST_GITHUB_TOKEN, "also normal"];
    const sanitized = await sanitizeDeep(input);
    expect(sanitized[0]).toBe("normal");
    expect(sanitized[1]).toContain("[REDACTED:");
    expect(sanitized[2]).toBe("also normal");
  });

  test("handles mixed nested structures", async () => {
    const input = {
      users: [
        { name: "Alice", token: TEST_GITHUB_TOKEN },
        { name: "Bob", token: null },
      ],
      config: {
        apiKey: TEST_GITHUB_TOKEN_2,
        enabled: true,
        count: 42,
      },
    };
    const sanitized = await sanitizeDeep(input);

    expect(sanitized.users[0].name).toBe("Alice");
    expect(sanitized.users[0].token).toContain("[REDACTED:");
    expect(sanitized.users[1].name).toBe("Bob");
    expect(sanitized.users[1].token).toBe(null);
    expect(sanitized.config.apiKey).toContain("[REDACTED:");
    expect(sanitized.config.enabled).toBe(true);
    expect(sanitized.config.count).toBe(42);
  });

  test("preserves null and undefined", async () => {
    expect(await sanitizeDeep(null)).toBe(null);
    expect(await sanitizeDeep(undefined)).toBe(undefined);
  });

  test("preserves numbers and booleans", async () => {
    expect(await sanitizeDeep(42)).toBe(42);
    expect(await sanitizeDeep(true)).toBe(true);
    expect(await sanitizeDeep(false)).toBe(false);
  });

  test("handles deeply nested structures within depth limit", async () => {
    // Create a structure 50 levels deep (well under the 100 limit)
    let nested: Record<string, unknown> = {
      value: TEST_GITHUB_TOKEN,
    };
    for (let i = 0; i < 50; i++) {
      nested = { child: nested };
    }

    const sanitized = await sanitizeDeep(nested);

    // Navigate to the deepest level
    let current = sanitized;
    for (let i = 0; i < 50; i++) {
      current = current.child as Record<string, unknown>;
    }

    expect(current.value).toContain("[REDACTED:");
  });
});

describe("sanitizeString edge cases", () => {
  test("handles multiple secrets in one string", async () => {
    // Use Slack webhooks which are reliably detected
    const webhook1 =
      "https://hooks.slack.com/services/T11111111/B11111111/AAAAAAAAAAAAAAAAAAAAAA11";
    const webhook2 =
      "https://hooks.slack.com/services/T22222222/B22222222/BBBBBBBBBBBBBBBBBBBBBB22";
    const content = `webhooks: ${webhook1} and ${webhook2}`;
    const sanitized = await sanitizeString(content);

    expect(sanitized).not.toContain("hooks.slack.com");
    expect(sanitized.match(/\[REDACTED:/g)?.length).toBe(2);
  });

  test("handles empty string", async () => {
    const sanitized = await sanitizeString("");
    expect(sanitized).toBe("");
  });
});

describe("SAFE_KEYS optimization", () => {
  test("skips sanitization for known-safe string fields", async () => {
    // These fields are in SAFE_KEYS and should not be sanitized even if they
    // look like they might contain secrets
    const input = {
      uuid: "ghp_1a2B3c4D5e6F7g8H9i0J1k2L3m4N5o6P7qRs", // Would match github-pat
      type: "user",
      timestamp: "2025-01-01T00:00:00Z",
      sessionId: "sess-12345",
      // This field is NOT in SAFE_KEYS and should be sanitized
      content: TEST_GITHUB_TOKEN,
    };

    const sanitized = await sanitizeDeep(input);

    // Safe keys should be passed through unchanged
    expect(sanitized.uuid).toBe(input.uuid);
    expect(sanitized.type).toBe(input.type);
    expect(sanitized.timestamp).toBe(input.timestamp);
    expect(sanitized.sessionId).toBe(input.sessionId);

    // Non-safe keys should be sanitized
    expect(sanitized.content).toContain("[REDACTED:");
    expect(sanitized.content).not.toContain("ghp_");
  });

  test("sanitizes non-string safe key values", async () => {
    // If a safe key has a non-string value (like an object), it should still
    // be recursively processed
    const input = {
      type: {
        nested: TEST_GITHUB_TOKEN,
      },
    };

    const sanitized = (await sanitizeDeep(input)) as {
      type: { nested: string };
    };

    // The nested string should be sanitized because type's value is an object
    expect(sanitized.type.nested).toContain("[REDACTED:");
  });
});
