/**
 * Consent window computation for determining session visibility.
 *
 * A session is visible to readers only if its lastModified timestamp falls
 * within a consent window for BOTH global and project consent layers.
 *
 * Window rules:
 * - First consent is retroactive: window starts at 0 (covers legacy data)
 * - Subsequent consents start at their timestamp time (gap sessions excluded)
 * - Revocations close the current window at their timestamp time
 * - A currently-active consent has end = Infinity
 */

export interface ConsentEvent {
  sessionSharing: boolean;
  timestamp: number;
}

export interface ConsentWindow {
  start: number;
  end: number; // Infinity if currently active
}

/**
 * Compute consent windows from an event log.
 *
 * Events are sorted by time. The first opt-in opens a window at time 0
 * (retroactive for legacy data). Subsequent opt-ins open windows at their
 * timestamp time. Opt-outs close the current window.
 */
export function computeConsentWindows(
  events: ConsentEvent[],
): ConsentWindow[] {
  const sorted = [...events].sort((a, b) => a.timestamp - b.timestamp);
  const windows: ConsentWindow[] = [];
  let isOpen = false;
  let isFirst = true;

  for (const event of sorted) {
    if (event.sessionSharing && !isOpen) {
      windows.push({
        start: isFirst ? 0 : event.timestamp,
        end: Infinity,
      });
      isOpen = true;
      isFirst = false;
    } else if (!event.sessionSharing && isOpen) {
      // Close the current window
      windows[windows.length - 1].end = event.timestamp;
      isOpen = false;
    }
  }

  return windows;
}

/**
 * Check if a timestamp falls within any consent window.
 */
export function isInConsentWindow(
  timestamp: number,
  windows: ConsentWindow[],
): boolean {
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
  events: ProjectConsentEvent[];
}

/**
 * Classify a legacy `project` string into directory or gitRemote.
 * Absolute paths (starting with /) are directories; everything else is a git remote.
 */
export function classifyLegacyProject(project: string): ProjectIdentifiers {
  if (project.startsWith("/")) {
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

/**
 * Group consent events for one user into connected components.
 *
 * Two events belong to the same group if they share any identifier
 * (directory or gitRemote), directly or transitively. This merges
 * consent timelines across identifiers so that consenting by path
 * and later by remote produces one continuous timeline.
 */
export function groupProjectConsentEvents(events: ProjectConsentEvent[]): {
  groups: ProjectGroup[];
  lookup: Map<string, number>;
} {
  const groups: ProjectGroup[] = [];
  const lookup = new Map<string, number>();

  for (const event of events) {
    const keys: string[] = [];
    if (event.directory) keys.push(`dir:${event.directory}`);
    if (event.gitRemote) keys.push(`remote:${event.gitRemote}`);

    // Find all existing groups that match any of this event's identifiers
    const matchedGroupIndices = new Set<number>();
    for (const key of keys) {
      const idx = lookup.get(key);
      if (idx !== undefined) matchedGroupIndices.add(idx);
    }

    if (matchedGroupIndices.size === 0) {
      // New group
      const idx = groups.length;
      groups.push({
        directories: new Set(event.directory ? [event.directory] : []),
        gitRemotes: new Set(event.gitRemote ? [event.gitRemote] : []),
        events: [event],
      });
      for (const key of keys) lookup.set(key, idx);
    } else if (matchedGroupIndices.size === 1) {
      // Add to existing group
      const idx = [...matchedGroupIndices][0];
      const group = groups[idx];
      if (event.directory) group.directories.add(event.directory);
      if (event.gitRemote) group.gitRemotes.add(event.gitRemote);
      group.events.push(event);
      for (const key of keys) lookup.set(key, idx);
    } else {
      // Merge multiple groups — pick the lowest index as target
      const indices = [...matchedGroupIndices].sort((a, b) => a - b);
      const targetIdx = indices[0];
      const target = groups[targetIdx];

      for (let i = 1; i < indices.length; i++) {
        const sourceIdx = indices[i];
        const source = groups[sourceIdx];
        for (const d of source.directories) target.directories.add(d);
        for (const r of source.gitRemotes) target.gitRemotes.add(r);
        target.events.push(...source.events);
        // Redirect all lookup entries from source to target
        for (const [key, val] of lookup) {
          if (val === sourceIdx) lookup.set(key, targetIdx);
        }
        // Mark source as merged (empty)
        groups[sourceIdx] = { directories: new Set(), gitRemotes: new Set(), events: [] };
      }

      // Add current event
      if (event.directory) target.directories.add(event.directory);
      if (event.gitRemote) target.gitRemotes.add(event.gitRemote);
      target.events.push(event);
      for (const key of keys) lookup.set(key, targetIdx);
    }
  }

  // Compact: remove empty (merged) groups, reindex
  const compacted: ProjectGroup[] = [];
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

/**
 * Find the group index for given identifiers.
 * Returns undefined if no match or if identifiers match different groups (ambiguous).
 */
export function findGroupForIdentifiers(
  lookup: Map<string, number>,
  identifiers: ProjectIdentifiers,
): number | undefined {
  let result: number | undefined;

  if (identifiers.directory) {
    const idx = lookup.get(`dir:${identifiers.directory}`);
    if (idx !== undefined) result = idx;
  }

  if (identifiers.gitRemote) {
    const idx = lookup.get(`remote:${identifiers.gitRemote}`);
    if (idx !== undefined) {
      if (result !== undefined && result !== idx) return undefined; // ambiguous
      result = idx;
    }
  }

  return result;
}
