import { describe, expect, test } from "bun:test";
import { submissionSchema } from "./server";

const base = {
  token: "id.sig",
  rating: 7,
  triedOrChanged: "I now use worktrees for parallel agents.",
};

describe("submission schema", () => {
  test("accepts a minimal valid submission and defaults optional fields", () => {
    const parsed = submissionSchema.parse(base);
    expect(parsed.improve).toBe("");
    expect(parsed.testimonial).toBe("");
    expect(parsed.name).toBe("");
  });

  test("keeps formula-looking text as plain strings", () => {
    // Defense against spreadsheet formula injection is valueInputOption=RAW at the append layer;
    // the schema must not mangle or reject these.
    for (const prefix of ["=", "+", "-", "@"]) {
      const text = `${prefix}HYPERLINK("https://evil.example")`;
      const parsed = submissionSchema.parse({ ...base, triedOrChanged: text });
      expect(parsed.triedOrChanged).toBe(text);
    }
  });

  test("rejects out-of-range ratings and oversized text", () => {
    expect(submissionSchema.safeParse({ ...base, rating: 11 }).success).toBe(
      false,
    );
    expect(submissionSchema.safeParse({ ...base, rating: -1 }).success).toBe(
      false,
    );
    expect(submissionSchema.safeParse({ ...base, rating: 6.5 }).success).toBe(
      false,
    );
    expect(
      submissionSchema.safeParse({ ...base, triedOrChanged: "" }).success,
    ).toBe(false);
    expect(
      submissionSchema.safeParse({ ...base, triedOrChanged: "x".repeat(5001) })
        .success,
    ).toBe(false);
    expect(
      submissionSchema.safeParse({ ...base, name: "x".repeat(201) }).success,
    ).toBe(false);
  });

  test("rejects missing token or answers", () => {
    expect(submissionSchema.safeParse({ ...base, token: "" }).success).toBe(
      false,
    );
    expect(
      submissionSchema.safeParse({ rating: 5, token: "id.sig" }).success,
    ).toBe(false);
  });
});
