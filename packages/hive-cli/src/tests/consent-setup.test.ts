import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import * as realOs from 'node:os';
import { tmpdir } from 'node:os';
import { basename, join } from 'node:path';
import { afterAll, beforeAll, beforeEach, describe, expect, test } from 'bun:test';
import * as realPrompts from '@inquirer/prompts';
import * as realAuth from '../lib/auth';
import * as realConvex from '../lib/convex';
import * as realDiscovery from '../lib/transcript-discovery';
import { mockForFile } from './file-mock';

type ServerProject = { directories: Array<string>; gitRemotes: Array<string>; sessionSharing: boolean };

const updates: Array<{ identifier: unknown; sessionSharing: boolean }> = [];
let ticked: Array<string> = [];
let choices: Array<{ value: string; checked?: boolean }> = [];
let serverProjects: Array<ServerProject> = [];
/** ~/.claude/projects entry name -> the cwd its transcripts record. */
const cwds = new Map<string, string>();

mockForFile('../lib/auth', realAuth, { getAuthData: () => Promise.resolve({ accessToken: 'test' }) });
mockForFile('../lib/convex', realConvex, {
  getConsentStatus: () => Promise.resolve({ hasConsent: true, sessionSharing: true }),
  getProjectSharing: () => Promise.resolve(serverProjects),
  updateProjectSharing: (changes: typeof updates) => {
    updates.push(...changes);
    return Promise.resolve();
  },
  getRepoLinkStatus: () => Promise.resolve('linked'),
});
mockForFile('../lib/transcript-discovery', realDiscovery, {
  extractCwd: (projectDir: string) => cwds.get(basename(projectDir)) ?? null,
});
mockForFile('@inquirer/prompts', realPrompts, {
  checkbox: (config: { choices: typeof choices }) => {
    choices = config.choices;
    return Promise.resolve(ticked);
  },
});

let home = '';
// Bun resolves os.homedir() from the OS rather than $HOME.
mockForFile('node:os', realOs, { homedir: () => home });

const { consentSetup } = await import('../commands/consent-setup');
const { statePaths } = await import('../lib/config');

const marker = (checkout: string): string => statePaths(join(checkout, '.claude', 'hive')).sharingDisabled;

async function optOut(checkout: string): Promise<void> {
  await mkdir(join(checkout, '.claude', 'hive'), { recursive: true });
  await writeFile(marker(checkout), '');
}

/** A checkout Claude Code has a transcript dir for, optionally a git repo with an origin remote. */
async function addCheckout(entry: string, remote?: string): Promise<string> {
  const dir = join(home, 'work', entry);
  await mkdir(dir, { recursive: true });
  await mkdir(join(home, '.claude', 'projects', `-work-${entry}`), { recursive: true });
  cwds.set(`-work-${entry}`, dir);
  if (remote) {
    execFileSync('git', ['init', '-q', dir]);
    execFileSync('git', ['-C', dir, 'remote', 'add', 'origin', remote]);
  }
  return dir;
}

beforeAll(async () => {
  home = await realpath(await mkdtemp(join(tmpdir(), 'hive-consent-home-')));
});

beforeEach(async () => {
  updates.length = 0;
  serverProjects = [];
  choices = [];
  cwds.clear();
  await rm(join(home, '.claude'), { recursive: true, force: true });
  await rm(join(home, 'work'), { recursive: true, force: true });
});

afterAll(async () => {
  await rm(home, { recursive: true, force: true });
});

describe('consent setup', () => {
  test('re-enabling a project clears the local sharing-disabled marker', async () => {
    const project = await addCheckout('proj');
    await optOut(project);
    ticked = [project];

    expect(await consentSetup()).toBe(0);

    expect(updates).toEqual([{ identifier: expect.anything(), sessionSharing: true }]);
    expect(existsSync(marker(project))).toBe(false);
  });

  // `consent disable` writes its marker even when the server can't hear of it, so the server may still have it on.
  test('a server-enabled project held back by the local marker is offered unticked, and ticking it clears the marker', async () => {
    const project = await addCheckout('proj');
    await optOut(project);
    serverProjects = [{ directories: [project], gitRemotes: [], sessionSharing: true }];
    ticked = [project];

    expect(await consentSetup()).toBe(0);

    expect(choices).toEqual([expect.objectContaining({ value: project, checked: false })]);
    expect(updates).toEqual([{ identifier: expect.anything(), sessionSharing: true }]);
    expect(existsSync(marker(project))).toBe(false);
  });

  test('leaving such a project unticked disables it on the server and keeps the marker', async () => {
    const project = await addCheckout('proj');
    await optOut(project);
    serverProjects = [{ directories: [project], gitRemotes: [], sessionSharing: true }];
    ticked = [];

    expect(await consentSetup()).toBe(0);

    expect(updates).toEqual([{ identifier: expect.anything(), sessionSharing: false }]);
    expect(existsSync(marker(project))).toBe(true);
  });

  test("separate clones of one repo share a row, and any clone's marker holds it back until ticked", async () => {
    const remote = 'https://github.com/example/repo.git';
    const first = await addCheckout('a', remote);
    const second = await addCheckout('b', remote);
    await optOut(second);
    serverProjects = [{ directories: [], gitRemotes: ['github.com/example/repo'], sessionSharing: true }];
    ticked = ['github.com/example/repo'];

    expect(await consentSetup()).toBe(0);

    expect(choices).toEqual([expect.objectContaining({ value: 'github.com/example/repo', checked: false })]);
    expect(updates).toEqual([{ identifier: expect.anything(), sessionSharing: true }]);
    expect(existsSync(marker(first))).toBe(false);
    expect(existsSync(marker(second))).toBe(false);
  });
});
