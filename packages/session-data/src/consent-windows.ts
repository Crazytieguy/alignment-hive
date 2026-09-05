/**
 * Consent windows: a session is visible to readers only if its lastModified falls inside a
 * window of BOTH the global and the project consent timelines.
 */

export interface ConsentEvent {
  sessionSharing: boolean;
  timestamp: number;
}

export interface ConsentWindow {
  start: number;
  end: number;
}

/**
 * Windows from an event log. The first opt-in is retroactive (starts at 0, covering legacy
 * data); later opt-ins start at their timestamp; an opt-out closes the open window; an open
 * window ends at Infinity.
 */
export function computeConsentWindows(events: Array<ConsentEvent>): Array<ConsentWindow> {
  const windows: Array<ConsentWindow> = [];
  for (const event of [...events].sort((a, b) => a.timestamp - b.timestamp)) {
    const open = windows.at(-1)?.end === Infinity;
    if (event.sessionSharing && !open) {
      windows.push({ start: windows.length === 0 ? 0 : event.timestamp, end: Infinity });
    } else if (!event.sessionSharing && open) {
      windows[windows.length - 1].end = event.timestamp;
    }
  }
  return windows;
}

/** Whether a timestamp falls within any window (start inclusive, end exclusive). */
export function isInConsentWindow(timestamp: number, windows: Array<ConsentWindow>): boolean {
  return windows.some((w) => timestamp >= w.start && timestamp < w.end);
}

// --- Project identifier utilities ---

export interface ProjectIdentifiers {
  directory?: string;
  gitRemote?: string;
}

export interface ProjectConsentEvent extends ConsentEvent, ProjectIdentifiers {}

export interface ProjectGroup {
  directories: Set<string>;
  gitRemotes: Set<string>;
  events: Array<ProjectConsentEvent>;
}

/**
 * Classify a legacy `project` string into directory or gitRemote.
 * Absolute paths (starting with /) are directories; everything else is a git remote.
 */
export function classifyLegacyProject(project: string): ProjectIdentifiers {
  if (project.startsWith('/')) {
    return { directory: project };
  }
  return { gitRemote: project };
}

/**
 * Extract normalized identifiers from a record that may have new fields,
 * legacy `project` field, or both. Prefers new fields over legacy.
 */
export function extractIdentifiers(record: {
  project?: string;
  directory?: string;
  gitRemote?: string;
}): ProjectIdentifiers {
  if (record.directory || record.gitRemote) {
    return {
      directory: record.directory,
      gitRemote: record.gitRemote,
    };
  }
  if (record.project) {
    return classifyLegacyProject(record.project);
  }
  return {};
}

/** The lookup keys an identifier pair maps to; the single definition of the key encoding. */
function identifierKeys({ directory, gitRemote }: ProjectIdentifiers): Array<string> {
  const keys: Array<string> = [];
  if (directory) keys.push(`dir:${directory}`);
  if (gitRemote) keys.push(`remote:${gitRemote.toLowerCase()}`);
  return keys;
}

/**
 * Group consent events for one user into connected components.
 *
 * Two events belong to the same group if they share any identifier
 * (directory or gitRemote), directly or transitively. This merges
 * consent timelines across identifiers so that consenting by path
 * and later by remote produces one continuous timeline.
 */
export function groupProjectConsentEvents(events: Array<ProjectConsentEvent>): {
  groups: Array<ProjectGroup>;
  lookup: Map<string, number>;
} {
  const groups: Array<ProjectGroup> = [];
  const lookup = new Map<string, number>();

  for (const event of events) {
    const keys = identifierKeys(event);
    const matched = [...new Set(keys.map((k) => lookup.get(k)).filter((i): i is number => i !== undefined))].sort(
      (a, b) => a - b,
    );

    let targetIdx: number;
    if (matched.length === 0) {
      targetIdx = groups.length;
      groups.push({ directories: new Set(), gitRemotes: new Set(), events: [] });
    } else {
      // Merge every other matched group into the lowest-indexed one.
      targetIdx = matched[0];
      const target = groups[targetIdx];
      for (const sourceIdx of matched.slice(1)) {
        const source = groups[sourceIdx];
        for (const d of source.directories) target.directories.add(d);
        for (const r of source.gitRemotes) target.gitRemotes.add(r);
        target.events.push(...source.events);
        for (const [key, val] of lookup) if (val === sourceIdx) lookup.set(key, targetIdx);
        groups[sourceIdx] = { directories: new Set(), gitRemotes: new Set(), events: [] }; // merged away; compacted below
      }
    }

    const target = groups[targetIdx];
    if (event.directory) target.directories.add(event.directory);
    if (event.gitRemote) target.gitRemotes.add(event.gitRemote);
    target.events.push(event);
    for (const key of keys) lookup.set(key, targetIdx);
  }

  // Compact: remove empty (merged) groups, reindex
  const compacted: Array<ProjectGroup> = [];
  const oldToNew = new Map<number, number>();
  for (let i = 0; i < groups.length; i++) {
    if (groups[i].events.length > 0) {
      oldToNew.set(i, compacted.length);
      compacted.push(groups[i]);
    }
  }
  const compactedLookup = new Map<string, number>();
  for (const [key, oldIdx] of lookup) {
    const newIdx = oldToNew.get(oldIdx);
    if (newIdx !== undefined) compactedLookup.set(key, newIdx);
  }

  return { groups: compacted, lookup: compactedLookup };
}

/** Group index for the identifiers, or undefined when nothing matches or they match different groups. */
export function findGroupForIdentifiers(
  lookup: Map<string, number>,
  identifiers: ProjectIdentifiers,
): number | undefined {
  const found = new Set(
    identifierKeys(identifiers)
      .map((k) => lookup.get(k))
      .filter((i) => i !== undefined),
  );
  return found.size === 1 ? [...found][0] : undefined;
}
