/**
 * Tests for CLI commands: grep and read
 *
 * These tests verify public behavior by:
 * 1. Creating temp session files
 * 2. Mocking process.cwd and process.argv
 * 3. Capturing console output
 * 4. Verifying expected results
 */

import { mkdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { afterEach, beforeEach, describe, expect, spyOn, test } from "bun:test";

// Test session data
function createTestSession(sessionId: string, entries: Array<object>, options?: { agentId?: string }): string {
  const meta = {
    _type: "hive-mind-meta",
    version: "0.1",
    sessionId,
    checkoutId: "test-checkout-id",
    extractedAt: "2025-01-01T00:00:00Z",
    rawMtime: "2025-01-01T00:00:00Z",
    rawPath: `/fake/path/${sessionId}.jsonl`,
    messageCount: entries.length,
    ...(options?.agentId && { agentId: options.agentId }),
  };
  return [JSON.stringify(meta), ...entries.map((e) => JSON.stringify(e))].join("\n");
}

const userEntry = (uuid: string, content: string) => ({
  type: "user",
  uuid,
  parentUuid: null,
  timestamp: "2025-01-01T00:00:00Z",
  message: { role: "user", content },
});

const assistantEntry = (uuid: string, parentUuid: string, content: string) => ({
  type: "assistant",
  uuid,
  parentUuid,
  timestamp: "2025-01-01T00:00:01Z",
  message: { role: "assistant", content },
});

const assistantWithThinking = (uuid: string, parentUuid: string, thinking: string, text: string) => ({
  type: "assistant",
  uuid,
  parentUuid,
  timestamp: "2025-01-01T00:00:01Z",
  message: {
    role: "assistant",
    content: [
      { type: "thinking", thinking },
      { type: "text", text },
    ],
  },
});

const assistantWithToolUse = (uuid: string, parentUuid: string, toolName: string, input: object) => ({
  type: "assistant",
  uuid,
  parentUuid,
  timestamp: "2025-01-01T00:00:01Z",
  message: {
    role: "assistant",
    content: [{ type: "tool_use", id: "tool-1", name: toolName, input }],
  },
});

const userWithToolResult = (uuid: string, parentUuid: string, toolUseId: string, result: string) => ({
  type: "user",
  uuid,
  parentUuid,
  timestamp: "2025-01-01T00:00:02Z",
  message: {
    role: "user",
    content: [{ type: "tool_result", tool_use_id: toolUseId, content: result }],
  },
});

describe("grep command", () => {
  let tempDir: string;
  let sessionsDir: string;
  let originalCwd: () => string;
  let originalArgv: Array<string>;
  let consoleOutput: Array<string>;
  let consoleSpy: ReturnType<typeof spyOn>;

  beforeEach(async () => {
    // Create temp directory structure
    tempDir = join(tmpdir(), `hive-test-${Date.now()}-${Math.random().toString(36).slice(2)}`);
    sessionsDir = join(tempDir, ".claude", "hive-mind", "sessions");
    await mkdir(sessionsDir, { recursive: true });

    // Mock process.cwd
    originalCwd = process.cwd;
    process.cwd = () => tempDir;

    // Save original argv
    originalArgv = process.argv;

    // Capture console output
    consoleOutput = [];
    consoleSpy = spyOn(console, "log").mockImplementation((...args: Array<unknown>) => {
      consoleOutput.push(args.map(String).join(" "));
    });
  });

  afterEach(async () => {
    process.cwd = originalCwd;
    process.argv = originalArgv;
    consoleSpy.mockRestore();
    await rm(tempDir, { recursive: true });
  });

  test("finds simple pattern in session", async () => {
    await writeFile(
      join(sessionsDir, "test-session-1.jsonl"),
      createTestSession("test-session-1", [
        userEntry("1", "Hello world"),
        assistantEntry("2", "1", "Hi there! How can I help with your TODO list?"),
      ])
    );

    process.argv = ["node", "cli", "grep", "TODO"];
    const { grep } = await import("../commands/grep");
    await grep();

    expect(consoleOutput.some((line) => line.includes("TODO"))).toBe(true);
    expect(consoleOutput.some((line) => line.includes("test-ses"))).toBe(true);
  });

  test("case insensitive search with -i flag", async () => {
    await writeFile(
      join(sessionsDir, "test-session-2.jsonl"),
      createTestSession("test-session-2", [
        userEntry("1", "hello"),
        assistantEntry("2", "1", "HELLO back to you"),
      ])
    );

    // Without -i, should not match lowercase when searching uppercase
    process.argv = ["node", "cli", "grep", "HELLO"];
    const { grep } = await import("../commands/grep");
    consoleOutput = [];
    await grep();
    const withoutI = consoleOutput.filter((line) => line.includes("hello")).length;

    // With -i, should match both
    process.argv = ["node", "cli", "grep", "-i", "HELLO"];
    consoleOutput = [];
    await grep();
    const withI = consoleOutput.filter((line) => line.toLowerCase().includes("hello")).length;

    expect(withI).toBeGreaterThan(withoutI);
  });

  test("count mode with -c flag", async () => {
    await writeFile(
      join(sessionsDir, "test-session-3.jsonl"),
      createTestSession("test-session-3", [
        userEntry("1", "error one"),
        assistantEntry("2", "1", "error two and error three"),
      ])
    );

    process.argv = ["node", "cli", "grep", "-c", "error"];
    const { grep } = await import("../commands/grep");
    await grep();

    // Should output session:count format
    expect(consoleOutput.some((line) => /test-ses.*:\d+/.test(line))).toBe(true);
  });

  test("list mode with -l flag", async () => {
    await writeFile(
      join(sessionsDir, "test-session-4.jsonl"),
      createTestSession("test-session-4", [
        userEntry("1", "find me"),
        assistantEntry("2", "1", "found you"),
      ])
    );

    process.argv = ["node", "cli", "grep", "-l", "find"];
    const { grep } = await import("../commands/grep");
    await grep();

    // Should output only session ID, not the matching line
    expect(consoleOutput.length).toBe(1);
    expect(consoleOutput[0]).toMatch(/^test-ses/);
    expect(consoleOutput[0]).not.toContain("find me");
  });

  test("max matches with -m flag", async () => {
    await writeFile(
      join(sessionsDir, "test-session-5.jsonl"),
      createTestSession("test-session-5", [
        userEntry("1", "match1"),
        assistantEntry("2", "1", "match2\nmatch3\nmatch4\nmatch5"),
      ])
    );

    process.argv = ["node", "cli", "grep", "-m", "2", "match"];
    const { grep } = await import("../commands/grep");
    await grep();

    // Should stop after 2 matches
    const matchCount = consoleOutput.filter((line) => line.includes("match")).length;
    expect(matchCount).toBe(2);
  });

  test("context lines with -C flag", async () => {
    await writeFile(
      join(sessionsDir, "test-session-6.jsonl"),
      createTestSession("test-session-6", [
        userEntry("1", "line1\nline2\nTARGET\nline4\nline5"),
        assistantEntry("2", "1", "ok"),
      ])
    );

    process.argv = ["node", "cli", "grep", "-C", "1", "TARGET"];
    const { grep } = await import("../commands/grep");
    await grep();

    // Should show context lines around match
    const output = consoleOutput.join("\n");
    expect(output).toContain("line2");
    expect(output).toContain("TARGET");
    expect(output).toContain("line4");
  });

  test("searches thinking blocks", async () => {
    await writeFile(
      join(sessionsDir, "test-session-7.jsonl"),
      createTestSession("test-session-7", [
        userEntry("1", "question"),
        assistantWithThinking("2", "1", "Let me think about SECRET_THOUGHT", "Here is my answer"),
      ])
    );

    process.argv = ["node", "cli", "grep", "SECRET_THOUGHT"];
    const { grep } = await import("../commands/grep");
    await grep();

    expect(consoleOutput.some((line) => line.includes("SECRET_THOUGHT"))).toBe(true);
  });

  test("searches tool inputs", async () => {
    await writeFile(
      join(sessionsDir, "test-session-8.jsonl"),
      createTestSession("test-session-8", [
        userEntry("1", "read a file"),
        assistantWithToolUse("2", "1", "Read", { file_path: "/path/to/SPECIAL_FILE.txt" }),
        userWithToolResult("3", "2", "tool-1", "file contents"),
        assistantEntry("4", "3", "done"),
      ])
    );

    process.argv = ["node", "cli", "grep", "SPECIAL_FILE"];
    const { grep } = await import("../commands/grep");
    await grep();

    expect(consoleOutput.some((line) => line.includes("SPECIAL_FILE"))).toBe(true);
  });

  test("does not search tool-result-only entries by default", async () => {
    // Tool-result-only user entries are skipped by getLogicalEntries
    // because they're displayed merged with the tool_use that triggered them.
    // This test verifies that behavior.
    await writeFile(
      join(sessionsDir, "test-session-9.jsonl"),
      createTestSession("test-session-9", [
        userEntry("1", "run command"),
        assistantWithToolUse("2", "1", "Bash", { command: "ls" }),
        userWithToolResult("3", "2", "tool-1", "UNIQUE_OUTPUT_12345"),
        assistantEntry("4", "3", "done"),
      ])
    );

    process.argv = ["node", "cli", "grep", "UNIQUE_OUTPUT_12345"];
    const { grep } = await import("../commands/grep");
    await grep();

    // Tool result content in tool-result-only entries is not searched by default
    expect(consoleOutput.some((line) => line.includes("UNIQUE_OUTPUT_12345"))).toBe(false);
  });

  test("searches tool results with --include-tool-results flag", async () => {
    await writeFile(
      join(sessionsDir, "test-session-10.jsonl"),
      createTestSession("test-session-10", [
        userEntry("1", "run command"),
        assistantWithToolUse("2", "1", "Bash", { command: "ls" }),
        userWithToolResult("3", "2", "tool-1", "SEARCHABLE_OUTPUT_67890"),
        assistantEntry("4", "3", "done"),
      ])
    );

    process.argv = ["node", "cli", "grep", "--include-tool-results", "SEARCHABLE_OUTPUT_67890"];
    const { grep } = await import("../commands/grep");
    await grep();

    // With --include-tool-results, tool result content IS searched
    expect(consoleOutput.some((line) => line.includes("SEARCHABLE_OUTPUT_67890"))).toBe(true);
  });

  test("filters to specific session with -s flag", async () => {
    // Create two sessions
    await writeFile(
      join(sessionsDir, "session-aaa111.jsonl"),
      createTestSession("session-aaa111", [
        userEntry("1", "FINDME in aaa"),
        assistantEntry("2", "1", "response"),
      ])
    );
    await writeFile(
      join(sessionsDir, "session-bbb222.jsonl"),
      createTestSession("session-bbb222", [
        userEntry("1", "FINDME in bbb"),
        assistantEntry("2", "1", "response"),
      ])
    );

    process.argv = ["node", "cli", "grep", "-s", "session-aaa", "FINDME"];
    const { grep } = await import("../commands/grep");
    await grep();

    // Should find in aaa session only
    expect(consoleOutput.some((line) => line.includes("aaa"))).toBe(true);
    expect(consoleOutput.some((line) => line.includes("bbb"))).toBe(false);
  });

  test("skips agent sessions", async () => {
    // Regular session
    await writeFile(
      join(sessionsDir, "regular-session.jsonl"),
      createTestSession("regular-session", [
        userEntry("1", "FINDME regular"),
        assistantEntry("2", "1", "response"),
      ])
    );

    // Agent session (should be skipped)
    await writeFile(
      join(sessionsDir, "agent-abc123.jsonl"),
      createTestSession("agent-abc123", [
        userEntry("1", "FINDME agent"),
        assistantEntry("2", "1", "response"),
      ], { agentId: "abc123" })
    );

    process.argv = ["node", "cli", "grep", "FINDME"];
    const { grep } = await import("../commands/grep");
    await grep();

    // Should find regular session
    expect(consoleOutput.some((line) => line.includes("regular"))).toBe(true);
    // Should NOT find agent session content
    expect(consoleOutput.some((line) => line.includes("agent"))).toBe(false);
  });

  test("handles regex patterns", async () => {
    await writeFile(
      join(sessionsDir, "test-session-10.jsonl"),
      createTestSession("test-session-10", [
        userEntry("1", "error123 and error456"),
        assistantEntry("2", "1", "ok"),
      ])
    );

    process.argv = ["node", "cli", "grep", "error\\d+"];
    const { grep } = await import("../commands/grep");
    await grep();

    expect(consoleOutput.some((line) => line.includes("error123"))).toBe(true);
  });

  test("shows usage when no pattern provided", async () => {
    process.argv = ["node", "cli", "grep"];
    const { grep } = await import("../commands/grep");
    await grep();

    expect(consoleOutput.some((line) => line.includes("Usage"))).toBe(true);
  });
});

describe("read command", () => {
  let tempDir: string;
  let sessionsDir: string;
  let originalCwd: () => string;
  let originalArgv: Array<string>;
  let consoleOutput: Array<string>;
  let consoleSpy: ReturnType<typeof spyOn>;

  beforeEach(async () => {
    tempDir = join(tmpdir(), `hive-test-${Date.now()}-${Math.random().toString(36).slice(2)}`);
    sessionsDir = join(tempDir, ".claude", "hive-mind", "sessions");
    await mkdir(sessionsDir, { recursive: true });

    originalCwd = process.cwd;
    process.cwd = () => tempDir;
    originalArgv = process.argv;

    consoleOutput = [];
    consoleSpy = spyOn(console, "log").mockImplementation((...args: Array<unknown>) => {
      consoleOutput.push(args.map(String).join(" "));
    });
  });

  afterEach(async () => {
    process.cwd = originalCwd;
    process.argv = originalArgv;
    consoleSpy.mockRestore();
    await rm(tempDir, { recursive: true });
  });

  test("reads all entries truncated by default", async () => {
    await writeFile(
      join(sessionsDir, "read-test-1.jsonl"),
      createTestSession("read-test-1", [
        userEntry("1", "line1\nline2\nline3\nline4\nline5"),
        assistantEntry("2", "1", "response"),
      ])
    );

    process.argv = ["node", "cli", "read", "read-tes"];
    const { read } = await import("../commands/read");
    await read();

    const output = consoleOutput.join("\n");
    // Should show truncation indicator (format is "+Nlines")
    expect(output).toMatch(/\+\d+lines/);
  });

  test("reads all entries full with --full flag", async () => {
    await writeFile(
      join(sessionsDir, "read-test-2.jsonl"),
      createTestSession("read-test-2", [
        userEntry("1", "line1\nline2\nline3"),
        assistantEntry("2", "1", "response"),
      ])
    );

    process.argv = ["node", "cli", "read", "read-test-2", "--full"];
    const { read } = await import("../commands/read");
    await read();

    const output = consoleOutput.join("\n");
    // Should show all lines without truncation
    expect(output).toContain("line1");
    expect(output).toContain("line2");
    expect(output).toContain("line3");
    expect(output).not.toContain("[+");
  });

  test("reads specific entry by number", async () => {
    await writeFile(
      join(sessionsDir, "read-test-3.jsonl"),
      createTestSession("read-test-3", [
        userEntry("1", "first entry"),
        assistantEntry("2", "1", "second entry"),
        userEntry("3", "third entry"),
        assistantEntry("4", "3", "fourth entry"),
      ])
    );

    process.argv = ["node", "cli", "read", "read-test-3", "2"];
    const { read } = await import("../commands/read");
    await read();

    const output = consoleOutput.join("\n");
    expect(output).toContain("second entry");
  });

  test("context before and after with -C flag", async () => {
    await writeFile(
      join(sessionsDir, "read-test-4.jsonl"),
      createTestSession("read-test-4", [
        userEntry("1", "entry one"),
        assistantEntry("2", "1", "entry two"),
        userEntry("3", "entry three TARGET"),
        assistantEntry("4", "3", "entry four"),
        userEntry("5", "entry five"),
        assistantEntry("6", "5", "entry six"),
      ])
    );

    process.argv = ["node", "cli", "read", "read-test-4", "3", "-C", "1"];
    const { read } = await import("../commands/read");
    await read();

    const output = consoleOutput.join("\n");
    // Should show entry 2, 3 (target), and 4
    expect(output).toContain("entry two");
    expect(output).toContain("TARGET");
    expect(output).toContain("entry four");
    // Should NOT show entry 1 or 5
    expect(output).not.toContain("entry one");
    expect(output).not.toContain("entry five");
  });

  test("context before with -B flag", async () => {
    await writeFile(
      join(sessionsDir, "read-test-5.jsonl"),
      createTestSession("read-test-5", [
        userEntry("1", "entry one"),
        assistantEntry("2", "1", "entry two"),
        userEntry("3", "entry three TARGET"),
        assistantEntry("4", "3", "entry four"),
      ])
    );

    process.argv = ["node", "cli", "read", "read-test-5", "3", "-B", "2"];
    const { read } = await import("../commands/read");
    await read();

    const output = consoleOutput.join("\n");
    // Should show entries 1, 2, 3 (target)
    expect(output).toContain("entry one");
    expect(output).toContain("entry two");
    expect(output).toContain("TARGET");
    // Should NOT show entry 4
    expect(output).not.toContain("entry four");
  });

  test("context after with -A flag", async () => {
    await writeFile(
      join(sessionsDir, "read-test-6.jsonl"),
      createTestSession("read-test-6", [
        userEntry("1", "entry one"),
        assistantEntry("2", "1", "entry two"),
        userEntry("3", "entry three TARGET"),
        assistantEntry("4", "3", "entry four"),
        userEntry("5", "entry five"),
        assistantEntry("6", "5", "entry six"),
      ])
    );

    // Note: Using different values for entry number (3) and -A value (2) to avoid
    // a parsing quirk where identical values get filtered together
    process.argv = ["node", "cli", "read", "read-test-6", "3", "-A", "2"];
    const { read } = await import("../commands/read");
    await read();

    const output = consoleOutput.join("\n");
    // Should show entries 3 (target), 4, 5
    expect(output).toContain("TARGET");
    expect(output).toContain("entry four");
    expect(output).toContain("entry five");
    // Should NOT show entries 1, 2, or 6
    expect(output).not.toContain("entry one");
    expect(output).not.toContain("entry two");
    expect(output).not.toContain("entry six");
  });

  test("combined -B and -A flags", async () => {
    await writeFile(
      join(sessionsDir, "read-test-7.jsonl"),
      createTestSession("read-test-7", [
        userEntry("1", "entry one"),
        assistantEntry("2", "1", "entry two"),
        userEntry("3", "entry three TARGET"),
        assistantEntry("4", "3", "entry four"),
        userEntry("5", "entry five"),
        assistantEntry("6", "5", "entry six"),
      ])
    );

    process.argv = ["node", "cli", "read", "read-test-7", "3", "-B", "1", "-A", "2"];
    const { read } = await import("../commands/read");
    await read();

    const output = consoleOutput.join("\n");
    // Should show entries 2, 3 (target), 4, 5
    expect(output).toContain("entry two");
    expect(output).toContain("TARGET");
    expect(output).toContain("entry four");
    expect(output).toContain("entry five");
    // Should NOT show entry 1 or 6
    expect(output).not.toContain("entry one");
    expect(output).not.toContain("entry six");
  });

  test("context flags require entry number", async () => {
    await writeFile(
      join(sessionsDir, "read-test-8.jsonl"),
      createTestSession("read-test-8", [
        userEntry("1", "entry"),
        assistantEntry("2", "1", "response"),
      ])
    );

    // Mock console.error to capture error output
    const errorOutput: Array<string> = [];
    const errorSpy = spyOn(console, "error").mockImplementation((...args: Array<unknown>) => {
      errorOutput.push(args.map(String).join(" "));
    });

    process.argv = ["node", "cli", "read", "read-test-8", "-C", "2"];
    const { read } = await import("../commands/read");
    await read();

    errorSpy.mockRestore();

    // Should show error about context flags requiring entry number
    expect(errorOutput.some((line) => line.includes("require") && line.includes("entry"))).toBe(true);
  });

  test("handles entry number same as flag value (regression test)", async () => {
    // This tests the fix for a bug where entry number "2" was incorrectly filtered
    // when -A value was also "2"
    await writeFile(
      join(sessionsDir, "read-test-regression.jsonl"),
      createTestSession("read-test-regression", [
        userEntry("1", "entry one"),
        assistantEntry("2", "1", "entry two TARGET"),
        userEntry("3", "entry three"),
        assistantEntry("4", "3", "entry four"),
      ])
    );

    process.argv = ["node", "cli", "read", "read-test-regression", "2", "-A", "2"];
    const { read } = await import("../commands/read");
    await read();

    const output = consoleOutput.join("\n");
    // Should correctly read entry 2 with 2 entries after
    expect(output).toContain("TARGET");
    expect(output).toContain("entry three");
    expect(output).toContain("entry four");
  });

  test("session prefix matching", async () => {
    await writeFile(
      join(sessionsDir, "abcd1234-full-session-id.jsonl"),
      createTestSession("abcd1234-full-session-id", [
        userEntry("1", "found by prefix"),
        assistantEntry("2", "1", "response"),
      ])
    );

    process.argv = ["node", "cli", "read", "abcd"];
    const { read } = await import("../commands/read");
    await read();

    const output = consoleOutput.join("\n");
    expect(output).toContain("found by prefix");
  });

  test("reports error for non-existent entry number", async () => {
    await writeFile(
      join(sessionsDir, "read-test-9.jsonl"),
      createTestSession("read-test-9", [
        userEntry("1", "only entry"),
        assistantEntry("2", "1", "response"),
      ])
    );

    const errorOutput: Array<string> = [];
    const errorSpy = spyOn(console, "error").mockImplementation((...args: Array<unknown>) => {
      errorOutput.push(args.map(String).join(" "));
    });

    process.argv = ["node", "cli", "read", "read-test-9", "99"];
    const { read } = await import("../commands/read");
    await read();

    errorSpy.mockRestore();

    expect(errorOutput.some((line) => line.includes("not found"))).toBe(true);
  });

  test("shows usage when no session provided", async () => {
    const errorOutput: Array<string> = [];
    const errorSpy = spyOn(console, "error").mockImplementation((...args: Array<unknown>) => {
      errorOutput.push(args.map(String).join(" "));
    });

    process.argv = ["node", "cli", "read"];
    const { read } = await import("../commands/read");
    await read();

    errorSpy.mockRestore();

    expect(errorOutput.some((line) => line.includes("Usage"))).toBe(true);
  });
});
