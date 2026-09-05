import { describe, expect, test } from 'bun:test';
import {
  computeConsentWindows,
  extractIdentifiers,
  findGroupForIdentifiers,
  groupProjectConsentEvents,
  isInConsentWindow,
} from './consent-windows';
import type { ProjectConsentEvent, ProjectIdentifiers } from './consent-windows';

/** The group that identifiers resolve to, through the public reader. */
const groupFor = (r: ReturnType<typeof groupProjectConsentEvents>, ids: ProjectIdentifiers) => {
  const idx = findGroupForIdentifiers(r.lookup, ids);
  return idx === undefined ? undefined : r.groups[idx];
};

describe('computeConsentWindows', () => {
  test('first opt-in is retroactive, opt-out closes, a later opt-in starts at its timestamp', () => {
    const windows = computeConsentWindows([
      { sessionSharing: true, timestamp: 1000 },
      { sessionSharing: false, timestamp: 2000 },
      { sessionSharing: true, timestamp: 3000 },
    ]);
    expect(windows).toEqual([
      { start: 0, end: 2000 },
      { start: 3000, end: Infinity },
    ]);
  });

  test('a repeated opt-in does not open a second window, so a later opt-out closes everything', () => {
    const windows = computeConsentWindows([
      { sessionSharing: true, timestamp: 1000 },
      { sessionSharing: true, timestamp: 2000 },
      { sessionSharing: false, timestamp: 3000 },
    ]);
    expect(windows).toEqual([{ start: 0, end: 3000 }]);
    expect(isInConsentWindow(3001, windows)).toBe(false);
  });

  test('a leading opt-out is a no-op', () => {
    expect(
      computeConsentWindows([
        { sessionSharing: false, timestamp: 1000 },
        { sessionSharing: true, timestamp: 2000 },
      ]),
    ).toEqual([{ start: 0, end: Infinity }]);
  });

  test('windows are start-inclusive and end-exclusive', () => {
    const windows = [{ start: 10, end: 20 }];
    expect(isInConsentWindow(10, windows)).toBe(true);
    expect(isInConsentWindow(20, windows)).toBe(false);
  });
});

