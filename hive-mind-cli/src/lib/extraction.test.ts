import { describe, expect, test } from "bun:test";
import { parseJsonl } from "./extraction";

describe("parseJsonl", () => {
  test("parses valid JSONL lines", () => {
    const content = `{"type":"user","uuid":"abc"}
{"type":"assistant","uuid":"def"}`;
    const entries = [...parseJsonl(content)];
    expect(entries).toHaveLength(2);
    expect(entries[0]).toEqual({ type: "user", uuid: "abc" });
    expect(entries[1]).toEqual({ type: "assistant", uuid: "def" });
  });

  test("skips empty lines", () => {
    const content = `{"type":"user"}

{"type":"assistant"}
`;
    const entries = [...parseJsonl(content)];
    expect(entries).toHaveLength(2);
  });

  test("skips malformed lines", () => {
    const content = `{"type":"user"}
this is not json
{"type":"assistant"}`;
    const entries = [...parseJsonl(content)];
    expect(entries).toHaveLength(2);
  });
});

describe("transformEntry", () => {
  // We need to import the internal function for testing
  // For now, let's test through the public extractSession function
  // by creating test fixtures

  test("strips file-history-snapshot entries", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, rm } = await import("node:fs/promises");
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      await writeFile(
        rawPath,
        [
          JSON.stringify({
            type: "user",
            uuid: "1",
            parentUuid: null,
            timestamp: "2025-01-01",
            message: { role: "user", content: "hello" },
          }),
          JSON.stringify({ type: "file-history-snapshot", messageId: "1" }),
          JSON.stringify({
            type: "assistant",
            uuid: "2",
            parentUuid: "1",
            timestamp: "2025-01-01",
            message: { role: "assistant", content: "hi" },
          }),
        ].join("\n"),
      );

      const result = await extractSession({ rawPath, outputPath: outPath });
      expect(result.messageCount).toBe(2); // user + assistant, not file-history-snapshot
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("strips queue-operation entries", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, rm } = await import("node:fs/promises");
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      await writeFile(
        rawPath,
        [
          JSON.stringify({
            type: "user",
            uuid: "1",
            parentUuid: null,
            timestamp: "2025-01-01",
            message: { role: "user", content: "hello" },
          }),
          JSON.stringify({
            type: "queue-operation",
            operation: "enqueue",
            content: "test",
          }),
        ].join("\n"),
      );

      const result = await extractSession({ rawPath, outputPath: outPath });
      expect(result.messageCount).toBe(1);
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });
});

