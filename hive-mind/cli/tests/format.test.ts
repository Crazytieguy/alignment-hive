import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { describe, expect, test } from "bun:test";
import { parseJsonl } from "../lib/extraction";
import { ReadFieldFilter } from "../lib/field-filter";
import { formatSession, redactMultiline } from "../lib/format";
import { parseKnownEntry } from "../lib/schemas";
import type { KnownEntry } from "../lib/schemas";

// Tests run from hive-mind/, but sessions are at repo root
const repoRoot = dirname(dirname(dirname(import.meta.dir)));
const sessionsDir = join(repoRoot, ".claude", "hive-mind", "sessions");
const snapshotsDir = join(import.meta.dir, "__snapshots__");

const TEST_SESSIONS = [
  { prefix: "agent-ac1684a", name: "agent-ac1684a-2-entries" },
  { prefix: "agent-a6b700c", name: "agent-a6b700c-9-entries" },
  { prefix: "agent-a78d046", name: "agent-a78d046-15-entries" },
  { prefix: "agent-aaf8774", name: "agent-aaf8774-orphan-38-entries" },
  { prefix: "efbbb724", name: "efbbb724-with-thinking-57-entries" },
  { prefix: "cb6aa757", name: "cb6aa757-with-summary-38-entries" },
  { prefix: "f968233b", name: "f968233b-41-entries" },
  { prefix: "5e41ef2f", name: "5e41ef2f-no-summary-67-entries" },
];

async function loadSessionEntries(sessionPrefix: string): Promise<Array<KnownEntry>> {
  const files = await readdir(sessionsDir);
  const match = files.find(f => f.startsWith(sessionPrefix) && f.endsWith(".jsonl"));
  if (!match) throw new Error(`No session matching ${sessionPrefix}`);

  const content = await readFile(join(sessionsDir, match), "utf-8");
  const lines = Array.from(parseJsonl(content));
  const rawEntries = lines.slice(1); // Skip metadata

  const entries: Array<KnownEntry> = [];
  for (const raw of rawEntries) {
    const result = parseKnownEntry(raw);
    if (result.data) {
      entries.push(result.data);
    }
  }
  return entries;
}

async function formatFullSession(sessionPrefix: string, redact = false): Promise<string> {
  const entries = await loadSessionEntries(sessionPrefix);
  return formatSession(entries, { redact });
}

async function readSnapshot(name: string): Promise<string | null> {
  try {
    return await readFile(join(snapshotsDir, `${name}.txt`), "utf-8");
  } catch {
    return null;
  }
}

async function writeSnapshot(name: string, content: string): Promise<void> {
  await mkdir(snapshotsDir, { recursive: true });
  await writeFile(join(snapshotsDir, `${name}.txt`), content);
}

describe("redactMultiline", () => {
  test("single line passes through unchanged", () => {
    expect(redactMultiline("hello world")).toBe("hello world");
  });

  test("empty string passes through", () => {
    expect(redactMultiline("")).toBe("");
  });

  test("two lines shows first line + count", () => {
    expect(redactMultiline("line1\nline2")).toBe("line1\n[+1 lines]");
  });

  test("many lines shows first line + count", () => {
    const input = "first line\nsecond\nthird\nfourth\nfifth";
    expect(redactMultiline(input)).toBe("first line\n[+4 lines]");
  });

  test("handles trailing newline", () => {
    expect(redactMultiline("line1\nline2\n")).toBe("line1\n[+2 lines]");
  });
});

describe("format full sessions", () => {
  for (const { prefix, name } of TEST_SESSIONS) {
    test(name, async () => {
      const output = await formatFullSession(prefix);
      const existing = await readSnapshot(name);

      if (existing === null || process.env.UPDATE_SNAPSHOTS) {
        await writeSnapshot(name, output);
        if (existing === null) {
          console.log(`Created snapshot: ${name}.txt`);
        }
      } else {
        expect(output).toBe(existing);
      }
    });
  }
});

describe("format redacted sessions", () => {
  // Test all sessions with redaction for comparison
  const REDACTED_SESSIONS = [
    { prefix: "agent-ac1684a", name: "agent-ac1684a-2-entries-redacted" },
    { prefix: "agent-a6b700c", name: "agent-a6b700c-9-entries-redacted" },
    { prefix: "agent-a78d046", name: "agent-a78d046-15-entries-redacted" },
    { prefix: "agent-aaf8774", name: "agent-aaf8774-orphan-38-entries-redacted" },
    { prefix: "efbbb724", name: "efbbb724-with-thinking-57-entries-redacted" },
    { prefix: "cb6aa757", name: "cb6aa757-with-summary-38-entries-redacted" },
    { prefix: "f968233b", name: "f968233b-41-entries-redacted" },
    { prefix: "5e41ef2f", name: "5e41ef2f-no-summary-67-entries-redacted" },
  ];

  for (const { prefix, name } of REDACTED_SESSIONS) {
    test(name, async () => {
      const output = await formatFullSession(prefix, true);
      const existing = await readSnapshot(name);

      if (existing === null || process.env.UPDATE_SNAPSHOTS) {
        await writeSnapshot(name, output);
        if (existing === null) {
          console.log(`Created snapshot: ${name}.txt`);
        }
      } else {
        expect(output).toBe(existing);
      }
    });
  }
});

describe("format with field filtering", () => {
  // Use a session with thinking blocks for testing field filters
  const SESSION_WITH_THINKING = { prefix: "efbbb724", name: "efbbb724" };

  async function formatWithFilter(
    sessionPrefix: string,
    show: Array<string>,
    hide: Array<string>
  ): Promise<string> {
    const entries = await loadSessionEntries(sessionPrefix);
    const fieldFilter = new ReadFieldFilter(show, hide);
    return formatSession(entries, { redact: true, fieldFilter });
  }

  test("show thinking expands thinking content", async () => {
    const output = await formatWithFilter(SESSION_WITH_THINKING.prefix, ["thinking"], []);
    const name = `${SESSION_WITH_THINKING.name}-show-thinking`;
    const existing = await readSnapshot(name);

    if (existing === null || process.env.UPDATE_SNAPSHOTS) {
      await writeSnapshot(name, output);
      if (existing === null) {
        console.log(`Created snapshot: ${name}.txt`);
      }
    } else {
      expect(output).toBe(existing);
    }
  });

  test("hide user removes user entries", async () => {
    const output = await formatWithFilter(SESSION_WITH_THINKING.prefix, [], ["user"]);
    const name = `${SESSION_WITH_THINKING.name}-hide-user`;
    const existing = await readSnapshot(name);

    if (existing === null || process.env.UPDATE_SNAPSHOTS) {
      await writeSnapshot(name, output);
      if (existing === null) {
        console.log(`Created snapshot: ${name}.txt`);
      }
    } else {
      expect(output).toBe(existing);
    }
  });

  test("hide thinking removes thinking entries", async () => {
    const output = await formatWithFilter(SESSION_WITH_THINKING.prefix, [], ["thinking"]);
    const name = `${SESSION_WITH_THINKING.name}-hide-thinking`;
    const existing = await readSnapshot(name);

    if (existing === null || process.env.UPDATE_SNAPSHOTS) {
      await writeSnapshot(name, output);
      if (existing === null) {
        console.log(`Created snapshot: ${name}.txt`);
      }
    } else {
      expect(output).toBe(existing);
    }
  });
});