describe('groupProjectConsentEvents', () => {
  test('returns empty groups for empty input', () => {
    const result = groupProjectConsentEvents([]);
    expect(result.groups).toEqual([]);
    expect(result.lookup.size).toBe(0);
  });

  test('creates one group for a single event, reachable by either identifier', () => {
    const event = {
      directory: '/home/user/project',
      gitRemote: 'github.com/user/repo',
      sessionSharing: true,
      timestamp: 100,
    };
    const result = groupProjectConsentEvents([event]);
    expect(result.groups).toHaveLength(1);
    expect(result.groups[0].directories).toEqual(new Set(['/home/user/project']));
    expect(result.groups[0].gitRemotes).toEqual(new Set(['github.com/user/repo']));
    expect(groupFor(result, { directory: '/home/user/project' })?.events).toEqual([event]);
    expect(groupFor(result, { gitRemote: 'github.com/user/repo' })?.events).toEqual([event]);
  });

  test('merges two events with same directory but different remotes into one group', () => {
    const events: Array<ProjectConsentEvent> = [
      { directory: '/home/user/project', gitRemote: 'github.com/user/repo', sessionSharing: true, timestamp: 100 },
      { directory: '/home/user/project', gitRemote: 'github.com/other/fork', sessionSharing: true, timestamp: 200 },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(1);
    expect(result.groups[0].gitRemotes).toEqual(new Set(['github.com/user/repo', 'github.com/other/fork']));
    expect(result.groups[0].events).toHaveLength(2);
    expect(groupFor(result, { gitRemote: 'github.com/other/fork' })).toBe(
      groupFor(result, { directory: '/home/user/project' }),
    );
  });

  test('merges two events with same remote but different directories into one group', () => {
    const events: Array<ProjectConsentEvent> = [
      { directory: '/home/user/project-a', gitRemote: 'github.com/user/repo', sessionSharing: true, timestamp: 100 },
      { directory: '/home/user/project-b', gitRemote: 'github.com/user/repo', sessionSharing: false, timestamp: 200 },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(1);
    expect(result.groups[0].directories).toEqual(new Set(['/home/user/project-a', '/home/user/project-b']));
    expect(result.groups[0].events).toHaveLength(2);
  });

  test('keeps two unrelated events in separate groups', () => {
    const events: Array<ProjectConsentEvent> = [
      { directory: '/home/user/project-a', gitRemote: 'github.com/user/repo-a', sessionSharing: true, timestamp: 100 },
      { directory: '/home/user/project-b', gitRemote: 'github.com/user/repo-b', sessionSharing: true, timestamp: 200 },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(2);
    const a = groupFor(result, { directory: '/home/user/project-a' });
    const b = groupFor(result, { directory: '/home/user/project-b' });
    expect(a).not.toBe(b);
    expect(groupFor(result, { gitRemote: 'github.com/user/repo-a' })).toBe(a);
    expect(groupFor(result, { gitRemote: 'github.com/user/repo-b' })).toBe(b);
  });

  test('merges transitively: A shares dir with B, B shares remote with C', () => {
    const events: Array<ProjectConsentEvent> = [
      { directory: '/a', sessionSharing: true, timestamp: 100 },
      { directory: '/a', gitRemote: 'r1', sessionSharing: true, timestamp: 200 },
      { directory: '/b', gitRemote: 'r1', sessionSharing: true, timestamp: 300 },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(1);
    expect(result.groups[0].directories).toEqual(new Set(['/a', '/b']));
    expect(result.groups[0].events).toHaveLength(3);
    expect(groupFor(result, { directory: '/b' })).toBe(groupFor(result, { gitRemote: 'r1' }));
  });

  test('groups by shared identifiers regardless of consent state', () => {
    const events: Array<ProjectConsentEvent> = [
      { directory: '/shared', sessionSharing: true, timestamp: 100 },
      { directory: '/shared', sessionSharing: false, timestamp: 200 },
      { directory: '/other', sessionSharing: true, timestamp: 300 },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(2);
    expect(groupFor(result, { directory: '/shared' })).not.toBe(groupFor(result, { directory: '/other' }));
  });

  test('merges events with same gitRemote in different cases into one group', () => {
    const events: Array<ProjectConsentEvent> = [
      { gitRemote: 'github.com/User/Repo', sessionSharing: true, timestamp: 100 },
      { gitRemote: 'github.com/user/repo', sessionSharing: false, timestamp: 200 },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(1);
    expect(result.groups[0].events).toHaveLength(2);
    // Both original-case remotes are preserved in the set
    expect(result.groups[0].gitRemotes).toEqual(new Set(['github.com/User/Repo', 'github.com/user/repo']));
    expect(groupFor(result, { gitRemote: 'GITHUB.COM/USER/REPO' })).toBe(result.groups[0]);
  });

  test('handles events with no identifiers', () => {
    const result = groupProjectConsentEvents([{ sessionSharing: true, timestamp: 100 }]);
    expect(result.groups).toHaveLength(1);
    expect(result.groups[0].directories).toEqual(new Set());
    expect(result.groups[0].gitRemotes).toEqual(new Set());
    expect(result.groups[0].events).toHaveLength(1);
  });

  test('handles complex transitive chains across many events', () => {
    const events: Array<ProjectConsentEvent> = [
      { directory: '/a', gitRemote: 'r1', sessionSharing: true, timestamp: 100 },
      { directory: '/b', gitRemote: 'r2', sessionSharing: true, timestamp: 200 },
      { directory: '/b', gitRemote: 'r1', sessionSharing: true, timestamp: 300 },
    ];
    const result = groupProjectConsentEvents(events);
    expect(result.groups).toHaveLength(1);
    expect(result.groups[0].directories).toEqual(new Set(['/a', '/b']));
    expect(result.groups[0].gitRemotes).toEqual(new Set(['r1', 'r2']));
    expect(result.groups[0].events).toHaveLength(3);
  });
});

describe('findGroupForIdentifiers', () => {
  const lookup = new Map<string, number>([
    ['dir:/home/user/project', 0],
    ['remote:github.com/user/repo', 0],
    ['dir:/other', 1],
    ['remote:github.com/other/repo', 1],
  ]);

  test('finds group by directory', () => {
    expect(findGroupForIdentifiers(lookup, { directory: '/home/user/project' })).toBe(0);
  });

  test('finds group by gitRemote', () => {
    expect(findGroupForIdentifiers(lookup, { gitRemote: 'github.com/other/repo' })).toBe(1);
  });

  test('returns the group index when both identifiers match the same group', () => {
    expect(
      findGroupForIdentifiers(lookup, { directory: '/home/user/project', gitRemote: 'github.com/user/repo' }),
    ).toBe(0);
  });

  test('returns undefined when identifiers match different groups (ambiguous)', () => {
    expect(
      findGroupForIdentifiers(lookup, { directory: '/home/user/project', gitRemote: 'github.com/other/repo' }),
    ).toBeUndefined();
  });

  test('returns undefined when no identifiers match', () => {
    expect(
      findGroupForIdentifiers(lookup, { directory: '/somewhere/else', gitRemote: 'gitlab.com/other/repo' }),
    ).toBeUndefined();
  });

  test('returns undefined when identifiers are empty', () => {
    expect(findGroupForIdentifiers(lookup, {})).toBeUndefined();
  });

  test('matches gitRemote case-insensitively', () => {
    expect(findGroupForIdentifiers(lookup, { gitRemote: 'github.com/User/Repo' })).toBe(0);
  });
});

describe('extractIdentifiers', () => {
  test('prefers new fields over legacy project', () => {
    expect(
      extractIdentifiers({
        project: 'github.com/user/repo',
        directory: '/home/user/project',
        gitRemote: 'github.com/other/repo',
      }),
    ).toEqual({ directory: '/home/user/project', gitRemote: 'github.com/other/repo' });
  });

  test('uses directory alone when gitRemote is absent', () => {
    expect(extractIdentifiers({ directory: '/home/user/project' })).toEqual({
      directory: '/home/user/project',
      gitRemote: undefined,
    });
  });

  test('uses gitRemote alone when directory is absent', () => {
    expect(extractIdentifiers({ gitRemote: 'github.com/user/repo' })).toEqual({
      directory: undefined,
      gitRemote: 'github.com/user/repo',
    });
  });

  test('falls back to classifyLegacyProject for path-based project', () => {
    expect(extractIdentifiers({ project: '/home/user/project' })).toEqual({ directory: '/home/user/project' });
  });

  test('falls back to classifyLegacyProject for remote-based project', () => {
    expect(extractIdentifiers({ project: 'github.com/user/repo' })).toEqual({ gitRemote: 'github.com/user/repo' });
  });

  test('returns empty object when no fields are set', () => {
    expect(extractIdentifiers({})).toEqual({});
  });
});