describe("tool result transformation", () => {
  test("Bash tool: keeps command, stdout, stderr, exitCode", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: { role: "user", content: "ran bash" },
        toolUseResult: {
          command: "ls -la",
          stdout: "file1.txt\nfile2.txt",
          stderr: "",
          exitCode: 0,
          interrupted: false,
          // These should be stripped
          duration: 150,
          cwd: "/some/path",
        },
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      expect(extracted.toolUseResult.command).toBe("ls -la");
      expect(extracted.toolUseResult.stdout).toBe("file1.txt\nfile2.txt");
      expect(extracted.toolUseResult.stderr).toBe("");
      expect(extracted.toolUseResult.exitCode).toBe(0);
      expect(extracted.toolUseResult.interrupted).toBe(false);
      expect(extracted.toolUseResult.duration).toBeUndefined();
      expect(extracted.toolUseResult.cwd).toBeUndefined();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("Write tool: keeps filePath and content", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: { role: "user", content: "wrote file" },
        toolUseResult: {
          filePath: "/path/to/new-file.ts",
          content: "export const foo = 'bar';",
          // These should be stripped
          success: true,
          bytesWritten: 25,
        },
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      expect(extracted.toolUseResult.filePath).toBe("/path/to/new-file.ts");
      expect(extracted.toolUseResult.content).toBe("export const foo = 'bar';");
      expect(extracted.toolUseResult.success).toBeUndefined();
      expect(extracted.toolUseResult.bytesWritten).toBeUndefined();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("Glob tool: keeps filenames, numFiles, truncated", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: { role: "user", content: "glob result" },
        toolUseResult: {
          filenames: ["file1.ts", "file2.ts"],
          numFiles: 2,
          truncated: false,
          // Should be stripped
          pattern: "**/*.ts",
          duration: 50,
        },
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      expect(extracted.toolUseResult.filenames).toEqual([
        "file1.ts",
        "file2.ts",
      ]);
      expect(extracted.toolUseResult.numFiles).toBe(2);
      expect(extracted.toolUseResult.truncated).toBe(false);
      expect(extracted.toolUseResult.pattern).toBeUndefined();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("Grep tool: keeps filenames, content, numFiles", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: { role: "user", content: "grep result" },
        toolUseResult: {
          filenames: ["src/index.ts"],
          content: "1:import foo from 'bar';",
          numFiles: 1,
          // Should be stripped
          pattern: "import.*foo",
        },
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      expect(extracted.toolUseResult.filenames).toEqual(["src/index.ts"]);
      expect(extracted.toolUseResult.content).toBe("1:import foo from 'bar';");
      expect(extracted.toolUseResult.numFiles).toBe(1);
      expect(extracted.toolUseResult.pattern).toBeUndefined();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("WebFetch tool: keeps url, prompt, content", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: { role: "user", content: "fetched url" },
        toolUseResult: {
          url: "https://example.com/docs",
          prompt: "Extract the API docs",
          content: "# API Documentation\n...",
          // Should be stripped
          statusCode: 200,
          headers: { "content-type": "text/html" },
        },
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      expect(extracted.toolUseResult.url).toBe("https://example.com/docs");
      expect(extracted.toolUseResult.prompt).toBe("Extract the API docs");
      expect(extracted.toolUseResult.content).toBe("# API Documentation\n...");
      expect(extracted.toolUseResult.statusCode).toBeUndefined();
      expect(extracted.toolUseResult.headers).toBeUndefined();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("WebSearch tool: keeps query and results", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: { role: "user", content: "searched web" },
        toolUseResult: {
          query: "typescript best practices",
          results: [
            {
              title: "TypeScript Handbook",
              url: "https://typescriptlang.org/docs",
            },
          ],
          // Should be stripped
          totalResults: 1000000,
        },
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      expect(extracted.toolUseResult.query).toBe("typescript best practices");
      expect(extracted.toolUseResult.results).toEqual([
        {
          title: "TypeScript Handbook",
          url: "https://typescriptlang.org/docs",
        },
      ]);
      expect(extracted.toolUseResult.totalResults).toBeUndefined();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("Task tool: keeps agentId, prompt, status, content", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: { role: "user", content: "task result" },
        toolUseResult: {
          agentId: "agent-123",
          prompt: "Search the codebase",
          status: "completed",
          content: "Found 5 relevant files...",
          // Should be stripped
          duration: 5000,
          toolCalls: 15,
        },
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      expect(extracted.toolUseResult.agentId).toBe("agent-123");
      expect(extracted.toolUseResult.prompt).toBe("Search the codebase");
      expect(extracted.toolUseResult.status).toBe("completed");
      expect(extracted.toolUseResult.content).toBe("Found 5 relevant files...");
      expect(extracted.toolUseResult.duration).toBeUndefined();
      expect(extracted.toolUseResult.toolCalls).toBeUndefined();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("Unknown tool: passes through all fields", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: { role: "user", content: "unknown tool" },
        toolUseResult: {
          customField1: "value1",
          customField2: 42,
          nested: { a: 1, b: 2 },
        },
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      // Unknown tools pass through unchanged
      expect(extracted.toolUseResult.customField1).toBe("value1");
      expect(extracted.toolUseResult.customField2).toBe(42);
      expect(extracted.toolUseResult.nested).toEqual({ a: 1, b: 2 });
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("Read tool: strips file content, keeps metadata", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: {
          role: "user",
          content: [
            {
              type: "tool_result",
              tool_use_id: "t1",
              content: "file content here",
            },
          ],
        },
        toolUseResult: {
          file: {
            filePath: "/path/to/file.txt",
            content: "this should be stripped because its large file content",
            numLines: 100,
            totalLines: 100,
          },
          isImage: false,
        },
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      expect(extracted.toolUseResult.file.filePath).toBe("/path/to/file.txt");
      expect(extracted.toolUseResult.file.numLines).toBe(100);
      expect(extracted.toolUseResult.file.content).toBeUndefined();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("Edit tool: strips originalFile, keeps structuredPatch", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: { role: "user", content: "edited file" },
        toolUseResult: {
          filePath: "/path/to/file.txt",
          oldString: "old",
          newString: "new",
          originalFile:
            "this is the entire original file content that should be stripped",
          structuredPatch: [
            { oldStart: 1, newStart: 1, lines: ["-old", "+new"] },
          ],
        },
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      expect(extracted.toolUseResult.filePath).toBe("/path/to/file.txt");
      expect(extracted.toolUseResult.oldString).toBe("old");
      expect(extracted.toolUseResult.newString).toBe("new");
      expect(extracted.toolUseResult.structuredPatch).toBeDefined();
      expect(extracted.toolUseResult.originalFile).toBeUndefined();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });
});

describe("content block transformation", () => {
  test("replaces image base64 with size placeholder", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      // Create a fake base64 image (100 chars = ~75 bytes decoded)
      const fakeBase64 = "A".repeat(100);
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: {
          role: "user",
          content: [
            {
              type: "image",
              source: {
                type: "base64",
                media_type: "image/png",
                data: fakeBase64,
              },
            },
            { type: "text", text: "here is an image" },
          ],
        },
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      const imageBlock = extracted.message.content[0];
      expect(imageBlock.type).toBe("image");
      expect(imageBlock.size).toBe(75); // 100 * 3/4
      expect(imageBlock.source).toBeUndefined();
      expect(imageBlock.data).toBeUndefined();

      // Text block should be unchanged
      expect(extracted.message.content[1]).toEqual({
        type: "text",
        text: "here is an image",
      });
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("replaces document base64 with size placeholder", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const fakeBase64 = "B".repeat(200);
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: {
          role: "user",
          content: [
            {
              type: "document",
              source: {
                type: "base64",
                media_type: "application/pdf",
                data: fakeBase64,
              },
            },
          ],
        },
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      const docBlock = extracted.message.content[0];
      expect(docBlock.type).toBe("document");
      expect(docBlock.media_type).toBe("application/pdf");
      expect(docBlock.size).toBe(150); // 200 * 3/4
      expect(docBlock.source).toBeUndefined();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });
});

describe("metadata stripping", () => {
  test("strips requestId, slug, userType from entries", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        sessionId: "sess-123",
        cwd: "/home/user",
        gitBranch: "main",
        version: "2.0.76",
        userType: "external",
        slug: "my-session",
        requestId: "req-123",
        message: {
          role: "user",
          content: "hello",
          id: "msg-123",
          usage: { input: 100, output: 50 },
        },
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      // Should keep these
      expect(extracted.uuid).toBe("1");
      expect(extracted.sessionId).toBe("sess-123");
      expect(extracted.cwd).toBe("/home/user");

      // Should strip these
      expect(extracted.requestId).toBeUndefined();
      expect(extracted.slug).toBeUndefined();
      expect(extracted.userType).toBeUndefined();
      expect(extracted.message.id).toBeUndefined();
      expect(extracted.message.usage).toBeUndefined();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });
});

describe("metadata line", () => {
  test("writes correct metadata as first line", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      await writeFile(
        rawPath,
        [
          JSON.stringify({
            type: "summary",
            summary: "Test session",
            leafUuid: "2",
          }),
          JSON.stringify({
            type: "user",
            uuid: "1",
            parentUuid: null,
            timestamp: "2025-01-01",
            message: { role: "user", content: "hello" },
          }),
          JSON.stringify({
            type: "assistant",
            uuid: "2",
            parentUuid: "1",
            timestamp: "2025-01-01",
            message: { role: "assistant", content: "hi" },
          }),
        ].join("\n"),
      );

      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const meta = JSON.parse(lines[0]);

      expect(meta._type).toBe("hive-mind-meta");
      expect(meta.version).toBe("0.1");
      expect(meta.sessionId).toBe("test-session");
      expect(meta.machineId).toBeDefined();
      expect(typeof meta.machineId).toBe("string");
      expect(meta.machineId.length).toBeGreaterThan(0);
      expect(meta.messageCount).toBe(3); // summary + user + assistant
      expect(meta.summary).toBe("Test session");
      expect(meta.rawPath).toBe(rawPath);
      expect(meta.extractedAt).toBeDefined();
      expect(meta.rawMtime).toBeDefined();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });
});

describe("summary validation", () => {
  test("uses summary with matching leafUuid", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      await writeFile(
        rawPath,
        [
          // Contaminated summary (leafUuid doesn't exist in this file)
          JSON.stringify({
            type: "summary",
            summary: "Wrong session",
            leafUuid: "nonexistent",
          }),
          // Valid summary (leafUuid exists)
          JSON.stringify({
            type: "summary",
            summary: "Correct session",
            leafUuid: "2",
          }),
          JSON.stringify({
            type: "user",
            uuid: "1",
            parentUuid: null,
            timestamp: "2025-01-01",
            message: { role: "user", content: "hello" },
          }),
          JSON.stringify({
            type: "assistant",
            uuid: "2",
            parentUuid: "1",
            timestamp: "2025-01-01",
            message: { role: "assistant", content: "hi" },
          }),
        ].join("\n"),
      );

      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const meta = JSON.parse(lines[0]);

      expect(meta.summary).toBe("Correct session");
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });
});

describe("system entries", () => {
  test("keeps system entries with content and level", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      await writeFile(
        rawPath,
        [
          JSON.stringify({
            type: "system",
            subtype: "error",
            uuid: "sys-1",
            parentUuid: null,
            timestamp: "2025-01-01",
            content: "An error occurred",
            level: "error",
          }),
          JSON.stringify({
            type: "user",
            uuid: "1",
            parentUuid: "sys-1",
            timestamp: "2025-01-01",
            message: { role: "user", content: "hello" },
          }),
        ].join("\n"),
      );

      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      expect(lines.length).toBe(3); // meta + system + user

      const systemEntry = JSON.parse(lines[1]);
      expect(systemEntry.type).toBe("system");
      expect(systemEntry.subtype).toBe("error");
      expect(systemEntry.content).toBe("An error occurred");
      expect(systemEntry.level).toBe("error");
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });
});

describe("edge cases", () => {
  test("handles empty session file", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      await writeFile(rawPath, "");
      const result = await extractSession({ rawPath, outputPath: outPath });

      expect(result.messageCount).toBe(0);
      expect(result.summary).toBeUndefined();

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      // Only metadata line
      expect(lines.length).toBe(1);
      const meta = JSON.parse(lines[0]);
      expect(meta.messageCount).toBe(0);
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("handles null toolUseResult", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: { role: "user", content: "hello" },
        toolUseResult: null,
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      expect(extracted.type).toBe("user");
      // null is preserved as-is (not transformed)
      expect(extracted.toolUseResult).toBeNull();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("handles string toolUseResult (error case)", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: { role: "user", content: "hello" },
        toolUseResult: "Error: something went wrong",
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      // String result should pass through
      expect(extracted.toolUseResult).toBe("Error: something went wrong");
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("handles array toolUseResult", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: { role: "user", content: "hello" },
        toolUseResult: [{ item: 1 }, { item: 2 }],
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      // Array result should pass through (unknown tool)
      expect(extracted.toolUseResult).toEqual([{ item: 1 }, { item: 2 }]);
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("handles message with string content", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: { role: "user", content: "just a simple string message" },
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      expect(extracted.message.content).toBe("just a simple string message");
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("handles base64 with padding in size calculation", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      // "Hello" encodes to "SGVsbG8=" (8 chars with 1 padding = 5 bytes decoded)
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: {
          role: "user",
          content: [
            {
              type: "image",
              source: {
                type: "base64",
                media_type: "image/png",
                data: "SGVsbG8=",
              },
            },
          ],
        },
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      // (8 - 1) * 3 / 4 = 5.25 → floor = 5
      expect(extracted.message.content[0].size).toBe(5);
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("handles nested tool_result with base64 content", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      const fakeBase64 = "A".repeat(100);
      const userEntry = {
        type: "user",
        uuid: "1",
        parentUuid: null,
        timestamp: "2025-01-01",
        message: {
          role: "user",
          content: [
            {
              type: "tool_result",
              tool_use_id: "t1",
              content: [
                {
                  type: "image",
                  source: {
                    type: "base64",
                    media_type: "image/png",
                    data: fakeBase64,
                  },
                },
                { type: "text", text: "Image result" },
              ],
            },
          ],
        },
      };

      await writeFile(rawPath, JSON.stringify(userEntry));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      const extracted = JSON.parse(lines[1]);

      const toolResult = extracted.message.content[0];
      expect(toolResult.type).toBe("tool_result");
      expect(toolResult.content[0].type).toBe("image");
      expect(toolResult.content[0].size).toBe(75);
      expect(toolResult.content[0].source).toBeUndefined();
      expect(toolResult.content[1]).toEqual({
        type: "text",
        text: "Image result",
      });
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test("skips unknown entry types gracefully", async () => {
    const { extractSession } = await import("./extraction");
    const { mkdtemp, writeFile, readFile, rm } = await import(
      "node:fs/promises"
    );
    const { join } = await import("node:path");
    const { tmpdir } = await import("node:os");

    const tempDir = await mkdtemp(join(tmpdir(), "hive-test-"));
    const rawPath = join(tempDir, "test-session.jsonl");
    const outPath = join(tempDir, "out", "test-session.jsonl");

    try {
      await writeFile(
        rawPath,
        [
          JSON.stringify({ type: "unknown-future-type", data: "whatever" }),
          JSON.stringify({
            type: "user",
            uuid: "1",
            parentUuid: null,
            timestamp: "2025-01-01",
            message: { role: "user", content: "hello" },
          }),
          JSON.stringify({ type: "another-unknown", foo: "bar" }),
        ].join("\n"),
      );

      const result = await extractSession({ rawPath, outputPath: outPath });
      // Only the user entry should be extracted
      expect(result.messageCount).toBe(1);

      const output = await readFile(outPath, "utf-8");
      const lines = output.trim().split("\n");
      expect(lines.length).toBe(2); // meta + user
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });
});
