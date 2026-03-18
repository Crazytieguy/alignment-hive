import { describe, expect, it } from "bun:test";
import {
  classifyLegacyProject,
  computeConsentWindows,
  extractIdentifiers,
  findGroupForIdentifiers,
  groupProjectConsentEvents,
  type ProjectConsentEvent,
} from "./consent-windows";

describe("groupProjectConsentEvents", () => {
  it("returns empty groups for empty input", () => {
    const result = groupProjectConsentEvents([]);
    expect(result.groups).toEqual([]);
    expect(result.lookup.size).toBe(0);
  });

  it("creates one group for a single event with directory only", () => {
    const events: ProjectConsentEvent[] = [
      { directory: "/home/user/project", sessionSharing: true, timestamp: 100 },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(1);
    expect(result.groups[0].directories).toEqual(new Set(["/home/user/project"]));
    expect(result.groups[0].gitRemotes).toEqual(new Set());
    expect(result.groups[0].events).toEqual(events);
    expect(result.lookup.get("dir:/home/user/project")).toBe(0);
  });

  it("creates one group for a single event with gitRemote only", () => {
    const events: ProjectConsentEvent[] = [
      { gitRemote: "github.com/user/repo", sessionSharing: true, timestamp: 100 },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(1);
    expect(result.groups[0].directories).toEqual(new Set());
    expect(result.groups[0].gitRemotes).toEqual(new Set(["github.com/user/repo"]));
    expect(result.groups[0].events).toEqual(events);
    expect(result.lookup.get("remote:github.com/user/repo")).toBe(0);
  });

  it("creates one group for a single event with both directory and gitRemote", () => {
    const events: ProjectConsentEvent[] = [
      {
        directory: "/home/user/project",
        gitRemote: "github.com/user/repo",
        sessionSharing: true,
        timestamp: 100,
      },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(1);
    expect(result.groups[0].directories).toEqual(new Set(["/home/user/project"]));
    expect(result.groups[0].gitRemotes).toEqual(new Set(["github.com/user/repo"]));
    expect(result.groups[0].events).toEqual(events);
    expect(result.lookup.get("dir:/home/user/project")).toBe(0);
    expect(result.lookup.get("remote:github.com/user/repo")).toBe(0);
  });

  it("merges two events with same directory but different remotes into one group", () => {
    const events: ProjectConsentEvent[] = [
      {
        directory: "/home/user/project",
        gitRemote: "github.com/user/repo",
        sessionSharing: true,
        timestamp: 100,
      },
      {
        directory: "/home/user/project",
        gitRemote: "github.com/other/fork",
        sessionSharing: true,
        timestamp: 200,
      },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(1);
    expect(result.groups[0].directories).toEqual(new Set(["/home/user/project"]));
    expect(result.groups[0].gitRemotes).toEqual(
      new Set(["github.com/user/repo", "github.com/other/fork"]),
    );
    expect(result.groups[0].events).toHaveLength(2);
    const idx = result.lookup.get("dir:/home/user/project");
    expect(idx).toBeDefined();
    expect(result.lookup.get("remote:github.com/user/repo")).toBe(idx);
    expect(result.lookup.get("remote:github.com/other/fork")).toBe(idx);
  });

  it("merges two events with same remote but different directories into one group", () => {
    const events: ProjectConsentEvent[] = [
      {
        directory: "/home/user/project-a",
        gitRemote: "github.com/user/repo",
        sessionSharing: true,
        timestamp: 100,
      },
      {
        directory: "/home/user/project-b",
        gitRemote: "github.com/user/repo",
        sessionSharing: false,
        timestamp: 200,
      },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(1);
    expect(result.groups[0].directories).toEqual(
      new Set(["/home/user/project-a", "/home/user/project-b"]),
    );
    expect(result.groups[0].gitRemotes).toEqual(new Set(["github.com/user/repo"]));
    expect(result.groups[0].events).toHaveLength(2);
  });

  it("keeps two unrelated events in separate groups", () => {
    const events: ProjectConsentEvent[] = [
      {
        directory: "/home/user/project-a",
        gitRemote: "github.com/user/repo-a",
        sessionSharing: true,
        timestamp: 100,
      },
      {
        directory: "/home/user/project-b",
        gitRemote: "github.com/user/repo-b",
        sessionSharing: true,
        timestamp: 200,
      },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(2);

    const idxA = result.lookup.get("dir:/home/user/project-a");
    const idxB = result.lookup.get("dir:/home/user/project-b");
    expect(idxA).toBeDefined();
    expect(idxB).toBeDefined();
    expect(idxA).not.toBe(idxB);

    expect(result.lookup.get("remote:github.com/user/repo-a")).toBe(idxA);
    expect(result.lookup.get("remote:github.com/user/repo-b")).toBe(idxB);
  });

  it("merges transitively: A shares dir with B, B shares remote with C", () => {
    const events: ProjectConsentEvent[] = [
      { directory: "/a", sessionSharing: true, timestamp: 100 },
      { directory: "/a", gitRemote: "r1", sessionSharing: true, timestamp: 200 },
      { directory: "/b", gitRemote: "r1", sessionSharing: true, timestamp: 300 },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(1);
    expect(result.groups[0].directories).toEqual(new Set(["/a", "/b"]));
    expect(result.groups[0].gitRemotes).toEqual(new Set(["r1"]));
    expect(result.groups[0].events).toHaveLength(3);

    const idx = result.lookup.get("dir:/a");
    expect(result.lookup.get("dir:/b")).toBe(idx);
    expect(result.lookup.get("remote:r1")).toBe(idx);
  });

  it("groups by shared identifiers regardless of consent state", () => {
    const events: ProjectConsentEvent[] = [
      { directory: "/shared", sessionSharing: true, timestamp: 100 },
      { directory: "/shared", sessionSharing: false, timestamp: 200 },
      { directory: "/other", sessionSharing: true, timestamp: 300 },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(2);
    const sharedIdx = result.lookup.get("dir:/shared");
    const otherIdx = result.lookup.get("dir:/other");
    expect(sharedIdx).not.toBe(otherIdx);
  });

  it("produces correct consent windows when events from a merged group are passed to computeConsentWindows", () => {
    const T1 = 1000;
    const T2 = 2000;
    const T3 = 3000;

    const events: ProjectConsentEvent[] = [
      { directory: "/project", sessionSharing: true, timestamp: T1 },
      { directory: "/project", sessionSharing: false, timestamp: T2 },
      {
        directory: "/project",
        gitRemote: "github.com/user/repo",
        sessionSharing: true,
        timestamp: T3,
      },
    ];

    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(1);

    const windows = computeConsentWindows(result.groups[0].events);
    expect(windows).toEqual([
      { start: 0, end: T2 },
      { start: T3, end: Infinity },
    ]);
  });

  it("handles events with no identifiers", () => {
    const events: ProjectConsentEvent[] = [
      { sessionSharing: true, timestamp: 100 },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(1);
    expect(result.groups[0].directories).toEqual(new Set());
    expect(result.groups[0].gitRemotes).toEqual(new Set());
    expect(result.groups[0].events).toHaveLength(1);
  });

  it("handles complex transitive chains across many events", () => {
    const events: ProjectConsentEvent[] = [
      { directory: "/a", gitRemote: "r1", sessionSharing: true, timestamp: 100 },
      { directory: "/b", gitRemote: "r2", sessionSharing: true, timestamp: 200 },
      { directory: "/b", gitRemote: "r1", sessionSharing: true, timestamp: 300 },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(1);
    expect(result.groups[0].directories).toEqual(new Set(["/a", "/b"]));
    expect(result.groups[0].gitRemotes).toEqual(new Set(["r1", "r2"]));
    expect(result.groups[0].events).toHaveLength(3);
  });
});

describe("findGroupForIdentifiers", () => {
  it("returns undefined for empty lookup", () => {
    const lookup = new Map<string, number>();
    expect(findGroupForIdentifiers(lookup, { directory: "/foo" })).toBeUndefined();
  });

  it("finds group by directory", () => {
    const lookup = new Map<string, number>([
      ["dir:/home/user/project", 0],
      ["remote:github.com/user/repo", 0],
    ]);
    expect(findGroupForIdentifiers(lookup, { directory: "/home/user/project" })).toBe(0);
  });

  it("finds group by gitRemote", () => {
    const lookup = new Map<string, number>([
      ["dir:/home/user/project", 0],
      ["remote:github.com/user/repo", 0],
    ]);
    expect(findGroupForIdentifiers(lookup, { gitRemote: "github.com/user/repo" })).toBe(0);
  });

  it("returns the group index when both identifiers match the same group", () => {
    const lookup = new Map<string, number>([
      ["dir:/home/user/project", 2],
      ["remote:github.com/user/repo", 2],
    ]);
    expect(
      findGroupForIdentifiers(lookup, {
        directory: "/home/user/project",
        gitRemote: "github.com/user/repo",
      }),
    ).toBe(2);
  });

  it("returns undefined when identifiers match different groups (ambiguous)", () => {
    const lookup = new Map<string, number>([
      ["dir:/home/user/project", 0],
      ["remote:github.com/other/repo", 1],
    ]);
    expect(
      findGroupForIdentifiers(lookup, {
        directory: "/home/user/project",
        gitRemote: "github.com/other/repo",
      }),
    ).toBeUndefined();
  });

  it("returns undefined when no identifiers match", () => {
    const lookup = new Map<string, number>([
      ["dir:/home/user/project", 0],
      ["remote:github.com/user/repo", 0],
    ]);
    expect(
      findGroupForIdentifiers(lookup, {
        directory: "/somewhere/else",
        gitRemote: "gitlab.com/other/repo",
      }),
    ).toBeUndefined();
  });

  it("returns undefined when identifiers are empty", () => {
    const lookup = new Map<string, number>([["dir:/home/user/project", 0]]);
    expect(findGroupForIdentifiers(lookup, {})).toBeUndefined();
  });

  it("matches on one identifier when the other is undefined", () => {
    const lookup = new Map<string, number>([
      ["dir:/project-a", 0],
      ["remote:github.com/repo-a", 0],
      ["dir:/project-b", 1],
      ["remote:github.com/repo-b", 1],
    ]);
    expect(findGroupForIdentifiers(lookup, { gitRemote: "github.com/repo-b" })).toBe(1);
  });
});

describe("classifyLegacyProject", () => {
  it("classifies absolute Unix path as directory", () => {
    expect(classifyLegacyProject("/home/user/project")).toEqual({
      directory: "/home/user/project",
    });
  });

  it("classifies root path as directory", () => {
    expect(classifyLegacyProject("/")).toEqual({ directory: "/" });
  });

  it("classifies non-slash string as gitRemote", () => {
    expect(classifyLegacyProject("github.com/user/repo")).toEqual({
      gitRemote: "github.com/user/repo",
    });
  });

  it("classifies SSH-style remote as gitRemote", () => {
    expect(classifyLegacyProject("git@github.com:user/repo.git")).toEqual({
      gitRemote: "git@github.com:user/repo.git",
    });
  });

  it("classifies relative path (no leading slash) as gitRemote", () => {
    expect(classifyLegacyProject("my-project")).toEqual({
      gitRemote: "my-project",
    });
  });
});

describe("extractIdentifiers", () => {
  it("prefers new fields over legacy project", () => {
    expect(
      extractIdentifiers({
        project: "github.com/user/repo",
        directory: "/home/user/project",
        gitRemote: "github.com/other/repo",
      }),
    ).toEqual({
      directory: "/home/user/project",
      gitRemote: "github.com/other/repo",
    });
  });

  it("uses directory alone when gitRemote is absent", () => {
    expect(extractIdentifiers({ directory: "/home/user/project" })).toEqual({
      directory: "/home/user/project",
      gitRemote: undefined,
    });
  });

  it("uses gitRemote alone when directory is absent", () => {
    expect(extractIdentifiers({ gitRemote: "github.com/user/repo" })).toEqual({
      directory: undefined,
      gitRemote: "github.com/user/repo",
    });
  });

  it("falls back to classifyLegacyProject for path-based project", () => {
    expect(extractIdentifiers({ project: "/home/user/project" })).toEqual({
      directory: "/home/user/project",
    });
  });

  it("falls back to classifyLegacyProject for remote-based project", () => {
    expect(extractIdentifiers({ project: "github.com/user/repo" })).toEqual({
      gitRemote: "github.com/user/repo",
    });
  });

  it("returns empty object when no fields are set", () => {
    expect(extractIdentifiers({})).toEqual({});
  });

  it("ignores project when directory is set", () => {
    expect(
      extractIdentifiers({ project: "/old/path", directory: "/new/path" }),
    ).toEqual({ directory: "/new/path", gitRemote: undefined });
  });
});
